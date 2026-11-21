//! Compare the public in-memory transform with its input in Node. The probes run
//! outside the transform, so they keep using the original public names.

use std::io::ErrorKind;
use std::process::Command;

use estrogen_core::{obfuscate_js, Options};
use regex::Regex;

fn options(seed: &[u8]) -> Options {
    Options {
        seed: Some(seed.to_vec()),
        // Test the edit-and-splice shape, not the single-line minified reprint.
        minify: false,
        ..Options::default()
    }
}

fn transform(source: &str) -> String {
    obfuscate_js(source, &options(b"mangle-node")).unwrap().code
}

/// The edit-and-splice path with the VM off, so the top-level rename and the
/// export rewrite run (the VM path is a separate whole-file replacement).
fn splice(source: &str) -> String {
    let opts = Options {
        seed: Some(b"toplevel-node".to_vec()),
        vm: false,
        minify: false,
        ..Options::default()
    };
    obfuscate_js(source, &opts).unwrap().code
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
    assert!(
        output.contains("=(function(){"),
        "property access should use the decoder"
    );
    assert!(!output.contains(".push("), "method name survived: {output}");
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
fn private_function_and_class_declarations_shorten_when_called_directly() {
    let source = r#"export function calculate() {
        function calculateInternalPrivateValue(amount) { return amount + 4; }
        class InternalPrivateBox {
            constructor(amount) { this.amount = amount; }
            read() { return this.amount; }
        }
        return calculateInternalPrivateValue(new InternalPrivateBox(9).read());
    }"#;
    let output = splice(source);
    assert!(
        !output.contains("function calculateInternalPrivateValue"),
        "private function binding was left visible: {output}"
    );
    assert!(
        !output.contains("class InternalPrivateBox"),
        "private class binding was left visible: {output}"
    );
    compare_module(source, &output, "api.calculate()");
}

#[test]
fn public_and_observable_callable_names_are_preserved() {
    let source = r#"export function PublishedPublicFunction() {
        function PrivateNamedFunction() {}
        class PrivateNamedClass {}
        return [PrivateNamedFunction.name, new PrivateNamedClass().constructor.name];
    }
    export class PublishedPublicClass {}"#;
    let output = splice(source);
    assert!(
        output.contains("function PublishedPublicFunction"),
        "{output}"
    );
    assert!(output.contains("class PublishedPublicClass"), "{output}");
    assert!(output.contains("PrivateNamedFunction"), "{output}");
    assert!(output.contains("PrivateNamedClass"), "{output}");
    compare_module(
        source,
        &output,
        "[api.PublishedPublicFunction.name, api.PublishedPublicClass.name, api.PublishedPublicFunction()]",
    );
}

#[test]
fn object_name_keys_do_not_block_private_name_renaming() {
    // `{ name: value }` sets a property, it does not read a callable's `.name`,
    // so a private, directly-called function is still renamed around it.
    let source = r#"function baseComponent(tag) { return { name: tag, kind: 'widget' }; }
    function describe(id) { return baseComponent(id).kind; }
    export function build(id) { return describe(id) + ':' + describe(id); }"#;
    let output = splice(source);
    assert!(
        !output.contains("baseComponent"),
        "an object `name:` key kept a private function readable: {output}"
    );
    assert!(
        !output.contains("describe"),
        "a private function was kept: {output}"
    );
    assert!(
        output.contains("build"),
        "the exported name must stay public"
    );
    compare_module(source, &output, "api.build('x')");
}

#[test]
fn a_destructured_name_read_still_keeps_private_names() {
    // `const { name } = fn` reads the function's `.name`, so the scan still
    // treats that as an observation and keeps every private name.
    let source = r#"function widgetMaker() { return 1; }
    export function nameOf() { const { name } = widgetMaker; return name; }"#;
    let output = splice(source);
    assert!(
        output.contains("widgetMaker"),
        "a destructured name read must keep the private name: {output}"
    );
    compare_module(source, &output, "api.nameOf()");
}

