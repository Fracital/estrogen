//! Proxy and store name pass.
//!
//! Spec: `obfuscate-build.mjs`. It renames the proxy engine names (scramjet,
//! stomp and their cased forms) and guards the store names, rewriting them in
//! string literals, regex literals and vendored markup to the per-build
//! substitutes in the names map. The store-name guard refuses a store name that
//! is not in the allow-list, and a proxy name that would fall across a regex
//! escape is a hard failure.
//!
//! Identity milestone: this returns no edits. Collecting the name ranges and
//! applying the map is the engine work. The names map it will read is
//! [`crate::options::Options::names`].

use crate::edit::EditSet;
use crate::error::Result;
use crate::pass::{FileContext, Pass};

/// The proxy and store name pass.
#[derive(Debug, Default)]
pub struct NamesPass;

impl Pass for NamesPass {
    fn name(&self) -> &'static str {
        "names"
    }

    fn collect_edits(&self, _ctx: &FileContext) -> Result<EditSet> {
        // Identity: no edits yet. The engine will walk string and regex
        // literals and vendored markup, map each name, and guard the store
        // names against the allow-list.
        Ok(EditSet::new())
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
        let src = "const engine = 'scramjet';";
        let ctx = FileContext::new("app.js", FileKind::Js, src, &opts);
        let edits = NamesPass.collect_edits(&ctx).unwrap();
        assert!(edits.is_empty());
    }
}
