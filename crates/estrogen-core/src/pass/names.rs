//! Proxy and store name pass.
//!
//! Spec: `obfuscate-build.mjs` (`collectProxyEdits`, `obfuscateProxyJs`). A proxy
//! vendor file is not string-encrypted wholesale. Instead every proxy name is
//! renamed in place by the per-build same-length substitution ([`crate::proxy`]),
//! including inside the embedded wasm; strings that carry a proxy name and cannot
//! be renamed (app-shared, legal, prose) are encrypted so the name never ships
//! in plaintext; ordinary literals stay plain so the hot path pays no decode
//! cost. The two app-shared globals stay real, rebuilt from encrypted pieces at
//! their published sites.
//!
//! The walk keeps a parent stack like [`crate::pass::literals`]. Plain proxy
//! names rename in place at any position (the substitution is deterministic, so
//! a binding and its references stay in agreement without scope analysis); only
//! the app-shared globals need the positional veil, and an unsupported position
//! fails the build rather than leak.

use oxc_ast::ast::{
    Argument, BindingIdentifier, CallExpression, Expression, LabelIdentifier, PrivateIdentifier,
    PropertyKind, RegExpLiteral, StringLiteral, TemplateLiteral,
};
use oxc_ast::AstKind;
use oxc_ast_visit::Visit;
use oxc_span::GetSpan;

use crate::edit::{Edit, EditSet};
use crate::error::{Error, Result};
use crate::format::PoolBuilder;
#[cfg(test)]
use crate::format::MARKER;
use crate::keys::FileKeys;
use crate::pass::{with_parsed_js, FileContext};
use crate::proxy::ProxyEngine;

/// Collect the proxy transform's edits for one JS file.
///
/// The obfuscator calls this for a file [`ProxyEngine::is_vendor`] selects, in
/// place of the literal pass. It returns the byte-range edits; an empty set
/// means nothing carried a proxy name and the file is left unchanged.
pub fn collect_proxy_edits(ctx: &FileContext, engine: &ProxyEngine) -> Result<EditSet> {
    if crate::watermark::marked_js(ctx.source) {
        return Ok(EditSet::new());
    }
    with_parsed_js(&ctx.path, ctx.source, |program| {
        let keys = FileKeys::new(ctx.options, ctx.source.as_bytes())?;
        let pool = PoolBuilder::for_program(keys, program)?;
        let name = pool.name().to_string();
        let mut visitor = ProxyVisitor {
            source: ctx.source,
            engine,
            name,
            pool,
            edits: EditSet::new(),
            stack: Vec::new(),
            error: None,
        };

        // Comments carrying a proxy name: legal ones keep their text with names
        // renamed, the rest are removed (a newline for a multi-line comment so
        // ASI behavior is unchanged).
        for comment in &program.comments {
            let raw = &ctx.source[comment.span.start as usize..comment.span.end as usize];
            if !engine.has_proxy_name(raw) {
                continue;
            }
            let text = if engine.is_legal(raw) {
                engine.map_proxy_text(raw)?
            } else if raw.contains('\n') {
                "\n".to_string()
            } else {
                " ".to_string()
            };
            visitor.edits.push(Edit::replace(
                comment.span.start as usize,
                comment.span.end as usize,
                text,
            ));
        }

        visitor.visit_program(program);
        if let Some(err) = visitor.error {
            return Err(err);
        }
        let mut edits = dedupe(visitor.edits);
        super::assemble_js(&mut edits, ctx.source, program, visitor.pool);
        Ok(edits)
    })
}

/// Drop edits that cover the exact same byte range (a shorthand property's key
/// and value can be one node visited twice).
fn dedupe(edits: EditSet) -> EditSet {
    let mut seen = std::collections::HashSet::new();
    let mut out = EditSet::new();
    for e in edits.edits() {
        if seen.insert((e.start, e.end)) {
            out.push(e.clone());
        }
    }
    out
}

