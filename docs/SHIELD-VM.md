# Bytecode VM pass

estrogen can compile an app chunk to encrypted stack bytecode and ship a small
runtime that runs it, instead of editing the chunk's literals in place. This is
the default transform for a non-vendor JS chunk. A chunk the VM cannot compile
yet falls back to the literal-pool and name passes, so no chunk is left
unprotected.

This raises the cost of reading a build. It is not secrecy. The pool key ships
in the output next to the decoder, and the VM runs in the page, so anyone with
devtools can run the code back. The README says the same about the rest of
estrogen, and it is true here too: the VM raises reading cost, it does not hide
anything.

The mechanism is a port of abulxt's shield VM
(`~/src/abulxt/main/src/server/shield/`). The shield is TypeScript on acorn,
shipped as a Vite plugin. estrogen is one Rust binary on OXC. So the port is the
compiler (AST to bytecode) rewritten in Rust against OXC, emitting the exact
program shape the shield runtime runs, plus the runtime's execution core
vendored into the repo.

## The choice: port the compiler, reuse the runtime

The brief named two options.

- (a) Port the compiler (AST to bytecode) to Rust against OXC's AST, and ship
  the shield's `runtime-template.js` as the runtime, with the Rust side emitting
  the exact bytecode and constant-pool format the runtime executes.
- (b) Something else.

This pass takes (a), with one deliberate change to the outer layer. The reasons:

- The bytecode and the runtime are two halves of one contract. The runtime
  already exists, is tested against native `eval` in the shield's own corpus, and
  is plain JavaScript with no Node built-ins, so it runs unchanged in a browser.
  Rewriting it in Rust would mean reimplementing and re-proving a 977-line
  interpreter for no gain. Porting only the compiler keeps the proven half.
- The program shape is small and stable: `{ version, layout, consts, functions,
mainIndex }`, where a function is `{ code, name, isAsync, isArrow, numParams }`
  and `layout` is the per-build opcode byte assignment. Rust emits that JSON
  directly.
- The opcode semantics live in the runtime's handler table. estrogen's compiler
  emits bytecode the runtime already knows how to run, so the runtime defines
  correctness and the Rust side is checked against it by running real programs
  (see "The proof").

The one change from the shield is the outer encryption layer. The shield wraps
the serialized program in XChaCha20-Poly1305 from `@noble/ciphers`. estrogen wraps
it in its own ChaCha20 pool instead (`format.rs`, the same decoder the literal
pass already ships and the same decoder the symmetry test already covers). The
reasons:

- estrogen already has a tested, symmetric ChaCha20 encoder and emitted JS
  decoder. Reusing it adds no dependency and no second cipher to audit.
- The Poly1305 tag is not a security boundary here. The key ships in the file, so
  authentication only tells a reader that they edited the bytecode, which they
  can see anyway. estrogen drops it and keeps the cost-raising part.
- The decoder runs in the browser with `atob`, `Uint8Array` and `TextDecoder`,
  which the literal pool already relies on, so there is nothing new to load.

The result reuses the shield runtime's execution core verbatim (the dispatch
loop, the slot scope model, and every opcode handler body estrogen emits), and
swaps only the decrypt wrapper. The vendored runtime is
`crates/estrogen-core/src/vm/runtime.js`.

## The format estrogen emits

A program is the shield's `ShieldProgram`, minus the module descriptor the slice
does not emit yet:

```
{
  "version": 1,
  "layout":  { "version": 1, "tableSize": 256, "byteOf": {…}, "nameOf": {…} },
  "consts":  [ … ],
  "functions": [ { "code": [ … ], "name": null, "isAsync": false,
                   "isArrow": false, "numParams": 0 } ],
  "mainIndex": 0
}
```

- `layout` is the per-build opcode byte assignment. `byteOf` maps an opcode name
  to its byte in the code stream; `nameOf` is the inverse. estrogen builds it with
  `vm/layout.rs`, a bit-for-bit port of the shield's `buildLayout`: the same
  `mulberry32` PRNG, the same FNV-1a `hashSeed`, and the same Fisher-Yates
  shuffle over the opcode names in `vm/opcodes.rs`, whose order matches the
  shield's `ARITY` object. The runtime rebuilds its dispatch table from `nameOf`,
  so the byte assignment is data, not textual renaming, and it differs per build.
  Unused bytes are trap slots.
