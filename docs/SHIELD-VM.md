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
- The program shape is small and stable: a layout, a constant pool, a function
  list, and the main index, where a function carries its code and, only when set,
  a name, the async/arrow/generator flags, and a parameter count. `layout` is the
  per-build opcode byte assignment. Rust emits that JSON directly, with the
  single-character keys described under "The format estrogen emits".
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
does not emit yet. The keys are single characters and a default field is left
out, to keep the encrypted payload small: `l`=layout, `k`=consts, `f`=functions,
`m`=mainIndex (omitted when 0); a function is `c`=code, `n`=name, `a`=isAsync,
`r`=isArrow, `g`=isGenerator, `p`=numParams, each omitted when false, null, or 0.
The unused version tag is gone. The runtime defaults every omitted field, so a
plain sync function with no name and no parameters is just `{ "c": "…" }` rather
than the seven-key form, which saves about seventy bytes per function. `c` is not
a JSON number array: it is a base64 string of the opcode bytes and operands
encoded as unsigned LEB128 varints. Every code value is non-negative, so the
encoding is exact, and it drops the comma and the digits a number array pays per
value. The runtime decodes `c` to a number array once when the program loads, so
the dispatch loop still reads numbers.

```
{
  "l": 3221897405,
  "k": [ … ],
  "f": [ { "c": "DAUi…" }, { "c": "…", "n": "name", "p": 2, "a": 1 } ]
}
```

- `l` is the layout: just the hashed seed, one number. estrogen used to ship the
  whole byte assignment as an `ops` map, one `"byte": [stableIndex, arity]` entry
  for every opcode (all of `OP_NAMES`, about 1.8 KB), inside the encrypted
  program. Now it ships only the hashed seed, and the runtime rebuilds the exact
  byte assignment with the same `mulberry32` PRNG and Fisher-Yates shuffle the
  compiler uses in `vm/layout.rs` (a bit-for-bit port of the shield's
  `buildLayout`); the seed is pre-hashed with FNV-1a so the runtime skips the
  string. Each opcode's arity comes from a static bit string in the runtime, one
  bit per stable index in `OP_NAMES` order, which a `vm/layout.rs` test checks
  against `vm/opcodes.rs`. The shuffle over `OP_NAMES` order is the stable index
  space; the runtime looks a handler up by that index, so the byte assignment is
  data, not textual renaming, and it differs per build. Unused bytes are trap
  slots, and no opcode name is serialized.
- `consts` is the constant pool. A string, a finite number, and a BigInt are the
  values the slice interns. NaN, the infinities, and negative zero use the
  shield's tagged form (`{ "$": "n", "v": "NaN" }` and so on), which the runtime's
  `decodeConst` reverses, because JSON cannot carry them.
- `code` (the `c` field) is the flat opcode array: an opcode byte, then its one
  inline operand when it takes one (a const index, a packed slot address
  `depth << 16 | index`, a jump target as an absolute code index, or an argument
  count). Jump targets are patched to absolute indices after the body is emitted.
  It ships as the base64 LEB128 string described above, which the runtime unpacks
  to this number array on load.
- The main function ends by setting the completion value on the last top-level
  expression statement (`SET_COMPLETION`) and then `HALT`, which returns that
  value. This matches the shield, and it matches native `eval` of the same
  source, which is what the test compares against.

estrogen serializes this to JSON, wraps the JSON in a one-entry ChaCha20 pool, and
emits a chunk shaped like:

```
/* generated by https://estro.help */
const <helper> = (function () { …runtime execution core… return createRuntime; })();
var <pool> = (function () { …ChaCha20 decoder… })();
<helper>(JSON.parse(<pool>(0)))();
```

`<helper>` and `<pool>` are both short generated names in the `$XXXX` shape,
drawn from the file's key material by `FileKeys::decoder_name`. The runtime IIFE
evaluates to the factory; calling it with the decoded program returns the entry
function, which the trailing `()` runs. The old build bound the runtime to the
literal name `__estrogen_vm` and called `.createRuntime(...).run()`; both are
gone, so the chunk carries no readable name for the helper, the factory, or the
entry call.

