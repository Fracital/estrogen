# @sysce/estrogen

Post-build web obfuscator. estrogen rewrites an already built output directory in
place: string and template literals into an encrypted per-file pool, proxy and
store names into per-build substitutes, and the embedded wasm names to match. It
leaves a build with no sensitive plaintext from the canary list and does not
change what the code does.

estrogen raises the cost of reading a build. It is not secrecy. A decoder key ships
in each file, and anyone with devtools can run the code. Do not describe the
output as secret.

This package is the Node launcher and integration layer. It resolves the native
binary for the running platform from a per-target optional dependency and runs
it. The obfuscation engine is a separate native binary; this package only
resolves and spawns it.

## Install

```sh
npm install --save-dev @sysce/estrogen
```

npm installs the one `@sysce/estrogen-<os>-<cpu>` package that matches your
platform through optional dependencies. The supported targets are
`linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64`, and `win32-x64`.

## CLI

```sh
estrogen [options] <dir>...
```

`estrogen` rewrites each directory in place. Options:

| flag                    | meaning                                                         |
| ----------------------- | --------------------------------------------------------------- |
| `-c`, `--config <file>` | config file                                                     |
| `--canary <file>`       | canary list the output must not leak                            |
| `--names <file>`        | proxy and store names to rename                                 |
| `--sourcemap <dir>`     | write maps here, outside the served tree, for private debugging |
| `--seed <hex>`          | fixed seed for reproducible output (tests only)                 |
| `-j`, `--jobs <n>`      | worker count                                                    |
| `--check`               | report and run the canary check without writing                 |
| `--quiet`               | suppress progress output                                        |
| `-v`, `--verbose`       | per-file detail                                                 |
| `--version`             | print the version                                               |
| `-h`, `--help`          | print help                                                      |

Exit code 0 means success. Any nonzero code means the build was not fully
rewritten; estrogen writes no partial output for a file it cannot handle and names
the file and the reason.

## Node API

```ts
import { obfuscate } from '@sysce/estrogen';

const report = await obfuscate({
	dirs: 'dist',
	canary: 'canary-list.json',
});
console.log(report.ok, report.durationMs);
```

`obfuscate(options)` spawns the binary once over one or more directories. It
resolves with a `Report` when estrogen exits 0 and rejects with a `EstrogenError`
(carrying the partial report) when it exits nonzero or fails to start.

## esbuild

```ts
import estrogen from '@sysce/estrogen/esbuild';
import { build } from 'esbuild';

await build({
	entryPoints: ['src/app.ts'],
	bundle: true,
	outdir: 'dist',
	plugins: [estrogen({ canary: 'canary-list.json' })],
});
```

The plugin runs in `onEnd`, after esbuild writes the bundle. It takes the output
directory from `outdir`, or from the directory of `outfile`, resolved against
`absWorkingDir`. It skips a build that produced errors and turns an estrogen failure
into an esbuild error.

## Rsbuild

```ts
import { defineConfig } from '@rsbuild/core';
import { pluginEstrogen } from '@sysce/estrogen/rsbuild';

export default defineConfig({
	plugins: [pluginEstrogen({ canary: 'canary-list.json' })],
});
```

The plugin runs in `onAfterBuild`, after Rsbuild writes the output. It takes the
dist directory from `api.context.distPath`, the path Rsbuild resolved for the
build.

`esbuild` and `@rsbuild/core` are optional peer dependencies: install whichever
one you use. The plugin options are the same as `obfuscate` options without
`dirs`, which each plugin fills in from the build.

## Source maps

A map that traces the output back to its source is the one thing the
obfuscation withholds, so the default writes none and strips any
`sourceMappingURL` comment from each file it processes. Pass `--sourcemap <dir>`
(or the `sourcemap` option) to write maps outside the served tree for private
debugging, and keep that directory private.

## License

ISC. David Reed.
