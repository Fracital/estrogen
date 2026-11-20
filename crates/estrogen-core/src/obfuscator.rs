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

use crate::edit::EditSet;
use crate::error::{Error, Result};
use crate::options::Options;
use crate::pass::literals::LiteralsPass;
use crate::pass::markup::MarkupPass;
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
        let edits = match kind {
            FileKind::Js => {
                if self.proxy.is_vendor(path) {
                    collect_proxy_edits(&ctx, &self.proxy)?
                } else {
                    LiteralsPass.collect_edits(&ctx)?
                }
            }
            FileKind::Html | FileKind::Svg => MarkupPass.collect_edits(&ctx)?,
            FileKind::Wasm | FileKind::Other => EditSet::new(),
        };

        if edits.is_empty() {
            // Identity milestone: the passes make no edits, so the file is left
            // exactly as it was. It counts as unprotected in a check.
            return Ok(FileReport::unchanged(path, kind));
        }

        let edit_count = edits.len();
        let rewritten = edits.apply(&source)?;
        // Fail closed: a JS chunk that no longer parses is a pass bug, and estrogen
        // never writes a broken chunk over a working one.
        if kind == FileKind::Js {
            crate::pass::ensure_js_reparses(path, &rewritten)?;
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
}
