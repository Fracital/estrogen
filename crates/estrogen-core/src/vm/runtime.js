// estrogen VM runtime (execution core).
//
// This is the stack-bytecode interpreter estrogen's VM pass compiles to. It is a
// trimmed, dependency-free port of abulxt's shield runtime-template.js: the
// handler-table dispatch, the slot-based scope model, and the opcode handler
// bodies are copied byte-for-byte for the opcodes estrogen's compiler emits, so a
// program this file runs has the exact bytecode and constant-pool shape the
// shield runtime executes. The parts of the shield runtime that the slice does
// not need are left out: XChaCha20-Poly1305 decryption (estrogen wraps the
// program in its own ChaCha20 pool decoder instead), the anti-tamper snapshot,
// the async/class/module/exception handlers, and the whole-chunk loader.
//
// The program shape is { version, layout, consts, functions, mainIndex }, where
// layout is { version, tableSize, byteOf, nameOf }, a function is
// { code, name, isAsync, isArrow, numParams }, and the main function ends with
// SET_COMPLETION on the last top-level expression then HALT, which returns the
// completion value. See docs/SHIELD-VM.md.
//
// It raises the cost of reading a build. It is not secrecy: the pool key ships
// in the output next to the decoder, so anyone can run it back.

const ARITY = {
	HALT: 0,
	NOP: 0,
	POP: 0,
	DUP: 0,
	DUP2: 0,
	SWAP: 0,
	ROTL3: 0,
	PUSH_CONST: 1,
	PUSH_UNDEF: 0,
	PUSH_NULL: 0,
	PUSH_TRUE: 0,
	PUSH_FALSE: 0,
	PUSH_THIS: 0,
	LOAD_VAR: 1,
	STORE_VAR: 1,
	INIT_VAR: 1,
	DECLARE_VAR: 1,
	DECLARE_LET: 1,
	DECLARE_CONST: 1,
	DEF_FUNC: 1,
	TYPEOF_VAR: 1,
	LOAD_ARG: 1,
	LOAD_ARGS_FROM: 1,
	LOAD_SLOT: 1,
	STORE_SLOT: 1,
	INIT_SLOT: 1,
	TYPEOF_SLOT: 1,
	DECLARE_SLOT: 1,
	DEF_FUNC_SLOT: 1,
	LOAD_GLOBAL: 1,
	STORE_GLOBAL: 1,
	TYPEOF_GLOBAL: 1,
	IMPORT_SLOT: 1,
	BIND_PARAM: 1,
	LOAD_SLOT_PROP: 1,
	LOAD_GLOBAL_PROP: 1,
	LOAD_SLOT_DUP_PROP: 1,
	LOAD_GLOBAL_DUP_PROP: 1,
	OBJ_SET_CONST: 1,
	DECLARE_SLOTS: 1,
	ENTER_SCOPE: 0,
	EXIT_SCOPE: 0,
	ADD: 0,
	SUB: 0,
	MUL: 0,
	DIV: 0,
	MOD: 0,
	EXP: 0,
	EQ: 0,
	NEQ: 0,
	SEQ: 0,
	SNEQ: 0,
	LT: 0,
	LTE: 0,
	GT: 0,
	GTE: 0,
	BAND: 0,
	BOR: 0,
	BXOR: 0,
	SHL: 0,
	SHR: 0,
	USHR: 0,
	NOT: 0,
	NEG: 0,
	POS: 0,
	BNOT: 0,
	TYPEOF: 0,
	INSTANCEOF: 0,
	IN: 0,
	GET_PROP: 1,
	GET_PROP_DYN: 0,
	SET_PROP: 1,
	SET_PROP_DYN: 0,
	DELETE_PROP: 1,
	DELETE_PROP_DYN: 0,
	JMP: 1,
	JMP_IF_FALSE: 1,
	JMP_IF_TRUE: 1,
	JMP_IF_FALSY_KEEP: 1,
	JMP_IF_TRUTHY_KEEP: 1,
	JMP_IF_NOT_NULLISH_KEEP: 1,
	OPT_SHORT: 1,
	OPT_SHORT_CALL: 1,
	CALL: 1,
	CALL_METHOD: 1,
	NEW: 1,
	CALL_APPLY: 0,
	CALL_METHOD_APPLY: 0,
	NEW_APPLY: 0,
	MAKE_FUNC: 1,
	RETURN: 0,
	RETURN_UNDEF: 0,
	ARR_EMPTY: 0,
	ARR_APPEND: 0,
	ARR_SPREAD: 0,
	OBJ_EMPTY: 0,
	OBJ_SET: 1,
	OBJ_SET_DYN: 0,
	OBJ_SPREAD: 0,
	OBJ_DEFINE_GETTER: 1,
	OBJ_DEFINE_SETTER: 1,
	OBJ_REST: 1,
	OBJ_REST_DYN: 0,
	GET_ITERATOR: 0,
	FOR_ITER: 1,
	ITER_NEXT_PUSH: 0,
	ITER_REST_PUSH: 0,
	FOR_IN_KEYS: 0,
	THROW: 0,
	RETHROW: 0,
	PUSH_HANDLER: 1,
	PUSH_FINALLY: 1,
	POP_HANDLER: 0,
	PUSH_COMPLETION_NORMAL: 0,
	END_FINALLY: 1,
	ABRUPT_JUMP: 1,
	AWAIT: 0,
	BUILD_CLASS: 1,
	LOAD_NEW_TARGET: 0,
	SUPER_GET: 1,
	SUPER_GET_DYN: 0,
	SUPER_SET: 1,
	SUPER_SET_DYN: 0,
	SUPER_CALL: 1,
	SUPER_CALL_APPLY: 0,
	GET_PRIVATE: 1,
	SET_PRIVATE: 1,
	HAS_PRIVATE: 1,
	INIT_PRIVATE: 1,
	MODULE_INIT: 1,
	IMPORT_BINDING: 1,
	EXPORT_BINDING: 1,
	DYNAMIC_IMPORT: 0,
	IMPORT_META: 0,
	AWAIT_TOP: 0,
	SET_COMPLETION: 0,
};

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
	const e = new Error('estrogen vm: ' + message);
	e.__estrogenFatal = true;
	return e;
}

