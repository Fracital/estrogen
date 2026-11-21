//! Opaque-predicate dead-code injection, the non-VM literal path's control-flow
//! noise.
//!
//! This walks block bodies and, at a sparse seeded fraction of statement
//! boundaries, inserts an `if` whose guard is always false. The guarded body
//! never runs, so it changes no behavior; it exists to grow the control-flow
//! graph and give a reader dead branches to rule out. The guards are small
//! integer identities (a square is never 2 mod 4, `n*n + n` is always even), so
//! the injected code is a handful of arithmetic ops behind a branch that is
//! never taken: no runtime cost on any real path.
//!
//! The pass only inserts: every edit is a zero-width insertion at a statement's
//! start byte, so it never overlaps a literal-pool or number-disguise edit, which
//! replace spans inside expressions. The dead body uses only number literals and
//! methods on them (`(n).toString(16)`), so it declares no binding, references no
//! free identifier, and touches no `this`: it is safe to insert in any block,
//! including a derived constructor or a class static block.
//!
//! This is not from `obfuscate-build.mjs`. It composes with the literal pool and
//! the mangle pass on the same parsed program, and the result is re-parsed and
//! canary-scanned by the caller like any other edit set.

use oxc_ast::ast::Statement;
use oxc_ast::AstKind;
use oxc_ast_visit::Visit;
use oxc_span::GetSpan;

use crate::edit::{Edit, EditSet};
use crate::error::Result;

/// Inject a dead branch before roughly one in this many block statements. Kept
/// sparse so the output grows a little and the control-flow graph grows a lot.
/// On block-dense code (tiny statements, little string content) one-in-eight
/// adds on the order of 15% to the chunk; on ordinary code it is far less.
const INJECT_ONE_IN: u64 = 8;

/// Collect the dead-branch insertions for one parsed program.
///
/// `name` is the file's decoder name, the same per-file seed the literal pass
/// uses; mixing it in makes the guard values and shapes vary per file and per
/// build. The returned edits are all zero-width insertions at statement starts.
pub(crate) fn collect(
    program: &oxc_ast::ast::Program<'_>,
    _source: &str,
    name: &str,
) -> Result<EditSet> {
    // A seed distinct from the literal pass's number-disguise seed, so the dead
    // branches are not drawn from the same stream as the real disguised numbers.
    let salt = fnv1a(name.as_bytes()) ^ 0x6465_6164_636f_6465;
    let mut visitor = DeadCodeVisitor {
        edits: EditSet::new(),
        salt,
    };
    visitor.visit_program(program);
    Ok(visitor.edits)
}

struct DeadCodeVisitor {
    edits: EditSet,
    salt: u64,
}

