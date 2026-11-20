import { dirname, resolve as resolvePath } from 'node:path';
import type { OutputOptions, Plugin } from 'rollup';
import { obfuscate } from './obfuscate.js';
import type { PluginOptions } from './options.js';

/**
 * Rollup plugin that runs estrogen over the output directory once Rollup has
 * written it.
 *
 * It finds the directory from the write's `output.dir`, or from the directory of
 * `output.file` for a single-file build, resolved against the current working
 * directory. It runs in `writeBundle`, after the files are on disk. An estrogen
 * failure is thrown so the build fails.
 *
 * estrogen rewrites that directory in place and raises the cost of reading it.
 * It is not secrecy: a decoder key ships in each file.
 *
 * @example
 * ```ts
 * import { pluginEstrogen } from '@sysce/estrogen/rollup';
 *
 * export default {
 *   input: 'src/app.js',
 *   output: { dir: 'dist', format: 'es' },
 *   plugins: [pluginEstrogen({ canary: 'canary-list.json' })],
 * };
 * ```
 *
 * @param options estrogen options. `dirs` is ignored; the output directory comes
 *   from the Rollup output config.
 */
export function pluginEstrogen(options: PluginOptions = {}): Plugin {
	return {
		name: '@sysce/estrogen',
		async writeBundle(output: OutputOptions) {
			const outDir = resolveOutputDir(output);
			if (outDir === null) {
				throw new Error(
					'estrogen could not find the Rollup output directory. ' +
						'Set output.dir or output.file so estrogen knows what to rewrite.',
				);
			}
			await obfuscate({ ...options, dirs: outDir });
		},
	};
}

/**
 * Resolve the absolute output directory from a Rollup write. Prefers `dir`,
 * falls back to the directory of `file`, and resolves relative paths against the
 * working directory. Returns null when neither is set.
 */
function resolveOutputDir(output: OutputOptions): string | null {
	if (output.dir !== undefined) return resolvePath(process.cwd(), output.dir);
	if (output.file !== undefined)
		return resolvePath(process.cwd(), dirname(output.file));
	return null;
}

export default pluginEstrogen;
