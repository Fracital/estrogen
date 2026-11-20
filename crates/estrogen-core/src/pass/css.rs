//! CSS asset minification.
//!
//! This shrinks the `.css` files a bundler writes: it drops comments and
//! insignificant whitespace, shortens colors and values, and reprints the
//! stylesheet in its minimal form. It is the stylesheet counterpart of the SVG
//! and HTML asset steps ([`crate::pass::svg`], [`crate::pass::html`]), and it
//! replaces a site's separate CSS minify step.
//!
//! The minifier is [lightningcss](https://lightningcss.dev), which parses the
//! stylesheet, minifies it, and reprints it. estrogen already links lightningcss
//! through `oxvg`, so this adds no new compiled crate.
//!
//! CSS is not an obfuscation target: it carries no secret the way a chunk does,
//! so estrogen does not rewrite it, it only shrinks it. The obfuscator drives this
//! as an asset step, outside the eligibility gate that governs JS, HTML and SVG.
//!
//! It is safe. The result comes from a parsed stylesheet reprinted by
//! lightningcss, so it is valid CSS by construction. On any parse or print error
//! [`minify_css_asset`] returns `None` and the original file is kept, so a
//! malformed stylesheet is left exactly as the bundler wrote it. A result that
//! reprints to nothing is also kept, so an input is never replaced by an empty
//! file.

use lightningcss::stylesheet::{MinifyOptions, ParserOptions, PrinterOptions, StyleSheet};

/// Minify one stylesheet, or `None` to keep the original.
///
/// Returns `Some(out)` with the minified CSS when the stylesheet parsed, minified
/// and reprinted, and the result still has content. The result is not guaranteed
/// smaller: the caller writes it only when it is, but scans it for canaries
/// whenever it parsed, so a sensitive literal is caught even in a stylesheet that
/// was already minimal. Returns `None` on any parse, minify or print error, or
/// when the result reprints to nothing, in which case the original is kept.
pub(crate) fn minify_css_asset(source: &str) -> Option<String> {
    let mut sheet = StyleSheet::parse(source, ParserOptions::default()).ok()?;
    sheet.minify(MinifyOptions::default()).ok()?;
    let out = sheet
        .to_css(PrinterOptions {
            minify: true,
            ..Default::default()
        })
        .ok()?
        .code;
    // A result that lost everything (an empty or whitespace-only reprint of a
    // non-empty input) is suspect; keep the original rather than write an empty
    // file.
    if out.trim().is_empty() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fat stylesheet: comments, indentation, long color and length forms, and a
    // trailing newline, all of which minification removes or shortens.
    const FAT: &str = r#"/* theme */
.button {
    color: #ffffff;
    background-color: #000000;
    margin: 0px 0px 0px 0px;
    padding: 10px;
}

.button:hover {
    color: #ffffff;
}
"#;

    #[test]
    fn a_fat_stylesheet_shrinks_and_stays_valid() {
        let out = minify_css_asset(FAT).expect("fat css should minify");
        assert!(out.len() < FAT.len(), "output must be smaller: {out}");
        assert!(!out.contains("/* theme */"), "comment survived: {out}");
        // The selectors and a declaration are still there, so it is still CSS.
        assert!(out.contains(".button"), "selector lost: {out}");
        assert!(out.contains("padding"), "declaration lost: {out}");
        // #ffffff shortens to #fff.
        assert!(!out.contains("#ffffff"), "color not shortened: {out}");
    }

    #[test]
    fn minification_is_idempotent() {
        let once = minify_css_asset(FAT).expect("first pass minifies");
        let twice = minify_css_asset(&once).expect("minified css still parses");
        assert_eq!(once, twice, "a second pass must not change minified css");
    }

    #[test]
    fn a_malformed_stylesheet_fails_closed() {
        // An unterminated attribute selector: lightningcss reports a parse error
        // (estrogen parses strictly and does not recover), so the original is kept.
        // A CSS parser recovers an unclosed block at end of input, which is valid
        // per spec, so this uses a genuine selector error instead.
        let bad = "a[ { color: red }";
        assert_eq!(minify_css_asset(bad), None);
    }

    #[test]
    fn an_empty_rule_does_not_become_an_empty_file() {
        // `body{}` has no declarations; minification drops the empty rule. The
        // result would be empty, so the original is kept rather than written blank.
        assert_eq!(minify_css_asset("body{}"), None);
    }
}
