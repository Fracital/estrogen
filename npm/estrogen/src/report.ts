/**
 * The result of a estrogen run, resolved by {@link obfuscate}. estrogen exits 0 on
 * success and nonzero on failure, so a resolved report always has
 * `exitCode === 0`; a failed run rejects with a {@link EstrogenError} instead.
 */
export interface Report {
	/** True when estrogen exited 0. */
	ok: boolean;
	/** The process exit code. 0 on success. */
	exitCode: number;
	/** The directories estrogen was asked to rewrite. */
	dirs: string[];
	/** The binary that ran. */
	binaryPath: string;
	/** The argument vector passed to the binary. */
	args: string[];
	/** Captured standard output. */
	stdout: string;
	/** Captured standard error. */
	stderr: string;
	/** Wall-clock run time in milliseconds. */
	durationMs: number;
	/**
	 * Structured detail, when the binary prints a JSON object on stdout. The
	 * shape is the binary's own and may grow; read it defensively.
	 */
	details?: unknown;
}

/**
 * Thrown when estrogen exits nonzero or fails to launch. Carries the partial
 * {@link Report} so a caller can read the exit code, stderr, and the command
 * that ran.
 */
export class EstrogenError extends Error {
	/** The report captured up to the failure. */
	readonly report: Report;

	constructor(message: string, report: Report) {
		super(message);
		this.name = 'EstrogenError';
		this.report = report;
	}
}

/**
 * Parse a structured detail object out of estrogen's stdout. estrogen may print a
 * JSON object as its last non-empty line. Return it when present, otherwise
 * undefined. A line that is not JSON is ignored, not an error.
 */
export function parseDetails(stdout: string): unknown {
	const lines = stdout.split('\n');
	for (let i = lines.length - 1; i >= 0; i -= 1) {
		const line = lines[i].trim();
		if (line === '') continue;
		if (line[0] !== '{') return undefined;
		try {
			return JSON.parse(line);
		} catch {
			return undefined;
		}
	}
	return undefined;
}
