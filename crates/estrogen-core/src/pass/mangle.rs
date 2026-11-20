//! Shorten JavaScript locals without reprinting the AST.
//!
//! OXC resolves each occurrence and assigns short names without changing scope
//! resolution. This pass only writes identifier tokens. A shorthand property
//! keeps its key and expands to `key:short` when its local binding changes.

use std::collections::{HashMap, HashSet};

use oxc_ast::ast::{BindingIdentifier, BindingPattern, Expression, Program};
use oxc_ast::AstKind;
use oxc_mangler::{MangleOptions, MangleOptionsKeepNames, Mangler};
use oxc_semantic::{SemanticBuilder, SymbolId};
use oxc_span::Span;

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
    let mut occurrences: HashMap<SymbolId, Occurrences<'_>> = HashMap::new();
    let mut shorthand = HashSet::new();
    for node in semantic.nodes().iter() {
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
            // A named declaration can share a symbol with a prior `var` or
            // create a sloppy-mode outer binding (Annex B). Keep the spelling
            // across both bindings, not only the one on the declaration node.
            AstKind::Function(function) => reserve_binding(function.id.as_ref(), &mut options),
            AstKind::Class(class) => reserve_binding(class.id.as_ref(), &mut options),
            _ => {}
        }
    }

    options.reserved.extend(
        occurrences
            .values()
            .filter(|entry| entry.name.len() == 1)
            .map(|entry| entry.name.into()),
    );
    if occurrences.is_empty() {
        return Ok(EditSet::new());
    }
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

    let mut edits = EditSet::new();
    for (symbol, entry) in occurrences {
        let target = scoping.symbol_name(symbol);
        if target == entry.name || blocked.contains(entry.name) {
            continue;
        }
        for span in entry.spans {
            let start = span.start as usize;
            let end = span.end as usize;
            let replacement = if shorthand.contains(&span) {
                format!("{}:{target}", &source[start..end])
            } else {
                target.to_owned()
            };
            edits.push(Edit::replace(start, end, replacement));
        }
    }
    Ok(edits)
}

struct Occurrences<'a> {
    name: &'a str,
    spans: Vec<Span>,
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
}
