//! String and template literal pass.
//!
//! Spec: `obfuscate-build.mjs` (`collectEdits`, `shouldSkipLiteral`,
//! `literalIsComputedKey`, `prologueEnd`, `assemble`, and the `IDENT_PART` rule
//! in `applyEdits`). It collects every string literal in a safe value or key
//! position and every quasi of a non-tagged template literal, encrypts each
//! unique string into a per-file ChaCha20 pool, and replaces the literal with a
//! memoized `NAME(idx)` decode call. The positions that stay static are listed
//! in that file's header: directive prologues, import and export module
//! specifiers and names, import attributes, a `constructor` method key, a
//! `__proto__` proto key, and tagged template quasis.
//!
//! App chunks also shorten local bindings. Both passes collect edits against
//! the original source. Vendor chunks take the separate proxy-name path.
//!
//! The walk keeps a parent stack through [`Visit::enter_node`] and
//! [`Visit::leave_node`]: when a string or template literal is entered, the node
//! above it on the stack is its parent, which is what the skip and computed-key
//! rules read. estrogen edits the source by byte range; it does not reprint the AST.

use oxc_ast::ast::{Expression, PropertyKind, StringLiteral, TemplateLiteral};
use oxc_ast::AstKind;
use oxc_ast_visit::Visit;
use oxc_span::GetSpan;

use crate::edit::{Edit, EditSet};
use crate::error::{Error, Result};
use crate::format::PoolBuilder;
use crate::keys::FileKeys;
use crate::pass::{with_parsed_js, FileContext, Pass};

/// The literal-pool pass.
#[derive(Debug, Default)]
pub struct LiteralsPass;

impl Pass for LiteralsPass {
    fn name(&self) -> &'static str {
        "literals"
    }

    fn collect_edits(&self, ctx: &FileContext) -> Result<EditSet> {
        if crate::watermark::marked_js(ctx.source) {
            return Ok(EditSet::new());
        }
        with_parsed_js(&ctx.path, ctx.source, |program| {
            let keys = FileKeys::new(ctx.options, ctx.source.as_bytes())?;
            let pool = PoolBuilder::for_program(keys, program)?;
            let name = pool.name().to_string();
            let mut visitor = LiteralVisitor {
                source: ctx.source,
                name,
                pool,
                edits: EditSet::new(),
                stack: Vec::new(),
                error: None,
            };
            visitor.visit_program(program);
            if let Some(err) = visitor.error {
                return Err(err);
            }
            let mut edits = visitor.edits;
            edits.extend(super::mangle::collect(program, ctx.source, &visitor.name)?);
            super::assemble_js(&mut edits, ctx.source, program, visitor.pool);
            Ok(edits)
        })
    }
}

/// Walks a parsed program, encrypting string and template literals into the pool
/// and recording the byte-range edits that replace them with decode calls.
struct LiteralVisitor<'a> {
    source: &'a str,
    name: String,
    pool: PoolBuilder,
    edits: EditSet,
    stack: Vec<AstKind<'a>>,
    error: Option<Error>,
}

impl<'a> LiteralVisitor<'a> {
    /// Push `value` into the pool, recording any error for the caller to return.
    /// Returns the pool index, or `None` once an error has been seen.
    fn push(&mut self, value: &str) -> Option<usize> {
        if self.error.is_some() {
            return None;
        }
        match self.pool.push(value) {
            Ok(idx) => Some(idx),
            Err(err) => {
                self.error = Some(err);
                None
            }
        }
    }

    /// A value-position decode call, with a leading space if it would otherwise
    /// fuse with an identifier character just before it in the source (minified
    /// `return"x"`, `case"x":`, `typeof"x"`). The call text starts with the
    /// decoder name, so its first character is always an identifier character.
    fn value_call(&self, start: usize, idx: usize) -> String {
        let call = format!("{}({idx})", self.name);
        if start > 0 && byte_is_ident_part(self.source.as_bytes()[start - 1]) {
            format!(" {call}")
        } else {
            call
        }
    }

