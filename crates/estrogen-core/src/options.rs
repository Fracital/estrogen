//! Run options and config loading.
//!
//! [`Options`] is everything a run needs that is not the directories themselves:
//! the proxy and store name map, the canary list, where to write private source
//! maps, a test seed, the job count, and the check flag. It is built from a TOML
//! config file, or assembled directly, and the CLI overlays its own flags on top.
//!
//! Config shapes:
//! - estrogen config (`--config`, TOML): a `[names]` table, a `[[canary]]` array of
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
/// The reason is documentation carried from `canary-list.json`; estrogen checks the
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

/// The default path for the vendor manifest, under the dist root.
pub const DEFAULT_VENDOR_MANIFEST: &str = "estrogen-vendor-manifest.json";

/// The `[options]` table of an estrogen config.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ConfigOptions {
    sourcemap: Option<PathBuf>,
    jobs: Option<usize>,
    check: bool,
    vm: Option<bool>,
    minify: Option<bool>,
    optimize_svg: Option<bool>,
    minify_html: Option<bool>,
    minify_css: Option<bool>,
    top_level_names: Option<bool>,
    /// Where `build` writes the vendor manifest. Absent uses
    /// [`DEFAULT_VENDOR_MANIFEST`]; an empty string turns it off. A relative path
    /// lands under the dist root, an absolute path is used as is.
    manifest: Option<String>,
}

/// How a vendored input is brought into the build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VendorMode {
    /// Copy the file as is. It is still source-map stripped and canary checked,
    /// but not obfuscated, which is what a vendored transport runtime wants.
    #[default]
    Verbatim,
    /// Run the obfuscation pass over the copied file on the way in.
    Obfuscate,
}

/// One reference rewrite applied to a vendored file's text. `{stamp}` in `to`
/// expands to the per-build stamp, so a reference can point at the stamped name.
///
/// A rewrite is a tripwire as much as a substitution. By default `required` is
/// true: if `find` never appears across the input's files, the build fails
/// instead of shipping a reshaped package with a dangling reference. This is
/// what every reference script does by hand (Fracital and abstract-v2 both
/// `throw` when a patched literal is missing). Set `required = false` for a
/// rewrite that targets an optional fallback, such as the dead
/// `new URL("epoxy.wasm")` an inline-wasm transport leaves behind. Set `count`
/// to assert the exact number of occurrences across the input.
#[derive(Debug, Clone, Deserialize)]
pub struct Rewrite {
    /// The text to find.
    pub find: String,
    /// What to replace it with. `{stamp}` expands to the build stamp.
    pub to: String,
    /// Fail the build when `find` is not present. True by default, so a package
    /// reshape that drops the patched literal is caught at build time. Set it
    /// false for a rewrite whose target may legitimately be absent.
    #[serde(default = "default_required")]
    pub required: bool,
    /// When set, `find` must appear exactly this many times across the input's
    /// files, or the build fails. `0` asserts the literal is absent. When unset,
    /// `required` governs: at least one occurrence, or none required.
    #[serde(default)]
    pub count: Option<usize>,
}

impl Default for Rewrite {
    fn default() -> Self {
        Rewrite {
            find: String::new(),
            to: String::new(),
            required: true,
            count: None,
        }
    }
}

/// The default for [`Rewrite::required`]: true. A rewrite asserts its target is
/// present unless told otherwise.
fn default_required() -> bool {
    true
}

/// One vendored input: a `[[vendor]]` table. It copies a package directory, a
/// folder, or a glob into the dist under a stamped neutral name, optionally
/// rewriting references and obfuscating on the way in. This replaces the manual
/// copy step each site keeps; the prior art is SystemYA's `prepare-stomp.mjs`.
#[derive(Debug, Clone, Deserialize)]
pub struct VendorInput {
    /// A package directory, a folder, or a glob, relative to the working dir.
    pub from: String,
    /// The destination under the dist root.
    pub to: String,
    /// Verbatim (the default) or obfuscate on the way in.
    #[serde(default)]
    pub mode: VendorMode,
    /// Insert the per-build stamp before the extension of each copied file, so
    /// no served file names its package.
    #[serde(default)]
    pub stamp: bool,
    /// Reference rewrites applied to each copied file's text.
    #[serde(default)]
    pub rewrite: Vec<Rewrite>,
}

