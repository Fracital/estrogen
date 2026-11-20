//! Proxy name engine: the per-build renaming the proxy vendor path uses.
//!
//! The proxy transport code is not string-encrypted wholesale, because a decode
//! on every literal of a multi-megabyte transport bundle buys no secrecy and
//! costs the hot path. Instead the proxy names are renamed by a deterministic,
//! same-length substitution applied identically to identifiers, whitespace-free
//! strings and the embedded wasm, so every reference inside the proxy set still
//! agrees and the wasm section sizes stay valid. Strings that carry a proxy name
//! and cannot be renamed (app-shared, legal notices, prose) are encrypted
//! instead, so the name never ships in plaintext.
//!
//! The substitution is keyed by a per-build 32-byte key, so it is not
//! reproducible outside the build. This raises the cost of reading a build; it
//! is not secrecy. Nothing here is secret.
//!
//! Spec: `obfuscate-build.mjs` (`proxyReplacement`, `mapProxyText`,
//! `mapSharedProtected`, `mapStoreName`, `mapProxyRegex`, `unlistedStoreOpen`,
//! `isProxyVendor`, `hasProxyName`). The Fracital-specific vocabulary lives in
//! [`crate::options::ProxyConfig`]; this engine compiles it once per run.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use hmac::{Hmac, Mac};
use regex::Regex;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::options::{Canary, ProxyConfig};

type HmacSha256 = Hmac<Sha256>;

/// How many redraws a same-length rename gets before crack gives up. Each redraw
/// changes every letter, so a canary-free result lands at once in practice.
const MAX_RENAME_ATTEMPTS: usize = 1000;

/// A byte that may appear in a JS identifier, `[A-Za-z0-9_$]`.
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The compiled proxy renaming for one run. Built once and shared by every
/// proxy-path file and by the embedded wasm mapping.
pub struct ProxyEngine {
    proxy_re: Regex,
    shared_names: BTreeMap<String, Vec<String>>,
    shared_strings: HashSet<String>,
    store_tokens: BTreeMap<String, String>,
    store_names: HashSet<String>,
    vendor: GlobSet,
    legal_re: Regex,
    key: [u8; 32],
    letter_canaries: Vec<String>,
}

impl ProxyEngine {
    /// Build the engine from the proxy config, the canary list, and the run
    /// seed. With a seed the per-build key is deterministic (for parity); without
    /// one it is 32 bytes of OS entropy.
    pub fn new(config: &ProxyConfig, canaries: &[Canary], seed: Option<&[u8]>) -> Result<Self> {
        let pattern = config.names.join("|");
        let proxy_re = Regex::new(&format!("(?i){pattern}"))
            .map_err(|e| config_err(format!("proxy names regex: {e}")))?;

        for (name, pieces) in &config.shared_names {
            if pieces.concat() != *name {
                return Err(config_err(format!(
                    "proxy shared_names: pieces for {name:?} join to {:?}",
                    pieces.concat()
                )));
            }
        }

        let mut vendor = GlobSetBuilder::new();
        for g in &config.vendor {
            vendor.add(
                Glob::new(g).map_err(|e| config_err(format!("proxy vendor glob {g:?}: {e}")))?,
            );
        }
        let vendor = vendor
            .build()
            .map_err(|e| config_err(format!("proxy vendor globs: {e}")))?;

        let key = match seed {
            Some(s) => {
                let mut h = Sha256::new();
                h.update(s);
                h.update([0x50]);
                h.update(b"proxy-key");
                let d = h.finalize();
                let mut k = [0u8; 32];
                k.copy_from_slice(&d);
                k
            }
            None => {
                let mut k = [0u8; 32];
                getrandom::getrandom(&mut k)
                    .map_err(|e| config_err(format!("OS randomness: {e}")))?;
                k
            }
        };

        let letter_canaries = canaries
            .iter()
            .map(|c| c.value.to_ascii_lowercase())
            .filter(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_lowercase()))
            .collect();

