import { join } from 'node:path';
import { arch as processArch, platform as processPlatform } from 'node:process';

/**
 * The npm name of the per-platform binary package for each supported target.
 * Keyed by `<os>-<cpu>` where os is `process.platform` and cpu is
 * `process.arch`.
 */
export const PLATFORM_PACKAGES: Record<string, string> = {
	'linux-x64': '@sysce/crack-linux-x64',
	'linux-arm64': '@sysce/crack-linux-arm64',
	'darwin-x64': '@sysce/crack-darwin-x64',
	'darwin-arm64': '@sysce/crack-darwin-arm64',
	'win32-x64': '@sysce/crack-win32-x64',
};

/**
 * The `<os>-<cpu>` target key for a platform and arch, or null when the pair is
 * not a supported target. Defaults to the running process.
 */
export function targetKey(
	platform: string = processPlatform,
	arch: string = processArch,
): string | null {
	const key = `${platform}-${arch}`;
	return key in PLATFORM_PACKAGES ? key : null;
}

/**
 * The npm package name that carries the binary for a platform and arch, or null
 * when unsupported. Defaults to the running process.
 */
export function platformPackageName(
	platform: string = processPlatform,
	arch: string = processArch,
): string | null {
	const key = targetKey(platform, arch);
	return key === null ? null : PLATFORM_PACKAGES[key];
}

/**
 * The binary's path inside its package. Windows carries `bin/crack.exe`, every
 * other target `bin/crack`.
 */
export function binSubpath(platform: string = processPlatform): string {
	return platform === 'win32' ? join('bin', 'crack.exe') : join('bin', 'crack');
}

/**
 * Thrown when no per-platform binary package resolves for the running system.
 * The message names the target and the package npm would have installed.
 */
export class MissingBinaryError extends Error {
	constructor(message: string) {
		super(message);
		this.name = 'MissingBinaryError';
	}
}

/**
 * Locate the crack binary for the running platform.
 *
 * Resolution order:
 * 1. `override`, when given, is returned as is (the `binaryPath` option and the
 *    bin launcher's own override use this).
 * 2. Otherwise the matching `@sysce/crack-<os>-<cpu>` package is resolved
 *    through `req.resolve` and the binary path inside it is returned.
 *
 * `req` is a `require` function anchored in the launcher package, so resolution
 * finds the optional dependency whether npm installed it beside the launcher or
 * hoisted it to a parent `node_modules`. Throws {@link MissingBinaryError} when
 * the platform is unsupported or the package is not installed.
 */
export function resolveBinaryPath(req: NodeRequire, override?: string): string {
	if (override !== undefined && override !== '') return override;

	const key = targetKey();
	const pkg = platformPackageName();
	if (key === null || pkg === null) {
		const supported = Object.keys(PLATFORM_PACKAGES).join(', ');
		throw new MissingBinaryError(
			`crack has no binary for ${processPlatform}-${processArch}. ` +
				`Supported targets: ${supported}.`,
		);
	}

	try {
		const pkgJson = req.resolve(`${pkg}/package.json`);
		const pkgDir = pkgJson.slice(0, pkgJson.length - '/package.json'.length);
		return join(pkgDir, binSubpath());
	} catch {
		throw new MissingBinaryError(
			`crack could not find its binary package ${pkg} for ${key}. ` +
				`Install it, for example with "npm install ${pkg}", ` +
				`or reinstall @sysce/crack so the optional dependency is picked up.`,
		);
	}
}
