//! Compare the public in-memory transform with its input in Node. The probes run
//! outside the transform, so they keep using the original public names.

use std::io::ErrorKind;
use std::process::Command;

use estrogen_core::{obfuscate_js, Options};
use regex::Regex;

fn options(seed: &[u8]) -> Options {
    Options {
        seed: Some(seed.to_vec()),
        ..Options::default()
    }
}

fn transform(source: &str) -> String {
    obfuscate_js(source, &options(b"mangle-node")).unwrap().code
}

fn compare_in_node(source: &str, output: &str, probe: &str) {
    let driver = format!(
        "import assert from 'node:assert/strict';\n\
         async function observe(source, tag) {{ {probe} }}\n\
         const before = await observe(process.argv[1], 'original');\n\
         assert.deepEqual(await observe(process.argv[2], 'transformed'), before);"
    );
    let result = Command::new("node")
        .args(["--input-type=module", "-e", &driver, "--", source, output])
        .output();
    let result = match result {
        Err(err) if err.kind() == ErrorKind::NotFound => {
            eprintln!("SKIP: node not found on PATH, cannot compare runtime values");
            return;
        }
        result => result.expect("start Node"),
    };
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn compare_module(source: &str, output: &str, expression: &str) {
    let probe = format!(
        "const url = 'data:text/javascript;base64,' + Buffer.from(source).toString('base64');\n\
         const api = await import(url + '#' + tag);\n\
         return {expression};"
    );
    compare_in_node(source, output, &probe);
}

#[test]
fn shadowed_locals_shorten_without_a_pool_and_keep_their_closures() {
    let source = r#"export function collectScores(startingScoreForTeam) {
        let runningScoreForTeam = 317;
        const readOuter = () => runningScoreForTeam + startingScoreForTeam;
        const scores = [readBonus(), readOuter()];
        {
            let runningScoreForTeam = 8;
            scores.push(() => ++runningScoreForTeam);
        }
        var hoistedBonusForTeam = 6;
        function readBonus() { return hoistedBonusForTeam; }
        runningScoreForTeam += readBonus();
        return [scores[0], scores[1], scores[2](), scores[2](), readOuter()];
    }"#;
    let output = transform(source);
    assert!(!output.contains("=(function(){"), "unexpected decoder");
    compare_module(source, &output, "api.collectScores(3)");

    // Find each binding by its initializer, not by the minifier's choice of name.
    let declarations = Regex::new(r"\b(?:let|var|const)\s+([^\s=,;]+)\s*=\s*(?:317|8)\b").unwrap();
    let names: Vec<_> = declarations
        .captures_iter(&output)
        .map(|c| c[1].len())
        .collect();
    assert_eq!(names.len(), 2, "missing one of the two score bindings");
    assert!(
        names.iter().all(|&n| n < "runningScoreForTeam".len()),
        "score bindings did not shorten: {output}"
    );
}

#[test]
fn classic_script_globals_remain_accessible_to_a_second_script() {
    let source = r#"var publishedOffset = 3;
        function readPublicScale(ordinaryInput) {
            let localAdjustment = ordinaryInput + publishedOffset;
            return localAdjustment + a;
        }"#;
    compare_in_node(
        source,
        &transform(source),
        r#"
        const { createContext, runInContext } = await import('node:vm');
        const context = createContext({ a: 40 });
        runInContext(source, context);
        return [context.publishedOffset, context.readPublicScale(2), context.a];
    "#,
    );
}

#[test]
fn named_exports_and_observable_function_and_class_names_survive() {
    let source = r#"export default function describeTools() {
        function paintBrush() {}
        class CanvasPalette {}
        const inferredBrushFactory = () => 1;
        const InferredPalette = class {};
        let assignedBrush;
        assignedBrush = function () {};
        const [fallbackBrush = () => 2] = [];
        const { fallbackPalette = class {} } = {};
        return [paintBrush, CanvasPalette, inferredBrushFactory, InferredPalette,
            assignedBrush, fallbackBrush, fallbackPalette].map(tool => tool.name);
    }
    export { describeTools as toolNames };"#;
    compare_module(
        source,
        &transform(source),
        "[Object.keys(api), api.default === api.toolNames, api.default.name, api.toolNames()]",
    );
}

