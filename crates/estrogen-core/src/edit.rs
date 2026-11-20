//! Edit-and-splice core.
//!
//! This is the decision that shapes estrogen: it edits the original source text,
//! it does not reprint the AST. A pass walks the parse tree, finds the byte
//! ranges it wants to change, and records each one as an [`Edit`]. [`EditSet`]
//! sorts the edits and splices them into the original source string. Every byte
//! a pass does not touch stays exactly as the bundler emitted it: formatting,
//! comments, and the decoder a later pass prepends.
//!
//! Offsets are UTF-8 byte offsets, which is what `oxc_span` reports and what
//! Rust string slicing uses. The reference `obfuscate-build.mjs` works in
//! acorn's UTF-16 code-unit offsets and splices by those; estrogen splices bytes,
//! so ports of its logic match by position role, not by copying the numbers.

use crate::error::{Error, Result};

/// One replacement over a byte range of the source.
///
/// `start..end` is a half-open byte range into the original source string.
/// `replacement` takes its place. An insertion is `start == end`; a deletion is
/// an empty `replacement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// First byte of the range, inclusive.
    pub start: usize,
    /// One past the last byte of the range, exclusive.
    pub end: usize,
    /// Text that replaces the range.
    pub replacement: String,
}

impl Edit {
    /// Replace `start..end` with `replacement`.
    pub fn replace(start: usize, end: usize, replacement: impl Into<String>) -> Self {
        Edit {
            start,
            end,
            replacement: replacement.into(),
        }
    }

    /// Insert `text` at `at`, changing no existing bytes.
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Edit {
            start: at,
            end: at,
            replacement: text.into(),
        }
    }
}

/// A set of edits for one file, applied together.
///
/// A pass pushes edits in any order. [`EditSet::apply`] sorts them by start
/// offset and splices them into the source in one pass. Overlapping ranges and
/// ranges outside the source are errors, because the passes are meant to produce
/// disjoint in-bounds edits; estrogen fails closed rather than guess.
#[derive(Debug, Default, Clone)]
pub struct EditSet {
    edits: Vec<Edit>,
}

impl EditSet {
    /// An empty set, which applies as the identity.
    pub fn new() -> Self {
        EditSet { edits: Vec::new() }
    }

    /// Add one edit.
    pub fn push(&mut self, edit: Edit) {
        self.edits.push(edit);
    }

    /// How many edits are queued.
    pub fn len(&self) -> usize {
        self.edits.len()
    }

    /// Whether no edits are queued. An empty set leaves the source unchanged.
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// Read-only view of the queued edits, in push order.
    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }

    /// Move the other set's edits into this one.
    pub fn extend(&mut self, other: EditSet) {
        self.edits.extend(other.edits);
    }

    /// The edits in the order [`apply`](Self::apply) splices them: by start
    /// offset, then by end, stably. A source map is built by walking these in the
    /// same order as the output is assembled.
    pub fn sorted(&self) -> Vec<&Edit> {
        let mut ordered: Vec<&Edit> = self.edits.iter().collect();
        ordered.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
        ordered
    }

    /// Splice the edits into `source` and return the rewritten string.
    ///
    /// The edits are sorted by start offset first, so a pass may push them in
    /// any order. An empty set returns `source` unchanged. Two edits that cover
    /// the same bytes give [`Error::OverlappingEdits`]; a range past the end of
    /// `source` gives [`Error::EditOutOfBounds`]. A zero-width insertion at the
    /// same point as a replacement is allowed and is emitted before the
    /// replacement.
    pub fn apply(&self, source: &str) -> Result<String> {
        if self.edits.is_empty() {
            return Ok(source.to_string());
        }

        let mut ordered: Vec<&Edit> = self.edits.iter().collect();
        // Sort by start, then by end so a zero-width insert at a point comes
        // before a replacement that begins there.
        ordered.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));

        let len = source.len();
        let mut out = String::with_capacity(len);
        let mut cursor = 0usize;
        let mut prev_end = 0usize;

        for (i, edit) in ordered.iter().enumerate() {
            if edit.end > len || edit.start > edit.end {
                return Err(Error::EditOutOfBounds {
                    start: edit.start,
                    end: edit.end,
                    len,
                });
            }
            if i > 0 && edit.start < prev_end {
                let prev = ordered[i - 1];
                return Err(Error::OverlappingEdits {
                    a_start: prev.start,
                    a_end: prev.end,
                    b_start: edit.start,
                    b_end: edit.end,
                });
            }
            out.push_str(&source[cursor..edit.start]);
            out.push_str(&edit.replacement);
            cursor = edit.end;
            prev_end = edit.end;
        }
        out.push_str(&source[cursor..]);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_set_is_identity() {
        let set = EditSet::new();
        assert!(set.is_empty());
        assert_eq!(set.apply("hello world").unwrap(), "hello world");
    }

    #[test]
    fn single_replacement() {
        let mut set = EditSet::new();
        set.push(Edit::replace(0, 5, "HELLO"));
        assert_eq!(set.apply("hello world").unwrap(), "HELLO world");
    }

    #[test]
    fn unsorted_input_is_sorted_before_apply() {
        let mut set = EditSet::new();
        // Pushed out of order on purpose.
        set.push(Edit::replace(6, 11, "there"));
        set.push(Edit::replace(0, 5, "HELLO"));
        assert_eq!(set.apply("hello world").unwrap(), "HELLO there");
    }

    #[test]
    fn insertion_keeps_surrounding_bytes() {
        let mut set = EditSet::new();
        set.push(Edit::insert(5, "!!"));
        assert_eq!(set.apply("hello world").unwrap(), "hello!! world");
    }

    #[test]
    fn deletion_removes_range() {
        let mut set = EditSet::new();
        set.push(Edit::replace(5, 6, ""));
        assert_eq!(set.apply("hello world").unwrap(), "helloworld");
    }

    #[test]
    fn overlap_is_rejected() {
        let mut set = EditSet::new();
        set.push(Edit::replace(0, 6, "x"));
        set.push(Edit::replace(3, 9, "y"));
        assert!(matches!(
            set.apply("hello world").unwrap_err(),
            Error::OverlappingEdits { .. }
        ));
    }

    #[test]
    fn out_of_bounds_is_rejected() {
        let mut set = EditSet::new();
        set.push(Edit::replace(0, 100, "x"));
        assert!(matches!(
            set.apply("short").unwrap_err(),
            Error::EditOutOfBounds { .. }
        ));
    }

    #[test]
    fn non_ascii_bytes_round_trip() {
        // "café" is 5 bytes; edit only the ascii tail, leave the é alone.
        let src = "café bar";
        let mut set = EditSet::new();
        set.push(Edit::replace(6, 9, "BAZ"));
        assert_eq!(set.apply(src).unwrap(), "café BAZ");
    }

    #[test]
    fn adjacent_edits_do_not_overlap() {
        let mut set = EditSet::new();
        set.push(Edit::replace(0, 5, "HELLO"));
        set.push(Edit::replace(5, 11, "-WORLD"));
        assert_eq!(set.apply("hello world").unwrap(), "HELLO-WORLD");
    }
}
