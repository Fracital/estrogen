//! The compiled program and its on-disk shape.
//!
//! A program is `{ version, layout, consts, functions, mainIndex }`, the same
//! JSON object abulxt's shield runtime executes (its `ShieldProgram`, minus the
//! module descriptor estrogen does not emit yet). estrogen serializes it, then
//! wraps the JSON in its own ChaCha20 pool so the bytecode and the string
//! constants never ship in the clear. The runtime decodes the pool, JSON-parses
//! the program, and runs it. See [`super`] and docs/SHIELD-VM.md.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::layout::Layout;

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
    /// Declared parameter count. Zero for the main.
    pub num_params: usize,
}

impl FunctionDef {
    fn to_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert(
            "code".to_string(),
            Value::Array(self.code.iter().map(|&n| Value::from(n)).collect()),
        );
        obj.insert(
            "name".to_string(),
            self.name
                .as_ref()
                .map_or(Value::Null, |n| Value::from(n.clone())),
        );
        obj.insert("isAsync".to_string(), Value::from(self.is_async));
        obj.insert("isArrow".to_string(), Value::from(self.is_arrow));
        obj.insert("numParams".to_string(), Value::from(self.num_params as u64));
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
}

impl Program {
    /// The program as its JSON value, the exact shape the runtime consumes.
    pub fn into_json(self) -> Value {
        let mut obj = Map::new();
        obj.insert("version".to_string(), Value::from(1u32));
        obj.insert("layout".to_string(), self.layout.to_json());
        obj.insert(
            "consts".to_string(),
            Value::Array(self.consts.into_values()),
        );
        obj.insert(
            "functions".to_string(),
            Value::Array(self.functions.iter().map(FunctionDef::to_json).collect()),
        );
        obj.insert("mainIndex".to_string(), Value::from(self.main_index as u64));
        Value::Object(obj)
    }

    /// The program serialized to a compact JSON string.
    pub fn into_json_string(self) -> String {
        self.into_json().to_string()
    }
}
