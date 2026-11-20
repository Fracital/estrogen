//! Run reports.
//!
//! [`FileReport`] records what happened to one file; [`Report`] collects them
//! for a run and answers the two questions the CLI asks: did everything that
//! should be protected get protected, and what do I print. In `--check` mode the
//! CLI exits nonzero when [`Report::has_unprotected`] is true.

use std::path::PathBuf;

use crate::walk::{FileKind, SkipReason};

/// What estrogen did with one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    /// Eligible and obfuscated: `edits` byte-range edits were applied.
    Obfuscated {
        /// How many edits were spliced in.
        edits: usize,
    },
    /// Eligible and parsed, but no pass changed it: no literal, name or canary,
    /// and no source-map comment. `protected` is false: the file parsed but was
    /// not obfuscated, which is what `--check` flags.
    Unchanged,
    /// Copied into the dist by a `[[vendor]]` input. `obfuscated` is true when it
    /// was obfuscated on the way in, false when it was kept verbatim (a vendored
    /// transport runtime). A verbatim vendored file is intentional, not a miss,
    /// so it is not flagged as unprotected.
    Vendored {
        /// Whether the obfuscation pass ran over it.
        obfuscated: bool,
    },
    /// Not eligible, with the reason from the walk gate.
    Skipped(SkipReason),
}

/// The outcome for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReport {
    /// The file this reports on.
    pub path: PathBuf,
    /// Its kind by extension.
    pub kind: FileKind,
    /// What estrogen did with it.
    pub status: FileStatus,
}

impl FileReport {
    /// A report for an eligible file that was obfuscated with `edits` edits.
    pub fn obfuscated(path: impl Into<PathBuf>, kind: FileKind, edits: usize) -> Self {
        FileReport {
            path: path.into(),
            kind,
            status: FileStatus::Obfuscated { edits },
        }
    }

    /// A report for an eligible file that parsed but was not changed.
    pub fn unchanged(path: impl Into<PathBuf>, kind: FileKind) -> Self {
        FileReport {
            path: path.into(),
            kind,
            status: FileStatus::Unchanged,
        }
    }

    /// A report for a file that was skipped, with the reason.
    pub fn skipped(path: impl Into<PathBuf>, kind: FileKind, reason: SkipReason) -> Self {
        FileReport {
            path: path.into(),
            kind,
            status: FileStatus::Skipped(reason),
        }
    }

    /// A report for a file copied in by a `[[vendor]]` input. `obfuscated` is
    /// true when the obfuscation pass ran over it, false when it was kept
    /// verbatim.
    pub fn vendored(path: impl Into<PathBuf>, kind: FileKind, obfuscated: bool) -> Self {
        FileReport {
            path: path.into(),
            kind,
            status: FileStatus::Vendored { obfuscated },
        }
    }

    /// Whether this file was considered for processing (not skipped).
    pub fn is_eligible(&self) -> bool {
        !matches!(self.status, FileStatus::Skipped(_))
    }

    /// Whether this file was actually obfuscated: an obfuscated chunk, or a
    /// vendored file that was obfuscated on the way in.
    pub fn is_protected(&self) -> bool {
        matches!(
            self.status,
            FileStatus::Obfuscated { .. } | FileStatus::Vendored { obfuscated: true }
        )
    }

    /// Whether this file should have been protected but was not: eligible, yet
    /// left unchanged. A skipped or verbatim-vendored file is deliberate, so
    /// only an unchanged eligible file counts. This is what `--check` fails on.
    pub fn is_unprotected(&self) -> bool {
        matches!(self.status, FileStatus::Unchanged)
    }
}

/// Every file's outcome for one run, plus the roots that were walked.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// The input directories this run walked.
    pub roots: Vec<PathBuf>,
    /// One entry per file seen, in walk order.
    pub files: Vec<FileReport>,
}

