//! The obfuscator: walk a built directory and run the passes over it.
//!
//! [`Obfuscator`] holds the run [`Options`] and drives the work. It walks a
//! directory, reads each file, decides eligibility with [`crate::walk`], runs
//! the passes for the file's kind, splices their edits into the source, and
//! writes the result back in place. Files are independent, so a directory is
//! processed in parallel with rayon, sized by [`Options::jobs`].
//!
//! It fails closed. The first file that cannot be handled aborts the run with an
//! [`Error`], and no partial output is written for that file. In `--check` mode
//! ([`Options::check`]) nothing is written at all; the report says what would
//! happen.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rayon::ThreadPoolBuilder;

use crate::edit::Edit;
use crate::edit::EditSet;
use crate::error::{Error, Result};
use crate::options::{Options, VendorInput};
use crate::pass::literals::LiteralsPass;
use crate::pass::markup::collect_markup_edits;
use crate::pass::names::collect_proxy_edits;
use crate::pass::{FileContext, Pass};
use crate::proxy::ProxyEngine;
use crate::report::{FileReport, Report};
use crate::vendor::{self, VendorFile};
use crate::walk::{eligibility, walk_files, Eligibility, FileKind};

/// The obfuscator. Build it with [`Obfuscator::new`] from a set of options, then
/// call [`Obfuscator::obfuscate_dir`] on each built directory.
pub struct Obfuscator {
    options: Options,
    proxy: ProxyEngine,
}

impl Obfuscator {
    /// Build an obfuscator from run options.
    ///
    /// This does not touch the filesystem. It compiles the proxy renaming engine
    /// from the options, drawing the per-build rename key once, so every file in
    /// the run renames a proxy name to the same substitute. It returns `Err` if
    /// the proxy config does not compile (a bad names regex or vendor glob).
    pub fn new(options: Options) -> Result<Self> {
        let proxy = ProxyEngine::new(&options.proxy, &options.canaries, options.seed.as_deref())?;
        // TODO(sourcemap): emit a V3 map from the edit list when a dir is set.
        // Until then estrogen fails closed rather than silently ship no map for a
        // build that asked for one.
        if options.sourcemap_dir.is_some() {
            return Err(Error::Engine(
                "source map emission is not implemented yet; omit --sourcemap",
            ));
        }
        Ok(Obfuscator { options, proxy })
    }

    /// The options this obfuscator runs with.
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// Obfuscate every eligible file under `dir`, in place.
    ///
    /// Returns a [`Report`] of every file seen. In `--check` mode no file is
    /// written. The run aborts on the first file that cannot be handled.
    pub fn obfuscate_dir(&self, dir: &Path) -> Result<Report> {
        let files = walk_files(dir);
        let mut report = Report::new(vec![dir.to_path_buf()]);
        for fr in self.process_files(&files, &HashSet::new())? {
            report.push(fr);
        }
        Ok(report)
    }

    /// Obfuscate each file in `files` in parallel, skipping any path in
    /// `exclude`, and collect the per-file reports. The first file that cannot
    /// be handled aborts with its error.
    fn process_files(
        &self,
        files: &[PathBuf],
        exclude: &HashSet<PathBuf>,
    ) -> Result<Vec<FileReport>> {
        let pool = ThreadPoolBuilder::new()
            .num_threads(self.options.jobs.max(1))
            .build()
            .map_err(|e| Error::Config {
                path: PathBuf::from("<pool>"),
                message: format!("could not start the worker pool: {e}"),
            })?;

        let results: Vec<Result<FileReport>> = pool.install(|| {
            files
                .par_iter()
                .filter(|p| !exclude.contains(*p))
                .map(|p| self.obfuscate_file(p))
                .collect()
        });
        results.into_iter().collect()
    }