// Slot-based locals: every lexical binding resolves to a (depth, index) pair at
// compile time. A scope is { parent, slots: [] }; a slot holds a cell
// { v, kind, init }. kind: 0=var, 1=let, 2=const.
function scopeAt(scope, depth) {
	while (depth > 0) {
		scope = scope.parent;
		depth--;
	}
	return scope;
}
const TDZ_READ = 'Cannot access a lexical binding before initialization';

function createRuntime(program, options) {
	const opts = options || {};
	const globals = opts.globals || globalThis;
	const stepCap =
		typeof opts.budget === 'number' && isFinite(opts.budget) && opts.budget > 0
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
	const nameOf = layout.nameOf;
	for (const byteStr of Object.keys(nameOf)) {
		const byte = Number(byteStr);
		const name = nameOf[byteStr];
		arityByByte[byte] = ARITY[name];
		handlers[byte] = HANDLERS[name];
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

	function callValue(fn, thisArg, args) {
		if (typeof fn !== 'function')
			throw new TypeError(String(fn) + ' is not a function');
		return Reflect.apply(fn, thisArg, args);
	}

	// The one interpreter loop. Returns a signal: {type:'return'|'halt', value}
	// or {type:'throw', value}. The slice has no in-frame handlers, so a thrown
	// error propagates straight out.
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
				callValue,
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
		if (sig.type === 'return' || sig.type === 'halt') return sig.value;
		throw sig.value;
	}

	function run(args) {
		steps = 0;
		const def = functions[program.mainIndex];
		const frame = makeFrame(def, null, undefined, args || []);
		return runSync(frame);
	}

	return {
		run,
		program,
		_consts: consts,
		get _steps() {
			return steps;
		},
	};
}

