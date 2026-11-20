import { dirname, resolve as resolvePath } from 'node:path';
import type { Plugin, PluginBuild } from 'esbuild';
import { obfuscate } from './obfuscate.js';
import type { PluginOptions } from './options.js';

/**
 * esbuild plugin that runs estrogen over the build's output directory after esbuild
 * writes it.
 *
 * It finds the directory from `initialOptions.outdir`, or from the directory of
 * `initialOptions.outfile` when a single file is built, resolved against the
 * build's `absWorkingDir` or the current working directory. It runs in `onEnd`,
 * once the bundle is on disk, and skips when the build produced errors. A estrogen
 * failure is turned into an esbuild error so the build fails.
 *
 * estrogen rewrites that directory in place and raises the cost of reading it. It
 * is not secrecy: a decoder key ships in each file.
 *
 * @example
 * ```ts
 * import { build } from 'esbuild';
 * import estrogen from '@sysce/estrogen/esbuild';
 *
 * await build({
 *   entryPoints: ['src/app.ts'],
 *   bundle: true,
 *   outdir: 'dist',
 *   plugins: [estrogen({ canary: 'canary-list.json' })],
 * });
 * ```
 *
 * @param options estrogen options. `dirs` is ignored; the output directory is
 *   taken from the esbuild config.
 */
export function estrogen(options: PluginOptions = {}): Plugin {
	return {
		name: '@sysce/estrogen',
		setup(build: PluginBuild) {
			build.onEnd(async (result) => {
				if (result.errors.length > 0) return;

				const outDir = resolveOutputDir(build);
				if (outDir === null) {
					return {
						errors: [
							{
								text:
									'estrogen could not find the esbuild output directory. ' +
									'Set outdir or outfile so estrogen knows what to rewrite.',
							},
						],
					};
				}

				try {
					await obfuscate({ ...options, dirs: outDir });
					return null;
				} catch (error) {
					const message =
						error instanceof Error ? error.message : String(error);
					return { errors: [{ text: `estrogen failed: ${message}` }] };
				}
			});
		},
	};
}

/**
 * Resolve the absolute output directory from an esbuild build. Prefers `outdir`,
 * falls back to the directory of `outfile`, and resolves relative paths against
 * `absWorkingDir`. Returns null when neither is set.
 */
function resolveOutputDir(build: PluginBuild): string | null {
	const { outdir, outfile, absWorkingDir } = build.initialOptions;
	const base = absWorkingDir ?? process.cwd();
	if (outdir !== undefined) return resolvePath(base, outdir);
	if (outfile !== undefined) return resolvePath(base, dirname(outfile));
	return null;
}

export default estrogen;
