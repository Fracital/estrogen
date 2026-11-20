import type { RsbuildPlugin } from '@rsbuild/core';
import { obfuscate } from './obfuscate.js';
import type { PluginOptions } from './options.js';

/**
 * Rsbuild plugin that runs estrogen over the resolved dist directory after the
 * build finishes.
 *
 * It reads the dist path from the Rsbuild context (`api.context.distPath`),
 * which is the absolute output directory Rsbuild resolved for the build, and
 * runs estrogen in `onAfterBuild`, once the files are written. A estrogen failure is
 * rethrown so the build fails.
 *
 * estrogen rewrites that directory in place and raises the cost of reading it. It
 * is not secrecy: a decoder key ships in each file.
 *
 * @example
 * ```ts
 * import { defineConfig } from '@rsbuild/core';
 * import { pluginEstrogen } from '@sysce/estrogen/rsbuild';
 *
 * export default defineConfig({
 *   plugins: [pluginEstrogen({ canary: 'canary-list.json' })],
 * });
 * ```
 *
 * @param options estrogen options. `dirs` is ignored; the dist path comes from the
 *   Rsbuild context.
 */
export function pluginEstrogen(options: PluginOptions = {}): RsbuildPlugin {
	return {
		name: '@sysce/estrogen',
		setup(api) {
			api.onAfterBuild(async () => {
				const distPath = api.context.distPath;
				if (!distPath) {
					throw new Error(
						'estrogen could not find the Rsbuild dist path. ' +
							'Set output.distPath.root so estrogen knows what to rewrite.',
					);
				}
				await obfuscate({ ...options, dirs: distPath });
			});
		},
	};
}

export default pluginEstrogen;