        Ok(ProxyEngine {
            proxy_re,
            shared_names: config.shared_names.clone(),
            shared_strings: config.shared_strings.iter().cloned().collect(),
            store_tokens: config.store_tokens.clone(),
            store_names: config.store_names.iter().cloned().collect(),
            vendor,
            legal_re: Regex::new("(?i)@license|@preserve|licen[sc]|copyright|^!")
                .expect("static legal regex"),
            key,
            letter_canaries,
        })
    }

    /// Whether a JS file at `path` takes the proxy path instead of full literal
    /// encryption.
    pub fn is_vendor(&self, path: &Path) -> bool {
        // Match against the forward-slash form so the globs are portable.
        let s = path.to_string_lossy().replace('\\', "/");
        self.vendor.is_match(&s)
    }

    /// Whether a string carries any proxy name.
    pub fn has_proxy_name(&self, s: &str) -> bool {
        self.proxy_re.is_match(s)
    }

    /// Whether a string is a legal or preserve notice (kept verbatim).
    pub fn is_legal(&self, s: &str) -> bool {
        self.legal_re.is_match(s)
    }

    /// Whether a string value is app-shared (kept verbatim, encrypted).
    pub fn is_app_shared(&self, s: &str) -> bool {
        self.shared_strings.contains(s)
    }

    /// Whether a string is a listed persistent store name.
    pub fn is_store_name(&self, s: &str) -> bool {
        self.store_names.contains(s)
    }

    /// The rebuild pieces for an app-shared global, if it is one.
    pub fn shared_pieces(&self, name: &str) -> Option<&[String]> {
        self.shared_names.get(name).map(|v| v.as_slice())
    }

    /// Whether a value would spell a lower-case canary.
    fn spells_letter_canary(&self, text: &str) -> bool {
        if self.letter_canaries.is_empty() {
            return false;
        }
        let lower = text.to_ascii_lowercase();
        self.letter_canaries.iter().any(|c| lower.contains(c))
    }

    /// Same-length substitution for one proxy token. The letters depend only on
    /// the per-build key and the lower-cased token, keep the case of each
    /// position, and are redrawn while they would spell a canary.
    pub fn proxy_replacement(&self, token: &str) -> Result<String> {
        let lower = token.to_ascii_lowercase();
        for attempt in 0..MAX_RENAME_ATTEMPTS {
            let msg = if attempt == 0 {
                lower.clone()
            } else {
                format!("{lower}:{attempt}")
            };
            let mut mac = HmacSha256::new_from_slice(&self.key).expect("hmac key length");
            mac.update(msg.as_bytes());
            let digest = mac.finalize().into_bytes();
            if token.len() > digest.len() {
                return Err(config_err(format!(
                    "proxy token {token:?} is longer than the 32-byte digest"
                )));
            }
            let out: String = token
                .bytes()
                .enumerate()
                .map(|(i, src)| {
                    let letter = (b'a' + digest[i] % 26) as char;
                    if src.is_ascii_uppercase() {
                        letter.to_ascii_uppercase()
                    } else {
                        letter
                    }
                })
                .collect();
            if !self.spells_letter_canary(&out) {
                return Ok(out);
            }
        }
        Err(config_err(format!(
            "proxy rename for {token:?} did not converge to a canary-free value"
        )))
    }

    /// Rename every proxy token in a string by [`proxy_replacement`]. The result
    /// is the same length as the input.
    pub fn map_proxy_text(&self, s: &str) -> Result<String> {
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        for m in self.proxy_re.find_iter(s) {
            out.push_str(&s[last..m.start()]);
            out.push_str(&self.proxy_replacement(m.as_str())?);
            last = m.end();
        }
        out.push_str(&s[last..]);
        Ok(out)
    }

    /// The earliest app-shared-name match in `s`: a shared name followed by a
    /// non-identifier byte or the end. Ties break to the longest name, so
    /// `$scramjetController` wins over `$scramjet`.
    fn shared_match(&self, s: &str) -> Option<(usize, usize)> {
        let bytes = s.as_bytes();
        let mut best: Option<(usize, usize)> = None;
        for name in self.shared_names.keys() {
            let nb = name.as_bytes();
            let mut from = 0;
            while let Some(rel) = find_sub(&bytes[from..], nb) {
                let at = from + rel;
                let after = at + nb.len();
                let boundary = after >= bytes.len() || !is_ident_byte(bytes[after]);
                if boundary {
                    let cand = (at, nb.len());
                    best = Some(match best {
                        Some((bs, bl)) if bs < at || (bs == at && bl >= nb.len()) => (bs, bl),
                        _ => cand,
                    });
                    break;
                }
                from = at + 1;
            }
        }
        best
    }

    /// Whether a string contains an app-shared name at an identifier boundary.
    pub fn contains_shared_name(&self, s: &str) -> bool {
        self.shared_match(s).is_some()
    }

    /// Rename only the non-shared proxy tokens in a string, leaving every
    /// app-shared name real. The earliest of the next proxy match and the next
    /// shared match wins, so a shared name passes through untouched while the
    /// proxy tokens around it are renamed.
    pub fn map_shared_protected(&self, s: &str) -> Result<String> {
        let mut result = String::with_capacity(s.len());
        let mut i = 0;
        while i < s.len() {
            let rest = &s[i..];
            let pm = self
                .proxy_re
                .find(rest)
                .map(|m| (m.start(), m.end(), m.as_str()));
            let sm = self.shared_match(rest);
            // The earliest of the next proxy match and the next shared name wins;
            // on a tie the shared name wins so it stays real.
            let shared_wins = match (&pm, &sm) {
                (_, Some((s_at, _))) => pm.as_ref().is_none_or(|(p_at, _, _)| s_at <= p_at),
                (_, None) => false,
            };
            if pm.is_none() && sm.is_none() {
                result.push_str(rest);
                break;
            }
            if shared_wins {
                let (s_at, s_len) = sm.expect("shared match present when it wins");
                result.push_str(&rest[..s_at + s_len]);
                i += s_at + s_len;
            } else {
                let (p_at, p_end, p_text) =
                    pm.expect("proxy match present when shared does not win");
                result.push_str(&rest[..p_at]);
                result.push_str(&self.proxy_replacement(p_text)?);
                i += p_end;
            }
        }
        Ok(result)
    }

    /// Same-length substitution for a persistent store name, using the fixed
    /// `store_tokens` so the name is identical every build. A matched token with
    /// no fixed substitution fails the build.
    pub fn map_store_name(&self, s: &str) -> Result<String> {
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        for m in self.proxy_re.find_iter(s) {
            out.push_str(&s[last..m.start()]);
            let token = m.as_str();
            let letters = self
                .store_tokens
                .get(&token.to_ascii_lowercase())
                .ok_or_else(|| Error::Proxy {
                    message: format!(
                        "no fixed store substitution for {token:?} in store name {s:?}"
                    ),
                })?;
            out.push_str(&apply_case(token, letters));
            last = m.end();
        }
        out.push_str(&s[last..]);
        Ok(out)
    }

    /// Rename proxy names inside a regular expression pattern, but only where the
    /// name stands for itself (not inside a class, a group name, or across an
    /// escape). A name that falls across such a boundary is a hard failure, as
    /// is a pattern that names an app-shared global.
    pub fn map_proxy_regex(&self, pattern: &str, flags: &str) -> Result<String> {
        if !self.has_proxy_name(pattern) {
            return Ok(pattern.to_string());
        }
        if self.contains_shared_name(pattern) {
            return Err(regex_err(pattern, flags, "names an app-shared global"));
        }
        let b = pattern.as_bytes();
        let mut plain = vec![false; b.len()];
        let unicode_sets = flags.contains('v');
        let mut depth = 0i32;
        let mut i = 0usize;
        while i < b.len() {
            let ch = b[i];
            if ch == b'\\' {
                let next = b.get(i + 1).copied();
                if next == Some(b'k') && b.get(i + 2) == Some(&b'<') {
                    i = index_of(b, b'>', i)
                        .ok_or_else(|| regex_err(pattern, flags, "unreadable"))?;
                } else if matches!(next, Some(b'p') | Some(b'P') | Some(b'u'))
                    && b.get(i + 2) == Some(&b'{')
                {
                    i = index_of(b, b'}', i)
                        .ok_or_else(|| regex_err(pattern, flags, "unreadable"))?;
                } else {
                    i += 1;
                }
                i += 1;
                continue;
            }
            if depth > 0 {
                if ch == b']' {
                    depth -= 1;
                } else if ch == b'[' && unicode_sets {
                    depth += 1;
                }
                i += 1;
                continue;
            }
            if ch == b'[' {
                depth += 1;
                i += 1;
                continue;
            }
            if ch == b'('
                && b.get(i + 1) == Some(&b'?')
                && b.get(i + 2) == Some(&b'<')
                && !matches!(b.get(i + 3), Some(&b'=') | Some(&b'!'))
            {
                i = index_of(b, b'>', i).ok_or_else(|| regex_err(pattern, flags, "unreadable"))?;
                i += 1;
                continue;
            }
            plain[i] = true;
            i += 1;
        }
        let mut out = String::with_capacity(pattern.len());
        let mut last = 0;
        for m in self.proxy_re.find_iter(pattern) {
            if plain[m.start()..m.end()].iter().any(|&p| !p) {
                return Err(regex_err(
                    pattern,
                    flags,
                    "spells a proxy name across an escape, a class or a group name; rename it by hand",
                ));
            }
            out.push_str(&pattern[last..m.start()]);
            out.push_str(&self.proxy_replacement(m.as_str())?);
            last = m.end();
        }
        out.push_str(&pattern[last..]);
        Ok(out)
    }
}

