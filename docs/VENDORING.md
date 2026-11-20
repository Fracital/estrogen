# Vendoring

estrogen copies files it did not bundle into the served tree and prepares them
to ship: a whole `node_modules` package, a folder, a proxy transport runtime, a
wasm core. The `[[vendor]]` tables in the config drive it, so a site states what
to copy and how instead of keeping a hand written script for it.

This raises the cost of reading a build. It is not secrecy: a stamped name is a
neutral token, not a secret, and anyone with the served files can read them. The
point is to drop the package names from the served tree and let the app still
find what moved.

## The scripts this replaces

Every site that ships a proxy transport kept its own copy step. The shapes estrogen
generalizes:

- SystemYA's `prepare-stomp.mjs`: copy stomp's files into the output under
  stamped neutral names, rewrite the references between them, strip source maps,
  obfuscate the app code.
- Fracital: recompute a `__BUILD_STAMP__` so the app can name the stamped files,
  and `throw` when a literal it means to patch is not found.
- abstract-v2: copy transports into fixed directories and `throw` when a patched
  literal is missing.
- Holy Unblocker (`website2`) and Fracital: rename the proxy names inside a
  transport's `.wasm` with an equal length substitution, re-validate the module,
  and revert on breakage.
- powerMouse: version a whole epoxy directory by one hash segment
  (`/epoxy/<hash>/`) and leave the inner names alone, so the transport's own
  `new URL("epoxy.wasm", import.meta.url)` resolves with no rewrite.
- Ruffle: copy the player directory keeping its content hashed names, drop the
  `.map` and `.d.ts` files, and rename only the entry (`ruffle.js` to
  `player.js`).

The first three gaps below close the core those scripts share. The rest have a
plan here and land later.

## Config surface

A `[[vendor]]` table is one input:

```toml
[[vendor]]
from = "node_modules/@mercuryworkshop/epoxy-tls/full/index.js"
to = "assets/transport"
mode = "verbatim"   # or "obfuscate"
stamp = true

[[vendor.rewrite]]
find = "epoxy.wasm"
to = "ln.{stamp}.wasm"
# required defaults to true; set count to assert an exact number
```

- `from`: a file, a directory (copied recursively), or a glob
  (`node_modules/ruffle/**/*.js`). A `from` that matches nothing fails the build.
- `to`: the destination under the dist root.
- `mode`: `verbatim` (the default) copies the file as is, still source map
  stripped and canary checked, which is what a transport runtime wants.
  `obfuscate` runs the obfuscation pass over it on the way in.
- `stamp`: insert the per-build stamp before each copied file's extension, so
  `sw.js` becomes `sw.<stamp>.js` and no served file names its package.
- `rewrite`: reference rewrites applied to each copied file (see below).

### The build stamp

estrogen mints one stamp per build: eight lowercase base36 characters, reused
across every `[[vendor]]` so a reference rewritten to a stamped name matches the
file that lands there. It is drawn from OS entropy, or from `--seed` for a
reproducible build, and it never spells a canary. The stamp is in the manifest
(below) so a build-time define can read it.

## Item 1: the vendor manifest

estrogen mints the stamp internally, so without help the app cannot locate what
was renamed. The manifest exports that mapping.

`build` writes a JSON file under the dist root, `estrogen-vendor-manifest.json`
by default. Set `[options] manifest = "path/to/file.json"` to move it (relative
to dist, or absolute), or `manifest = ""` to turn it off. In `--check` mode
nothing is written. The manifest is written after the obfuscation walk, so it is
never itself obfuscated.

```json
{
	"stamp": "k3x9mb2a",
	"files": {
		"assets/transport/index.js": "assets/transport/index.k3x9mb2a.js",
		"assets/transport/epoxy.wasm": "assets/transport/epoxy.k3x9mb2a.wasm"
	}
}
```

- `stamp`: the raw per-build stamp. A build-time define (Vite `define`,
  `__BUILD_STAMP__`) reads it, so app code can build a stamped name itself.
- `files`: each vendored file's public path mapped to its served path. The
  public path is what the file would be named without the stamp, so it is a
  stable key the app knows at author time; the served path is where the bytes
  landed. Both are relative to the dist root with forward slashes. For an input
  that does not stamp, the two are equal. The map is sorted, so the file is byte
  for byte stable for a given build.

The app reads the manifest at build time (the asset map SystemYA injects) or at
runtime (fetch it once, then resolve names through it).

Rust API: `Options.manifest` is `Option<PathBuf>`. `None` writes no manifest.
`Obfuscator::build` returns the usual `Report`; the manifest is a side effect of
the build, keyed off `Options.manifest`.

