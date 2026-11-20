//! Wasm name and base64 pass.
//!
//! Spec: `obfuscate-build.mjs` and SystemYA's `prepare-stomp.mjs` /
//! `prepare-powermouse.mjs`. The proxy transports embed a wasm core module whose
//! name ranges have to match the renamed proxy names, and the module may be
//! carried as base64. This pass rewrites the wasm name ranges and handles the
//! base64 so the transport still loads. A wasm literal in mixed base64 alphabets
//! is a hard failure.
//!
//! Identity milestone: this returns no edits. Parsing the wasm name section,
//! rewriting the ranges, and re-encoding the base64 is the engine work.

use crate::edit::EditSet;
use crate::error::Result;
use crate::pass::{FileContext, Pass};

/// The wasm name and base64 pass.
#[derive(Debug, Default)]
pub struct WasmPass;

impl Pass for WasmPass {
    fn name(&self) -> &'static str {
        "wasm"
    }

    fn collect_edits(&self, _ctx: &FileContext) -> Result<EditSet> {
        // Identity: no edits yet. The engine will locate the wasm name ranges
        // (and the base64 that may wrap them) and rewrite them to agree with the
        // renamed proxy names.
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
        let src = "const w = 'AGFzbQEAAAA=';";
        let ctx = FileContext::new("transport.js", FileKind::Js, src, &opts);
        let edits = WasmPass.collect_edits(&ctx).unwrap();
        assert!(edits.is_empty());
    }
}