    /// Vendor the configured `[[vendor]]` inputs into `dist`, then obfuscate the
    /// rest of the tree. This is the build-glue entry point: it copies packages
    /// and folders into the served tree under stamped neutral names, rewrites
    /// their references, strips source maps, obfuscates the ones that ask for it
    /// and the app chunks, and keeps the vendored transports verbatim but canary
    /// checked. [`obfuscate_dir`](Self::obfuscate_dir) stays obfuscate-only.
    ///
    /// The per-build stamp is minted once and reused across every input, so a
    /// reference rewritten to a stamped name matches the file that lands there.
    pub fn build(&self, dist: &Path) -> Result<Report> {
        let mut report = Report::new(vec![dist.to_path_buf()]);
        let stamp = vendor::mint_stamp(self.options.seed.as_deref(), &self.canary_values())?;

        let mut vendored: HashSet<PathBuf> = HashSet::new();
        for input in &self.options.vendor {
            for vf in vendor::resolve(input, dist, &stamp)? {
                let fr = self.vendor_one(&vf, input, &stamp)?;
                vendored.insert(vf.dest);
                report.push(fr);
            }
        }

        // Obfuscate everything else under dist (the app chunks and shells),
        // leaving the vendored files exactly as they were brought in.
        for fr in self.process_files(&walk_files(dist), &vendored)? {
            report.push(fr);
        }
        Ok(report)
    }

    /// Bring one vendored file in: copy it to its destination, apply the input's
    /// text rewrites and strip a source-map comment, then obfuscate it when the
    /// input asks or keep it verbatim. A binary file is copied byte for byte. In
    /// every case the result is canary checked and the build fails closed on a
    /// surviving canary.
    fn vendor_one(&self, vf: &VendorFile, input: &VendorInput, stamp: &str) -> Result<FileReport> {
        if let Some(parent) = vf.dest.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        let kind = FileKind::classify(&vf.dest);

        match fs::read_to_string(&vf.src) {
            Ok(text) => {
                let rewritten = vendor::apply_rewrites(&text, &input.rewrite, stamp);
                let mut set = EditSet::new();
                if let Some(edit) = sourcemap_strip_edit(&rewritten) {
                    set.push(edit);
                }
                let text = set.apply(&rewritten)?;
                if !self.options.check {
                    fs::write(&vf.dest, &text).map_err(|e| Error::io(&vf.dest, e))?;
                }
                match input.mode {
                    crate::options::VendorMode::Obfuscate if !self.options.check => {
                        let inner = self.obfuscate_file(&vf.dest)?;
                        Ok(FileReport::vendored(&vf.dest, kind, inner.is_protected()))
                    }
                    crate::options::VendorMode::Obfuscate => {
                        Ok(FileReport::vendored(&vf.dest, kind, false))
                    }
                    crate::options::VendorMode::Verbatim => {
                        self.scan_canaries(&vf.dest, text.as_bytes())?;
                        Ok(FileReport::vendored(&vf.dest, kind, false))
                    }
                }
            }
            Err(_) => {
                // A binary file (wasm, an image): copy the bytes unchanged. Text
                // rewrites and obfuscation do not apply; it is still canary
                // checked.
                let bytes = fs::read(&vf.src).map_err(|e| Error::io(&vf.src, e))?;
                if !self.options.check {
                    fs::write(&vf.dest, &bytes).map_err(|e| Error::io(&vf.dest, e))?;
                }
                self.scan_canaries(&vf.dest, &bytes)?;
                Ok(FileReport::vendored(&vf.dest, kind, false))
            }
        }
    }

    /// Fail closed if any canary appears verbatim in `bytes`.
    fn scan_canaries(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        for canary in &self.options.canaries {
            let needle = canary.value.as_bytes();
            if !needle.is_empty() && bytes.windows(needle.len()).any(|w| w == needle) {
                return Err(Error::Canary {
                    path: path.to_path_buf(),
                    literal: canary.value.clone(),
                });
            }
        }
        Ok(())
    }

    /// The canary values, for the stamp mint.
    fn canary_values(&self) -> Vec<String> {
        self.options
            .canaries
            .iter()
            .map(|c| c.value.clone())
            .collect()
    }