## Item 2: rewrite assertions

A rewrite finds text in a copied file and replaces it, expanding `{stamp}` in
the replacement. The danger is silence: if a package reshape drops the literal a
rewrite looks for, a plain find and replace does nothing and the build ships a
dangling reference. Every reference script guards against this by throwing when a
patched literal is missing.

Two fields on a rewrite make it a tripwire:

- `required` (bool, default `true`): the build fails when `find` matches nothing
  across the input's files. The default is `true` because a missing target is
  almost always a reshape to catch, not an intended no-op. The failure names the
  input and the literal and says to fix the `find` or set `required = false`.
- `count` (optional integer): the build fails unless `find` matches exactly this
  many times across the input's files. `count = 0` asserts the literal is
  absent. When `count` is set it governs; otherwise `required` does.

The check is per input, not per file. A transport's reference usually lives in
one loader file while the others carry none, so estrogen sums each rewrite's
matches across all the input's files and checks the total once. A rewrite that
targets an optional fallback, such as the dead `new URL("epoxy.wasm")` an inline
wasm transport leaves in its source, sets `required = false` so its absence is
fine (see item 7).

The assertion runs in `--check` too, so CI catches a reshape without writing
anything.

## Item 3: wasm-aware rewrites

A transport's proxy names can live inside its `.wasm`: in import and export
names, data segments, and the custom `name` section. A blind text replace over a
`.wasm` corrupts it, because the bytes that spell a name also appear in code, in
LEB128 section lengths, and in constant expressions, and because a replacement
of a different length shifts every following offset.

estrogen already renames proxy names in wasm safely for the proxy pass, in
`crates/estrogen-core/src/wasm.rs`: `wasm_name_ranges` walks the module and
returns only the byte ranges that hold names or data, and `map_wasm_bytes`
rewrites those ranges with a substitution, requires each replacement to keep its
range's length, and fails closed otherwise. Vendored wasm rewrites reuse that
code. `vendor::apply_wasm_rewrites`:

1. Routes a `.wasm` by extension, never by whether its bytes decode as UTF-8, so
   a module whose bytes happen to be valid UTF-8 is never text rewritten.
2. Counts each rewrite's `find` inside the name and data ranges only (from
   `wasm_name_ranges`), so a name spelled by chance in the instruction stream is
   not counted and the item 2 assertion measures real names.
3. Applies the rewrites through `map_wasm_bytes`, which edits only the name and
   data ranges and enforces equal length. A rewrite whose `to` differs in length
   from its `find` fails the build instead of shipping a corrupt binary, so a
   wasm rename must be a same length substitution.
4. Re-validates: it walks the mapped module again and fails closed if the walk
   does not succeed. estrogen never writes a module it cannot re-walk.

This matches what Fracital and Holy Unblocker do by hand (`rewriteWasmGlobals`:
equal length, re-validate, revert).

A base64 inline wasm is a different case; see item 7.

## Planned work

The items below have a design but are not built yet. Each names the config
surface, the algorithm, and the site that needs it, so a follow up can implement
one at a time.

### Item 4: directory atomic versioning

Some transports load their own files by a relative URL the source already has,
such as `new URL("epoxy.wasm", import.meta.url)`. Stamping each file inside the
directory would break that URL. The fix is to version the whole directory by one
hash segment and leave the inner names alone, so the relative URL still resolves
with no rewrite.

- Who needs it: powerMouse serves epoxy from `/epoxy/<hash>/` with verbatim inner
  names. abstract-v2 uses fixed directories for the same reason.
