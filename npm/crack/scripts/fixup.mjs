// Post-build fixup. tsc writes the compiled JS and type files; this adds the two
// things it cannot:
//  1. a package.json type marker in each output directory, so Node reads
//     dist/esm as ES modules and dist/cjs as CommonJS whatever the root type is.
//  2. the format-specific node-require runtime, copied from the hand-written
//     templates in src (tsc skips them because they are plain .js).
import { copyFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const pkgRoot = dirname(here);
const dist = join(pkgRoot, 'dist');
const src = join(pkgRoot, 'src');

mkdirSync(join(dist, 'esm'), { recursive: true });
mkdirSync(join(dist, 'cjs'), { recursive: true });

writeFileSync(join(dist, 'esm', 'package.json'), `${JSON.stringify({ type: 'module' }, null, 2)}\n`);
writeFileSync(join(dist, 'cjs', 'package.json'), `${JSON.stringify({ type: 'commonjs' }, null, 2)}\n`);

copyFileSync(join(src, 'node-require.esm.js'), join(dist, 'esm', 'node-require.js'));
copyFileSync(join(src, 'node-require.cjs.js'), join(dist, 'cjs', 'node-require.js'));

process.stdout.write('fixup: wrote dist type markers and node-require runtimes\n');
