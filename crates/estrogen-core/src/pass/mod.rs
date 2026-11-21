//! Obfuscation passes.
//!
//! Each pass reads a file's source and the run [`Options`], and returns an
//! [`EditSet`] of byte-range edits to splice into that source. A pass never
//! reprints the AST; it locates ranges with `oxc_span` offsets and rewrites the
//! original string. See [`crate::edit`] for why.
//!
//! The literal, vendor and markup transforms follow `obfuscate-build.mjs`:
//! - [`literals`]: string and template literals into an encrypted pool.
//! - [`names`]: the proxy and store name transform for vendor files, which also
//!   maps the embedded wasm names through the crate's `wasm` module.
//! - [`markup`]: the HTML shells and the SVG cloak.
//! - [`svg`]: SVG asset optimization, the Rust port of SVGO's default job set.
//!   It runs before the cloak pass over plain image assets, not from
//!   `obfuscate-build.mjs` but replacing a site's separate `svgmin` step.
//! - [`html`]: HTML shell minification, run before the cloak pass.
//! - [`css`]: CSS asset minification. These three asset steps shrink output a
//!   bundler wrote; they do not obfuscate, and they are not from
//!   `obfuscate-build.mjs`.
//!
//! A pass parses the file (so a parse failure fails closed), walks it, and
//! returns the byte-range edits it wants; the obfuscator applies them and
//! re-parses the output. Each JS file takes one transform: the proxy path for a
//! vendor file, else the literal pool and local renames.

// The asset optimizers (svg, html, css) pull heavy native dependencies (oxvg,
// minify-html, lightningcss). They are not built for wasm32, so the in-browser
// binding stays lean; the obfuscator's calls into them are compiled out there
// too.
#[cfg(not(target_arch = "wasm32"))]
pub mod css;
mod deadcode;
#[cfg(not(target_arch = "wasm32"))]
pub mod html;
pub mod literals;
mod mangle;
pub mod markup;
pub mod names;
#[cfg(not(target_arch = "wasm32"))]
pub mod svg;

use std::path::{Path, PathBuf};

use oxc_allocator::Allocator;
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
    pub fn new(
        path: impl Into<PathBuf>,
        kind: FileKind,
        source: &'a str,
        options: &'a Options,
    ) -> Self {
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
    /// The pass parses the file (for JS) and returns the byte-range edits it
    /// wants. A parse failure is returned as [`Error::Parse`], so a file estrogen
    /// cannot read never ships.
    fn collect_edits(&self, ctx: &FileContext) -> Result<EditSet>;
}

/// Insert the watermark after the hashbang and the decoder after directives.
/// A local-only rename needs the watermark but no key or decoder in the file.
fn assemble_js(
    edits: &mut EditSet,
    source: &str,
    program: &oxc_ast::ast::Program<'_>,
    pool: crate::format::PoolBuilder,
) {
    if edits.is_empty() {
        return;
    }
    let start = crate::watermark::js_start(source);
    edits.push(crate::edit::Edit::insert(
        start,
        format!("/* {} */", crate::format::MARKER),
    ));
    if !pool.is_empty() {
        let name = pool.name().to_owned();
        let decoder = pool.into_pool().emit_decoder(&name);
        let (at, prefix) = program
            .directives
            .last()
            .map_or((start, ""), |d| (d.span.end as usize, "\n"));
        edits.push(crate::edit::Edit::insert(
            at,
            format!("{prefix}{decoder}\n"),
        ));
    }
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
/// estrogen never ships a file it cannot read, so a parse error is fatal for that
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

/// Re-parse obfuscated JS output and fail closed if it does not parse.
///
/// This is the safety net the reference `assemble` keeps: after a pass rewrites
/// a chunk, the result must still be valid JS. A pass that produces unparseable
/// output is a bug, not bad input, so estrogen refuses to write the file rather
/// than ship a broken chunk. The error names the file and marks it as the
/// obfuscated output, not the source.
pub fn ensure_js_reparses(path: &Path, output: &str) -> Result<()> {
    let allocator = Allocator::default();
    let source_type = source_type_for(path);
    let ret = Parser::new(&allocator, output, source_type).parse();
    if let Some(err) = ret
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
    {
        return Err(Error::Parse {
            path: path.to_path_buf(),
            message: format!("obfuscated output did not re-parse: {err}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
