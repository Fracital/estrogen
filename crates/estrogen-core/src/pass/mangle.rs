//! Shorten JavaScript names without reprinting the AST.
//!
//! OXC resolves each occurrence and assigns short names without changing scope
//! resolution. This pass only writes identifier tokens. A shorthand property
//! keeps its key and expands to `key:short` when its local binding changes.
//!
//! In a file that has `import` or `export` syntax (so it is an ES module, not a
//! classic script whose top-level names may be globals) the pass also renames
//! top-level bindings. An exported binding keeps the public name importers use:
//! `export { proxyRouteCookie }` becomes `export { a as proxyRouteCookie }`, and
//! `export const proxyRouteCookie = v` becomes `const a = v; export { a as
//! proxyRouteCookie }`. The public export name stays a visible literal, because
//! a static export name must be an identifier or a string literal, never a
//! run-time expression. See `docs/TOPLEVEL-MANGLE.md`.

use std::collections::{HashMap, HashSet};

use oxc_ast::ast::{
    BindingIdentifier, BindingPattern, ClassElement, Declaration, Expression, ModuleExportName,
    Program, Statement,
};
use oxc_ast::AstKind;
use oxc_mangler::{MangleOptions, MangleOptionsKeepNames, Mangler};
use oxc_semantic::{AstNodes, Scoping, SemanticBuilder, SymbolId};
use oxc_span::{GetSpan, Span};

use crate::edit::{Edit, EditSet};
use crate::error::Result;

