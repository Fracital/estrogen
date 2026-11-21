// VM runtime execution core (maintainable source).
//
// This is the stack-bytecode interpreter estrogen's VM pass compiles to. It is a
// trimmed, dependency-free port of abulxt's shield runtime-template.js: the
// handler-table dispatch, the slot-based scope model, the closure model, and the
// opcode handler bodies are copied byte-for-byte for the opcodes estrogen's
// compiler emits, so a program this file runs has the exact bytecode and
// constant-pool shape the shield runtime executes. It runs the compiler's
// subset: expressions, let/const declarations, functions (declarations,
// expressions, arrows), closures with lexical capture, and control flow
// (if/while/do/for/switch with break/continue).
//
// This file is the maintainable source. The VM pass mangles every binding in it
// and prints it minified (OXC mangler plus oxc_codegen) before it ships inside a
// chunk, so the shipped runtime is one compact line of short names with no
// opcode names and nothing that spells the runtime's purpose. See vm/mod.rs.
//
// Dispatch is numeric. The on-disk program carries, per per-build opcode byte,
// the opcode's stable index (its position in opcodes.rs OP_NAMES) and its arity,
// in layout.ops. The runtime looks the handler up in HANDLERS by that stable
// index, so no opcode name appears here as a string or an identifier. The handler
// table below is written in OP_NAMES order through a running counter, so a
// handler lands at the index the compiler assigned the same opcode. See
// docs/SHIELD-VM.md.
//
// It raises the cost of reading a build. It is not secrecy: the pool key ships
// in the output next to the decoder, so anyone can run it back.

