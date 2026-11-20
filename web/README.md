# estrogen web demo

A static Astro page that reproduces the obfuscator.io interface, rebranded to
estrogen, and runs the real estrogen engine in the browser as WebAssembly. There
is no server: your code never leaves the page.

estrogen rewrites built JavaScript so a reader pays more to make sense of it. It
is not secrecy. A decoder key ships in each file, and anyone with devtools can
run the code back. The demo copy and FAQ say so.

## Commands

```
npm install        # install astro and codemirror
npm run dev        # dev server at http://localhost:4321
npm run build      # static output in dist/
npm run preview    # serve the built dist/
```

## What the page does

- Tab 1 pastes JavaScript into a CodeMirror 5 editor and runs the app-chunk pass
  on it.
- Tab 2 drops or picks a .js file and loads it into the editor.
- Tab 3 shows the obfuscated output, the count of pooled literals, a download
  button, and an Evaluate checkbox that runs the output with a captured console.
- The options column has one real knob, a Seed field. Empty gives random keys
  each run; a value gives deterministic output. The rest of the column is honest
  info about what the pass does and where it stops.

## The engine files

`src/wasm/{estrogen.js, estrogen_bg.wasm, estrogen.d.ts, estrogen_bg.wasm.d.ts}`
are generated, not hand-edited. Regenerate them from the `estrogen-wasm` crate:

```
scripts/build-wasm.sh
```

That runs `wasm-pack build crates/estrogen-wasm --target web` and copies the
output into `src/wasm`. The client imports `estrogen.js`, and `await init()`
resolves `estrogen_bg.wasm` through `new URL('estrogen_bg.wasm', import.meta.url)`,
which Astro and Vite emit as a hashed asset.

## Credits

The interface is derived from
[javascript-obfuscator-ui](https://github.com/javascript-obfuscator/javascript-obfuscator-ui)
(BSD-2-Clause) by Timofey Kachalov and Tiago Serafim. It bundles Semantic UI
(MIT) and CodeMirror (MIT). See [THIRD-PARTY.md](./THIRD-PARTY.md).