/// Collect renames against the same source as the literal-pool edits.
pub(crate) fn collect(program: &Program<'_>, source: &str, reserved: &str) -> Result<EditSet> {
    let built = SemanticBuilder::new()
        .with_build_nodes(true)
        .with_class_table(true)
        .build(program);
    let mut semantic = built.semantic;
    let mut options = MangleOptions {
        top_level: Some(false),
        keep_names: MangleOptionsKeepNames::all_true(),
        ..MangleOptions::default()
    };
    // The pool inserts a String reference inside template expressions. Keep
    // existing bindings of that name. Keep the decoder out of the allocator's
    // candidates and preserve the spellings a CommonJS export scanner reads.
    options.reserved.extend(
        [
            reserved, "String", "exports", "module", "require", "eval", "async", "of",
        ]
        .into_iter()
        .map(Into::into),
    );

    let scoping = semantic.scoping();
    let nodes = semantic.nodes();
    let root = scoping.root_scope_id();
    // OXC propagates direct eval to the ancestor scopes. The fresh-name pass
    // below must honor it too, even for declarations OXC leaves unchanged.
    if scoping.scope_flags(root).contains_direct_eval() {
        return Ok(EditSet::new());
    }
    let has_module_syntax = program.body.iter().any(|statement| {
        matches!(
            statement,
            Statement::ImportDeclaration(_)
                | Statement::ExportDeclaration(_)
                | Statement::ExportNamedDeclaration(_)
                | Statement::ExportDefaultDeclaration(_)
                | Statement::ExportAllDeclaration(_)
                | Statement::ExportFromDeclaration(_)
        )
    });
    let explicitly_strict = program
        .directives
        .iter()
        .any(|d| d.directive == "use strict");
    // An indirect read such as instance.constructor.name, or a destructure of
    // a constructor's name, need not reference the declaration it observes, so
    // the scan stays broad. The one `name` identifier that observes nothing is
    // an object-literal key: `{ name: value }` and the shorthand `{ name }` set
    // a property, they do not read a callable's `.name`. Those keys are common
    // in bundled code, and counting them kept the whole file from renaming, so
    // they are excluded here while every other `name` identifier still counts.
    let reads_callable_names = nodes.iter().any(|node| match node.kind() {
        AstKind::IdentifierName(name) if name.name == "name" => {
            !matches!(nodes.parent_kind(node.id()), AstKind::ObjectProperty(_))
        }
        AstKind::StringLiteral(name) => name.value.as_str() == Some("name"),
        AstKind::NewTarget(_) => true,
        _ => false,
    });
    let mut occurrences: HashMap<SymbolId, Occurrences<'_>> = HashMap::new();
    let mut shorthand = HashSet::new();
    // Export-specifier locals that are shorthand (`export { x }`, where the local
    // and the public name are the same token). Renaming the local here must keep
    // the public name: `x` becomes `<mangled> as x`.
    let mut export_shorthand: HashSet<Span> = HashSet::new();
    // Symbols named by any `export { ... }` specifier. These are not candidates
    // for the inline-export rename, since oxc does not rename a specifier local
    // (it is a plain reference), so a fresh rename would break the specifier.
    let mut exported_by_specifier: HashSet<SymbolId> = HashSet::new();
    // OXC keeps inline exports and callable names. Eligible private callable
    // declarations can use a fresh binding because only direct calls remain.
    let mut fresh_renames = Vec::new();
    let mut inline_exports: Vec<InlineExport<'_>> = Vec::new();

    for node in nodes.iter() {
        match node.kind() {
            // A with object can intercept a lookup that appears to be lexical.
            AstKind::WithStatement(_) => return Ok(EditSet::new()),
            AstKind::BindingIdentifier(id) => {
                if let Some(symbol) = id.symbol_id.get() {
                    occurrence(&mut occurrences, symbol, id.name.as_str(), id.span);
                }
            }
            AstKind::IdentifierReference(id) => {
                if let Some(symbol) = id
                    .reference_id
                    .get()
                    .and_then(|reference| scoping.get_reference(reference).symbol_id())
                {
                    occurrence(&mut occurrences, symbol, id.name.as_str(), id.span);
                }
            }
            AstKind::ObjectProperty(prop) if prop.shorthand => {
                if let Expression::Identifier(id) = &prop.value {
                    if id.name == "__proto__" {
                        // `__proto__: value` would be a prototype setter, not a data property.
                        options.reserved.insert(id.name.as_str().into());
                    } else {
                        shorthand.insert(id.span);
                    }
                }
            }
            AstKind::BindingProperty(prop) if prop.shorthand => {
                let pattern = match &prop.value {
                    BindingPattern::AssignmentPattern(default) => &default.left,
                    pattern => pattern,
                };
                if let BindingPattern::BindingIdentifier(id) = pattern {
                    shorthand.insert(id.span);
                }
            }
            AstKind::AssignmentTargetPropertyIdentifier(prop) => {
                shorthand.insert(prop.binding.span);
            }
            AstKind::FormalParameter(param) => {
                if let Some(init) = &param.initializer {
                    keep_inferred_name(&param.pattern, init, &mut options);
                }
            }
            AstKind::VariableDeclarator(decl) => {
                if let Some(init) = &decl.init {
                    // An initializer can be on a later `var` declaration for
                    // the same symbol, not its first declaration.
                    keep_inferred_name(&decl.id, init, &mut options);
                }
            }
            AstKind::AssignmentPattern(pattern) => {
                keep_inferred_name(&pattern.left, &pattern.right, &mut options);
            }
            AstKind::Function(_) | AstKind::Class(_) => {
                let (id, eligible, constructor_only) = match node.kind() {
                    AstKind::Function(function) => (
                        function.id.as_ref(),
                        function.is_declaration(),
                        false,
                    ),
                    AstKind::Class(class) => (
                        class.id.as_ref(),
                        class.is_declaration()
                            && class.heritage.is_none()
                            && class.decorators.is_empty()
                            && !class.body.body.iter().any(|element| {
                                matches!(element, ClassElement::StaticBlock(_))
                                    || matches!(element, ClassElement::PropertyDefinition(p) if p.r#static)
                                    || matches!(element, ClassElement::AccessorProperty(p) if p.r#static)
                            }),
                        true,
                    ),
                    _ => unreachable!(),
                };
                let Some(id) = id else { continue };
                // OXC must not reuse an original callable name for a different
                // symbol, including while allocating the fresh short name.
                reserve_binding(Some(id), &mut options);
                let symbol = id.symbol_id.get().filter(|symbol| {
                    eligible
                        && !reads_callable_names
                        // An exported callable's `.name` can be read by its importer.
                        && !matches!(
                            nodes.parent_kind(node.id()),
                            AstKind::ExportDeclaration(_) | AstKind::ExportDefaultDeclaration(_)
                        )
                        // A sloppy block function may also declare a var binding.
                        && (has_module_syntax
                            || (explicitly_strict && scoping.symbol_scope_id(*symbol) != root))
                        && scoping.symbol_declaration(*symbol) == node.id()
                        && scoping.symbol_redeclarations(*symbol).is_empty()
                        && uses_are_direct(nodes, scoping, *symbol, constructor_only)
                });
                if let Some(symbol) = symbol {
                    fresh_renames.push((symbol, id.name.as_str()));
                }
            }
            AstKind::ExportDeclaration(decl) => {
                if let Some(inline) = inline_export(decl.span, &decl.declaration) {
                    inline_exports.push(inline);
                }
            }
            // Never rename an import binding: the imported name is the other
            // module's contract, and the pass does not rewrite import specifiers.
            AstKind::ImportSpecifier(spec) => {
                options.reserved.insert(spec.local.name.as_str().into());
            }
            AstKind::ImportDefaultSpecifier(spec) => {
                options.reserved.insert(spec.local.name.as_str().into());
            }
            AstKind::ImportNamespaceSpecifier(spec) => {
                options.reserved.insert(spec.local.name.as_str().into());
            }
            AstKind::ExportSpecifier(spec) => {
                if let ModuleExportName::IdentifierReference(local) = &spec.local {
                    if let Some(symbol) = local
                        .reference_id
                        .get()
                        .and_then(|reference| scoping.get_reference(reference).symbol_id())
                    {
                        exported_by_specifier.insert(symbol);
                    }
                    if spec.local.span() == spec.exported.span() {
                        export_shorthand.insert(spec.local.span());
                    }
                }
            }
            _ => {}
        }
    }

    // Top-level renaming is safe only once the file is known to be a module.
    options.top_level = Some(has_module_syntax);

    options.reserved.extend(
        occurrences
            .values()
            .filter(|entry| entry.name.len() == 1)
            .map(|entry| entry.name.into()),
    );
    if occurrences.is_empty() {
        return Ok(EditSet::new());
    }
    // A snapshot of the reserved names, read back after the mangler takes the
    // options by value, to keep a fresh name clear of them.
    let reserved_snapshot: HashSet<String> = options
        .reserved
        .iter()
        .map(|name| name.as_str().to_string())
        .collect();

    Mangler::new()
        .with_options(options)
        .build_with_semantic(&mut semantic, program);
    let scoping = semantic.scoping();

    // Keep only renames that shorten their binding. Restoring any original
    // name can collide with a target OXC thought it had freed. Reject those
    // targets too, in a single worklist. Names kept by OXC were already safe;
    // this extra file-wide check only needs to run for restored names.
    let mut blocked = HashSet::new();
    let mut pending = Vec::new();
    let mut by_target: HashMap<&str, Vec<&str>> = HashMap::new();
    for (symbol, entry) in &occurrences {
        let target = scoping.symbol_name(*symbol);
        if target == entry.name {
            continue;
        }
        if target.len() >= entry.name.len() {
            if blocked.insert(entry.name) {
                pending.push(entry.name);
            }
        } else {
            by_target.entry(target).or_default().push(entry.name);
        }
    }
    while let Some(name) = pending.pop() {
        if let Some(originals) = by_target.remove(name) {
            for original in originals {
                if blocked.insert(original) {
                    pending.push(original);
                }
            }
        }
    }

    // Final name for every symbol the pass renames. OXC's renames come first;
    // the inline-export bindings OXC kept get a fresh name the pass allocates.
    let mut final_names: HashMap<SymbolId, String> = HashMap::new();
    for (symbol, entry) in &occurrences {
        let target = scoping.symbol_name(*symbol);
        if target != entry.name && !blocked.contains(entry.name) {
            final_names.insert(*symbol, target.to_string());
        }
    }

    let mut inline_rewrites: Vec<InlineRewrite> = Vec::new();
    if !inline_exports.is_empty() || !fresh_renames.is_empty() {
        let mut forbidden: HashSet<String> = HashSet::new();
        forbidden.extend(scoping.symbol_names().map(str::to_string));
        forbidden.extend(
            scoping
                .root_unresolved_references()
                .keys()
                .map(|name| name.as_str().to_string()),
        );
        forbidden.extend(reserved_snapshot);
        for inline in &inline_exports {
            // Skip a binding another export specifier also names, one oxc already
            // renamed, or one the shorten check blocked: a fresh rename there is
            // either unsafe or redundant.
            if exported_by_specifier.contains(&inline.symbol)
                || final_names.contains_key(&inline.symbol)
                || blocked.contains(inline.name)
            {
                continue;
            }
            if scoping.symbol_name(inline.symbol) != inline.name {
                continue; // oxc changed it against expectation; leave it alone.
            }
            let Some(fresh) = fresh_name(&forbidden, inline.name.len()) else {
                continue; // no shorter name is free; leave the export unchanged.
            };
            forbidden.insert(fresh.clone());
            final_names.insert(inline.symbol, fresh.clone());
            inline_rewrites.push(InlineRewrite {
                export_start: inline.export_span.start as usize,
                decl_start: inline.decl_span.start as usize,
                export_end: inline.export_span.end as usize,
                fresh,
                public: inline.name.to_string(),
            });
        }
        for (symbol, name) in fresh_renames {
            if exported_by_specifier.contains(&symbol)
                || final_names.contains_key(&symbol)
                || blocked.contains(name)
                || scoping.symbol_name(symbol) != name
            {
                continue;
            }
            let Some(fresh) = fresh_name(&forbidden, name.len()) else {
                continue;
            };
            forbidden.insert(fresh.clone());
            final_names.insert(symbol, fresh);
        }
    }

    let mut edits = EditSet::new();
    for (symbol, entry) in occurrences {
        let Some(final_name) = final_names.get(&symbol) else {
            continue;
        };
        for span in entry.spans {
            let start = span.start as usize;
            let end = span.end as usize;
            let replacement = if export_shorthand.contains(&span) {
                // `export { x }` -> `export { <mangled> as x }`.
                format!("{final_name} as {}", format_export_name(entry.name))
            } else if shorthand.contains(&span) {
                format!("{}:{final_name}", &source[start..end])
            } else {
                final_name.clone()
            };
            edits.push(Edit::replace(start, end, replacement));
        }
    }
    for rewrite in inline_rewrites {
        // Drop the `export ` keyword before the declaration.
        edits.push(Edit::replace(
            rewrite.export_start,
            rewrite.decl_start,
            String::new(),
        ));
        // Re-expose the binding under its old public name. The leading `;`
        // terminates the declaration whether or not it already ended in one.
        edits.push(Edit::insert(
            rewrite.export_end,
            format!(
                ";export {{ {} as {} }};",
                rewrite.fresh,
                format_export_name(&rewrite.public)
            ),
        ));
    }
    Ok(edits)
}

struct Occurrences<'a> {
    name: &'a str,
    spans: Vec<Span>,
}

/// A `export const/let/var NAME = value` the pass may rename and re-expose.
struct InlineExport<'a> {
    symbol: SymbolId,
    name: &'a str,
    export_span: Span,
    decl_span: Span,
}

