//! Per-file key material and canary-free sampling.
//!
//! Every file estrogen encrypts gets its own ChaCha20 key, a fresh nonce per pool
//! entry, and a decoder name. [`FileKeys`] is that source of randomness. In a
//! production run it seeds from the OS; under [`crate::options::Options::seed`]
//! it seeds deterministically from the file's bytes, so a run on one input gives
//! the same output twice and the parity harness can diff it.
//!
//! Random output must never spell a canary by chance. The pool ships as base64
//! of random ciphertext, so a short canary made of base64-alphabet letters turns
//! up on some seeds with nothing to do with the source. Every base64 string estrogen
//! ships (the key and each pool entry) is rejection-sampled against the canaries,
//! case-insensitively, so a build is clean whatever its seed. This mirrors
//! `canaryFreeBase64` in `obfuscate-build.mjs`.
//!
//! The key ships in the file. This raises the cost of reading a build; it is not
//! secrecy. Nothing here is secret.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use sha2::{Digest, Sha256};

use crate::chacha::Keystream;
use crate::error::{Error, Result};
use crate::options::Options;

/// How many redraws a base64 sample gets before estrogen gives up. The canary list
/// is short and the alphabet is 64 wide, so a clean sample lands in the first
/// few tries; this bound only catches a pathological list, matching the
/// reference's own cap.
const MAX_SAMPLE_ATTEMPTS: usize = 1000;

/// Per-file random source behind the key, the nonces, and the decoder name.
pub struct FileKeys {
    stream: Keystream,
    /// Canaries, lowercased, that can occur in the base64 alphabet. Only these
    /// can appear in random base64, so only these are worth checking, and the
    /// rejection loop always terminates quickly.
    b64_canaries: Vec<String>,
}

impl FileKeys {
    /// Build the key source for one file.
    ///
    /// `seed_material` is the file's bytes. With [`Options::seed`] set, the
    /// per-file seed is `sha256(global_seed || 0x1f || seed_material)`, so the
    /// stream is reproducible and independent of where the file lives. Without a
    /// seed, the stream starts from 32 bytes of OS entropy and the material is
    /// ignored.
    pub fn new(options: &Options, seed_material: &[u8]) -> Result<Self> {
        let seed = match &options.seed {
            Some(global) => {
                let mut h = Sha256::new();
                h.update(global);
                h.update([0x1f]);
                h.update(seed_material);
                let digest = h.finalize();
                let mut s = [0u8; 32];
                s.copy_from_slice(&digest);
                s
            }
            None => {
                let mut s = [0u8; 32];
                getrandom::getrandom(&mut s).map_err(|e| Error::Config {
                    path: std::path::PathBuf::from("<entropy>"),
                    message: format!("could not read OS randomness: {e}"),
                })?;
                s
            }
        };
        let b64_canaries = options
            .canaries
            .iter()
            .map(|c| c.value.to_ascii_lowercase())
            .filter(|c| !c.is_empty() && c.bytes().all(is_base64_alpha))
            .collect();
        Ok(FileKeys {
            stream: Keystream::new(&seed),
            b64_canaries,
        })
    }

    /// Whether a base64 string contains a canary, case-insensitively. Only
    /// canaries expressible in the alphabet are checked.
    pub fn spells_b64_canary(&self, text: &str) -> bool {
        if self.b64_canaries.is_empty() {
            return false;
        }
        let lower = text.to_ascii_lowercase();
        self.b64_canaries.iter().any(|c| lower.contains(c))
    }

    /// Draw the per-file key: 32 random bytes whose base64 is canary-free. Both
    /// the raw key (for encryption) and its base64 (for the shipped decoder) are
    /// returned.
    pub fn draw_key(&mut self) -> Result<([u8; 32], String)> {
        for _ in 0..MAX_SAMPLE_ATTEMPTS {
            let mut key = [0u8; 32];
            self.stream.fill(&mut key);
            let b64 = STANDARD.encode(key);
            if !self.spells_b64_canary(&b64) {
                return Ok((key, b64));
            }
        }
        Err(Error::Engine("canary-free key sampling did not converge"))
    }