    /// Obfuscate one file in place and report what happened to it.
    ///
    /// A skipped file (a `.map`, an unhandled type, or one already obfuscated) is
    /// read only enough to decide, and reported as a skip. An eligible file is
    /// parsed and run through its passes; the edits are spliced into the source
    /// and, unless `--check` is set or there were no edits, written back. A file
    /// that cannot be parsed or handled returns an [`Error`] and is not written.
    pub fn obfuscate_file(&self, path: &Path) -> Result<FileReport> {
        // Decide on the path alone first, so a `.map` or an unhandled type is
        // never even read.
        if let Eligibility::Skip(reason) = eligibility(path, None) {
            return Ok(FileReport::skipped(path, FileKind::classify(path), reason));
        }

        let source = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;

        let kind = match eligibility(path, Some(&source)) {
            Eligibility::Process(kind) => kind,
            Eligibility::Skip(reason) => {
                return Ok(FileReport::skipped(path, FileKind::classify(path), reason));
            }
        };

        // One transform per file: a proxy vendor JS file is renamed, any other
        // JS chunk gets full literal encryption, and HTML/SVG run the markup
        // pass. This mirrors the reference's `isProxyVendor ? proxy : app` split.
        let ctx = FileContext::new(path, kind, &source, &self.options);
        let mut edits = match kind {
            FileKind::Js => {
                if self.proxy.is_vendor(path) {
                    collect_proxy_edits(&ctx, &self.proxy)?
                } else {
                    LiteralsPass.collect_edits(&ctx)?
                }
            }
            FileKind::Html | FileKind::Svg => collect_markup_edits(&ctx, &self.proxy)?,
            FileKind::Wasm | FileKind::Other => EditSet::new(),
        };

        // Strip any bundler sourceMappingURL comment from a processed JS chunk.
        // Its map points at the pre-obfuscation source and must never ship.
        if kind == FileKind::Js {
            if let Some(edit) = sourcemap_strip_edit(&source) {
                edits.push(edit);
            }
        }

        if edits.is_empty() {
            // Nothing to rewrite: no literal, name or canary, and no source-map
            // comment. The file is left exactly as it was, and counts as
            // unprotected in a check.
            return Ok(FileReport::unchanged(path, kind));
        }

        let edit_count = edits.len();
        let rewritten = edits.apply(&source)?;
        // Fail closed: a JS chunk that no longer parses is a pass bug, and estrogen
        // never writes a broken chunk over a working one.
        if kind == FileKind::Js {
            crate::pass::ensure_js_reparses(path, &rewritten)?;
        }
        // Fail closed: no canary literal may survive verbatim in the output. This
        // is the check-plaintext-leaks guarantee enforced at write time.
        for canary in &self.options.canaries {
            if !canary.value.is_empty() && rewritten.contains(&canary.value) {
                return Err(Error::Canary {
                    path: path.to_path_buf(),
                    literal: canary.value.clone(),
                });
            }
        }
        if !self.options.check {
            write_preserving_bom(path, &source, &rewritten)?;
        }
        Ok(FileReport::obfuscated(path, kind, edit_count))
    }

    /// Obfuscate several directories and merge their reports.
    pub fn obfuscate_dirs(&self, dirs: &[PathBuf]) -> Result<Report> {
        let mut combined = Report::new(dirs.to_vec());
        for dir in dirs {
            combined.absorb(self.obfuscate_dir(dir)?);
        }
        Ok(combined)
    }
}

/// An edit that deletes a trailing bundler `sourceMappingURL` comment, or
/// `None` when the chunk has none. Both the `//#` and `//@` spellings are
/// handled, and one preceding newline is removed so no blank line is left. A
/// match that is not the last content in the file is left alone, matching the
/// reference's end-of-file anchor.
fn sourcemap_strip_edit(source: &str) -> Option<Edit> {
    let start = ["//# sourceMappingURL=", "//@ sourceMappingURL="]
        .iter()
        .filter_map(|m| source.rfind(m))
        .max()?;
    let line_end = source[start..]
        .find('\n')
        .map(|n| start + n)
        .unwrap_or(source.len());
    if !source[line_end..].chars().all(char::is_whitespace) {
        return None;
    }
    let del_start = if source[..start].ends_with('\n') {
        start - 1
    } else {
        start
    };
    Some(Edit::replace(del_start, line_end, String::new()))
}

