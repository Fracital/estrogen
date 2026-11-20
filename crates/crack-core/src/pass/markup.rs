//! HTML shell and SVG cloak pass.
//!
//! Spec: the `obfuscate-build.mjs` header. For an HTML shell it neutralizes a
//! canary `<title>` or description `<meta>` and encrypts the canary-bearing
//! inline `<script>` the same way as a JS chunk. For an SVG cloak served as
//! `image/svg+xml` it does the same, with two XML twists: the inline `<script>`
//! body is XML, so entities are decoded (`&amp;` to `&`) and any CDATA section
//! is taken literally before obfuscating, which recovers the exact JS the
//! browser runs; and the obfuscated body, whose decoder uses `<`, `<<` and `&`,
//! is re-emitted inside `<![CDATA[ ... ]]>`, with any `]]>` split across CDATA
//! sections.
//!
//! Identity milestone: this returns no edits. The entity and CDATA handling, the
//! title and meta neutralizing, and the inline-script encryption are the engine
//! work.

use crate::edit::EditSet;
use crate::error::Result;
use crate::pass::{FileContext, Pass};

/// The HTML and SVG markup pass.
#[derive(Debug, Default)]
pub struct MarkupPass;

impl Pass for MarkupPass {
    fn name(&self) -> &'static str {
        "markup"
    }

    fn collect_edits(&self, _ctx: &FileContext) -> Result<EditSet> {
        // Identity: no edits yet. The engine will neutralize canary title and
        // meta, find each inline <script>, decode XML entities and CDATA for the
        // SVG case, and encrypt the recovered JS.
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
        let src = "<html><title>Fracital</title></html>";
        let ctx = FileContext::new("index.html", FileKind::Html, src, &opts);
        let edits = MarkupPass.collect_edits(&ctx).unwrap();
        assert!(edits.is_empty());
    }
}