// ---- opcode handlers ----
// Bodies copied from abulxt shield runtime-template.js for the opcodes estrogen's
// VM compiler emits. Each handler receives
// (frame, arg, consts, functions, globals, callValue) and returns a signal to
// stop the frame, else nothing.
const HANDLERS = {
	HALT(frame) {
		return { type: 'halt', value: frame.completion };
	},
	NOP() {},
	POP(frame) {
		frame.stack.pop();
	},
	DUP(frame) {
		const s = frame.stack;
		s.push(s[s.length - 1]);
	},
	SET_COMPLETION(frame) {
		frame.completion = frame.stack.pop();
	},

	PUSH_CONST(frame, arg, consts) {
		frame.stack.push(consts[arg]);
	},
	PUSH_UNDEF(frame) {
		frame.stack.push(undefined);
	},
	PUSH_NULL(frame) {
		frame.stack.push(null);
	},
	PUSH_TRUE(frame) {
		frame.stack.push(true);
	},
	PUSH_FALSE(frame) {
		frame.stack.push(false);
	},
	PUSH_THIS(frame) {
		frame.stack.push(frame.thisVal);
	},

	LOAD_SLOT(frame, arg) {
		const b = scopeAt(frame.scope, arg >>> 16).slots[arg & 0xffff];
		if (!b.init) throw new ReferenceError(TDZ_READ);
		frame.stack.push(b.v);
	},
	STORE_SLOT(frame, arg) {
		const v = frame.stack.pop();
		const b = scopeAt(frame.scope, arg >>> 16).slots[arg & 0xffff];
		if (b.kind === 2 && b.init)
			throw new TypeError('Assignment to constant variable.');
		if (!b.init) throw new ReferenceError(TDZ_READ);
		b.v = v;
		frame.stack.push(v);
	},
	INIT_SLOT(frame, arg) {
		const v = frame.stack.pop();
		const b = scopeAt(frame.scope, arg >>> 16).slots[arg & 0xffff];
		b.v = v;
		b.init = true;
	},
	TYPEOF_SLOT(frame, arg) {
		const b = scopeAt(frame.scope, arg >>> 16).slots[arg & 0xffff];
		if (!b.init) throw new ReferenceError(TDZ_READ);
		frame.stack.push(typeof b.v);
	},
	DECLARE_SLOT(frame, arg) {
		frame.scope.slots[arg >>> 2] = {
			v: undefined,
			kind: arg & 3,
			init: (arg & 3) === 0,
		};
	},

	LOAD_GLOBAL(frame, arg, consts, functions, globals) {
		const name = consts[arg];
		if (name in globals) {
			frame.stack.push(globals[name]);
			return;
		}
		throw new ReferenceError(name + ' is not defined');
	},
	STORE_GLOBAL(frame, arg, consts, functions, globals) {
		const name = consts[arg];
		const v = frame.stack.pop();
		if (name in globals) {
			globals[name] = v;
			frame.stack.push(v);
			return;
		}
		throw new ReferenceError(name + ' is not defined');
	},
	TYPEOF_GLOBAL(frame, arg, consts, functions, globals) {
		const name = consts[arg];
		frame.stack.push(name in globals ? typeof globals[name] : 'undefined');
	},

	ADD(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a + b);
	},
	SUB(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a - b);
	},
	MUL(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a * b);
	},
	DIV(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a / b);
	},
	MOD(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a % b);
	},
	EXP(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a ** b);
	},
	EQ(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a == b);
	},
	NEQ(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a != b);
	},
	SEQ(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a === b);
	},
	SNEQ(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a !== b);
	},
	LT(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a < b);
	},
	LTE(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a <= b);
	},
	GT(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a > b);
	},
	GTE(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a >= b);
	},
	BAND(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a & b);
	},
	BOR(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a | b);
	},
	BXOR(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a ^ b);
	},
	SHL(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a << b);
	},
	SHR(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a >> b);
	},
	USHR(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a >>> b);
	},
	NOT(frame) {
		const s = frame.stack;
		s.push(!s.pop());
	},
	NEG(frame) {
		const s = frame.stack;
		s.push(-s.pop());
	},
	POS(frame) {
		const s = frame.stack;
		s.push(+s.pop());
	},
	BNOT(frame) {
		const s = frame.stack;
		s.push(~s.pop());
	},
	TYPEOF(frame) {
		const s = frame.stack;
		s.push(typeof s.pop());
	},
	INSTANCEOF(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a instanceof b);
	},
	IN(frame) {
		const s = frame.stack;
		const b = s.pop();
		const a = s.pop();
		s.push(a in b);
	},

	GET_PROP(frame, arg, consts) {
		const s = frame.stack;
		const o = s.pop();
		s.push(o[consts[arg]]);
	},
	GET_PROP_DYN(frame) {
		const s = frame.stack;
		const k = s.pop();
		const o = s.pop();
		s.push(o[k]);
	},

	JMP(frame, arg) {
		frame.pc = arg;
	},
	JMP_IF_FALSE(frame, arg) {
		if (!frame.stack.pop()) frame.pc = arg;
	},
	JMP_IF_TRUE(frame, arg) {
		if (frame.stack.pop()) frame.pc = arg;
	},
	JMP_IF_FALSY_KEEP(frame, arg) {
		const s = frame.stack;
		if (!s[s.length - 1]) frame.pc = arg;
		else s.pop();
	},
	JMP_IF_TRUTHY_KEEP(frame, arg) {
		const s = frame.stack;
		if (s[s.length - 1]) frame.pc = arg;
		else s.pop();
	},
	JMP_IF_NOT_NULLISH_KEEP(frame, arg) {
		const s = frame.stack;
		if (s[s.length - 1] != null) frame.pc = arg;
		else s.pop();
	},

	CALL(frame, arg, consts, functions, globals, callValue) {
		const s = frame.stack;
		const args = new Array(arg);
		for (let i = arg - 1; i >= 0; i--) args[i] = s.pop();
		const fn = s.pop();
		s.push(callValue(fn, undefined, args));
	},
	CALL_METHOD(frame, arg, consts, functions, globals, callValue) {
		const s = frame.stack;
		const args = new Array(arg);
		for (let i = arg - 1; i >= 0; i--) args[i] = s.pop();
		const fn = s.pop();
		const recv = s.pop();
		s.push(callValue(fn, recv, args));
	},
	NEW(frame, arg) {
		const s = frame.stack;
		const args = new Array(arg);
		for (let i = arg - 1; i >= 0; i--) args[i] = s.pop();
		const ctor = s.pop();
		if (typeof ctor !== 'function')
			throw new TypeError(String(ctor) + ' is not a constructor');
		s.push(Reflect.construct(ctor, args));
	},

	ARR_EMPTY(frame) {
		frame.stack.push([]);
	},
	ARR_APPEND(frame) {
		const s = frame.stack;
		const v = s.pop();
		s[s.length - 1].push(v);
	},
	OBJ_EMPTY(frame) {
		frame.stack.push({});
	},
	OBJ_SET(frame, arg, consts) {
		const s = frame.stack;
		const v = s.pop();
		s[s.length - 1][consts[arg]] = v;
	},
};

export { createRuntime, ARITY };