/// The edits that hide one inline-declared export binding.
struct InlineRewrite {
    export_start: usize,
    decl_start: usize,
    export_end: usize,
    fresh: String,
    public: String,
}

fn occurrence<'a>(
    map: &mut HashMap<SymbolId, Occurrences<'a>>,
    symbol: SymbolId,
    name: &'a str,
    span: Span,
) {
    map.entry(symbol)
        .or_insert_with(|| Occurrences {
            name,
            spans: Vec::new(),
        })
        .spans
        .push(span);
}

/// Rename a callable only when every use is an immediate call or construction.
/// Passing it as a value could let other code observe its original name.
fn uses_are_direct(
    nodes: &AstNodes<'_>,
    scoping: &Scoping,
    symbol: SymbolId,
    constructor_only: bool,
) -> bool {
    scoping.get_resolved_references(symbol).all(|reference| {
        if reference.is_write() {
            return false;
        }
        let id = reference.node_id();
        let AstKind::IdentifierReference(name) = nodes.kind(id) else {
            return false;
        };
        match nodes.parent_kind(id) {
            AstKind::CallExpression(call) if !constructor_only => {
                !call.optional && call.callee.span() == name.span
            }
            AstKind::NewExpression(new) => new.callee.span() == name.span,
            _ => false,
        }
    })
}