- `consts` is the constant pool. A string, a finite number, and a BigInt are the
  values the slice interns. NaN, the infinities, and negative zero use the
  shield's tagged form (`{ "$": "n", "v": "NaN" }` and so on), which the runtime's
  `decodeConst` reverses, because JSON cannot carry them.
- `code` is the flat opcode array: an opcode byte, then its one inline operand
  when it takes one (a const index, a packed slot address `depth << 16 | index`, a
  jump target as an absolute code index, or an argument count). Jump targets are
  patched to absolute indices after the body is emitted.
- The main function ends by setting the completion value on the last top-level
  expression statement (`SET_COMPLETION`) and then `HALT`, which returns that
  value. This matches the shield, and it matches native `eval` of the same
  source, which is what the test compares against.

estrogen serializes this to JSON, wraps the JSON in a one-entry ChaCha20 pool, and
emits a chunk shaped like:

```
/* generated by https://estro.help */
const __estrogen_vm = (function () { …runtime execution core… return { createRuntime }; })();
var <pool> = (function () { …ChaCha20 decoder… })();
__estrogen_vm.createRuntime(JSON.parse(<pool>(0))).run();
```

No string constant and no bytecode ship in the clear: they are inside the
encrypted pool. Every local binding name is gone, because locals compile to slot
indices. So a VM chunk carries no plaintext from the canary list, which the
obfuscator's output scan checks the same way it checks every other chunk.

The runtime execution core ships minified. `vm/runtime.js` is the maintainable
source, with comments and Prettier formatting; `vm/mod.rs` strips the comments
and collapses the whitespace before it inlines the body, so the shipped runtime
is one compact line and does not read like annotated source. The minifier is a
small tokenizer, not a text pass: it tracks string and template literals, so a
`//` inside a string is left alone, and `runtime.js` has no regex literals, so a
`/` is only ever a comment start or an operator. `embedded_runtime_has_no_comments`
proves the embedded runtime has no comments and still loads in node, and the
corpus test runs the whole program suite through the minified runtime.

## Where it sits in the pass architecture

estrogen's other passes edit the source bytes and splice (`edit.rs`): a pass returns
an `EditSet` of byte-range edits, and the obfuscator applies them without
reprinting the AST. The VM does not fit that model, because it replaces the whole
chunk with a runtime and a program. It is wired at the one point where a chunk's
transform is chosen, `Obfuscator::obfuscate_file_in` in `obfuscator.rs`, not as a
`Pass` implementation:

- For a non-vendor JS chunk, the obfuscator calls `try_vm`, which calls
  `vm::try_compile_chunk`.
- On success, the whole-chunk replacement is represented as one edit,
  `Edit::replace(0, source.len(), replacement)`, so it flows through the same
  apply, re-parse, and canary-scan path as every other transform. The emitted
  chunk is re-parsed (`ensure_js_reparses`) and scanned for canaries before it is
  written, so a VM bug fails closed like any other.
- On decline, the obfuscator falls back to the literal-pool pass
  (`LiteralsPass`), which is the former default. The `mangle` pass runs inside
  `LiteralsPass` as before.

The app-vs-vendor split is unchanged. A vendor-glob JS file still takes the
proxy-name path (`collect_proxy_edits`); only a non-vendor app chunk is offered
to the VM. HTML and SVG still take the markup pass. So the VM composes with the
existing classification rather than changing it.

A VM chunk is a full replacement, so the bundler `sourceMappingURL` strip and the
private source map do not apply to it (the replacement carries neither, and a
strip edit would overlap the whole-file replacement). The obfuscator skips both
for a VM chunk.

## How it composes with the literal and name passes

For an app chunk, the VM is the first choice and the literal-pool plus name
passes are the fallback, chunk by chunk. The VM either compiles a whole chunk or
declines it; it never compiles half a chunk. So a chunk is transformed by exactly
one path:

- The VM path, when every construct in the chunk is in the subset. The chunk
  becomes encrypted bytecode. The literal and name passes do not run on it,
  because the VM already removes the plaintext (strings into the encrypted pool,
  names into slots).
- The literal-pool plus name path, when any construct is outside the subset. The
  chunk is protected exactly as before this pass existed.

