# Rust API

The obfuscation engine is not implemented yet. This page documents the intended
public surface of `estrogen-core`, not current behavior. The types and the format
version below are the contract; the method bodies do not exist yet, and the
field set may grow as the passes land.

The workspace has two crates:

- `estrogen-core`: the library. The passes (literals, names, wasm, markup), the
  shared edit-and-splice core, and the pool and decoder format behind a format
  version.
- `estrogen-cli`: the `estrogen` binary. Argument parsing and the file walk, calling
  `estrogen-core`.

Only `estrogen-core` is a library. `estrogen-cli` is not published to crates.io; use
the CLI or the npm package to run estrogen. The Rust surface below is for embedding
`estrogen-core` directly.

## FORMAT_VERSION

```rust
pub const FORMAT_VERSION: u32;
```

The version of the pool and decoder format estrogen emits. It appears in every
[`Report`](#report) as `format_version`. A consumer that decodes estrogen output
checks it against the decoder it expects. Bump it when the pool layout or the
emitted decoder changes in a way that an older decoder cannot read.

## Options

```rust
pub struct Options { /* ... */ }

impl Options {
    pub fn new() -> Self;
}

impl Default for Options { /* ... */ }
```

How one pass is configured: which names to rename, the canary list, the source
map directory, the seed, and the parallelism. The fields mirror the
[CLI flags](CLI.md) and the
[TOML config](https://github.com/Fracital/estrogen/wiki/Configuration). The exact
field names are not frozen yet; build an `Options` through its constructor and
setters rather than a struct literal.

## Obfuscator

```rust
pub struct Obfuscator { /* ... */ }

impl Obfuscator {
    pub fn new(options: Options) -> Self;

    pub fn run(&self, dirs: &[std::path::PathBuf]) -> Result<Report, Error>;
}
```

The entry point. `Obfuscator::new` takes an `Options`; `run` processes the given
directories and returns a [`Report`](#report) or an [`Error`](#error). `run`
fails closed: on the first file it cannot fully process, it writes no partial
output for that file and returns an `Error` naming the file and the reason.

Files are independent and processed in parallel up to the job count in
`Options`. The pass is not idempotent; run it once per build.

## Report

```rust
pub struct Report {
    pub format_version: u32,
    pub files_processed: usize,
    pub files_skipped: usize,
    pub duration: std::time::Duration,
    pub check: bool,
}
```

What a successful `run` returns. `format_version` equals
[`FORMAT_VERSION`](#format_version). `files_skipped` counts files estrogen left
untouched (a `.map`, an already-processed file, a vendored transport asset).
`check` is true when the run was a check and wrote nothing.

## Error

```rust
pub enum Error { /* ... */ }

impl std::error::Error for Error { /* ... */ }
impl std::fmt::Display for Error { /* ... */ }
```

The failure type `run` returns. Every variant carries enough to name the file
and the reason: a parse error, a store name outside the allow-list, a proxy name
that falls across a regex escape, a wasm literal in mixed base64 alphabets, a
surviving canary literal, or an I/O error. The CLI prints `Display` to stderr
and exits nonzero. The variant list is not frozen yet, so match with a wildcard
arm.

## Dependencies

`estrogen-core` parses with OXC (`oxc_parser`, `oxc_ast`, `oxc_span`,
`oxc_allocator`, and `oxc_ast_visit` for the walk). It locates edits in the AST
and splices byte ranges into the original source string; it does not reprint the
AST through a code generator. Encryption uses a ChaCha20 crate, and the emitted
JS decoder plus the pool layout are the format contract that
[`FORMAT_VERSION`](#format_version) tracks.