fn reserve_binding(id: Option<&BindingIdentifier<'_>>, options: &mut MangleOptions) {
    if let Some(id) = id {
        options.reserved.insert(id.name.as_str().into());
    }
}

fn keep_inferred_name(
    pattern: &BindingPattern<'_>,
    init: &Expression<'_>,
    options: &mut MangleOptions,
) {
    if let BindingPattern::BindingIdentifier(id) = pattern {
        if init.is_anonymous_function_definition() {
            reserve_binding(Some(id), options);
        }
    }
}

/// An inline export the pass can hide: a single `const`/`let`/`var` declarator
/// bound to one identifier, whose value is not a function or class (whose
/// `.name` an importer could read). Everything else returns `None` and is left
/// as oxc kept it.
fn inline_export<'a>(export_span: Span, declaration: &Declaration<'a>) -> Option<InlineExport<'a>> {
    let Declaration::VariableDeclaration(var) = declaration else {
        return None;
    };
    if var.declarations.len() != 1 {
        return None;
    }
    let declarator = &var.declarations[0];
    let BindingPattern::BindingIdentifier(id) = &declarator.id else {
        return None;
    };
    if let Some(init) = &declarator.init {
        if init.is_anonymous_function_definition() {
            return None;
        }
    }
    let symbol = id.symbol_id.get()?;
    Some(InlineExport {
        symbol,
        name: id.name.as_str(),
        export_span,
        decl_span: var.span,
    })
}