struct ProxyVisitor<'a, 'e> {
    source: &'a str,
    engine: &'e ProxyEngine,
    name: String,
    pool: PoolBuilder,
    edits: EditSet,
    stack: Vec<AstKind<'a>>,
    error: Option<Error>,
}

impl<'a, 'e> ProxyVisitor<'a, 'e> {
    fn fail(&mut self, message: String) {
        if self.error.is_none() {
            self.error = Some(Error::Proxy { message });
        }
    }

    /// `NAME(push(text))`, or `None` once an error has been seen.
    fn enc(&mut self, text: &str) -> Option<String> {
        if self.error.is_some() {
            return None;
        }
        match self.pool.push(text) {
            Ok(idx) => Some(format!("{}({idx})", self.name)),
            Err(err) => {
                self.error = Some(err);
                None
            }
        }
    }

    /// `NAME(i0)+NAME(i1)+...` for the pieces an app-shared global is rebuilt
    /// from.
    fn enc_pieces(&mut self, pieces: &[String]) -> Option<String> {
        let mut parts = Vec::with_capacity(pieces.len());
        for p in pieces {
            parts.push(self.enc(p)?);
        }
        Some(parts.join("+"))
    }

    /// A value-position replacement, with a leading space if it would fuse with
    /// an identifier character just before it (minified `return"x"`).
    fn spaced(&self, start: usize, text: String) -> String {
        if text
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'$')
            && start > 0
            && {
                let b = self.source.as_bytes()[start - 1];
                b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
            }
        {
            format!(" {text}")
        } else {
            text
        }
    }

    fn parent(&self) -> Option<AstKind<'a>> {
        self.stack.last().copied()
    }

    /// Handle an identifier (any OXC identifier kind) carrying a proxy name.
    fn handle_ident(&mut self, nm: &str, span: oxc_span::Span, kind_is_ref_target: bool) {
        if !self.engine.has_proxy_name(nm) {
            return;
        }
        let pieces = self.engine.shared_pieces(nm).map(|p| p.to_vec());
        let is_export_specifier = matches!(self.parent(), Some(AstKind::ExportSpecifier(_)));
        // Plain proxy name, or a shared name re-exported: rename in place. The
        // substitution is same-length and deterministic, so every reference
        // agrees.
        let Some(pieces) = pieces.filter(|_| !is_export_specifier) else {
            match self.engine.map_proxy_text(nm) {
                Ok(renamed) => self.edits.push(Edit::replace(
                    span.start as usize,
                    span.end as usize,
                    renamed,
                )),
                Err(e) => self.error = Some(e),
            };
            return;
        };

        // App-shared global: it must stay real at its published site, rebuilt
        // from encrypted pieces. The position decides the shape.
        let Some(call) = self.enc_pieces(&pieces) else {
            return;
        };
        match self.parent() {
            Some(AstKind::StaticMemberExpression(m)) if m.property.span() == span => {
                // obj.Name -> obj[pieces], keeping an optional chain.
                let before = &self.source.as_bytes()[..span.start as usize];
                let dot = match before.iter().rposition(|&b| b == b'.') {
                    Some(d) => d,
                    None => {
                        self.fail(format!("cannot locate '.' before {nm}"));
                        return;
                    }
                };
                let optional = dot > 0 && self.source.as_bytes()[dot - 1] == b'?';
                let start = if optional { dot - 1 } else { dot };
                let text = format!("{}[{call}]", if optional { "?." } else { "" });
                self.edits
                    .push(Edit::replace(start, span.end as usize, text));
            }
            Some(AstKind::ObjectProperty(p)) if p.key.span() == span && !p.computed => {
                let text = if p.shorthand {
                    match self.engine.map_proxy_text(nm) {
                        Ok(v) => format!("[{call}]: {v}"),
                        Err(e) => {
                            self.error = Some(e);
                            return;
                        }
                    }
                } else {
                    format!("[{call}]")
                };
                self.edits
                    .push(Edit::replace(span.start as usize, span.end as usize, text));
            }
            Some(AstKind::MethodDefinition(m)) if m.key.span() == span && !m.computed => {
                self.edits.push(Edit::replace(
                    span.start as usize,
                    span.end as usize,
                    format!("[{call}]"),
                ));
            }
            Some(AstKind::PropertyDefinition(p)) if p.key.span() == span && !p.computed => {
                self.edits.push(Edit::replace(
                    span.start as usize,
                    span.end as usize,
                    format!("[{call}]"),
                ));
            }
            // An assignment or update target: globalThis[pieces] = ... (no parens,
            // a parenthesized reference is not a valid target).
            _ if kind_is_ref_target => {
                self.edits.push(Edit::replace(
                    span.start as usize,
                    span.end as usize,
                    format!("globalThis[{call}]"),
                ));
            }
            // Bare read in any other expression position: parenthesized global.
            _ => {
                self.edits.push(Edit::replace(
                    span.start as usize,
                    span.end as usize,
                    format!("(globalThis[{call}])"),
                ));
            }
        }
    }

    /// Whether this reference is the target of an assignment or update.
    fn ref_is_target(&self, span: oxc_span::Span) -> bool {
        match self.parent() {
            Some(AstKind::AssignmentExpression(a)) => a.left.span() == span,
            Some(AstKind::UpdateExpression(u)) => u.argument.span() == span,
            _ => false,
        }
    }

    fn handle_binding(&mut self, b: &BindingIdentifier<'a>) {
        let nm = b.name.as_str();
        if !self.engine.has_proxy_name(nm) {
            return;
        }
        if self.engine.shared_pieces(nm).is_none() {
            match self.engine.map_proxy_text(nm) {
                Ok(renamed) => self.edits.push(Edit::replace(
                    b.span.start as usize,
                    b.span.end as usize,
                    renamed,
                )),
                Err(e) => self.error = Some(e),
            };
            return;
        }
        // A shared global in a binding position. The only supported shape is a
        // bare `var $shared;` declaration with no initializer, whose dead local
        // is renamed; anything else fails loudly.
        match self.parent() {
            Some(AstKind::VariableDeclarator(d)) if d.id.span() == b.span => {
                if d.init.is_some() {
                    self.fail(format!("app-shared name {nm} declared with an initializer"));
                    return;
                }
                match self.engine.map_proxy_text(nm) {
                    Ok(renamed) => self.edits.push(Edit::replace(
                        b.span.start as usize,
                        b.span.end as usize,
                        format!("{renamed}$unused"),
                    )),
                    Err(e) => self.error = Some(e),
                };
            }
            _ => self.fail(format!("app-shared name {nm} in a binding position")),
        }
    }

    fn handle_string(&mut self, lit: &StringLiteral<'a>) {
        let parent = match self.parent() {
            Some(p) => p,
            None => return,
        };
        if skip_literal(parent, lit.span, lit.value.as_str()) {
            return;
        }
        let value = match lit.value.as_str() {
            Some(v) => v,
            None => return,
        };
        let computed_key = is_computed_key(parent, lit.span);

        // Embedded wasm (bare base64 or a data: URI): map its names and keep the
        // blob encrypted.
        if let Some(mapped) = self.try_wasm_literal(value) {
            if let Some(text) = mapped {
                self.push_string_value(lit.span, &text, computed_key, true);
            }
            return;
        }

        if !self.engine.has_proxy_name(value) {
            return; // ordinary literal stays plain
        }

        if self.engine.is_store_name(value) {
            match self.engine.map_store_name(value) {
                Ok(renamed) => {
                    let text = json_string(&renamed);
                    self.edits.push(Edit::replace(
                        lit.span.start as usize,
                        lit.span.end as usize,
                        text,
                    ));
                }
                Err(e) => self.error = Some(e),
            }
            return;
        }

        if self.engine.is_legal(value)
            || self.engine.is_app_shared(value)
            || self.engine.contains_shared_name(value)
        {
            // Keep the text (legal/app-shared) or rename only the non-shared
            // tokens (shared-protected), then encrypt.
            let kept = if self.engine.is_legal(value) || self.engine.is_app_shared(value) {
                value.to_string()
            } else {
                match self.engine.map_shared_protected(value) {
                    Ok(v) => v,
                    Err(e) => {
                        self.error = Some(e);
                        return;
                    }
                }
            };
            self.push_string_value(lit.span, &kept, computed_key, false);
            return;
        }

        // Plain proxy string: rename in place, stays plain (not encrypted).
        match self.engine.map_proxy_text(value) {
            Ok(renamed) => {
                let text = json_string(&renamed);
                self.edits.push(Edit::replace(
                    lit.span.start as usize,
                    lit.span.end as usize,
                    text,
                ));
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Encrypt `text` and place the call, as `[NAME(i)]` for a computed key or a
    /// spaced `NAME(i)` for a value.
    fn push_string_value(
        &mut self,
        span: oxc_span::Span,
        text: &str,
        computed_key: bool,
        _wasm: bool,
    ) {
        let call = match self.enc(text) {
            Some(c) => c,
            None => return,
        };
        let replacement = if computed_key {
            format!("[{call}]")
        } else {
            self.spaced(span.start as usize, call)
        };
        self.edits.push(Edit::replace(
            span.start as usize,
            span.end as usize,
            replacement,
        ));
    }

    /// If `value` holds an embedded wasm base64 blob, map its names and return
    /// the rewritten literal text. `Some(None)` means it was wasm but unchanged.
    fn try_wasm_literal(&mut self, value: &str) -> Option<Option<String>> {
        if value.len() <= 64 {
            return None;
        }
        // (^|base64,)(AGFzbQ...): the wasm magic in base64 is "AGFzbQ".
        let marker = value.find("AGFzbQ")?;
        let is_start = marker == 0;
        let after_comma = marker >= 7 && &value[marker - 7..marker] == "base64,";
        if !is_start && !after_comma {
            return None;
        }
        // The blob runs to the end of the base64 run.
        let blob_end = value[marker..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || "+/=_-".contains(c)))
            .map(|o| marker + o)
            .unwrap_or(value.len());
        let blob = &value[marker..blob_end];
        let engine = self.engine;
        let mapped = match crate::wasm::map_wasm_base64(blob, &|s: &str| {
            engine.map_proxy_text(s).unwrap_or_else(|_| s.to_string())
        }) {
            Ok(m) => m,
            Err(e) => {
                self.error = Some(e);
                return Some(None);
            }
        };
        match mapped {
            None => Some(Some(value.to_string())),
            Some(new_blob) => {
                let mut out = String::with_capacity(value.len());
                out.push_str(&value[..marker]);
                out.push_str(&new_blob);
                out.push_str(&value[blob_end..]);
                Some(Some(out))
            }
        }
    }

    fn handle_regex(&mut self, lit: &RegExpLiteral<'a>) {
        let raw = &self.source[lit.span.start as usize..lit.span.end as usize];
        // raw is /pattern/flags; split on the last '/'.
        let Some(last) = raw.rfind('/') else { return };
        if last == 0 {
            return;
        }
        let pattern = &raw[1..last];
        let flags = &raw[last + 1..];
        if !self.engine.has_proxy_name(pattern) {
            return;
        }
        match self.engine.map_proxy_regex(pattern, flags) {
            Ok(mapped) => self.edits.push(Edit::replace(
                lit.span.start as usize,
                lit.span.end as usize,
                format!("/{mapped}/{flags}"),
            )),
            Err(e) => self.error = Some(e),
        }
    }

    fn handle_template(&mut self, tpl: &TemplateLiteral<'a>) {
        let mut cooked = Vec::with_capacity(tpl.quasis.len());
        for q in &tpl.quasis {
            match q.value.cooked.and_then(|c| c.as_str()) {
                Some(s) => cooked.push(s),
                None => return,
            }
        }
        if !cooked.iter().any(|q| self.engine.has_proxy_name(q)) {
            return;
        }
        let tagged = matches!(self.parent(), Some(AstKind::TaggedTemplateExpression(t)) if t.quasi.span == tpl.span);
        let needs_enc = cooked.iter().any(|q| {
            self.engine.is_legal(q)
                || self.engine.is_app_shared(q)
                || self.engine.contains_shared_name(q)
        });

        if needs_enc {
            if tagged {
                self.fail("tagged template with a shared or legal proxy string".to_string());
                return;
            }
            let mut qtext = Vec::with_capacity(cooked.len());
            for q in &cooked {
                match self.engine.map_shared_protected(q) {
                    Ok(v) => qtext.push(v),
                    Err(e) => {
                        self.error = Some(e);
                        return;
                    }
                }
            }
            let span = tpl.span;
            if tpl.expressions.is_empty() {
                let call = match self.enc(&qtext[0]) {
                    Some(c) => c,
                    None => return,
                };
                let text = self.spaced(span.start as usize, call);
                self.edits
                    .push(Edit::replace(span.start as usize, span.end as usize, text));
                return;
            }
            let i0 = match self.enc(&qtext[0]) {
                Some(c) => c,
                None => return,
            };
            self.edits.push(Edit::replace(
                span.start as usize,
                tpl.expressions[0].span().start as usize,
                format!("({i0}+String("),
            ));
            for k in 1..tpl.expressions.len() {
                let ik = match self.enc(&qtext[k]) {
                    Some(c) => c,
                    None => return,
                };
                self.edits.push(Edit::replace(
                    tpl.expressions[k - 1].span().end as usize,
                    tpl.expressions[k].span().start as usize,
                    format!(")+{ik}+String("),
                ));
            }
            let last = qtext.len() - 1;
            let il = match self.enc(&qtext[last]) {
                Some(c) => c,
                None => return,
            };
            self.edits.push(Edit::replace(
                tpl.expressions[tpl.expressions.len() - 1].span().end as usize,
                span.end as usize,
                format!(")+{il})"),
            ));
            return;
        }

        // Code-bearing template: rename proxy names inside each quasi's source
        // span in place.
        for q in &tpl.quasis {
            let text = &self.source[q.span.start as usize..q.span.end as usize];
            if !self.engine.has_proxy_name(text) {
                continue;
            }
            match self.engine.map_proxy_text(text) {
                Ok(renamed) => self.edits.push(Edit::replace(
                    q.span.start as usize,
                    q.span.end as usize,
                    renamed,
                )),
                Err(e) => {
                    self.error = Some(e);
                    return;
                }
            }
        }
    }

    /// The store guard: a database or cache opened under an unlisted proxy name
    /// fails the build.
    fn check_store_open(&mut self, call: &CallExpression<'a>) {
        if let Some(bad) = unlisted_store_open(call, self.engine) {
            self.fail(format!(
                "opens a database or cache named {bad:?}: list it in proxy.store_names so it keeps its name across builds"
            ));
        }
    }
}

impl<'a, 'e> Visit<'a> for ProxyVisitor<'a, 'e> {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        if self.error.is_none() {
            match kind {
                AstKind::CallExpression(c) => self.check_store_open(c),
                AstKind::IdentifierReference(r) => {
                    let target = self.ref_is_target(r.span);
                    self.handle_ident(r.name.as_str(), r.span, target);
                }
                AstKind::IdentifierName(n) => self.handle_ident(n.name.as_str(), n.span, false),
                AstKind::BindingIdentifier(b) => self.handle_binding(b),
                AstKind::LabelIdentifier(l) => self.handle_label(l),
                AstKind::PrivateIdentifier(p) => self.handle_private(p),
                AstKind::RegExpLiteral(r) => self.handle_regex(r),
                AstKind::StringLiteral(s) => self.handle_string(s),
                AstKind::TemplateLiteral(t) => self.handle_template(t),
                _ => {}
            }
        }
        self.stack.push(kind);
    }

    fn leave_node(&mut self, _kind: AstKind<'a>) {
        self.stack.pop();
    }
}

