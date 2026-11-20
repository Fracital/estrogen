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
use crate::pass::names::NamesPass;
use crate::pass::wasm::WasmPass;
use crate::pass::{FileContext, Pass};
use crate::report::{FileReport, Report};
use crate::walk::{eligibility, walk_files, Eligibility, FileKind};

/// The obfuscator. Build it with [`Obfuscator::new`] from a set of options, then
/// call [`Obfuscator::obfuscate_dir`] on each built directory.
pub struct Obfuscator {
    options: Options,
}

impl Obfuscator {
    /// Build an obfuscator from run options.
    ///
    /// This does not touch the filesystem. It returns `Err` only if the options
    /// themselves are unusable; today they always are usable, so it is
    /// infallible in practice, and the `Result` leaves room for future
    /// validation.
    pub fn new(options: Options) -> Result<Self> {
        Ok(Obfuscator { options })
    }

    /// The options this obfuscator runs with.
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// The passes that apply to a file of the given kind, in run order.
    ///
    /// The order follows the `protect` script: literals, then names, then wasm
    /// for JS; markup for the HTML shells and SVG cloaks.
    fn passes_for(kind: FileKind) -> Vec<Box<dyn Pass + Sync>> {
        match kind {
            FileKind::Js => vec![
                Box::new(LiteralsPass),
                Box::new(NamesPass),
                Box::new(WasmPass),
            ],
            FileKind::Html | FileKind::Svg => vec![Box::new(MarkupPass)],
            FileKind::Wasm | FileKind::Other => Vec::new(),
        }
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

        let mut edits = EditSet::new();
        for pass in Self::passes_for(kind) {
            let ctx = FileContext::new(path, kind, &source, &self.options);
            edits.extend(pass.collect_edits(&ctx)?);
        }

        if edits.is_empty() {
            // Identity milestone: the passes make no edits, so the file is left
            // exactly as it was. It counts as unprotected in a check.
            return Ok(FileReport::unchanged(path, kind));
        }

        let edit_count = edits.len();
        let rewritten = edits.apply(&source)?;
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
            "crack-obf-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn identity_run_leaves_files_byte_for_byte() {
        let dir = tmp_dir();
        let js = dir.join("app.js");
        let original = "const a = 'hello world';\n";
        fs::write(&js, original).unwrap();

        let obf = Obfuscator::new(Options::default()).unwrap();
        let report = obf.obfuscate_dir(&dir).unwrap();

        // The file is unchanged on disk.
        assert_eq!(fs::read_to_string(&js).unwrap(), original);
        assert_eq!(report.total(), 1);
        // Eligible but not obfuscated, so it is unprotected in the identity
        // milestone.
        assert!(report.has_unprotected());
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