/// A public export name for the `as` clause: the identifier itself when it is a
/// valid identifier, else a double-quoted string (the ES2022 arbitrary module
/// name form). A binding name is always a valid identifier, so the string form
/// only guards a public name that is not.
fn format_export_name(name: &str) -> String {
    if is_identifier_name(name) {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// Whether `name` is a plain ASCII JavaScript identifier. Unicode identifiers
/// are valid but are treated as needing the string form, which stays correct.
fn is_identifier_name(name: &str) -> bool {
    let mut chars = name.bytes();
    match chars.next() {
        Some(b) if b.is_ascii_alphabetic() || b == b'_' || b == b'$' => {}
        _ => return false,
    }
    chars.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$')
}

const FIRST: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_$";
const REST: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_$";

/// The shortest identifier shorter than `max_len` that is free: not in
/// `forbidden` and not a reserved word. Returns `None` when no candidate of
/// length one or two qualifies, in which case the caller leaves the binding
/// alone rather than make the output longer.
fn fresh_name(forbidden: &HashSet<String>, max_len: usize) -> Option<String> {
    if max_len <= 1 {
        return None;
    }
    let mut n: u64 = 0;
    loop {
        let name = nth_name(n);
        if name.len() >= max_len || name.len() > 2 {
            return None;
        }
        if !is_reserved_word(&name) && !forbidden.contains(&name) {
            return Some(name);
        }
        n += 1;
    }
}

/// The `n`th candidate identifier, shortest first: 54 one-character names, then
/// the two-character names, and so on.
fn nth_name(mut n: u64) -> String {
    let mut len = 1usize;
    loop {
        let count = 54u64 * 64u64.pow((len - 1) as u32);
        if n < count {
            break;
        }
        n -= count;
        len += 1;
    }
    let mut bytes = vec![0u8; len];
    for slot in bytes.iter_mut().skip(1).rev() {
        *slot = REST[(n % 64) as usize];
        n /= 64;
    }
    bytes[0] = FIRST[(n % 54) as usize];
    String::from_utf8(bytes).expect("ascii name")
}

/// Whether `name` is a word that cannot be a binding name. A one-character name
/// never is; this guards the two-character names (`if`, `in`, `do`, `of`, `as`).
fn is_reserved_word(name: &str) -> bool {
    matches!(
        name,
        "do" | "if"
            | "in"
            | "for"
            | "let"
            | "new"
            | "try"
            | "var"
            | "case"
            | "else"
            | "enum"
            | "eval"
            | "null"
            | "this"
            | "true"
            | "void"
            | "with"
            | "await"
            | "break"
            | "catch"
            | "class"
            | "const"
            | "false"
            | "super"
            | "throw"
            | "while"
            | "yield"
            | "delete"
            | "export"
            | "import"
            | "return"
            | "static"
            | "switch"
            | "typeof"
            | "default"
            | "extends"
            | "finally"
            | "continue"
            | "debugger"
            | "function"
            | "arguments"
            | "instanceof"
            // Contextual words the pass already keeps out of the pool.
            | "as"
            | "of"
            | "async"
            | "get"
            | "set"
    )
}

#[cfg(test)]
mod tests {
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;

    use super::*;

    fn mangle(source: &str, source_type: SourceType) -> String {
        let allocator = Allocator::default();
        let parsed = Parser::new(&allocator, source, source_type).parse();
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let output = collect(&parsed.program, source, "decoder")
            .unwrap()
            .apply(source)
            .unwrap();
        let reparsed = Parser::new(&allocator, &output, source_type).parse();
        assert!(
            reparsed.diagnostics.is_empty(),
            "{:?}",
            reparsed.diagnostics
        );
        output
    }

    #[test]
    fn preserves_names_in_parameter_defaults_and_later_var_initializers() {
        let input = "function demo(automaticCallback = () => 0, scalarParameter = 1) {
            var callbackAssignedLater;
            var callbackAssignedLater = () => scalarParameter;
            return [automaticCallback.name, callbackAssignedLater.name];
        }";
        let output = mangle(input, SourceType::mjs());
        assert!(output.contains("automaticCallback = "));
        assert!(output.contains("callbackAssignedLater = "));
        assert!(!output.contains("scalarParameter"));
    }

    #[test]
    fn expands_only_shorthand_tokens_and_keeps_comments() {
        let input = "function demo() {
            const { /* binding */ longProperty /* key */ = 3 } = {};
            return { longProperty /* value */ };
        }";
        let output = mangle(input, SourceType::mjs());
        assert!(output.contains("/* binding */ longProperty:"));
        assert!(output.contains("/* key */ = 3"));
        assert!(output.contains("return { longProperty:"));
        assert!(output.contains(" /* value */ }"));
    }

    #[test]
    fn keeps_object_proto_shorthand_and_generated_helper_names() {
        let input = "function demo(__proto__, String, readThis) {
            return { __proto__, value: String(readThis) };
        }";
        let output = mangle(input, SourceType::mjs());
        assert!(output.contains("{ __proto__, value: String("));
        assert!(!output.contains("readThis"));
    }

    #[test]
    fn leaves_dynamic_with_scopes_alone() {
        let input = "function demo(longName) { with ({ longName: 1 }) { return longName; } }";
        assert_eq!(mangle(input, SourceType::cjs()), input);
    }

    #[test]
    fn a_script_keeps_its_top_level_names() {
        // No module syntax, so the file might be a classic script whose top-level
        // `var` and `function` are globals. The pass must not rename them.
        let input = "var topLevelGlobal = 1; function topLevelHelper() { return topLevelGlobal; }";
        assert_eq!(mangle(input, SourceType::cjs()), input);
    }

    #[test]
    fn a_shorthand_export_keeps_its_public_name() {
        let input =
            "const internalBinding = 1; useIt(internalBinding); export { internalBinding };";
        let output = mangle(input, SourceType::mjs());
        assert!(!output.contains("const internalBinding"));
        // The public name survives behind an `as` clause.
        assert!(output.contains("as internalBinding }"));
    }

    #[test]
    fn an_aliased_export_keeps_its_public_alias() {
        let input =
            "const internalBinding = 1; useIt(internalBinding); export { internalBinding as publicName };";
        let output = mangle(input, SourceType::mjs());
        assert!(!output.contains("internalBinding as"));
        assert!(output.contains("as publicName }"));
    }

    #[test]
    fn an_inline_const_export_is_renamed_and_re_exposed() {
        let input = "export const proxyRouteCookie = 1; useIt(proxyRouteCookie);";
        let output = mangle(input, SourceType::mjs());
        assert!(!output.contains("export const proxyRouteCookie"));
        assert!(output.contains("as proxyRouteCookie }"));
        // The binding and its reference moved together.
        assert_eq!(output.matches("proxyRouteCookie").count(), 1);
    }

    #[test]
    fn a_non_identifier_export_name_uses_the_string_form() {
        let input = "const internalBinding = 1; useIt(internalBinding); export { internalBinding as \"weird-name\" };";
        let output = mangle(input, SourceType::mjs());
        assert!(output.contains("as \"weird-name\" }"));
    }

    #[test]
    fn an_imported_binding_is_left_unmangled() {
        let input = "import { importedName } from './m'; useIt(importedName);";
        let output = mangle(input, SourceType::mjs());
        assert!(output.contains("import { importedName }"));
        // The reference keeps the imported name too.
        assert_eq!(output.matches("importedName").count(), 2);
    }

    #[test]
    fn a_re_export_is_left_unmangled() {
        let input = "export { somethingElse } from './m';";
        assert_eq!(mangle(input, SourceType::mjs()), input);
    }

    #[test]
    fn an_exported_function_keeps_its_name() {
        // A function `.name` is observable, so the inline-export rename skips it.
        let input = "export function exportedFunction() { return 1; }";
        let output = mangle(input, SourceType::mjs());
        assert!(output.contains("export function exportedFunction"));
    }

    #[test]
    fn format_export_name_quotes_only_non_identifiers() {
        assert_eq!(format_export_name("plainName"), "plainName");
        assert_eq!(format_export_name("$weird_1"), "$weird_1");
        assert_eq!(format_export_name("weird-name"), "\"weird-name\"");
        assert_eq!(format_export_name("has space"), "\"has space\"");
    }

    #[test]
    fn fresh_name_is_short_free_and_not_a_keyword() {
        let mut forbidden = HashSet::new();
        forbidden.insert("a".to_string());
        let name = fresh_name(&forbidden, 10).unwrap();
        assert!(name.len() < 10);
        assert_ne!(name, "a");
        assert!(!is_reserved_word(&name));
        // A one-character original cannot be shortened.
        assert_eq!(fresh_name(&HashSet::new(), 1), None);
    }
}
