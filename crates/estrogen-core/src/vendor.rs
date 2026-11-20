//! Vendor and copy pipeline.
//!
//! A build often has to copy files it did not bundle into the served tree: a
//! whole `node_modules` package, a folder, a transport runtime. Each site kept a
//! hand-written script for that (SystemYA's `prepare-stomp.mjs` is the prior
//! art): copy a package's files into the output under stamped neutral names,
//! rewrite the references between them, strip source maps, and obfuscate the app
//! code. This module makes that one declarative step, driven by the `[[vendor]]`
//! tables in the config.
//!
//! The pieces here resolve a [`VendorInput`] to the files it copies and where
//! they land, stamp the names, and rewrite the text. [`crate::obfuscator`] drives
//! them: it writes each file, obfuscates or keeps it verbatim, and canary checks
//! the result. The per-build stamp is minted once and reused, so a reference
//! rewritten to a stamped name matches the file that lands under it.
//!
//! This raises the cost of reading a build; it is not secrecy. Nothing here is
//! secret.

use std::path::{Path, PathBuf};

use globset::Glob;
use walkdir::WalkDir;

use crate::error::{Error, Result};
use crate::options::{Rewrite, VendorInput};

/// Characters that make a `from` a glob rather than a plain path.
const GLOB_CHARS: &[char] = &['*', '?', '[', ']', '{', '}'];

/// One file a vendor input brings in: read `src`, write `dest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorFile {
    /// The source file on disk.
    pub src: PathBuf,
    /// Where it lands under the dist root, stamped if the input asked.
    pub dest: PathBuf,
}

/// Mint the per-build stamp: a short neutral token, reused across every
/// `[[vendor]]` of the build. With `seed` set it is deterministic, so a build
/// repeats byte for byte; without a seed it is drawn from OS entropy. It never
/// spells a canary, so a stamped name cannot reintroduce one.
pub fn mint_stamp(seed: Option<&[u8]>, canaries: &[String]) -> Result<String> {
    let lower_canaries: Vec<String> = canaries
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| !c.is_empty())
        .collect();
    for attempt in 0u32..1000 {
        let bytes = stamp_bytes(seed, attempt)?;
        // 8 lowercase base36 characters: a neutral token, no package name.
        let stamp: String = bytes
            .iter()
            .take(8)
            .map(|b| char::from_digit((*b % 36) as u32, 36).unwrap())
            .collect();
        if !lower_canaries.iter().any(|c| stamp.contains(c.as_str())) {
            return Ok(stamp);
        }
    }
    Err(Error::Config {
        path: PathBuf::from("<stamp>"),
        message: "could not mint a canary-free build stamp".to_string(),
    })
}

/// Draw the bytes behind a stamp attempt: `sha256(seed || attempt)` when seeded,
/// else OS entropy.
fn stamp_bytes(seed: Option<&[u8]>, attempt: u32) -> Result<[u8; 16]> {
    let mut out = [0u8; 16];
    match seed {
        Some(seed) => {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(seed);
            h.update(b"stamp");
            h.update(attempt.to_le_bytes());
            out.copy_from_slice(&h.finalize()[..16]);
        }
        None => {
            getrandom::getrandom(&mut out).map_err(|e| Error::Config {
                path: PathBuf::from("<stamp>"),
                message: format!("could not read OS randomness: {e}"),
            })?;
        }
    }
    Ok(out)
}

/// Resolve a vendor input to the files it copies and their destinations under
/// `dist`. Handles a single file, a directory (copied recursively), and a glob.
/// A `from` that matches nothing is an error, so a renamed or missing input
/// fails the build rather than silently vendoring nothing.
pub fn resolve(input: &VendorInput, dist: &Path, stamp: &str) -> Result<Vec<VendorFile>> {
    let from = Path::new(&input.from);
    let to = Path::new(&input.to);
    let is_glob = input.from.contains(GLOB_CHARS);

    let mut files = Vec::new();

    if is_glob {
        let base = glob_base(&input.from);
        let matcher = Glob::new(&input.from)
            .map_err(|e| vendor_err(&input.from, format!("bad glob: {e}")))?
            .compile_matcher();
        for entry in WalkDir::new(&base).sort_by_file_name() {
            let entry = entry.map_err(|e| vendor_err(&input.from, e.to_string()))?;
            if !entry.file_type().is_file() || !matcher.is_match(entry.path()) {
                continue;
            }
            let rel = entry.path().strip_prefix(&base).unwrap_or(entry.path());
            files.push(dest_for(entry.path(), to, rel, dist, input.stamp, stamp));
        }
    } else if from.is_dir() {
        for entry in WalkDir::new(from).sort_by_file_name() {
            let entry = entry.map_err(|e| vendor_err(&input.from, e.to_string()))?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(from).unwrap_or(entry.path());
            files.push(dest_for(entry.path(), to, rel, dist, input.stamp, stamp));
        }
    } else if from.is_file() {
        // A single file. When `to` names a file (it has an extension and is not
        // an existing directory), it is the destination name; otherwise `to` is
        // a directory and the source's own name is kept.
        let to_is_file = to.extension().is_some() && !dist.join(to).is_dir();
        let dest = if to_is_file {
            stamp_path(&dist.join(to), input.stamp, stamp)
        } else {
            let name = from.file_name().unwrap_or_default();
            stamp_path(&dist.join(to).join(name), input.stamp, stamp)
        };
        files.push(VendorFile {
            src: from.to_path_buf(),
            dest,
        });
    } else {
        return Err(vendor_err(
            &input.from,
            "no such file, directory, or glob match".to_string(),
        ));
    }

    if files.is_empty() {
        return Err(vendor_err(&input.from, "matched no files".to_string()));
    }
    Ok(files)
}

