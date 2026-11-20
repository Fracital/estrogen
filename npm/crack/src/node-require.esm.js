// ESM runtime for getRequire. Copied to dist/esm/node-require.js by the build.
// import.meta.url anchors the require in this file's package so it resolves the
// optional binary packages correctly.
import { createRequire } from 'node:module';

export function getRequire() {
  return createRequire(import.meta.url);
}
