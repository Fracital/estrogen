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

use oxc_allocator::Allocator;
use oxc_diagnostics::Severity;
use oxc_parser::Parser;
use oxc_span::SourceType;
use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::format::{PoolBuilder, MARKER};
use crate::keys::FileKeys;
use crate::options::Options;

pub use compiler::Unsupported;

/// The VM runtime execution core, vendored from abulxt's shield runtime.
const RUNTIME_JS: &str = include_str!("runtime.js");

/// The internal name the emitted chunk binds the runtime to. The chunk is a full
/// replacement, so only estrogen's own code is present and the name cannot clash.
const VM_BINDING: &str = "__estrogen_vm";

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
    let keys = FileKeys::new(options, source.as_bytes())?;
    let mut pool = PoolBuilder::new(keys)?;
    pool.push(program_json)?;
    let pool_name = pool.name().to_string();
    let decoder = pool.into_pool().emit_decoder(&pool_name);
    Ok(format!(
        "/* {MARKER} */\nconst {VM_BINDING}={runtime};\n{decoder}\n{VM_BINDING}.createRuntime(JSON.parse({pool_name}(0))).run();\n",
        runtime = runtime_iife(),
    ))
}

/// The runtime execution core as an inlinable expression. The vendored file is
/// an ES module; this strips its `export` and wraps the body in an IIFE that
/// returns the one entry point the chunk calls.
pub fn runtime_iife() -> String {
    let body = RUNTIME_JS.replace("export { createRuntime, ARITY };", "");
    format!("(function(){{\n{body}\nreturn {{ createRuntime }};\n}})()")
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
