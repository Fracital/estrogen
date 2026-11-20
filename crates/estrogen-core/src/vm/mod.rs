//! The bytecode VM pass: compile an app chunk to encrypted stack bytecode.
//!
//! This is the default transform for a non-vendor app chunk, the heavier
//! sibling of the literal-pool pass. Instead of editing literals in place, it
//! compiles the chunk to a flat opcode array plus a constant pool (the shield
//! bytecode shape, [`program`]), encrypts the serialized program with estrogen's
//! own ChaCha20 pool so no bytecode or string ships in the clear, and replaces
//! the whole chunk with a small runtime that decodes and runs it.
//!
//! It raises the cost of reading a build. It is not secrecy. The pool key ships
//! in the output next to the decoder and the VM runs in the page, so anyone with
//! devtools can run the code back. See docs/SHIELD-VM.md.
//!
//! # Falling back
//!
//! The compiler handles a sound subset and returns [`compiler::Unsupported`] for
//! anything else (functions, classes, control flow, `import`/`export`, `var`,
//! and the rest). [`try_compile_chunk`] turns that into `Ok(None)`, and the
//! obfuscator falls back to the literal-pool and name passes for that chunk,
//! the same way abulxt's shield degrades a chunk to `@no-vm`. Nothing breaks; a
//! chunk the VM cannot compile is still protected by the other passes.

mod compiler;
mod layout;
pub mod opcodes;
mod program;

use std::sync::OnceLock;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_diagnostics::Severity;
use oxc_mangler::{MangleOptions, Mangler, ManglerReturn};
use oxc_parser::Parser;
use oxc_span::SourceType;
use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::format::{PoolBuilder, MARKER};
use crate::keys::FileKeys;
use crate::options::Options;

pub use compiler::Unsupported;

/// The VM runtime execution core, vendored from abulxt's shield runtime. This is
/// the maintainable, Prettier-clean source; [`runtime_iife`] mangles and minifies
/// it before it ships, so the shipped runtime carries no readable names.
const RUNTIME_JS: &str = include_str!("runtime.js");

/// Compile `source` to a VM program and return the JSON the runtime runs, or
/// `None` if the chunk is already processed, does not parse, or uses a construct
/// outside the subset. Used by the end-to-end tests, which run the JSON through
/// the vendored runtime and compare to native `eval`.
pub fn compile_module_json(source: &str, seed: &str) -> Option<String> {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, source, SourceType::mjs()).parse();
    if ret
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        return None;
    }
    match compiler::compile_program(&ret.program, seed) {
        Ok(program) => Some(program.into_json_string()),
        Err(_) => None,
    }
}

/// Try to compile a JS chunk to an encrypted VM chunk.
///
/// Returns `Ok(Some(replacement))` with the full replacement source when the
/// chunk compiles, `Ok(None)` when it is already processed, does not parse, or
/// uses a construct outside the subset (the caller then falls back), and `Err`
/// only on an internal failure drawing the pool key.
pub fn try_compile_chunk(source: &str, options: &Options) -> Result<Option<String>> {
    if crate::watermark::marked_js(source) {
        return Ok(None);
    }
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, source, SourceType::mjs()).parse();
    if ret
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        // Let the literal-pool path report the parse error (fail closed) rather
        // than deciding here; the VM just declines the chunk.
        return Ok(None);
    }
    let seed = layout_seed(options, source);
    let program = match compiler::compile_program(&ret.program, &seed) {
        Ok(p) => p,
        Err(_) => return Ok(None),
    };
    let json = program.into_json_string();
    Ok(Some(emit_chunk(&json, source, options)?))
}