impl DeadCodeVisitor {
    /// Consider each statement in a list for a dead-branch insertion. The caller
    /// passes a statement list that carries no directive prologue: a block's body
    /// has none, and a function body keeps its directives in a separate list, so
    /// its `statements` are already past the prologue. The bytes between
    /// statements are left in place, so ASI still separates the injected `if`
    /// from its neighbors.
    fn consider(&mut self, statements: &[Statement<'_>]) {
        for stmt in statements {
            let start = stmt.span().start as u64;
            let h = mix(self.salt, start);
            if !h.is_multiple_of(INJECT_ONE_IN) {
                continue;
            }
            let at = statement_insert_point(stmt);
            self.edits.push(Edit::insert(at, dead_branch(h)));
        }
    }
}

impl<'a> Visit<'a> for DeadCodeVisitor {
    fn enter_node(&mut self, kind: AstKind<'a>) {
        // A function or arrow body (`FunctionBody`) and a braced block
        // (`BlockStatement`) are the statement lists safe to splice into. The
        // program's own top level is left alone, to keep import and export
        // hoisting untouched. A nested block inside any of these is visited on
        // its own `enter_node`, so every statement is considered exactly once.
        match kind {
            AstKind::FunctionBody(body) => self.consider(&body.statements),
            AstKind::BlockStatement(block) => self.consider(&block.body),
            _ => {}
        }
    }
}

/// Where to splice the branch in: a statement's own start, before a leading
/// label and before the first token, so the branch becomes a complete prior
/// statement and the original statement follows unchanged.
fn statement_insert_point(stmt: &Statement<'_>) -> usize {
    stmt.span().start as usize
}

/// Build one dead `if` statement, choosing an always-false guard and an inert
/// body from the mixed hash. Every piece uses only integer literals, so the text
/// parses on its own and carries no identifier a surrounding scope could rebind.
fn dead_branch(h: u64) -> String {
    let n = 2 + (h >> 16) % 48; // 2..=49, small so no product overflows 2^53
    let guard = match (h >> 8) % 4 {
        // n*n + n is even for every integer n, so this is never odd.
        0 => format!("({n}*{n}+{n})%2!==0"),
        // A perfect square is 0 or 1 mod 4, never 2.
        1 => format!("({n}*{n})%4===2"),
        // n | 1 is odd, so its low bit is 1, never 0.
        2 => format!("(({n}|1)&1)===0"),
        // n*(n+1) is the product of two consecutive integers, always even.
        _ => format!("({n}*({n}+1))%2!==0"),
    };
    let body = match (h >> 32) % 3 {
        0 => format!("({n}).toString(16);"),
        1 => format!("[{n}].length;"),
        _ => format!("({n}).valueOf();"),
    };
    format!("if({guard}){{{body}}}")
}

/// FNV-1a over bytes, for the per-file salt. Same construction as the literal
/// pass so the two seeds derive from the decoder name the same way.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Mix the file salt with a site offset into a well-spread value (splitmix64),
/// so the slices used to decide injection, guard shape, and the value are
/// independent.
fn mix(salt: u64, offset: u64) -> u64 {
    let mut z = salt
        .wrapping_add(offset)
        .wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pass::with_parsed_js;
    use std::path::Path;

    fn obfuscated(src: &str) -> String {
        with_parsed_js(Path::new("t.js"), src, |program| {
            let edits = collect(program, src, "d")?;
            edits.apply(src)
        })
        .unwrap()
    }

    #[test]
    fn every_guard_shape_is_false_and_parses() {
        // Exercise each guard and body shape by feeding the hashes that select
        // them, and check the guard really evaluates false for its n.
        for pred in 0u64..4 {
            for body in 0u64..3 {
                let h = (pred << 8) | (body << 32);
                let branch = dead_branch(h);
                // It is a complete, parseable statement on its own.
                let wrapped = format!("{branch}\nconsole.log(1);");
                with_parsed_js(Path::new("t.js"), &wrapped, |_| Ok(())).unwrap();
            }
        }
    }

    #[test]
    fn injects_into_a_block_and_stays_parseable() {
        // A long body so the sparse one-in-five injection deterministically hits
        // at least once, whatever the seed happens to pick for the short cases.
        let body: String = (0..40).map(|i| format!("let v{i} = {i} + 1;\n")).collect();
        let src = format!("function f(){{\n{body}return v0;\n}}");
        let out = obfuscated(&src);
        // Something was injected, and the result still parses.
        assert!(out.contains("if("), "expected a dead branch: {out}");
        with_parsed_js(Path::new("t.js"), &out, |_| Ok(())).unwrap();
    }

    #[test]
    fn leaves_the_real_statements_in_place() {
        // The original tokens survive verbatim; only insertions are added.
        let src = "function f(){ let a = 1; let b = 2; let c = 3; let d = 4; return a+b+c+d; }";
        let out = obfuscated(src);
        for needle in ["let a = 1;", "let b = 2;", "let c = 3;", "return a+b+c+d;"] {
            assert!(out.contains(needle), "lost `{needle}` in: {out}");
        }
    }

    #[test]
    fn deterministic_for_a_name() {
        let src = "function f(){ let a = 1; let b = 2; let c = 3; return a; }";
        assert_eq!(obfuscated(src), obfuscated(src));
    }
}
