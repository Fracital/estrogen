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

use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rayon::ThreadPoolBuilder;

use crate::edit::Edit;
use crate::edit::EditSet;
use crate::error::{Error, Result};
use crate::options::Options;
use crate::pass::literals::LiteralsPass;
use crate::pass::markup::collect_markup_edits;
use crate::pass::names::collect_proxy_edits;
use crate::pass::{FileContext, Pass};
use crate::proxy::ProxyEngine;
use crate::report::{FileReport, Report};
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
        let pool = ThreadPoolBuilder::new()
            .num_threads(self.options.jobs.max(1))
            .build()
            .map_err(|e| Error::Config {
                path: dir.to_path_buf(),
                message: format!("could not start the worker pool: {e}"),
            })?;

        let results: Vec<Result<FileReport>> =
            pool.install(|| files.par_iter().map(|p| self.obfuscate_file(p)).collect());

        let mut report = Report::new(vec![dir.to_path_buf()]);
        for r in results {
            report.push(r?);
        }
        Ok(report)
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
}