- Config: a new per input `stamp` mode. Today `stamp` is a bool; widen it to
  accept `"file"` (the current behavior, stamp each file's name), `"dir"`
  (insert one `<stamp>` path segment at the destination root and keep inner names
  verbatim), or `false`. A string keeps the bool forms working through a serde
  untagged or custom deserialize.
- Algorithm: for `stamp = "dir"`, compute the destination as
  `to/<segment>/<relative path>` where `<segment>` is the build stamp (or a
  content hash of the directory, decided by a `segment` sub option: `stamp` or
  `hash`). Record one manifest entry for the directory,
  `to/` mapped to `to/<segment>/`, plus the per file entries under it. No text or
  wasm rewrite is needed, which is the point.

### Item 5: per-input naming policy

One input often holds files that each want a different naming rule. Today `stamp`
is all or nothing per input.

- Who needs it: a service worker whose `importScripts` targets must keep fixed
  names; Ruffle and Monaco files that are already content hashed and should pass
  through unchanged; an entry file that should be renamed once (`ruffle.js` to
  `player.js`).
- Config: a `naming` list on the input, each rule a glob and a policy:

  ```toml
  [[vendor.naming]]
  match = "sw.js"
  policy = "fixed"            # keep the name as is

  [[vendor.naming]]
  match = "**/*.wasm"
  policy = "stamp"            # insert the build stamp

  [[vendor.naming]]
  match = "ruffle.js"
  policy = "rename"
  to = "player.js"           # rename the one entry

  [[vendor.naming]]
  match = "**/*.js"
  policy = "hash"            # content hash, for already hashed vendors
  ```

- Algorithm: resolve each copied file against the `naming` rules in order, first
  match wins, falling back to the input's `stamp` default. `fixed` keeps the
  name, `stamp` inserts the build stamp (today's behavior), `hash` inserts a
  short content hash of the file, `rename` sets an exact name. Every rule records
  its own manifest entry, so a rewrite or the app can still find the result.

### Item 6: file synthesis

Some transports need a small generated file built from the stamped names, which
no copy can produce.

- Who needs it: Ultraviolet's `uv.config.js` points at the stamped handler and
  client file names; a loader shim that imports the stamped entry.
- Config: a `[[synthesize]]` table, a template plus the output path:

  ```toml
  [[synthesize]]
  to = "assets/uv/uv.config.js"
  template = "self.__uv$config = { handler: '{manifest:assets/uv/uv.handler.js}' };"
  ```

- Algorithm: after vendoring, expand the template. `{stamp}` expands to the build
  stamp; `{manifest:<public path>}` expands to that file's served path from the
  manifest, so a synthesized file names the stamped targets. Write the result,
  canary check it, and record it in the manifest. A `{manifest:...}` reference to
  a path no input produced fails the build, the same tripwire as item 2.

### Item 7: inline wasm tolerance

epoxy and libcurl inline their wasm as a base64 string, so there is no sibling
`.wasm` file, and the source keeps a dead `new URL("epoxy.wasm", import.meta.url)`
fallback that never runs. A `required` rewrite aimed at that fallback would fail
the build because the literal is genuinely absent from the served path.

- Who needs it: epoxy and libcurl builds that inline their wasm.
- Config: no new field. This is why item 2 gives `required = false`. A rewrite
  for the dead fallback sets `required = false` and its absence is fine.
- Algorithm: for the inline base64 itself, reuse `wasm::map_wasm_base64` from the
  proxy pass, which decodes the literal, maps the name and data ranges at equal
  length in the literal's own alphabet and padding, and returns `None` when the
  literal is not a wasm module or nothing changed. A future `rewrite` option
  `wasm_base64 = true` would route a text rewrite's `find` through that path
  instead of a plain string replace, so the inline module is mapped safely while
  the surrounding JavaScript is left alone.

### Item 8: Ruffle passthrough

Ruffle ships a directory of already content hashed files plus `.map` and `.d.ts`
files that must not reach the served tree, and one entry to rename.

- Who needs it: the Ruffle player.
- Config: an `exclude` list on the input, plus the item 5 `naming` rules:

  ```toml
  [[vendor]]
  from = "node_modules/@ruffle-rs/ruffle"
  to = "assets/ruffle"
  stamp = false
  exclude = ["**/*.map", "**/*.d.ts"]

  [[vendor.naming]]
  match = "ruffle.js"
  policy = "rename"
  to = "player.js"
  ```

- Algorithm: resolve the directory, drop every file matching an `exclude` glob,
  keep the content hashed names (the input's `stamp = false` default), and apply
  the `naming` rename to the entry. Record each kept file in the manifest,
  including the renamed entry, so the app loads `player.js`.

## Verification

The vendor pipeline is tested in
`crates/estrogen-core/src/{vendor.rs, obfuscator.rs}`:

- the manifest maps each public path to its stamped served path and carries the
  stamp (`build_writes_a_vendor_manifest`, `manifest_maps_public_to_served_relative_to_dist`);
- a required rewrite that misses fails the build with a clear error
  (`build_fails_when_a_required_rewrite_misses`, `a_required_rewrite_that_misses_fails_with_a_clear_error`),
  and `count` asserts an exact number (`a_count_asserts_the_exact_number`);
- a vendored wasm rewrite edits only the name ranges, keeps the module length,
  and re-walks (`build_rewrites_a_vendored_wasm_in_its_name_range`,
  `wasm_rewrite_edits_only_the_name_range_and_revalidates`), and an unequal
  length rewrite fails closed (`wasm_rewrite_of_unequal_length_fails_closed`).
