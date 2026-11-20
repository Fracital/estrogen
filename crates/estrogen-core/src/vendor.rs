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

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use globset::Glob;
use serde::Serialize;
use walkdir::WalkDir;

use crate::error::{Error, Result};
use crate::options::{Rewrite, VendorInput, DEFAULT_VENDOR_MANIFEST};

/// Characters that make a `from` a glob rather than a plain path.
const GLOB_CHARS: &[char] = &['*', '?', '[', ']', '{', '}'];

/// One file a vendor input brings in: read `src`, write `dest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorFile {
    /// The source file on disk.
    pub src: PathBuf,
    /// Where it lands under the dist root, stamped if the input asked.
    pub dest: PathBuf,
    /// Where it would land without a stamp: the public path the app references,
    /// relative to nothing (still rooted at dist). The manifest maps this to
    /// [`dest`](Self::dest) so the app can find the stamped name. When the input
    /// does not stamp, this equals `dest`.
    pub public_path: PathBuf,
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
        let base = if to_is_file {
            dist.join(to)
        } else {
            let name = from.file_name().unwrap_or_default();
            dist.join(to).join(name)
        };
        files.push(VendorFile {
            src: from.to_path_buf(),
            dest: stamp_path(&base, input.stamp, stamp),
            public_path: base,
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
    let base = dist.join(to).join(rel);
    VendorFile {
        src: src.to_path_buf(),
        dest: stamp_path(&base, stamp_on, stamp),
        public_path: base,
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
/// replacement. Rewrites run in order, each replacing every occurrence. Returns
/// the rewritten text and, per rewrite in order, how many times its `find`
/// matched in this file, so the caller can enforce [`Rewrite::required`] and
/// [`Rewrite::count`] across the input's files.
pub fn apply_rewrites(text: &str, rewrites: &[Rewrite], stamp: &str) -> (String, Vec<usize>) {
    let mut out = text.to_string();
    let mut counts = Vec::with_capacity(rewrites.len());
    for rw in rewrites {
        let hits = if rw.find.is_empty() {
            0
        } else {
            out.matches(&rw.find).count()
        };
        counts.push(hits);
        let to = rw.to.replace("{stamp}", stamp);
        out = out.replace(&rw.find, &to);
    }
    (out, counts)
}

/// Apply a vendor input's rewrites to a wasm module's bytes, safely. A blind
/// text replace over a `.wasm` would corrupt LEB128 section lengths and the
/// instruction stream, so this edits only the name and data byte ranges, at
/// equal length, and re-validates the module afterward. It reuses
/// [`crate::wasm::map_wasm_bytes`], which walks the module, rewrites each name or
/// data range, and fails closed if a replacement would change a range's length.
/// A rewrite whose `to` differs in length from its `find` therefore fails the
/// build rather than shipping a corrupt binary. Returns the mapped bytes and the
/// per-rewrite match counts (counted inside the name and data ranges only, where
/// a rename is real, so a name spelled by chance in the code is not counted).
pub fn apply_wasm_rewrites(
    bytes: &[u8],
    rewrites: &[Rewrite],
    stamp: &str,
    path: &Path,
) -> Result<(Vec<u8>, Vec<usize>)> {
    // The name and data ranges are where a rename is genuine. Count there, so
    // the required/count tripwire measures real names, not code bytes.
    let ranges = crate::wasm::wasm_name_ranges(bytes).map_err(|e| wasm_rewrite_err(path, e))?;
    let names: Vec<String> = ranges
        .iter()
        .map(|&(a, b)| bytes[a..b].iter().map(|&c| c as char).collect())
        .collect();
    let mut counts = Vec::with_capacity(rewrites.len());
    for rw in rewrites {
        let hits = if rw.find.is_empty() {
            0
        } else {
            names.iter().map(|n| n.matches(&rw.find).count()).sum()
        };
        counts.push(hits);
    }

    // The substitution applied to each name or data range: run every rewrite in
    // order with `{stamp}` expanded. Same-length is enforced by map_wasm_bytes.
    let subst = |s: &str| -> String {
        let mut out = s.to_string();
        for rw in rewrites {
            let to = rw.to.replace("{stamp}", stamp);
            out = out.replace(&rw.find, &to);
        }
        out
    };
    let out = crate::wasm::map_wasm_bytes(bytes, &subst).map_err(|e| wasm_rewrite_err(path, e))?;
    // Re-validate: a structurally broken module must never be written. The walk
    // succeeds only on a well-formed module, so a failure here fails the build.
    crate::wasm::wasm_name_ranges(&out).map_err(|e| wasm_rewrite_err(path, e))?;
    Ok((out, counts))
}

/// Enforce each rewrite's [`Rewrite::required`] and [`Rewrite::count`] against
/// the totals counted across every file of one input. A `count` asserts the
/// exact number of matches; otherwise `required` asserts at least one. The build
/// fails closed with a clear error naming the input and the literal when an
/// assertion does not hold, so a package reshape that drops a patched reference
/// is caught here instead of shipping a dangling one.
pub fn check_rewrites(input: &VendorInput, totals: &[usize]) -> Result<()> {
    for (rw, &found) in input.rewrite.iter().zip(totals) {
        match rw.count {
            Some(want) if found != want => {
                return Err(rewrite_err(
                    &input.from,
                    format!(
                        "rewrite {:?} matched {found} time(s), expected exactly {want}",
                        rw.find
                    ),
                ));
            }
            None if rw.required && found == 0 => {
                return Err(rewrite_err(
                    &input.from,
                    format!(
                        "required rewrite {:?} matched nothing; the vendored package may have \
                         been reshaped. Fix the `find`, or set `required = false` if the target \
                         is an optional fallback.",
                        rw.find
                    ),
                ));
            }
            _ => {}
        }
    }
    Ok(())
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

/// A rewrite-assertion error naming the input whose rewrite failed.
fn rewrite_err(from: &str, message: String) -> Error {
    Error::Config {
        path: PathBuf::from(from),
        message: format!("vendor rewrite: {message}"),
    }
}

/// Wrap a wasm walk or mapping failure as an error naming the file it hit.
fn wasm_rewrite_err(path: &Path, err: Error) -> Error {
    Error::Config {
        path: path.to_path_buf(),
        message: format!("vendor wasm rewrite: {err}"),
    }
}

/// The vendor manifest: the per-build stamp and the map from each vendored
/// file's public path to its stamped served path. `build` writes it to a path
/// under the dist root so the app can find what estrogen renamed, the same job
/// Fracital does by recomputing a build stamp and SystemYA by injecting an asset
/// map. Both paths are relative to the dist root with forward slashes. It raises
/// the cost of reading a build; it is not secrecy.
#[derive(Debug, Clone, Serialize)]
pub struct VendorManifest {
    /// The per-build stamp, also usable as a build-time define.
    pub stamp: String,
    /// Public path to served path, both relative to dist, sorted.
    pub files: BTreeMap<String, String>,
}

impl VendorManifest {
    /// An empty manifest carrying the build stamp.
    pub fn new(stamp: &str) -> Self {
        VendorManifest {
            stamp: stamp.to_string(),
            files: BTreeMap::new(),
        }
    }

    /// Record one vendored file: its public path mapped to its served path, both
    /// relative to `dist`.
    pub fn add(&mut self, dist: &Path, vf: &VendorFile) {
        self.files
            .insert(rel_to(dist, &vf.public_path), rel_to(dist, &vf.dest));
    }

    /// Serialize to pretty JSON with a trailing newline. The `files` map is
    /// sorted, so the output is byte-for-byte stable for a given build.
    pub fn to_json(&self) -> Result<String> {
        let mut json = serde_json::to_string_pretty(self).map_err(|e| Error::Config {
            path: PathBuf::from(DEFAULT_VENDOR_MANIFEST),
            message: format!("could not serialize the vendor manifest: {e}"),
        })?;
        json.push('\n');
        Ok(json)
    }

    /// Write the manifest to `path`, resolved under `dist` when relative.
    pub fn write(&self, dist: &Path, path: &Path) -> Result<()> {
        let target = if path.is_absolute() {
            path.to_path_buf()
        } else {
            dist.join(path)
        };
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        fs::write(&target, self.to_json()?).map_err(|e| Error::io(&target, e))
    }
}

/// A path under `dist` as a forward-slash string relative to it, for the
/// manifest. A path that is not under `dist` is kept whole.
fn rel_to(dist: &Path, path: &Path) -> String {
    path.strip_prefix(dist)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
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
        let (out, counts) = apply_rewrites(
            "load('epoxy.wasm')",
            &[Rewrite {
                find: "epoxy.wasm".to_string(),
                to: "ln.{stamp}.wasm".to_string(),
                ..Rewrite::default()
            }],
            "k3x9",
        );
        assert_eq!(out, "load('ln.k3x9.wasm')");
        assert_eq!(counts, vec![1]);
    }

    fn rw(find: &str, to: &str) -> Rewrite {
        Rewrite {
            find: find.to_string(),
            to: to.to_string(),
            ..Rewrite::default()
        }
    }

    fn input_with(from: &str, rewrite: Vec<Rewrite>) -> VendorInput {
        VendorInput {
            from: from.to_string(),
            to: "assets".to_string(),
            mode: crate::options::VendorMode::Verbatim,
            stamp: true,
            rewrite,
        }
    }

    #[test]
    fn a_required_rewrite_that_misses_fails_with_a_clear_error() {
        let input = input_with(
            "node_modules/epoxy",
            vec![rw("epoxy.wasm", "ln.{stamp}.wasm")],
        );
        // No file matched the literal: the aggregated total is zero.
        let err = check_rewrites(&input, &[0]).unwrap_err();
        match err {
            Error::Config { message, .. } => {
                assert!(message.contains("required rewrite"), "message: {message}");
                assert!(
                    message.contains("epoxy.wasm"),
                    "names the literal: {message}"
                );
            }
            other => panic!("expected a config error, got {other:?}"),
        }
        // One match satisfies it.
        assert!(check_rewrites(&input, &[1]).is_ok());
    }

    #[test]
    fn an_optional_rewrite_may_miss() {
        let mut r = rw("dead.wasm", "x");
        r.required = false;
        let input = input_with("node_modules/epoxy", vec![r]);
        assert!(check_rewrites(&input, &[0]).is_ok());
    }

    #[test]
    fn a_count_asserts_the_exact_number() {
        let mut r = rw("libcurl", "xxxxxxx");
        r.count = Some(2);
        let input = input_with("node_modules/libcurl", vec![r]);
        assert!(check_rewrites(&input, &[2]).is_ok());
        let err = check_rewrites(&input, &[1]).unwrap_err();
        assert!(matches!(err, Error::Config { .. }));
        // count = 0 asserts absence, and passes only when nothing matched.
        let mut r0 = rw("gone", "gone");
        r0.count = Some(0);
        let input0 = input_with("x", vec![r0]);
        assert!(check_rewrites(&input0, &[0]).is_ok());
        assert!(check_rewrites(&input0, &[1]).is_err());
    }

    /// A minimal valid wasm module exporting one empty function under `name`.
    fn wasm_exporting(name: &str) -> Vec<u8> {
        let n = name.as_bytes();
        let mut exp = vec![0x01, n.len() as u8];
        exp.extend_from_slice(n);
        exp.extend_from_slice(&[0x00, 0x00]);
        let sec = |id: u8, body: &[u8]| {
            let mut v = vec![id, body.len() as u8];
            v.extend_from_slice(body);
            v
        };
        let mut wasm = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        wasm.extend(sec(0x01, &[0x01, 0x60, 0x00, 0x00]));
        wasm.extend(sec(0x03, &[0x01, 0x00]));
        wasm.extend(sec(0x07, &exp));
        wasm.extend(sec(0x0a, &[0x01, 0x02, 0x00, 0x0b]));
        wasm
    }

    #[test]
    fn wasm_rewrite_edits_only_the_name_range_and_revalidates() {
        // The export name carries "epoxy"; a same-length rewrite renames it in
        // the name range and leaves the module valid.
        let wasm = wasm_exporting("epoxy_add");
        let (out, counts) = apply_wasm_rewrites(
            &wasm,
            &[rw("epoxy", "ln0x0")],
            "k3x9",
            Path::new("core.wasm"),
        )
        .unwrap();
        assert_eq!(
            out.len(),
            wasm.len(),
            "same length keeps the structure valid"
        );
        assert_eq!(counts, vec![1], "one match in the name range");
        // The old name is gone, the new one is present, and the module re-walks.
        assert!(!contains(&out, b"epoxy_add"));
        assert!(contains(&out, b"ln0x0_add"));
        assert!(crate::wasm::wasm_name_ranges(&out).is_ok());
    }

    #[test]
    fn wasm_rewrite_of_unequal_length_fails_closed() {
        let wasm = wasm_exporting("epoxy_add");
        // "epoxy" -> "libcurl" changes the range length, which would corrupt the
        // section. map_wasm_bytes refuses it, and the build fails.
        let err = apply_wasm_rewrites(
            &wasm,
            &[rw("epoxy", "libcurl")],
            "k3x9",
            Path::new("core.wasm"),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Config { .. }));
    }

    #[test]
    fn wasm_rewrite_does_not_count_a_name_in_the_code() {
        // No name range holds "epoxy"; the export name is neutral. The count is
        // zero even though the module is valid, so a required assertion would
        // catch a reshape.
        let wasm = wasm_exporting("plain_export");
        let (_out, counts) = apply_wasm_rewrites(
            &wasm,
            &[rw("epoxy", "ln0x0")],
            "k3x9",
            Path::new("core.wasm"),
        )
        .unwrap();
        assert_eq!(counts, vec![0]);
    }

    #[test]
    fn manifest_maps_public_to_served_relative_to_dist() {
        let dist = Path::new("/tmp/dist");
        let vf = VendorFile {
            src: PathBuf::from("/pkg/sw.js"),
            dest: PathBuf::from("/tmp/dist/assets/sw.k3x9.js"),
            public_path: PathBuf::from("/tmp/dist/assets/sw.js"),
        };
        let mut m = VendorManifest::new("k3x9");
        m.add(dist, &vf);
        assert_eq!(
            m.files.get("assets/sw.js").map(String::as_str),
            Some("assets/sw.k3x9.js")
        );
        let json = m.to_json().unwrap();
        assert!(json.contains("\"stamp\": \"k3x9\""));
        assert!(json.ends_with("}\n"));
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
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
