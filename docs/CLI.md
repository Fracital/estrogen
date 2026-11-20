# CLI reference

## Synopsis

```
estrogen [options] <dir>...
estrogen build [options] <dir>...
```

estrogen takes one or more already-built directories and rewrites the JS, HTML and
SVG in each, in place. It does not run a bundler; it only rewrites output a
bundler produced. Give it the directory your build wrote, such as `dist`.

`estrogen build` first vendors the `[[vendor]]` inputs from the config into each
directory, copying packages and folders in under per-build stamped names and
obfuscating them or copying them verbatim, then obfuscates the rest. A plain
`estrogen` run vendors nothing.

`estrogen build` also writes a vendor manifest, `estrogen-vendor-manifest.json`
under each directory, mapping each vendored file's public path to its stamped
served path and carrying the build stamp, so the app can find what was renamed.
Set `[options] manifest` in the config to move or disable it. See
[Vendoring](VENDORING.md) for the config surface, the manifest shape, the
rewrite assertions (`required` and `count`), and how a vendored `.wasm` is
rewritten safely.

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

`--watch`
: Watch the given directories and re-run on each change, debounced. Works with
`estrogen` and `estrogen build`. estrogen re-obfuscates only the fresh chunks a
bundler wrote and skips the ones it already marked, so its own output is never
re-wrapped. A failed pass is reported and the watch keeps running.

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

estrogen does not promise a specific nonzero value. Treat any nonzero exit as
failure; do not branch on a specific number.

## Fail closed

estrogen fails closed. If any file in a directory cannot be fully processed, the
run fails: estrogen writes no partial output for that file, names the file and the
reason on stderr, and exits nonzero. A build that cannot be fully protected
fails instead of shipping part of the output.

This matches the Node scripts estrogen replaces, which throw on anything they
cannot handle: a parse error, a store name not in the allow-list, a proxy name
that falls across a regex escape, or a wasm literal in mixed base64 alphabets.

## What estrogen does and does not touch

estrogen rewrites app chunks, inline scripts, HTML and SVG. App scripts get a
literal pool and shorter bindings. In a file with `import` or `export` syntax
(so it is a module, not a script whose top-level names may be globals) estrogen
also renames top-level bindings and the binding behind an export, rewriting the
statement to keep the public name: `export { proxyRouteCookie }` becomes
`export { a as proxyRouteCookie }`. The public export name and property keys stay
the same at runtime; a public export name cannot be hidden, because it must be a
literal in the module's static export list. Import bindings and classic-script
top-level names are left alone. See `docs/TOPLEVEL-MANGLE.md`. Transport chunks
take the separate vendor pass.

A leading `/* generated by https://estro.help */` marks JavaScript as processed.
HTML and SVG use `<!-- generated by https://estro.help -->`. estrogen also
recognizes the previous Fracital header and skips `.map` files.

Input is UTF-8. estrogen keeps a byte-order mark if the file has one, and non-ASCII
inside a literal round-trips through the encrypted pool unchanged.
