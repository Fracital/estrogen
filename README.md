# crack

[![npm](https://img.shields.io/npm/v/@sysce/crack?maxAge=3600&label=npm)](https://www.npmjs.com/package/@sysce/crack)
[![CI](https://github.com/Fracital/crack/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Fracital/crack/actions/workflows/ci.yml)
[![license: ISC](https://img.shields.io/badge/license-ISC-blue.svg)](LICENSE)
[![GitHub issues](https://img.shields.io/github/issues/Fracital/crack?style=flat&color=orange)](https://github.com/Fracital/crack/issues)
[![GitHub stars](https://img.shields.io/github/stars/Fracital/crack?style=flat&color=orange)](https://github.com/Fracital/crack/stargazers)

A post-build web obfuscator. It takes a directory a bundler already produced and
rewrites the JS, HTML and SVG in place: string and template literals into an
encrypted per-file pool, proxy and store names into per-build substitutes, and
embedded wasm names to match. It leaves no sensitive plaintext from the canary
list, and it does not change what the code does.

crack is written in Rust on [OXC](https://oxc.rs) and published to npm as
[`@sysce/crack`](https://www.npmjs.com/package/@sysce/crack). The binary is
`crack`.

**crack raises the cost of reading a build. It does not make the build secret.**
A decoder key ships in each file, and anyone with devtools can run the code. TLS
is the only transport protection. Do not describe the output as secret.

The obfuscation engine is not implemented yet. This repository documents the
stable public surface (the CLI, the Node API, the Rust API, and the integration
steps), not current behavior.

## Install

```sh
npm i -D @sysce/crack
```

The package pulls the matching per-target binary (`@sysce/crack-<os>-<cpu>`) as
an optional dependency. Supported targets: linux x64 and arm64, darwin x64 and
arm64, win32 x64.

## Quick start

Run it over a built directory from the command line:

```sh
npx crack dist
```

Or run it after an esbuild build, over the output directory:

```js
import crack from '@sysce/crack/esbuild';
import { build } from 'esbuild';

await build({
	entryPoints: ['src/index.ts'],
	bundle: true,
	outdir: 'dist',
	plugins: [crack()],
});
```

crack runs exactly once per build. It is not idempotent: a second run would
encrypt the decoder the first run added.

## Documentation

- [CLI reference](docs/CLI.md): every flag, examples, exit codes, the
  fail-closed rule.
- [Node API](docs/NODE-API.md): `obfuscate()`, the esbuild and rsbuild plugins,
  the options object, the `Report` shape.
- [Rust API](docs/RUST-API.md): the `crack-core` public surface.
- [Integration](docs/INTEGRATION.md): esbuild, rsbuild, Vite, and a plain CLI
  step in `package.json`.
- [Wiki](https://github.com/Fracital/crack/wiki): install, configuration,
  source maps, security and limits, contributing.

## Powering

<table>
<tr><th></th><th>Site</th><th>Link</th><th>Repo</th></tr>
<tr>
<td align="center" valign="middle"><img src="docs/assets/brands/abstract.svg" alt="abstract v2" width="28"></td>
<td valign="middle">abstract v2</td>
<td valign="middle"><a href="https://abstracteducation.loan">abstracteducation.loan</a></td>
<td valign="middle"></td>
</tr>
<tr>
<td align="center" valign="middle"><img src="docs/assets/brands/fracital.svg" alt="Fracital" width="28"></td>
<td valign="middle">Fracital</td>
<td valign="middle"><a href="https://fracit.al">fracit.al</a></td>
<td valign="middle"><a href="https://github.com/Fracital/Fracital"><code>Fracital/Fracital</code></a></td>
</tr>
<tr>
<td align="center" valign="middle"><img src="docs/assets/brands/systemya.png" alt="SystemYA" width="28"></td>
<td valign="middle">SystemYA</td>
<td valign="middle"><a href="https://systemya.fracit.al">systemya.fracit.al</a></td>
<td valign="middle"><a href="https://github.com/Fracital/SystemYA"><code>Fracital/SystemYA</code></a></td>
</tr>
<tr>
<td align="center" valign="middle"><img src="docs/assets/brands/stomp.png" alt="STomp" width="28"></td>
<td valign="middle">STomp (powerMouse)</td>
<td valign="middle"><a href="https://stomp.fracit.al">stomp.fracit.al</a></td>
<td valign="middle"><a href="https://github.com/Fracital/powerMouse"><code>Fracital/powerMouse</code></a></td>
</tr>
<tr>
<td align="center" valign="middle"><img src="docs/assets/brands/holy-unblocker.svg" alt="Holy Unblocker" width="28"></td>
<td valign="middle">Holy Unblocker</td>
<td valign="middle"><a href="https://holyunb.locker">holyunb.locker</a></td>
<td valign="middle"><a href="https://github.com/holy-unblocker/website2"><code>holy-unblocker/website2</code></a></td>
</tr>
</table>

## Credits

|                                                                               | GitHub                         | Name       |
| ----------------------------------------------------------------------------- | ------------------------------ | ---------- |
| <img src="docs/assets/am%20i%20cool%20yet.png" width="64" alt="e9x's avatar"> | [@e9x](https://github.com/e9x) | David Reed |

## License

ISC. See [LICENSE](LICENSE).
