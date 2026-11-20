//! End-to-end check that estrogen's VM bytecode runs correctly.
//!
//! This is the "verify by running, not by shape" proof for the VM pass. For a
//! corpus of supported sources, estrogen compiles each to a VM program in Rust,
//! then `node` runs the program through the vendored runtime and compares the
//! result to native `eval` of the same source. A mismatch fails the test. If
//! `node` is not on PATH the test prints a clear SKIP rather than passing as if
//! it had verified anything.

use std::io::Write;
use std::process::Command;

use estrogen_core::options::Options;
use estrogen_core::vm::{compile_module_json, runtime_iife, try_compile_chunk};

/// Whether `node` can be run. Prints a SKIP line and returns false when not.
fn node_available(test: &str) -> bool {
    match Command::new("node").arg("--version").output() {
        Ok(out) if out.status.success() => true,
        _ => {
            eprintln!("SKIP: node not found on PATH, cannot verify {test} by running");
            false
        }
    }
}

/// Run `code` as an ES module with node and return (success, stdout, stderr).
fn run_node(code: &str, tag: &str) -> (bool, String, String) {
    let mut path = std::env::temp_dir();
    path.push(format!("estrogen-vm-node-{}-{tag}.mjs", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).expect("create temp");
        f.write_all(code.as_bytes()).expect("write temp");
    }
    let out = Command::new("node").arg(&path).output().expect("run node");
    std::fs::remove_file(&path).ok();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The supported corpus: each source is a module whose last top-level expression
/// is the value to compare. These exercise literals, locals (slot load/init),
/// globals, member access, calls and method calls, `new`, unary and binary
/// operators, logical and conditional operators, arrays, objects, sequences, and
/// no-substitution templates.
const EXPRESSIONS: &[&str] = &[
    "1 + 2 * 3",
    "(10 - 4) / 2",
    "2 ** 10",
    "'a' + 'b' + 'c'",
    "true && 'yes'",
    "false || 42",
    "null ?? 'fallback'",
    "1 < 2 ? 'lt' : 'ge'",
    "5 >= 6 ? 'a' : 'b'",
    "typeof someUndefinedGlobalXyz",
    "const x = 5; const y = x * x; y + 1",
    "let a = 3; a + 4",
    "Math.max(3, 7, 2)",
    "Math.max(1, 2) + Math.min(5, 1)",
    "[1, 2, 3].length",
    "({ a: 1, b: 2 }).b",
    "'abcdef'.slice(1, 3)",
    "(1, 2, 3)",
    "`hello world`",
    "-5 + +'3'",
    "~0",
    "!0",
    "5 % 3",
    "'x' in { x: 1 }",
    "[] instanceof Array",
    "new Array(1, 2, 3).length",
    "String(42)",
    "const n = 2; [n, n * n, n ** n]",
    "Number('0x10') | 1",
    "'ABC'.toLowerCase() + '-' + 'z'.toUpperCase()",
];

/// Functions, closures, assignment, and update. Each module ends in the value to
/// compare.
const FUNCTIONS: &[&str] = &[
    // recursion
    "function fact(n) { if (n <= 1) return 1; return n * fact(n - 1); } fact(5)",
    // mutual-ish nesting and lexical capture of an outer local
    "function outer() { const a = 2; function inner() { return a * 3; } return inner(); } outer()",
    // closure counter via assignment
    "function make() { let c = 0; return function () { c = c + 1; return c; }; } const f = make(); f(); f(); f()",
    // closure counter via prefix update, two independent closures stay separate
    "function make() { let c = 0; return function () { return ++c; }; } const a = make(); const b = make(); a(); a(); b()",
    // arrow expression body capturing an outer const
    "const base = 10; const add = (x) => x + base; add(5)",
    // arrow block body with a local
    "const f = (a, b) => { const s = a + b; return s * 2; }; f(3, 4)",
    // higher-order: map with an arrow
    "[1, 2, 3].map((x) => x * 2)",
    // higher-order: reduce with an arrow capturing nothing
    "[1, 2, 3, 4].reduce((acc, x) => acc + x, 0)",
    // method call: `this` is the receiver
    "const obj = { v: 7, get: function () { return this.v; } }; obj.get()",
    // default parameters are not emulated, so a plain-param function is used
    "function pow(b, e) { let r = 1; for (let i = 0; i < e; i++) { r = r * b; } return r; } pow(2, 8)",
    // postfix update returns the old value
    "let i = 5; const old = i++; [old, i]",
];

/// Control flow: if/else, while, do/while, C-style for, break/continue, switch,
/// and nested loops (jump backpatching). Each module ends in the value.
const CONTROL: &[&str] = &[
    // if / else
    "let r; if (3 > 2) { r = 'a'; } else { r = 'b'; } r",
    "let r; if (3 < 2) { r = 'a'; } else { r = 'b'; } r",
    // while
    "let i = 0; let s = 0; while (i < 5) { s = s + i; i = i + 1; } s",
    // do / while runs the body once even when the test is false
    "let i = 10; let s = 0; do { s = s + 1; i++; } while (i < 3); s",
    // C-style for summing an array
    "const arr = [1, 2, 3, 4]; let sum = 0; for (let i = 0; i < arr.length; i++) { sum += arr[i]; } sum",
    // break
    "let s = 0; for (let i = 0; i < 10; i++) { if (i === 5) break; s += i; } s",
    // continue
    "let s = 0; for (let i = 0; i < 6; i++) { if (i % 2 === 0) continue; s += i; } s",
    // switch with break
    "let x = 2; let r; switch (x) { case 1: r = 'a'; break; case 2: r = 'b'; break; default: r = 'c'; } r",
    // switch default
    "let x = 9; let r; switch (x) { case 1: r = 'a'; break; default: r = 'd'; } r",
    // switch fall-through
    "let x = 1; let r = ''; switch (x) { case 1: r += '1'; case 2: r += '2'; break; default: r += 'd'; } r",
    // nested loops (backpatching): sum of i*j
    "let t = 0; for (let i = 0; i < 3; i++) { for (let j = 0; j < 3; j++) { t += i * j; } } t",
    // nested loops with inner break and continue
    "let t = 0; for (let i = 0; i < 3; i++) { for (let j = 0; j < 4; j++) { if (j === i) continue; if (j > 2) break; t += 1; } } t",
    // nested loops with a block-scoped local in the outer body and a continue
    "let out = 0; for (let i = 0; i < 3; i++) { let b = i * 10; for (let j = 0; j < 2; j++) { if (j === 1) continue; out += b + j; } } out",
    // while with break and continue
    "let i = 0; let s = 0; while (true) { i++; if (i > 10) break; if (i % 2 === 0) continue; s += i; } s",
];

#[test]
fn vm_bytecode_matches_native_eval() {
    if !node_available("vm_bytecode_matches_native_eval") {
        return;
    }
    // Build one node script that runs every case through the vendored runtime and
    // compares to native eval, so the whole corpus is one node invocation.
    let mut cases = String::from("const CASES = [\n");
    for src in EXPRESSIONS.iter().chain(FUNCTIONS).chain(CONTROL) {
        let json = compile_module_json(src, "vm-node-seed")
            .unwrap_or_else(|| panic!("supported source did not compile: {src}"));
        let src_lit = serde_json::to_string(src).unwrap();
        // The program JSON is itself a valid JS object literal.
        cases.push_str(&format!("  {{ src: {src_lit}, program: {json} }},\n"));
    }
    cases.push_str("];\n");

    let script = format!(
        "const __mk = {runtime};\n\
         {cases}\n\
         const fails = [];\n\
         for (const c of CASES) {{\n\
         \tlet vm, nat;\n\
         \ttry {{ vm = __mk(c.program)(); }} catch (e) {{ vm = {{ __err: e && e.constructor && e.constructor.name }}; }}\n\
         \ttry {{ nat = (0, eval)(c.src); }} catch (e) {{ nat = {{ __err: e && e.constructor && e.constructor.name }}; }}\n\
         \tconst a = JSON.stringify(vm), b = JSON.stringify(nat);\n\
         \tif (!(Object.is(vm, nat) || a === b)) fails.push(c.src + '  vm=' + a + '  native=' + b);\n\
         }}\n\
         if (fails.length) {{ console.error(fails.join('\\n')); process.exit(1); }}\n\
         console.log('OK ' + CASES.length);\n",
        runtime = runtime_iife(),
    );

    let (ok, stdout, stderr) = run_node(&script, "corpus");
    assert!(ok, "vm corpus mismatch or node error:\n{stderr}\n{stdout}");
    assert!(
        stdout.starts_with("OK "),
        "unexpected node output: {stdout}"
    );
}

/// Constructs outside the subset must decline (return `None`) so the obfuscator
/// falls back to the literal-pool and name passes instead of miscompiling.
#[test]
fn unsupported_constructs_decline() {
    let unsupported = [
        "var z = 1; z",
        "import x from './y.js'; x",
        "export const q = 1;",
        "class C {}",
        "const [a, b] = [1, 2]; a",
        "function* gen() { yield 1; }",
        "async function af() { return 1; }",
        "const g = (...rest) => rest.length; g(1, 2)",
        "function withArgs() { return arguments.length; } withArgs(1, 2)",
        "const named = function self() { return self; };",
        "`a${1}b`",
        "const o = { x: 1 }; o?.x",
        "await Promise.resolve(1)",
        "for (const k of [1]) {}",
        "for (const k in { a: 1 }) {}",
        "try { 1; } catch (e) {}",
        "outer: for (;;) { break outer; }",
        // a for-let binding captured by a closure must decline, because estrogen
        // uses one binding where native makes one per iteration.
        "const fns = []; for (let i = 0; i < 3; i++) { fns.push(() => i); } fns[0]()",
    ];
    for src in unsupported {
        assert!(
            compile_module_json(src, "seed").is_none(),
            "expected the VM to decline (fall back) for: {src}"
        );
    }
}

/// The runtime embedded in a VM chunk must ship compact and comment-free, so the
/// output does not read like plain annotated source.
#[test]
fn embedded_runtime_has_no_comments() {
    let runtime = runtime_iife();
    assert!(
        !runtime.contains("//"),
        "embedded runtime has a line comment"
    );
    assert!(
        !runtime.contains("/*"),
        "embedded runtime has a block comment"
    );
    assert!(
        !runtime.contains("*/"),
        "embedded runtime has a block comment"
    );
    assert!(
        !runtime.contains('\n'),
        "embedded runtime still carries source newlines"
    );
    // A string literal in the runtime survives minification intact (quote style
    // is codegen's choice, so match the inner text only).
    assert!(
        runtime.contains("Cannot access a lexical binding before initialization"),
        "a runtime string literal was lost in minification"
    );
    // The mangled, minified runtime is one compact line: no run of two or more
    // spaces and no newline survive.
    assert!(
        !runtime.contains("  "),
        "the minified runtime still has runs of whitespace"
    );
    // It is a valid expression that node can evaluate: it is the factory, and
    // calling it with a program returns the entry function the chunk runs.
    if node_available("embedded_runtime_has_no_comments") {
        let script = format!(
            "const mk = {runtime};\n\
             if (typeof mk !== 'function') {{ console.error('no factory'); process.exit(1); }}\n\
             console.log('OK');\n",
            runtime = runtime,
        );
        let (ok, stdout, stderr) = run_node(&script, "minify");
        assert!(ok, "minified runtime failed to load:\n{stderr}");
        assert!(stdout.starts_with("OK"), "unexpected output: {stdout}");
    }
}

/// A VM chunk whose compiled function carries a canary string and a
/// canary-named local must leak neither: the string lives in the encrypted pool
/// and the name compiles to a slot index.
#[test]
fn vm_function_chunk_leaks_no_plaintext() {
    let src = "function reveal() { const secretCanaryLocal = 'topSecretCanaryValue'; \
               return secretCanaryLocal; } reveal();\n";
    let opts = Options {
        seed: Some(b"leak-seed".to_vec()),
        ..Options::default()
    };
    let out = try_compile_chunk(src, &opts)
        .expect("compile should not error")
        .expect("a function chunk should VM-compile");
    assert!(out.contains("JSON.parse("), "expected a VM chunk:\n{out}");
    assert!(
        !out.contains("topSecretCanaryValue"),
        "the string literal leaked in the clear"
    );
    assert!(
        !out.contains("secretCanaryLocal"),
        "the local binding name leaked in the clear"
    );
    assert!(
        !out.contains("reveal"),
        "the function name leaked in the clear"
    );
}

/// The shipped VM runtime leaks no opcode name and no readable runtime
/// identifier, and its body is one minified line. This is the obfuscation
/// guarantee for the runtime itself, on top of the per-chunk canary scan.
#[test]
fn vm_chunk_runtime_is_mangled_and_names_free() {
    use estrogen_core::vm::opcodes::OP_NAMES;

    let opts = Options {
        seed: Some(b"leak-scan-seed".to_vec()),
        ..Options::default()
    };
    // A chunk that exercises many opcode groups (arithmetic, calls, control flow,
    // slots, arrays, closures), so its runtime carries their handlers.
    let src =
        "function pow(b, e) { let r = 1; for (let i = 0; i < e; i++) { r = r * b; } return r; } \
               const xs = [1, 2, 3].map((n) => n + 1); pow(2, xs.length);\n";
    let out = try_compile_chunk(src, &opts)
        .expect("compile should not error")
        .expect("a function chunk should VM-compile");
    assert!(out.contains("JSON.parse("), "expected a VM chunk:\n{out}");

    // The chunk is one line per structural piece:
    //   /* MARKER */\nconst NAME=<runtime>;\n<decoder>\nNAME(JSON.parse(..))();\n
    // The runtime is the second line. The opcode-name scan runs against that
    // line, not the whole chunk, because the encrypted program ships as base64
    // and a random base64 blob can hold a short opcode substring by chance; the
    // runtime line carries no base64, so the check there is exact.
    let runtime_line = out.lines().nth(1).expect("the chunk has a runtime line");
    assert!(
        runtime_line.starts_with("const "),
        "unexpected runtime line: {runtime_line}"
    );
    for name in OP_NAMES {
        assert!(
            !runtime_line.contains(name),
            "opcode name {name} leaked into the shipped runtime"
        );
    }

    // No readable runtime identifier or brand word survives. These are long or
    // carry punctuation, so scanning the whole chunk is safe from base64 chance.
    for ident in [
        "estrogen",
        "__estrogen_vm",
        "ARITY",
        "createRuntime",
        "createClosure",
        "decodeConst",
        "HANDLERS",
        "scopeAt",
        "runtime",
        ".run(",
    ] {
        assert!(
            !out.contains(ident),
            "runtime identifier {ident} leaked into the VM chunk"
        );
    }

    // The runtime body is one minified line: no newline (it is a single line by
    // construction) and no run of two or more spaces.
    assert!(
        !runtime_line.contains("  "),
        "the shipped runtime still has runs of whitespace"
    );
    assert!(
        out.lines().count() <= 5,
        "the VM chunk is not compact: {} lines",
        out.lines().count()
    );
}

/// A chunk with an out-of-subset construct still produces a valid fallback chunk
/// through the obfuscator, and the VM declines it.
#[test]
fn unsupported_chunk_still_declines_at_the_api() {
    let opts = Options {
        seed: Some(b"fallback-seed".to_vec()),
        ..Options::default()
    };
    // A class is outside the subset; the VM must decline so the caller falls back.
    let declined = try_compile_chunk("class C { m() { return 1; } }\n", &opts)
        .expect("compile should not error");
    assert!(
        declined.is_none(),
        "a class chunk must decline to the fallback path"
    );
}
