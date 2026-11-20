# Integration

The obfuscation engine is not implemented yet. These steps document the stable
integration surface, not current behavior. Each example is copy-pasteable and
will work once the engine ships; until then crack is a no-op you can wire in
early.

crack runs after a bundler, over the directory the bundler wrote. It never runs
the bundler itself. Wire it as the last step of the build, before you publish or
deploy the output. Keep your canary check in the pipeline; crack runs the canary
check itself and fails closed, but a separate check in CI is a second guard.

crack runs exactly once per build. It is not idempotent: a second run encrypts
the decoder the first run added. Do not put it in a watch loop that reprocesses
the same directory.

## esbuild

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

The plugin runs after the build, over `outdir`. Pass options to override the
inferred settings, for example a config file:

```js
plugins: [crack({ config: 'crack.toml' })]
```

## rsbuild

```js
// rsbuild.config.ts
import { defineConfig } from '@rsbuild/core'
import { pluginCrack } from '@sysce/crack/rsbuild'

export default defineConfig({
  plugins: [pluginCrack()],
})
```

The plugin runs after the build, over the resolved dist path. Pass options to
override, the same as the esbuild plugin.

## Vite, via a generic post-build step

Vite has no crack plugin. Run crack over the output directory after `vite build`
with the Node API, in a small script:

```js
// scripts/protect.mjs
import { build } from 'vite'
import { obfuscate } from '@sysce/crack'

await build()                       // writes dist/
await obfuscate({ dirs: ['dist'] }) // rewrites dist/ in place
```

```sh
node scripts/protect.mjs
```

Or keep `vite build` as its own step and run the CLI after it; see the next
section. Either way crack runs once, after the full build, over the finished
`dist`.

## Plain CLI step in package.json

Run the binary over the output directory as a build step. This needs no code and
works with any bundler.

```json
{
  "scripts": {
    "build": "vite build && crack dist",
    "check:protect": "crack --check dist"
  }
}
```

`npm run build` builds, then rewrites `dist` in place. `npm run check:protect`
runs the passes and the canary check without writing anything, and exits nonzero
if the directory would fail; use it in CI.

Point crack at a config file for the per-site names and canary list:

```json
{
  "scripts": {
    "build": "vite build && crack -c crack.toml dist"
  }
}
```

## Source maps and the edge backstop

By default crack writes no source map and strips `sourceMappingURL` comments
from every chunk it processes, so the bundler's own maps (which point at
pre-obfuscation source) never ship. A build that needs a map for private
debugging passes `--sourcemap <dir>` (CLI) or `sourcemap` (Node options) to
write it outside the served tree.

This is not the whole guard. Every consumer host also returns 403 for `*.map`
and for any `sourceMappingURL` target, so a map that reaches a published
directory by accident is still never served. That edge rule is a required
integration step and a config-parity item, not an afterthought. It lives in the
edge config, not in crack. See
[Source maps](https://github.com/Fracital/crack/wiki/Source-maps).
