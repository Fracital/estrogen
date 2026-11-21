//! End-to-end check that estrogen's VM bytecode runs correctly.
//!
//! This is the "verify by running, not by shape" proof for the VM pass. For a
//! corpus of supported sources, estrogen compiles each to a VM program in Rust,
//! then `node` runs the program through the vendored runtime and compares the
//! result to native `eval` of the same source. A mismatch fails the test. If
//! `node` is not on PATH the test prints a clear SKIP rather than passing as if
//! it had verified anything.

use std::io::Write;
use std::process::Command;

use estrogen_core::options::Options;
use estrogen_core::vm::{compile_module_json, runtime_iife, try_compile_chunk};

/// Whether `node` can be run. Prints a SKIP line and returns false when not.
fn node_available(test: &str) -> bool {
    match Command::new("node").arg("--version").output() {
        Ok(out) if out.status.success() => true,
        _ => {
            eprintln!("SKIP: node not found on PATH, cannot verify {test} by running");
            false
        }
    }
}

/// Run `code` as an ES module with node and return (success, stdout, stderr).
fn run_node(code: &str, tag: &str) -> (bool, String, String) {
    let mut path = std::env::temp_dir();
    path.push(format!("estrogen-vm-node-{}-{tag}.mjs", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).expect("create temp");
        f.write_all(code.as_bytes()).expect("write temp");
    }
    let out = Command::new("node").arg(&path).output().expect("run node");
    std::fs::remove_file(&path).ok();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The supported corpus: each source is a module whose last top-level expression
/// is the value to compare. These exercise literals, locals (slot load/init),
/// globals, member access, calls and method calls, `new`, unary and binary
/// operators, logical and conditional operators, arrays, objects, sequences, and
/// no-substitution templates.
const EXPRESSIONS: &[&str] = &[
    "1 + 2 * 3",
    "(10 - 4) / 2",
    "2 ** 10",
    "'a' + 'b' + 'c'",
    "true && 'yes'",
    "false || 42",
    "null ?? 'fallback'",
    "1 < 2 ? 'lt' : 'ge'",
    "5 >= 6 ? 'a' : 'b'",
    "typeof someUndefinedGlobalXyz",
    "const x = 5; const y = x * x; y + 1",
    "let a = 3; a + 4",
    "Math.max(3, 7, 2)",
    "Math.max(1, 2) + Math.min(5, 1)",
    "[1, 2, 3].length",
    "({ a: 1, b: 2 }).b",
    "'abcdef'.slice(1, 3)",
    "(1, 2, 3)",
    "`hello world`",
    "`left${1}right`",
    "`x${true}y${null}z`",
    "const n = 4; `value=${n + 1}`",
    "const o = {}; o[Symbol.toPrimitive] = (hint) => hint; `x${o}z`",
    "`${Symbol('s')}`",
    "-5 + +'3'",
    "~0",
    "!0",
    "5 % 3",
    "'x' in { x: 1 }",
    "[] instanceof Array",
    "new Array(1, 2, 3).length",
    "String(42)",
    "const n = 2; [n, n * n, n ** n]",
    "[1, ...[2, 3], 4]",
    "[...new Set([2, 3]), 4]",
    "const a = [, 3, ,]; [a.length, 0 in a, 1 in a, 2 in a]",
    "const a = [,,]; [a.length, 0 in a, 1 in a]",
    "[...[, 3, ,]]",
    "JSON.stringify([...new Set(['a', 'b'])])",
    "JSON.stringify({ a: 1, ...{ b: 2 }, a: 3 })",
    "JSON.stringify({ ...null, ...undefined, ...'ab' })",
    "Math.max(1, ...[3, 9, 2], 5)",
    "Math.min(...new Set([9, 4, 7]))",
    "const o = { start: 4, add: function (x, y) { return this.start + x + y; } }; o.add(...[2, 3])",
    "const o = { start: 4, add: function (x, y) { return this.start + x + y; } }; const key = 'add'; o[key](...[2, 3])",
    "new Map(...[[['a', 5], ['b', 8]]]).get('b')",
    "const f = (x, y) => x * y; f(3, ...[7])",
    "const f = () => 7; ['prototype' in f, typeof f.prototype]",
    "const f = () => 7; let name = ''; try { new f(); } catch (e) { name = e.name; } name",
    "Math.max(...42)",
    "const o = null; o?.x",
    "const o = { a: { b: 7 } }; o?.a?.b",
    "const o = {}; o?.a?.b",
    "const o = { a: null }; o?.a.b",
    "const o = { a: null }; o.a?.b.c",
    "const o = null; let calls = 0; const value = o?.[calls++]; [value, calls]",
    "const fn = null; let calls = 0; const value = fn?.(calls++); [value, calls]",
    "const o = { value: 10, add: function (x) { return this.value + x; } }; o?.add(3)",
    "const o = { value: 10, add: function (x) { return this.value + x; } }; o.add?.(3)",
    "const o = { value: 10, add: function (x) { return this.value + x; } }; o?.['add']?.(4)",
    "const o = { f: null }; o.f?.(1)",
    "const o = { f: function (x) { return { y: x }; } }; o?.f(7)?.y",
    "const o = null; let args = 0; const result = o?.f(args++); [result, args]",
    "const o = null; o?.f(...[7, 8])",
    "const o = { value: 7, get: function () { return this.value; } }; (o.get)()",
    "const __proto__ = 7; ({ __proto__ }).__proto__",
    "({ __proto__: { marker: 7 }, value: 2 }).marker",
    "Object.getPrototypeOf({ ...JSON.parse('{\"__proto__\":7}') }) === Object.prototype",
    "const key = 'name'; const value = 13; ({ [key]: value }).name",
    "const key = Symbol('secret'); const value = { [key]: 23 }; value[key]",
    "const value = { [3]: 'three', [5n]: 'five' }; [value[3], value[5]]",
    "const base = { marker: 5 }; const key = '__proto__'; const value = { [key]: base }; [Object.getPrototypeOf(value) === Object.prototype, value[key].marker]",
    "let steps = []; const key = () => {steps.push(1); return 'a'}; const value = () => {steps.push(2); return 9}; ({ [key()]: value() }).a; steps",
    "Number('0x10') | 1",
    "'ABC'.toLowerCase() + '-' + 'z'.toUpperCase()",
    "/a/.test('cat')",
    "const r = /a/gi; [r.source, r.flags, r.test('A')]",
    "const f = () => /a/g; [f() !== f(), f().lastIndex]",
    "const f = () => /a/g; const a = f(), b = f(); a.test('a'); [a.lastIndex, b.lastIndex]",
    "const r = /\\u{1F600}/u; r.test('😀')",
];

/// Functions, closures, assignment, and update. Each module ends in the value to
/// compare.
const FUNCTIONS: &[&str] = &[
    "function f(){if(false){var x=9;} return x;} f()",
    "function f(x){var x; return x;} f(13)",
    "function f(){var x=5; var x; return x;} f()",
    "function f(){var x=1; {var x=7;} return x;} f()",
    "function f(){ L: { var x = 5; break L; } return x; } f()",
    "function f(){ outer: for (var i = 0; i < 3; i++) { var last = i; } return [i, last]; } f()",
    "function f(){for(var i=0;i<3;i++){} return i;} f()",
    "function f(){const funcs=[]; for(var i=0;i<3;i++){funcs.push(()=>i);} return funcs.map(g=>g());} f()",
    "function f(){var x=1; function update(){x=x+2;} update(); return x;} f()",
    "function f(){switch(0){case 1:var x=2;}return x;} f()",
    "function f(){return x; var x=9;} f()",
    "function f(){function x(){return 9;} var x; return x();} f()",
    "function f(){return typeof Array; var Array;} f()",
    // recursion
    "function fact(n) { if (n <= 1) return 1; return n * fact(n - 1); } fact(5)",
    // mutual-ish nesting and lexical capture of an outer local
    "function outer() { const a = 2; function inner() { return a * 3; } return inner(); } outer()",
    // closure counter via assignment
    "function make() { let c = 0; return function () { c = c + 1; return c; }; } const f = make(); f(); f(); f()",
    // closure counter via prefix update, two independent closures stay separate
    "function make() { let c = 0; return function () { return ++c; }; } const a = make(); const b = make(); a(); a(); b()",
    // arrow expression body capturing an outer const
    "const base = 10; const add = (x) => x + base; add(5)",
    // arrow block body with a local
    "const f = (a, b) => { const s = a + b; return s * 2; }; f(3, 4)",
    // higher-order: map with an arrow
    "[1, 2, 3].map((x) => x * 2)",
    // higher-order: reduce with an arrow capturing nothing
    "[1, 2, 3, 4].reduce((acc, x) => acc + x, 0)",
    // method call: `this` is the receiver
    "const obj = { v: 7, get: function () { return this.v; } }; obj.get()",
    // default parameters are not emulated, so a plain-param function is used
    "function pow(b, e) { let r = 1; for (let i = 0; i < e; i++) { r = r * b; } return r; } pow(2, 8)",
    // postfix update returns the old value
    "let i = 5; const old = i++; [old, i]",
];

/// Non-derived class definitions run through BUILD_CLASS. The same source is
/// evaluated natively and by the VM, including descriptors and error types.
const CLASSES: &[&str] = &[
    "class Box { constructor(x) { this.x = x; } get() { return this.x; } } new Box(7).get()",
    "class Counter { constructor(x) { this.x = x; } add(n) { return this.x + n; } static make(n) { return new Counter(n); } } [Counter.make(4).add(3), new Counter(4) instanceof Counter]",
    "class Empty {} const x = new Empty(); [x instanceof Empty, Empty.name, Empty.length, typeof Empty]",
    "class C { x = 3; y; constructor() { this.seen = [this.x, 'y' in this]; this.x = 7; } } const c = new C(); [c.x, c.y, c.seen]",
    "class C { x = 1; static x = 2; get() { return this.x; } static get() { return this.x; } } [new C().get(), C.get()]",
    "class C { static a = 7; static b = this.a + 2; static get() { return this.b; } } [C.get(), C.a]",
    "class C { static me = C; static fn = () => this; } [C.me === C, C.fn() === C]",
    "class C { constructor(x, y) { this.sum = x + y; } } [C.length, Object.getOwnPropertyDescriptor(C, 'prototype').writable, new C(4, 5).sum]",
    "class C { method(a, b) { return a + b; } static staticMethod(n) { return n * 2; } } const d = Object.getOwnPropertyDescriptor(C.prototype, 'method'); const s = Object.getOwnPropertyDescriptor(C, 'staticMethod'); [d.enumerable, d.writable, d.configurable, d.value.length, d.value.name, s.enumerable, s.writable, s.configurable, s.value.name, C.staticMethod(4)]",
    "class C { x = 5; } const d = Object.getOwnPropertyDescriptor(new C(), 'x'); [d.value, d.writable, d.enumerable, d.configurable]",
    "class C { m() { return this; } } const m = (new C()).m; m() === undefined",
    "class C { m() {} } [typeof C.prototype.m.prototype, Object.prototype.hasOwnProperty.call(C.prototype.m, 'prototype')]",
    "class C { m() {} } new (new C().m)()",
    "class C {} C()",
    "class C { constructor() { return { other: 1 }; } x = 5; } const c = new C(); [c.other, c.x]",
    "class C { constructor() { return 17; } } new C() instanceof C",
    "class C { constructor() { this.seen = this.x; } x = 9; } new C().seen",
    "class C { static name = 'renamed'; static value; } [C.name, C.value, Object.getOwnPropertyDescriptor(C, 'name').enumerable]",
    "const Named = class { x = 2; }; [Named.name, new Named().x]",
    "const Outer = class Inner { static self = Inner; m() { return Inner.name; } }; [Outer.name, Outer.self === Outer, new Outer().m(), typeof Inner]",
    "let read; { class Scoped { x = 3; m() { return Scoped.name; } } read = () => new Scoped().m(); } read()",
    "class C { x = () => this; } const c = new C(); c.x() === c",
    "class C { x = () => this; } const c = new C(); [c.x.name, typeof c.x.prototype, c.x() === c]",
    "class C { static x = () => this; } [C.x.name, C.x() === C]",
    "class C { f = function () { return this; }; } const c = new C(); const f = c.f; [f.name, f() === undefined, c.f() === c]",
    "class C { nested = class {}; static StaticNested = class {}; } const c = new C(); [c.nested.name, C.StaticNested.name]",
    "class C { f = () => 7; } new (new C().f)()",
    "class C { nested() { return function () { return this; }; } } const f = new C().nested(); f() === undefined",
    "class C { static method() { return this; } } const fn = C.method; [C.method() === C, fn() === undefined]",
    "class C { static x = 3; } const d = Object.getOwnPropertyDescriptor(C, 'x'); [d.value, d.writable, d.enumerable, d.configurable]",
    "class C {} const p = Object.getOwnPropertyDescriptor(C, 'prototype'); const n = Object.getOwnPropertyDescriptor(C, 'name'); const l = Object.getOwnPropertyDescriptor(C, 'length'); const ctor = Object.getOwnPropertyDescriptor(C.prototype, 'constructor'); [p.writable, p.enumerable, p.configurable, n.value, n.writable, n.enumerable, n.configurable, l.value, l.writable, l.enumerable, l.configurable, ctor.value === C, ctor.writable, ctor.enumerable, ctor.configurable]",
    "function f() { var Local = class { x = 2; }; return [Local.name, new Local().x]; } f()",
    "const Out = class Internal { static ref = Internal; }; [Out.name, Out.ref === Out]",
    "typeof C; class C {}",
    "class C { static x = MissingName; } C.x",
    // extends: super() sets up this, inherited method dispatch, instanceof
    "class A { constructor(x) { this.x = x; } getX() { return this.x; } } class B extends A { constructor(x, y) { super(x); this.y = y; } sum() { return this.getX() + this.y; } } const b = new B(3, 4); [b.x, b.y, b.sum(), b.getX(), b instanceof A, b instanceof B]",
    // super.method() resolves on the parent prototype
    "class A { greet() { return 'A'; } } class B extends A { greet() { return 'B' + super.greet(); } } new B().greet()",
    // super.prop read as a value, then called with this
    "class A { m() { return this.n; } } class B extends A { constructor() { super(); this.n = 9; } test() { const f = super.m; return f.call(this); } } new B().test()",
    // the derived prototype chain and non-writable prototype descriptor
    "class A {} class B extends A {} const p = Object.getOwnPropertyDescriptor(B, 'prototype'); [p.writable, p.enumerable, p.configurable, Object.getPrototypeOf(B.prototype) === A.prototype, Object.getPrototypeOf(B) === A]",
    // an implicit constructor forwards arguments to super
    "class A { constructor(x) { this.x = x; } } class B extends A {} const b = new B(7); [b.constructor.name, b.x, b instanceof A]",
    // new.target is the constructed class, in a plain function it is set only on new
    "class A { constructor() { this.t = new.target.name; } } class B extends A {} [new A().t, new B().t]",
    "function F() { this.isNew = !!new.target; } [new F().isNew, (function g() { return !!new.target; })()]",
    // static field initialization order across a derived class
    "let log = []; class A { static a = log.push('a'); } class B extends A { static b = log.push('b'); } B; log",
    // using this before super() in a derived constructor throws
    "class A {} class B extends A { constructor() { return this; } } let e; try { new B(); } catch (err) { e = err && err.constructor && err.constructor.name; } e",
    // extends null: the prototype has no parent
    "class A extends null { constructor() { return Object.create(new.target.prototype); } } [Object.getPrototypeOf(A.prototype), A.prototype instanceof Object]",
    // a derived class instance field initializes after super() (reads a super-set prop)
    "class A { constructor() { this.x = 10; } } class B extends A { y = this.x + 5; } const b = new B(); [b.x, b.y]",
    // derived field with an explicit constructor, readable in the body after super()
    "class A { constructor(n) { this.n = n; } } class B extends A { doubled = this.n * 2; constructor(n) { super(n); this.extra = this.doubled + 1; } } const b = new B(5); [b.n, b.doubled, b.extra]",
    // field initializes after super() runs but before the rest of the body
    "let log = []; class A { constructor() { log.push('super'); } } class B extends A { f = (log.push('field'), 1); constructor() { super(); log.push('body'); } } new B(); log.join(',')",
    // derived field with an implicit constructor
    "class A { constructor(a) { this.a = a; } } class B extends A { b = 7; } const o = new B(3); [o.a, o.b]",
    // a static field on a derived class
    "class A {} class B extends A { static s = 99; } B.s",
    // an instance getter and setter sharing a key
    "class C { constructor() { this._v = 0; } get x() { return this._v; } set x(v) { this._v = v * 2; } } const c = new C(); c.x = 5; c.x",
    // a getter returns a computed value and its descriptor is an accessor
    "class C { get x() { return 42; } } const d = Object.getOwnPropertyDescriptor(C.prototype, 'x'); [new C().x, typeof d.get, d.set, d.enumerable, d.configurable]",
    // a static getter on the class itself
    "class C { static get n() { return 7; } } [C.n, Object.getOwnPropertyDescriptor(C, 'n').enumerable]",
    // a getter inherited through extends
    "class A { get kind() { return 'a'; } } class B extends A {} new B().kind",
    // a setter with no getter leaves the getter undefined
    "class C { set x(v) { this.stored = v; } } const c = new C(); c.x = 9; [c.stored, Object.getOwnPropertyDescriptor(C.prototype, 'x').get]",
    // a static block runs at definition with the class as this
    "class C { static { this.init = 'done'; } } C.init",
    // a static block reads an earlier static field
    "class C { static x = 1; static { this.y = this.x + 1; } } [C.x, C.y]",
    // static block and static fields run in source order
    "const log = []; class C { static a = (log.push('a'), 1); static { log.push('block'); } static b = (log.push('b'), 2); } [log.join(','), C.a, C.b]",
    // a static block has its own scope for locals
    "class C { static { let n = 5; this.v = n * 2; } } C.v",
    // a static block in a derived class sees super through the class
    "class A { static who() { return 'A'; } } class B extends A { static { this.tag = super.who(); } } B.tag",
    // a private field reads and writes through the class brand
    "class C { #x = 5; get() { return this.#x; } set(v) { this.#x = v; } } const c = new C(); c.set(c.get() + 10); c.get()",
    // a private field is not an own string property
    "class C { #x = 1; } const c = new C(); [Object.keys(c).length, JSON.stringify(c)]",
    // reading a private member of a wrong-brand object throws a TypeError
    "class C { #x = 1; static read(o) { return o.#x; } } let e = ''; try { C.read({}); } catch (err) { e = err.constructor.name; } e",
    // `#x in obj` is a brand check
    "class C { #x = 1; static has(o) { return #x in o; } } [C.has(new C()), C.has({})]",
    // a private field holding a function keeps `this` when called
    "class C { #f = function () { return this.#v; }; #v = 9; run() { return this.#f(); } } new C().run()",
    // a derived class declares its own private field
    "class A { a() { return 1; } } class B extends A { #x = 7; get() { return this.#x + this.a(); } } new B().get()",
    // a private field initializer reads an earlier private field
    "class C { #a = 2; #b = this.#a * 3; get() { return this.#b; } } new C().get()",
    // a private field in a nested closure inside a method
    "class C { #x = 4; make() { return () => this.#x * 2; } } new C().make()()",
];

/// Control flow: if/else, while, do/while, C-style for, break/continue, switch,
/// and nested loops (jump backpatching). Each module ends in the value.
const CONTROL: &[&str] = &[
    "let sum = 0; for (const value of [1, 2, 3]) { sum += value; } sum",
    "let sum = 0; for (let value of new Set([2, 3, 4])) { sum += value; } sum",
    "let result = []; for (let item of ['a', 'b']) { result.push(() => item); } result.map((f) => f())",
    "let result = []; const obj = { a: 1, b: 2 }; for (const key in obj) { result.push(key); } result",
    "let result = []; const proto = { inherited: 4 }; const obj = Object.create(proto); obj.own = 2; for (const key in obj) { result.push(key); } result",
    "let result = []; for (const key in null) { result.push(key); } result",
    "function sum(){ let result=0; for (var value of [2,3,4]) { result+=value; } return [result, value]; } sum()",
    "let events = []; const iterable = { [Symbol.iterator]: function() { let i=0; return {next: function() {return i<3 ? {value: i++, done:false} : {done:true}}, return: function(){events.push('close');return {done:true}} } } }; let sum=0; for (const x of iterable) {sum+=x;} [sum,events]",
    "let events = []; const iterable = { [Symbol.iterator]: function() { let i=0; return {next: function() {return i<3 ? {value: i++, done:false} : {done:true}}, return: function(){events.push('close');return {done:true}} } } }; try { for (const x of iterable) { if (x===1) throw 17; } } catch(e) {} events",
    "let result = 0; for (const x of [1, 2, 3, 4]) { if (x === 3) break; result += x; } result",
    "let result = 0; for (const x of [1, 2, 3, 4]) { if (x % 2 === 0) continue; result += x; } result",
    "let events = []; const iterable = { [Symbol.iterator]: function() { let i=0; return {next: function() {return i<3 ? {value: i++, done:false} : {done:true}}, return: function(){events.push('close');return {done:true}} } } }; for (const x of iterable) { if (x===1) break; } events",
    "let events = []; const iterable = { [Symbol.iterator]: function() { let i=0; return {next: function() {return i<3 ? {value: i++, done:false} : {done:true}}, return: function(){events.push('close');return {done:true}} } } }; for (const x of iterable) { if (x===1) continue; } events",
    "let events = []; const iterable = { [Symbol.iterator]: function() { return {next: function(){return {value:11,done:false}},return:function(){events.push('close');return {done:true}}} } }; function first(){for(const x of iterable){return x;}} [first(),events]",
    "let events = []; const iterable = { [Symbol.iterator]: function() { return {next: function(){return {value:11,done:false}},return:function(){events.push('close');return {done:true}}} } }; function first(){for(const x of iterable){return;}} [first(),events]",
    "function f(){let sum=0; for (let key in {a:1,b:2,c:3}) { if (key === 'c') break; sum++; } return sum;} f()",
    "function f(){let result=''; for (const key in {a:1,b:2,c:3}) { if (key === 'b') continue; result += key; } return result;} f()",
    "let result = ''; try { for (const x of null) {} } catch(e) { result = e.name; } result",
    "function f() { const x = [1]; let result = ''; try { for (const x of x) {} } catch(e) { result = e.name; } return result; } f()",
    "let result = ''; try { throw 'x'; } catch (e) { result = e + 'y'; } result",
    "let result = 3; try { result += 1; } catch (e) { result = 12; } result",
    "let result = 3; try { throw 7; } catch { result = 12; } result",
    "let result = 0; try { { let a = 4; throw a; } } catch (e) { result = e; } result",
    "let result = ''; try { try { throw 'inner'; } catch (e) { throw e + 'outer'; } } catch (e) { result = e; } result",
    "let result = 0; try { const o = null; o.method(); } catch (e) { result = e.constructor.name; } result",
    "function f(){ try { return 17; } catch(e) { return 0; } } f()",
    "let e = 3; let result = 0; try { throw 9; } catch(e) { result = e; } [e, result]",
    "function f(){try{ throw 23 }catch(e){var z=e;}return z;} f()",
    // if / else
    "let r; if (3 > 2) { r = 'a'; } else { r = 'b'; } r",
    "let r; if (3 < 2) { r = 'a'; } else { r = 'b'; } r",
    // while
    "let i = 0; let s = 0; while (i < 5) { s = s + i; i = i + 1; } s",
    // do / while runs the body once even when the test is false
    "let i = 10; let s = 0; do { s = s + 1; i++; } while (i < 3); s",
    // C-style for summing an array
    "const arr = [1, 2, 3, 4]; let sum = 0; for (let i = 0; i < arr.length; i++) { sum += arr[i]; } sum",
    // break
    "let s = 0; for (let i = 0; i < 10; i++) { if (i === 5) break; s += i; } s",
    // continue
    "let s = 0; for (let i = 0; i < 6; i++) { if (i % 2 === 0) continue; s += i; } s",
    // switch with break
    "let x = 2; let r; switch (x) { case 1: r = 'a'; break; case 2: r = 'b'; break; default: r = 'c'; } r",
    // switch default
    "let x = 9; let r; switch (x) { case 1: r = 'a'; break; default: r = 'd'; } r",
    // switch fall-through
    "let x = 1; let r = ''; switch (x) { case 1: r += '1'; case 2: r += '2'; break; default: r += 'd'; } r",
    // nested loops (backpatching): sum of i*j
    "let t = 0; for (let i = 0; i < 3; i++) { for (let j = 0; j < 3; j++) { t += i * j; } } t",
    // nested loops with inner break and continue
    "let t = 0; for (let i = 0; i < 3; i++) { for (let j = 0; j < 4; j++) { if (j === i) continue; if (j > 2) break; t += 1; } } t",
    // nested loops with a block-scoped local in the outer body and a continue
    "let out = 0; for (let i = 0; i < 3; i++) { let b = i * 10; for (let j = 0; j < 2; j++) { if (j === 1) continue; out += b + j; } } out",
    // while with break and continue
    "let i = 0; let s = 0; while (true) { i++; if (i > 10) break; if (i % 2 === 0) continue; s += i; } s",
    // labeled break out of the outer loop from an inner loop
    "let t = 0; outer: for (let i = 0; i < 4; i++) { for (let j = 0; j < 4; j++) { if (i * j >= 4) break outer; t += 1; } } t",
    // labeled continue to the outer loop from an inner loop
    "let t = 0; outer: for (let i = 0; i < 3; i++) { for (let j = 0; j < 3; j++) { if (j === 1) continue outer; t += i * 10 + j; } } t",
    // labeled break out of a plain block
    "let r = 'x'; block: { r = 'a'; if (r === 'a') break block; r = 'b'; } r",
    // labeled break with a block-scoped local in the loop body (scope unwinding)
    "let out = 0; L: for (let i = 0; i < 4; i++) { let base = i * 100; for (let j = 0; j < 4; j++) { let k = base + j; if (k > 201) break L; out += k; } } out",
    // labeled while with labeled continue
    "let i = 0; let s = 0; loop: while (i < 10) { i++; if (i % 3 === 0) continue loop; s += i; } s",
    // labeled do-while with labeled break from an inner loop
    "let n = 0; d: do { for (let j = 0; j < 5; j++) { n++; if (n === 3) break d; } n += 100; } while (n < 1000); n",
    // labeled switch: break label exits the switch
    "let x = 2; let r = ''; sw: switch (x) { case 2: r += 'a'; if (r === 'a') break sw; r += 'b'; default: r += 'c'; } r",
    // labeled block wrapping a loop, break label leaves the whole block
    "let acc = 0; outer: { for (let i = 0; i < 5; i++) { acc += i; if (i === 2) break outer; } acc += 1000; } acc",
    // labeled break crossing a single for-of iterator (outer while loop), so
    // the inner iterator is closed as the break leaves it
    "let t = 0; let i = 1; L: while (i < 5) { for (const y of [10, 20, 30]) { if (i * y >= 60) break L; t += i + y; } i++; } t",
    // labeled continue to an outer for-of from an inner plain loop
    "let s = 0; L: for (const x of [1, 2, 3]) { for (let j = 0; j < 3; j++) { if (j === x) continue L; s += x * 10 + j; } } s",
    // finally runs after the try block completes normally, then control falls
    // through to the code after it
    "let o = []; function f() { try { o.push('t'); } finally { o.push('f'); } o.push('c'); return o; } f()",
    // finally runs on the way out of a return; the returned value is computed
    // before the finally
    "let x = 0; function f() { try { return 1; } finally { x = 9; } } const r = f(); [r, x]",
    // a return in the finally overrides a return in the try
    "function f() { try { return 1; } finally { return 2; } } f()",
    // a return in the finally overrides a throw in the try, swallowing it
    "function f() { try { throw new Error('x'); } finally { return 9; } } f()",
    // try, catch, and finally all run in order when the body throws
    "let o = []; function f() { try { o.push('t'); throw 1; } catch (e) { o.push('c'); } finally { o.push('f'); } return o; } f()",
    // a finally with no catch lets the throw propagate to an outer catch, after
    // the finally runs
    "let x = 0; function f() { try { try { throw 5; } finally { x = 7; } } catch (e) { return [e, x]; } } f()",
    // nested try/finally run inner to outer
    "let o = []; function f() { try { try { o.push(1); } finally { o.push(2); } } finally { o.push(3); } return o; } f()",
    // the return value is read before the finally mutates shared state
    "let o = []; function f() { try { o.push('t'); return o.length; } finally { o.push('x'); } } const r = f(); [r, o.length]",
    // finally over a for-of body (iterator closes normally, finally still runs)
    "let o = []; function f() { for (const n of [1, 2]) { try { o.push(n); } finally { o.push(-n); } } return o; } f()",
    // a for-let whose body contains a for-of no longer over-declines
    "let total = 0; for (let i = 0; i < 3; i++) { for (const x of [10, 20]) total += i * x; } total",
    // a for-let whose body contains a try no longer over-declines
    "let s = 0; for (let i = 0; i < 4; i++) { try { if (i === 2) throw i; s += i; } catch (e) { s += 100; } } s",
    // a for-let whose body contains a labeled statement no longer over-declines
    "let s = 0; for (let i = 0; i < 3; i++) { L: { if (i === 1) break L; s += i; } } s",
    // a let/const declared directly in a switch case, shared across the case block
    "function kind(n) { switch (n) { case 0: let a = 'zero'; return a; case 1: const b = 'one'; return b; default: return 'other'; } } [kind(0), kind(1), kind(2)]",
    // the discriminant reads the outer binding, not the case-scoped shadow
    "function f() { let x = 5; switch (x) { case 5: let x = 99; return x; default: return -1; } } f()",
    // a case-scoped let is in TDZ for a case that does not declare it
    "function f(n) { switch (n) { case 1: return 'a'; case 2: let v = 7; return v; } } [f(1), f(2)]",
    // a break that targets a loop nested inside the try does not escape it, so
    // the try compiles even with a finally present
    "let o = []; function f() { try { for (let i = 0; i < 5; i++) { if (i === 2) break; o.push(i); } } finally { o.push('f'); } return o; } f()",
    // a continue to an inner loop inside a try/finally also stays inside
    "let s = 0; function f() { try { for (let i = 0; i < 4; i++) { if (i % 2 === 0) continue; s += i; } } finally { s += 100; } return s; } f()",
    // a switch break inside a try block stays inside the try
    "function f() { let r = ''; try { switch (2) { case 2: r += 'a'; break; default: r += 'b'; } } finally { r += 'z'; } return r; } f()",
    // an inner loop with break inside a plain try/catch
    "function f() { let t = 0; try { for (const x of [1, 2, 3]) { if (x === 3) break; t += x; } } catch (e) { t = -1; } return t; } f()",
    // a break that escapes a try/finally runs the finally then exits the loop
    "let o = []; for (let i = 0; i < 5; i++) { try { if (i === 2) break; o.push(i); } finally { o.push(-1); } } o",
    // a continue that crosses a finally runs it then continues
    "let o = []; for (let i = 0; i < 3; i++) { try { if (i === 1) continue; o.push(i); } finally { o.push(10 + i); } } o",
    // a return that crosses a finally runs it, inside a loop
    "function f() { let o = []; for (let i = 0; i < 3; i++) { try { if (i === 1) return o; } finally { o.push(i); } } return o; } f()",
    // a finally that returns overrides a pending break
    "function f() { for (;;) { try { break; } finally { return 'from-finally'; } } return 'after'; } f()",
    // a break crossing two nested finallys runs them inner-to-outer
    "let o = []; for (let i = 0; i < 2; i++) { try { try { if (i === 0) break; o.push('b' + i); } finally { o.push('f1-' + i); } } finally { o.push('f2-' + i); } } o",
    // a break that escapes a try/catch (no finally) pops the catch handler
    "let o = []; for (let i = 0; i < 4; i++) { try { if (i === 2) break; o.push(i); } catch (e) {} } o",
    // a break escaping a try/finally out of a while loop
    "let i = 0; let s = 0; while (i < 5) { try { i++; if (i === 3) break; s += i; } finally { s += 100; } } s",
    // a continue escaping a nested block scope and a finally keeps the binding
    "let o = []; for (let i = 0; i < 3; i++) { try { let k = i * 2; if (i === 1) continue; o.push(k); } finally { o.push(-i); } } o",
];

/// Expression-surface mutation: compound, logical, and update assignment to a
/// member target, `delete`, and optional calls. Cases stay on configurable
/// properties and number/string operands, where strict-module and sloppy-eval
/// semantics agree, since the native oracle here is indirect `eval` (sloppy).
const MUTATION: &[&str] = &[
    // compound assignment to a static member
    "const o = { n: 1 }; o.n += 5; o.n",
    "const o = { n: 10 }; o.n -= 3; o.n *= 2; o.n",
    "const o = { s: 'a' }; o.s += 'b'; o.s",
    "const o = { x: 6 }; o.x %= 4; o.x",
    "const o = { x: 3 }; o.x **= 2; o.x",
    "const o = { f: 12 }; o.f &= 10; o.f |= 1; o.f ^= 3; o.f",
    "const o = { f: 1 }; o.f <<= 4; o.f >>= 1; o.f",
    // compound assignment to a computed member, key evaluated once
    "const a = [1, 2, 3]; a[1] *= 10; a[1]",
    "let calls = 0; const a = [10, 20]; function idx() { calls++; return 1; } a[idx()] += 5; [a[1], calls]",
    // logical assignment to a member, only assigns when the guard fails
    "const o = { a: 0, b: 2 }; o.a ||= 5; o.b ||= 9; [o.a, o.b]",
    "const o = { a: 1, b: 0 }; o.a &&= 7; o.b &&= 9; [o.a, o.b]",
    "const o = { a: null, b: 3 }; o.a ??= 4; o.b ??= 9; [o.a, o.b]",
    "let n = 0; const a = [0]; function i() { n++; return 0; } a[i()] ||= 8; [a[0], n]",
    // update on a member: prefix yields new, postfix yields old
    "const o = { n: 5 }; const r = o.n++; [r, o.n]",
    "const o = { n: 5 }; const r = ++o.n; [r, o.n]",
    "const o = { n: 5 }; o.n--; o.n",
    "const a = [1, 2, 3]; const r = a[1]++; [r, a[1]]",
    "const a = [1, 2, 3]; const r = --a[0]; [r, a[0]]",
    "let c = 0; const a = [10]; function i() { c++; return 0; } a[i()]++; [a[0], c]",
    "const o = { n: '5' }; o.n++; o.n",
    // delete a configurable member (static and computed)
    "const o = { a: 1, b: 2 }; delete o.a; [o.a, o.b, 'a' in o]",
    "const o = { x: 5 }; delete o.x",
    "const o = {}; delete o.missing",
    "const o = { a: 1 }; const k = 'a'; delete o[k]; 'a' in o",
    "const a = [1, 2, 3]; delete a[1]; [a.length, 1 in a]",
    // optional call: callee short-circuits on nullish, receiver preserved
    "const f = () => 42; f?.()",
    "let g; g?.()",
    "const o = { v: 9, m: function () { return this.v; } }; o.m?.()",
    "const o = { v: 3 }; o.missing?.()",
    "const a = { b: { c: () => 'deep' } }; a.b?.c?.()",
];

/// The wider function surface: default and rest parameters, the arguments
/// object, and named function expressions. Each case's last expression is the
/// value compared against native eval.
const FUNCTION_SURFACE: &[&str] = &[
    // default parameters, including one that reads an earlier parameter
    "function f(a, b = 10) { return a + b; } [f(1), f(1, 2), f(1, undefined)]",
    "const g = (x, y = x * 2) => x + y; [g(3), g(3, 1)]",
    "function f(a, b = a + 1) { return [a, b]; } [f(5), f(5, 0)]",
    // native length stops at the first default or rest
    "function f(a, b = 1, c) { return f.length; } f()",
    // rest parameters bind a real Array
    "function sum(...xs) { let t = 0; for (const x of xs) t += x; return t; } [sum(), sum(1, 2, 3)]",
    "function h(a, ...rest) { return [a, rest.length, Array.isArray(rest)]; } [h(1), h(1, 2, 3)]",
    "const r = (...x) => x.length; [r(), r(1, 2), r.length]",
    // the arguments object: length, indexing, iteration, spread, and that it is
    // a genuine arguments object rather than an Array
    "function f() { return arguments.length; } [f(), f(1, 2, 3)]",
    "function f() { let t = 0; for (let i = 0; i < arguments.length; i++) t += arguments[i]; return t; } f(1, 2, 3, 4)",
    "function f() { return [Array.isArray(arguments), Object.prototype.toString.call(arguments)]; } f(1)",
    "function f() { return [...arguments]; } f(1, 2, 3)",
    // an arrow shares the enclosing function's arguments through the scope chain
    "function f() { const g = () => arguments[0] + arguments.length; return g(); } f(40, 2)",
    // named function expressions: the name is bound to the function in its body
    "const fact = function self(n) { return n <= 1 ? 1 : n * self(n - 1); }; fact(5)",
    "const f = function g() { return typeof g; }; f()",
    // the inner name does not leak to the outer scope
    "const f = function g() { return 1; }; typeof g",
    // the function expression's own name wins over the inferred binding name
    "const f = function named() { return named.name; }; f()",
];

/// Destructuring bindings and assignment targets. Each case's last expression
/// is the value the VM and native eval compare on. These exercise object and
/// array patterns, defaults, renaming, computed keys, object and array rest,
/// holes, nesting, member targets, and the iterator protocol for arrays.
const DESTRUCTURING: &[&str] = &[
    "const { a, b } = { a: 1, b: 2 }; [a, b]",
    "const { a: x, b: y = 9 } = { a: 1 }; [x, y]",
    "const { a = 5, b = 6 } = { a: undefined, b: null }; [a, b]",
    "const k = 'z'; const { [k]: v } = { z: 42 }; v",
    "const { a, ...rest } = { a: 1, b: 2, c: 3 }; [a, rest.b, rest.c]",
    "const k = 'a'; const { [k]: first, ...rest } = { a: 1, b: 2, c: 3 }; [first, rest.b, rest.c, Object.keys(rest).length]",
    "const [a, b, c] = [1, 2, 3]; [a, b, c]",
    "const [a, , c] = [1, 2, 3]; [a, c]",
    "const [a = 7, b = 8] = [1]; [a, b]",
    "const [a, ...rest] = [1, 2, 3, 4]; [a, rest.length, rest[0], rest[2]]",
    "const [a, [b, c]] = [1, [2, 3]]; [a, b, c]",
    "const { a: { b } } = { a: { b: 10 } }; b",
    "const [{ a }, { b }] = [{ a: 1 }, { b: 2 }]; [a, b]",
    "const [a, b] = new Set([10, 20, 30]); [a, b]",
    "const [a, b] = 'hi'; [a, b]",
    "let a, b; ({ a, b } = { a: 1, b: 2 }); [a, b]",
    "let a, b; [a, b] = [3, 4]; [a, b]",
    "const o = {}; [o.x, o.y] = [5, 6]; [o.x, o.y]",
    "let a, rest; ({ a, ...rest } = { a: 1, b: 2 }); [a, rest.b]",
    "let x = 1, y = 2; [x, y] = [y, x]; [x, y]",
    "let a = 0; [a = 9] = []; a",
    "const var_a = (function () { var [p, q] = [1, 2]; return p + q; })(); var_a",
    // destructured function parameters
    "function f({ a, b }) { return a + b; } f({ a: 1, b: 2 })",
    "function f({ a, b = 10 }) { return a + b; } [f({ a: 1 }), f({ a: 1, b: 2 })]",
    "function f([x, y]) { return x - y; } f([5, 2])",
    "function f([x, ...rest]) { return [x, rest.length, rest[1]]; } f([1, 2, 3])",
    "const g = ({ a }, [b]) => a + b; g({ a: 1 }, [2])",
    "function f({ a: { b } }) { return b; } f({ a: { b: 9 } })",
    "function f({ a } = {}) { return a === undefined; } [f(), f({ a: 7 })]",
    "function f(...[a, b]) { return a + b; } f(1, 2)",
    // a destructured param without a default still counts toward length
    "function f({ a }, b) { return f.length; } f()",
    "function f({ a } = {}, b) { return f.length; } f()",
    // destructuring in for-of and for-in heads
    "let s = 0; for (const { a } of [{ a: 1 }, { a: 2 }]) s += a; s",
    "let s = 0; for (const [x, y] of [[1, 2], [3, 4]]) s += x * y; s",
    "const r = []; for (const { a, b = 0 } of [{ a: 1 }, { a: 2, b: 5 }]) r.push(a + b); r",
    "const out = []; for (const [i, v] of [[0, 'a'], [1, 'b']]) out.push(i + v); out",
    // a fresh binding per iteration, captured by a closure
    "const fns = []; for (const { a } of [{ a: 1 }, { a: 2 }]) fns.push(() => a); [fns[0](), fns[1]()]",
    // destructured var head leaks the binding to the function scope
    "const run = () => { let out = []; for (var { a } of [{ a: 1 }, { a: 2 }]) out.push(a); return [out, typeof a]; }; run()",
];

/// Generator functions: the runtime drives the frame to each `yield`. Each
/// case's last expression is the compared value. These cover spread, manual
/// `next`, values sent back into `yield`, a `return` inside the body, closure
/// capture across yields, iteration with `for`/`of`, and early `return` through
/// a `for`/`of` break.
const GENERATORS: &[&str] = &[
    "function* g() { yield 1; yield 2; yield 3; } [...g()]",
    "function* g() { yield 1; yield 2; } const it = g(); [it.next().value, it.next().value, it.next().value, it.next().done]",
    "function* g() { const a = yield 1; const b = yield a + 1; return a + b; } const it = g(); [it.next().value, it.next(10).value, it.next(20).value, it.next().done]",
    "function* range(n) { for (let i = 0; i < n; i++) yield i; } [...range(4)]",
    "function* g() { yield 1; return 99; yield 2; } [...g()]",
    "function* counter(start) { let c = start; while (true) { yield c++; if (c > start + 2) return; } } [...counter(5)]",
    "function* g() { yield 1; yield 2; yield 3; } let s = 0; for (const x of g()) s += x; s",
    "function* g() { yield 1; yield 2; yield 3; } const r = []; for (const x of g()) { r.push(x); if (x === 2) break; } r",
    "const make = function* named(n) { yield n; yield n + 1; }; [...make(7)]",
    "function* g() { yield; yield 5; } const it = g(); [it.next().value, it.next().value]",
    // yield* delegation: over an array, a string, and another generator
    "function* g() { yield* [10, 20, 30]; } [...g()]",
    "function* g() { yield 0; yield* 'ab'; yield 1; } [...g()]",
    "function* inner() { yield 1; yield 2; } function* outer() { yield 0; yield* inner(); yield 3; } [...outer()]",
    // yield* result is the delegate's return value
    "function* inner() { yield 1; return 99; } function* outer() { const r = yield* inner(); yield r; } [...outer()]",
    // nested yield* chains
    "function* a() { yield 1; } function* b() { yield* a(); yield 2; } function* c() { yield* b(); yield 3; } [...c()]",
    // a value sent through yield* reaches the delegate's yield
    "function* inner() { const x = yield 1; return x * 10; } function* outer() { const r = yield* inner(); yield r; } const it = outer(); [it.next().value, it.next(5).value]",
];

/// Tagged templates, object method shorthand, accessors, and spread in `new`.
/// Each case's last expression is compared against native eval.
const TEMPLATES_OBJECTS: &[&str] = &[
    // tagged template: cooked strings and substitutions in order
    "function tag(s, ...v) { return s.join('|') + '#' + v.join(','); } tag`a${1}b${2}c`",
    // the raw strings keep their backslashes
    "function tag(s) { return s.raw[0]; } tag`a\\nb`",
    // one frozen strings object per call site: the same site twice is the same object
    "function tag(s) { return s; } function f() { return tag`y`; } f() === f()",
    // an invalid escape leaves cooked undefined while raw keeps the text
    "function tag(s) { return [s[0], s.raw[0]]; } tag`\\u`",
    // a member tag keeps its receiver as this
    "const o = { p: 'P', tag(s, v) { return this.p + s[0] + v; } }; o.tag`a${9}`",
    // the built-in String.raw over a tagged template
    "String.raw`a${1}\\n${2}b`",
    // object method shorthand, including this and a computed key
    "const o = { greet() { return 'hi'; } }; o.greet()",
    "const o = { x: 5, read() { return this.x; } }; o.read()",
    "const k = 'm'; const o = { [k]() { return 42; } }; o.m()",
    // getters and setters, including a get/set pair that composes
    "const o = { get x() { return 7; } }; o.x",
    "let captured; const o = { set x(v) { captured = v; } }; o.x = 9; captured",
    "let store = 0; const o = { get x() { return store; }, set x(v) { store = v * 2; } }; o.x = 5; o.x",
    "const o = { get x() { return 1; } }; Object.keys(o)",
    // spread arguments in new
    "function F(a, b) { this.sum = a + b; } const args = [3, 4]; new F(...args).sum",
];

#[test]
fn vm_bytecode_matches_native_eval() {
    if !node_available("vm_bytecode_matches_native_eval") {
        return;
    }
    // Build one node script that runs every case through the vendored runtime and
    // compares to native eval, so the whole corpus is one node invocation.
    let mut cases = String::from("const CASES = [\n");
    for src in EXPRESSIONS
        .iter()
        .chain(FUNCTIONS)
        .chain(CONTROL)
        .chain(CLASSES)
        .chain(MUTATION)
        .chain(FUNCTION_SURFACE)
        .chain(DESTRUCTURING)
        .chain(GENERATORS)
        .chain(TEMPLATES_OBJECTS)
    {
        let json = compile_module_json(src, "vm-node-seed")
            .unwrap_or_else(|| panic!("supported source did not compile: {src}"));
        let src_lit = serde_json::to_string(src).unwrap();
        // The program JSON is itself a valid JS object literal.
        cases.push_str(&format!("  {{ src: {src_lit}, program: {json} }},\n"));
    }
    cases.push_str("];\n");

    let script = format!(
        "const __mk = {runtime};\n\
         {cases}\n\
         const fails = [];\n\
         for (const c of CASES) {{\n\
         \tlet vm, nat;\n\
         \ttry {{ vm = __mk(c.program)(); }} catch (e) {{ vm = {{ __err: e && e.constructor && e.constructor.name }}; }}\n\
         \ttry {{ nat = (0, eval)(c.src); }} catch (e) {{ nat = {{ __err: e && e.constructor && e.constructor.name }}; }}\n\
         \tconst a = JSON.stringify(vm), b = JSON.stringify(nat);\n\
         \tif (!(Object.is(vm, nat) || a === b)) fails.push(c.src + '  vm=' + a + '  native=' + b);\n\
         }}\n\
         if (fails.length) {{ console.error(fails.join('\\n')); process.exit(1); }}\n\
         console.log('OK ' + CASES.length);\n",
        runtime = runtime_iife(),
    );

    let (ok, stdout, stderr) = run_node(&script, "corpus");
    assert!(ok, "vm corpus mismatch or node error:\n{stderr}\n{stdout}");
    assert!(
        stdout.starts_with("OK "),
        "unexpected node output: {stdout}"
    );
}

/// Async functions and arrows compile to a frame the runtime drives with its
/// async promise loop. Each case's last top-level expression is a promise; the
/// test awaits both the VM result and native `eval` of the same source and
/// compares them, so await ordering, resolution, and rejection routed through a
/// catch are all checked against the engine.
const ASYNC_CASES: &[&str] = &[
    "const f = async (x) => x + 1; f(41)",
    "async function f() { const a = await Promise.resolve(2); return a * 21; } f()",
    "async function chain() { return (await Promise.resolve(1)) + (await Promise.resolve(41)); } chain()",
    "const g = async function () { let s = 0; for (const n of [1, 2, 3, 4]) s += await Promise.resolve(n); return s; }; g()",
    "const e = async () => { try { await Promise.reject(new Error('x')); return 'no'; } catch (err) { return 'caught ' + err.message; } }; e()",
    "const p = (async () => 7)(); p",
    "async function outer() { async function inner(n) { return await Promise.resolve(n * 2); } return (await inner(3)) + (await inner(4)); } outer()",
    // async generators: a body that both awaits and yields, consumed by manual
    // next() (a consumer `for await` would itself decline, so drive it by hand)
    "async function* g() { yield 1; yield await Promise.resolve(2); yield 3; } const it = g(); (async () => { const r = []; r.push((await it.next()).value); r.push((await it.next()).value); r.push((await it.next()).value); const d = await it.next(); r.push(d.value === undefined); r.push(d.done); return r; })()",
    // a value sent into yield, and the return value as the final done result
    "async function* g() { const a = yield 1; const b = yield a + 1; return a + b; } const it = g(); (async () => { const r1 = await it.next(); const r2 = await it.next(10); const r3 = await it.next(20); return [r1.value, r2.value, r3.value, r3.done]; })()",
    // await inside a loop in the generator body, driven to completion
    "async function* range(n) { let i = 0; while (i < n) { yield await Promise.resolve(i * 10); i++; } } const it = range(3); (async () => { const out = []; let x; while (!(x = await it.next()).done) out.push(x.value); return out; })()",
    // a rejection awaited in the body propagates out of next()
    "async function* g() { yield await Promise.reject(new Error('boom')); } const it = g(); (async () => { try { await it.next(); return 'no'; } catch (e) { return 'caught ' + e.message; } })()",
];

#[test]
fn vm_async_matches_native_eval() {
    if !node_available("vm_async_matches_native_eval") {
        return;
    }
    let mut cases = String::from("const CASES = [\n");
    for src in ASYNC_CASES {
        let json = compile_module_json(src, "vm-async-seed")
            .unwrap_or_else(|| panic!("async source did not compile: {src}"));
        let src_lit = serde_json::to_string(src).unwrap();
        cases.push_str(&format!("  {{ src: {src_lit}, program: {json} }},\n"));
    }
    cases.push_str("];\n");

    // Top-level await compiles the module main itself to an async frame; native
    // eval cannot parse a bare top-level await, so this one is checked against a
    // known value rather than against eval of the same text.
    let top_level = compile_module_json("await Promise.resolve(40 + 2)", "vm-async-seed")
        .expect("top-level await should compile");

    let script = format!(
        "const __mk = {runtime};\n\
         {cases}\n\
         const TOP = {top_level};\n\
         (async () => {{\n\
         \tconst fails = [];\n\
         \tfor (const c of CASES) {{\n\
         \t\tlet vm, nat;\n\
         \t\ttry {{ vm = await __mk(c.program)(); }} catch (e) {{ vm = {{ __err: e && e.constructor && e.constructor.name }}; }}\n\
         \t\ttry {{ nat = await (0, eval)(c.src); }} catch (e) {{ nat = {{ __err: e && e.constructor && e.constructor.name }}; }}\n\
         \t\tconst a = JSON.stringify(vm), b = JSON.stringify(nat);\n\
         \t\tif (!(Object.is(vm, nat) || a === b)) fails.push(c.src + '  vm=' + a + '  native=' + b);\n\
         \t}}\n\
         \tconst top = await __mk(TOP)();\n\
         \tif (top !== 42) fails.push('top-level await  vm=' + JSON.stringify(top) + '  expected=42');\n\
         \tif (fails.length) {{ console.error(fails.join('\\n')); process.exit(1); }}\n\
         \tconsole.log('OK ' + (CASES.length + 1));\n\
         }})();\n",
        runtime = runtime_iife(),
    );

    let (ok, stdout, stderr) = run_node(&script, "async-corpus");
    assert!(
        ok,
        "vm async corpus mismatch or node error:\n{stderr}\n{stdout}"
    );
    assert!(
        stdout.starts_with("OK "),
        "unexpected node output: {stdout}"
    );
}

/// Constructs outside the subset must decline (return `None`) so the obfuscator
/// falls back to the literal-pool and name passes instead of miscompiling.
#[test]
fn unsupported_constructs_decline() {
    let unsupported = [
        "var z = 1; z",
        "import x from './y.js'; x",
        "class C { [Math.random()]() {} }",
        "class C { [Math.random()] = 1; }",
        "class C { #m() { return 1; } }",
        "class C { static #s = 1; }",
        "class C { async method() {} }",
        "class C { *method() {} }",
        "let C; C = class {}; C.name",
        "const object = { C: class {} }; object.C.name",
        // a generator (sync or async) declines when its own frame would hold a
        // handler across a yield (try, for-of, for-in)
        "function* g() { try { yield 1; } finally {} }",
        "function* g() { for (const x of [1, 2]) yield x; }",
        "async function* g() { try { yield 1; } finally {} }",
        // yield* in an async generator and a consumer for-await still decline
        "async function* g() { yield* [1, 2]; }",
        "async function f() { for await (const x of []) {} }",
        "const o = { value: 7, get: function () { return this.value; } }; (o?.get)()",
        "function f() { const x = 7; return eval('x'); } f()",
        "function f(){for(const x of [1,2]){for(const y of [3,4]){return x+y;}}} f()",
        // a break or continue inside a finally that escapes it would abandon the
        // pending completion record, which is not ported, so it declines.
        "for (;;) { try { x(); } finally { continue; } }",
        // a labeled jump across a try is not routed through the finally chain.
        "outer: for (;;) { try { break outer; } finally {} }",
        // an escaping jump out of a for-of across a try would skip the iterator
        // close, so it declines.
        "for (const x of [1, 2]) { try { if (x === 1) break; } finally {} }",
        "try { throw { x: 1 }; } catch ({ x }) {}",
        // a for-let binding captured by a closure must decline, because estrogen
        // uses one binding where native makes one per iteration.
        "const fns = []; for (let i = 0; i < 3; i++) { fns.push(() => i); } fns[0]()",
        // the closure check still finds a capture reached through an inner for-of
        "const fns = []; for (let i = 0; i < 2; i++) { for (const x of [0]) fns.push(() => i); } fns[0]()",
        // a function declaration in a switch case has Annex B hoisting the VM does
        // not model, so it declines even though lexical case bindings compile
        "switch (1) { case 1: function g() { return 1; } g(); }",
        // a labeled break that would close more than one for-of iterator at once
        // declines, matching the conservative multi-iterator unwinding stance.
        "L: for (const x of [1, 2]) { for (const y of [3, 4]) { break L; } }",
    ];
    for src in unsupported {
        assert!(
            compile_module_json(src, "seed").is_none(),
            "expected the VM to decline (fall back) for: {src}"
        );
    }
}

#[test]
fn class_chunks_require_known_module_scope() {
    let opts = Options {
        seed: Some(b"vm-class-module".to_vec()),
        ..Options::default()
    };
    // A plain .js file can be a classic script. Its root class binding must
    // remain available to later script tags, which the VM cannot model yet.
    for source in ["class Plain {}", "const Plain = class {};"] {
        assert!(
            try_compile_chunk(source, &opts)
                .expect("class fallback")
                .is_none(),
            "source kind is unknown: {source}"
        );
    }
    if !node_available("class_chunks_require_known_module_scope") {
        return;
    }
    // A real import proves ESM scope. The original module and its VM version
    // must agree on class behavior and must not expose a global binding.
    let cases = [
        "import 'data:text/javascript,'; class C { value = 3; constructor(n) { this.value = this.value + n; } get() { return this.value; } static make(n) { return new C(n); } } globalThis.__result = [C.make(4).get(), Object.hasOwn(globalThis, 'C')];",
        "import 'data:text/javascript,'; class C { static self = C; static x = 7; } globalThis.__result = [C.self === C, C.x, C.name];",
    ];
    for (index, source) in cases.iter().enumerate() {
        let output = try_compile_chunk(source, &opts)
            .expect("class VM compile")
            .unwrap_or_else(|| panic!("VM declined known ESM class: {source}"));
        assert!(output.contains("JSON.parse("), "not bytecode: {source}");
        let native = format!("{source}\nconsole.log(JSON.stringify(globalThis.__result));");
        let vm = format!("{output}\nconsole.log(JSON.stringify(globalThis.__result));");
        let (ok_original, native_stdout, native_stderr) =
            run_node(&native, &format!("class-native-{index}"));
        assert!(ok_original, "native class failed: {native_stderr}");
        let (ok_vm, vm_stdout, vm_stderr) = run_node(&vm, &format!("class-vm-{index}"));
        assert!(ok_vm, "VM class failed: {vm_stderr}");
        assert_eq!(
            vm_stdout, native_stdout,
            "class module behavior changed: {source}"
        );
    }
}

/// import.meta, a local `export const`, and dynamic `import()` run through the
/// native module wrapper. Each case is compiled to a VM chunk, then both the
/// original source and the transformed chunk run as ES modules in node and their
/// observation is compared, so the wrapper plumbs the module context correctly.
#[test]
fn vm_module_context_matches_native() {
    if !node_available("vm_module_context_matches_native") {
        return;
    }
    let cases = [
        // import.meta: a real object, stable identity, with a url string. The url
        // itself differs per file, so compare its type, not its value.
        (
            "const m = import.meta; globalThis.__r = [typeof m, m === import.meta, typeof m.url];",
            "globalThis.__r",
        ),
        // local export const: the value is readable both as the VM result and
        // through the re-exported native binding.
        (
            "export const a = 6, b = a * 7; globalThis.__r = [a, b];",
            "[globalThis.__r, a, b]",
        ),
        // dynamic import with top-level await: the wrapper awaits the async main.
        (
            "globalThis.__r = (await import('data:text/javascript,export default 42')).default + 1;",
            "globalThis.__r",
        ),
    ];
    for (i, (source, observe)) in cases.iter().enumerate() {
        let output = try_compile_chunk(
            source,
            &Options {
                seed: Some(b"vm-module-context".to_vec()),
                ..Options::default()
            },
        )
        .expect("VM module compilation failed")
        .unwrap_or_else(|| panic!("VM declined module context: {source}"));
        assert!(output.contains("JSON.parse("), "not VM bytecode: {output}");
        let original = format!("{source}\nconsole.log(JSON.stringify({observe}));");
        let transformed = format!("{output}\nconsole.log(JSON.stringify({observe}));");
        let (ok_native, stdout_native, stderr_native) =
            run_node(&original, &format!("module-native-{i}"));
        assert!(ok_native, "native source failed: {stderr_native}");
        let (ok_vm, stdout_vm, stderr_vm) = run_node(&transformed, &format!("module-vm-{i}"));
        assert!(ok_vm, "VM module failed: {stderr_vm}\n{output}");
        assert_eq!(stdout_vm, stdout_native, "module context changed: {source}");
    }
}

#[test]
fn static_imports_use_native_linking_and_vm_bytecode() {
    if !node_available("static_imports_use_native_linking_and_vm_bytecode") {
        return;
    }
    let examples = [
        "import { value as imported, bump } from 'data:text/javascript,export let value=3;export function bump(){value+=1}'; bump(); globalThis.__result = imported",
        "import answer from 'data:text/javascript,export default 11'; globalThis.__result = answer * 2",
        "import * as ns from 'data:text/javascript,export const value=13'; globalThis.__result = ns.value",
        "import 'data:text/javascript,globalThis.__side=7'; globalThis.__result = globalThis.__side",
        "import { value as imported } from 'data:text/javascript,export const value=17'; globalThis.counter = imported; counter = 23; globalThis.__result = [counter, globalThis.counter]",
        "import { value } from 'data:text/javascript,export const value=7'; var local = value; if (false) { var missing = 9; } globalThis.__result = [local, missing, Object.hasOwn(globalThis, 'local')]",
        "import { value } from 'data:text/javascript,export const value=7'; for (var i = 0; i < value; i++) {} globalThis.__result = [i, Object.hasOwn(globalThis, 'i')]",
        "import 'data:text/javascript,Object.defineProperty(globalThis,\"testValue\",{configurable:true,get:function(){return this===globalThis?11:22},set:function(v){globalThis.testSetter=this===globalThis?v:v+10}})'; globalThis.__result = testValue; testValue = 13; globalThis.__result = [globalThis.__result, globalThis.testSetter]",
    ];
    for (i, source) in examples.iter().enumerate() {
        let output = try_compile_chunk(
            source,
            &Options {
                seed: Some(b"vm-native-imports".to_vec()),
                ..Options::default()
            },
        )
        .expect("VM import compilation failed")
        .unwrap_or_else(|| panic!("VM declined native imports: {source}"));
        assert!(output.contains("JSON.parse("), "not VM bytecode: {output}");
        let original = format!("{source};\nconsole.log(globalThis.__result);");
        let transformed = format!("{output}\nconsole.log(globalThis.__result);");
        let (ok_native, stdout_native, stderr_native) =
            run_node(&original, &format!("native-import-{i}"));
        assert!(ok_native, "native source failed: {stderr_native}");
        let (ok_vm, stdout_vm, stderr_vm) = run_node(&transformed, &format!("vm-import-{i}"));
        assert!(ok_vm, "VM import failed: {stderr_vm}");
        assert_eq!(stdout_vm, stdout_native, "import binding changed: {source}");
    }
}

#[test]
fn imported_names_used_by_the_vm_runtime_decline_safely() {
    for name in ["undefined", "Infinity", "NaN", "BigInt", "RegExp", "Proxy"] {
        let source = format!(
            "import {{ value as {name} }} from 'data:text/javascript,export const value=9'; globalThis.__result = {name}"
        );
        assert!(
            try_compile_chunk(&source, &Options::default())
                .expect("compiler failed")
                .is_none(),
            "runtime builtin shadowing must take fallback: {source}"
        );
    }
}

#[test]
fn native_reexports_keep_their_public_names_in_vm_chunks() {
    if !node_available("native_reexports_keep_their_public_names_in_vm_chunks") {
        return;
    }
    let examples = [
        "export { value as publicValue } from 'data:text/javascript,export const value=13'; globalThis.__result = 5",
        "export * from 'data:text/javascript,export const value=17'; globalThis.__result = 6",
        "export * as publicSpace from 'data:text/javascript,export const value=19'; globalThis.__result = 7",
    ];
    for (i, source) in examples.iter().enumerate() {
        let output = try_compile_chunk(
            source,
            &Options {
                seed: Some(b"vm-native-reexports".to_vec()),
                ..Options::default()
            },
        )
        .expect("VM compilation failed")
        .unwrap_or_else(|| panic!("VM declined native reexport: {source}"));
        assert!(output.contains("JSON.parse("), "not VM bytecode: {output}");
        let mut results = Vec::new();
        for (name, candidate) in [("native", *source), ("vm", output.as_str())] {
            let script = format!(
                "const js = {};\nconst ns = await import('data:text/javascript,' + encodeURIComponent(js));\nconsole.log(JSON.stringify([Object.keys(ns), globalThis.__result, ns.publicValue ?? ns.value ?? ns.publicSpace?.value]));",
                serde_json::to_string(candidate).unwrap()
            );
            let (ok, stdout, stderr) = run_node(&script, &format!("{name}-reexport-{i}"));
            assert!(ok, "{name} reexport failed: {stderr}");
            results.push(stdout);
        }
        assert_eq!(results[0], results[1], "reexport changed: {source}");
    }
}

/// The runtime embedded in a VM chunk must ship compact and comment-free, so the
/// output does not read like plain annotated source.
#[test]
fn embedded_runtime_has_no_comments() {
    let runtime = runtime_iife();
    assert!(
        !runtime.contains("//"),
        "embedded runtime has a line comment"
    );
    assert!(
        !runtime.contains("/*"),
        "embedded runtime has a block comment"
    );
    assert!(
        !runtime.contains("*/"),
        "embedded runtime has a block comment"
    );
    assert!(
        !runtime.contains('\n'),
        "embedded runtime still carries source newlines"
    );
    // A string literal in the runtime survives minification intact (quote style
    // is codegen's choice, so match the inner text only).
    assert!(
        runtime.contains("Cannot access a lexical binding before initialization"),
        "a runtime string literal was lost in minification"
    );
    // The mangled, minified runtime is one compact line: no run of two or more
    // spaces and no newline survive.
    assert!(
        !runtime.contains("  "),
        "the minified runtime still has runs of whitespace"
    );
    // It is a valid expression that node can evaluate: it is the factory, and
    // calling it with a program returns the entry function the chunk runs.
    if node_available("embedded_runtime_has_no_comments") {
        let script = format!(
            "const mk = {runtime};\n\
             if (typeof mk !== 'function') {{ console.error('no factory'); process.exit(1); }}\n\
             console.log('OK');\n",
            runtime = runtime,
        );
        let (ok, stdout, stderr) = run_node(&script, "minify");
        assert!(ok, "minified runtime failed to load:\n{stderr}");
        assert!(stdout.starts_with("OK"), "unexpected output: {stdout}");
    }
}

/// A VM chunk whose compiled function carries a canary string and a
/// canary-named local must leak neither: the string lives in the encrypted pool
/// and the name compiles to a slot index.
#[test]
fn vm_function_chunk_leaks_no_plaintext() {
    let src = "function reveal() { const secretCanaryLocal = 'topSecretCanaryValue'; \
               return secretCanaryLocal; } reveal();\n";
    let opts = Options {
        seed: Some(b"leak-seed".to_vec()),
        ..Options::default()
    };
    let out = try_compile_chunk(src, &opts)
        .expect("compile should not error")
        .expect("a function chunk should VM-compile");
    assert!(out.contains("JSON.parse("), "expected a VM chunk:\n{out}");
    assert!(
        !out.contains("topSecretCanaryValue"),
        "the string literal leaked in the clear"
    );
    assert!(
        !out.contains("secretCanaryLocal"),
        "the local binding name leaked in the clear"
    );
    assert!(
        !out.contains("reveal"),
        "the function name leaked in the clear"
    );
}

/// A chunk that never observes a callable name carries no function or class
/// name in its program, so decrypting the program still reveals no original
/// identifier. A chunk that reads `.name`, `.constructor`, a `"name"` string, or
/// `new.target` keeps the names so reflection stays correct.
#[test]
fn vm_drops_callable_names_only_when_unobserved() {
    // Unobserved: no `.name`, `"name"`, `.constructor`, or `new.target`. The
    // program carries no `"n"` key and none of the original identifiers.
    let dropped = [
        ("function alpha(x) { return x + 1; } function beta(y) { return alpha(y) * 2; } beta(20)",
         &["alpha", "beta"][..]),
        ("const sum = (a, b) => a + b; const twice = (n) => sum(n, n); twice(21)",
         &["sum", "twice"][..]),
        ("class Point { constructor(x) { this.x = x; } read() { return this.x; } } new Point(42).read()",
         &["Point"][..]),
    ];
    for (src, idents) in dropped {
        let json = compile_module_json(src, "name-drop-seed")
            .unwrap_or_else(|| panic!("supported source did not VM-compile: {src}"));
        assert!(
            !json.contains("\"n\":"),
            "a function name survived though nothing observes it: {src}\n{json}"
        );
        for ident in idents {
            assert!(
                !json.contains(ident),
                "the identifier {ident} leaked into the program: {src}\n{json}"
            );
        }
    }

    // Observed: each reads a callable name, so the name stays in the program and
    // the identifier is present.
    let kept = [
        ("function alpha(x) { return x; } alpha.name", "alpha"),
        ("function beta() {} [beta.name, beta()]", "beta"),
        (
            "function gamma() { return gamma['name']; } gamma()",
            "gamma",
        ),
        (
            "class Widget { who() { return new.target; } } new Widget().who()",
            "Widget",
        ),
        ("class Gadget {} Gadget.name", "Gadget"),
    ];
    for (src, name) in kept {
        let json = compile_module_json(src, "name-keep-seed")
            .unwrap_or_else(|| panic!("supported source did not VM-compile: {src}"));
        assert!(
            json.contains(name),
            "the name {name} was dropped though the chunk observes a callable name: {src}\n{json}"
        );
    }

    // Node parity: dropping names changes nothing the dropped chunks compute, so
    // each still matches native eval of the same source.
    if !node_available("vm_drops_callable_names_only_when_unobserved") {
        return;
    }
    let mut cases = String::from("const CASES = [\n");
    for (src, _) in dropped {
        let json = compile_module_json(src, "name-drop-seed").unwrap();
        let src_lit = serde_json::to_string(src).unwrap();
        cases.push_str(&format!("  {{ src: {src_lit}, program: {json} }},\n"));
    }
    cases.push_str("];\n");
    let script = format!(
        "const __mk = {runtime};\n\
         {cases}\n\
         const fails = [];\n\
         for (const c of CASES) {{\n\
         \tlet vm, nat;\n\
         \ttry {{ vm = __mk(c.program)(); }} catch (e) {{ vm = {{ __err: e && e.constructor && e.constructor.name }}; }}\n\
         \ttry {{ nat = (0, eval)(c.src); }} catch (e) {{ nat = {{ __err: e && e.constructor && e.constructor.name }}; }}\n\
         \tconst a = JSON.stringify(vm), b = JSON.stringify(nat);\n\
         \tif (!(Object.is(vm, nat) || a === b)) fails.push(c.src + '  vm=' + a + '  native=' + b);\n\
         }}\n\
         if (fails.length) {{ console.error(fails.join('\\n')); process.exit(1); }}\n\
         console.log('OK ' + CASES.length);\n",
        runtime = runtime_iife(),
    );
    let (ok, stdout, stderr) = run_node(&script, "name-drop");
    assert!(ok, "name-drop parity failed:\n{stderr}\n{stdout}");
    assert!(
        stdout.starts_with("OK "),
        "unexpected node output: {stdout}"
    );
}

/// The shipped VM runtime leaks no opcode name and no readable runtime
/// identifier, and its body is one minified line. This is the obfuscation
/// guarantee for the runtime itself, on top of the per-chunk canary scan.
#[test]
fn vm_chunk_runtime_is_mangled_and_names_free() {
    use estrogen_core::vm::opcodes::OP_NAMES;

    let opts = Options {
        seed: Some(b"leak-scan-seed".to_vec()),
        ..Options::default()
    };
    // A chunk that exercises many opcode groups (arithmetic, calls, control flow,
    // slots, arrays, closures), so its runtime carries their handlers.
    let src =
        "function pow(b, e) { let r = 1; for (let i = 0; i < e; i++) { r = r * b; } return r; } \
               const xs = [1, 2, 3].map((n) => n + 1); pow(2, xs.length);\n";
    let out = try_compile_chunk(src, &opts)
        .expect("compile should not error")
        .expect("a function chunk should VM-compile");
    assert!(out.contains("JSON.parse("), "expected a VM chunk:\n{out}");

    // The chunk is a single line by construction: no stray newlines.
    assert!(
        !out.contains('\n'),
        "the VM chunk should be one line:\n{out}"
    );
    // The opcode-name scan runs against the code with string literals removed.
    // The encrypted program ships as base64 inside string literals and a random
    // base64 blob can hold a short opcode substring by chance; the code outside
    // strings carries no base64, so an opcode name there is a real leak.
    let code = strip_string_literals(&out);
    for name in OP_NAMES {
        assert!(
            !code.contains(name),
            "opcode name {name} leaked into the shipped runtime code"
        );
    }

    // No readable runtime identifier or brand word survives. These are long or
    // carry punctuation, so scanning the whole chunk is safe from base64 chance.
    for ident in [
        "estrogen",
        "__estrogen_vm",
        "ARITY",
        "createRuntime",
        "createClosure",
        "decodeConst",
        "HANDLERS",
        "scopeAt",
        "runtime",
        ".run(",
    ] {
        assert!(
            !out.contains(ident),
            "runtime identifier {ident} leaked into the VM chunk"
        );
    }

    // Minified: no run of two or more spaces anywhere in the chunk.
    assert!(
        !out.contains("  "),
        "the shipped runtime still has runs of whitespace"
    );
}

/// Blank out the content of every string and template literal, so a scan of the
/// result sees only code. Used to keep the base64 pool from tripping the
/// opcode-name check. A backslash escapes the next character inside a string.
fn strip_string_literals(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == '\\' {
                    chars.next();
                } else if c == q {
                    quote = None;
                    out.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' || c == '`' {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
    }
    out
}

/// A chunk with an out-of-subset construct still produces a valid fallback chunk
/// through the obfuscator, and the VM declines it.
#[test]
fn unsupported_chunk_still_declines_at_the_api() {
    let opts = Options {
        seed: Some(b"fallback-seed".to_vec()),
        ..Options::default()
    };
    // A private method is still outside the subset, so the VM declines it (a
    // private field now compiles).
    let declined = try_compile_chunk("class C { #m() { return 1; } }\n", &opts)
        .expect("compile should not error");
    assert!(
        declined.is_none(),
        "an out-of-subset class chunk must decline to the fallback path"
    );
}

/// A chunk ships only the opcode handlers it uses. A tiny program references a
/// handful of opcodes, so its whole emitted chunk (the pruned runtime, the pool
/// decoder, and the program) is smaller than the full runtime alone, and it
/// still runs correctly: no handler the program dispatches was dropped.
#[test]
fn a_small_chunk_ships_a_pruned_runtime() {
    let opts = Options {
        seed: Some(b"prune".to_vec()),
        ..Options::default()
    };
    let chunk = try_compile_chunk("globalThis.__prune_r = 1 + 2 * 20;", &opts)
        .unwrap()
        .expect("the tiny chunk compiles to VM bytecode");
    assert!(chunk.contains("JSON.parse("), "not a VM chunk: {chunk}");
    let full = runtime_iife().len();
    assert!(
        chunk.len() < full,
        "the pruned chunk ({}) should be smaller than the full runtime ({full})",
        chunk.len(),
    );

    if !node_available("a_small_chunk_ships_a_pruned_runtime") {
        return;
    }
    let script = format!("{chunk}\nconsole.log(globalThis.__prune_r);");
    let (ok, stdout, stderr) = run_node(&script, "pruned-runtime");
    assert!(ok, "the pruned chunk failed in node:\n{stderr}");
    assert_eq!(stdout.trim(), "41", "pruned chunk ran wrong: {stdout}");
}
