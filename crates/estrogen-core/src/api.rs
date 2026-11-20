//! In-memory obfuscation, for callers without a directory on disk.
//!
//! The [`Obfuscator`](crate::Obfuscator) walks a built directory and writes each
//! file back in place. The web demo has no such directory: a browser hands over
//! one source string and wants the obfuscated string back. [`obfuscate_js`] runs
//! the same app-chunk passes as the directory walk and returns the result. It
//! uses no filesystem or thread pool, so it runs on `wasm32-unknown-unknown`.
//!
//! This is the app-chunk transform only: it pools string and template literals
//! and shortens local bindings. It does not run the vendor or markup passes.

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
    /// The obfuscated source, or the original if nothing changed.
    pub code: String,
    /// Number of byte-range edits, including generated code and the watermark.
    pub edits: usize,
    /// The V3 source map JSON tracing the output back to `source`, when the
    /// result came from the edit-and-splice path. A VM chunk is a whole-file
    /// replacement with no edit list, so it carries no map, and unchanged input
    /// carries none either.
    pub map: Option<String>,
}

/// Obfuscate one JavaScript source string in memory.
///
/// The source is parsed as an ES module. With `options.vm` set (the default),
/// an app chunk the VM subset covers is compiled to bytecode, a whole-file
/// replacement with no source map; anything outside the subset falls back to
/// the edit-and-splice path, which pools literals, shortens locals, and carries
/// a V3 map. The result must parse and contain no configured canaries. Already
/// watermarked input and input with no edits come back unchanged with
/// `edits == 0`.
///
/// This raises the cost of reading the code. It is not secrecy: the pool key
/// ships in the output, right next to the decoder, so anyone can run it back.
pub fn obfuscate_js(source: &str, options: &Options) -> Result<Obfuscated> {
    let path = Path::new("input.js");

    // The VM path: a whole-file replacement, the default for a chunk it covers.
    if options.vm {
        if let Some(replacement) = crate::vm::try_compile_chunk(source, options)? {
            check_canaries(path, &replacement, options)?;
            return Ok(Obfuscated {
                code: replacement,
                edits: 1,
                map: None,
            });
        }
    }

    // The edit-and-splice path: the fallback, and what runs with the VM off.
    let ctx = FileContext::new(path, FileKind::Js, source, options);
    let edits: EditSet = LiteralsPass.collect_edits(&ctx)?;

    if edits.is_empty() {
        return Ok(Obfuscated {
            code: source.to_string(),
            edits: 0,
            map: None,
        });
    }

    let count = edits.len();
    let rewritten = edits.apply(source)?;
    ensure_js_reparses(path, &rewritten)?;
    check_canaries(path, &rewritten, options)?;

    // Collapse to a single line when minify is on (the default), matching the
    // directory walk. The reprint does not line up with the source, so a
    // minified chunk carries no map. minify_js re-parses its own output; if the
    // minified text would reintroduce a canary, keep the edit-and-splice output.
    if options.minify {
        if let Some(minified) = crate::minify::minify_js(path, &rewritten) {
            if check_canaries(path, &minified, options).is_ok() {
                return Ok(Obfuscated {
                    code: minified,
                    edits: count,
                    map: None,
                });
            }
        }
    }

    let map = crate::sourcemap::build_v3("input.js", source, &edits.sorted());
    Ok(Obfuscated {
        code: rewritten,
        edits: count,
        map: Some(map),
    })
}

/// Fail closed when a configured canary survives in the output.
fn check_canaries(path: &Path, output: &str, options: &Options) -> Result<()> {
    for canary in &options.canaries {
        if !canary.value.is_empty() && output.contains(&canary.value) {
            return Err(Error::Canary {
                path: path.to_path_buf(),
                literal: canary.value.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The literal-path tests pin `vm: false`, since the VM default would compile
    // these simple chunks to bytecode instead of pooling and splicing.
    fn literal_opts(seed: &[u8]) -> Options {
        // minify off so these tests see the edit-and-splice output and its map,
        // not the single-line reprint.
        Options {
            seed: Some(seed.to_vec()),
            vm: false,
            minify: false,
            ..Options::default()
        }
    }

    #[test]
    fn pools_a_string_literal_and_reparses() {
        let out = obfuscate_js(
            "const greeting = 'hello world';",
            &literal_opts(b"demo-seed"),
        )
        .unwrap();
        assert!(out.edits > 0);
        // The plaintext string is gone and the marker decoder is present.
        assert!(!out.code.contains("hello world"));
        assert!(out.code.contains(crate::format::MARKER));
        // The edit-and-splice path carries a source map.
        assert!(out.map.is_some());
    }

    #[test]
    fn source_with_no_literal_is_unchanged() {
        let out = obfuscate_js("const a = 1 + 2;", &literal_opts(b"x")).unwrap();
        assert_eq!(out.edits, 0);
        assert_eq!(out.code, "const a = 1 + 2;");
        assert!(out.map.is_none());
    }

    #[test]
    fn deterministic_under_a_seed() {
        let opts = literal_opts(b"fixed");
        let src = "export const x = 'alpha'; export const y = 'beta';";
        let a = obfuscate_js(src, &opts).unwrap();
        let b = obfuscate_js(src, &opts).unwrap();
        assert_eq!(a.code, b.code);
    }

    #[test]
    fn a_canary_in_the_output_fails_closed() {
        // A canary the literal pass cannot remove (it appears as an identifier,
        // not a string) must make the obfuscation fail rather than ship it.
        let mut opts = literal_opts(b"fixed");
        opts.canaries = vec![crate::options::Canary {
            value: "secretIdent".to_string(),
            reason: String::new(),
        }];
        let err = obfuscate_js("const secretIdent = 'x';", &opts).unwrap_err();
        assert!(matches!(err, Error::Canary { .. }));
    }

    #[test]
    fn the_vm_compiles_a_simple_chunk_with_no_map() {
        // The default: a chunk the VM subset covers compiles to bytecode, a
        // whole-file replacement that carries no edit-list source map.
        let opts = Options {
            seed: Some(b"vm".to_vec()),
            ..Options::default()
        };
        let out = obfuscate_js("const a = 1 + 2; const b = a * 10; b - 1;\n", &opts).unwrap();
        assert!(out.code.contains(crate::format::MARKER));
        assert!(
            out.code.contains("JSON.parse("),
            "not a vm chunk: {}",
            out.code
        );
        assert!(out.map.is_none());
    }

    #[test]
    fn a_chunk_off_the_vm_subset_falls_back_with_a_map() {
        // An import is outside the VM subset, so the chunk falls back to the
        // edit-and-splice path, which pools the string and (with minify off)
        // carries a map.
        let opts = Options {
            seed: Some(b"vm".to_vec()),
            minify: false,
            ..Options::default()
        };
        let out = obfuscate_js("import x from 'm'; const s = 'hello world'; x(s);", &opts).unwrap();
        assert!(!out.code.contains("JSON.parse("), "should have fallen back");
        assert!(!out.code.contains("hello world"));
        assert!(out.map.is_some());
    }
}
