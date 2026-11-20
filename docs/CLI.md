# CLI reference

The obfuscation engine is not implemented yet. This page documents the stable
command surface, not current behavior. The flags and exit codes below are the
contract the rest of the tooling builds against.

## Synopsis

```
estrogen [options] <dir>...
```

estrogen takes one or more already-built directories and rewrites the JS, HTML and
SVG in each, in place. It does not run a bundler; it only rewrites output a
bundler produced. Give it the directory your build wrote, such as `dist`.

```sh
estrogen dist
estrogen dist public/app
```

Multiple directories are processed in one pass. Running the whole pass once over
a directory is the intended use; do not call `estrogen` once per file from a shell.

## Options

`-c`, `--config <file>`
: Path to a TOML config file. The config sets the names to rename, the canary
list, and the obfuscation options. A flag given on the command line overrides
the same setting in the file. See the
[Configuration](https://github.com/Fracital/estrogen/wiki/Configuration) wiki
page for the file shape.

`--canary <file>`
: Path to the canary list (a JSON file of literals that must not survive in the
output verbatim). After the rewrite, estrogen checks every processed file against
this list and fails if any listed literal is still present. Overrides the
canary path from `--config`.

`--names <file>`
: Path to the name map (the proxy and store names to rename, and the allow-list
the store-name guard checks against). Overrides the names path from
`--config`.

`--sourcemap <dir>`
: Write source maps into `<dir>`, outside the build directory. Off by default:
with no `--sourcemap`, estrogen writes no map and strips any
`sourceMappingURL` comment from every chunk it processes. The directory you
pass stays private and must not be the served tree. See
[Source maps](https://github.com/Fracital/estrogen/wiki/Source-maps).

`--seed <hex>`
: Seed the per-file and per-build randomness from a fixed hex value, so two runs
on the same input produce the same bytes. **For tests and the parity harness
only.** Production builds use real randomness; a shipped build must never pass
`--seed`.

`-j`, `--jobs <n>`
: Process up to `n` files in parallel. Files are independent. The default is the
number of available CPUs.

`--check`
: Check only. Report what would change and run the canary check without writing
any file. Exit nonzero if any file would fail. Use it in CI to confirm a
directory is clean without mutating it.

`--quiet`
: Suppress progress output. Errors still print to stderr.

`-v`, `--verbose`
: Print per-file detail: which files were processed, which were skipped, and
why.

`--version`
: Print the version and exit.

`-h`, `--help`
: Print usage and exit.

## Exit codes

`0`
: Every directory was processed and every processed file passed the canary
check.

nonzero
: Something failed. estrogen prints the file and the reason to stderr. A directory
argument that does not exist, a parse error, a store name outside the
allow-list, a canary literal that survived, or an I/O error all exit nonzero.

The exact nonzero values are not settled yet. Treat any nonzero exit as failure;
do not branch on a specific number.

## Fail closed

estrogen fails closed. If any file in a directory cannot be fully processed, the
run fails: estrogen writes no partial output for that file, names the file and the
reason on stderr, and exits nonzero. A build that cannot be fully protected
fails the build. It never ships half done.

This matches the Node scripts estrogen replaces, which throw on anything they
cannot handle: a parse error, a store name not in the allow-list, a proxy name
that falls across a regex escape, or a wasm literal in mixed base64 alphabets.

## What estrogen does and does not touch

estrogen rewrites the app chunks and inline scripts a site serves, the HTML shells,
and the SVG cloaks. It never rewrites a `.map` file, and never a file it has
already processed. Vendored transport assets (such as epoxy, libcurl, and the
STomp runtime) are not re-obfuscated, but they are still canary-checked, so no
sensitive plaintext of ours rides in them.

Input is UTF-8. estrogen keeps a byte-order mark if the file has one, and non-ASCII
inside a literal round-trips through the encrypted pool unchanged.
