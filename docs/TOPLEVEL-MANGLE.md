# Top-level mangling and export-name preservation

estrogen's local rename pass (`crates/estrogen-core/src/pass/mangle.rs`, driven
from `pass/literals.rs`) shortens bindings with `oxc_mangler`. Until now it ran
with `top_level: Some(false)`, so it renamed locals inside functions and blocks
and left every module top-level name alone. This document records why top-level
renaming is safe for the chunks estrogen processes, where it is not, and how the
pass keeps a public export name while hiding the binding behind it.

## What a chunk is

estrogen rewrites one already-built file at a time. A post-build app chunk from
Vite or esbuild is a single ES module: it has its own top-level scope, it reaches
the outside world only through its `import` and `export` statements, and nothing
else links another file's code to a name inside it. The two consumers of a name
in such a file are:

- other code in the same file, which the parser resolves to the same binding, and
- an importer in another file, which binds by the public export name.

So a top-level binding can be renamed as long as every reference in the file moves
with it and every `export` that exposes it keeps the public name it had.

## When renaming a top-level binding is sound

A top-level binding is safe to rename when all of these hold:

- The file is an ES module. A module's top-level `var`, `let`, `const`,
  `function` and `class` are scoped to the module, not to the global object.
- Every reference to the binding is renamed with it. `oxc_mangler` resolves each
  reference to its symbol, so a consistent rename is a symbol rename, not a text
  substitution.
- The binding is not reached by a name the engine resolves at run time that the
  rename cannot follow: a `with` block (the pass already bails out of the whole
  file when it sees one), a direct `eval` scope (oxc reserves those bindings), or
  an exported name read by an importer.

For an exported binding the last point is the whole problem. The fix is to split
the two roles the name plays. `export { proxyRouteCookie }` both names a local
binding and declares a public export `proxyRouteCookie`. Rename the binding to
`a` and rewrite the statement to `export { a as proxyRouteCookie }`: the binding
is hidden, every in-file reference points at `a`, and the public export name is
unchanged, so importers still resolve it.

## When renaming is not sound, and the pass leaves it alone

- **A classic script.** Top-level `var` and `function` in a non-module script
  become properties of the global object, which other scripts read by name.
  estrogen cannot always tell a script from a module by extension, so it turns
  top-level renaming on only when the file actually contains `import` or `export`
  syntax. That syntax is a parse error in a script, so its presence proves the
  file is a module. A file with no module syntax keeps the old behaviour: locals
  only, top level untouched.
- **An imported binding.** `import { foo } from './m'` binds `foo` to `./m`'s
  export named `foo`. The imported name is the other module's contract. The pass
  reserves every import local name (default, namespace and named) so oxc never
  renames one, and it never rewrites an import specifier.
- **A free or global name.** A reference the file does not bind (`console`,
  `window`, a global the bundle expects) has no symbol to rename, and a new
  top-level binding must not shadow one. The fresh-name allocator (below) refuses
  any name that appears as a free reference in the file.
- **A name reached through a string the engine reads.** A property key, a
  `export { x as "name" }` public name, or a name built for `eval` is text the
  rename cannot follow. The pass keeps every such string exactly.
- **Anything ambiguous.** Where a construct is not clearly one of the handled
  shapes, the pass leaves the binding unmangled rather than guess.

## The export name is a visible literal, not an expression

A frequent question is whether the public export name can be hidden too, for
example decoded from the string pool at load time like the literals are. It
cannot. A static `export` names its export with an `IdentifierName` or a string
`StringLiteral`, fixed in the module's source text. The grammar has no form where
the exported name is an expression evaluated at run time:

```js
export { a as proxyRouteCookie }     // identifier, allowed
export { a as "proxyRouteCookie" }   // string literal, allowed (ES2022)
export { a as (decode(0)) }          // not valid: an expression is a syntax error
```

The reason is structural, not a parser limit estrogen could work around. A
module's export names form its static export list, which the host reads before
any of the module's code runs (an importer's binding is created during linking,
before evaluation). A name that only exists after code runs could not be in that
list. So the public export name stays in the output as a literal. estrogen hides
the binding the name points at; it does not hide the name. The product copy must
not claim otherwise.

The string form (`export { a as "proxyRouteCookie" }`) is still a literal. It
only lets a public name hold characters an identifier cannot, such as a dash. It
does not make the name any less visible.

## What the pass does, by export form

The pass runs `oxc_mangler` with `top_level` set to whether the file has module
syntax. It keeps function and class names through OXC's `keep_names` option,
then assigns fresh short names to eligible private callable declarations. All
renames use byte-range edits. It never reprints the AST.

