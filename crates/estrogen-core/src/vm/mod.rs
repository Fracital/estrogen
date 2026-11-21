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
//! anything else (classes, async, locally declared exports, exception handling,
//! and the rest). [`try_compile_chunk`] turns that into `Ok(None)`, and the
//! obfuscator falls back to the literal-pool and name passes for that chunk,
//! the same way abulxt's shield degrades a chunk to `@no-vm`. Nothing breaks; a
//! chunk the VM cannot compile is still protected by the other passes.

mod compiler;
mod layout;
pub mod opcodes;
mod program;

use std::collections::HashSet;
use std::sync::OnceLock;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ImportDeclarationSpecifier, ImportOrExportKind, Program as AstProgram, Statement,
};
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_diagnostics::Severity;
use oxc_mangler::{MangleOptions, Mangler, ManglerReturn};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
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

/// Every free identifier that the embedded runtime can read. A native import
/// is lexical across the whole module, including the VM helper emitted after
/// it. Importing a name the helper uses would silently capture that binding.
fn runtime_free_names() -> &'static HashSet<String> {
    static NAMES: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, RUNTIME_JS, SourceType::mjs()).parse();
        let built = SemanticBuilder::new().build(&parsed.program);
        built
            .semantic
            .scoping()
            .root_unresolved_references()
            .keys()
            .map(|name| name.as_str().to_string())
            .collect()
    })
}

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
    // Standalone JSON has no native import declarations or live binding
    // getters. Import-bearing modules use try_compile_chunk instead.
    if ret.program.body.iter().any(|statement| {
        matches!(
            statement,
            Statement::ImportDeclaration(_)
                | Statement::ExportAllDeclaration(_)
                | Statement::ExportFromDeclaration(_)
                | Statement::ExportNamedDeclaration(_)
        )
    }) {
        return None;
    }
    match compiler::compile_program(&ret.program, seed, true) {
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
    let Some(imports) = NativeImports::collect(&ret.program, source) else {
        #[cfg(not(target_arch = "wasm32"))]
        if std::env::var_os("ESTROGEN_VM_TRACE").is_some() {
            eprintln!(
                "estrogen VM fallback ({} bytes): native import/export wrapper declined",
                source.len()
            );
        }
        return Ok(None);
    };
    let seed = layout_seed(options, source);
    let program = match compiler::compile_program(&ret.program, &seed, false) {
        Ok(p) => p,
        Err(reason) => {
            #[cfg(not(target_arch = "wasm32"))]
            if std::env::var_os("ESTROGEN_VM_TRACE").is_some() {
                eprintln!(
                    "estrogen VM fallback ({} bytes): {}",
                    source.len(),
                    reason.0
                );
            }
            return Ok(None);
        }
    };
    let module = ModuleContext {
        needs_meta: program.uses_import_meta,
        needs_import: program.uses_dynamic_import,
        exports: program.exports.clone(),
        // A top-level await makes the main async, so the module evaluates to a
        // promise the wrapper must await before it is considered evaluated.
        main_is_async: program.functions[program.main_index].is_async,
    };
    let json = program.into_json_string();
    Ok(Some(emit_chunk(&json, source, options, &imports, &module)?))
}

/// What the emitted wrapper must pass to the runtime beyond imported bindings:
/// `import.meta`, native dynamic import, and the names to re-export.
struct ModuleContext {
    needs_meta: bool,
    needs_import: bool,
    exports: Vec<String>,
    main_is_async: bool,
}

#[derive(Default)]
struct NativeImports {
    statements: String,
    bindings: Vec<String>,
}

impl NativeImports {
    fn collect(program: &AstProgram<'_>, source: &str) -> Option<Self> {
        let mut out = Self::default();
        for statement in &program.body {
            let span = match statement {
                Statement::ImportDeclaration(decl) => {
                    if decl.phase.is_some() || decl.import_kind != ImportOrExportKind::Value {
                        return None;
                    }
                    for specifier in decl.specifiers.iter().flatten() {
                        let name = match specifier {
                            ImportDeclarationSpecifier::ImportSpecifier(spec) => {
                                if spec.import_kind != ImportOrExportKind::Value {
                                    return None;
                                }
                                spec.local.name.as_str()
                            }
                            ImportDeclarationSpecifier::ImportDefaultSpecifier(spec) => {
                                spec.local.name.as_str()
                            }
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(spec) => {
                                spec.local.name.as_str()
                            }
                        };
                        // The wrapper and the runtime use these builtins, so
                        // imported bindings must not shadow their names.
                        if runtime_free_names().contains(name)
                            || matches!(
                                name,
                                "Object"
                                    | "Proxy"
                                    | "globalThis"
                                    | "JSON"
                                    | "RegExp"
                                    | "Reflect"
                                    | "TypeError"
                                    | "Uint8Array"
                                    | "Uint32Array"
                                    | "TextDecoder"
                                    | "atob"
                                    | "Math"
                            )
                        {
                            return None;
                        }
                        out.bindings.push(name.to_string());
                    }
                    decl.span
                }
                Statement::ExportAllDeclaration(decl)
                    if decl.export_kind == ImportOrExportKind::Value =>
                {
                    decl.span
                }
                Statement::ExportFromDeclaration(decl)
                    if decl.export_kind == ImportOrExportKind::Value
                        && decl
                            .specifiers
                            .iter()
                            .all(|spec| spec.export_kind == ImportOrExportKind::Value) =>
                {
                    decl.span
                }
                _ => continue,
            };
            let text = source.get(span.start as usize..span.end as usize)?;
            out.statements.push_str(text);
            out.statements.push('\n');
        }
        Some(out)
    }
}

