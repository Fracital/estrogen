# Rust API

The workspace has two crates:

- `estrogen-core`: the library. The passes (literals, names, wasm, markup), the
  shared edit-and-splice core, the vendor pipeline, and the pool and decoder
  format behind a format version.
- `estrogen-cli`: the `estrogen` binary. Argument parsing, the file walk and the
  watcher, calling `estrogen-core`.

Only `estrogen-core` is a library. `estrogen-cli` is not published to crates.io; use
the CLI or the npm package to run estrogen. The Rust surface below is for embedding
`estrogen-core` directly.

## FORMAT_VERSION

```rust
pub const FORMAT_VERSION: u32;
```

The version of the pool and decoder format estrogen emits. A consumer that decodes
estrogen output checks it against the decoder it expects. Bump it when the pool
layout or the emitted decoder changes in a way that an older decoder cannot read.

## Options

```rust
pub struct Options {
    pub names: NameMap,
    pub canaries: Vec<Canary>,
    pub sourcemap_dir: Option<PathBuf>,
    pub seed: Option<Vec<u8>>,
    pub jobs: usize,
    pub check: bool,
    pub proxy: ProxyConfig,
    pub vendor: Vec<VendorInput>,
}

impl Default for Options { /* ... */ }
impl Options {
    pub fn from_config(path: &Path) -> Result<Options, Error>;
}
```

How one run is configured: the names to rename, the canary list, the source map
directory (off when `None`), a test seed, the parallelism, the `proxy` config
(the token vocabulary, the app-shared names, the store tokens and allow-list),
and the `vendor` inputs to copy in before obfuscating. The fields mirror the
[CLI flags](CLI.md) and the
[TOML config](https://github.com/Fracital/estrogen/wiki/Configuration). Take
`Options::default()` and set fields, or load a config with `Options::from_config`.

## Obfuscator

```rust
pub struct Obfuscator { /* ... */ }

impl Obfuscator {
    pub fn new(options: Options) -> Result<Obfuscator, Error>;

    pub fn obfuscate_dir(&self, dir: &Path) -> Result<Report, Error>;
    pub fn obfuscate_dirs(&self, dirs: &[PathBuf]) -> Result<Report, Error>;
    pub fn build(&self, dist: &Path) -> Result<Report, Error>;
}
```

The entry point. `Obfuscator::new` takes an `Options`. `obfuscate_dir` rewrites
one built directory in place and returns a [`Report`](#report) or an
[`Error`](#error); `obfuscate_dirs` does several. `build` first vendors the
`Options::vendor` inputs into the dist, then obfuscates the rest. Every path
fails closed: on the first file it cannot fully process, it writes no partial
output for that file and returns an `Error` naming the file and the reason.

Files are independent and processed in parallel up to the job count in
`Options`. The pass is not idempotent; run it once per build.

## Report

```rust
pub struct Report { /* ... */ }

impl Report {
    pub fn total(&self) -> usize;
    pub fn obfuscated(&self) -> usize;
    pub fn vendored(&self) -> usize;
    pub fn unchanged(&self) -> usize;
    pub fn skipped(&self) -> usize;
    pub fn has_unprotected(&self) -> bool;
    pub fn unprotected(&self) -> Vec<&FileReport>;
}
```

What a successful run returns: one `FileReport` per file seen, each with its
path, kind, and a `FileStatus` of `Obfuscated`, `Vendored`, `Unchanged` or
`Skipped`. The methods above total them; `skipped` covers a `.map`, an
already-processed file, or a file with no pass. `has_unprotected` is true when an
eligible file was left unchanged, which a `--check` run reports as a failure.

## Error

```rust
pub enum Error {
    Io { /* ... */ },
    Config { /* ... */ },
    Parse { /* ... */ },
    Canary { /* ... */ },
    OverlappingEdits { /* ... */ },
    EditOutOfBounds { /* ... */ },
    Engine(&'static str),
    Wasm { /* ... */ },
    Proxy { /* ... */ },
}
```

The failure type the run methods return. Every variant carries enough to name
the file and the reason: a parse error (`Parse`), a store name outside the
allow-list (`Proxy`), a surviving canary literal (`Canary`), a wasm module it
cannot map (`Wasm`), or an I/O error (`Io`). The CLI prints `Display` to stderr
and exits nonzero. Match with a wildcard arm, since the engine may add a variant.

## Dependencies

`estrogen-core` parses with OXC (`oxc_parser`, `oxc_ast`, `oxc_span`,
`oxc_allocator`, and `oxc_ast_visit` for the walk). It locates edits in the AST
and splices byte ranges into the original source string; it does not reprint the
AST through a code generator. ChaCha20 is implemented in-crate so the Rust
encryptor and the emitted JS decoder run the same block function; that decoder
and the pool layout are the format contract
[`FORMAT_VERSION`](#format_version) tracks.
