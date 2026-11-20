/**
 * `@sysce/crack`: run the crack obfuscator from Node.
 *
 * crack rewrites an already built output directory, in place: string and
 * template literals into an encrypted per-file pool, proxy and store names into
 * per-build substitutes, and the embedded wasm names to match. It raises the
 * cost of reading the build. It is not secrecy: a decoder key ships in each
 * file, and anyone with devtools can run the code.
 *
 * - {@link obfuscate} spawns the binary over one or more directories.
 * - The esbuild plugin lives at `@sysce/crack/esbuild`.
 * - The Rsbuild plugin lives at `@sysce/crack/rsbuild`.
 *
 * @packageDocumentation
 */
export { obfuscate } from './obfuscate.js';
export type { Options, PluginOptions } from './options.js';
export { normalizeDirs, buildArgs } from './options.js';
export { CrackError } from './report.js';
export type { Report } from './report.js';
export {
	PLATFORM_PACKAGES,
	MissingBinaryError,
	platformPackageName,
	targetKey,
	binSubpath,
	resolveBinaryPath,
} from './resolve-binary.js';
