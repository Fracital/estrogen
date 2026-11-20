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
	// a halt returns the main completion value.
	const SIG_RETURN = 0;
	const SIG_HALT = 1;

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
		OP_DYNAMIC_IMPORT = _n++, OP_IMPORT_META = _n++, OP_AWAIT_TOP = _n++, OP_SET_COMPLETION = _n++;

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
			if (entry.$ === 're') return new RegExp(entry.s, entry.f);
			if (entry.$ === 'bi') return BigInt(entry.v);
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
	HANDLERS[OP_SET_COMPLETION] = function (frame) {
		frame.completion = frame.stack.pop();
	};

	HANDLERS[OP_PUSH_CONST] = function (frame, arg, consts) {
		frame.stack.push(consts[arg]);
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
	HANDLERS[OP_NEW] = function (frame, arg) {
		const s = frame.stack;
		const args = new Array(arg);
		for (let i = arg - 1; i >= 0; i--) args[i] = s.pop();
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
		s[s.length - 1].push(v);
	};
	HANDLERS[OP_OBJ_EMPTY] = function (frame) {
		frame.stack.push({});
	};
	HANDLERS[OP_OBJ_SET] = function (frame, arg, consts) {
		const s = frame.stack;
		const v = s.pop();
		s[s.length - 1][consts[arg]] = v;
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
		const stepCap =
			typeof opts.budget === 'number' &&
			isFinite(opts.budget) &&
			opts.budget > 0
				? opts.budget
				: 0;
		const consts = program.consts.map(decodeConst);
		const functions = program.functions;
		const layout = program.layout;
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
				code: def.code,
				pc: 0,
				stack: [],
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
			const fn = function (...callArgs) {
				const thisVal = def.isArrow ? ctx.arrowThis : this;
				return runSync(makeFrame(def, ctx.scope, thisVal, callArgs));
			};
			try {
				Object.defineProperty(fn, 'length', {
					value: def.numParams,
					configurable: true,
				});
			} catch (e) {}
			if (def.name) {
				try {
					Object.defineProperty(fn, 'name', {
						value: def.name,
						configurable: true,
					});
				} catch (e) {}
			}
			return fn;
		}

		function callValue(fn, thisArg, args) {
			if (typeof fn !== 'function')
				throw new TypeError(String(fn) + ' is not a function');
			return Reflect.apply(fn, thisArg, args);
		}

		// Perform a return. estrogen does not emit the try/catch/finally handler
		// stack yet, so a return leaves the frame directly with a return signal.
		function doReturn(frame, value) {
			return { type: SIG_RETURN, value };
		}

		// The one interpreter loop. Returns a frame signal { type, value }. The
		// subset has no in-frame handlers, so a thrown error propagates straight
		// out.
		function runFrame(frame) {
			const code = frame.code;
			while (true) {
				burstStep();
				const op = code[frame.pc];
				const arity = arityByByte[op];
				const arg = arity ? code[frame.pc + 1] : 0;
				frame.pc += 1 + arity;
				const sig = handlers[op](
					frame,
					arg,
					consts,
					functions,
					globals,
					createClosure,
					callValue,
					doReturn,
				);
				if (sig) return sig;
			}
		}

		function burstStep() {
			steps++;
			if (stepCap && steps > stepCap) throw fatal('step budget exceeded');
		}

		function runSync(frame) {
			const sig = runFrame(frame);
			if (sig.type === SIG_RETURN || sig.type === SIG_HALT) return sig.value;
			throw sig.value;
		}

		function run(args) {
			steps = 0;
			const def = functions[program.mainIndex];
			const frame = makeFrame(def, null, undefined, args || []);
			return runSync(frame);
		}

		return run;
	}

	return createRuntime;
})();
