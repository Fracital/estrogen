//! Final JS minification for the edit-and-splice output.
//!
//! The literal and proxy passes edit the source by byte range, so their output
//! keeps the input's formatting, newlines and all. estrogen's VM path already
//! ships one line ([`crate::vm`]); this collapses the edit-and-splice output to
//! one line too, so every JS chunk estrogen writes reads as a single line.
//!
//! The collapse is a reparse and reprint, not a text scrub. The edited chunk is
//! parsed with OXC and reprinted with [`CodegenOptions::minify`], which drops
//! comments, collapses whitespace, and inserts the semicolons that make
//! automatic semicolon insertion (ASI) a non-issue: the one line cannot mean
//! anything the multi-line text did not, because the printer resolves every
//! statement boundary with a semicolon instead of a newline. The obfuscation
//! survives the reprint untouched: the encrypted pool array, the decoder IIFE,
//! and the `NAME(i)` decode calls are all plain JS the printer keeps verbatim.
//! Only the watermark comment is dropped, so it is re-added as the single
//! leading comment, on the same line as the code, exactly like the VM chunk.
//!
//! The `\n`-to-backtick shortening the user asked for is already part of this
//! reprint: `oxc_codegen`'s minifier prints a string that holds a `\n` as a
//! backtick template with a real newline (one byte instead of two), and that is
//! the only place a newline appears in the output. estrogen does not add a
//! separate transform for it; the printer owns it, and `newline_only_in_template`
//! in the tests pins the behavior so a codegen upgrade that drops it is caught.

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_diagnostics::Severity;
use oxc_parser::Parser;

use crate::format::MARKER;
use crate::pass::source_type_for;

/// Reprint an edited JS chunk as a single line, or `None` to keep the edited
/// text as it is.
///
/// `source` is the edit-and-splice output of the literal or proxy pass: valid
/// JS that still carries the watermark comment and the input's newlines. It is
/// parsed and reprinted minified, then the watermark is re-added at the head.
/// `None` means the reprint cannot be trusted (the chunk did not parse), and the
/// caller keeps the un-minified output and its source map. The caller re-parses
/// the returned line and re-scans it for canaries, so a reprint that broke the
/// chunk or surfaced a canary literal is caught there and the edited output kept
/// instead. This never writes a file and never fails the run; the decision to
/// ship the line or the edited text is the caller's.
pub fn minify_js(path: &Path, source: &str) -> Option<String> {
    let allocator = Allocator::default();
    let source_type = source_type_for(path);
    let ret = Parser::new(&allocator, source, source_type).parse();
    // The edited chunk already re-parsed once (the caller runs
    // `ensure_js_reparses` before this), so a parse error here is not expected;
    // fall back rather than reprint a half-parsed AST.
    if ret
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        return None;
    }
    let printed = Codegen::new()
        .with_options(CodegenOptions::minify())
        .build(&ret.program)
        .code;
    // Re-add the watermark the reprint dropped, as the single leading comment on
    // the same line as the code, matching [`crate::vm`]'s chunk. `trim_start`
    // guards against a stray leading newline the printer could emit.
    Some(format!("/* {MARKER} */{}", printed.trim_start()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn min(src: &str) -> String {
        minify_js(Path::new("app.js"), src).expect("valid js should reprint")
    }

    #[test]
    fn collapses_a_multi_line_chunk_to_one_line() {
        let src = "const a = 1;\nconst b = 2;\nconst c = a + b;\n";
        let out = min(src);
        assert!(out.lines().count() == 1, "expected one line, got: {out:?}");
        assert!(out.contains("const a=1"));
        assert!(out.contains("const c=a+b"));
    }

    #[test]
    fn re_adds_the_marker_on_the_code_line() {
        // The marker comment is dropped by the reprint and re-added at the head,
        // on the same line as the code (no newline after `*/`).
        let src = format!("/* {MARKER} */\nconst a = 1;\n");
        let out = min(&src);
        assert!(out.starts_with(&format!("/* {MARKER} */")));
        assert_eq!(out.lines().count(), 1, "marker must share the code line");
        // The marker appears once: the reprint dropped the original, and only the
        // re-added one is present.
        assert_eq!(out.matches(MARKER).count(), 1);
    }

    #[test]
    fn inserts_semicolons_so_asi_cannot_change_meaning() {
        // Two statements whose newline is the only separator. Collapsed onto one
        // line, a bare space would merge them into `a(b)` (a call) under ASI; the
        // printer inserts a semicolon so `b` stays its own statement.
        let src = "const a = 1\nconst b = 2\n(a)\n";
        let out = min(src);
        assert_eq!(out.lines().count(), 1);
        // Re-parsing is the real guard (the caller does it), but the printed form
        // must carry explicit boundaries, not rely on newlines.
        crate::pass::ensure_js_reparses(Path::new("app.js"), &out).unwrap();
    }

    #[test]
    fn newline_only_in_a_template_literal() {
        // A string with a `\n` escape prints as a backtick template with a real
        // newline (one byte, not two). That newline is the only one allowed in
        // the output, and it sits inside the backticks. This pins codegen's
        // behavior: it owns the `\n`-to-backtick shortening the user asked for.
        let out = min("const a = \"x\\ny\";\n");
        assert!(out.contains('`'), "expected a backtick template: {out:?}");
        assert!(
            out.contains("x\ny"),
            "expected a real newline inside: {out:?}"
        );
        // Outside the single backtick span the line has no newline: split on the
        // backticks and assert the non-template parts are newline free.
        let parts: Vec<&str> = out.split('`').collect();
        assert!(
            parts[0].chars().all(|c| c != '\n') && parts[2].chars().all(|c| c != '\n'),
            "a newline escaped the template: {out:?}"
        );
    }

    #[test]
    fn broken_js_returns_none() {
        assert!(minify_js(Path::new("app.js"), "const a = (((;").is_none());
    }

    #[test]
    fn reprint_preserves_a_decode_call_and_pool_array() {
        // A stand-in for the literal-pass shape: a decoder-like binding, a pool
        // of base64 strings, and a `NAME(i)` decode call. The reprint keeps the
        // string values and the call, so the obfuscation is intact. The printer
        // is free to pick the quote style (it prints the base64 strings as
        // backtick templates), so the test checks the values, not the quoting.
        let src = "var N = function(i){ return P[i]; };\nvar P = [\"AAAA\", \"BBBB\"];\nconsole.log(N(0));\n";
        let out = min(src);
        assert!(
            out.contains("AAAA") && out.contains("BBBB"),
            "pool lost: {out}"
        );
        assert!(out.contains("N(0)"), "decode call lost: {out}");
        assert_eq!(out.lines().count(), 1);
    }
}
