//! Run options and config loading.
//!
//! [`Options`] is everything a run needs that is not the directories themselves:
//! the proxy and store name map, the canary list, where to write private source
//! maps, a test seed, the job count, and the check flag. It is built from a TOML
//! config file, or assembled directly, and the CLI overlays its own flags on top.
//!
//! Config shapes:
//! - crack config (`--config`, TOML): a `[names]` table, a `[[canary]]` array of
//!   `{ value, reason }`, and an `[options]` table.
//! - canary list (`--canary`, JSON): `{ "canaries": [ { "value", "reason" } ] }`,
//!   the same shape as Fracital's `scripts/canary-list.json`. It overrides the
//!   config's canaries.
//! - names map (`--names`, JSON): a flat `{ "from": "to" }` object. It overrides
//!   the config's names.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

/// One canary: a plaintext literal that must not survive verbatim in the output,
/// with the reason it is hidden.
///
/// The reason is documentation carried from `canary-list.json`; crack checks the
/// `value`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Canary {
    /// The literal that must not appear verbatim in a shipped file.
    pub value: String,
    /// Why it is on the list. Free text, for humans.
    #[serde(default)]
    pub reason: String,
}

/// The canary list, as loaded from a JSON file.
///
/// Matches `scripts/canary-list.json`: a `canaries` array of `{ value, reason }`.
/// Extra keys such as `_README` are ignored.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CanaryList {
    /// The canaries. An entry with an empty `value` is dropped on load.
    #[serde(default)]
    pub canaries: Vec<Canary>,
}

impl CanaryList {
    /// Load a canary list from a JSON file.
    pub fn from_json_file(path: &Path) -> Result<CanaryList> {
        let text = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        let mut list: CanaryList = serde_json::from_str(&text).map_err(|e| Error::Config {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        list.canaries.retain(|c| !c.value.is_empty());
        Ok(list)
    }
}

/// The proxy and store name map: each source name mapped to its per-build
/// substitute. Ordered so reports and output are deterministic.
pub type NameMap = BTreeMap<String, String>;

/// Load a names map from a flat JSON object (`{ "from": "to" }`).
pub fn load_name_map(path: &Path) -> Result<NameMap> {
    let text = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    serde_json::from_str(&text).map_err(|e| Error::Config {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

/// The `[proxy]` table: how the proxy vendor runtime is renamed.
///
/// The proxy transport code is not string-encrypted wholesale. Instead the proxy
/// names (scramjet, stomp, epoxy and the rest) are renamed by a per-build
/// same-length substitution in identifiers, whitespace-free strings and the
/// embedded wasm, so the hot path pays no decode cost. This table names the
/// tokens, which files take the proxy path, the app-shared names that must stay
/// real, and the persistent store names that keep a fixed substitution.
///
/// Every field defaults to Fracital's values, so a config that omits `[proxy]`
/// obfuscates a Fracital build as the reference script does. Another site
/// overrides the fields it needs.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    /// Proxy token patterns, joined into one case-insensitive alternation. Each
    /// entry is a regex fragment (for example `bare[-_]?mux`).
    pub names: Vec<String>,
    /// Globs that select the files which take the proxy path instead of full
    /// literal encryption. A JS file matching any glob is a proxy vendor file.
    pub vendor: Vec<String>,
    /// App-shared globals that must stay real at runtime. Each maps a name to the
    /// pieces it is rebuilt from, so the published global keeps its exact name
    /// while every piece ships encrypted. Each pieces list must join to the name.
    pub shared_names: BTreeMap<String, Vec<String>>,
    /// App-shared string values that keep their text (encrypted, never renamed),
    /// because app code and the vendor exchange them verbatim.
    pub shared_strings: Vec<String>,
    /// Fixed same-length substitutions for the persistent store names, keyed by
    /// the lower-cased proxy token. These stay the same every build so a browser
    /// keeps the storage it already holds.
    pub store_tokens: BTreeMap<String, String>,
    /// The persistent store names that keep their name across builds. A proxy
    /// name opened as a store but not on this list fails the build.
    pub store_names: Vec<String>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        let s = |v: &str| v.to_string();
        ProxyConfig {
            names: [
                "scramjet",
                "stomp",
                "bare[-_]?mux",
                "wispurr",
                "epoxy",
                "libcurl",
                "ultraviolet",
                "titaniumnetwork",
                "uvserviceworker",
                "uvclient",
                "__uv",
            ]
            .iter()
            .map(|v| s(v))
            .collect(),
            vendor: vec![s("**/vendor/**"), s("**/sw.js")],
            shared_names: BTreeMap::from([
                (s("$scramjet"), vec![s("$"), s("scram"), s("jet")]),
                (
                    s("$scramjetController"),
                    vec![s("$"), s("scram"), s("jet"), s("Controller")],
                ),
            ]),
            shared_strings: [
                "bare-mux-path",
                "bare-mux-worker",
                "bare-mux-remote",
                "bare-mux",
            ]
            .iter()
            .map(|v| s(v))
            .collect(),
            store_tokens: BTreeMap::from([
                (s("stomp"), s("gdosx")),
                (s("scramjet"), s("ebshkqee")),
            ]),
            store_names: [
                "stompCookies",
                "stompStorage",
                "stompConfig",
                "stompServiceWorkers",
                "stompClientOrigins",
                "stompClientHints",
                "stompMediaLoads",
                "stompDeviceIds",
                "stompPermissions",
                "stompDeviceGrants",
                "stompPasskeys",
                "stomp-wisp-carrier",
                "stomp-http-cache",
                "stomp-rewrite-cache",
                "stomp-adblock",
                "stomp-document-bundles",
                "__scramjet_controller",
            ]
            .iter()
            .map(|v| s(v))
            .collect(),
        }
    }
}

/// The `[options]` table of a crack config.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ConfigOptions {
    sourcemap: Option<PathBuf>,
    jobs: Option<usize>,
    check: bool,
}

/// The whole crack config file, before it becomes [`Options`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ConfigFile {
    names: NameMap,
    canary: Vec<Canary>,
    options: ConfigOptions,
    proxy: ProxyConfig,
}

/// Everything a run needs besides the input directories.
///
/// Build it from a config file with [`Options::from_config`], or take
/// [`Options::default`] and set fields. The CLI loads a config, then overlays
/// `--canary`, `--names`, `--sourcemap`, `--seed`, `--jobs` and `--check`.
#[derive(Debug, Clone)]
pub struct Options {
    /// Proxy and store names mapped to their per-build substitutes.
    pub names: NameMap,
    /// Literals that must not survive verbatim in the output.
    pub canaries: Vec<Canary>,
    /// Where to write private source maps, or `None` to write none. The default
    /// is `None`: crack ships no map next to the served build.
    pub sourcemap_dir: Option<PathBuf>,
    /// A fixed seed for deterministic output, for tests only. `None` means real
    /// randomness, which production builds use. The bytes seed the per-file keys
    /// and stamped names.
    pub seed: Option<Vec<u8>>,
    /// How many files to process in parallel. Defaults to the CPU count.
    pub jobs: usize,
    /// Report only, write nothing, and exit nonzero if anything is unprotected.
    pub check: bool,
    /// How the proxy vendor runtime is renamed. Defaults to Fracital's values.
    pub proxy: ProxyConfig,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            names: NameMap::new(),
            canaries: Vec::new(),
            sourcemap_dir: None,
            seed: None,
            jobs: default_jobs(),
            check: false,
            proxy: ProxyConfig::default(),
        }
    }
}

