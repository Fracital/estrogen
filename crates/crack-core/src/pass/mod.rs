//! Obfuscation passes.
//!
//! Each pass reads a file's source and the run [`Options`], and returns an
//! [`EditSet`] of byte-range edits to splice into that source. A pass never
//! reprints the AST; it locates ranges with `oxc_span` offsets and rewrites the
//! original string. See [`crate::edit`] for why.
//!
//! The passes mirror `obfuscate-build.mjs`, one module each:
//! - [`literals`]: string and template literals into an encrypted pool.
//! - [`names`]: proxy and store names into per-build substitutes.
//! - [`wasm`]: the embedded wasm names and base64.
//! - [`markup`]: the HTML shells and the SVG cloak.
//!
//! The engine is not built yet. Every pass here is an identity stub: it parses
//! (so a parse failure still fails closed) and returns no edits. The real edit
//! collection is the engine work, cited against the reference in each module.

pub mod literals;
pub mod markup;
pub mod names;
pub mod wasm;

use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
use oxc_ast::ast::StringLiteral;
use oxc_ast_visit::Visit;
use oxc_diagnostics::Severity;
use oxc_parser::Parser;
use oxc_span::SourceType;

use crate::edit::EditSet;
use crate::error::{Error, Result};
use crate::options::Options;
use crate::walk::FileKind;

/// What a pass is given: the file being processed and the run options.
///
/// `source` is the original file text. A pass borrows it, collects edits against
/// its byte offsets, and returns them; the caller splices them in.
pub struct FileContext<'a> {
    /// The file's path, for error messages and name hints.
    pub path: PathBuf,
    /// Its kind by extension.
    pub kind: FileKind,
    /// The original source text.
    pub source: &'a str,
    /// The run options (names, canaries, seed, and the rest).
    pub options: &'a Options,
}

impl<'a> FileContext<'a> {
    /// Build a context for one file.
    pub fn new(path: impl Into<PathBuf>, kind: FileKind, source: &'a str, options: &'a Options) -> Self {
        FileContext {
            path: path.into(),
            kind,
            source,
            options,
        }
    }
}

/// One obfuscation pass.
///
/// A pass is pure over its input: given a [`FileContext`] it returns the edits
/// it wants, or an error if the file cannot be handled. It changes no files
/// itself; the caller applies the edits and writes the result.
pub trait Pass {
    /// A short name for reports and error messages.
    fn name(&self) -> &'static str;

    /// Collect this pass's edits for the file.
    ///
    /// In the identity milestone this parses (for JS) and returns an empty set.
    /// A parse failure is returned as [`Error::Parse`], so a file crack cannot
    /// read never ships.
    fn collect_edits(&self, ctx: &FileContext) -> Result<EditSet>;
}

/// Pick the OXC source type for a JS file. `.cjs` is a classic script; every
/// other JS extension is an ES module, which is what Vite emits.
pub fn source_type_for(path: &Path) -> SourceType {
    match path.extension().and_then(|e| e.to_str()) {
        Some("cjs") => SourceType::cjs(),
        _ => SourceType::mjs(),
    }
}

/// Parse a JS file and fail closed on any error-level diagnostic.
///
/// crack never ships a file it cannot read, so a parse error is fatal for that
/// file. This runs the parse inside a fresh [`Allocator`] and calls `walk` with
/// the parsed program, so a pass can collect edits from the AST without owning
/// the arena. The program and the arena live only for the duration of `walk`.
pub fn with_parsed_js<T>(
    path: &Path,
    source: &str,
    walk: impl FnOnce(&oxc_ast::ast::Program) -> Result<T>,
) -> Result<T> {
    let allocator = Allocator::default();
    let source_type = source_type_for(path);
    let ret = Parser::new(&allocator, source, source_type).parse();
    if let Some(err) = ret
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
    {
        return Err(Error::Parse {
            path: path.to_path_buf(),
            message: err.to_string(),
        });
    }
    walk(&ret.program)
}

/// A read-only visitor that counts string literals.
///
/// It does not collect edits. It exists so the identity milestone exercises the
/// `oxc_ast_visit` walk end to end: the literals pass runs it to prove the AST
/// is reachable before the real pool work replaces it. The count is reported as
/// a diagnostic aid, not acted on.
#[derive(Default)]
pub struct StringLiteralCounter {
    /// How many string literals were seen.
    pub count: usize,
}

impl<'a> Visit<'a> for StringLiteralCounter {
    fn visit_string_literal(&mut self, _lit: &StringLiteral<'a>) {
        self.count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn valid_js_parses_and_counts_literals() {
        let src = "const a = 'x'; const b = 'y'; const c = `t${a}`;";
        let n = with_parsed_js(Path::new("app.js"), src, |program| {
            let mut c = StringLiteralCounter::default();
            c.visit_program(program);
            Ok(c.count)
        })
        .unwrap();
        // Two string literals; the template quasi is not a StringLiteral.
        assert_eq!(n, 2);
    }

    #[test]
    fn broken_js_fails_closed() {
        let src = "const a = (((;";
        let err = with_parsed_js(Path::new("bad.js"), src, |_| Ok(())).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }));
    }

    #[test]
    fn cjs_is_parsed_as_script() {
        assert!(!source_type_for(Path::new("x.cjs")).is_module());
        assert!(source_type_for(Path::new("x.js")).is_module());
        assert!(source_type_for(Path::new("x.mjs")).is_module());
    }
}
