//! In-memory obfuscation, for callers without a directory on disk.
//!
//! The [`Obfuscator`](crate::Obfuscator) walks a built directory and writes each
//! file back in place. The web demo has no such directory: a browser hands over
//! one source string and wants the obfuscated string back. [`obfuscate_js`] runs
//! the same literal-pool pass the directory walk runs over an app chunk, over a
//! string, and returns the result. No file is read or written, and no thread
//! pool is built, so it compiles and runs under `wasm32-unknown-unknown`.
//!
//! This is the app-chunk transform only: string and template literals move into
//! the encrypted pool, and the decoder is prepended. It does not take the proxy
//! vendor path or the markup path, which both need a built tree to make sense.

use std::path::Path;

use crate::edit::EditSet;
use crate::error::{Error, Result};
use crate::options::Options;
use crate::pass::literals::LiteralsPass;
use crate::pass::{ensure_js_reparses, FileContext, Pass};
use crate::walk::FileKind;

/// What an in-memory obfuscation produced.
#[derive(Debug, Clone)]
pub struct Obfuscated {
    /// The obfuscated source. When nothing was eligible this is the input
    /// unchanged.
    pub code: String,
    /// How many byte-range edits were spliced in. Zero means the input had no
    /// string or template literal to pool, so the output equals the input.
    pub edits: usize,
}

/// Obfuscate one JavaScript source string in memory.
///
/// The source is parsed as an ES module, run through the literal-pool pass, and
/// the edits are spliced in. The output is re-parsed and fails closed if it does
/// not parse, and every configured canary is checked against the output, the
/// same guarantees the file path gives. A source with no poolable literal comes
/// back unchanged with `edits == 0`.
///
/// This raises the cost of reading the code. It is not secrecy: the pool key
/// ships in the output, right next to the decoder, so anyone can run it back.
pub fn obfuscate_js(source: &str, options: &Options) -> Result<Obfuscated> {
    let path = Path::new("input.js");
    let ctx = FileContext::new(path, FileKind::Js, source, options);
    let edits: EditSet = LiteralsPass.collect_edits(&ctx)?;

    if edits.is_empty() {
        return Ok(Obfuscated {
            code: source.to_string(),
            edits: 0,
        });
    }

    let count = edits.len();
    let rewritten = edits.apply(source)?;
    ensure_js_reparses(path, &rewritten)?;

    for canary in &options.canaries {
        if !canary.value.is_empty() && rewritten.contains(&canary.value) {
            return Err(Error::Canary {
                path: path.to_path_buf(),
                literal: canary.value.clone(),
            });
        }
    }

    Ok(Obfuscated {
        code: rewritten,
        edits: count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pools_a_string_literal_and_reparses() {
        let opts = Options {
            seed: Some(b"demo-seed".to_vec()),
            ..Options::default()
        };
        let out = obfuscate_js("const greeting = 'hello world';", &opts).unwrap();
        assert!(out.edits > 0);
        // The plaintext string is gone and the marker decoder is present.
        assert!(!out.code.contains("hello world"));
        assert!(out.code.contains(crate::format::MARKER));
    }

    #[test]
    fn source_with_no_literal_is_unchanged() {
        let opts = Options::default();
        let out = obfuscate_js("const a = 1 + 2;", &opts).unwrap();
        assert_eq!(out.edits, 0);
        assert_eq!(out.code, "const a = 1 + 2;");
    }

    #[test]
    fn deterministic_under_a_seed() {
        let opts = Options {
            seed: Some(b"fixed".to_vec()),
            ..Options::default()
        };
        let src = "export const x = 'alpha'; export const y = 'beta';";
        let a = obfuscate_js(src, &opts).unwrap();
        let b = obfuscate_js(src, &opts).unwrap();
        assert_eq!(a.code, b.code);
    }

    #[test]
    fn a_canary_in_the_output_fails_closed() {
        // A canary that the pass cannot remove (it appears as an identifier, not
        // a string) must make the obfuscation fail rather than ship it.
        let opts = Options {
            seed: Some(b"fixed".to_vec()),
            canaries: vec![crate::options::Canary {
                value: "secretIdent".to_string(),
                reason: String::new(),
            }],
            ..Options::default()
        };
        let err = obfuscate_js("const secretIdent = 'x';", &opts).unwrap_err();
        assert!(matches!(err, Error::Canary { .. }));
    }
}
