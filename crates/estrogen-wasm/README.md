# @sysce/estrogen-wasm

The estrogen obfuscator compiled to WebAssembly, for running the pass in a
browser. It takes one JavaScript source string and returns an obfuscated copy
plus a count of how many literals it pooled.

estrogen raises the cost of reading a build. It is not secrecy. The decoder key
ships in the returned source, so the output runs back to the input in any JS
engine, devtools included. Do not treat the output as secret.

## Install

```sh
npm install @sysce/estrogen-wasm
```

This is an ESM package built for the `web` target. Call `init()` once before
the first `obfuscate`, so the wasm module loads.

## Use

```js
import init, { obfuscate, version } from '@sysce/estrogen-wasm';

await init();

const { code, edits } = JSON.parse(
	obfuscate("const x = 'hi';", /* optional seed */ undefined),
);

console.log(version()); // the engine version, e.g. "0.1.0"
console.log(edits); // how many literals were pooled
console.log(code); // the obfuscated source
```

`obfuscate(source, seed)` returns a JSON string, so parse it. `code` is the
obfuscated source and `edits` is how many string and template literals moved
into the pool. It throws on a JavaScript parse error; catch it and keep your
previous output.

A seed makes the output deterministic: the same source and seed give
byte-identical output. Pass `undefined` or an empty string to draw fresh keys
from the browser's crypto on each call.

## License

ISC. Author: David Reed.