    /// Handle one string literal: skip it if its position requires it, convert a
    /// non-computed key to `[NAME(i)]`, otherwise replace the value with
    /// `NAME(i)`.
    fn handle_string(&mut self, lit: &StringLiteral<'a>) {
        let parent = match self.stack.last().copied() {
            Some(p) => p,
            None => return,
        };
        let value = match lit.value.as_str() {
            Some(v) => v,
            // A lone surrogate is not valid UTF-8; leave it as source rather than
            // risk a lossy round-trip. This does not occur in ordinary builds.
            None => return,
        };
        let span = lit.span;

        let mut computed_key = false;
        match parent {
            AstKind::ImportDeclaration(d) => {
                if d.source.span == span {
                    return; // module specifier
                }
            }
            AstKind::ExportFromDeclaration(d) => {
                if d.source.span == span {
                    return; // re-export module specifier
                }
            }
            AstKind::ExportAllDeclaration(d) => {
                if d.source.span == span {
                    return; // `export * from "x"` specifier
                }
                if d.exported.as_ref().map(|e| e.span()) == Some(span) {
                    return; // `export * as "name" from "x"`
                }
            }
            AstKind::ImportExpression(e) => {
                if expression_span(&e.source) == span {
                    return; // dynamic import() source
                }
            }
            AstKind::ImportSpecifier(_)
            | AstKind::ExportSpecifier(_)
            | AstKind::ImportAttribute(_)
            | AstKind::Directive(_) => {
                return; // binding names, import attributes, directive prologue
            }
            AstKind::ObjectProperty(p) => {
                if is_key(&p.key, span) && !p.computed {
                    if p.kind == PropertyKind::Init
                        && !p.method
                        && !p.shorthand
                        && value == "__proto__"
                    {
                        return; // proto-setter key stays a plain key
                    }
                    computed_key = true;
                }
            }
            // A string key in an object destructuring pattern (`const { "k": v }
            // = o`). OXC splits this from ObjectProperty, but it is the same key
            // position: a non-computed key must become `[NAME(i)]`, never a bare
            // value call, or the pattern no longer parses.
            AstKind::BindingProperty(p) => {
                if is_key(&p.key, span) && !p.computed {
                    if !p.shorthand && value == "__proto__" {
                        return; // a quoted `__proto__` key stays a plain key
                    }
                    computed_key = true;
                }
            }
            // The same key, in an assignment destructuring pattern
            // (`({ "k": v } = o)`). Its key field is `name`.
            AstKind::AssignmentTargetPropertyProperty(p) => {
                if is_key(&p.name, span) && !p.computed {
                    if value == "__proto__" {
                        return; // a quoted `__proto__` key stays a plain key
                    }
                    computed_key = true;
                }
            }
            AstKind::MethodDefinition(m) => {
                if is_key(&m.key, span) && !m.computed {
                    if value == "constructor" {
                        return; // constructor must stay a plain method key
                    }
                    computed_key = true;
                }
            }
            AstKind::PropertyDefinition(p) if is_key(&p.key, span) && !p.computed => {
                computed_key = true;
            }
            _ => {}
        }

        let idx = match self.push(value) {
            Some(i) => i,
            None => return,
        };
        let text = if computed_key {
            format!("[{}({idx})]", self.name)
        } else {
            self.value_call(span.start as usize, idx)
        };
        self.edits
            .push(Edit::replace(span.start as usize, span.end as usize, text));
    }

