/**
 * Options shared by the programmatic API ({@link obfuscate}) and the bundler
 * plugins. Each field maps to one crack CLI flag, so the same object configures
 * a direct spawn and a plugin run.
 *
 * crack rewrites a directory that a bundler has already written. It raises the
 * cost of reading that build. It is not secrecy: a decoder key ships in each
 * file, and anyone with devtools can run the code.
 */
export interface Options {
	/**
	 * Directories to rewrite in place. Pass one string or a list. The CLI accepts
	 * `crack <dir>...`, so a list runs one process over every directory.
	 *
	 * The plugins fill this in from the build's output directory, so plugin
	 * callers leave it unset. A direct {@link obfuscate} call must set it.
	 */
	dirs?: string | string[];

	/**
	 * Config file for crack (`-c`/`--config <file>`). Per-site settings that do
	 * not fit a flag live here.
	 */
	config?: string;

	/**
	 * Canary list file (`--canary <file>`). The list of literals that must not
	 * survive verbatim in the output. crack fails nonzero if one does.
	 */
	canary?: string;

	/**
	 * Names file (`--names <file>`). The proxy and store names to rename for this
	 * site.
	 */
	names?: string;

	/**
	 * Source map output directory (`--sourcemap <dir>`). Off by default. When set,
	 * crack writes maps here, outside the served tree, for private debugging. A
	 * map traces the output back to its source, so keep this directory private
	 * and never ship it.
	 */
	sourcemap?: string;

	/**
	 * Seed for the per-file and per-build randomness (`--seed <hex>`). Fixes the
	 * output so two runs on one input match. Use it for tests and parity harnesses
	 * only. Production builds leave it unset and use real randomness.
	 */
	seed?: string;

	/**
	 * Worker count (`-j`/`--jobs <n>`). crack processes files in parallel; this
	 * caps the workers. Unset lets the binary pick.
	 */
	jobs?: number;

	/**
	 * Check mode (`--check`). Report what crack would do and run the canary check
	 * without writing changes. Useful in CI to gate a build.
	 */
	check?: boolean;

	/** Quiet output (`--quiet`). Suppresses progress logging. */
	quiet?: boolean;

	/** Verbose output (`-v`/`--verbose`). Logs per-file detail. */
	verbose?: boolean;

	/**
	 * Path to the crack binary. Overrides the per-platform package resolution.
	 * Set it to run a locally built binary or a stub in tests. Normally leave it
	 * unset and let the matching `@sysce/crack-<os>-<cpu>` package provide it.
	 */
	binaryPath?: string;

	/** Working directory for the spawned process. Defaults to `process.cwd()`. */
	cwd?: string;

	/**
	 * Extra arguments appended after the flags and before the directories. An
	 * escape hatch for flags this type does not model yet. Prefer a typed field
	 * when one exists.
	 */
	extraArgs?: string[];
}

/**
 * Plugin options: {@link Options} without `dirs`, which a plugin takes from the
 * build's output directory. Everything else carries through to crack.
 */
export type PluginOptions = Omit<Options, 'dirs'>;

/**
 * Normalize {@link Options.dirs} to a list. Returns an empty list when unset.
 */
export function normalizeDirs(dirs: Options['dirs']): string[] {
	if (dirs === undefined) return [];
	return Array.isArray(dirs) ? dirs.slice() : [dirs];
}

/**
 * Turn {@link Options} into the crack CLI argument vector. The order is flags
 * first, then `extraArgs`, then the directories, matching `crack [options]
 * <dir>...`.
 */
export function buildArgs(options: Options): string[] {
	const args: string[] = [];
	if (options.config !== undefined) args.push('--config', options.config);
	if (options.canary !== undefined) args.push('--canary', options.canary);
	if (options.names !== undefined) args.push('--names', options.names);
	if (options.sourcemap !== undefined)
		args.push('--sourcemap', options.sourcemap);
	if (options.seed !== undefined) args.push('--seed', options.seed);
	if (options.jobs !== undefined) args.push('--jobs', String(options.jobs));
	if (options.check) args.push('--check');
	if (options.quiet) args.push('--quiet');
	if (options.verbose) args.push('--verbose');
	if (options.extraArgs) args.push(...options.extraArgs);
	args.push(...normalizeDirs(options.dirs));
	return args;
}
