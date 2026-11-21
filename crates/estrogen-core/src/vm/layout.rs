//! Per-build polymorphic opcode layout.
//!
//! Every opcode name gets a distinct byte in a 256-slot table, drawn from a
//! seed. The unused slots are trap slots the runtime fills with a wipe-and-throw
//! handler. This is a direct port of abulxt's shield `buildLayout`: the same
//! `mulberry32` PRNG, the same FNV-1a `hashSeed`, and the same Fisher-Yates
//! shuffle over [`OP_NAMES`](super::opcodes::OP_NAMES) in order, so a seed
//! produces the byte assignment the shield runtime would build for that seed.
//!
//! The byte assignment differs per build, so the same logical operation maps to
//! a different byte in every build. It is obfuscation, not secrecy: the layout
//! ships in the (encrypted) program and the runtime rebuilds its dispatch table
//! from it.

use serde_json::Value;

use super::opcodes::OP_NAMES;

const TABLE_SIZE: usize = 256;

/// A per-build opcode byte assignment.
pub struct Layout {
    /// Opcode name to its byte in the code stream.
    byte_of: std::collections::HashMap<&'static str, u8>,
    /// The hashed seed the runtime rebuilds the assignment from.
    seed_hash: u32,
}

impl Layout {
    /// Build the layout for `seed`. The seed is hashed, so any string works;
    /// estrogen passes a per-file string derived from its seed or key material.
    pub fn build(seed: &str) -> Layout {
        let seed_hash = hash_seed(seed);
        let mut rng = Mulberry32::new(seed_hash);
        let mut slots: Vec<usize> = (0..TABLE_SIZE).collect();
        // Fisher-Yates, matching the shield's loop bounds and index math.
        for i in (1..slots.len()).rev() {
            let j = (rng.next_f64() * (i as f64 + 1.0)).floor() as usize;
            slots.swap(i, j);
        }
        let mut byte_of = std::collections::HashMap::with_capacity(OP_NAMES.len());
        for (idx, name) in OP_NAMES.iter().enumerate() {
            byte_of.insert(*name, slots[idx] as u8);
        }
        Layout { byte_of, seed_hash }
    }

    /// The byte assigned to `name`. Panics on an unknown name, which is a
    /// compiler bug (every emitted name is in [`OP_NAMES`]).
    pub fn byte_of(&self, name: &str) -> u8 {
        *self
            .byte_of
            .get(name)
            .unwrap_or_else(|| panic!("estrogen vm: opcode {name} is not in the layout"))
    }

    /// The layout as the single number the runtime reads: the hashed seed. The
    /// runtime rebuilds the whole byte assignment from it with the same
    /// `mulberry32` and Fisher-Yates, and reads each opcode's arity from its own
    /// per-stable-index table, so the per-build `ops` map (one entry per opcode)
    /// never ships. No opcode name or index is serialized, so the runtime
    /// dispatches numerically and carries no opcode vocabulary.
    pub fn to_json(&self) -> Value {
        Value::from(self.seed_hash)
    }

    /// The byte assigned to each opcode, in `OP_NAMES` order. The runtime must
    /// rebuild this exact vector from [`to_json`]'s seed hash, so a test checks
    /// the JS port against it.
    #[cfg(test)]
    pub fn bytes_in_order(&self) -> Vec<u8> {
        OP_NAMES.iter().map(|name| self.byte_of(name)).collect()
    }
}

/// FNV-1a over the seed string, matching the shield's `hashSeed`. Works on the
/// UTF-16 code units of the string; estrogen seeds are ASCII, where UTF-16 code
/// units equal the bytes, so iterating chars as u32 matches.
fn hash_seed(seed: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for unit in seed.encode_utf16() {
        h ^= unit as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

/// The mulberry32 PRNG, a bit-for-bit port of the shield's generator. `Math.imul`
/// is a wrapping 32-bit multiply; `>>>` is a logical shift; `| 0` and `+ … | 0`
/// are 32-bit wrapping adds. Unsigned `u32` wrapping arithmetic reproduces every
/// one of those, so the stream matches the JS generator for the same seed.
struct Mulberry32 {
    a: u32,
}

impl Mulberry32 {
    fn new(seed: u32) -> Self {
        Mulberry32 { a: seed }
    }

    fn next_f64(&mut self) -> f64 {
        self.a = self.a.wrapping_add(0x6d2b79f5);
        let mut t = (self.a ^ (self.a >> 15)).wrapping_mul(1 | self.a);
        t = (t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t))) ^ t;
        ((t ^ (t >> 14)) as f64) / 4294967296.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_a_bijection_over_op_names() {
        let layout = Layout::build("seed-1");
        let mut bytes = std::collections::HashSet::new();
        for name in OP_NAMES {
            assert!(bytes.insert(layout.byte_of(name)), "byte reused for {name}");
        }
        assert_eq!(bytes.len(), OP_NAMES.len());
    }

    #[test]
    fn different_seeds_differ() {
        let a = Layout::build("seed-1");
        let b = Layout::build("seed-2");
        let differ = OP_NAMES.iter().any(|n| a.byte_of(n) != b.byte_of(n));
        assert!(differ, "layouts should differ across seeds");
    }

    #[test]
    fn same_seed_is_deterministic() {
        let a = Layout::build("fixed");
        let b = Layout::build("fixed");
        for name in OP_NAMES {
            assert_eq!(a.byte_of(name), b.byte_of(name));
        }
    }

    #[test]
    fn runtime_arity_string_matches_opcodes() {
        use super::super::opcodes::arity;
        let expected: String = OP_NAMES
            .iter()
            .map(|name| if arity(name) == 1 { '1' } else { '0' })
            .collect();
        let runtime = include_str!("runtime.js");
        let marker = runtime
            .find("const ARITY =")
            .expect("runtime.js defines ARITY");
        let after = &runtime[marker..];
        let open = after.find('\'').expect("ARITY opens a string");
        let body = &after[open + 1..];
        let close = body.find('\'').expect("ARITY closes a string");
        assert_eq!(
            &body[..close],
            expected,
            "runtime.js ARITY is out of sync with opcodes.rs; update the embedded \
             string so each opcode's operand count matches OP_NAMES order"
        );
    }

    #[test]
    fn hash_seed_matches_known_values() {
        // FNV-1a offset basis for the empty string.
        assert_eq!(hash_seed(""), 2166136261);
        // One ASCII char 'a' (0x61): (2166136261 ^ 0x61) * 16777619 mod 2^32.
        let expected = (2166136261u32 ^ 0x61u32).wrapping_mul(16777619);
        assert_eq!(hash_seed("a"), expected);
    }
}