    /// Handle one template literal: skip a tagged quasi, replace an
    /// expression-free template with a single decode call, and splice the
    /// quasi gaps of an interpolated template.
    fn handle_template(&mut self, tpl: &TemplateLiteral<'a>) {
        if let Some(AstKind::TaggedTemplateExpression(tagged)) = self.stack.last().copied() {
            if tagged.quasi.span == tpl.span {
                return; // tagged template quasi stays static
            }
        }

        // Every quasi must have a cooked value (always true for an untagged
        // template, which is the only kind reaching here). Resolve them first so
        // a stray None skips the template without leaving half its edits.
        let mut cooked = Vec::with_capacity(tpl.quasis.len());
        for q in &tpl.quasis {
            match q.value.cooked.and_then(|c| c.as_str()) {
                Some(s) => cooked.push(s),
                None => return,
            }
        }

        let span = tpl.span;
        if tpl.expressions.is_empty() {
            let idx = match self.push(cooked[0]) {
                Some(i) => i,
                None => return,
            };
            let text = self.value_call(span.start as usize, idx);
            self.edits
                .push(Edit::replace(span.start as usize, span.end as usize, text));
            return;
        }

        // Leading gap: `...${  ->  (NAME(i0)+String(
        let i0 = match self.push(cooked[0]) {
            Some(i) => i,
            None => return,
        };
        let first_expr = expression_span(&tpl.expressions[0]);
        self.edits.push(Edit::replace(
            span.start as usize,
            first_expr.start as usize,
            format!("({}({i0})+String(", self.name),
        ));

        // Middle gaps: }quasi${  ->  )+NAME(ik)+String(
        for k in 1..tpl.expressions.len() {
            let ik = match self.push(cooked[k]) {
                Some(i) => i,
                None => return,
            };
            let prev_end = expression_span(&tpl.expressions[k - 1]).end as usize;
            let next_start = expression_span(&tpl.expressions[k]).start as usize;
            self.edits.push(Edit::replace(
                prev_end,
                next_start,
                format!(")+{}({ik})+String(", self.name),
            ));
        }

        // Trailing gap: }quasi`  ->  )+NAME(in))
        let last = cooked.len() - 1;
        let i_last = match self.push(cooked[last]) {
            Some(i) => i,
            None => return,
        };
        let last_expr_end =
            expression_span(&tpl.expressions[tpl.expressions.len() - 1]).end as usize;
        self.edits.push(Edit::replace(
            last_expr_end,
            span.end as usize,
            format!(")+{}({i_last}))", self.name),
        ));
    }
}

impl<'a> Visit<'a> for LiteralVisitor<'a> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        if self.error.is_none() {
            match kind {
                AstKind::StringLiteral(lit) => self.handle_string(lit),
                AstKind::TemplateLiteral(tpl) => self.handle_template(tpl),
                _ => {}
            }
        }
        self.stack.push(kind);
    }

    fn leave_node(&mut self, _kind: AstKind<'a>) {
        self.stack.pop();
    }
}

/// Whether `span` is exactly the property key's span, so the string is the key
/// rather than the value.
fn is_key(key: &oxc_ast::ast::PropertyKey, span: oxc_span::Span) -> bool {
    key.span() == span
}

/// The span of an expression.
fn expression_span(expr: &Expression) -> oxc_span::Span {
    expr.span()
}

