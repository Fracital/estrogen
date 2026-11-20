//! String and template literal pass.
//!
//! Spec: `obfuscate-build.mjs`. It collects every string literal in a safe value
//! or key position and every quasi of a non-tagged template literal, encrypts
//! each unique string into a per-file ChaCha20 pool, and replaces the literal
//! with a memoized `NAME(idx)` decode call. The positions that must stay static
//! are listed in that file's header: directive prologues, import and export
//! specifiers, import attributes, a `constructor` method key, a `__proto__`
//! proto key, and tagged template quasis.
//!
//! Identity milestone: this parses the file (failing closed on a parse error),
//! walks it to prove the AST is reachable, and returns no edits. Collecting the
//! literal ranges and building the pool is the engine work.

use crate::edit::EditSet;
use crate::error::Result;
use crate::pass::{with_parsed_js, FileContext, Pass, StringLiteralCounter};
use oxc_ast_visit::Visit;

/// The literal-pool pass.
#[derive(Debug, Default)]
pub struct LiteralsPass;

impl Pass for LiteralsPass {
    fn name(&self) -> &'static str {
        "literals"
    }

    fn collect_edits(&self, ctx: &FileContext) -> Result<EditSet> {
        // Parse to validate (fail closed) and walk to confirm the AST is
        // reachable. The engine will collect literal ranges here instead of
        // only counting.
        with_parsed_js(&ctx.path, ctx.source, |program| {
            let mut counter = StringLiteralCounter::default();
            counter.visit_program(program);
            // Identity: no edits yet. The count is a reachability check only.
            let _ = counter.count;
            Ok(EditSet::new())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Options;
    use crate::walk::FileKind;

    #[test]
    fn identity_returns_no_edits() {
        let opts = Options::default();
        let src = "const a = 'hello'; const b = `t${a}`;";
        let ctx = FileContext::new("app.js", FileKind::Js, src, &opts);
        let edits = LiteralsPass.collect_edits(&ctx).unwrap();
        assert!(edits.is_empty());
        // Applying an empty set leaves the file byte-for-byte unchanged.
        assert_eq!(edits.apply(src).unwrap(), src);
    }

    #[test]
    fn parse_error_fails_closed() {
        let opts = Options::default();
        let src = "function (";
        let ctx = FileContext::new("bad.js", FileKind::Js, src, &opts);
        assert!(LiteralsPass.collect_edits(&ctx).is_err());
    }
}