/// Assemble the replacement chunk: the marker, the runtime, the encrypted
/// program as an estrogen ChaCha20 pool of one entry, and the call that decodes
/// and runs it.
fn emit_chunk(program_json: &str, source: &str, options: &Options) -> Result<String> {
    let mut keys = FileKeys::new(options, source.as_bytes())?;
    // The short generated name the chunk binds the runtime to, drawn from the
    // same key material and in the same `$XXXX` shape as the pool decoder name
    // (mirroring [`FileKeys::decoder_name`]). The chunk is a full replacement, so
    // the only other top-level name is the pool decoder; draw the helper name
    // first, then the pool, and guard against the astronomically unlikely clash.
    let vm_name = keys.decoder_name();
    let mut pool = PoolBuilder::new(keys)?;
    pool.push(program_json)?;
    let pool_name = pool.name().to_string();
    let vm_name = if vm_name == pool_name {
        format!("{vm_name}_")
    } else {
        vm_name
    };
    let decoder = pool.into_pool().emit_decoder(&pool_name);
    // The runtime IIFE evaluates to the factory; calling it with the decoded
    // program returns the entry function, which the trailing `()` runs. No part
    // of this spells the runtime's purpose: the names are generated and the
    // runtime body is mangled.
    Ok(format!(
        "/* {MARKER} */\nconst {vm_name}={runtime};\n{decoder}\n{vm_name}(JSON.parse({pool_name}(0)))();\n",
        runtime = runtime_iife(),
    ))
}

/// The runtime execution core as an inlinable expression that evaluates to the
/// factory the chunk calls. The vendored source is one IIFE expression statement;
/// this mangles every binding in it and prints it minified (see [`mangle_minify`]),
/// then strips the statement's trailing semicolon so the result is a bare
/// expression. Computed once and cached, because every VM chunk inlines it.
pub fn runtime_iife() -> String {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            let printed = mangle_minify(RUNTIME_JS);
            // `runtime.js` is a single expression statement; drop the trailing
            // `;` (and any surrounding whitespace) so the chunk can bind the
            // expression directly.
            printed.trim().trim_end_matches(';').trim_end().to_string()
        })
        .clone()
}

/// Mangle every binding in a self-contained JS source and print it minified,
/// using OXC's mangler and `oxc_codegen`. This reprints the AST, which the
/// edit-and-splice passes never do; it is allowed here because the runtime is
/// estrogen's own embedded asset, not a user chunk, so renaming and reformatting
/// it changes nothing a consumer depends on. The runtime is a closed IIFE, so
/// every name in it is internal and safe to rename. The shipped result is one
/// line of short names with no comments, no source formatting, and nothing that
/// spells an opcode name or the runtime's purpose.
fn mangle_minify(src: &str) -> String {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, src, SourceType::mjs()).parse();
    // `runtime.js` is estrogen's own asset and always parses; a parse error here
    // is a build bug, not a user input.
    debug_assert!(
        ret.diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error),
        "vm runtime.js failed to parse before mangling"
    );
    let ManglerReturn {
        scoping,
        class_private_mappings,
    } = Mangler::new()
        .with_options(MangleOptions {
            top_level: Some(true),
            ..MangleOptions::default()
        })
        .build(&ret.program);
    Codegen::new()
        .with_options(CodegenOptions::minify())
        .with_scoping(Some(scoping))
        .with_private_member_mappings(Some(class_private_mappings))
        .build(&ret.program)
        .code
}

/// Derive the opcode-layout seed for a file. Under a fixed `Options::seed` it is
/// deterministic and unique per file; otherwise it is random per run, so a build
/// gets a fresh polymorphic layout.
fn layout_seed(options: &Options, source: &str) -> String {
    let mut hasher = Sha256::new();
    match &options.seed {
        Some(seed) => {
            hasher.update(b"estrogen-vm-layout\0");
            hasher.update(seed);
            hasher.update(source.as_bytes());
        }
        None => {
            let mut rnd = [0u8; 32];
            // getrandom failing is not recoverable here; fall back to a fixed
            // label so a build still produces a (non-unique) layout.
            if getrandom::getrandom(&mut rnd).is_ok() {
                hasher.update(rnd);
            } else {
                hasher.update(b"estrogen-vm-layout-fallback");
            }
        }
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for b in &digest[..16] {
        out.push_str(&format!("{b:02x}"));
    }
    out
}
