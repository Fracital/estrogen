import { fileURLToPath } from 'node:url';
import { defineConfig } from 'astro/config';
import estrogen from '@sysce/estrogen/astro';

const estrogenBin =
	process.env.ESTROGEN_BIN ??
	fileURLToPath(new URL('../target/release/estrogen', import.meta.url));

export default defineConfig({
	output: 'static',
	integrations: [estrogen({ binaryPath: estrogenBin })],
});