| Source                                             | Output                             | How                                                                                                                                                                                                       |
| -------------------------------------------------- | ---------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `export { x }`                                     | `export { a as x }`                | oxc renames the binding `x` to `a`; the specifier's local span is rewritten to `a as x` so the public name `x` stays.                                                                                     |
| `export { x as y }`                                | `export { a as y }`                | oxc renames the local; only the `x` span moves, `as y` is untouched source.                                                                                                                               |
| `export { x as "y z" }`                            | `export { a as "y z" }`            | same as above; the string public name is untouched source.                                                                                                                                                |
| `export const x = v;`                              | `const a = v; export { a as x };`  | oxc keeps inline-declared exports, so the pass renames the binding to a fresh name itself, strips the `export ` keyword, and appends the re-export. Only when `v` is not a function or class (see below). |
| `export let x;`                                    | `let a; export { a as x };`        | live binding; `export { a as x }` tracks later assignments to `a`, same as the original.                                                                                                                  |
| `export function x() {}`                           | unchanged                          | a function's `.name` is observable; `keep_names` keeps it, so the pass leaves it.                                                                                                                         |
| `export class X {}`                                | unchanged                          | same reason as functions.                                                                                                                                                                                 |
| `export const x = () => 1;`                        | unchanged                          | the inferred `.name` of the arrow is observable, so the binding name is kept.                                                                                                                             |
| `export default <expr>`                            | binding in `<expr>` renamed if any | the public name is `default`, never the binding, so renaming a referenced local is safe. A named `export default function f` keeps `f` through `keep_names`.                                              |
| `export { x } from './m'`                          | unchanged                          | a re-export names `./m`'s export, not a local binding; oxc creates no symbol for it.                                                                                                                      |
| `export * from './m'`, `export * as ns from './m'` | unchanged                          | no local binding.                                                                                                                                                                                         |

### Inline-declared exports and the fresh name

`oxc_mangler` deliberately keeps a binding declared and exported in one statement
(`export const x = 1`): it collects those names into its exported set and never
renames them, so importers still resolve. To hide such a binding estrogen does
the rename itself, which needs a name that collides with nothing in the file.

After oxc has mangled the file, the allocator forbids every name that is:

- a current symbol name in the file (so it cannot clash with a binding oxc
  produced or kept),
- a free reference the file does not bind (so the new top-level binding cannot
  shadow a global the file uses),
- a reserved name the pass passed to oxc (the pool decoder, `String`, `exports`,
  `module`, `require`, `eval`, the import locals, and the single-character names
  already in use),
- a JavaScript reserved word.

It then takes the shortest identifier not forbidden. If no candidate is shorter
than the original export name, the pass leaves that export alone rather than make
the output longer. Only a single-declarator, single-identifier `const`/`let`/
`var` export whose value is not a function or class qualifies, and only when the
same binding is not also named by a separate `export { x }` specifier. Every
other inline export falls back to unchanged.

## Private function and class names

The pass can shorten a private function declaration when every reference is a
direct call. A private class declaration qualifies when every reference uses
`new`. Neither qualifies if its binding is reassigned or redeclared, if it is
exported, or if the file observes a callable name. Those name checks apply to the
whole file.

The name check counts a `.name` read, a `{ name }` destructure, a
`.constructor.name`, a string literal `"name"` (which can key a computed read or
a reflect call), and `new.target`. It does not count an object-literal key:
`{ name: value }` and the shorthand `{ name }` set a property, they do not read
any callable's `.name`. Bundled code uses those keys often, and an earlier
version counted them, so a single `{ name: ... }` kept the whole file from
renaming. Excluding the keys lets a chunk that only sets `name` properties still
shorten its private callables. A name reached through a dynamically assembled
property string may still be observable, so the pass does not promise to preserve
every form of reflection.

A private callable whose value is used directly (only called, or only
constructed) never escapes, so no other chunk can read its `.name`. A callable
passed, stored, or returned as a value does escape, so the pass keeps its name
rather than change a `.name` an importer might read. Shortening those escaping
names too would protect more of the file, at the cost of that cross-chunk
reflection; it is a deliberate choice left off by default.

The fresh-name allocator keeps the generated name away from existing bindings,
imports, unresolved globals, and the pool decoder. It also leaves named
expressions and inferred names unchanged. A script without module syntax may
rename private declarations only inside nested scopes when the script has an
explicit `"use strict"` directive. In a classic script, top-level callable names
remain unchanged.

Class names are observable through an instance's `constructor.name`, even when
the source does not read that property. Renaming an eligible class changes that
reported name if the instance leaves the file. Keep a class's name public, or
disable mangling for the chunk, when external callers depend on that reflection.
An exported function or class keeps its original `.name`.

The integration tests in `crates/estrogen-core/tests/mangle_node.rs` run private
callables, observable names, and public exports under Node and compare their
outputs with the source.

## The guard

Two things must hold for every file the pass touches: the output re-parses, and
no public export name is dropped. The second holds by construction, because the
only edits that touch an export specifier either add `as <public name>` or keep
the existing `as` clause, and the inline-export rewrite always appends
`export { <fresh> as <name> }`. The first is enforced where estrogen already
re-parses every rewritten chunk (`ensure_js_reparses`) and fails the file closed
if it does not parse. A file that would not re-parse is never written.
