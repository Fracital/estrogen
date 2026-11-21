//! The compiled program and its on-disk shape.
//!
//! A program is `{ version, layout, consts, functions, mainIndex }`, the same
//! JSON object abulxt's shield runtime executes (its `ShieldProgram`, minus the
//! module descriptor estrogen does not emit yet). estrogen serializes it, then
//! wraps the JSON in its own ChaCha20 pool so the bytecode and the string
//! constants never ship in the clear. The runtime decodes the pool, JSON-parses
//! the program, and runs it. See [`super`] and docs/SHIELD-VM.md.

use std::collections::HashMap;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{Map, Value};

use super::layout::Layout;

/// Encode a flat code array as a base64 string of unsigned LEB128 varints. Every
/// code value is non-negative (opcode bytes and operands: const indices, packed
/// slot addresses, absolute jump targets, and counts), so unsigned LEB128 is
/// exact. This is far smaller than a JSON number array, which pays a comma and
/// several digits per value; the runtime unpacks it once when the program loads.
fn encode_code(code: &[i64]) -> String {
    let mut bytes = Vec::with_capacity(code.len());
    for &n in code {
        debug_assert!(
            n >= 0,
            "VM code value must be non-negative for unsigned LEB128, got {n}"
        );
        let mut v = n as u64;
        loop {
            let mut byte = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if v == 0 {
                break;
            }
        }
    }
    STANDARD.encode(&bytes)
}

/// One compiled function: a flat code array, plus the metadata the runtime uses
/// to build a closure. The slice emits one function (the module main), but the
/// shape carries the rest so it matches the runtime verbatim.
pub struct FunctionDef {
    /// Opcode bytes and their inline operands, in execution order.
    pub code: Vec<i64>,
    /// The function name, or `None` for the anonymous main.
    pub name: Option<String>,
    /// Whether the function body is async (top-level await). Always false in the
    /// slice.
    pub is_async: bool,
    /// Whether it is an arrow. Always false in the slice.
    pub is_arrow: bool,
    /// Whether it is a generator. Calling it returns a generator object the
    /// runtime drives: the body runs to each `yield` and resumes on `next`.
    pub is_generator: bool,
    /// Declared parameter count. Zero for the main.
    pub num_params: usize,
}

impl FunctionDef {
    /// Serialize with single-character keys and no default fields, so a
    /// function costs the fewest bytes in the encrypted program. The runtime
    /// defaults every omitted field: `c`=code, `n`=name, `a`=isAsync,
    /// `r`=isArrow, `g`=isGenerator, `p`=numParams. A false flag, a zero param
    /// count, and a missing name are left out.
    fn to_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("c".to_string(), Value::from(encode_code(&self.code)));
        if let Some(name) = &self.name {
            obj.insert("n".to_string(), Value::from(name.clone()));
        }
        if self.is_async {
            obj.insert("a".to_string(), Value::from(1u8));
        }
        if self.is_arrow {
            obj.insert("r".to_string(), Value::from(1u8));
        }
        if self.is_generator {
            obj.insert("g".to_string(), Value::from(1u8));
        }
        if self.num_params > 0 {
            obj.insert("p".to_string(), Value::from(self.num_params as u64));
        }
        Value::Object(obj)
    }
}

/// An interned constant pool. A repeat of the same value returns the first
/// index, so the pool holds one entry per distinct constant, matching the
/// shield compiler's `addConst`.
#[derive(Default)]
pub struct ConstPool {
    entries: Vec<Value>,
    index: HashMap<String, usize>,
}

impl ConstPool {
    /// Store a class descriptor among the VM constants. It contains function
    /// indices and plain property names, and is consumed only by BUILD_CLASS.
    pub fn add_class(&mut self, descriptor: Value) -> usize {
        let key = format!("class:{descriptor}");
        self.intern(key, || descriptor)
    }

    /// Intern a string constant and return its pool index.
    pub fn add_str(&mut self, s: &str) -> usize {
        self.intern(format!("s:{s}"), || Value::from(s.to_string()))
    }

