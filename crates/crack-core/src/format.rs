//! Pool and decoder format.
//!
//! A file crack obfuscates carries, at its head, a small ChaCha20 decoder, an
//! encrypted constant pool, and the per-file key. Each literal in the body
//! becomes a `NAME(idx)` call that decodes one pool entry on first use and
//! memoizes it. This module owns that on-disk shape behind [`FORMAT_VERSION`],
//! so the Rust encoder and the emitted JS decoder stay one contract.
//!
//! The spec is Fracital's `scripts/obfuscate-build.mjs`: the cipher is ChaCha20
//! (RFC 8439), the shipped decoder is a compact vanilla ChaCha20 symmetric with
//! the encoder, and `obfuscate-build.test.mjs` asserts that the decoder decodes
//! the encoder's pool byte for byte. crack emits a decoder that decodes crack's
//! own pool and has to pass that same symmetry check.
//!
//! The key ships in the file. This raises the cost of reading a build; it is not
//! secrecy, and anyone with devtools can run the code. Nothing here is secret.
//!
//! The engine is not built yet. The types below name the pieces of the format;
//! the functions that encrypt a pool and emit a decoder are stubs that return
//! [`Error::Engine`], cited against the reference above.

use crate::error::{Error, Result};

/// Version of the pool and decoder format.
///
/// This is bumped when the on-disk pool shape or the emitted decoder change in
/// a way that breaks the other side. A file written by one version is decoded
/// by the decoder of that same version, which ships in the file, so the number
/// is a build-time contract between crack's encoder and its decoder emitter, not
/// a runtime negotiation.
pub const FORMAT_VERSION: u32 = 1;

/// Marker string crack writes into a file it has obfuscated.
///
/// A file that already contains it has been processed and is skipped, so a
/// second run does not encrypt the decoder the first run added. This is the
/// `MARKER` constant from `obfuscate-build.mjs`.
pub const MARKER: &str = "__FRX_OBF__";

/// An encrypted constant pool for one file.
///
/// `key` is the 32-byte ChaCha20 key that ships with the file. `entries` are the
/// ciphertext blocks, one per unique source literal, in the order the body's
/// `NAME(idx)` calls index them. The in-file encoding of these fields is what
/// [`FORMAT_VERSION`] governs.
#[derive(Debug, Clone, Default)]
pub struct Pool {
    /// Per-file ChaCha20 key, shipped in the file.
    pub key: Vec<u8>,
    /// Ciphertext for each unique literal, indexed by the body's decode calls.
    pub entries: Vec<Vec<u8>>,
}

impl Pool {
    /// Build an encrypted pool from a file's unique literals.
    ///
    /// Not built yet: this is where ChaCha20 encryption and the pool layout
    /// live. It returns [`Error::Engine`] until the engine lands.
    pub fn encrypt(_literals: &[String], _key: &[u8]) -> Result<Pool> {
        Err(Error::Engine("pool encryption (format.rs)"))
    }

    /// Emit the JS source of the decoder plus the encoded pool and key, to be
    /// prepended to the file.
    ///
    /// Not built yet: this emits the compact ChaCha20 decoder that is symmetric
    /// with [`Pool::encrypt`]. It returns [`Error::Engine`] until the engine
    /// lands.
    pub fn emit_decoder(&self, _global_name: &str) -> Result<String> {
        Err(Error::Engine("decoder emitter (format.rs)"))
    }
}