No string constant and no bytecode ship in the clear: they are inside the
encrypted pool. Every local binding name is gone, because locals compile to slot
indices. So a VM chunk carries no plaintext from the canary list, which the
obfuscator's output scan checks the same way it checks every other chunk.

The runtime execution core ships mangled and minified. `vm/runtime.js` is the
maintainable source, with comments and Prettier formatting (tabs, single quotes).
`vm/mod.rs` `mangle_minify` parses it with OXC, renames every binding in it with
`oxc_mangler` (the runtime is a closed IIFE, so every name is internal and safe
to rename), and prints it with `oxc_codegen` in minify mode, then strips the one
expression statement's trailing semicolon so the result is a bare expression.
Reprinting the AST is the one place the VM pass does it; it is allowed because
the runtime is estrogen's own embedded asset, not a user chunk, so renaming and
reformatting it changes nothing a consumer depends on. The shipped runtime is one
line of short names, with no comments, no opcode names, and nothing that spells
`estrogen`, `createRuntime`, `ARITY`, or any handler name.

The runtime is also tree-shaken per chunk. `runtime.js` defines a handler for
every opcode, but a chunk dispatches only the opcodes its bytecode references, so
each unused `HANDLERS[...]` assignment is weight in a prefix every VM chunk
inlines. `Program::used_opcode_names` walks every function's code, maps each
per-build byte back to its stable opcode through the layout, and collects the set
the chunk actually uses; `prune_handlers` removes the assignment for every other
opcode from the runtime source before `mangle_minify`. This is safe because the
dispatch table fills a byte only when its handler is defined
(`if (handler) handlers[byte] = handler`), so a removed handler leaves a trap
slot, and a trap byte never appears in a code this program runs. A small chunk
keeps a handful of handlers instead of all of them; `a_small_chunk_ships_a_pruned_runtime`
asserts a tiny chunk's whole output is smaller than the full runtime alone and
still runs in node.

`embedded_runtime_has_no_comments` proves the embedded runtime has no comments,
is one line with no run of two or more spaces, and still loads in node.
`vm_chunk_runtime_is_mangled_and_names_free` compiles a VM chunk and asserts its
runtime line holds none of the `OP_NAMES` opcode names and none of the readable
runtime identifiers, and that the chunk is compact (the opcode-name scan runs
against the runtime line, not the whole chunk, because the encrypted program
ships as base64 and a random blob can hold a short opcode substring by chance).
The corpus test runs the whole program suite through the mangled, minified,
numeric-dispatch runtime in node and compares to native `eval`.

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
  their textual position, so recursion works). Static imports and reexports can
  stay native in the emitted ES module while its executable statements run in
  the VM. Imported values are read through live getters. Global reads and
  writes go to `globalThis`, and imports whose names shadow the runtime's free
  builtins take the fallback.
- Functions: declarations, anonymous function expressions, and arrow functions
  with an expression or a block body, each synchronous or `async`. Plain
  identifier parameters, `return`, recursion, nested functions, and lexical
  closure capture. A name resolves to a packed `(depth, index)` slot that walks
  the runtime scope chain, which spans the closure boundary because a closure's
  base scope points at the defining frame's scope. `this` is the call-site
  receiver for a plain function and the defining `this` for an arrow.
- Async and `await`: an `async` function or arrow runs on the runtime's promise
  driver. It executes until it returns or hits `await`, suspends there, and
  resumes when the awaited value settles, pushing the resolved value back or
  routing a rejection through the frame's `catch` handlers. A module body with a
  top-level `await` compiles its main to the same driver, so the chunk evaluates
  to a promise. `AWAIT` and `AWAIT_TOP` carry the suspend; the driver and the
  synchronous loop share one handler stack.
- Classes: a constructor, plain methods, static methods, instance or static
  fields, and `static { ... }` blocks. `BUILD_CLASS` creates the methods and
  binds the class, then runs the static fields and static blocks in source
  order, each with the class as `this`. A class binding resolves to a slot like
  any other name.