    /// Store an abrupt-jump record `{ t: target, s: stop }` that ABRUPT_JUMP
    /// reads to route a break or continue through the finally chain. Each is a
    /// distinct code position, so it is not interned.
    pub fn add_jump_record(&mut self, target: i64, stop: i64) -> usize {
        let mut obj = Map::new();
        obj.insert("t".to_string(), Value::from(target));
        obj.insert("s".to_string(), Value::from(stop));
        let index = self.entries.len();
        self.entries.push(Value::Object(obj));
        index
    }

    /// Intern a finite number constant and return its pool index. NaN and the
    /// infinities are stored as tagged objects the runtime's `decodeConst`
    /// reverses, because JSON cannot carry them.
    pub fn add_num(&mut self, n: f64) -> usize {
        let encoded = encode_num(n);
        let key = format!("j:{encoded}");
        self.intern(key, || encoded.clone())
    }

    /// Intern a BigInt constant (its base-10 digit string) and return its index.
    pub fn add_bigint(&mut self, digits: &str) -> usize {
        let key = format!("bi:{digits}");
        self.intern(key, || {
            let mut obj = Map::new();
            obj.insert("$".to_string(), Value::from("bi"));
            obj.insert("v".to_string(), Value::from(digits.to_string()));
            Value::Object(obj)
        })
    }

    /// Intern a regexp's pattern and flags. The runtime constructs a fresh
    /// RegExp on each PUSH_CONST, matching literal evaluation semantics.
    pub fn add_regexp(&mut self, pattern: &str, flags: &str) -> usize {
        self.intern(format!("re:{pattern}/{flags}"), || {
            let mut obj = Map::new();
            obj.insert("$".to_string(), Value::from("re"));
            obj.insert("s".to_string(), Value::from(pattern.to_string()));
            obj.insert("f".to_string(), Value::from(flags.to_string()));
            Value::Object(obj)
        })
    }

    /// Intern a tagged-template strings object. The runtime's `decodeConst`
    /// builds one frozen array (with its frozen `.raw`) per call site at load,
    /// so every evaluation of the site passes the same object, as the language
    /// requires. A `None` cooked value (an invalid escape in a tagged template)
    /// becomes `undefined` in the cooked array while `.raw` keeps the text.
    pub fn add_template(&mut self, cooked: &[Option<String>], raw: &[String]) -> usize {
        let key = format!("tpl:{cooked:?}|{raw:?}");
        self.intern(key, || {
            let mut obj = Map::new();
            obj.insert("$".to_string(), Value::from("tpl"));
            obj.insert(
                "c".to_string(),
                Value::Array(
                    cooked
                        .iter()
                        .map(|c| c.as_ref().map_or(Value::Null, |s| Value::from(s.clone())))
                        .collect(),
                ),
            );
            obj.insert(
                "r".to_string(),
                Value::Array(raw.iter().map(|s| Value::from(s.clone())).collect()),
            );
            Value::Object(obj)
        })
    }

    fn intern(&mut self, key: String, make: impl FnOnce() -> Value) -> usize {
        if let Some(&i) = self.index.get(&key) {
            return i;
        }
        let i = self.entries.len();
        self.entries.push(make());
        self.index.insert(key, i);
        i
    }

    fn into_values(self) -> Vec<Value> {
        self.entries
    }
}

/// Encode a number the way the shield's `encodeConst` does: a tagged object for
/// the values JSON cannot represent, else the number itself.
fn encode_num(n: f64) -> Value {
    fn tag(v: &str) -> Value {
        let mut obj = Map::new();
        obj.insert("$".to_string(), Value::from("n"));
        obj.insert("v".to_string(), Value::from(v));
        Value::Object(obj)
    }
    if n.is_nan() {
        return tag("NaN");
    }
    if n == f64::INFINITY {
        return tag("Inf");
    }
    if n == f64::NEG_INFINITY {
        return tag("-Inf");
    }
    if n == 0.0 && n.is_sign_negative() {
        return tag("-0");
    }
    Value::from(n)
}

