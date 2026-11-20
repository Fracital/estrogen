# estrogen development handoff

This is the starting brief for building estrogen, a fast web-build obfuscator in
Rust on OXC, published to npm as `@sysce/estrogen`. It records what is decided, what
exists, what the tool has to do, and where the behaviour it copies already lives.
Read it before writing code, and keep it current as decisions change.

## What estrogen is for

The Fracital sites each obfuscate their build output with their own Node
scripts. The scripts drifted apart and are slow on large bundles. estrogen is one
binary that does that work for every site, fast enough to run on every build and
in CI without being the slow step.

It takes a built site directory and rewrites the JS, HTML and SVG in place:
string and template literals into an encrypted per-file pool, the proxy and
store names into per-build substitutes, and the embedded wasm names to match. It
leaves a build with no sensitive plaintext from the canary list, and it does not
change what the code does.

The limit is the same one the existing scripts state: this raises the cost of
reading the build. It is not secrecy. A key ships in each file, and anyone with
devtools can run the code. Say so in the README and do not describe the output as
secret.

## Decided

- Repo: `Fracital/estrogen`, public. Checked out at `~/src/estrogen`, remote
  `github2:Fracital/estrogen.git` (the `github2` SSH alias, now the `haveajew`
  account).
- License: ISC. Author credit: David Reed, with the GitHub avatar, in the README
  and the crate authors.
- npm: `@sysce/estrogen`, scoped public. The npm login is `e9x` (separate from the
  GitHub account). Publishing uses npm trusted publishing (OIDC from the repo's
  Actions), so the package has to exist before the first trusted publish.
