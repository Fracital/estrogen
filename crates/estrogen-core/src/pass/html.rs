//! HTML shell minification.
//!
//! This shrinks the `.html` shells a bundler writes: it strips comments and
//! collapses insignificant whitespace between tags. It runs before the HTML and
//! SVG cloak pass ([`crate::pass::markup`]), so the cloak pass still neutralizes
//! a canary `<title>` or `<meta>` and encrypts a canary-bearing inline
//! `<script>` over the minified text. It is the HTML counterpart of the SVG and
//! CSS asset steps ([`crate::pass::svg`], [`crate::pass::css`]).
//!
//! The minifier is [minify-html](https://github.com/wilsonzlin/minify-html)'s
//! one-pass engine. It is HTML-aware, so it never touches the content of
//! `<pre>`, `<textarea>`, `<script>` or `<style>`, and it keeps significant
//! whitespace around inline elements. Its own JS and CSS minifiers are left off:
//! estrogen's literal pool and the CSS asset step are the only things that rewrite
//! script and style bodies, and the cloak pass needs the inline script body
//! intact to find and encrypt it.
//!
//! The order matters. Minification runs first and leaves every `<script>` body as
//! it was, then the cloak pass obfuscates a canary-bearing one. So estrogen never
//! minifies an already-obfuscated script; it minifies around the original one and
//! the cloak pass rewrites it afterward.
//!
//! It is safe. The one-pass engine returns an error on malformed markup (a
//! mismatched or unexpected closing tag, an unexpected end), which is the
//! fail-closed path: [`minify_html_doc`] returns `None` and the original file is
//! kept. A result is used only when it is strictly smaller, which also makes the
//! step idempotent: a second run finds nothing smaller and changes nothing.

use minify_html_onepass::{copy, Cfg};

/// Minify one HTML document, or `None` to keep the original.
///
/// Returns `Some(out)` with the minified HTML when the one-pass engine succeeded,
/// the result is valid UTF-8, and it is strictly smaller than `source`. Returns
/// `None` on a minifier error (malformed markup), on a non-UTF-8 result, or when
/// the result is no smaller, in which case the caller keeps the original. The JS
/// and CSS minifiers are off, so `<script>` and `<style>` bodies are left exactly
/// as they were for the cloak pass and the CSS step.
pub(crate) fn minify_html_doc(source: &str) -> Option<String> {
    let cfg = Cfg {
        minify_js: false,
        minify_css: false,
    };
    // `copy` minifies a copy of the bytes and returns an error on malformed
    // markup, leaving the original untouched.
    let out = copy(source.as_bytes(), &cfg).ok()?;
    let out = String::from_utf8(out).ok()?;
    if out.is_empty() || out.len() >= source.len() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fat shell: a comment, indentation and newlines between tags, and a
    // whitespace-sensitive <pre> and <script> whose insides must survive.
    const FAT: &str = r#"<!doctype html>
<html>
    <head>
        <!-- build comment -->
        <title>Home</title>
    </head>
    <body>
        <p>hello</p>
        <pre>  indented
  text  </pre>
        <script>var   x   =   1;</script>
    </body>
</html>
"#;

    #[test]
    fn a_fat_shell_shrinks_and_stays_valid() {
        let out = minify_html_doc(FAT).expect("fat html should shrink");
        assert!(out.len() < FAT.len(), "output must be smaller: {out}");
        assert!(!out.contains("build comment"), "comment survived: {out}");
        // The structure is intact.
        assert!(out.contains("<title>Home</title>"), "title lost: {out}");
        // The inter-tag whitespace between <p> and <pre> was collapsed.
        assert!(
            !out.contains("</p>\n        <pre>"),
            "inter-tag whitespace not collapsed: {out}"
        );
    }

    #[test]
    fn whitespace_inside_pre_and_script_is_preserved() {
        let out = minify_html_doc(FAT).expect("fat html should shrink");
        // The <pre> content keeps its exact spacing.
        assert!(
            out.contains("  indented\n  text  "),
            "pre content was altered: {out}"
        );
        // The <script> body is left exactly as it was (minify_js off).
        assert!(
            out.contains("var   x   =   1;"),
            "script body was altered: {out}"
        );
    }

    #[test]
    fn minification_is_idempotent() {
        let once = minify_html_doc(FAT).expect("first pass shrinks");
        // A second pass over the minified shell finds nothing smaller.
        assert_eq!(
            minify_html_doc(&once),
            None,
            "an already-minified shell must not shrink again"
        );
    }

    #[test]
    fn malformed_html_fails_closed() {
        // A mismatched closing tag: the one-pass engine errors, so the original is
        // kept (None), never a reshaped or truncated document.
        let bad = "<div><p></div></section>";
        assert_eq!(minify_html_doc(bad), None);
    }
}
