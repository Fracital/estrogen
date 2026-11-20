/**
 * Return a `require` function anchored in this package. The two builds provide
 * it differently: the CommonJS build returns its own `require`, and the ESM
 * build builds one with `createRequire(import.meta.url)`. Keeping that one line
 * out of the shared TypeScript lets the same source compile to both module
 * formats without `import.meta` breaking the CommonJS compile.
 */
export declare function getRequire(): NodeRequire;