- Derived classes: `extends` wires the instance and static prototype chains, and
  BUILD_CLASS pops the superclass value the compiler pushes before it. `super(...)`
  in a constructor constructs the superclass with the active `new.target` through
  `Reflect.construct` and binds the result as `this`, which starts uninitialized
  so using `this` before `super()` throws. `super.prop` and `super.method()` read
  and call through the prototype of the method's home object with `this` as the
  receiver, and `new.target` loads from the frame. The class prototype stays
  non-writable. A derived class's instance fields initialize right after
  `super()` binds `this`: the constructor frame carries the field initializers
  and `SUPER_CALL` runs them on the new receiver, and an implicit derived
  constructor runs them on the `Reflect.construct` result.
- Class accessors: a `get` or `set`, instance or static, installs a merged
  accessor descriptor (`enumerable` false, `configurable` true) on the prototype
  or the class. A plain method still installs a value property.
- Private instance fields (`#x`): each private name is one per-class `WeakMap`
  keyed by the instance, not a string property, so it never shows in
  `Object.keys` or `JSON`. `BUILD_CLASS` creates the maps and threads the private
  env through every class frame and nested closure, so `this.#x` resolves inside
  a method or a closure it returns. `GET_PRIVATE`, `SET_PRIVATE`, and
  `HAS_PRIVATE` read, write, and brand-check (`#x in obj`), throwing a
  `TypeError` on a receiver whose class did not declare the name. A private field
  holding a function keeps its receiver when called as `this.#x()`.
- Control flow: `if`/`else`, `while`, `do`/`while`, C-style `for`, `for`/`of`,
  `for`/`in`, `switch`, `try`/`catch`/`finally`, `throw`, and `break`/`continue`,
  both unlabeled and to a label on an enclosing loop, `switch`, or block. A
  `finally` runs on normal completion, a `throw`, and a `return` that leaves the
  try, with its completion record replayed by `END_FINALLY`; a `return` in the
  `finally` overrides the pending one. A lexical
  block pushes a runtime scope (`ENTER_SCOPE`/`EXIT_SCOPE`); a block with no
  lexical declaration is elided. A `break` or `continue` that jumps out of blocks
  emits the `EXIT_SCOPE`s it skips, so the scope chain is at the depth its
  landing site expects. A labeled jump also closes any one `for`/`of` iterator it
  leaves. A `switch` holds its discriminant in a temp slot, not on the operand
  stack, so a jump out of it never leaves a stray value.
- Literals: number, string, boolean, null, BigInt, and templates with or without
  substitutions. Template substitutions use a runtime string conversion with
  the string hint, including the correct Symbol error behavior.
- Member access (static `a.b`, computed `a[b]`, and optional chains), calls and
  method calls, spread arguments, and `new`.
- Unary operators (`-`, `+`, `!`, `~`, `typeof`, `void`), every plain binary
  operator (arithmetic, comparison, equality, bitwise, shift, `in`,
  `instanceof`), the logical operators (`&&`, `||`, `??`) with short-circuit
  jumps, and the conditional operator.
- Assignment: plain and compound (`+=` and friends) and logical (`&&=`, `||=`,
  `??=`) to an identifier, and plain `=` to a member. Update (`++`/`--`) on an
  identifier, lowered the way the shield lowers it (coerce with `POS`, then add
  `+/-1`), which is correct for number and string operands.
- Array and object literals with plain elements, array holes, spread elements
  and properties, and static object keys. Sequence expressions.
- Function-local `var` with declaration hoisting through nested blocks, loops
  and switch cases. Module-root `var` only when a native import or export
  declaration proves ES module semantics.
- Parameters and the arguments object: default parameters (evaluated only when
  the argument is `undefined`, and able to read an earlier parameter), rest
  parameters bound to a real array, and `Function.length` counting the leading
  plain parameters. A non-arrow function reads an unmapped `arguments` object; an
  arrow inherits the enclosing function's. Named function expressions bind their
  own name to the function inside the body, so recursion works and the name does
  not leak out.
