#!/usr/bin/env node
// crack launcher. Resolves the per-platform binary from the installed
// @sysce/crack-<os>-<cpu> optional dependency and execs it, forwarding argv and
// the exit code. A missing binary is a clear error, not a stack trace.
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';

import { MissingBinaryError, resolveBinaryPath } from '../dist/esm/resolve-binary.js';

const require = createRequire(import.meta.url);

let binaryPath;
try {
  // CRACK_BINARY lets a local build or a test point the launcher at its own
  // binary without installing a per-platform package.
  binaryPath = resolveBinaryPath(require, process.env.CRACK_BINARY);
} catch (error) {
  if (error instanceof MissingBinaryError) {
    process.stderr.write(`${error.message}\n`);
    process.exit(1);
  }
  throw error;
}

const child = spawn(binaryPath, process.argv.slice(2), { stdio: 'inherit' });

child.on('error', (error) => {
  process.stderr.write(`crack failed to start: ${error.message}\n`);
  process.exit(1);
});

child.on('close', (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 1);
});