- Engine: OXC (https://oxc.rs) for the parser and AST. Rust toolchain 1.100 is
  installed under `~/.cargo` and `~/.rustup`.
- Consumers, which get a "Powering" section in the README (brand icon in the
  first column, then a link to the site and to its repo):
  - abstract v2, private, abstracteducation.loan
  - Fracital, fracit.al
  - SystemYA, systemya.fracit.al
  - STomp (powerMouse), stomp.fracit.al
  - Holy Unblocker, holyunb.locker

## README and credits

The README credits David Reed, with the GitHub avatar
(`docs/assets/am i cool yet.png`), matching the repo owner and the crate authors.

It carries a "Powering" table: the brand icon in the first column, then a link to
the site, then a link to its repo.

- abstract v2, abstracteducation.loan, `Fracital/abstract-v2` (both private)
- Fracital, fracit.al, `Fracital/Fracital`
- SystemYA, systemya.fracit.al, `Fracital/SystemYA`
- STomp (powerMouse), stomp.fracit.al, `Fracital/powerMouse`
- Holy Unblocker, holyunb.locker, `holy-unblocker/website2`

State the limit plainly in the README: estrogen raises the cost of reading a build,
it does not make it secret, and a key ships in each file.

## Repo state now

The obfuscation engine is built and tested against the reference, editing the
source by byte range with no AST reprint:

- `crates/estrogen-core`: the engine. The edit-and-splice core, the file walk
  and eligibility gate, config loading (`Options`, with the `[proxy]` table), and
  the passes: the ChaCha20 string and template pool with the emitted decoder
  (`chacha.rs`, `keys.rs`, `format.rs`, `pass/literals.rs`), the proxy and store
  name transform (`proxy.rs`, `pass/names.rs`), the wasm name and base64 mapping
  (`wasm.rs`), and the HTML and SVG markup pass (`pass/markup.rs`). Each JS file
  takes one transform: the proxy path for vendor-glob files, else the literal
  pool. The obfuscator re-parses its output and fails closed, and scans it for
  surviving canaries.
- `crates/estrogen-cli`: the `estrogen` binary. `estrogen [<dir>...]` plus
  `--config`, `--canary`, `--names`, `--sourcemap`, `--seed`, `--jobs`,
  `--check` and `--watch`.
- `npm/estrogen`: the launcher and per-target packages, and the esbuild,
  rsbuild, Vite and Rollup plugins.

Still open: source map V3 emission behind `--sourcemap` (the flag parses but
emission is not written yet, so it fails closed), the `--watch` watcher, and the
vendoring pipeline (the roadmap below). `target/` is build output and is
gitignored.

## What the tool has to do

The behaviour is defined by the Node scripts the sites already ship. Treat these
as the specification. Port their rules, do not invent new ones, and check estrogen
against their output on the same input.

The canonical implementation is Fracital's `~/src/Fracital/scripts/`:

- `obfuscate-build.mjs`: the whole post-build pass. Its header comment lists the
  exact positions that get encrypted and the ones that must stay static
  (directive prologues, import and export specifiers, import attributes, a
  `constructor` method key, a `__proto__` proto key, tagged template quasis).
  The same file holds the proxy-name renaming in strings, regex literals and
  vendored markup, the store-name guard, and the wasm name and base64 handling.
- `check-plaintext-leaks.mjs` and `canary-list.json`: the canary check that must
  pass at exit 0, and the list of literals that must not survive verbatim.
- `obfuscate-build.test.mjs`: the behaviour tests, including that the shipped
  decoder is byte-for-byte symmetric with the encryptor.
- `package.json` `protect` script: the order the passes run in a real build.

Two more sources for the asset and name layer:

- SystemYA `~/src/SystemYA/scripts/prepare-stomp.mjs` and
  `prepare-powermouse.mjs`: per-build stamped asset names (`sw.<stamp>.js` and
  the rest), the rule that no served file names its package, and the wasm-name
  rewrite inside the transports.
- SystemYA `~/src/SystemYA/src/systemya/obfuscate.ts`: the 2022 CSS text
  obfuscation (letters split across `<s>` elements, junk hidden by inserted
  rules). This one is page-specific and may stay out of estrogen; decide with the
  user.

Out of scope for the first version, unless the user asks: the main app's
bytecode shield VM (`~/src/abulxt/main/src/server/shield/`). That compiles chunks
to encrypted bytecode and is a different, heavier mechanism than the string pool.
Keep estrogen to the string, name, asset and markup layer first.

### Which files estrogen touches

- JS: the app chunks and inline scripts the site serves. `shouldProcessJs` in
  `obfuscate-build.mjs` is the gate; port it. Vendored transport assets (epoxy,
  libcurl, the STomp runtime) are not re-obfuscated, but they are still
  canary-checked, because no sensitive plaintext of ours may ride in them.
- HTML shells and SVG cloaks, as the `obfuscate-build.mjs` header describes.
- Never a `.map` file, and never a file estrogen has already processed.
- Input is UTF-8. Keep a byte-order mark if the file has one. Non-ASCII inside a
  literal round-trips through the pool unchanged.

### Fail closed

The Node scripts throw on anything they cannot handle: a parse error, a store
name not in the allow-list, a proxy name that falls across a regex escape, a wasm
literal in mixed base64 alphabets. estrogen does the same. It exits nonzero, writes
no partial output for that file, and names the file and the reason. A build that
cannot be fully protected fails the build; it never ships half done.

### Cipher parity

`obfuscate-build.mjs` encrypts the string pool with ChaCha20 (RFC 8439) and
ships a compact vanilla ChaCha20 decoder in each file. estrogen has to emit a
decoder that decodes estrogen's own pool. The Rust side can use any ChaCha20 crate
for encryption, but the emitted JS decoder and the pool format are the contract,
and the symmetry test from `obfuscate-build.test.mjs` has to pass against
estrogen's output.

## Build and distribution

- CLI: `estrogen <dir> [dir ...]`, matching `obfuscate-build.mjs`'s argument shape,
  plus flags for the parts that differ per site (which names to rename, the
  canary list path). Settle the flag set against how each site calls the current
  scripts.
- Cross-platform native binaries, built in CI: linux x64 and arm64, macOS x64
  and arm64, windows x64 at least. The release profile is already set for small
  binaries.
- npm: the `@sysce/estrogen` package ships the platform binaries as optional
  dependencies (one package per target, `os`/`cpu` gated), with a thin launcher
  that runs the right one. This is the pattern esbuild and swc use; follow it
  rather than a postinstall download.
- CI publishes on a tag, through npm trusted publishing. The package record has
  to exist first, so the first publish is a manual `npm publish` of an empty or
  0.0.x placeholder as `e9x`, then CI takes over.

## Development guide

### Edit the source text, do not reprint the AST

This is the decision that shapes everything else. `obfuscate-build.mjs` parses
with acorn, walks the tree to collect edits as `{start, end, replacement}` byte
ranges, and splices them into the original source string (`collectEdits` then
`applyEdits`). It never reprints the AST. That keeps every byte the obfuscator
does not touch exactly as the bundler emitted it: formatting, comments, the
decoder it prepends, the lot.

estrogen does the same. Use `oxc_parser` for the AST and `oxc_span` offsets, walk to
collect byte-range edits, sort and apply them against the original source. Do not
run `oxc_codegen` over the whole file. A reprint changes bytes estrogen must leave
alone and can reshape a literal in a way the canary check or the runtime does not
expect. The AST locates; the source string is what you rewrite.

One offset note: acorn offsets are UTF-16 code units, OXC spans are UTF-8 byte
offsets. estrogen splices bytes, which is consistent with OXC, so port the logic by
position role, not by copying the numbers.

### Useful OXC crates

`oxc_allocator`, `oxc_parser`, `oxc_ast`, `oxc_span`, and `oxc_ast_visit` for the
walk. Skip `oxc_codegen` for the reasons above. Pin exact versions and keep
`Cargo.lock` committed; this is a binary crate, so the lock is part of the build.

### Build in milestones, check parity at each

Before the passes, get the scaffold green: `cargo build` and `cargo test` pass in
CI across the targets, and the npm launcher resolves and execs a stub binary.
That proves the build and publish path before the hard part.

Build a parity harness first: run the Node script and estrogen on the same built
dir into two output copies and diff them. The goal is identical output, then
identical-enough (estrogen may order the pool differently, so compare runtime
behaviour and the canary result, not bytes, once they diverge on purpose).

1. Parse every eligible chunk and write it back unchanged. Diff is empty. This
   proves the walk and the splice are sound before anything is rewritten.
2. The string and template literal pool, with the ChaCha20 decoder. The
   decoder-symmetry test from `obfuscate-build.test.mjs` passes against estrogen's
   output.
3. Proxy-name renaming in strings, regex literals and vendored markup. The
   store-name guard.
4. The wasm name ranges and base64 handling.
5. The HTML shells and the SVG cloak (entity decode and CDATA, per the
   `obfuscate-build.mjs` header).

First acceptance: `estrogen dist` on a Fracital build passes that site's
`check-plaintext-leaks.mjs` at exit 0, and the site boots in a browser.

### Determinism and the parity harness

The pass uses per-file randomness: the ChaCha20 key, and per build the stamped
names and helper globals. For the harness and for CI, make that randomness
seedable, through a `--seed` or a fixed-key test mode, so two runs on one input
give the same bytes and estrogen's output diffs against a pinned expectation.
Production builds use real randomness; the seed is for tests.

Check by decoding, not by shape alone. In CI, run the emitted JS decoder against
estrogen's pool in Node or V8 and assert the decoded strings equal the originals. A
pool that parses but decodes wrong is the failure the symmetry test exists to
catch.

### Speed

Speed is why estrogen exists, so measure it against the Node scripts on a real
build. Files are independent, so process them in parallel (rayon). Run the whole
pass once over the dir, not once per file from a shell. The pass is not
idempotent, because a second run would encrypt the decoder the first run added,
so it runs exactly once in each site's pipeline. The `protect` script in
`package.json` is the reference order.

### Source maps

magic-string is the JS model for this: it records every edit against the
original string and emits a V3 source map from them. estrogen already edits by byte
range, so it holds the same information and can build a map the same way
(`oxc_sourcemap` emits V3, or generate the mappings from the edit list).

A map that traces obfuscated output back to its source is the one thing the
obfuscation is meant to withhold, so the default protects it:

- estrogen writes no map next to the served build, and strips any
  `//# sourceMappingURL` and `//@ sourceMappingURL` comment from every chunk it
  processes. The bundler's own maps point at the pre-obfuscation source and must
  never ship. `prepare-stomp.mjs` strips that comment today; estrogen takes the job
  over.
- A build that wants a map for private debugging gets it behind `--sourcemap
  <dir>`, off by default, writing outside the served tree. That directory stays
  private.
- Edge backstop, in infra: every consumer host returns 403 for `*.map` and for
  any `sourceMappingURL` target, so a map that reaches a published dir by
  accident is still never served. This is a rule in the edge config (the infra
  repo's edge role, and amph's OpenResty), not in estrogen. It is a required
  integration step and a config-parity item, not an afterthought.

### CLI and tests

Match `obfuscate-build.mjs`'s argument shape (`estrogen <dir> [dir ...]`). Add flags
only for what differs per site, and settle them against how each site's
`package.json` calls the current scripts. Keep Rust unit tests next to the
passes, and keep the parity harness runnable in CI.

## Repo layout to grow into

- `crates/estrogen-core`: the passes and the decoder emitter. One module per pass
  (literals, names, wasm, markup), a shared edit-and-splice core, and the pool
  and decoder format in one place behind a format version.
- `crates/estrogen-cli`: argument parsing and the file walk, calling core.
- `npm/`: the launcher package (`@sysce/estrogen`) and the per-target binary
  packages. The launcher resolves the right optional dependency and execs its
  binary.
- `.github/workflows/`: a build-and-test matrix across the targets, and a
  tag-triggered publish through trusted publishing.
- `docs/`: this handoff and the design notes as they settle.
- Parity fixtures: small built dirs, real or synthetic, one per site shape.

## Integration, once estrogen works

Each site swaps its obfuscate step for a `estrogen` call and keeps its canary check.
Do one site end to end first (Fracital, since estrogen copies its scripts), confirm
the output passes its own `check-plaintext-leaks.mjs` and the site runs, then the
others. STomp's build is where SystemYA pulls its runtime from, so changing
STomp's obfuscation reaches SystemYA through the nightly; coordinate that with
the STomp session rather than editing `~/src/stomp-rewrite` directly.

## Roadmap: estrogen as the build glue

The first version obfuscates an already-built dir. The next step is to make estrogen
the one build step that assembles the served tree and obfuscates it in the same
pass, so each site deletes its hand-written copy scripts. This is additive: the
obfuscation engine stays the core, and vendoring calls into it per input.

### Vendoring and copy

estrogen can copy packages and folders into the dist output, including whole
node_modules packages, driven by config. Each input is one declared step: where
it comes from, where it lands, and whether it is obfuscated on the way in or
copied as is. This replaces the manual copy step each site maintains, and it
generalizes what SystemYA does by hand in
`~/src/SystemYA/scripts/prepare-stomp.mjs` and `prepare-powermouse.mjs`: copy a
package's dist into the served tree under stamped neutral names, rewrite the wasm
references, strip source maps, and obfuscate. Read those as the prior art.

The config is TOML, in the same file as the obfuscation options. Each input is a
`[[vendor]]` block:

```toml
[[vendor]]
from = "node_modules/ruffle"   # a package dir, a folder, or a glob
to   = "assets"                # destination under the dist root
mode = "verbatim"              # or "obfuscate"; transports stay verbatim
stamp = true                   # insert a per-build stamp before the extension

  [[vendor.rewrite]]           # optional reference rewrites, e.g. wasm names
  find = "epoxy.wasm"
  to   = "ln.{stamp}.wasm"
```

The stamp is minted once per build and exposed to rewrites as `{stamp}`. The rule
that no served file names its package is the stamp plus a neutral name. Source
map stripping is already estrogen's job, so vendored files get it too.

### Public API growth

Vendoring grows the library API, additively:

- `Options` gains `vendor: Vec<VendorInput>`, the parsed `[[vendor]]` blocks.
- `Obfuscator::build(dir)` runs vendor-then-obfuscate for a dist. `obfuscate_dir`
  stays obfuscate-only, so its existing callers are untouched.
- `Report` gains entries for vendored files, alongside the obfuscated ones.

`FORMAT_VERSION`, `Error`, and the existing `obfuscate_dir` path keep their
shape. The npm package and the plugins mirror these, so a change here reaches the
consumers; announce the shape when it lands.

### Build system integrations

The esbuild and rsbuild plugins exist. Add a real Vite plugin, not a post-build
shell step, and a Rollup plugin, and keep the core API clean so webpack, Parcel
and a plain CLI can wrap it. A plugin runs the vendor and obfuscate step as part
of the build, so a project adds one plugin and deletes its copy scripts.

### Watcher

estrogen watches the declared input folders and re-runs the copy, vendor and
obfuscate work for what changed. Each builder integration hooks the builder's dev
server so the change reloads: Vite through `configureServer` and
`handleHotUpdate`, Rollup through `watchChange`. A standalone `estrogen --watch
<config>` runs outside a bundler too. The pass is not idempotent, so the watcher
re-runs from the pre-obfuscation source each time, never over its own output.

### CLI

`estrogen [<dir>...]` takes one or more output dirs, because the obfuscate-only path
processes several in one go (Fracital builds aws, jsdelivr and singlefile
together). Vendoring is config-driven and applies to the dist roots given.
`estrogen --watch <config>` runs the watcher. The Node option stays plural,
`dirs: string | string[]`. Plugins call the programmatic API with the builder's
resolved output dirs, not the CLI, so the CLI and the plugin path stay separate.

### Sequencing

This lands after the obfuscation engine core (the string, name, wasm and markup
passes). The vendor pipeline calls into that core per input, so the core API is
frozen first, then the vendoring work fans out: the vendor and copy pipeline, the
stamping and reference rewrite, the Vite and Rollup plugins, the watcher, and the
declarative port of the SystemYA prepare scripts as the first real config.

## Open questions for the user

- Does estrogen cover the 2022 CSS text obfuscation (`obfuscate.ts`), or is that
  SystemYA's alone?
- Is the bytecode shield VM ever an estrogen target, or does it stay in the main app?
- The exact flag set and config format: per-site flags, a config file, or both.
- Windows arm64 and any other target beyond the five above.

## Boundaries for whoever builds this

- Obfuscation build tooling only. The output raises reading cost; it is not
  secrecy, and the README says so.
- No AI attribution in commits, the README or anywhere else. Commits are David
  Reed's, signed, subject-then-body wrapped at 72 columns.
- Commit and push only when the user asks in that turn.
- Docs and README follow the no-slop rules in abulxt's memory
  (`anti-ai-writing-patterns.md`): sentence case headings, plain verbs, concrete
  names, no em dashes, run the grep before reporting.
