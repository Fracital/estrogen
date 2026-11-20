import { fileURLToPath } from 'node:url';
import { defineConfig } from 'astro/config';
import estrogen from '@sysce/estrogen/astro';

const estrogenBin =
	process.env.ESTROGEN_BIN ??
	fileURLToPath(new URL('../target/release/estrogen', import.meta.url));

// Name every built file by content hash only. Astro and Vite default to
// `[name].[hash]`, which leaks the source: a script becomes
// `index.astro_astro_type_script_index_0_lang.<hash>.js`, the styles keep
// `index`, and the assets keep `estrogen`. Hash-only names drop all of that,
// so the obfuscated output does not advertise its own file layout.
const hashedOutput = {
	entryFileNames: '_astro/[hash].js',
	chunkFileNames: '_astro/[hash].js',
	assetFileNames: '_astro/[hash][extname]',
};

export default defineConfig({
	output: 'static',
	integrations: [estrogen({ binaryPath: estrogenBin })],
	vite: {
		build: {
			rollupOptions: { output: hashedOutput },
		},
	},
});
