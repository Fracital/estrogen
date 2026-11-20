import { fileURLToPath } from 'node:url';
import type { AstroIntegration } from 'astro';
import { obfuscate } from './obfuscate.js';
import type { PluginOptions } from './options.js';

/**
 * Astro integration that runs estrogen over the build output directory once Astro
 * has written it.
 *
 * It hooks `astro:build:done`, which runs after the build is on disk, and reads
 * the final output directory from that hook's `dir` URL through
 * `fileURLToPath`. For a static (SSG) build this is the directory Astro serves;
 * for an SSR build it is the client output directory, so the integration
 * rewrites the client assets. An estrogen failure is thrown so the build fails.
 *
 * estrogen rewrites that directory in place and raises the cost of reading it.
 * It is not secrecy: a decoder key ships in each file, and anyone with devtools
 * can run the code.
 *
 * @example
 * ```ts
 * import { defineConfig } from 'astro/config';
 * import estrogen from '@sysce/estrogen/astro';
 *
 * export default defineConfig({
 *   integrations: [estrogen({ canary: 'canary-list.json' })],
 * });
 * ```
 *
 * @param options estrogen options. `dirs` is ignored; the output directory comes
 *   from the Astro build hook.
 */
export function estrogen(options: PluginOptions = {}): AstroIntegration {
	return {
		name: '@sysce/estrogen',
		hooks: {
			'astro:build:done': async ({ dir, logger }) => {
				const outDir = fileURLToPath(dir);
				logger.info(`running estrogen over ${outDir}`);
				await obfuscate({ ...options, dirs: outDir });
			},
		},
	};
}

export default estrogen;