/// Assemble the replacement chunk: the marker, the runtime, the encrypted
/// program as an estrogen ChaCha20 pool of one entry, and the call that decodes
/// and runs it.
fn emit_chunk(
    program_json: &str,
    source: &str,
    options: &Options,
    imports: &NativeImports,
    module: &ModuleContext,
) -> Result<String> {
    let mut keys = FileKeys::new(options, source.as_bytes())?;
    // The short generated name the chunk binds the runtime to, drawn from the
    // same key material and in the same `$XXXX` shape as the pool decoder name
    // (mirroring [`FileKeys::decoder_name`]). The chunk is a full replacement, so
    // the only other top-level name is the pool decoder; draw the helper name
    // first, then the pool, and guard against the astronomically unlikely clash.
    let mut vm_name = keys.decoder_name();
    let mut pool = PoolBuilder::new(keys)?;
    pool.push(program_json)?;
    let mut pool_name = pool.name().to_string();
    while source.contains(&vm_name) || vm_name == pool_name {
        vm_name.push('_');
    }
    while source.contains(&pool_name) || vm_name == pool_name {
        pool_name.push('_');
    }
    let decoder = pool.into_pool().emit_decoder(&pool_name);
    let mut setup = String::new();
    // The fields of the runtime options object the chunk passes after the
    // program. Each is added only when the chunk uses that part of the module
    // context, so a plain chunk keeps the bare `vm(program)()` call.
    let mut opts: Vec<String> = Vec::new();
    let fresh = |base: &str, source: &str, pool_name: &str| {
        let mut name = base.to_string();
        while source.contains(&name) || name == pool_name {
            name.push('_');
        }
        name
    };
    if !imports.bindings.is_empty() {
        let globals_name = fresh(&format!("{vm_name}_g"), source, &pool_name);
        let bindings_name = fresh(&format!("{globals_name}_i"), source, &pool_name);
        setup.push_str(&format!("const {bindings_name}=Object.create(null);"));
        for local in &imports.bindings {
            let literal = serde_json::to_string(local).expect("identifier JSON");
            setup.push_str(&format!(
                "Object.defineProperty({bindings_name},{literal},{{get:()=>{local},set:()=>{{throw new TypeError('Assignment to import binding')}}}});"
            ));
        }
        // An ordinary prototype chain would copy a free global assignment
        // onto the wrapper rather than write back to globalThis. Proxy the real
        // global object and overlay only the read-only imported bindings.
        setup.push_str(&format!(
            "const {globals_name}=new Proxy(globalThis,{{get:(target,key)=>Object.hasOwn({bindings_name},key)?{bindings_name}[key]:Reflect.get(target,key,target),set:(target,key,value)=>Object.hasOwn({bindings_name},key)?({bindings_name}[key]=value,true):Reflect.set(target,key,value,target),has:(target,key)=>Object.hasOwn({bindings_name},key)||Reflect.has(target,key)}});"
        ));
        opts.push(format!("globals:{globals_name}"));
    }
    if module.needs_meta {
        opts.push("meta:import.meta".to_string());
    }
    if module.needs_import {
        opts.push("import:$=>import($)".to_string());
    }
    let mut export_tail = String::new();
    if !module.exports.is_empty() {
        let exports_name = fresh(&format!("{vm_name}_e"), source, &pool_name);
        setup.push_str(&format!("const {exports_name}=Object.create(null);"));
        opts.push(format!("exports:{exports_name}"));
        // The VM wrote each export's final value into the exports object during
        // the run above; re-export them from native const bindings so a consumer
        // links to them. Exports are const, so the value is settled.
        let bindings: Vec<String> = module
            .exports
            .iter()
            .map(|name| {
                let literal = serde_json::to_string(name).expect("identifier JSON");
                format!("{name}={exports_name}[{literal}]")
            })
            .collect();
        export_tail = format!("export const {};", bindings.join(","));
    }
    let argument = if opts.is_empty() {
        String::new()
    } else {
        format!(",{{{}}}", opts.join(","))
    };
    // The runtime IIFE evaluates to the factory; calling it with the decoded
    // program returns the entry function, which the trailing `()` runs. No part
    // of this spells the runtime's purpose: the names are generated and the
    // runtime body is mangled.
    // A top-level await in the chunk makes the main async; await the run so the
    // module is not considered evaluated until it settles. The wrapper is an ES
    // module whenever this is reached, so a top-level await is valid.
    let await_kw = if module.main_is_async { "await " } else { "" };
    Ok(format!(
        "/* {MARKER} */{imports}const {vm_name}={runtime};{decoder}{setup}{await_kw}{vm_name}(JSON.parse({pool_name}(0)){argument})();{export_tail}",
        imports = imports.statements,
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