#[test]
fn const_bound_arrows_and_functions_shorten_when_called_directly() {
    // A const/let bound to an anonymous arrow or function infers its name from
    // the binding, so keep_names used to keep it readable. When every use is a
    // direct call and no name is observed, it renames like a private function.
    let source = r#"const formatMoney = (amount) => "$" + amount.toFixed(2);
    const addOne = function (x) { return x + 1; };
    const inner = () => addOne(2);
    export const label = formatMoney(inner());"#;
    let output = splice(source);
    assert!(
        !output.contains("formatMoney"),
        "a const-bound arrow was left readable: {output}"
    );
    assert!(
        !output.contains("addOne"),
        "a const-bound function was left readable: {output}"
    );
    assert!(
        !output.contains("inner"),
        "a const-bound arrow was kept: {output}"
    );
    assert!(
        output.contains("label"),
        "the exported name must stay public"
    );
    compare_module(source, &output, "api.label");
}

#[test]
fn a_const_arrow_with_an_observed_name_is_preserved() {
    // Reading the arrow's `.name` makes the whole-file scan conservative, so the
    // binding keeps its spelling for the reflection to still work.
    let source = r#"const namedArrow = () => 1;
    export const info = namedArrow.name + ':' + namedArrow();"#;
    let output = splice(source);
    assert!(
        output.contains("namedArrow"),
        "an observed `.name` must keep the binding: {output}"
    );
    compare_module(source, &output, "api.info");
}

#[test]
fn a_const_arrow_used_as_a_value_keeps_its_name() {
    // The arrow escapes as an array element, not only a call, so the direct-use
    // check declines to rename it (its `.name` could be read through the value).
    let source = r#"const callback = () => 42;
    export const pair = [callback, callback()];"#;
    let output = splice(source);
    assert!(
        output.contains("callback"),
        "a value-escaping arrow must keep its name: {output}"
    );
    compare_module(source, &output, "[api.pair[0](), api.pair[1]]");
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

#[test]
fn an_inline_const_export_is_hidden_but_resolves_by_name() {
    let source =
        "export const proxyRouteCookie = 1; export function readCookie() { return proxyRouteCookie; }";
    let output = splice(source);
    assert!(
        !output.contains("export const proxyRouteCookie"),
        "the inline export was not rewritten: {output}"
    );
    assert!(
        output.contains("as proxyRouteCookie }"),
        "the public name was dropped: {output}"
    );
    // The binding is hidden: only the public export name keeps the long spelling.
    assert_eq!(output.matches("proxyRouteCookie").count(), 1, "{output}");
    compare_module(source, &output, "[api.proxyRouteCookie, api.readCookie()]");
}

#[test]
fn a_shorthand_export_resolves_after_the_binding_is_hidden() {
    let source = "const internalBinding = 41; export function bump() { return internalBinding + 1; } export { internalBinding };";
    let output = splice(source);
    assert!(
        !output.contains("const internalBinding"),
        "binding not renamed: {output}"
    );
    assert!(
        output.contains("as internalBinding }"),
        "public name dropped: {output}"
    );
    compare_module(source, &output, "[api.internalBinding, api.bump()]");
}

#[test]
fn an_aliased_export_keeps_its_alias_and_resolves() {
    let source = "const localThing = 7; export { localThing as publicThing };";
    let output = splice(source);
    assert!(
        !output.contains("localThing as"),
        "local binding not renamed: {output}"
    );
    assert!(
        output.contains("as publicThing }"),
        "alias dropped: {output}"
    );
    compare_module(source, &output, "api.publicThing");
}

#[test]
fn a_string_named_export_keeps_the_string_form_and_resolves() {
    let source = "const localThing = 9; export { localThing as \"weird-name\" };";
    let output = splice(source);
    assert!(
        output.contains("as \"weird-name\" }"),
        "string export name dropped: {output}"
    );
    compare_module(source, &output, "api['weird-name']");
}

#[test]
fn export_default_resolves_after_locals_are_hidden() {
    let source = "const helperValue = 5; export default helperValue + 1;";
    let output = splice(source);
    assert!(
        !output.contains("helperValue"),
        "top-level local not renamed: {output}"
    );
    compare_module(source, &output, "api.default");
}