/// Whether a byte is an identifier character, `[A-Za-z0-9_$]`.
fn byte_is_ident_part(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::decode_entry;
    use crate::options::{Canary, Options};
    use crate::walk::FileKind;

    fn seeded(canaries: &[&str]) -> Options {
        Options {
            seed: Some(b"literals-test-seed".to_vec()),
            canaries: canaries
                .iter()
                .map(|v| Canary {
                    value: (*v).to_string(),
                    reason: String::new(),
                })
                .collect(),
            ..Options::default()
        }
    }

    fn obfuscate(src: &str, opts: &Options) -> String {
        let ctx = FileContext::new("app.js", FileKind::Js, src, opts);
        let edits = LiteralsPass.collect_edits(&ctx).unwrap();
        edits.apply(src).unwrap()
    }

    fn decode_name(output: &str) -> &str {
        output
            .split_once("var ")
            .unwrap()
            .1
            .split_once("=(function()")
            .unwrap()
            .0
    }

    #[test]
    fn identity_when_no_literals() {
        let opts = seeded(&[]);
        let src = "const a = 1 + 2;";
        let ctx = FileContext::new("app.js", FileKind::Js, src, &opts);
        let edits = LiteralsPass.collect_edits(&ctx).unwrap();
        assert!(edits.is_empty());
        assert_eq!(edits.apply(src).unwrap(), src);
    }

    #[test]
    fn parse_error_fails_closed() {
        let opts = seeded(&[]);
        let src = "function (";
        let ctx = FileContext::new("bad.js", FileKind::Js, src, &opts);
        assert!(LiteralsPass.collect_edits(&ctx).is_err());
    }

    #[test]
    fn value_strings_are_encrypted_and_marked() {
        let opts = seeded(&[]);
        let out = obfuscate("const a = 'secret value';", &opts);
        assert!(out.starts_with("/* generated by https://estro.help */"));
        assert!(out.contains("=(function(){")); // decoder
        assert!(!out.contains("'secret value'"));
        assert!(out.contains(&format!("{}(0)", decode_name(&out)))); // a decode call
    }

    #[test]
    fn directive_and_specifiers_stay_literal() {
        let opts = seeded(&[]);
        let src = "\"use strict\";\nimport x from \"./other.js\";\nexport { y } from \"./y.js\";\nconst v = \"plain-value\";\n";
        let out = obfuscate(src, &opts);
        assert!(out.contains("\"use strict\""), "directive must stay");
        assert!(out.contains("\"./other.js\""), "import specifier stays");
        assert!(out.contains("\"./y.js\""), "re-export specifier stays");
        assert!(!out.contains("\"plain-value\""), "value is encrypted");
    }

    #[test]
    fn hashbang_and_unterminated_directive_stay_first() {
        let opts = seeded(&[]);
        let src = "#!/usr/bin/env node\r\n'use strict'\nconsole.log('hi');";
        let out = obfuscate(src, &opts);
        assert!(out.starts_with(
            "#!/usr/bin/env node\r\n/* generated by https://estro.help */'use strict'\nvar "
        ));
        crate::pass::ensure_js_reparses(std::path::Path::new("app.js"), &out).unwrap();
        assert_eq!(obfuscate(&out, &opts), out);
    }

    #[test]
    fn import_attribute_stays_literal() {
        let opts = seeded(&[]);
        let src = "import data from \"./d.json\" with { type: \"json\" };\n";
        let out = obfuscate(src, &opts);
        assert!(out.contains("\"./d.json\""));
        assert!(out.contains("type: \"json\""), "import attribute stays");
    }

    #[test]
    fn computed_and_plain_keys_are_distinguished() {
        let opts = seeded(&[]);
        // "a" is a non-computed key -> [NAME(i)]; the value "v" -> NAME(i).
        let out = obfuscate("const o = { \"a\": \"v\" };", &opts);
        assert!(!out.contains("\"a\""));
        assert!(!out.contains("\"v\""));
        assert!(
            out.contains(&format!("[{}(", decode_name(&out))),
            "key became computed"
        );
    }

    #[test]
    fn proto_key_stays_literal() {
        let opts = seeded(&[]);
        let out = obfuscate("const o = { \"__proto__\": base };", &opts);
        assert!(
            out.contains("\"__proto__\""),
            "proto key must stay a plain key"
        );
    }

    #[test]
    fn constructor_key_stays_literal() {
        let opts = seeded(&[]);
        let out = obfuscate("class C { \"constructor\"() {} }", &opts);
        assert!(
            out.contains("\"constructor\""),
            "constructor key must stay plain"
        );
    }

    #[test]
    fn non_constructor_string_method_key_is_computed() {
        let opts = seeded(&[]);
        let out = obfuscate("class C { \"run\"() {} }", &opts);
        assert!(!out.contains("\"run\""));
        assert!(out.contains(&format!("[{}(", decode_name(&out))));
    }

    #[test]
    fn destructuring_string_keys_become_computed() {
        let opts = seeded(&[]);
        // The reduced trigger from the failing Astro chunk: a string key in an
        // object destructuring pattern must become `[NAME(i)]`, not a bare call.
        let src =
            "const { lang: _, \"data-astro-id\": astroId, \"define:vars\": vars, ...props } = p;";
        let out = obfuscate(src, &opts);
        assert!(!out.contains("\"data-astro-id\""));
        assert!(!out.contains("\"define:vars\""));
        assert!(
            out.contains(&format!("[{}(", decode_name(&out))),
            "key became computed: {out}"
        );
        crate::pass::ensure_js_reparses(std::path::Path::new("app.js"), &out).unwrap();
    }

    #[test]
    fn assignment_destructuring_string_keys_become_computed() {
        let opts = seeded(&[]);
        let src = "let a, b; ({ \"x-y\": a, \"z w\": b } = o);";
        let out = obfuscate(src, &opts);
        assert!(!out.contains("\"x-y\""));
        assert!(!out.contains("\"z w\""));
        assert!(out.contains(&format!("[{}(", decode_name(&out))));
        crate::pass::ensure_js_reparses(std::path::Path::new("app.js"), &out).unwrap();
    }

    #[test]
    fn destructuring_proto_key_stays_literal() {
        let opts = seeded(&[]);
        let out = obfuscate("const { \"__proto__\": base } = o;", &opts);
        assert!(
            out.contains("\"__proto__\""),
            "a quoted proto key in a pattern stays plain: {out}"
        );
    }

    #[test]
    fn template_without_expressions() {
        let opts = seeded(&[]);
        let out = obfuscate("const a = `hello world`;", &opts);
        assert!(!out.contains("`hello world`"));
        assert!(out.contains(&format!("{}(0)", decode_name(&out))));
    }

    #[test]
    fn template_with_expressions_keeps_structure() {
        let opts = seeded(&[]);
        let out = obfuscate("const a = `x${y}z${w}`;", &opts);
        assert!(out.contains("+String(y)+"));
        assert!(out.contains("+String(w)+"));
        assert!(!out.contains("`x"));
    }

    #[test]
    fn tagged_template_quasi_stays_literal() {
        let opts = seeded(&[]);
        let out = obfuscate("const a = tag`hello ${x}`;", &opts);
        assert!(out.contains("tag`hello ${x}`"), "tagged quasi stays static");
    }

    #[test]
    fn return_string_gets_separating_space() {
        let opts = seeded(&[]);
        // Minified: no space after return. The decode call must not fuse.
        let out = obfuscate("function f(){return\"x\"}", &opts);
        assert!(
            out.contains(&format!("return {}(", decode_name(&out))),
            "got: {out}"
        );
    }

    #[test]
    fn unicode_round_trips_through_the_pool() {
        let opts = seeded(&[]);
        let src = "const a = 'héllo ☃ \u{1f600}';";
        // Drive the pool directly to confirm the entry decodes to the original.
        let keys = FileKeys::new(&opts, src.as_bytes()).unwrap();
        let mut b = PoolBuilder::new(keys).unwrap();
        let idx = b.push("héllo ☃ \u{1f600}").unwrap();
        let pool = b.into_pool();
        let decoded = decode_entry(&pool.key_b64, &pool.entries[idx]).unwrap();
        assert_eq!(decoded, "héllo ☃ \u{1f600}".as_bytes());
        // And the pass encrypts it (not verbatim).
        let out = obfuscate(src, &opts);
        assert!(!out.contains('☃'));
    }

    #[test]
    fn a_string_equal_to_a_canary_is_encrypted() {
        let opts = seeded(&["Fracital"]);
        let out = obfuscate("const brand = 'Fracital';", &opts);
        assert!(!out.contains("'Fracital'"));
        assert!(
            !out.contains("Fracital"),
            "canary must not survive verbatim"
        );
    }

    #[test]
    fn deterministic_under_seed() {
        let opts = seeded(&[]);
        let src = "const a = 'x'; const b = `t${a}`;";
        assert_eq!(obfuscate(src, &opts), obfuscate(src, &opts));
    }
}