- Destructuring, as a `let`/`const`/`var` binding, an assignment target, a
  function parameter, or a `for`/`of` or `for`/`in` head: object patterns
  (renaming, defaults, computed keys, and `...rest`) and array patterns (holes,
  defaults, nesting, and `...rest`). Array patterns walk the iterator protocol,
  so a string, Set, or Map destructures like the engine does. A destructuring
  leaf can be a nested pattern or, for an assignment, an identifier or a member.
  The `...rest` of an object with a computed key before it builds its
  excluded-key set at runtime. A destructured parameter without a default counts
  toward `Function.length`; a rest parameter can itself be a pattern.
- Generators: a `function*` declaration or expression runs on the runtime's
  generator driver. Calling it returns a generator object; `next(v)` runs the
  body to the next `yield` or the end, `yield` suspends with its value and
  resumes with the value `next` was given, and spread, `for`/`of`, and manual
  `next`/`return` all consume it (a `for`/`of` break calls the generator's
  `return`). `yield*` delegates to another iterator, forwarding its values and
  return. The body must not use `try`, `for`/`of`, or `for`/`in`, since those
  keep a handler on the frame across a `yield`.
- Async generators: an `async function*` runs on the runtime's async-generator
  driver. Calling it returns an async iterator whose `next`/`throw`/`return`
  return promises of `{ value, done }`, queued so they settle in order. The body
  may both `await` and `yield`; an awaited rejection propagates out of the
  pending `next`. `yield*` in an async generator and a consumer `for await` are
  not ported, and the same `try`/`for`/`of`/`for`/`in` restriction applies.

Falls back (declines, so the literal and name passes run instead):

- A decorated class, a computed method, field, or accessor key, an `async` or
  generator method, private class members, a computed super
  member, and a spread in `super()`. Native
  static imports and reexports work, but the VM's full module loader is not
  ported, so a local export of a value computed in the VM beyond the simple
  cases takes the fallback.
- A labeled `break`/`continue` across a `try`, an escaping `break`/`continue`
  out of a `for`/`of` (its iterator close is not routed through the finally
  chain), and a `break`/`continue` inside a `finally` that leaves it (which would
  abandon the pending completion). An unlabeled `break`/`continue` that escapes a
  plain loop through a `try`/`catch` or `try`/`finally` compiles: a crossed catch
  is popped and a crossed finally runs first, through `ABRUPT_JUMP`. A `return`
  that leaves a `try` while a `for`/`of` iterator is still open; a `switch` case
  that declares a lexical binding; and a labeled jump that would close more than
  one `for`/`of` iterator at once.
- A generator whose own body uses `try`, `for`/`of`, or `for`/`in`; `yield*`
  inside an async generator; and a `for await` loop.
- A `for (let ...)` whose test, update, or body makes a closure: estrogen uses one
  binding where native makes one per iteration, so it declines rather than
  observe the difference wrongly. The closure check is conservative, so a
  `for (let ...)` whose body contains a `for`/`of`, `for`/`in`, `try`, or labeled
  statement also declines for now.
- Root `var` when the caller cannot prove ES module scope, `using`, a BigInt
  operand of `++`/`--`, method object keys, tagged templates, and `yield`.

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
the embedded runtime has no comments, is one minified line, and still loads in
node. A fourth compiles a function chunk whose string value, local name, and
function name are all canaries and asserts none survives in the clear. A fifth,
`vm_chunk_runtime_is_mangled_and_names_free`, asserts the shipped runtime line
holds none of the `OP_NAMES` opcode names and none of the readable runtime
identifiers (`estrogen`, `__estrogen_vm`, `ARITY`, `createRuntime`, the handler
helper names). If `node` is not on PATH the running tests print a SKIP rather than
passing without verifying.

The Node corpus now also covers optional chains, spread argument calls, array
holes, object spreads, template substitutions, and function-local `var`
hoisting. Additional tests execute native ES modules with imported live bindings
and reexports, comparing the VM replacement against the original module.
Free global writes, global accessor receivers, and imported builtin shadowing
have regression checks. The raw `compile_module_json` test helper still declines
module syntax because it has no native wrapper.