This mirrors the shield's `@no-vm` fallback: a chunk the VM cannot compile
degrades to the other protection instead of breaking.

## Default-on wiring

The VM is on by default.

- Library: `Options.vm` is a `bool` that defaults to `true` (`options.rs`). A
  config file sets it under `[options]` as `vm = false` to turn it off.
- CLI: `--no-vm` sets `options.vm = false` (`estrogen-cli/src/cli.rs`). With no
  flag, the VM is on.
- npm and the plugins call the same library `Options`, so they inherit the
  default. No plugin change is needed to turn the VM on, and a consumer that
  wants the old behavior passes `vm: false` (the plugins map this the same way
  they map `check` and the rest).

Turning the VM off never changes whether an app chunk is protected, only which
transform it takes. With the VM off, every app chunk takes the literal-pool plus
name path, which is what estrogen did before this pass.

## Source map interop

A worker session is adding V3 source-map emission from the edit list. The VM
output is not an edit list over the original source: it is a replacement whose
bytes do not correspond line-by-line to the input. So the mapping story for a VM
chunk is explicit and simple: a VM chunk has no source map.

- estrogen already writes no map next to the served build by default, and it
  strips any bundler `sourceMappingURL`. A VM chunk keeps that default: it ships
  no map and no map comment.
- The private debug map behind `--sourcemap <dir>` is built from the edit list,
  which a VM chunk does not have, so the obfuscator does not write a private map
  for a VM chunk. The map still covers every chunk that took the literal, proxy,
  or markup path, because those are edit-list transforms.
- This is not a regression against the default, which ships no map either way.
  The one honest limitation is that a developer who wants a private debug map for
  a specific chunk does not get one while that chunk is VM-compiled. Turning the
  VM off for a debugging build restores the edit-list map for that chunk.

A later version could emit a coarse map (every VM chunk byte maps to the chunk's
first line) so a stack trace still names the file. That is not built yet, and the
doc does not claim it.

## What runs today, and what falls back

The compiler handles a sound subset. Everything else declines, and the chunk
falls back. The subset:

- A module body of expression statements, `let`/`const` declarations with plain
  identifier bindings, and function declarations (hoisted and callable before
  their textual position, so recursion works).
- Functions: declarations, anonymous function expressions, and arrow functions
  with an expression or a block body. Plain identifier parameters, `return`,
  recursion, nested functions, and lexical closure capture. A name resolves to a
  packed `(depth, index)` slot that walks the runtime scope chain, which spans
  the closure boundary because a closure's base scope points at the defining
  frame's scope. `this` is the call-site receiver for a plain function and the
  defining `this` for an arrow.
- Control flow: `if`/`else`, `while`, `do`/`while`, C-style `for`, `switch`, and
  unlabeled `break`/`continue`. A lexical block pushes a runtime scope
  (`ENTER_SCOPE`/`EXIT_SCOPE`); a block with no lexical declaration is elided. A
  `break` or `continue` that jumps out of blocks emits the `EXIT_SCOPE`s it
  skips, so the scope chain is at the depth its landing site expects. A `switch`
  holds its discriminant in a temp slot, not on the operand stack, so a jump out
  of it never leaves a stray value.
- Literals: number, string, boolean, null, BigInt, and a template with no
  substitutions.
- Member access (static `a.b` and computed `a[b]`), calls and method calls, and
  `new`.
- Unary operators (`-`, `+`, `!`, `~`, `typeof`, `void`), every plain binary
  operator (arithmetic, comparison, equality, bitwise, shift, `in`,
  `instanceof`), the logical operators (`&&`, `||`, `??`) with short-circuit
  jumps, and the conditional operator.
- Assignment: plain and compound (`+=` and friends) and logical (`&&=`, `||=`,
  `??=`) to an identifier, and plain `=` to a member. Update (`++`/`--`) on an
  identifier, lowered the way the shield lowers it (coerce with `POS`, then add
  `+/-1`), which is correct for number and string operands.
- Array and object literals with plain elements and static keys, and sequence
  expressions.

Falls back (declines, so the literal and name passes run instead):

- Classes, `import`, and `export` (the whole-chunk loader is not ported yet, so
  a module with imports or exports is out of scope).
