/* tslint:disable */
/* eslint-disable */
/**
 * Obfuscate one JavaScript source string.
 *
 * `source` is the code to obfuscate. `seed`, when a non-empty string, makes the
 * output deterministic: the same source and seed give byte-identical output, so
 * the demo's seed field repeats a build. An empty or absent seed draws fresh
 * per-file keys from the browser's crypto.
 *
 * Returns a JSON string `{"code":...,"edits":...}` for the caller to parse. A
 * parse error, a pass failure, or a canary surviving in the output rejects with
 * the engine's error string; the demo shows it and keeps the previous output.
 */
export function obfuscate(source: string, seed?: string | null): string;
/**
 * estrogen's version, so the demo can print the engine it is running.
 */
export function version(): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
  readonly memory: WebAssembly.Memory;
  readonly obfuscate: (a: number, b: number, c: number, d: number, e: number) => void;
  readonly version: (a: number) => void;
  readonly __wbindgen_export_0: (a: number) => void;
  readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
  readonly __wbindgen_export_1: (a: number, b: number) => number;
  readonly __wbindgen_export_2: (a: number, b: number, c: number, d: number) => number;
  readonly __wbindgen_export_3: (a: number, b: number, c: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;
/**
* Instantiates the given `module`, which can either be bytes or
* a precompiled `WebAssembly.Module`.
*
* @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
*
* @returns {InitOutput}
*/
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
* If `module_or_path` is {RequestInfo} or {URL}, makes a request and
* for everything else, calls `WebAssembly.instantiate` directly.
*
* @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
*
* @returns {Promise<InitOutput>}
*/
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