/// The whole estrogen config file, before it becomes [`Options`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ConfigFile {
    names: NameMap,
    canary: Vec<Canary>,
    options: ConfigOptions,
    proxy: ProxyConfig,
    vendor: Vec<VendorInput>,
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
    /// is `None`: estrogen ships no map next to the served build.
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
    /// Inputs to copy into the dist before obfuscating, from `[[vendor]]`. Empty
    /// by default, so a plain obfuscate run vendors nothing.
    pub vendor: Vec<VendorInput>,
    /// Where [`crate::Obfuscator::build`] writes the vendor manifest: the stamp
    /// and the map from each vendored file's public path to its stamped served
    /// path. A relative path lands under the dist root; an absolute path is used
    /// as is. `None` writes no manifest. The default is
    /// [`DEFAULT_VENDOR_MANIFEST`] under dist, so the app can locate what was
    /// renamed.
    pub manifest: Option<PathBuf>,
    /// Compile app chunks to the bytecode VM, the default transform for a
    /// non-vendor JS chunk. On by default. A chunk the VM cannot compile falls
    /// back to the literal-pool and name passes, so turning this off only
    /// changes which transform an app chunk takes, never whether it is
    /// protected. The CLI exposes `--no-vm`.
    pub vm: bool,
    /// Collapse the edit-and-splice JS output to a single line. On by default.
    /// The VM path already ships one line; this reprints the literal and proxy
    /// pass output ([`crate::minify`]) so it does too, with newlines only inside
    /// backtick template literals. It fails closed: a chunk that would not
    /// re-parse or that surfaces a canary after the reprint keeps the
    /// edit-preserving output and its private source map. The CLI exposes
    /// `--no-minify`.
    pub minify: bool,
    /// Optimize `.svg` asset files: shrink each plain image SVG with the SVGO
    /// default job set (drop metadata and comments, collapse whitespace, round
    /// path numbers, remove default attributes) and write the smaller result.
    /// On by default; it replaces a site's separate `svgmin` step. A cloak SVG
    /// (one with an inline `<script>`) is left for the markup pass, and an SVG
    /// that does not shrink is left exactly as it was. The CLI exposes
    /// `--no-optimize-svg`.
    pub optimize_svg: bool,
    /// Minify `.html` shells: strip comments and collapse insignificant
    /// whitespace before the cloak pass runs, leaving `<pre>`, `<textarea>`,
    /// `<script>` and `<style>` content intact. On by default. Fails closed on
    /// malformed markup (the original is kept). The CLI exposes
    /// `--no-minify-html`.
    pub minify_html: bool,
    /// Minify `.css` stylesheets with lightningcss. On by default. CSS is not an
    /// obfuscation target, so this only shrinks it; it fails closed on a parse
    /// error (the original is kept). The CLI exposes `--no-minify-css`.
    pub minify_css: bool,
    /// Rename top-level identifiers in a classic script too, the way an ES
    /// module's already are. Off by default. A `function` or `var` at the top of
    /// a classic
    /// script is a property of the global object, so another script can read it
    /// by name; the mangle pass keeps those names unless this flag says the chunk
    /// is self-contained. With it on, a top-level name whose every use is a direct
    /// call is renamed, while an exported or observed name is still kept. The CLI
    /// exposes `--top-level` and the web demo the top-level toggle.
    pub top_level_names: bool,
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
            vendor: Vec::new(),
            manifest: Some(PathBuf::from(DEFAULT_VENDOR_MANIFEST)),
            vm: true,
            minify: true,
            optimize_svg: true,
            minify_html: true,
            minify_css: true,
            top_level_names: false,
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
            vendor: file.vendor,
            manifest: resolve_manifest(file.options.manifest),
            vm: file.options.vm.unwrap_or(true),
            minify: file.options.minify.unwrap_or(true),
            optimize_svg: file.options.optimize_svg.unwrap_or(true),
            minify_html: file.options.minify_html.unwrap_or(true),
            minify_css: file.options.minify_css.unwrap_or(true),
            top_level_names: file.options.top_level_names.unwrap_or(false),
        };
        opts.canaries.retain(|c| !c.value.is_empty());
        if opts.jobs == 0 {
            opts.jobs = default_jobs();
        }
        Ok(opts)
    }
}