impl Options {
    /// Load options from a TOML config file.
    ///
    /// The file may set `[names]`, `[[canary]]` and `[options]`; any table left
    /// out takes its default. `--canary` and `--names` on the CLI override what
    /// this returns.
    pub fn from_config(path: &Path) -> Result<Options> {
        let text = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        let file: ConfigFile = toml::from_str(&text).map_err(|e| Error::Config {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        let mut opts = Options {
            names: file.names,
            canaries: file.canary,
            sourcemap_dir: file.options.sourcemap,
            seed: None,
            jobs: file.options.jobs.unwrap_or_else(default_jobs),
            check: file.options.check,
            proxy: file.proxy,
        };
        opts.canaries.retain(|c| !c.value.is_empty());
        if opts.jobs == 0 {
            opts.jobs = default_jobs();
        }
        Ok(opts)
    }
}

/// The default job count: the number of CPUs, or 1 when that cannot be read.
pub fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_file(name: &str, body: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "crack-test-{}-{}-{name}",
            std::process::id(),
            // A per-call counter keeps parallel tests from colliding.
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    #[test]
    fn default_options_are_empty_and_write_no_map() {
        let o = Options::default();
        assert!(o.names.is_empty());
        assert!(o.canaries.is_empty());
        assert!(o.sourcemap_dir.is_none());
        assert!(o.seed.is_none());
        assert!(!o.check);
        assert!(o.jobs >= 1);
    }

    #[test]
    fn config_loads_names_canaries_and_options() {
        let path = tmp_file(
            "config.toml",
            r#"
[names]
scramjet = "x9"
Scramjet = "X9"

[[canary]]
value = "Fracital"
reason = "brand name"

[[canary]]
value = "scramjet"
reason = "proxy engine"

[options]
jobs = 4
check = true
sourcemap = "/tmp/private-maps"
"#,
        );
        let o = Options::from_config(&path).unwrap();
        assert_eq!(o.names.get("scramjet").map(String::as_str), Some("x9"));
        assert_eq!(o.names.get("Scramjet").map(String::as_str), Some("X9"));
        assert_eq!(o.canaries.len(), 2);
        assert_eq!(o.canaries[0].value, "Fracital");
        assert_eq!(o.jobs, 4);
        assert!(o.check);
        assert_eq!(o.sourcemap_dir, Some(PathBuf::from("/tmp/private-maps")));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn empty_config_takes_defaults() {
        let path = tmp_file("empty.toml", "");
        let o = Options::from_config(&path).unwrap();
        assert!(o.names.is_empty());
        assert!(o.canaries.is_empty());
        assert!(o.jobs >= 1);
        assert!(!o.check);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn canary_list_json_parses_and_drops_empty_values() {
        let path = tmp_file(
            "canary.json",
            r#"{ "_README": "ignore me",
                "canaries": [
                  { "value": "Fracital", "reason": "brand" },
                  { "value": "", "reason": "blank, dropped" },
                  { "value": "scramjet" }
                ] }"#,
        );
        let list = CanaryList::from_json_file(&path).unwrap();
        assert_eq!(list.canaries.len(), 2);
        assert_eq!(list.canaries[0].value, "Fracital");
        assert_eq!(list.canaries[1].value, "scramjet");
        assert_eq!(list.canaries[1].reason, "");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn name_map_json_parses() {
        let path = tmp_file("names.json", r#"{ "scramjet": "x9", "stomp": "q2" }"#);
        let m = load_name_map(&path).unwrap();
        assert_eq!(m.get("scramjet").map(String::as_str), Some("x9"));
        assert_eq!(m.get("stomp").map(String::as_str), Some("q2"));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn bad_toml_is_a_config_error() {
        let path = tmp_file("broken.toml", "this = = not toml");
        let err = Options::from_config(&path).unwrap_err();
        assert!(matches!(err, Error::Config { .. }));
        fs::remove_file(&path).ok();
    }
}
