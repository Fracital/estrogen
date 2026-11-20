//! WebAssembly binding for estrogen.
//!
//! [`obfuscate`] runs the app-chunk passes over a JavaScript source string and
//! returns the new source and edit count. It is the engine behind the web demo:
//! the same Rust code the CLI runs
//! over a built file, compiled to `wasm32-unknown-unknown` and called from the
//! browser. Nothing is read from or written to disk.
//!
//! This raises the cost of reading the code. It is not secrecy. The pool key
//! ships in the returned source, so the output runs back to the input in any JS
//! engine, devtools included.

use estrogen_core::options::Options;
use serde::Serialize;
use wasm_bindgen::prelude::*;

/// The result handed back to JavaScript, serialized as a plain object
/// `{ code, edits }`.
#[derive(Serialize)]
struct Output {
    /// The obfuscated source, or the original if nothing changed.
    code: String,
    /// Number of byte-range edits. Zero means no changes.
    edits: usize,
}

/// Obfuscate one JavaScript source string.
///
/// `source` is the code to obfuscate. `seed`, when a non-empty string, makes the
/// output deterministic: the same source and seed give byte-identical output, so
/// the demo's seed field repeats a build. An empty or absent seed draws fresh
/// per-file keys from the browser's crypto.
///
/// Returns a JSON string `{"code":...,"edits":...}` for the caller to parse. A
/// parse error, a pass failure, or a canary surviving in the output rejects with
/// the engine's error string; the demo shows it and keeps the previous output.
#[wasm_bindgen]
pub fn obfuscate(source: &str, seed: Option<String>) -> Result<String, JsValue> {
    let mut options = Options::default();
    if let Some(seed) = seed {
        if !seed.is_empty() {
            options.seed = Some(seed.into_bytes());
        }
    }

    let out = estrogen_core::obfuscate_js(source, &options)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;

    let payload = Output {
        code: out.code,
        edits: out.edits,
    };
    serde_json::to_string(&payload).map_err(|e| JsValue::from_str(&e.to_string()))
}

/// estrogen's version, so the demo can print the engine it is running.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