/// Resolve the config's `manifest` setting to a path. Absent takes
/// [`DEFAULT_VENDOR_MANIFEST`], an empty string turns it off, any other string is
/// the path.
fn resolve_manifest(setting: Option<String>) -> Option<PathBuf> {
    match setting {
        None => Some(PathBuf::from(DEFAULT_VENDOR_MANIFEST)),
        Some(s) if s.is_empty() => None,
        Some(s) => Some(PathBuf::from(s)),
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
            "estrogen-test-{}-{}-{name}",
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

    #[test]
    fn manifest_defaults_on_and_is_configurable() {
        // Absent: the default path under dist.
        let a = tmp_file("a.toml", "");
        assert_eq!(
            Options::from_config(&a).unwrap().manifest,
            Some(PathBuf::from(DEFAULT_VENDOR_MANIFEST))
        );
        fs::remove_file(&a).ok();

        // A custom path.
        let b = tmp_file("b.toml", "[options]\nmanifest = \"map/vendor.json\"\n");
        assert_eq!(
            Options::from_config(&b).unwrap().manifest,
            Some(PathBuf::from("map/vendor.json"))
        );
        fs::remove_file(&b).ok();

        // Empty turns it off.
        let c = tmp_file("c.toml", "[options]\nmanifest = \"\"\n");
        assert_eq!(Options::from_config(&c).unwrap().manifest, None);
        fs::remove_file(&c).ok();
    }

    #[test]
    fn asset_optimization_defaults_on_and_is_configurable() {
        // All three default on.
        let d = Options::default();
        assert!(d.optimize_svg && d.minify_html && d.minify_css);
        // A config that omits them stays on.
        let a = tmp_file("osa.toml", "");
        let oa = Options::from_config(&a).unwrap();
        assert!(oa.optimize_svg && oa.minify_html && oa.minify_css);
        fs::remove_file(&a).ok();
        // A config can turn each off independently.
        let b = tmp_file(
            "osb.toml",
            "[options]\noptimize_svg = false\nminify_html = false\nminify_css = false\n",
        );
        let ob = Options::from_config(&b).unwrap();
        assert!(!ob.optimize_svg && !ob.minify_html && !ob.minify_css);
        fs::remove_file(&b).ok();
    }

    #[test]
    fn minify_defaults_on_and_is_configurable() {
        // Default on, and a config that omits it stays on.
        assert!(Options::default().minify);
        let a = tmp_file("mina.toml", "");
        assert!(Options::from_config(&a).unwrap().minify);
        fs::remove_file(&a).ok();
        // A config can turn it off.
        let b = tmp_file("minb.toml", "[options]\nminify = false\n");
        assert!(!Options::from_config(&b).unwrap().minify);
        fs::remove_file(&b).ok();
    }

    #[test]
    fn rewrite_required_defaults_true_and_count_parses() {
        let path = tmp_file(
            "rw.toml",
            r#"
[[vendor]]
from = "node_modules/epoxy"
to = "assets/epoxy"

[[vendor.rewrite]]
find = "epoxy.wasm"
to = "ln.{stamp}.wasm"

[[vendor.rewrite]]
find = "dead.wasm"
to = "x"
required = false

[[vendor.rewrite]]
find = "libcurl"
to = "xxxxxxx"
count = 2
"#,
        );
        let o = Options::from_config(&path).unwrap();
        let rw = &o.vendor[0].rewrite;
        assert_eq!(rw.len(), 3);
        assert!(rw[0].required, "required defaults to true");
        assert_eq!(rw[0].count, None);
        assert!(!rw[1].required);
        assert_eq!(rw[2].count, Some(2));
        fs::remove_file(&path).ok();
    }
}