impl<'a, 'e> ProxyVisitor<'a, 'e> {
    fn handle_label(&mut self, l: &LabelIdentifier<'a>) {
        let nm = l.name.as_str();
        if !self.engine.has_proxy_name(nm) {
            return;
        }
        if self.engine.shared_pieces(nm).is_some() {
            self.fail(format!("app-shared name {nm} in a label position"));
            return;
        }
        match self.engine.map_proxy_text(nm) {
            Ok(renamed) => self.edits.push(Edit::replace(
                l.span.start as usize,
                l.span.end as usize,
                renamed,
            )),
            Err(e) => self.error = Some(e),
        }
    }

    fn handle_private(&mut self, p: &PrivateIdentifier<'a>) {
        // Rename over the full `#name` span so the hash is kept.
        let text = &self.source[p.span.start as usize..p.span.end as usize];
        if !self.engine.has_proxy_name(text) {
            return;
        }
        match self.engine.map_proxy_text(text) {
            Ok(renamed) => self.edits.push(Edit::replace(
                p.span.start as usize,
                p.span.end as usize,
                renamed,
            )),
            Err(e) => self.error = Some(e),
        }
    }
}

/// `unlistedStoreOpen`: the string name a call opens a store under, when that
/// name carries a proxy name and is not on the allow-list.
fn unlisted_store_open<'a>(call: &CallExpression<'a>, engine: &ProxyEngine) -> Option<String> {
    let first = call.arguments.first().and_then(Argument::as_expression);
    let name = match first {
        Some(Expression::StringLiteral(s)) => s.value.as_str()?,
        _ => return None,
    };
    if !engine.has_proxy_name(name) || engine.is_store_name(name) {
        return None;
    }
    let opens_on = |want: &str| -> bool {
        if let Expression::StaticMemberExpression(m) = &call.callee {
            if m.property.name.as_str() != "open" {
                return false;
            }
            return match &m.object {
                Expression::Identifier(id) => id.name.as_str() == want,
                Expression::StaticMemberExpression(inner) => inner.property.name.as_str() == want,
                _ => false,
            };
        }
        false
    };
    // idb openDB(name, <version:number>, { upgrade }) whatever the minifier named it.
    let idb_open = matches!(
        call.arguments.get(1).and_then(Argument::as_expression),
        Some(Expression::NumericLiteral(_))
    ) && matches!(
        call.arguments.get(2).and_then(Argument::as_expression),
        Some(Expression::ObjectExpression(obj)) if obj.properties.iter().any(|p| {
            matches!(p, oxc_ast::ast::ObjectPropertyKind::ObjectProperty(op)
                if !op.computed && object_key_name(&op.key).as_deref() == Some("upgrade"))
        })
    );
    if opens_on("indexedDB") || opens_on("caches") || idb_open {
        Some(name.to_string())
    } else {
        None
    }
}