/// A whole compiled program, ready to serialize.
pub struct Program {
    /// The per-build opcode layout.
    pub layout: Layout,
    /// The constant pool.
    pub consts: ConstPool,
    /// The functions; `main_index` names the entry.
    pub functions: Vec<FunctionDef>,
    /// Index of the module main in `functions`.
    pub main_index: usize,
    /// The chunk reads `import.meta`, so the wrapper passes the module's meta.
    /// Not serialized; it drives the emitted wrapper only.
    pub uses_import_meta: bool,
    /// The chunk uses dynamic `import()`, so the wrapper passes native import.
    pub uses_dynamic_import: bool,
    /// Names the chunk exports from its own bindings, in declaration order. The
    /// wrapper re-exports each from the module exports object the VM fills.
    pub exports: Vec<String>,
}

impl Program {
    /// The program as its JSON value, the exact shape the runtime consumes.
    pub fn into_json(self) -> Value {
        // Short keys, and the unused version tag and a zero main index are left
        // out: `l`=layout, `k`=consts, `f`=functions, `m`=mainIndex.
        let mut obj = Map::new();
        obj.insert("l".to_string(), self.layout.to_json());
        obj.insert("k".to_string(), Value::Array(self.consts.into_values()));
        obj.insert(
            "f".to_string(),
            Value::Array(self.functions.iter().map(FunctionDef::to_json).collect()),
        );
        if self.main_index > 0 {
            obj.insert("m".to_string(), Value::from(self.main_index as u64));
        }
        Value::Object(obj)
    }

    /// The program serialized to a compact JSON string.
    pub fn into_json_string(self) -> String {
        self.into_json().to_string()
    }

    /// The set of opcode names the program's bytecode actually references. Walk
    /// every function's code, mapping each per-build byte back to its stable
    /// opcode through the layout and skipping its operands by arity. The runtime
    /// emitted for this chunk keeps only the handlers for these opcodes; every
    /// other byte stays a trap slot that the code never reaches.
    pub fn used_opcode_names(&self) -> std::collections::HashSet<&'static str> {
        use super::opcodes::{arity, OP_NAMES};
        // Per-build byte -> (stable opcode index, arity).
        let mut by_byte: [(usize, usize); 256] = [(0, 0); 256];
        for (index, name) in OP_NAMES.iter().enumerate() {
            by_byte[self.layout.byte_of(name) as usize] = (index, arity(name));
        }
        let mut used = std::collections::HashSet::new();
        for function in &self.functions {
            let mut pos = 0usize;
            while pos < function.code.len() {
                let byte = (function.code[pos] as usize) & 0xff;
                let (index, operands) = by_byte[byte];
                used.insert(OP_NAMES[index]);
                pos += 1 + operands;
            }
        }
        used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The packed base64 form of a representative code array is smaller than the
    /// JSON number array it replaces, and the varints decode back to the input.
    #[test]
    fn packed_code_is_smaller_and_round_trips() {
        let code: Vec<i64> = vec![12, 5, 34, 0, 100, 200, 5, 65536, 131073, 42, 7];
        let packed = encode_code(&code);
        let array = serde_json::to_string(&code).unwrap();
        assert!(
            packed.len() < array.len(),
            "packed {packed:?} ({} B) should beat array {array:?} ({} B)",
            packed.len(),
            array.len(),
        );

        // Decode the base64 LEB128 the way the runtime does, and confirm it
        // reconstructs the input exactly.
        let bytes = STANDARD.decode(packed).unwrap();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let mut val: u64 = 0;
            let mut shift = 0;
            loop {
                let b = bytes[i];
                i += 1;
                val |= u64::from(b & 0x7f) << shift;
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
            }
            out.push(val as i64);
        }
        assert_eq!(out, code);
    }
}
