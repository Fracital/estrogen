import { spawn } from 'node:child_process';
import { getRequire } from './node-require.js';
import { buildArgs, normalizeDirs, type Options } from './options.js';
import { EstrogenError, parseDetails, type Report } from './report.js';
import { resolveBinaryPath } from './resolve-binary.js';

/**
 * Run estrogen over one or more built directories.
 *
 * Resolves the per-platform binary (or uses {@link Options.binaryPath}), spawns
 * it once with the flags and directories from `options`, and collects its
 * output. Resolves with a {@link Report} when estrogen exits 0. Rejects with a
 * {@link EstrogenError} when it exits nonzero or fails to launch; the error carries
 * the partial report.
 *
 * estrogen rewrites the directories in place and raises the cost of reading the
 * build. It is not secrecy: a decoder key ships in each file.
 *
 * @example
 * ```ts
 * import { obfuscate } from '@sysce/estrogen';
 *
 * const report = await obfuscate({ dirs: 'dist', canary: 'canary-list.json' });
 * console.log(report.stdout);
 * ```
 */
export function obfuscate(options: Options): Promise<Report> {
	const dirs = normalizeDirs(options.dirs);
	if (dirs.length === 0) {
		return Promise.reject(
			new EstrogenError(
				'estrogen needs at least one directory to rewrite; set options.dirs.',
				{
					ok: false,
					exitCode: -1,
					dirs,
					binaryPath: options.binaryPath ?? '',
					args: [],
					stdout: '',
					stderr: '',
					durationMs: 0,
				},
			),
		);
	}

	let binaryPath: string;
	try {
		binaryPath = resolveBinaryPath(getRequire(), options.binaryPath);
	} catch (error) {
		return Promise.reject(error);
	}

	const args = buildArgs(options);
	const started = Date.now();

	return new Promise<Report>((resolve, reject) => {
		const child = spawn(binaryPath, args, {
			cwd: options.cwd ?? process.cwd(),
			stdio: ['ignore', 'pipe', 'pipe'],
		});

		let stdout = '';
		let stderr = '';
		child.stdout.setEncoding('utf8');
		child.stderr.setEncoding('utf8');
		child.stdout.on('data', (chunk: string) => {
			stdout += chunk;
		});
		child.stderr.on('data', (chunk: string) => {
			stderr += chunk;
		});

		child.on('error', (error) => {
			const report: Report = {
				ok: false,
				exitCode: -1,
				dirs,
				binaryPath,
				args,
				stdout,
				stderr,
				durationMs: Date.now() - started,
			};
			reject(
				new EstrogenError(`estrogen failed to start: ${error.message}`, report),
			);
		});

		child.on('close', (code, signal) => {
			const exitCode = code ?? -1;
			const report: Report = {
				ok: exitCode === 0,
				exitCode,
				dirs,
				binaryPath,
				args,
				stdout,
				stderr,
				durationMs: Date.now() - started,
				details: parseDetails(stdout),
			};
			if (report.ok) {
				resolve(report);
				return;
			}
			const reason = signal ? `signal ${signal}` : `exit code ${exitCode}`;
			reject(new EstrogenError(`estrogen exited with ${reason}.`, report));
		});
	});
}
