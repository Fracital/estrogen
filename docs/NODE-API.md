# Node API

The package is `@sysce/estrogen`. It ships the native binary as a per-target
optional dependency and a thin launcher that runs the right one. The Node API
calls that binary over a built directory.

## obfuscate

```js
import { obfuscate } from '@sysce/estrogen';

const report = await obfuscate({ dirs: ['dist'] });
```

`obfuscate(options): Promise<Report>` runs the whole pass over one or more
directories and resolves with a [`Report`](#report). It rejects if any file
fails (the same fail-closed rule as the CLI): no partial output is written for a
failing file, and the error names the file and the reason.

## esbuild plugin

```js
import estrogen from '@sysce/estrogen/esbuild';
import { build } from 'esbuild';

await build({
	entryPoints: ['src/index.ts'],
	bundle: true,
	outdir: 'dist',
	plugins: [estrogen()],
});
```

The default export of `@sysce/estrogen/esbuild` is `estrogen(options?):
esbuild.Plugin`. The plugin runs after the build finishes, over the build's
output directory (`outdir`, or the directory of `outfile`). Pass `options` to
override what estrogen would otherwise infer; see [Options](#options). If a file
fails, the build fails.

## rsbuild plugin

```js
import { pluginEstrogen } from '@sysce/estrogen/rsbuild';

export default {
	plugins: [pluginEstrogen()],
};
```

`pluginEstrogen(options?): RsbuildPlugin` is a named export of
`@sysce/estrogen/rsbuild`. The plugin runs after the build finishes, over the
resolved dist path. Pass `options` to override the inferred settings. A failing
file fails the build.

## Vite plugin

```js
import { pluginEstrogen } from '@sysce/estrogen/vite';

export default {
	plugins: [pluginEstrogen()],
};
```

`pluginEstrogen(options?): Plugin` is a named export (and the default export) of
`@sysce/estrogen/vite`. It sets `apply: 'build'` and `enforce: 'post'`, so it runs
after `vite build` writes the output, over `build.outDir`, and stays out of
`vite serve`. A failing file fails the build.

## Rollup plugin

```js
import { pluginEstrogen } from '@sysce/estrogen/rollup';

export default {
	input: 'src/index.js',
	output: { dir: 'dist', format: 'es' },
	plugins: [pluginEstrogen()],
};
```

`pluginEstrogen(options?): Plugin` is a named export (and the default export) of
`@sysce/estrogen/rollup`. It runs in `writeBundle`, over `output.dir` or the
directory of `output.file`. A failing file fails the build.

## Astro integration

```js
import estrogen from '@sysce/estrogen/astro';
import { defineConfig } from 'astro/config';

export default defineConfig({
	integrations: [estrogen()],
});
```

The default export of `@sysce/estrogen/astro` is `estrogen(options?):
AstroIntegration`. It runs on the `astro:build:done` hook, over the final static
output directory; for an SSR build it rewrites the client output directory. Pass
`options` to override the inferred settings; it takes the same fields as the
other plugins except `dirs`, which it fills from the build output. A failing file
fails the build.

## Options

The same options object is accepted by `obfuscate()` and, as overrides, by the
esbuild, rsbuild, Vite and Rollup plugins and the Astro integration. Every field
is optional for the plugins, which infer `dirs` from the build; `obfuscate()`
needs at least `dirs`.

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

`obfuscate()` runs the `estrogen` binary over the directories and resolves with a
`Report` of that run:

```ts
interface Report {
	ok: boolean; // true when estrogen exited 0
	exitCode: number; // the process exit code
	dirs: string[]; // the directories passed
	binaryPath: string; // the per-target binary that ran
	args: string[]; // the arguments it was given
	stdout: string; // captured stdout
	stderr: string; // captured stderr
	durationMs: number; // wall-clock time for the run
	details?: unknown; // parsed per-file detail when estrogen emits it
}
```

When estrogen exits nonzero (a parse error, a surviving canary, a store name
outside the allow-list), `obfuscate()` rejects with an error that carries the
`Report`, and `stderr` names the file and the reason. estrogen writes no partial
output for a failing file.

## TypeScript

The package ships type declarations for every entry point (`@sysce/estrogen` and
its `/esbuild`, `/rsbuild`, `/vite` and `/rollup` plugins and its `/astro`
integration). The `Options` and `Report` types are exported from
`@sysce/estrogen`.
