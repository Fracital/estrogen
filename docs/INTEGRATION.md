# Integration

estrogen runs after a bundler, over the directory the bundler wrote. It never runs
the bundler itself. Wire it as the last step of the build, before you publish or
deploy the output. Keep your canary check in the pipeline; estrogen runs the canary
check itself and fails closed, but a separate check in CI is a second guard.

estrogen runs exactly once per build. It is not idempotent: a second run encrypts
the decoder the first run added. For watch builds, `estrogen --watch` re-runs on
change and skips the chunks it already marked, so it does not re-wrap its own
output; do not instead loop a plain `estrogen` over the same directory.

## esbuild

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

The plugin runs after the build, over `outdir`. Pass options to override the
inferred settings, for example a config file:

```js
plugins: [estrogen({ config: 'estrogen.toml' })];
```

## rsbuild

```js
// rsbuild.config.ts
import { defineConfig } from '@rsbuild/core';
import { pluginEstrogen } from '@sysce/estrogen/rsbuild';

export default defineConfig({
	plugins: [pluginEstrogen()],
});
```

The plugin runs after the build, over the resolved dist path. Pass options to
override, the same as the esbuild plugin.

## Vite

```ts
// vite.config.ts
import { pluginEstrogen } from '@sysce/estrogen/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [pluginEstrogen()],
});
```

The plugin runs after `vite build` writes the output, over `build.outDir`, and
stays out of `vite serve`. Pass options to override, the same as the other
plugins.

## Rollup

```js
// rollup.config.js
import { pluginEstrogen } from '@sysce/estrogen/rollup';

export default {
	input: 'src/index.js',
	output: { dir: 'dist', format: 'es' },
	plugins: [pluginEstrogen()],
};
```

The plugin runs in `writeBundle`, over the output directory.

## Astro

```js
// astro.config.mjs
import estrogen from '@sysce/estrogen/astro';
import { defineConfig } from 'astro/config';

export default defineConfig({
	integrations: [estrogen()],
});
```

The integration runs on Astro's `astro:build:done` hook, over the final static
output directory. For an SSR build it rewrites the client output directory. Pass
options to override, the same as the other plugins; it fills `dirs` from the
build output. A failing file fails the build.

## Plain CLI step in package.json

Run the binary over the output directory as a build step. This needs no code and
works with any bundler.

```json
{
	"scripts": {
		"build": "vite build && estrogen dist",
		"check:protect": "estrogen --check dist"
	}
}
```

`npm run build` builds, then rewrites `dist` in place. `npm run check:protect`
runs the passes and the canary check without writing anything, and exits nonzero
if the directory would fail; use it in CI.

Point estrogen at a config file for the per-site names and canary list:

```json
{
	"scripts": {
		"build": "vite build && estrogen -c estrogen.toml dist"
	}
}
```

## Source maps and the edge backstop

By default estrogen writes no source map and strips `sourceMappingURL` comments
from every chunk it processes, so the bundler's own maps (which point at
pre-obfuscation source) never ship. A build that needs a map for private
debugging passes `--sourcemap <dir>` (CLI) or `sourcemap` (Node options) to
write it outside the served tree.

This is not the whole guard. Every consumer host also returns 403 for `*.map`
and for any `sourceMappingURL` target, so a map that reaches a published
directory by accident is still never served. That edge rule is a required
integration step and a config-parity item, not an afterthought. It lives in the
edge config, not in estrogen. See
[Source maps](https://github.com/Fracital/estrogen/wiki/Source-maps).