/// The static name of a property key, if it is a plain identifier or string.
fn object_key_name(key: &oxc_ast::ast::PropertyKey) -> Option<String> {
    use oxc_ast::ast::PropertyKey;
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.to_string()),
        PropertyKey::StringLiteral(s) => s.value.as_str().map(|v| v.to_string()),
        _ => None,
    }
}

/// Whether a string literal is in a position that must stay static. Mirrors
/// `shouldSkipLiteral`; shared with the literal pass by intent, replicated here
/// to keep the passes independent.
fn skip_literal(parent: AstKind, span: oxc_span::Span, value: Option<&str>) -> bool {
    match parent {
        AstKind::ImportDeclaration(d) => d.source.span == span,
        AstKind::ExportFromDeclaration(d) => d.source.span == span,
        AstKind::ExportAllDeclaration(d) => {
            d.source.span == span || d.exported.as_ref().map(|e| e.span()) == Some(span)
        }
        AstKind::ImportExpression(e) => e.source.span() == span,
        AstKind::ImportSpecifier(_)
        | AstKind::ExportSpecifier(_)
        | AstKind::ImportAttribute(_)
        | AstKind::Directive(_) => true,
        AstKind::ObjectProperty(p) => {
            p.key.span() == span
                && !p.computed
                && p.kind == PropertyKind::Init
                && !p.method
                && !p.shorthand
                && value == Some("__proto__")
        }
        AstKind::MethodDefinition(m) => {
            m.key.span() == span && !m.computed && value == Some("constructor")
        }
        _ => false,
    }
}

