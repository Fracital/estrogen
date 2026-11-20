// Merge publish metadata into the wasm-pack generated pkg/package.json.
//
// wasm-pack rewrites pkg/package.json on every build and only fills in a few
// fields (name, version, files, main, types, sideEffects). This script adds the
// rest of the npm metadata and fixes the fields wasm-pack gets wrong for this
// package, so the result is ready for `npm publish`. Run it after every build.

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const pkgPath = resolve(here, '..', 'pkg', 'package.json');

const pkg = JSON.parse(readFileSync(pkgPath, 'utf8'));

// Fields this script owns. wasm-pack never writes these, or writes them wrong.
const meta = {
	description:
		'The estrogen obfuscator compiled to WebAssembly. Raises the cost of reading JavaScript, not secrecy: a decoder key ships in each file.',
	license: 'ISC',
	author: 'David Reed',
	repository: {
		type: 'git',
		url: 'git+https://github.com/Fracital/estrogen.git',
	},
	homepage: 'https://github.com/Fracital/estrogen#readme',
	bugs: {
		url: 'https://github.com/Fracital/estrogen/issues',
	},
	keywords: [
		'obfuscator',
		'obfuscation',
		'wasm',
		'webassembly',
		'javascript',
		'browser',
	],
	files: [
		'estrogen_bg.wasm',
		'estrogen_bg.wasm.d.ts',
		'estrogen.js',
		'estrogen.d.ts',
		'README.md',
	],
};

// wasm-pack records the maintainer under `collaborators`. `author` is the field
// npm shows, so drop the duplicate once `author` is set.
delete pkg.collaborators;

Object.assign(pkg, meta);

writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + '\n');

console.log(`wrote ${pkgPath}`);
