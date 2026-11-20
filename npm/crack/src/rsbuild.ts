import type { RsbuildPlugin } from '@rsbuild/core';
import { obfuscate } from './obfuscate.js';
import type { PluginOptions } from './options.js';

/**
 * Rsbuild plugin that runs crack over the resolved dist directory after the
 * build finishes.
 *
 * It reads the dist path from the Rsbuild context (`api.context.distPath`),
 * which is the absolute output directory Rsbuild resolved for the build, and
 * runs crack in `onAfterBuild`, once the files are written. A crack failure is
 * rethrown so the build fails.
 *
 * crack rewrites that directory in place and raises the cost of reading it. It
 * is not secrecy: a decoder key ships in each file.
 *
 * @example
 * ```ts
 * import { defineConfig } from '@rsbuild/core';
 * import { pluginCrack } from '@sysce/crack/rsbuild';
 *
 * export default defineConfig({
 *   plugins: [pluginCrack({ canary: 'canary-list.json' })],
 * });
 * ```
 *
 * @param options crack options. `dirs` is ignored; the dist path comes from the
 *   Rsbuild context.
 */
export function pluginCrack(options: PluginOptions = {}): RsbuildPlugin {
	return {
		name: '@sysce/crack',
		setup(api) {
			api.onAfterBuild(async () => {
				const distPath = api.context.distPath;
				if (!distPath) {
					throw new Error(
						'crack could not find the Rsbuild dist path. ' +
							'Set output.distPath.root so crack knows what to rewrite.',
					);
				}
				await obfuscate({ ...options, dirs: distPath });
			});
		},
	};
}

export default pluginCrack;