    /// Draw one 12-byte nonce. The canary check is on the finished pool entry
    /// (nonce and ciphertext together), so the caller redraws the nonce and
    /// re-encrypts; this just supplies the bytes.
    pub fn draw_nonce(&mut self) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        self.stream.fill(&mut nonce);
        nonce
    }

    /// Seal one plaintext into a pool entry: `base64(nonce || ChaCha20(key,
    /// nonce, plaintext))`, redrawing the nonce until the entry is canary-free.
    /// This is the per-entry half of the reference's `canaryFreeBase64`.
    pub fn seal_entry(&mut self, key: &[u8; 32], plaintext: &[u8]) -> Result<String> {
        for _ in 0..MAX_SAMPLE_ATTEMPTS {
            let nonce = self.draw_nonce();
            let ct = crate::chacha::chacha20_xor(key, &nonce, plaintext);
            let mut buf = Vec::with_capacity(nonce.len() + ct.len());
            buf.extend_from_slice(&nonce);
            buf.extend_from_slice(&ct);
            let b64 = STANDARD.encode(&buf);
            if !self.spells_b64_canary(&b64) {
                return Ok(b64);
            }
        }
        Err(Error::Engine(
            "canary-free pool entry sampling did not converge",
        ))
    }

    /// The per-file decoder name, `__frx$` followed by six hex digits, matching
    /// `obfuscate-build.mjs`. The suffix is three bytes of the stream.
    pub fn decoder_name(&mut self) -> String {
        let mut suffix = [0u8; 3];
        self.stream.fill(&mut suffix);
        format!("__frx${:02x}{:02x}{:02x}", suffix[0], suffix[1], suffix[2])
    }
}

/// Whether a byte is in the standard base64 alphabet (lowercased input).
fn is_base64_alpha(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'+' || b == b'/'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Canary;

    fn opts_with_seed(seed: &[u8], canaries: &[&str]) -> Options {
        Options {
            seed: Some(seed.to_vec()),
            canaries: canaries
                .iter()
                .map(|v| Canary {
                    value: (*v).to_string(),
                    reason: String::new(),
                })
                .collect(),
            ..Options::default()
        }
    }

    #[test]
    fn seeded_keys_are_reproducible() {
        let opts = opts_with_seed(b"seed-1", &[]);
        let mut a = FileKeys::new(&opts, b"file body").unwrap();
        let mut b = FileKeys::new(&opts, b"file body").unwrap();
        assert_eq!(a.draw_key().unwrap(), b.draw_key().unwrap());
        assert_eq!(a.draw_nonce(), b.draw_nonce());
        assert_eq!(a.decoder_name(), b.decoder_name());
    }

    #[test]
    fn different_material_diverges() {
        let opts = opts_with_seed(b"seed-1", &[]);
        let mut a = FileKeys::new(&opts, b"file one").unwrap();
        let mut b = FileKeys::new(&opts, b"file two").unwrap();
        assert_ne!(a.draw_key().unwrap().0, b.draw_key().unwrap().0);
    }

    #[test]
    fn decoder_name_shape() {
        let opts = opts_with_seed(b"s", &[]);
        let name = FileKeys::new(&opts, b"x").unwrap().decoder_name();
        assert!(name.starts_with("__frx$"));
        assert_eq!(name.len(), "__frx$".len() + 6);
        assert!(name["__frx$".len()..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn only_base64_expressible_canaries_are_kept() {
        // "epoxy" is all base64 letters; "Fra$tal!" has non-alphabet chars.
        let opts = opts_with_seed(b"s", &["epoxy", "Fra$tal!", "ABC"]);
        let keys = FileKeys::new(&opts, b"x").unwrap();
        assert!(keys.spells_b64_canary("zzepoxyzz"));
        assert!(keys.spells_b64_canary("qqABCqq")); // case-insensitive
        assert!(!keys.spells_b64_canary("Fra")); // the non-alphabet one was dropped
    }

    #[test]
    fn drawn_key_base64_never_spells_a_kept_canary() {
        let opts = opts_with_seed(b"canary-seed", &["aaa"]);
        let mut keys = FileKeys::new(&opts, b"body").unwrap();
        let (_, b64) = keys.draw_key().unwrap();
        assert!(!b64.to_ascii_lowercase().contains("aaa"));
    }
}