#[test]
fn shorthand_keys_and_destructuring_defaults_keep_their_public_spelling() {
    let source = r#"export function readLayout({ panelWidth = 5, gutter: sideGap = panelWidth + 1 }, overrides = {}) {
        const { panelDepth = sideGap } = overrides;
        let borderSize;
        ({ borderSize = panelDepth + 1 } = overrides);
        return { panelWidth, sideGap, panelDepth, borderSize };
    }"#;
    compare_module(source, &transform(source), "[api.readLayout({}), api.readLayout({ panelWidth: 9, gutter: 2 }, { panelDepth: 4, borderSize: 3 })]");
}

#[test]
fn direct_eval_can_read_its_argument_and_outer_bindings() {
    let source = r#"export function measuredTotal(firstReading) {
        const secondReading = 5;
        return (function (thirdReading) {
            return eval('firstReading + secondReading + thirdReading');
        })(9);
    }"#;
    compare_module(source, &transform(source), "api.measuredTotal(4)");
}

#[test]
fn pooled_and_unpooled_outputs_have_one_visible_watermark_and_are_idempotent() {
    const HEADER: &str = "/* generated by https://estro.help */";
    let fixed = options(b"watermark-node");
    for source in [
        "export function label(headingNumber) { return headingNumber + 1; }",
        "export function label(headingNumber) { return ['@sysce/estrogen', '/*__FRX_OBF__*/', headingNumber]; }",
    ] {
        let first = obfuscate_js(source, &fixed).unwrap();
        assert!(first.edits > 0);
        compare_module(source, &first.code, "api.label(2)");
        assert!(first.code.starts_with(HEADER), "missing watermark");
        assert_eq!(first.code.matches(HEADER).count(), 1);
        assert!(!first.code.contains("__FRX_OBF__"));
        assert_eq!(obfuscate_js(source, &fixed).unwrap().code, first.code);

        let second = obfuscate_js(&first.code, &options(b"another-seed")).unwrap();
        assert_eq!(second.edits, 0);
        assert_eq!(second.code, first.code);
    }
}

#[test]
fn legacy_watermark_is_still_recognized() {
    let source = "/*__FRX_OBF__*/export const message = 'already processed';";
    let output = obfuscate_js(source, &options(b"watermark-node")).unwrap();
    assert_eq!(output.edits, 0);
    assert_eq!(output.code, source);
}

#[test]
fn watermark_and_decoder_keep_hashbang_and_directive_prologue() {
    let source =
        "#!/usr/bin/env node\r\n'use strict'\nexport function readLabel() { return 'apricot'; }";
    let output = transform(source);
    assert!(output.starts_with("#!/usr/bin/env node\r\n/* generated by https://estro.help */"));
    assert!(output.find("'use strict'").unwrap() < output.find("var $").unwrap());
    compare_module(source, &output, "api.readLabel()");
    assert_eq!(transform(&output), output);
}

fn check_decoder_collision(source: &str, seed: u32, expression: &str) {
    let output = obfuscate_js(source, &options(&seed.to_le_bytes()))
        .unwrap()
        .code;
    let pattern = Regex::new(r"\bvar (\$[A-Za-z0-9_$]{4})=\(function\(\)\{").unwrap();
    let decoder = pattern.captures(&output).expect("missing short decoder");
    assert_ne!(&decoder[1], "$aaaa", "decoder reused the colliding name");
    assert!(!output.contains("apricot"), "collision prevented pooling");
    compare_module(source, &output, expression);
}

#[test]
fn decoder_does_not_collide_with_an_existing_export() {
    // For these exact source bytes and seed, the first decoder candidate is $aaaa.
    let source = "export const $aaaa = 7; export const result = [$aaaa, 'apricot'];";
    check_decoder_collision(source, 5_283_038, "[api.$aaaa, api.result]");
}

#[test]
fn escaped_local_binding_does_not_shadow_the_decoder() {
    // This seed also draws $aaaa first. The escape spells the same identifier,
    // but the raw source does not contain the decoder name.
    let source = r"export function label() { const $aaaa = () => 7; return [$aaaa(), 'apricot']; }";
    check_decoder_collision(source, 14_994_058, "api.label()");
}