- `try`/`catch`/`finally`, for-of and for-in, labeled `break`/`continue`, and a
  `switch` case that declares a lexical binding.
- Named function expressions, default, rest, and destructured parameters,
  generators, `async`, and `arguments`.
- A `for (let ...)` whose test, update, or body makes a closure: estrogen uses one
  binding where native makes one per iteration, so it declines rather than
  observe the difference wrongly.
- `var` and `using`, destructuring bindings, compound or update assignment to a
  member, a BigInt operand of `++`/`--`, optional chaining, spread, templates
  with substitutions, tagged templates, `await`, and `yield`.

## The proof

`crates/estrogen-core/tests/vm_node.rs` compiles a corpus of supported sources to
VM programs in Rust, then runs each program through the vendored runtime in
`node` and compares the result to native `eval` of the same source. A mismatch
fails the test. The corpus covers the expression subset (literals, slot locals,
globals, member access, calls and method calls, `new`, every operator group,
arrays, objects, sequences, and a no-substitution template); functions and
closures (recursion with factorial, a closure counter, an arrow capturing an
outer variable, a higher-order map and reduce, nested functions, a method call
reading `this`, and postfix update); and control flow (if/else, while, do/while,
a C-style for summing an array, break, continue, switch with break, default, and
fall-through, and nested loops with inner break/continue and a block-scoped
local, which proves jump backpatching and scope unwinding). A second test asserts
that each out-of-subset construct declines, including a `for (let ...)` captured
by a closure, so the fallback classification is checked too. A third test proves
the embedded runtime has no comments and still loads in node. A fourth compiles a
function chunk whose string value, local name, and function name are all canaries
and asserts none survives in the clear. If `node` is not on PATH the running
tests print a SKIP rather than passing without verifying.

At the obfuscator level, `obfuscator.rs` tests assert that a compilable chunk
becomes a VM chunk, that a chunk outside the subset (an `export`) falls back to
the literal decoder, and that a VM chunk whose string value and binding name are
both on the canary list leaks neither (the string is encrypted, the name is a
slot), so the build does not fail closed on it.

## Remaining work to full default-on coverage

Functions, closures, and the common control-flow statements now compile (see the
subset above). Reaching the point where the VM compiles a real app bundle, the
way the shield's 25-of-25 integration proof does, is still a large amount of
further work. In rough dependency order:

1. The rest of the function surface: `arguments`, named function expressions,
   default, rest, and destructured parameters, generators, and `async`.
2. The rest of control flow: the `try`/`catch`/`finally` handler stack and
   `break`/`continue` across a `finally` (which the shield routes through
   completion records), for-of and for-in, labeled `break`/`continue`, and a
   `switch` case that declares a lexical binding.
3. ES modules and the loader: the module descriptor (`imports`, `exports`,
   `reexports`, `deps`, `exportSlots`), the module ops, and a port of the shield's
   `loader-template.js` so VM chunks can import each other and native chunks. A
   real build needs the loader, because chunks are an import graph. Until then the
   VM only fires on an import-free, export-free chunk, which is rare in a Vite
   build, so on a real build almost every chunk falls back today. That is honest
   and expected for a slice; it is not a claim that the VM protects a real bundle
   yet.
4. Classes: `BUILD_CLASS`, methods, fields, private state, `extends`/`super`, and
   `new.target`.
5. async/await and top-level await: the async frame driver and `AWAIT`.
6. The soundness fixpoint the shield computes (no native chunk may import a VM
   chunk), the emit-time guard against a chunk naming a renamed sibling file, and
   the per-chunk integrity hash and anti-tamper checks (P7), which estrogen does
   not emit yet.
7. The rest of the expression surface: destructuring, compound and update
   assignment to a member, a BigInt operand of `++`/`--`, templates with
   substitutions, spread, optional chaining, and the other forms the compiler
   declines.
8. A shared runtime bootstrap. Today a VM chunk inlines the runtime, which is
   fine for the import-free chunks the slice compiles but would duplicate the
   runtime across many chunks once the loader lands. The shield emits one
   bootstrap and per-chunk register artifacts; estrogen's vendor pipeline is the
   place to ship a shared runtime once there is more than one VM chunk per build.

Until those land, the default-on VM protects the chunks it can compile and falls
back for the rest, and the canary scan and re-parse keep every chunk safe either
way.
