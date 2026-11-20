# Asset optimization

estrogen shrinks the static assets in a built directory as well as obfuscating
the code. It optimizes `.svg` images, minifies `.html` shells, and minifies
`.css` stylesheets. All three are on by default and replace the separate minify
steps a site otherwise keeps in its bundler (an `svgo` or `svgmin` transform, an
HTML minifier, a CSS minifier).

These steps shrink output; they do not obfuscate. estrogen still obfuscates JS
chunks and inline scripts, encrypts literals, and runs the HTML and SVG cloak
pass. Asset optimization runs alongside that work, over the same directory, after
the bundler has written it.

## What each step does

### SVG

estrogen optimizes every `.svg` asset with [oxvg](https://github.com/noahbald/oxvg),
the Rust port of SVGO, running SVGO's default job set: it drops editor metadata
and comments, collapses whitespace, rounds path numbers, and removes default
attributes. The output matches what a `svgo` default run produces on the same
input.

A `.svg` with an inline `<script>` is a cloak, not an image. estrogen leaves it
for the cloak pass, which neutralizes a canary `<title>` or `<meta>` and encrypts
the inline script; a cloak is never optimized. A plain image asset is optimized,
and the cloak pass still runs over the optimized text, so a canary in an asset's
`<title>` is still neutralized.

### HTML

estrogen minifies every `.html` shell with
[minify-html](https://github.com/wilsonzlin/minify-html)'s one-pass engine: it
strips comments and collapses insignificant whitespace between tags. It is
HTML-aware, so it never touches the content of `<pre>`, `<textarea>`, `<script>`
or `<style>`, and it keeps significant whitespace around inline elements.

Minification runs before the cloak pass, so the cloak pass neutralizes a canary
`<title>` or `<meta>` and encrypts a canary-bearing inline `<script>` over the
minified text. The HTML minifier's own JS and CSS minifiers are left off:
estrogen's literal pool and the CSS step are the only things that rewrite script
and style bodies, and the cloak pass needs the inline script body intact to find
and encrypt it.

### CSS

estrogen minifies every `.css` stylesheet with
[lightningcss](https://lightningcss.dev): it drops comments and insignificant
whitespace, shortens colors and values, and reprints the stylesheet in its
minimal form. CSS carries no secret the way a chunk does, so estrogen does not
rewrite it, it only shrinks it.

## Safety

Every step is safe by the same rules:

- It never writes invalid output. An SVG and a stylesheet are reparsed and
  reserialized, so the result is well formed by construction; the HTML minifier
  rejects malformed markup outright.
- It fails closed. On any parser or minifier error estrogen keeps the original
  file byte for byte. A malformed SVG, HTML or CSS file is left exactly as the
  bundler wrote it, never reshaped or truncated.
- It only writes a smaller result. A file that does not shrink is left as it was,
  which makes each step idempotent: a second run finds nothing smaller and
  changes nothing.

## Canaries

A canary is a literal that must not survive verbatim in a shipped file. When
estrogen optimizes an asset, it scans the result for every canary and fails the
build if one is still present, the same check it runs on every chunk it rewrites.
Optimization only removes text (metadata, comments, whitespace), so it never
hides a canary that was already gone. A canary that a minifier reveals, for
example by decoding an HTML entity back to the plain literal, is caught by this
scan and fails the build.

The CSS step scans a stylesheet whenever it parses, even one that does not
shrink. The SVG and HTML steps scan the result when the asset is optimized. An
asset that does not shrink, and so is left untouched, is not scanned by the SVG
and HTML steps. Put any literal you need caught in the canary list; do not rely on
asset optimization alone to find one.

## Turning it off

Each step has its own switch, all on by default.

Command line:

```sh
estrogen --no-optimize-svg --no-minify-html --no-minify-css dist
```

Config file (`[options]` table):

```toml
[options]
optimize_svg = false
minify_html = false
minify_css = false
```

Bundler plugins take `optimizeSvg`, `minifyHtml` and `minifyCss`:

```ts
import estrogen from '@sysce/estrogen/esbuild';

estrogen({ minifyCss: false });
```

## Dropping a site's own minify steps

A site that minifies assets in its bundler, for example with an `svgo` transform,
a hand-written `svgmin.mjs`, an HTML minifier plugin, or a CSS minifier, does not
need those steps once estrogen runs over the build. estrogen optimizes every
`.svg`, `.html` and `.css` in the output directory after the bundler writes it,
so the minification moves from the bundler to the estrogen pass.

Before, with a Vite or Astro plugin that ran `svgo` on each imported icon:

```ts
// astro.config.ts
import { defineConfig } from 'astro/config';
import { svgr } from './svgmin.mjs'; // runs svgo.optimize() on *.svg?react

export default defineConfig({
	vite: { plugins: [svgr()] },
});
```

After, with estrogen's Astro integration (or any of its bundler plugins), and no
`svgmin.mjs`:

```ts
// astro.config.ts
import { defineConfig } from 'astro/config';
import estrogen from '@sysce/estrogen/astro';

export default defineConfig({
	integrations: [estrogen({ canary: 'canary-list.json' })],
});
```

estrogen runs after the build and optimizes the assets on disk, so the result is
at least as small as the old steps produced, from the same SVGO default job set
for SVG and an equivalent minifier for HTML and CSS. The one difference is where
it runs: a bundler plugin optimizes each asset as it is imported, while estrogen
optimizes the final files on disk. An asset the bundler inlines into a JS chunk
as a string is handled by that chunk's own obfuscation, not these steps, which
act on files.

## Build size

SVG optimization adds the `oxvg` optimiser and its XML front end to the estrogen
binary. `oxvg` pulls in a CSS parser (`lightningcss`, `cssparser`) and a path
geometry crate (`i_overlay`) for its style and path jobs. CSS minification reuses
that same `lightningcss`, so it adds no new compiled crate. HTML minification adds
minify-html's one-pass engine. The binary is larger than before; the cost is paid
once at build time and does not reach any shipped file.