(function () {
	// A frame signal is { type, value }. The type is numeric so no readable
	// opcode-like word ships: a return leaves the frame with the returned value,
	// a halt returns the main completion value, and an await suspends the frame
	// so the async driver can resume it when the awaited value settles.
	const SIG_RETURN = 0;
	const SIG_HALT = 1;
	const SIG_AWAIT = 2;
	const SIG_YIELD = 3;
	const SIG_YIELD_STAR = 4;

	// A handler on frame.handlers is a catch or a finally. A finally runs on
	// every way its try completes; the pending completion rides frame.completions
	// as a numeric kind so no readable word ships. END_FINALLY replays it: a
	// normal completion falls through, a throw rethrows, a return re-returns.
	const HANDLER_CATCH = 0;
	const HANDLER_FINALLY = 1;
	const DONE_NORMAL = 0;
	const DONE_THROW = 1;
	const DONE_RETURN = 2;

	// Stable opcode indices, in the exact order of estrogen-core's opcodes.rs
	// OP_NAMES. The counter assigns each name its position, so a handler stored at
	// OP_<name> below sits at the same index the compiler's layout records for
	// that opcode. These bindings are mangled away in the shipped runtime; only
	// the numbers survive, and only inside the (encrypted) program's layout.ops.
	let _n = 0;
	// prettier-ignore
	const
		OP_HALT = _n++, OP_NOP = _n++, OP_POP = _n++, OP_DUP = _n++,
		OP_DUP2 = _n++, OP_SWAP = _n++, OP_ROTL3 = _n++, OP_PUSH_CONST = _n++,
		OP_PUSH_UNDEF = _n++, OP_PUSH_NULL = _n++, OP_PUSH_TRUE = _n++, OP_PUSH_FALSE = _n++,
		OP_PUSH_THIS = _n++, OP_LOAD_VAR = _n++, OP_STORE_VAR = _n++, OP_INIT_VAR = _n++,
		OP_DECLARE_VAR = _n++, OP_DECLARE_LET = _n++, OP_DECLARE_CONST = _n++, OP_DEF_FUNC = _n++,
		OP_TYPEOF_VAR = _n++, OP_LOAD_ARG = _n++, OP_LOAD_ARGS_FROM = _n++, OP_LOAD_SLOT = _n++,
		OP_STORE_SLOT = _n++, OP_INIT_SLOT = _n++, OP_TYPEOF_SLOT = _n++, OP_DECLARE_SLOT = _n++,
		OP_DEF_FUNC_SLOT = _n++, OP_LOAD_GLOBAL = _n++, OP_STORE_GLOBAL = _n++, OP_TYPEOF_GLOBAL = _n++,
		OP_IMPORT_SLOT = _n++, OP_BIND_PARAM = _n++, OP_LOAD_SLOT_PROP = _n++, OP_LOAD_GLOBAL_PROP = _n++,
		OP_LOAD_SLOT_DUP_PROP = _n++, OP_LOAD_GLOBAL_DUP_PROP = _n++, OP_OBJ_SET_CONST = _n++, OP_DECLARE_SLOTS = _n++,
		OP_ENTER_SCOPE = _n++, OP_EXIT_SCOPE = _n++, OP_ADD = _n++, OP_SUB = _n++,
		OP_MUL = _n++, OP_DIV = _n++, OP_MOD = _n++, OP_EXP = _n++,
		OP_EQ = _n++, OP_NEQ = _n++, OP_SEQ = _n++, OP_SNEQ = _n++,
		OP_LT = _n++, OP_LTE = _n++, OP_GT = _n++, OP_GTE = _n++,
		OP_BAND = _n++, OP_BOR = _n++, OP_BXOR = _n++, OP_SHL = _n++,
		OP_SHR = _n++, OP_USHR = _n++, OP_NOT = _n++, OP_NEG = _n++,
		OP_POS = _n++, OP_BNOT = _n++, OP_TYPEOF = _n++, OP_INSTANCEOF = _n++,
		OP_IN = _n++, OP_GET_PROP = _n++, OP_GET_PROP_DYN = _n++, OP_SET_PROP = _n++,
		OP_SET_PROP_DYN = _n++, OP_DELETE_PROP = _n++, OP_DELETE_PROP_DYN = _n++, OP_JMP = _n++,
		OP_JMP_IF_FALSE = _n++, OP_JMP_IF_TRUE = _n++, OP_JMP_IF_FALSY_KEEP = _n++, OP_JMP_IF_TRUTHY_KEEP = _n++,
		OP_JMP_IF_NOT_NULLISH_KEEP = _n++, OP_OPT_SHORT = _n++, OP_OPT_SHORT_CALL = _n++, OP_CALL = _n++,
		OP_CALL_METHOD = _n++, OP_NEW = _n++, OP_CALL_APPLY = _n++, OP_CALL_METHOD_APPLY = _n++,
		OP_NEW_APPLY = _n++, OP_MAKE_FUNC = _n++, OP_RETURN = _n++, OP_RETURN_UNDEF = _n++,
		OP_ARR_EMPTY = _n++, OP_ARR_APPEND = _n++, OP_ARR_SPREAD = _n++, OP_OBJ_EMPTY = _n++,
		OP_OBJ_SET = _n++, OP_OBJ_SET_DYN = _n++, OP_OBJ_SPREAD = _n++, OP_OBJ_DEFINE_GETTER = _n++,
		OP_OBJ_DEFINE_SETTER = _n++, OP_OBJ_REST = _n++, OP_OBJ_REST_DYN = _n++, OP_GET_ITERATOR = _n++,
		OP_FOR_ITER = _n++, OP_ITER_NEXT_PUSH = _n++, OP_ITER_REST_PUSH = _n++, OP_FOR_IN_KEYS = _n++,
		OP_THROW = _n++, OP_RETHROW = _n++, OP_PUSH_HANDLER = _n++, OP_PUSH_FINALLY = _n++,
		OP_POP_HANDLER = _n++, OP_PUSH_COMPLETION_NORMAL = _n++, OP_END_FINALLY = _n++, OP_ABRUPT_JUMP = _n++,
		OP_AWAIT = _n++, OP_BUILD_CLASS = _n++, OP_LOAD_NEW_TARGET = _n++, OP_SUPER_GET = _n++,
		OP_SUPER_GET_DYN = _n++, OP_SUPER_SET = _n++, OP_SUPER_SET_DYN = _n++, OP_SUPER_CALL = _n++,
		OP_SUPER_CALL_APPLY = _n++, OP_GET_PRIVATE = _n++, OP_SET_PRIVATE = _n++, OP_HAS_PRIVATE = _n++,
		OP_INIT_PRIVATE = _n++, OP_MODULE_INIT = _n++, OP_IMPORT_BINDING = _n++, OP_EXPORT_BINDING = _n++,
		OP_DYNAMIC_IMPORT = _n++, OP_IMPORT_META = _n++, OP_AWAIT_TOP = _n++, OP_SET_COMPLETION = _n++,
		OP_LOAD_ARGUMENTS = _n++, OP_YIELD = _n++, OP_YIELD_STAR = _n++;

	// A constant pool entry may be a tagged object for a value JSON cannot carry
	// directly. The compiler writes these; decodeConst reverses them.
	function decodeConst(entry) {
		if (entry && typeof entry === 'object') {
			if (entry.$ === 'n') {
				if (entry.v === 'NaN') return NaN;
				if (entry.v === 'Inf') return Infinity;
				if (entry.v === '-Inf') return -Infinity;
				if (entry.v === '-0') return -0;
			}
			if (entry.$ === 're') return new NativeRegExp(entry.s, entry.f);
			if (entry.$ === 'bi') return BigInt(entry.v);
			if (entry.$ === 'tpl') {
				// One frozen strings object per tagged-template site, built once
				// at load so every evaluation passes the same object. A null
				// cooked value is an invalid escape and reads as undefined.
				const strings = entry.c.map(function (v) {
					return v === null ? undefined : v;
				});
				strings.raw = Object.freeze(entry.r.slice());
				return Object.freeze(strings);
			}
		}
		return entry;
	}

	function fatal(message) {
		return new Error(message);
	}

	// Slot-based locals: every lexical binding resolves to a (depth, index) pair
	// at compile time. A scope is { parent, slots: [] }; a slot holds a cell
	// { v, kind, init }. kind: 0=var, 1=let, 2=const.
	function scopeAt(scope, depth) {
		while (depth > 0) {
			scope = scope.parent;
			depth--;
		}
		return scope;
	}
	const TDZ_READ = 'Cannot access a lexical binding before initialization';
	const NativeRegExp = /(?:)/.constructor;
	// Build a real arguments object from the call arguments. Applying a strict
	// helper with no parameters yields an unmapped arguments object, so the
	// result has the right length, indices, and iterator and is not an Array.
	function buildArguments(args) {
		'use strict';
		return function () {
			return arguments;
		}.apply(null, args);
	}

	// Opcode handlers, indexed by stable opcode index. Each receives
	// (frame, arg, consts, functions, globals, createClosure, callValue, doReturn)
	// and returns a frame signal to stop the frame, else nothing. Bodies are
	// copied from abulxt's shield runtime-template.js for the opcodes estrogen's
	// compiler emits; unlisted indices stay empty, and the runtime maps their
	// bytes to the trap.
	const HANDLERS = [];

	HANDLERS[OP_HALT] = function (frame) {
		return { type: SIG_HALT, value: frame.completion };
	};
	HANDLERS[OP_NOP] = function () {};
	HANDLERS[OP_POP] = function (frame) {
		frame.stack.pop();
	};
	HANDLERS[OP_DUP] = function (frame) {
		const s = frame.stack;
		s.push(s[s.length - 1]);
	};
	HANDLERS[OP_SWAP] = function (frame) {
		const s = frame.stack;
		const a = s.pop();
		const b = s.pop();
		s.push(a, b);
	};
	HANDLERS[OP_SET_COMPLETION] = function (frame) {
		frame.completion = frame.stack.pop();
	};

	HANDLERS[OP_PUSH_CONST] = function (frame, arg, consts) {
		const value = consts[arg];
		frame.stack.push(
			value instanceof NativeRegExp
				? new NativeRegExp(value.source, value.flags)
				: value,
		);
	};
	HANDLERS[OP_PUSH_UNDEF] = function (frame) {
		frame.stack.push(undefined);
	};
	HANDLERS[OP_PUSH_NULL] = function (frame) {
		frame.stack.push(null);
	};
	HANDLERS[OP_PUSH_TRUE] = function (frame) {
		frame.stack.push(true);
	};
	HANDLERS[OP_PUSH_FALSE] = function (frame) {
		frame.stack.push(false);
	};
	HANDLERS[OP_PUSH_THIS] = function (frame) {
		frame.stack.push(frame.thisVal);
	};

	HANDLERS[OP_LOAD_SLOT] = function (frame, arg) {
		const b = scopeAt(frame.scope, arg >>> 16).slots[arg & 0xffff];
		if (!b.init) throw new ReferenceError(TDZ_READ);
		frame.stack.push(b.v);
	};
	HANDLERS[OP_STORE_SLOT] = function (frame, arg) {
		const v = frame.stack.pop();
		const b = scopeAt(frame.scope, arg >>> 16).slots[arg & 0xffff];
		if (b.kind === 2 && b.init)
			throw new TypeError('Assignment to constant variable.');
		if (!b.init) throw new ReferenceError(TDZ_READ);
		b.v = v;
		frame.stack.push(v);
	};
	HANDLERS[OP_INIT_SLOT] = function (frame, arg) {
		const v = frame.stack.pop();
		const b = scopeAt(frame.scope, arg >>> 16).slots[arg & 0xffff];
		b.v = v;
		b.init = true;
	};
	HANDLERS[OP_TYPEOF_SLOT] = function (frame, arg) {
		const b = scopeAt(frame.scope, arg >>> 16).slots[arg & 0xffff];
		if (!b.init) throw new ReferenceError(TDZ_READ);
		frame.stack.push(typeof b.v);
	};
	HANDLERS[OP_DECLARE_SLOT] = function (frame, arg) {
		frame.scope.slots[arg >>> 2] = {
			v: undefined,
			kind: arg & 3,
			init: (arg & 3) === 0,
		};
	};
	// arg packs slot<<8 | argIndex: bind a plain parameter to its slot.
	HANDLERS[OP_BIND_PARAM] = function (frame, arg) {
		frame.scope.slots[arg >>> 8] = {
			v: frame.args[arg & 0xff],
			kind: 0,
			init: true,
		};
	};
	// Push one call argument by index (undefined past the end). Used by default
	// parameters, which test the loaded value against undefined.
	HANDLERS[OP_LOAD_ARG] = function (frame, arg) {
		frame.stack.push(frame.args[arg]);
	};
	// Push a fresh array of the call arguments from `arg` onward. Backs a rest
	// parameter (`...xs`), which is a real Array.
	HANDLERS[OP_LOAD_ARGS_FROM] = function (frame, arg) {
		frame.stack.push(frame.args.slice(arg));
	};
	// Push a genuine (unmapped) arguments object built from the call arguments.
	// A fresh arguments object is array-like with the right length, indices, and
	// iterator, and is not an Array, matching a strict function's `arguments`.
	HANDLERS[OP_LOAD_ARGUMENTS] = function (frame) {
		frame.stack.push(buildArguments(frame.args));
	};
	// Store a freshly made closure into a hoisted function-declaration slot.
	HANDLERS[OP_DEF_FUNC_SLOT] = function (frame, arg) {
		frame.scope.slots[arg] = { v: frame.stack.pop(), kind: 0, init: true };
	};

	// Block scopes: a fresh child scope for a lexical block, popped on exit.
	HANDLERS[OP_ENTER_SCOPE] = function (frame) {
		frame.scope = { parent: frame.scope, slots: [] };
	};
	HANDLERS[OP_EXIT_SCOPE] = function (frame) {
		frame.scope = frame.scope.parent;
	};

	HANDLERS[OP_LOAD_GLOBAL] = function (frame, arg, consts, functions, globals) {
		const name = consts[arg];
		if (name === '\0t') {
			frame.stack.push((value) => `${value}`);
			return;
		}
		if (name in globals) {
			frame.stack.push(globals[name]);
			return;
		}
		throw new ReferenceError(name + ' is not defined');
	};
	HANDLERS[OP_STORE_GLOBAL] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
	) {
		const name = consts[arg];
		const v = frame.stack.pop();
		if (name in globals) {
			globals[name] = v;
			frame.stack.push(v);
			return;
		}
		throw new ReferenceError(name + ' is not defined');
	};
	HANDLERS[OP_TYPEOF_GLOBAL] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
	) {
		const name = consts[arg];
		frame.stack.push(name in globals ? typeof globals[name] : 'undefined');
	};

	HANDLERS[OP_ADD] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a + b);
	};
	HANDLERS[OP_SUB] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a - b);
	};
	HANDLERS[OP_MUL] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a * b);
	};
	HANDLERS[OP_DIV] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a / b);
	};
	HANDLERS[OP_MOD] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a % b);
	};
	HANDLERS[OP_EXP] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a ** b);
	};
	HANDLERS[OP_EQ] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a == b);
	};
	HANDLERS[OP_NEQ] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a != b);
	};
	HANDLERS[OP_SEQ] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a === b);
	};
	HANDLERS[OP_SNEQ] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a !== b);
	};
	HANDLERS[OP_LT] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a < b);
	};
	HANDLERS[OP_LTE] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a <= b);
	};
	HANDLERS[OP_GT] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a > b);
	};
	HANDLERS[OP_GTE] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a >= b);
	};
	HANDLERS[OP_BAND] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a & b);
	};
	HANDLERS[OP_BOR] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a | b);
	};
	HANDLERS[OP_BXOR] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a ^ b);
	};
	HANDLERS[OP_SHL] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a << b);
	};
	HANDLERS[OP_SHR] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a >> b);
	};
	HANDLERS[OP_USHR] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a >>> b);
	};
	HANDLERS[OP_NOT] = function (frame) {
		const s = frame.stack;
		s.push(!s.pop());
	};
	HANDLERS[OP_NEG] = function (frame) {
		const s = frame.stack;
		s.push(-s.pop());
	};
	HANDLERS[OP_POS] = function (frame) {
		const s = frame.stack;
		s.push(+s.pop());
	};
	HANDLERS[OP_BNOT] = function (frame) {
		const s = frame.stack;
		s.push(~s.pop());
	};
	HANDLERS[OP_TYPEOF] = function (frame) {
		const s = frame.stack;
		s.push(typeof s.pop());
	};
	HANDLERS[OP_INSTANCEOF] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a instanceof b);
	};
	HANDLERS[OP_IN] = function (frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a in b);
	};

	HANDLERS[OP_GET_PROP] = function (frame, arg, consts) {
		const s = frame.stack;
		const o = s.pop();
		s.push(o[consts[arg]]);
	};
	HANDLERS[OP_GET_PROP_DYN] = function (frame) {
		const s = frame.stack;
		const k = s.pop();
		const o = s.pop();
		s.push(o[k]);
	};
	HANDLERS[OP_SET_PROP] = function (frame, arg, consts) {
		const s = frame.stack;
		const v = s.pop();
		const o = s.pop();
		o[consts[arg]] = v;
		s.push(v);
	};
	HANDLERS[OP_SET_PROP_DYN] = function (frame) {
		const s = frame.stack;
		const v = s.pop();
		const k = s.pop();
		const o = s.pop();
		o[k] = v;
		s.push(v);
	};
	// `delete` over a member target. estrogen compiles strict ES modules, where a
	// delete that returns false is a TypeError; a sloppy-mode `delete` returns
	// false instead. Raising on a false result replays the strict behavior no
	// matter which mode this runtime itself runs in.
	HANDLERS[OP_DELETE_PROP] = function (frame, arg, consts) {
		const s = frame.stack;
		const o = s.pop();
		const key = consts[arg];
		if (!delete o[key])
			throw new TypeError("Cannot delete property '" + key + "' of " + o);
		s.push(true);
	};
	HANDLERS[OP_DELETE_PROP_DYN] = function (frame) {
		const s = frame.stack;
		const k = s.pop();
		const o = s.pop();
		if (!delete o[k])
			throw new TypeError("Cannot delete property '" + String(k) + "' of " + o);
		s.push(true);
	};

	HANDLERS[OP_JMP] = function (frame, arg) {
		frame.pc = arg;
	};
	HANDLERS[OP_JMP_IF_FALSE] = function (frame, arg) {
		if (!frame.stack.pop()) frame.pc = arg;
	};
	HANDLERS[OP_JMP_IF_TRUE] = function (frame, arg) {
		if (frame.stack.pop()) frame.pc = arg;
	};
	HANDLERS[OP_JMP_IF_FALSY_KEEP] = function (frame, arg) {
		const s = frame.stack;
		if (!s[s.length - 1]) frame.pc = arg;
		else s.pop();
	};
	HANDLERS[OP_JMP_IF_TRUTHY_KEEP] = function (frame, arg) {
		const s = frame.stack;
		if (s[s.length - 1]) frame.pc = arg;
		else s.pop();
	};
	HANDLERS[OP_JMP_IF_NOT_NULLISH_KEEP] = function (frame, arg) {
		const s = frame.stack;
		if (s[s.length - 1] != null) frame.pc = arg;
		else s.pop();
	};
	HANDLERS[OP_THROW] = function (frame) {
		throw frame.stack.pop();
	};
	HANDLERS[OP_PUSH_HANDLER] = function (frame, target) {
		frame.handlers.push({
			type: HANDLER_CATCH,
			pc: target,
			scope: frame.scope,
			stackLength: frame.stack.length,
		});
	};
	HANDLERS[OP_POP_HANDLER] = function (frame) {
		frame.handlers.pop();
	};
	// A finally handler. It stays on the stack while the try body and any catch
	// run, so an abrupt completion (throw, return) that crosses it routes a
	// completion record through the finally block before leaving.
	HANDLERS[OP_PUSH_FINALLY] = function (frame, target) {
		frame.handlers.push({
			type: HANDLER_FINALLY,
			pc: target,
			scope: frame.scope,
			stackLength: frame.stack.length,
		});
	};
	// Normal completion of a try: the finally runs and then falls through to the
	// code after it.
	HANDLERS[OP_PUSH_COMPLETION_NORMAL] = function (frame) {
		frame.completions.push({ kind: DONE_NORMAL });
	};
	// Run after a finally block. Replay the pending completion: a normal one
	// jumps past the finally, a throw rethrows, a return re-returns (routing
	// through any outer finally in turn).
	HANDLERS[OP_END_FINALLY] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
		createClosure,
		callValue,
		doReturn,
	) {
		const record = frame.completions.pop();
		if (record.kind === DONE_NORMAL) {
			frame.pc = arg;
			return;
		}
		if (record.kind === DONE_THROW) throw record.value;
		return doReturn(frame, record.value);
	};
	// Suspend the frame with the value on top of the stack. The async driver
	// resumes the frame after the value settles, pushing the resolved value
	// back (or routing a rejection through the handler stack). AWAIT_TOP is the
	// module-root form a top-level await compiles to; it suspends the same way.
	HANDLERS[OP_AWAIT] = function (frame) {
		return { type: SIG_AWAIT, value: frame.stack.pop() };
	};
	HANDLERS[OP_AWAIT_TOP] = function (frame) {
		return { type: SIG_AWAIT, value: frame.stack.pop() };
	};
	// Suspend a generator frame with the yielded value. The generator driver
	// returns it as { value, done: false }; the next resume pushes the value
	// sent to next() back as this expression's result.
	HANDLERS[OP_YIELD] = function (frame) {
		return { type: SIG_YIELD, value: frame.stack.pop() };
	};
	// Suspend to delegate to the iterable on top of the stack. The generator
	// driver drives its iterator, forwarding next/throw/return, and resumes the
	// frame with the delegate's return value once it is done.
	HANDLERS[OP_YIELD_STAR] = function (frame) {
		return { type: SIG_YIELD_STAR, value: frame.stack.pop() };
	};
	// Module context handlers. moduleCtx is the tenth dispatch argument, filled
	// from the native wrapper. IMPORT_META reads import.meta, DYNAMIC_IMPORT
	// calls the native import() with the popped specifier (a promise), and
	// EXPORT_BINDING writes the popped value to the module exports object under
	// the constant-pool name in the operand.
	HANDLERS[OP_IMPORT_META] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
		createClosure,
		callValue,
		doReturn,
		buildClass,
		moduleCtx,
	) {
		frame.stack.push(moduleCtx.meta);
	};
	HANDLERS[OP_DYNAMIC_IMPORT] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
		createClosure,
		callValue,
		doReturn,
		buildClass,
		moduleCtx,
	) {
		frame.stack.push(moduleCtx.loadImport(frame.stack.pop()));
	};
	HANDLERS[OP_EXPORT_BINDING] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
		createClosure,
		callValue,
		doReturn,
		buildClass,
		moduleCtx,
	) {
		moduleCtx.exports[consts[arg]] = frame.stack.pop();
	};

	HANDLERS[OP_CALL] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
		createClosure,
		callValue,
	) {
		const s = frame.stack;
		const args = new Array(arg);
		for (let i = arg - 1; i >= 0; i--) args[i] = s.pop();
		const fn = s.pop();
		s.push(callValue(fn, undefined, args));
	};
	HANDLERS[OP_CALL_METHOD] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
		createClosure,
		callValue,
	) {
		const s = frame.stack;
		const args = new Array(arg);
		for (let i = arg - 1; i >= 0; i--) args[i] = s.pop();
		const fn = s.pop();
		const recv = s.pop();
		s.push(callValue(fn, recv, args));
	};
	HANDLERS[OP_CALL_APPLY] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
		createClosure,
		callValue,
	) {
		const s = frame.stack;
		const args = s.pop();
		const fn = s.pop();
		s.push(callValue(fn, undefined, args));
	};
	HANDLERS[OP_CALL_METHOD_APPLY] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
		createClosure,
		callValue,
	) {
		const s = frame.stack;
		const args = s.pop();
		const fn = s.pop();
		const recv = s.pop();
		s.push(callValue(fn, recv, args));
	};
	HANDLERS[OP_NEW] = function (frame, arg) {
		const s = frame.stack;
		const args = new Array(arg);
		for (let i = arg - 1; i >= 0; i--) args[i] = s.pop();
		const ctor = s.pop();
		if (typeof ctor !== 'function')
			throw new TypeError(String(ctor) + ' is not a constructor');
		s.push(Reflect.construct(ctor, args));
	};
	HANDLERS[OP_NEW_APPLY] = function (frame) {
		const s = frame.stack;
		const args = s.pop();
		const ctor = s.pop();
		if (typeof ctor !== 'function')
			throw new TypeError(String(ctor) + ' is not a constructor');
		s.push(Reflect.construct(ctor, args));
	};
	HANDLERS[OP_MAKE_FUNC] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
		createClosure,
	) {
		const def = functions[arg];
		frame.stack.push(
			createClosure(def, { scope: frame.scope, arrowThis: frame.thisVal }),
		);
	};
	HANDLERS[OP_BUILD_CLASS] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
		createClosure,
		callValue,
		doReturn,
		buildClass,
	) {
		frame.stack.push(buildClass(consts[arg], frame.scope));
	};
	HANDLERS[OP_RETURN] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
		createClosure,
		callValue,
		doReturn,
	) {
		return doReturn(frame, frame.stack.pop());
	};
	HANDLERS[OP_RETURN_UNDEF] = function (
		frame,
		arg,
		consts,
		functions,
		globals,
		createClosure,
		callValue,
		doReturn,
	) {
		return doReturn(frame, undefined);
	};

	HANDLERS[OP_ARR_EMPTY] = function (frame) {
		frame.stack.push([]);
	};
	HANDLERS[OP_ARR_APPEND] = function (frame) {
		const s = frame.stack;
		const v = s.pop();
		const arr = s[s.length - 1];
		Object.defineProperty(arr, arr.length, {
			value: v,
			writable: true,
			enumerable: true,
			configurable: true,
		});
	};
	HANDLERS[OP_ARR_SPREAD] = function (frame) {
		const s = frame.stack;
		const values = s.pop();
		const arr = s[s.length - 1];
		for (const value of values) {
			Object.defineProperty(arr, arr.length, {
				value,
				writable: true,
				enumerable: true,
				configurable: true,
			});
		}
	};
	HANDLERS[OP_OBJ_EMPTY] = function (frame) {
		frame.stack.push({});
	};
	HANDLERS[OP_OBJ_SET] = function (frame, arg, consts) {
		const s = frame.stack;
		const v = s.pop();
		const obj = s[s.length - 1];
		const key = consts[arg];
		if (key === '__proto__') {
			if (v === null || typeof v === 'object' || typeof v === 'function') {
				Object.setPrototypeOf(obj, v);
			}
		} else {
			Object.defineProperty(obj, key, {
				value: v,
				writable: true,
				enumerable: true,
				configurable: true,
			});
		}
	};
	HANDLERS[OP_OBJ_SET_DYN] = function (frame) {
		const s = frame.stack;
		const v = s.pop();
		const key = s.pop();
		const obj = s[s.length - 1];
		Object.defineProperty(obj, key, {
			value: v,
			writable: true,
			enumerable: true,
			configurable: true,
		});
	};
	// Define an accessor. A get then a set on the same configurable key merge
	// into one get/set accessor, so `{ get x() {}, set x(v) {} }` composes.
	HANDLERS[OP_OBJ_DEFINE_GETTER] = function (frame, arg, consts) {
		const s = frame.stack;
		const getter = s.pop();
		const obj = s[s.length - 1];
		Object.defineProperty(obj, consts[arg], {
			get: getter,
			enumerable: true,
			configurable: true,
		});
	};
	HANDLERS[OP_OBJ_DEFINE_SETTER] = function (frame, arg, consts) {
		const s = frame.stack;
		const setter = s.pop();
		const obj = s[s.length - 1];
		Object.defineProperty(obj, consts[arg], {
			set: setter,
			enumerable: true,
			configurable: true,
		});
	};
	HANDLERS[OP_OBJ_SPREAD] = function (frame) {
		const s = frame.stack;
		const source = s.pop();
		const obj = s[s.length - 1];
		if (source == null) return;
		for (const key of Reflect.ownKeys(Object(source))) {
			const desc = Object.getOwnPropertyDescriptor(source, key);
			if (desc && desc.enumerable) {
				Object.defineProperty(obj, key, {
					value: source[key],
					writable: true,
					enumerable: true,
					configurable: true,
				});
			}
		}
	};
	// Iterator records cache the next method once, as GetIterator does. Their
	// close method implements IteratorClose for exceptional loop completion.
	HANDLERS[OP_GET_ITERATOR] = function (frame) {
		const value = frame.stack.pop();
		const iteratorMethod = value[Symbol.iterator];
		if (typeof iteratorMethod !== 'function')
			throw new TypeError('Value is not iterable');
		const iterator = Reflect.apply(iteratorMethod, value, []);
		if (
			iterator == null ||
			(typeof iterator !== 'object' && typeof iterator !== 'function')
		)
			throw new TypeError('Iterator method must return an object');
		const next = iterator.next;
		frame.stack.push({
			nextStep() {
				const item = Reflect.apply(next, iterator, []);
				if (
					item == null ||
					(typeof item !== 'object' && typeof item !== 'function')
				)
					throw new TypeError('Iterator result must be an object');
				return item;
			},
			close(originalError, isThrow) {
				try {
					const method = iterator.return;
					if (method == null) return;
					const result = Reflect.apply(method, iterator, []);
					if (
						result == null ||
						(typeof result !== 'object' && typeof result !== 'function')
					)
						throw new TypeError('Iterator return must return an object');
				} catch (error) {
					if (!isThrow) throw error;
				}
			},
		});
	};
	// Array destructuring steps. The iterator wrapper (from GET_ITERATOR) stays
	// on the stack so later elements keep pulling from it; the value, or the
	// remaining values as an array, is pushed above it.
	HANDLERS[OP_ITER_NEXT_PUSH] = function (frame) {
		const s = frame.stack;
		const item = s[s.length - 1].nextStep();
		s.push(item.done ? undefined : item.value);
	};
	HANDLERS[OP_ITER_REST_PUSH] = function (frame) {
		const s = frame.stack;
		const iterator = s[s.length - 1];
		const rest = [];
		while (true) {
			const item = iterator.nextStep();
			if (item.done) break;
			rest.push(item.value);
		}
		s.push(rest);
	};
	// Object rest: a new object with the source's own enumerable keys minus the
	// ones already bound. OBJ_REST reads the excluded names from a JSON const;
	// OBJ_REST_DYN reads them from an array on the stack (computed keys).
	HANDLERS[OP_OBJ_REST] = function (frame, arg, consts) {
		const s = frame.stack;
		const source = Object(s.pop());
		const excluded = JSON.parse(consts[arg]);
		const rest = {};
		for (const key of Object.keys(source))
			if (excluded.indexOf(key) < 0) rest[key] = source[key];
		s.push(rest);
	};
	HANDLERS[OP_OBJ_REST_DYN] = function (frame) {
		const s = frame.stack;
		const excluded = s
			.pop()
			.map((key) => (typeof key === 'symbol' ? key : String(key)));
		const source = Object(s.pop());
		const rest = {};
		for (const key of Object.keys(source))
			if (excluded.indexOf(key) < 0) rest[key] = source[key];
		s.push(rest);
	};
	HANDLERS[OP_FOR_ITER] = function (frame, target) {
		const iterator = frame.stack.pop();
		const item = iterator.nextStep();
		if (item.done) {
			frame.pc = target;
		} else {
			frame.stack.push(item.value);
		}
	};
	HANDLERS[OP_FOR_IN_KEYS] = function (frame) {
		const value = frame.stack.pop();
		frame.stack.push(
			(function* () {
				for (const key in value) {
					yield key;
				}
			})(),
		);
	};

	// Build a runnable VM over a decoded program, and return its entry function.
	// Calling the returned function runs the main function and returns its
	// completion value. The program shape is
	// { version, layout, consts, functions, mainIndex }, where layout is
	// { version, tableSize, ops }, ops maps a per-build opcode byte to
	// [stableIndex, arity].
	function createRuntime(program, options) {
		const opts = options || {};
		const globals = opts.globals || globalThis;
		// The native module wrapper supplies these: `meta` is import.meta,
		// `exports` is the object local exports are written to, and `loadImport`
		// is the native dynamic import. Absent for a plain chunk.
		const moduleCtx = {
			meta: opts.meta,
			exports: opts.exports,
			loadImport: opts.import,
		};
		const stepCap =
			typeof opts.budget === 'number' &&
			isFinite(opts.budget) &&
			opts.budget > 0
				? opts.budget
				: 0;
		const consts = program.k.map(decodeConst);
		const functions = program.f;
		const layout = program.l;
		let steps = 0;

		const tableSize = layout.tableSize;
		const arityByByte = new Array(tableSize).fill(0);
		const handlers = new Array(tableSize);
		const trap = function () {
			throw fatal('illegal instruction (trap slot)');
		};
		for (let b = 0; b < tableSize; b++) handlers[b] = trap;
		const ops = layout.ops;
		for (const byteStr of Object.keys(ops)) {
			const byte = Number(byteStr);
			const pair = ops[byteStr];
			const handler = HANDLERS[pair[0]];
			if (handler) handlers[byte] = handler;
			arityByByte[byte] = pair[1];
		}

		function makeFrame(def, closureScope, thisVal, args) {
			return {
				code: def.c,
				pc: 0,
				stack: [],
				handlers: [],
				completions: [],
				scope: { parent: closureScope, slots: [] },
				thisVal,
				args,
				completion: undefined,
			};
		}

		// Wrap a function def in a JS closure. ctx carries the defining frame's
		// scope (lexical capture) and this value (used by arrows). A plain
		// function takes its this from the call site; an arrow takes it from ctx.
		// Calling the closure makes a fresh frame whose base scope's parent is the
		// captured scope, so slot addresses resolve across the closure boundary.
		function createClosure(def, ctx) {
			'use strict';
			const drive = def.g ? makeGenerator : def.a ? runAsync : runSync;
			const fn = def.r
				? (...callArgs) =>
						drive(makeFrame(def, ctx.scope, ctx.arrowThis, callArgs))
				: function (...callArgs) {
						return drive(makeFrame(def, ctx.scope, this, callArgs));
					};
			try {
				Object.defineProperty(fn, 'length', {
					value: def.p || 0,
					configurable: true,
				});
			} catch (e) {}
			try {
				Object.defineProperty(fn, 'name', {
					value: def.n || '',
					configurable: true,
				});
			} catch (e) {}
			return fn;
		}

		function buildClass(desc, classScope) {
			'use strict';
			const defineField = (receiver, field) => {
				const value =
					field.fn === null
						? undefined
						: runSync(makeFrame(functions[field.fn], classScope, receiver, []));
				Object.defineProperty(receiver, field.key, {
					value,
					writable: true,
					enumerable: true,
					configurable: true,
				});
			};
			const Class = function (...args) {
				if (!new.target)
					throw new TypeError(
						'Class constructor cannot be invoked without new',
					);
				for (const field of desc.fields) {
					if (!field.static) defineField(this, field);
				}
				if (desc.ctor !== null) {
					const value = runSync(
						makeFrame(functions[desc.ctor], classScope, this, args),
					);
					if (
						value !== null &&
						(typeof value === 'object' || typeof value === 'function')
					) {
						return value;
					}
				}
			};
			Object.defineProperty(Class, 'name', {
				value: desc.name,
				configurable: true,
			});
			Object.defineProperty(Class, 'length', {
				value: desc.ctor === null ? 0 : functions[desc.ctor].p || 0,
				configurable: true,
			});
			Object.defineProperty(Class, 'prototype', { writable: false });
			for (const method of desc.methods) {
				const key = method.key;
				const def = functions[method.fn];
				// Concise methods have no own prototype and cannot be constructed.
				const fn = {
					[key](...args) {
						return runSync(makeFrame(def, classScope, this, args));
					},
				}[key];
				Object.defineProperty(fn, 'length', {
					value: def.p || 0,
					configurable: true,
				});
				Object.defineProperty(method.static ? Class : Class.prototype, key, {
					value: fn,
					writable: true,
					enumerable: false,
					configurable: true,
				});
			}
			if (desc.bind !== null) {
				const slot = scopeAt(classScope, desc.bind >>> 16).slots[
					desc.bind & 0xffff
				];
				slot.v = Class;
				slot.init = true;
			}
			for (const field of desc.fields) {
				if (field.static) defineField(Class, field);
			}
			return Class;
		}

		function callValue(fn, thisArg, args) {
			if (typeof fn !== 'function')
				throw new TypeError(String(fn) + ' is not a function');
			return Reflect.apply(fn, thisArg, args);
		}

		// Perform a return, routing through any active finally handlers. Pops
		// catch handlers it leaves; at the nearest finally it records the pending
		// return, jumps to the finally block, and returns null so the frame keeps
		// running (END_FINALLY completes the return). With no finally left it
		// returns the return signal the driver and runSync turn into the result.
		function doReturn(frame, value) {
			while (frame.handlers.length) {
				const handler = frame.handlers.pop();
				if (handler.type === HANDLER_FINALLY) {
					frame.scope = handler.scope;
					frame.stack.length = handler.stackLength;
					frame.completions.push({ kind: DONE_RETURN, value });
					frame.pc = handler.pc;
					return null;
				}
			}
			return { type: SIG_RETURN, value };
		}

		// Route a thrown value into the frame's nearest handler. Pops one handler
		// and restores its scope and stack height. A catch lands the error on the
		// stack and jumps to the catch body; a finally records the pending throw
		// and jumps to the finally block (END_FINALLY rethrows). Returns true when
		// a handler took over, false when the value should propagate out of the
		// frame. Shared by the synchronous dispatch loop and the async driver's
		// rejection path.
		function unwindFrame(frame, error) {
			const handler = frame.handlers.pop();
			if (!handler) return false;
			frame.scope = handler.scope;
			frame.stack.length = handler.stackLength;
			if (handler.type === HANDLER_FINALLY) {
				frame.completions.push({ kind: DONE_THROW, value: error });
			} else {
				frame.stack.push(error);
			}
			frame.pc = handler.pc;
			return true;
		}

		// The one interpreter loop. Returns a frame signal { type, value }: a
		// return or halt ends the frame, an await suspends it. A thrown error is
		// routed into the frame's handler stack; with no handler it propagates
		// out as a real throw.
		function runFrame(frame) {
			const code = frame.code;
			while (true) {
				burstStep();
				const op = code[frame.pc];
				const arity = arityByByte[op];
				const arg = arity ? code[frame.pc + 1] : 0;
				frame.pc += 1 + arity;
				let sig;
				try {
					sig = handlers[op](
						frame,
						arg,
						consts,
						functions,
						globals,
						createClosure,
						callValue,
						doReturn,
						buildClass,
						moduleCtx,
					);
				} catch (error) {
					if (!unwindFrame(frame, error)) throw error;
					continue;
				}
				if (sig) return sig;
			}
		}

		function burstStep() {
			steps++;
			if (stepCap && steps > stepCap) throw fatal('step budget exceeded');
		}

		function runSync(frame) {
			const sig = runFrame(frame);
			if (sig.type === SIG_AWAIT)
				throw new TypeError('await is only valid in async functions');
			return sig.value;
		}

		// Drive a generator frame. Calling the generator function returns this
		// object; each next(v) runs the frame from where the last yield left it
		// until the next yield or the end. The compiler only emits a generator
		// whose body has no try or for-of/in, so the frame never holds a handler
		// across a yield: throw(e) with no handler propagates and return(v) stops
		// the generator, which is the correct behavior without a pending finally.
		function makeGenerator(frame) {
			let done = false;
			let started = false;
			// The iterator of an active `yield*`, or null. While set, next/throw/
			// return forward to it instead of the frame.
			let delegate = null;

			// Run the frame to the next suspend. A plain yield yields its value; a
			// yield* starts delegating to the iterable's iterator.
			function runStep() {
				let sig;
				try {
					sig = runFrame(frame);
				} catch (error) {
					done = true;
					throw error;
				}
				if (sig.type === SIG_YIELD) return { value: sig.value, done: false };
				if (sig.type === SIG_YIELD_STAR) {
					const method = sig.value[Symbol.iterator];
					if (typeof method !== 'function')
						throw new TypeError('Value is not iterable');
					delegate = Reflect.apply(method, sig.value, []);
					if (
						delegate == null ||
						(typeof delegate !== 'object' && typeof delegate !== 'function')
					)
						throw new TypeError('Iterator method must return an object');
					return driveDelegate(undefined, 'next');
				}
				done = true;
				return { value: sig.value, done: true };
			}

			// Resume the frame body after a value lands on the stack.
			function resumeFrame(value, mode) {
				if (mode === 'throw') {
					if (!unwindFrame(frame, value)) {
						done = true;
						throw value;
					}
				} else {
					frame.stack.push(value);
				}
				return runStep();
			}

			// Forward one next/throw/return into the active delegate. On a normal
			// result it yields the delegate's value; when the delegate is done it
			// resumes the frame with the delegate's return value as the yield*
			// result (mode next), or ends this generator (mode return).
			function driveDelegate(value, mode) {
				let result;
				try {
					if (mode === 'throw') {
						const thrower = delegate.throw;
						if (thrower == null) {
							const closer = delegate.return;
							if (closer != null) Reflect.apply(closer, delegate, []);
							delegate = null;
							return resumeFrame(
								new TypeError('The iterator does not provide a throw method'),
								'throw',
							);
						}
						result = Reflect.apply(thrower, delegate, [value]);
					} else if (mode === 'return') {
						const closer = delegate.return;
						if (closer == null) {
							delegate = null;
							done = true;
							return { value: value, done: true };
						}
						result = Reflect.apply(closer, delegate, [value]);
					} else {
						result = Reflect.apply(delegate.next, delegate, [value]);
					}
				} catch (error) {
					delegate = null;
					return resumeFrame(error, 'throw');
				}
				if (
					result == null ||
					(typeof result !== 'object' && typeof result !== 'function')
				)
					throw new TypeError('Iterator result must be an object');
				if (result.done) {
					delegate = null;
					if (mode === 'return') {
						done = true;
						return { value: result.value, done: true };
					}
					return resumeFrame(result.value, 'next');
				}
				return { value: result.value, done: false };
			}

			function resume(value, mode) {
				if (done) {
					if (mode === 'throw') throw value;
					return { value: mode === 'return' ? value : undefined, done: true };
				}
				if (delegate) return driveDelegate(value, mode);
				if (!started && mode !== 'next') {
					// A throw or return before the body starts never runs it.
					done = true;
					if (mode === 'throw') throw value;
					return { value: value, done: true };
				}
				if (!started) {
					started = true;
					return runStep();
				}
				if (mode === 'return') {
					done = true;
					return { value: value, done: true };
				}
				return resumeFrame(value, mode);
			}
			const gen = {
				next: function (value) {
					return resume(value, 'next');
				},
				throw: function (error) {
					return resume(error, 'throw');
				},
				return: function (value) {
					return resume(value, 'return');
				},
			};
			gen[Symbol.iterator] = function () {
				return this;
			};
			return gen;
		}

		// Drive an async frame to a promise. Each burst runs the frame until it
		// returns, halts, or awaits. On await, the value is resolved and the
		// frame resumed with the result pushed, or the rejection routed through
		// its handler stack. A frame that returns or halts settles the promise.
		function runAsync(frame) {
			return new Promise(function (resolve, reject) {
				function settle(sig) {
					if (sig.type === SIG_AWAIT) {
						Promise.resolve(sig.value).then(
							function (value) {
								frame.stack.push(value);
								step();
							},
							function (error) {
								if (!unwindFrame(frame, error)) {
									reject(error);
									return;
								}
								step();
							},
						);
						return;
					}
					resolve(sig.value);
				}
				function step() {
					let sig;
					try {
						sig = runFrame(frame);
					} catch (error) {
						reject(error);
						return;
					}
					settle(sig);
				}
				step();
			});
		}

		function run(args) {
			steps = 0;
			const def = functions[program.m || 0];
			const frame = makeFrame(def, null, undefined, args || []);
			return def.a ? runAsync(frame) : runSync(frame);
		}

		return run;
	}

	return createRuntime;
})();