impl Report {
    /// An empty report for the given roots.
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Report {
            roots,
            files: Vec::new(),
        }
    }

    /// Record one file's outcome.
    pub fn push(&mut self, file: FileReport) {
        self.files.push(file);
    }

    /// Merge another report's files into this one. The roots are kept as is.
    pub fn absorb(&mut self, other: Report) {
        self.files.extend(other.files);
    }

    /// How many files were seen in total, processed or not.
    pub fn total(&self) -> usize {
        self.files.len()
    }

    /// How many files were obfuscated in place (app chunks and shells). A
    /// vendored file that was obfuscated on the way in is counted by
    /// [`vendored`](Self::vendored), not here, so the two do not overlap.
    pub fn obfuscated(&self) -> usize {
        self.files
            .iter()
            .filter(|f| matches!(f.status, FileStatus::Obfuscated { .. }))
            .count()
    }

    /// How many eligible files were left unchanged.
    pub fn unchanged(&self) -> usize {
        self.files
            .iter()
            .filter(|f| matches!(f.status, FileStatus::Unchanged))
            .count()
    }

    /// How many files were skipped.
    pub fn skipped(&self) -> usize {
        self.files
            .iter()
            .filter(|f| matches!(f.status, FileStatus::Skipped(_)))
            .count()
    }

    /// How many files were copied in by a `[[vendor]]` input.
    pub fn vendored(&self) -> usize {
        self.files
            .iter()
            .filter(|f| matches!(f.status, FileStatus::Vendored { .. }))
            .count()
    }

    /// The eligible files that were not protected, if any. `--check` reports
    /// these and the run exits nonzero when the list is not empty.
    pub fn unprotected(&self) -> Vec<&FileReport> {
        self.files.iter().filter(|f| f.is_unprotected()).collect()
    }

    /// Whether any eligible file went unprotected.
    pub fn has_unprotected(&self) -> bool {
        self.files.iter().any(|f| f.is_unprotected())
    }

    /// The files that were obfuscated, in walk order.
    pub fn protected_files(&self) -> impl Iterator<Item = &FileReport> {
        self.files.iter().filter(|f| f.is_protected())
    }
}

/// A human-readable name for a skip reason, for report lines.
pub fn skip_reason_label(reason: SkipReason) -> &'static str {
    match reason {
        SkipReason::SourceMap => "source map",
        SkipReason::Policy => "not a processed type",
        SkipReason::AlreadyProcessed => "already obfuscated",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_with(files: Vec<FileReport>) -> Report {
        let mut r = Report::new(vec![PathBuf::from("dist")]);
        for f in files {
            r.push(f);
        }
        r
    }

    #[test]
    fn counts_add_up() {
        let r = report_with(vec![
            FileReport::obfuscated("a.js", FileKind::Js, 3),
            FileReport::unchanged("b.js", FileKind::Js),
            FileReport::skipped("c.css", FileKind::Other, SkipReason::Policy),
            FileReport::skipped("d.js.map", FileKind::Other, SkipReason::SourceMap),
        ]);
        assert_eq!(r.total(), 4);
        assert_eq!(r.obfuscated(), 1);
        assert_eq!(r.unchanged(), 1);
        assert_eq!(r.skipped(), 2);
    }

    #[test]
    fn unchanged_eligible_file_is_unprotected() {
        let r = report_with(vec![FileReport::unchanged("b.js", FileKind::Js)]);
        assert!(r.has_unprotected());
        assert_eq!(r.unprotected().len(), 1);
    }

    #[test]
    fn obfuscated_file_is_protected() {
        let r = report_with(vec![FileReport::obfuscated("a.js", FileKind::Js, 2)]);
        assert!(!r.has_unprotected());
        assert_eq!(r.obfuscated(), 1);
    }

    #[test]
    fn skipped_files_are_not_unprotected() {
        let r = report_with(vec![
            FileReport::skipped("c.css", FileKind::Other, SkipReason::Policy),
            FileReport::skipped("d.js.map", FileKind::Other, SkipReason::SourceMap),
        ]);
        assert!(!r.has_unprotected());
    }

    #[test]
    fn absorb_merges_files() {
        let mut a = report_with(vec![FileReport::obfuscated("a.js", FileKind::Js, 1)]);
        let b = report_with(vec![FileReport::unchanged("b.js", FileKind::Js)]);
        a.absorb(b);
        assert_eq!(a.total(), 2);
    }
}