/// Write `rewritten` to `path`, keeping a UTF-8 byte-order mark if the original
/// had one. Input is UTF-8; a BOM at the head of the original is preserved.
fn write_preserving_bom(path: &Path, original: &str, rewritten: &str) -> Result<()> {
    const BOM: &str = "\u{feff}";
    let body = if original.starts_with(BOM) && !rewritten.starts_with(BOM) {
        let mut with_bom = String::with_capacity(BOM.len() + rewritten.len());
        with_bom.push_str(BOM);
        with_bom.push_str(rewritten);
        with_bom
    } else {
        rewritten.to_string()
    };
    fs::write(path, body).map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn tmp_dir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "estrogen-obf-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_run_obfuscates_a_js_chunk_in_place() {
        let dir = tmp_dir();
        let js = dir.join("app.js");
        let original = "const a = 'hello world';\n";
        fs::write(&js, original).unwrap();

        let obf = Obfuscator::new(Options::default()).unwrap();
        let report = obf.obfuscate_dir(&dir).unwrap();

        // The literal pass rewrote the file: the plaintext is gone, the marker
        // and the decoder are in, and the chunk is reported as obfuscated.
        let written = fs::read_to_string(&js).unwrap();
        assert_ne!(written, original);
        assert!(!written.contains("'hello world'"));
        assert!(written.contains(crate::format::MARKER));
        assert_eq!(report.total(), 1);
        assert!(!report.has_unprotected());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn map_and_css_are_skipped() {
        let dir = tmp_dir();
        fs::write(dir.join("app.js.map"), "{}").unwrap();
        fs::write(dir.join("styles.css"), "body{}").unwrap();

        let obf = Obfuscator::new(Options::default()).unwrap();
        let report = obf.obfuscate_dir(&dir).unwrap();
        assert_eq!(report.skipped(), 2);
        assert!(!report.has_unprotected());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn already_obfuscated_js_is_skipped() {
        let dir = tmp_dir();
        let body = format!("/* {} */ const a = 1;", crate::format::MARKER);
        fs::write(dir.join("done.js"), &body).unwrap();

        let obf = Obfuscator::new(Options::default()).unwrap();
        let report = obf.obfuscate_dir(&dir).unwrap();
        assert_eq!(report.skipped(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_error_aborts_the_run() {
        let dir = tmp_dir();
        fs::write(dir.join("bad.js"), "function (((").unwrap();

        let obf = Obfuscator::new(Options::default()).unwrap();
        let err = obf.obfuscate_dir(&dir).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn check_mode_writes_nothing() {
        let dir = tmp_dir();
        let js = dir.join("app.js");
        let original = "const a = 'x';\n";
        fs::write(&js, original).unwrap();

        let opts = Options {
            check: true,
            ..Options::default()
        };
        let obf = Obfuscator::new(opts).unwrap();
        let report = obf.obfuscate_dir(&dir).unwrap();
        assert_eq!(fs::read_to_string(&js).unwrap(), original);
        assert_eq!(report.total(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sourcemap_strip_helper_only_strips_a_trailing_comment() {
        assert!(sourcemap_strip_edit("const a=1;").is_none());
        assert!(sourcemap_strip_edit("x\n//# sourceMappingURL=a.map").is_some());
        assert!(sourcemap_strip_edit("//@ sourceMappingURL=a.map\n").is_some());
        // A match with real code after it is not a bundler link; leave it.
        assert!(sourcemap_strip_edit("//# sourceMappingURL=a.map\ncode()").is_none());
    }

    #[test]
    fn sourcemappingurl_is_stripped_from_output() {
        let dir = tmp_dir();
        let js = dir.join("app.js");
        fs::write(&js, "const a = 'x';\n//# sourceMappingURL=app.js.map\n").unwrap();

        let opts = Options {
            seed: Some(b"seed".to_vec()),
            ..Options::default()
        };
        let obf = Obfuscator::new(opts).unwrap();
        obf.obfuscate_dir(&dir).unwrap();
        let out = fs::read_to_string(&js).unwrap();
        assert!(
            !out.contains("sourceMappingURL"),
            "map link survived: {out}"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_surviving_canary_fails_the_build() {
        let dir = tmp_dir();
        let js = dir.join("app.js");
        // "Fracital" is an identifier, not a string, so the literal pass does
        // not touch it; the output scan must catch the leak and fail closed.
        fs::write(&js, "const Fracital = 'x';").unwrap();

        let opts = Options {
            seed: Some(b"seed".to_vec()),
            canaries: vec![crate::options::Canary {
                value: "Fracital".to_string(),
                reason: String::new(),
            }],
            ..Options::default()
        };
        let obf = Obfuscator::new(opts).unwrap();
        let err = obf.obfuscate_dir(&dir).unwrap_err();
        assert!(matches!(err, Error::Canary { .. }));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sourcemap_dir_is_a_clear_not_implemented_error() {
        let opts = Options {
            sourcemap_dir: Some(PathBuf::from("/tmp/maps")),
            ..Options::default()
        };
        assert!(matches!(Obfuscator::new(opts), Err(Error::Engine(_))));
    }

    // Vendor pipeline tests.
    use crate::options::{Rewrite, VendorInput, VendorMode};

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn vendor_opts(vendor: Vec<VendorInput>) -> Options {
        Options {
            seed: Some(b"vendor-seed".to_vec()),
            vendor,
            ..Options::default()
        }
    }

    fn input(from: &Path, to: &str, mode: VendorMode, stamp: bool) -> VendorInput {
        VendorInput {
            from: from.display().to_string(),
            to: to.to_string(),
            mode,
            stamp,
            rewrite: Vec::new(),
        }
    }

    #[test]
    fn build_vendors_a_folder_under_a_stamped_neutral_name() {
        let pkg = tmp_dir();
        write(&pkg.join("sw.js"), "export const n = 1;\n");
        let dist = tmp_dir();

        let opts = vendor_opts(vec![input(&pkg, "assets", VendorMode::Verbatim, true)]);
        let obf = Obfuscator::new(opts).unwrap();
        let report = obf.build(&dist).unwrap();

        let stamp = vendor::mint_stamp(Some(b"vendor-seed"), &[]).unwrap();
        let dest = dist.join("assets").join(format!("sw.{stamp}.js"));
        assert!(dest.exists(), "stamped file missing: {}", dest.display());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "export const n = 1;\n");
        assert_eq!(report.vendored(), 1);
        fs::remove_dir_all(&pkg).ok();
        fs::remove_dir_all(&dist).ok();
    }

    #[test]
    fn build_obfuscate_mode_runs_the_pass() {
        let pkg = tmp_dir();
        write(&pkg.join("app.js"), "const a = 'secret value';\n");
        let dist = tmp_dir();

        let opts = vendor_opts(vec![input(&pkg, "a", VendorMode::Obfuscate, false)]);
        let obf = Obfuscator::new(opts).unwrap();
        let report = obf.build(&dist).unwrap();

        let dest = dist.join("a").join("app.js");
        let out = fs::read_to_string(&dest).unwrap();
        assert!(out.contains(crate::format::MARKER));
        assert!(!out.contains("'secret value'"));
        assert_eq!(report.vendored(), 1);
        assert!(report.protected_files().any(|f| f.path == dest));
        fs::remove_dir_all(&pkg).ok();
        fs::remove_dir_all(&dist).ok();
    }

    #[test]
    fn build_rewrite_interpolates_the_stamp() {
        let pkg = tmp_dir();
        write(&pkg.join("loader.js"), "load('epoxy.wasm');\n");
        let dist = tmp_dir();

        let mut vi = input(&pkg, "assets", VendorMode::Verbatim, true);
        vi.rewrite = vec![Rewrite {
            find: "epoxy.wasm".to_string(),
            to: "ln.{stamp}.wasm".to_string(),
        }];
        let obf = Obfuscator::new(vendor_opts(vec![vi])).unwrap();
        obf.build(&dist).unwrap();

        let stamp = vendor::mint_stamp(Some(b"vendor-seed"), &[]).unwrap();
        let dest = dist.join("assets").join(format!("loader.{stamp}.js"));
        let out = fs::read_to_string(&dest).unwrap();
        assert!(
            out.contains(&format!("ln.{stamp}.wasm")),
            "rewrite missing: {out}"
        );
        assert!(!out.contains("epoxy.wasm"));
        fs::remove_dir_all(&pkg).ok();
        fs::remove_dir_all(&dist).ok();
    }

    #[test]
    fn build_verbatim_strips_sourcemappingurl() {
        let pkg = tmp_dir();
        write(
            &pkg.join("t.js"),
            "const a = 1;\n//# sourceMappingURL=t.js.map\n",
        );
        let dist = tmp_dir();
        let obf = Obfuscator::new(vendor_opts(vec![input(
            &pkg,
            "v",
            VendorMode::Verbatim,
            false,
        )]))
        .unwrap();
        obf.build(&dist).unwrap();

        let out = fs::read_to_string(dist.join("v").join("t.js")).unwrap();
        assert!(
            !out.contains("sourceMappingURL"),
            "map link survived: {out}"
        );
        assert!(out.contains("const a = 1;"));
        fs::remove_dir_all(&pkg).ok();
        fs::remove_dir_all(&dist).ok();
    }

    #[test]
    fn build_is_deterministic_under_a_seed() {
        let pkg = tmp_dir();
        write(&pkg.join("app.js"), "const a = 'x';\n");
        let run = |dist: &Path| {
            let obf = Obfuscator::new(vendor_opts(vec![input(
                &pkg,
                "a",
                VendorMode::Obfuscate,
                true,
            )]))
            .unwrap();
            obf.build(dist).unwrap();
        };
        let d1 = tmp_dir();
        let d2 = tmp_dir();
        run(&d1);
        run(&d2);
        let stamp = vendor::mint_stamp(Some(b"vendor-seed"), &[]).unwrap();
        let name = format!("app.{stamp}.js");
        assert_eq!(
            fs::read_to_string(d1.join("a").join(&name)).unwrap(),
            fs::read_to_string(d2.join("a").join(&name)).unwrap()
        );
        fs::remove_dir_all(&pkg).ok();
        fs::remove_dir_all(&d1).ok();
        fs::remove_dir_all(&d2).ok();
    }

    #[test]
    fn build_fails_closed_on_a_canary_in_a_vendored_file() {
        let pkg = tmp_dir();
        write(&pkg.join("t.js"), "// Fracital internal\nconst a = 1;\n");
        let dist = tmp_dir();
        let opts = Options {
            canaries: vec![crate::options::Canary {
                value: "Fracital".to_string(),
                reason: String::new(),
            }],
            ..vendor_opts(vec![input(&pkg, "v", VendorMode::Verbatim, false)])
        };
        let obf = Obfuscator::new(opts).unwrap();
        let err = obf.build(&dist).unwrap_err();
        assert!(matches!(err, Error::Canary { .. }));
        fs::remove_dir_all(&pkg).ok();
        fs::remove_dir_all(&dist).ok();
    }

    #[test]
    fn build_resolves_a_glob_from() {
        let pkg = tmp_dir();
        write(&pkg.join("keep.js"), "const a = 1;\n");
        write(&pkg.join("skip.css"), "body{}\n");
        let dist = tmp_dir();
        let glob = format!("{}/**/*.js", pkg.display());
        let obf = Obfuscator::new(vendor_opts(vec![VendorInput {
            from: glob,
            to: "g".to_string(),
            mode: VendorMode::Verbatim,
            stamp: false,
            rewrite: Vec::new(),
        }]))
        .unwrap();
        obf.build(&dist).unwrap();
        assert!(dist.join("g").join("keep.js").exists());
        assert!(!dist.join("g").join("skip.css").exists());
        fs::remove_dir_all(&pkg).ok();
        fs::remove_dir_all(&dist).ok();
    }
}
