# Integration

The obfuscation engine is not implemented yet. These steps document the stable
integration surface, not current behavior. Each example is copy-pasteable and
will work once the engine ships; until then estrogen is a no-op you can wire in
early.

estrogen runs after a bundler, over the directory the bundler wrote. It never runs
the bundler itself. Wire it as the last step of the build, before you publish or
deploy the output. Keep your canary check in the pipeline; estrogen runs the canary
check itself and fails closed, but a separate check in CI is a second guard.

estrogen runs exactly once per build. It is not idempotent: a second run encrypts
the decoder the first run added. Do not put it in a watch loop that reprocesses
the same directory.

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

## Vite, via a generic post-build step

Vite has no estrogen plugin. Run estrogen over the output directory after `vite build`
with the Node API, in a small script:

```js
// scripts/protect.mjs
import { obfuscate } from '@sysce/estrogen';
import { build } from 'vite';

await build(); // writes dist/
await obfuscate({ dirs: ['dist'] }); // rewrites dist/ in place
```

```sh
node scripts/protect.mjs
```

Or keep `vite build` as its own step and run the CLI after it; see the next
section. Either way estrogen runs once, after the full build, over the finished
`dist`.

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
