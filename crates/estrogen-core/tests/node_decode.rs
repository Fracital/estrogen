//! End-to-end check that the emitted JS decoder decodes estrogen's own pool.
//!
//! This is the "verify by decoding, not by shape" requirement from the handoff
//! and the symmetry contract in `obfuscate-build.test.mjs`. It obfuscates a
//! script through the public API, runs the result in `node`, and asserts the
//! runtime values equal the originals. If `node` is not on PATH the test prints
//! a clear SKIP and returns rather than passing as if it had verified anything.

use std::io::Write;
use std::process::Command;

use estrogen_core::options::Options;
use estrogen_core::pass::literals::LiteralsPass;
use estrogen_core::pass::{FileContext, Pass};
use estrogen_core::walk::FileKind;

/// Whether `node` can be run. Prints a SKIP line and returns false when not.
fn node_available(test: &str) -> bool {
    match Command::new("node").arg("--version").output() {
        Ok(out) if out.status.success() => true,
        _ => {
            eprintln!("SKIP: node not found on PATH, cannot verify {test} by decoding");
            false
        }
    }
}

/// Obfuscate `src` as a JS module through the literals pass and apply the edits.
fn obfuscate(src: &str) -> String {
    let opts = Options {
        seed: Some(b"node-decode-seed".to_vec()),
        ..Options::default()
    };
    let ctx = FileContext::new("app.js", FileKind::Js, src, &opts);
    let edits = LiteralsPass
        .collect_edits(&ctx)
        .expect("obfuscation failed");
    edits.apply(src).expect("apply failed")
}

/// Write `code` to a temp file, run it with node, and return trimmed stdout.
fn run_node(code: &str, tag: &str) -> String {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "estrogen-node-decode-{}-{tag}.js",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&path).expect("create temp");
        f.write_all(code.as_bytes()).expect("write temp");
    }
    let out = Command::new("node").arg(&path).output().expect("run node");
    std::fs::remove_file(&path).ok();
    assert!(
        out.status.success(),
        "node exited with error for {tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("utf8 stdout")
        .trim()
        .to_string()
}

fn compare_values_in_node(source: &str, tag: &str) -> String {
    let transformed = obfuscate(source);
    let original = run_node(
        &format!("{source}\nconsole.log(JSON.stringify(globalThis.__out));"),
        &format!("{tag}-original"),
    );
    let rewritten = run_node(
        &format!("{transformed}\nconsole.log(JSON.stringify(globalThis.__out));"),
        &format!("{tag}-transformed"),
    );
    assert_eq!(rewritten, original, "runtime values differ for {tag}");
    transformed
}

#[test]
fn property_lookups_and_keys_keep_runtime_behavior() {
    if !node_available("property_lookups_and_keys_keep_runtime_behavior") {
        return;
    }
    let source = r#"
        const base = { fromPrototype: 19 };
        const object = {
            __proto__: base,
            publicPropertyName: 7,
            method() { return this.publicPropertyName; },
            ['computedField']: 11
        };
        const missing = null;
        const { publicPropertyName: extracted } = object;
        let assigned;
        ({ publicPropertyName: assigned } = object);
        globalThis.__out = [
            object.publicPropertyName,
            object.method(),
            object?.publicPropertyName,
            missing?.publicPropertyName,
            object /* leading */ . /* trailing */ publicPropertyName,
            object.fromPrototype,
            object['computedField'],
            extracted, assigned
        ];
    "#;
    let out = compare_values_in_node(source, "public-properties");
    assert!(
        !out.contains(".publicPropertyName"),
        "dot spelling survived: {out}"
    );
    assert!(
        !out.contains("publicPropertyName:"),
        "plain key survived: {out}"
    );
    assert!(out.contains("__proto__"), "prototype setter changed: {out}");
}

#[test]
fn numeric_members_and_class_fields_preserve_syntax_and_values() {
    if !node_available("numeric_members_and_class_fields_preserve_syntax_and_values") {
        return;
    }
    let source = r#"
        class Box {
            firstField = 8
            secondField = 5
            'thirdField' = 2
            constructor() { this.offset = 4; }
            getTotal() { return this.firstField + this.secondField + this.thirdField + this.offset; }
        }
        const box = new Box();
        globalThis.__out = [box.getTotal(), 1..toString(), (2).toString(), box?.secondField];
    "#;
    let out = compare_values_in_node(source, "class-fields");
    assert!(!out.contains("firstField ="), "plain field survived: {out}");
    assert!(
        !out.contains("secondField ="),
        "plain field survived: {out}"
    );
}

#[test]
fn emitted_decoder_round_trips_values_in_node() {
    if !node_available("emitted_decoder_round_trips_values_in_node") {
        return;
    }
    // Strings and templates in value position; the obfuscated code rebuilds them
    // at runtime through the shipped decoder.
    let src = r#"globalThis.__out = ["Fracital","Privacy Policy","Go ad-free for $1","✓ ☃ é 🎮","","x", `tmpl ${1+2} end`, `plain template`];"#;
    let obfuscated = obfuscate(src);

    // The decoder must be present and the plaintext gone.
    assert!(obfuscated.contains("=(function(){"), "decoder missing");
    assert!(
        !obfuscated.contains("\"Go ad-free for $1\""),
        "plaintext survived in output"
    );

    let runner = format!("{obfuscated}\nconsole.log(JSON.stringify(globalThis.__out));\n");
    let got = run_node(&runner, "roundtrip");

    // JSON.stringify of the expected values. Template `tmpl ${1+2} end` -> "tmpl 3 end".
    let expected = "[\"Fracital\",\"Privacy Policy\",\"Go ad-free for $1\",\"\u{2713} \u{2603} \u{e9} \u{1f3ae}\",\"\",\"x\",\"tmpl 3 end\",\"plain template\"]";
    assert_eq!(got, expected, "decoded values differ from originals");
}