/// Apply the case of each position in `token` to `letters` (same length).
fn apply_case(token: &str, letters: &str) -> String {
    token
        .bytes()
        .zip(letters.bytes())
        .map(|(t, l)| {
            if t.is_ascii_uppercase() {
                (l as char).to_ascii_uppercase()
            } else {
                l as char
            }
        })
        .collect()
}

/// First index of `needle` in `haystack`, by byte.
fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Index of byte `target` in `b` at or after `from`.
fn index_of(b: &[u8], target: u8, from: usize) -> Option<usize> {
    b[from..]
        .iter()
        .position(|&x| x == target)
        .map(|p| from + p)
}

fn config_err(message: String) -> Error {
    Error::Config {
        path: std::path::PathBuf::from("<proxy>"),
        message,
    }
}

fn regex_err(pattern: &str, flags: &str, why: &str) -> Error {
    Error::Proxy {
        message: format!("regular expression /{pattern}/{flags} {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> ProxyEngine {
        ProxyEngine::new(&ProxyConfig::default(), &[], Some(b"proxy-seed")).unwrap()
    }

    #[test]
    fn proxy_replacement_is_same_length_cased_and_deterministic() {
        let e = engine();
        let a = e.proxy_replacement("Scramjet").unwrap();
        assert_eq!(a.len(), "Scramjet".len());
        assert!(a.starts_with(|c: char| c.is_ascii_uppercase())); // keeps the S case
        assert!(a[1..].bytes().all(|b| b.is_ascii_lowercase()));
        // Deterministic across engines seeded the same.
        let b = engine().proxy_replacement("Scramjet").unwrap();
        assert_eq!(a, b);
        // A different token maps differently.
        assert_ne!(e.proxy_replacement("epoxy").unwrap(), "epoxy");
    }

    #[test]
    fn map_proxy_text_renames_every_token_consistently() {
        let e = engine();
        let one = e.map_proxy_text("scramjetClient uses epoxy").unwrap();
        assert!(!one.contains("scramjet"));
        assert!(!one.contains("epoxy"));
        // The same token renames to the same letters wherever it appears.
        let two = e.map_proxy_text("scramjet and scramjet").unwrap();
        let parts: Vec<&str> = two.split(" and ").collect();
        assert_eq!(parts[0], parts[1]);
    }

    #[test]
    fn shared_protected_keeps_the_global_real_but_renames_around_it() {
        let e = engine();
        let out = e
            .map_shared_protected("$scramjetController.load(scramjetPath)")
            .unwrap();
        // The published global stays exactly real.
        assert!(out.contains("$scramjetController"));
        // The config-key token "scramjet" inside scramjetPath is renamed.
        assert!(!out.contains("scramjetPath"));
    }

    #[test]
    fn store_name_uses_the_fixed_substitution() {
        let e = engine();
        assert_eq!(e.map_store_name("stompCookies").unwrap(), "gdosxCookies");
        assert_eq!(
            e.map_store_name("__scramjet_controller").unwrap(),
            "__ebshkqee_controller"
        );
    }

    #[test]
    fn is_vendor_matches_the_default_globs() {
        let e = engine();
        assert!(e.is_vendor(std::path::Path::new("dist/vendor/core.js")));
        assert!(e.is_vendor(std::path::Path::new("dist/sw.js")));
        assert!(!e.is_vendor(std::path::Path::new("dist/assets/app.js")));
    }

    #[test]
    fn regex_renames_a_plain_name_and_refuses_one_across_a_class() {
        let e = engine();
        assert_eq!(e.map_proxy_regex("ab", "").unwrap(), "ab"); // no proxy name
        let renamed = e.map_proxy_regex("scramjet", "g").unwrap();
        assert_ne!(renamed, "scramjet");
        assert_eq!(renamed.len(), "scramjet".len());
        // A proxy name sitting inside a character class cannot be renamed safely.
        assert!(e.map_proxy_regex("[scramjet]", "").is_err());
    }

    #[test]
    fn shared_name_is_detected_at_a_boundary() {
        let e = engine();
        assert!(e.contains_shared_name("x=$scramjetController;"));
        // Followed by an identifier char, it is not the shared global.
        assert!(!e.contains_shared_name("$scramjetControllerX"));
    }
}