/// Build the destination for one source file copied under `to`, keeping its
/// relative path and stamping the final component when asked.
fn dest_for(
    src: &Path,
    to: &Path,
    rel: &Path,
    dist: &Path,
    stamp_on: bool,
    stamp: &str,
) -> VendorFile {
    let dest = stamp_path(&dist.join(to).join(rel), stamp_on, stamp);
    VendorFile {
        src: src.to_path_buf(),
        dest,
    }
}

/// Stamp the final component of a path, inserting `.<stamp>` before its last
/// extension (`sw.js` becomes `sw.<stamp>.js`). A name with no extension gets
/// `.<stamp>` appended. Leaves the path unchanged when `on` is false.
fn stamp_path(path: &Path, on: bool, stamp: &str) -> PathBuf {
    if !on {
        return path.to_path_buf();
    }
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return path.to_path_buf(),
    };
    let stamped = match name.rfind('.') {
        Some(i) if i > 0 => format!("{}.{}{}", &name[..i], stamp, &name[i..]),
        _ => format!("{name}.{stamp}"),
    };
    path.with_file_name(stamped)
}

/// Apply a vendor input's text rewrites, expanding `{stamp}` in each
/// replacement. Rewrites run in order, each replacing every occurrence.
pub fn apply_rewrites(text: &str, rewrites: &[Rewrite], stamp: &str) -> String {
    let mut out = text.to_string();
    for rw in rewrites {
        let to = rw.to.replace("{stamp}", stamp);
        out = out.replace(&rw.find, &to);
    }
    out
}

/// The longest leading prefix of a glob that has no glob characters, as the
/// directory to walk. `node_modules/ruffle/**/*.js` walks `node_modules/ruffle`.
fn glob_base(pattern: &str) -> PathBuf {
    let mut base = PathBuf::new();
    if pattern.starts_with('/') {
        base.push("/");
    }
    for part in pattern.split('/') {
        if part.is_empty() {
            continue;
        }
        if part.contains(GLOB_CHARS) {
            break;
        }
        base.push(part);
    }
    if base.as_os_str().is_empty() {
        base.push(".");
    }
    base
}

/// A vendor error naming the input that failed.
fn vendor_err(from: &str, message: String) -> Error {
    Error::Config {
        path: PathBuf::from(from),
        message: format!("vendor input: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_inserts_before_the_extension() {
        assert_eq!(
            stamp_path(Path::new("dist/assets/sw.js"), true, "k3x9"),
            PathBuf::from("dist/assets/sw.k3x9.js")
        );
        assert_eq!(
            stamp_path(Path::new("dist/core.wasm"), true, "k3x9"),
            PathBuf::from("dist/core.k3x9.wasm")
        );
        assert_eq!(
            stamp_path(Path::new("dist/worker"), true, "k3x9"),
            PathBuf::from("dist/worker.k3x9")
        );
        assert_eq!(
            stamp_path(Path::new("dist/sw.js"), false, "k3x9"),
            PathBuf::from("dist/sw.js")
        );
    }

    #[test]
    fn rewrites_expand_the_stamp() {
        let out = apply_rewrites(
            "load('epoxy.wasm')",
            &[Rewrite {
                find: "epoxy.wasm".to_string(),
                to: "ln.{stamp}.wasm".to_string(),
            }],
            "k3x9",
        );
        assert_eq!(out, "load('ln.k3x9.wasm')");
    }

    #[test]
    fn seeded_stamp_is_reproducible_and_canary_free() {
        let a = mint_stamp(Some(b"seed"), &["epoxy".to_string()]).unwrap();
        let b = mint_stamp(Some(b"seed"), &["epoxy".to_string()]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
        assert!(!a.contains("epoxy"));
    }

    #[test]
    fn glob_base_stops_at_the_first_wildcard() {
        assert_eq!(
            glob_base("node_modules/ruffle/**/*.js"),
            PathBuf::from("node_modules/ruffle")
        );
        assert_eq!(glob_base("*.js"), PathBuf::from("."));
    }
}