/// The snippet used for the stay/go parity check, and the substrings that must
/// stay literal versus must not survive verbatim.
const PARITY_SRC: &str = concat!(
    "\"use strict\";\n",
    "import x from \"./other.js\";\n",
    "const o = { \"__proto__\": base, \"k\": \"v\" };\n",
    "class C { \"constructor\"(){} \"run\"(){} }\n",
    "const s = \"plain-value\"; const t = tag`q ${x}`;"
);
const PARITY_STAY: &[&str] = &[
    "\"use strict\"",
    "\"./other.js\"",
    "\"__proto__\"",
    "\"constructor\"",
];
const PARITY_GO: &[&str] = &["\"plain-value\"", "\"run\"", "\"v\""];

#[test]
fn estrogen_skip_and_encrypt_decisions() {
    let out = obfuscate(PARITY_SRC);
    for s in PARITY_STAY {
        assert!(out.contains(s), "estrogen dropped a must-stay literal: {s}");
    }
    for s in PARITY_GO {
        assert!(!out.contains(s), "estrogen left a must-go literal: {s}");
    }
}

#[test]
fn matches_reference_skip_decisions() {
    let script = dirs_home().join("src/Fracital/scripts/obfuscate-build.mjs");
    if !script.exists() {
        eprintln!("SKIP: reference {script:?} not present, cannot cross-check decisions");
        return;
    }
    if !node_available("matches_reference_skip_decisions") {
        return;
    }
    // Run the reference obfuscateJs on the same snippet and record which
    // must-stay / must-go substrings it keeps.
    let probe = format!(
        "import({script:?}).then(m => {{ const {{ code }} = m.obfuscateJs({}, \"module\"); \
         const stay = {}; const go = {}; \
         console.log(JSON.stringify({{ stay: stay.map(s=>code.includes(s)), go: go.map(s=>code.includes(s)) }})); }});",
        js_string(PARITY_SRC),
        js_array(PARITY_STAY),
        js_array(PARITY_GO),
    );
    let out = Command::new("node")
        .arg("--input-type=module")
        .arg("-e")
        .arg(&probe)
        .output()
        .expect("run node");
    assert!(
        out.status.success(),
        "reference probe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let reference = String::from_utf8(out.stdout).unwrap();
    let reference = reference.trim();
    // The reference keeps all stay and drops all go; estrogen must do the same.
    let expected_stay = js_array(&PARITY_STAY.iter().map(|_| "true").collect::<Vec<_>>());
    let _ = expected_stay;
    assert!(
        reference.contains("\"stay\":[true,true,true,true]"),
        "reference stay decisions changed: {reference}"
    );
    assert!(
        reference.contains("\"go\":[false,false,false]"),
        "reference go decisions changed: {reference}"
    );
    // And estrogen agrees with that same contract.
    let out = obfuscate(PARITY_SRC);
    for s in PARITY_STAY {
        assert!(out.contains(s), "estrogen disagrees on must-stay: {s}");
    }
    for s in PARITY_GO {
        assert!(!out.contains(s), "estrogen disagrees on must-go: {s}");
    }
}

/// The user home directory, for locating the optional reference script.
fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

/// A JSON/JS string literal for a Rust string (ASCII snippet, so escaping quotes
/// and backslashes is enough).
fn js_string(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JS array literal of the given raw items (each already a JS expression or a
/// string to be quoted when it starts with a quote char).
fn js_array(items: &[&str]) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|it| {
            if *it == "true" || *it == "false" {
                (*it).to_string()
            } else {
                js_string(it)
            }
        })
        .collect();
    format!("[{}]", parts.join(","))
}

#[test]
fn obfuscated_code_reparses_in_node() {
    if !node_available("obfuscated_code_reparses_in_node") {
        return;
    }
    // A spread of positions: object with string key and value, computed key,
    // directive, a function with a minified return string, nested template.
    let src = r#""use strict";
function pick(n){ return n > 0 ? "positive" : "non-positive"; }
const obj = { "label": "ready", ["dyn" + "amic"]: 1 };
const t = `count=${obj.label}; pick=${pick(2)}`;
globalThis.__out = [pick(5), obj["label"], t];
"#;
    let obfuscated = obfuscate(src);
    // Node parses and runs it; a syntax slip (missing keyword space, bad splice)
    // would throw here.
    let runner = format!("{obfuscated}\nconsole.log(JSON.stringify(globalThis.__out));\n");
    let got = run_node(&runner, "reparse");
    let expected = "[\"positive\",\"ready\",\"count=ready; pick=positive\"]";
    assert_eq!(got, expected);
}