At the obfuscator level, `obfuscator.rs` and `api.rs` tests assert that a
compilable chunk becomes a VM chunk, that unsupported local export declarations
take the literal fallback, and that a VM chunk whose string
value and binding name are both on the canary list leaks neither (the string is
encrypted, the name is a slot), so the build does not fail closed on it. They key
on `JSON.parse(` as the VM-chunk marker, since the mangled runtime no longer
ships a `createRuntime` or `.run()` token to key on.

## Remaining work to full default-on coverage

Functions, closures, `async`/`await`, non-derived classes, `try`/`catch`,
`try`/`finally`, `for`/`of`, `for`/`in`, labeled jumps, the arguments object,
named function expressions, default and rest parameters, destructuring, and
generators now compile (see the subset above). Reaching the point where the VM
compiles a real app bundle, the way the shield's 25-of-25 integration proof
does, is still a large amount of further work. In rough dependency order:

1. The rest of generators: a `try`, `for`/`of`, or `for`/`in` inside a generator
   body, `yield*` inside an async generator, and a `for await` consumer, which
   the completion-record and iterator-close machinery can extend to.
2. The rest of control flow: a labeled jump across a `try`, an escaping jump out
   of a `for`/`of` (routing the iterator close through the finally chain), a
   `break`/`continue` that leaves a `finally`, a `switch` case that declares a
   lexical binding, and the conservative closure check that makes a
   `for (let ...)` containing a `for`/`of`, `try`, or labeled statement decline.
3. ES modules and the loader: static imports and native reexports have a hybrid
   wrapper now. Locally declared exports, the module descriptor (`imports`,
   `exports`, `reexports`, `deps`, `exportSlots`), the module ops, and the shield
   `loader-template.js` still need a full port. A real Vite graph also has
   third-party code with unsupported syntax, so the main estro.help editor
   bundle still uses literal-pool fallback.
4. The rest of the class surface: private state, computed method, field, and
   accessor keys, `async` and generator methods, a computed super
   member, and a spread in `super()`. The common derived
   case (`extends`, `super(...)`, `super.method()`/`super.prop`, `new.target`,
   instance fields, getters and setters, the prototype chain) compiles.
5. The soundness fixpoint the shield computes (no native chunk may import a VM
   chunk), the emit-time guard against a chunk naming a renamed sibling file, and
   the per-chunk integrity hash and anti-tamper checks (P7), which estrogen does
   not emit yet.
6. The rest of the expression surface: computed or method object-literal keys, a
   BigInt operand of `++`/`--`, tagged templates, and the other forms the
   compiler declines.
7. A shared runtime bootstrap. Today a VM chunk inlines the runtime, which is
   fine for the import-free chunks the slice compiles but would duplicate the
   runtime across many chunks once the loader lands. The shield emits one
   bootstrap and per-chunk register artifacts; estrogen's vendor pipeline is the
   place to ship a shared runtime once there is more than one VM chunk per build.

Until those land, the default-on VM protects the chunks it can compile and falls
back for the rest, and the canary scan and re-parse keep every chunk safe either
way.

## estro.help build coverage

`web/src/scripts/site-runtime.js` has the live footer clock and service worker
registration. Astro emits it as a separate hashed JavaScript asset through
`?url`, with `assetsInlineLimit: 0`. The estrogen build integration compiles
that file into VM bytecode. `web/scripts/verify-vm.mjs` fails the Astro build
if the output is missing its VM marker, and compares the VM result against the
source in Node at seven clock ages, with missing DOM nodes and service worker
registration covered too.

The main editor bundle still uses the literal-pool fallback. The bundled
CodeMirror and file utilities include top-level `var`, `try` statements,
`for..of`, classes, and other unsupported syntax. The separate site runtime
proves that an actual browser loads VM bytecode, but does not claim every
JavaScript asset on the site is VM compiled.
