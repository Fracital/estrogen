# Node API

The obfuscation engine is not implemented yet. This page documents the stable
JavaScript surface, not current behavior. The import paths, function signatures,
options object, and `Report` shape below are the contract the integrations build
against.

The package is `@sysce/crack`. It ships the native binary as a per-target
optional dependency and a thin launcher that runs the right one. The Node API
calls that binary over a built directory.

## obfuscate

```js
import { obfuscate } from '@sysce/crack'

const report = await obfuscate({ dirs: ['dist'] })
```

`obfuscate(options): Promise<Report>` runs the whole pass over one or more
directories and resolves with a [`Report`](#report). It rejects if any file
fails (the same fail-closed rule as the CLI): no partial output is written for a
failing file, and the error names the file and the reason.

## esbuild plugin

```js
import { build } from 'esbuild'
import crack from '@sysce/crack/esbuild'

await build({
  entryPoints: ['src/index.ts'],
  bundle: true,
  outdir: 'dist',
  plugins: [crack()],
})
```

The default export of `@sysce/crack/esbuild` is `crack(options?):
esbuild.Plugin`. The plugin runs after the build finishes, over the build's
output directory (`outdir`, or the directory of `outfile`). Pass `options` to
override what crack would otherwise infer; see [Options](#options). If a file
fails, the build fails.

## rsbuild plugin

```js
import { pluginCrack } from '@sysce/crack/rsbuild'

export default {
  plugins: [pluginCrack()],
}
```

`pluginCrack(options?): RsbuildPlugin` is a named export of
`@sysce/crack/rsbuild`. The plugin runs after the build finishes, over the
resolved dist path. Pass `options` to override the inferred settings. A failing
file fails the build.

## Options

The same options object is accepted by `obfuscate()` and, as overrides, by both
plugins. Every field is optional for the plugins, which infer `dirs` from the
build; `obfuscate()` needs at least `dirs`.

`dirs: string[]`
: The built directories to process. For the plugins this defaults to the build's
  output directory.

`config?: string`
: Path to a TOML config file. Fields set directly on the options object override
  the file.

`canary?: string`
: Path to the canary list (JSON). Overrides the canary path from `config`.

`names?: string`
: Path to the name map. Overrides the names path from `config`.

`sourcemap?: string`
: Write source maps into this directory, outside the build. Omitted means no map
  and `sourceMappingURL` comments stripped. Keep this directory private and out
  of the served tree.

`seed?: string`
: Fixed hex seed for deterministic output. **Tests only.** Never set it on a
  shipped build.

`jobs?: number`
: Maximum files processed in parallel. Defaults to the CPU count.

`check?: boolean`
: Check only. Run the passes and the canary check without writing any file. The
  returned `Report` says what would change. A file that would fail rejects the
  promise.

`quiet?: boolean`
: Suppress progress output.

`verbose?: boolean`
: Emit per-file detail.

## Report

`obfuscate()` resolves with a `Report`. The shape is part of the stable surface;
the exact numbers depend on the engine, which is not built yet.

```ts
interface Report {
  formatVersion: number   // the pool and decoder format version crack emitted
  dirs: string[]          // the directories processed
  filesProcessed: number  // files crack rewrote
  filesSkipped: number    // eligible-looking files left untouched (maps, already processed, vendored)
  durationMs: number      // wall-clock time for the pass
  check: boolean          // true if this was a --check run and nothing was written
}
```

A rejection is an `Error` whose message names the file and the reason. crack
does not resolve with a partial `Report` when a file fails; it rejects.

## TypeScript

The package ships type declarations for all three entry points
(`@sysce/crack`, `@sysce/crack/esbuild`, `@sysce/crack/rsbuild`). The `Options`
and `Report` types are exported from `@sysce/crack`.
