//! SVG asset optimization.
//!
//! This shrinks the `.svg` files a bundler writes: it drops editor metadata and
//! comments, collapses whitespace, rounds path numbers, removes default
//! attributes and the rest of the SVGO default job set. It replaces the
//! hand-written `svgmin` step a site otherwise keeps (the prior art is
//! website2's `svgmin.mjs`, which runs `svgo` with its default preset over each
//! imported icon).
//!
//! The optimizer is [`oxvg`](https://github.com/noahbald/oxvg), the Rust port of
//! SVGO. [`oxvg_optimiser::Jobs::default`] is the same default job set SVGO runs,
//! so the output matches what `svgmin.mjs` produces on the same input.
//!
//! This step is separate from the HTML and SVG cloak pass ([`crate::pass::markup`]).
//! The cloak pass neutralizes a canary `<title>` or `<meta>` and encrypts an
//! inline `<script>`; this one optimizes the vector asset. A cloak SVG carries an
//! inline `<script>` whose body is not XML the optimizer should reparse, so
//! [`is_cloak_or_marked`] leaves those for the cloak pass and only plain image
//! assets are optimized. When an asset is optimized, the cloak pass still runs
//! over the optimized text, so a canary in an asset's `<title>` is still caught.
//!
//! It is safe. The optimizer parses the document and reserializes it, so a
//! result is well formed by construction. On any optimizer error the original is
//! kept ([`optimize_svg_asset`] returns `None`), so estrogen never writes an SVG
//! it could not optimize cleanly. The result is used only when it is strictly
//! smaller than the input, which also makes the step idempotent: a second run
//! over an already-optimized asset finds nothing smaller and leaves it alone.

use oxvg_ast::parse::roxmltree::parse;
use oxvg_ast::serialize::Node as _;
use oxvg_ast::visitor::Info;
use oxvg_optimiser::Jobs;

/// Whether this SVG should be left for the cloak pass instead of optimized.
///
/// True when the document carries an inline `<script>` (a cloak, whose script
/// body the cloak pass obfuscates and whose XML the optimizer should not
/// reparse) or already carries estrogen's output marker (a second run must not
/// reshape it). The check is a case-insensitive substring scan, matching how the
/// markup pass locates tags; it never parses, so a malformed cloak is still
/// skipped here rather than handed to the optimizer.
pub(crate) fn is_cloak_or_marked(source: &str) -> bool {
    if crate::watermark::marked_markup(source, true) {
        return true;
    }
    source.to_ascii_lowercase().contains("<script")
}

/// Optimize one SVG asset, returning the smaller result or `None` to keep the
/// original.
///
/// Runs the SVGO default job set over the document and reserializes it. Returns
/// `Some(out)` only when the optimizer succeeded, the output still has an `<svg`
/// root, and it is strictly smaller than `source`. Returns `None` on any
/// optimizer or serialization error, on a result that lost its root, or on a
/// result no smaller than the input. A `None` means the caller keeps the
/// original file unchanged, so a malformed SVG fails closed to its own bytes and
/// an already-minimal asset is left alone.
pub(crate) fn optimize_svg_asset(source: &str) -> Option<String> {
    // parse returns the closure's value, or an error if the XML will not parse.
    // The closure runs the jobs and serializes, returning an error of its own on
    // a jobs or writer failure. Either error means: keep the original.
    let serialized: String = match parse(source, |dom, allocator| {
        let jobs = Jobs::default();
        jobs.run(dom, &Info::new(allocator))
            .map_err(|e| e.to_string())?;
        dom.serialize().map_err(|e| e.to_string())
    }) {
        Ok(Ok(s)) => s,
        Ok(Err(_)) | Err(_) => return None,
    };

    // A result that lost its root is not an SVG; keep the original.
    if !serialized.to_ascii_lowercase().contains("<svg") {
        return None;
    }
    // Only take the result when it actually shrank. Equal length is treated as no
    // win, which keeps the step idempotent.
    if serialized.len() < source.len() {
        Some(serialized)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A deliberately fat SVG: an XML declaration, a comment, editor metadata, a
    // lot of whitespace, and over-precise path numbers. Every one of these is
    // something the default job set strips or shortens.
    const FAT: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<!-- drawn by hand -->
<svg xmlns="http://www.w3.org/2000/svg" xmlns:sketch="http://www.bohemiancoding.com/sketch/ns" width="24" height="24" viewBox="0 0 24 24">
    <metadata>
        <sketch:metadata>editor junk</sketch:metadata>
    </metadata>
    <title>icon</title>
    <g fill="none" fill-rule="evenodd">
        <path d="M12.00000,2.000000 L22.0000000,22.00000 L2.0000000,22.000000 Z" fill="#000000"></path>
    </g>
</svg>
"##;

    #[test]
    fn a_fat_svg_shrinks_and_stays_valid() {
        let out = optimize_svg_asset(FAT).expect("fat svg should shrink");
        assert!(out.len() < FAT.len(), "output must be smaller");
        // Still an SVG, and the editor junk and comment are gone.
        assert!(out.to_ascii_lowercase().contains("<svg"));
        assert!(!out.contains("drawn by hand"), "comment survived: {out}");
        assert!(!out.contains("editor junk"), "metadata survived: {out}");
        // The path numbers were rounded, so the long zero runs are gone.
        assert!(!out.contains("2.000000"), "numbers not rounded: {out}");
    }

    #[test]
    fn optimization_is_idempotent() {
        let once = optimize_svg_asset(FAT).expect("first pass shrinks");
        // A second pass over the optimized asset finds nothing smaller.
        assert_eq!(
            optimize_svg_asset(&once),
            None,
            "an already-optimized asset must not shrink again"
        );
    }

    #[test]
    fn a_malformed_svg_fails_closed() {
        // Unclosed tag: roxmltree cannot parse it, so the optimizer errors and the
        // original is kept (None), never a reshaped or truncated file.
        let bad = "<svg><g><path d=\"M0 0\"></svg>";
        assert_eq!(optimize_svg_asset(bad), None);
    }

    #[test]
    fn an_already_minimal_svg_is_left_alone() {
        // Nothing to strip and no whitespace to collapse: no strictly smaller
        // result, so None.
        let tiny = r#"<svg xmlns="http://www.w3.org/2000/svg"/>"#;
        assert_eq!(optimize_svg_asset(tiny), None);
    }

    #[test]
    fn a_cloak_svg_is_left_for_the_markup_pass() {
        let cloak = r#"<svg xmlns="http://www.w3.org/2000/svg"><script>var a = 1;</script></svg>"#;
        assert!(is_cloak_or_marked(cloak));
        // An asset with no script is not a cloak.
        let asset = r#"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0"/></svg>"#;
        assert!(!is_cloak_or_marked(asset));
    }

    #[test]
    fn a_marked_svg_is_left_alone() {
        // The cloak pass inserts the marker ahead of the root, so a re-run sees it
        // at the document head.
        let marked = format!(
            r#"<!-- {} --><svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0"/></svg>"#,
            crate::format::MARKER
        );
        assert!(is_cloak_or_marked(&marked));
    }
}