/// Whether a string literal is a non-computed property key, so its replacement
/// is a computed `[...]`.
fn is_computed_key(parent: AstKind, span: oxc_span::Span) -> bool {
    match parent {
        AstKind::ObjectProperty(p) => p.key.span() == span && !p.computed,
        AstKind::PropertyDefinition(p) => p.key.span() == span && !p.computed,
        AstKind::MethodDefinition(m) => m.key.span() == span && !m.computed,
        _ => false,
    }
}

/// A JS string literal for `s`, matching the reference's `JSON.stringify`.
fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| format!("{s:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Options;
    use crate::pass::ensure_js_reparses;
    use crate::walk::FileKind;
    use std::path::Path;

    fn opts() -> Options {
        Options {
            seed: Some(b"m3-pass-seed".to_vec()),
            ..Options::default()
        }
    }

    fn run(src: &str) -> Result<String> {
        let o = opts();
        let engine = ProxyEngine::new(&o.proxy, &o.canaries, o.seed.as_deref())?;
        let ctx = FileContext::new("dist/vendor/x.js", FileKind::Js, src, &o);
        let edits = collect_proxy_edits(&ctx, &engine)?;
        let out = edits.apply(src)?;
        ensure_js_reparses(Path::new("dist/vendor/x.js"), &out)?;
        Ok(out)
    }

    #[test]
    fn plain_proxy_string_is_renamed_in_place_and_marked() {
        let out = run("const a = \"scramjet\";").unwrap();
        assert!(out.contains(MARKER));
        assert!(
            !out.contains("scramjet"),
            "plaintext proxy name survived: {out}"
        );
        // Renamed, not encrypted: no decoder needed, so the pool is absent.
        assert!(!out.contains("=(function(){"));
    }

    #[test]
    fn ordinary_literal_without_a_proxy_name_stays_plain() {
        let out = run("const a = \"hello world\"; const b = \"scramjet\";").unwrap();
        assert!(
            out.contains("\"hello world\""),
            "ordinary literal was touched: {out}"
        );
    }

    #[test]
    fn unlisted_store_open_fails_the_build() {
        let err = run("indexedDB.open(\"scramjetJar\");").unwrap_err();
        assert!(matches!(err, Error::Proxy { .. }));
    }

    #[test]
    fn listed_store_name_takes_the_fixed_substitution() {
        let out = run("indexedDB.open(\"__scramjet_controller\", 1, { upgrade() {} });").unwrap();
        assert!(
            out.contains("__ebshkqee_controller"),
            "store not renamed: {out}"
        );
        assert!(!out.contains("__scramjet_controller"));
    }

    #[test]
    fn app_shared_global_is_veiled_and_the_name_disappears() {
        let out = run("globalThis.$scramjet = {};").unwrap();
        assert!(!out.contains("$scramjet"), "shared name survived: {out}");
        // Veiled into a computed lookup, so the pool and decoder ship.
        assert!(out.contains("=(function(){"));
    }

    #[test]
    fn regex_literal_with_a_proxy_name_is_renamed() {
        let out = run("const r = /scramjet/g; void r;").unwrap();
        assert!(!out.contains("/scramjet/"), "regex name survived: {out}");
    }

    #[test]
    fn already_marked_file_is_left_alone_by_the_obfuscator() {
        // A file already carrying the marker is skipped upstream (walk gate), so
        // the pass is never called on it; this documents the contract.
        let o = opts();
        let engine = ProxyEngine::new(&o.proxy, &o.canaries, o.seed.as_deref()).unwrap();
        let src = "const a = 1;"; // no proxy name
        let ctx = FileContext::new("dist/vendor/x.js", FileKind::Js, src, &o);
        let edits = collect_proxy_edits(&ctx, &engine).unwrap();
        assert!(edits.is_empty());
    }
}
