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

use estrogen_core::vm::{compile_module_json, runtime_iife};

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
const SUPPORTED: &[&str] = &[
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

#[test]
fn vm_bytecode_matches_native_eval() {
    if !node_available("vm_bytecode_matches_native_eval") {
        return;
    }
    // Build one node script that runs every case through the vendored runtime and
    // compares to native eval, so the whole corpus is one node invocation.
    let mut cases = String::from("const CASES = [\n");
    for src in SUPPORTED {
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
         \ttry {{ vm = __mk.createRuntime(c.program).run(); }} catch (e) {{ vm = {{ __err: e && e.constructor && e.constructor.name }}; }}\n\
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
        "function f() { return 1; }",
        "const g = (x) => x + 1; g(2)",
        "var z = 1; z",
        "import x from './y.js'; x",
        "export const q = 1;",
        "class C {}",
        "const [a, b] = [1, 2]; a",
        "let obj = {}; obj.x = 1;",
        "let i = 0; i++",
        "`a${1}b`",
        "const o = { x: 1 }; o?.x",
        "await Promise.resolve(1)",
        "for (const k of [1]) {}",
        "if (true) { 1 } else { 2 }",
    ];
    for src in unsupported {
        assert!(
            compile_module_json(src, "seed").is_none(),
            "expected the VM to decline (fall back) for: {src}"
        );
    }
}
