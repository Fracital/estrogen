import { resolve as resolvePath } from 'node:path';
import type { Plugin, ResolvedConfig } from 'vite';
import { obfuscate } from './obfuscate.js';
import type { PluginOptions } from './options.js';

/**
 * Vite plugin that runs estrogen over the build output directory once Vite has
 * written it.
 *
 * It reads the output directory from the resolved config (`build.outDir`,
 * resolved against `root`) in `configResolved`, and runs estrogen in
 * `closeBundle`, after the bundle is on disk. `apply: 'build'` with
 * `enforce: 'post'` keeps it out of `vite serve` and after every other plugin,
 * so it rewrites the finished build. An estrogen failure is thrown so the build
 * fails.
 *
 * estrogen rewrites that directory in place and raises the cost of reading it.
 * It is not secrecy: a decoder key ships in each file.
 *
 * @example
 * ```ts
 * import { defineConfig } from 'vite';
 * import { pluginEstrogen } from '@sysce/estrogen/vite';
 *
 * export default defineConfig({
 *   plugins: [pluginEstrogen({ canary: 'canary-list.json' })],
 * });
 * ```
 *
 * @param options estrogen options. `dirs` is ignored; the output directory comes
 *   from the Vite config.
 */
export function pluginEstrogen(options: PluginOptions = {}): Plugin {
	let outDir: string | null = null;
	return {
		name: '@sysce/estrogen',
		apply: 'build',
		enforce: 'post',
		configResolved(config: ResolvedConfig) {
			outDir = resolvePath(config.root, config.build.outDir);
		},
		async closeBundle() {
			if (outDir === null) return;
			await obfuscate({ ...options, dirs: outDir });
		},
		// Watcher seam: `estrogen --watch` and dev-server HMR land as a companion
		// serve-mode plugin (configureServer + handleHotUpdate) that re-runs the
		// vendor and obfuscate step on change. This build plugin stays build-only;
		// the dev integration is tracked separately and not wired here.
	};
}

export default pluginEstrogen;
