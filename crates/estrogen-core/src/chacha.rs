//! ChaCha20 stream cipher (RFC 8439), the build side of the pool.
//!
//! estrogen encrypts each pool entry here and ships a compact JS decoder that runs
//! the same block function (see [`crate::format::decoder_source`]). Keeping both
//! sides in this crate means the encryptor and the decoder are one algorithm,
//! and the symmetry test in [`crate::format`] proves they agree. The spec is
//! `obfuscate-build.mjs`: the IETF variant, a 96-bit nonce, a 32-bit block
//! counter that starts at 0, little-endian words throughout.
//!
//! The key ships in the file. This raises the cost of reading a build; it is not
//! secrecy. Nothing here is secret.
//!
//! ChaCha20 is a stream cipher, so encryption and decryption are the same XOR
//! against the keystream. [`chacha20_xor`] does both.

/// The four ChaCha20 constants ("expand 32-byte k"), as little-endian words.
const CONSTANTS: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

/// One quarter-round on the working state.
#[inline]
fn quarter_round(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// Read four little-endian bytes as a word.
#[inline]
fn load_le(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// The 64-byte keystream block for one counter value, as little-endian bytes.
fn keystream_block(key_words: &[u32; 8], nonce_words: &[u32; 3], counter: u32) -> [u8; 64] {
    let init: [u32; 16] = [
        CONSTANTS[0],
        CONSTANTS[1],
        CONSTANTS[2],
        CONSTANTS[3],
        key_words[0],
        key_words[1],
        key_words[2],
        key_words[3],
        key_words[4],
        key_words[5],
        key_words[6],
        key_words[7],
        counter,
        nonce_words[0],
        nonce_words[1],
        nonce_words[2],
    ];
    let mut s = init;
    for _ in 0..10 {
        // Column rounds.
        quarter_round(&mut s, 0, 4, 8, 12);
        quarter_round(&mut s, 1, 5, 9, 13);
        quarter_round(&mut s, 2, 6, 10, 14);
        quarter_round(&mut s, 3, 7, 11, 15);
        // Diagonal rounds.
        quarter_round(&mut s, 0, 5, 10, 15);
        quarter_round(&mut s, 1, 6, 11, 12);
        quarter_round(&mut s, 2, 7, 8, 13);
        quarter_round(&mut s, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for (word_index, word) in s.iter().enumerate() {
        let v = word.wrapping_add(init[word_index]);
        out[word_index * 4..word_index * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// Read a 32-byte key into eight little-endian words.
fn key_words(key: &[u8; 32]) -> [u32; 8] {
    std::array::from_fn(|i| load_le(key, i * 4))
}

/// Read a 12-byte nonce into three little-endian words.
fn nonce_words(nonce: &[u8; 12]) -> [u32; 3] {
    std::array::from_fn(|i| load_le(nonce, i * 4))
}

/// Encrypt or decrypt `data` under `key` and `nonce`. The block counter starts
/// at 0 and steps once per 64 bytes, matching the shipped decoder.
///
/// ChaCha20 is symmetric: calling this on ciphertext with the same key and nonce
/// returns the plaintext.
pub fn chacha20_xor(key: &[u8; 32], nonce: &[u8; 12], data: &[u8]) -> Vec<u8> {
    let kw = key_words(key);
    let nw = nonce_words(nonce);

    let mut out = data.to_vec();
    for (block_index, chunk) in out.chunks_mut(64).enumerate() {
        let ks = keystream_block(&kw, &nw, block_index as u32);
        for (byte, k) in chunk.iter_mut().zip(ks.iter()) {
            *byte ^= *k;
        }
    }
    out
}

/// A ChaCha20 keystream read as a byte source. estrogen uses it as the per-file
/// CSPRNG behind the key, the nonces, and the decoder name: seed it once per
/// file (from entropy, or deterministically from `--seed`) and draw bytes.
///
/// The nonce is fixed at zero; the block counter steps from 0, so the stream is
/// the key's keystream. One seed gives one reproducible stream.
pub struct Keystream {
    key_words: [u32; 8],
    counter: u32,
    block: [u8; 64],
    pos: usize,
}

impl Keystream {
    /// Start a keystream from a 32-byte seed.
    pub fn new(seed: &[u8; 32]) -> Self {
        Keystream {
            key_words: key_words(seed),
            counter: 0,
            block: [0u8; 64],
            pos: 64,
        }
    }

    /// The next keystream byte.
    pub fn next_byte(&mut self) -> u8 {
        if self.pos == 64 {
            self.block = keystream_block(&self.key_words, &[0, 0, 0], self.counter);
            self.counter = self.counter.wrapping_add(1);
            self.pos = 0;
        }
        let b = self.block[self.pos];
        self.pos += 1;
        b
    }

    /// Fill `buf` with the next bytes of the stream.
    pub fn fill(&mut self, buf: &mut [u8]) {
        for slot in buf.iter_mut() {
            *slot = self.next_byte();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_then_decrypt_round_trips() {
        let key = [7u8; 32];
        let nonce = [3u8; 12];
        let plain = b"the quick brown fox jumps over the lazy dog, twice over for 65+";
        let ct = chacha20_xor(&key, &nonce, plain);
        assert_ne!(&ct[..], &plain[..]);
        let back = chacha20_xor(&key, &nonce, &ct);
        assert_eq!(&back[..], &plain[..]);
    }

    #[test]
    fn keystream_matches_xor_over_zeros_and_is_deterministic() {
        let seed = [9u8; 32];
        let mut ks = Keystream::new(&seed);
        let mut got = [0u8; 200];
        ks.fill(&mut got);
        // The keystream is the cipher run over zero bytes at nonce 0.
        let want = chacha20_xor(&seed, &[0u8; 12], &[0u8; 200]);
        assert_eq!(&got[..], &want[..]);
        // A second stream from the same seed is identical (what --seed needs).
        let mut ks2 = Keystream::new(&seed);
        let mut again = [0u8; 200];
        ks2.fill(&mut again);
        assert_eq!(got, again);
    }

    #[test]
    fn empty_input_is_empty_output() {
        let ct = chacha20_xor(&[0u8; 32], &[0u8; 12], b"");
        assert!(ct.is_empty());
    }

    #[test]
    fn rfc8439_keystream_first_block() {
        // RFC 8439 Appendix A.1, Test Vector 1: all-zero key, all-zero nonce,
        // block counter 0. XOR over zero bytes yields the keystream directly, so
        // the first 64 bytes are the published block.
        let stream = chacha20_xor(&[0u8; 32], &[0u8; 12], &[0u8; 64]);
        let expected: [u8; 64] = [
            0x76, 0xb8, 0xe0, 0xad, 0xa0, 0xf1, 0x3d, 0x90, 0x40, 0x5d, 0x6a, 0xe5, 0x53, 0x86,
            0xbd, 0x28, 0xbd, 0xd2, 0x19, 0xb8, 0xa0, 0x8d, 0xed, 0x1a, 0xa8, 0x36, 0xef, 0xcc,
            0x8b, 0x77, 0x0d, 0xc7, 0xda, 0x41, 0x59, 0x7c, 0x51, 0x57, 0x48, 0x8d, 0x77, 0x24,
            0xe0, 0x3f, 0xb8, 0xd8, 0x4a, 0x37, 0x6a, 0x43, 0xb8, 0xf4, 0x15, 0x18, 0xa1, 0x1c,
            0xc3, 0x87, 0xb6, 0x69, 0xb2, 0xee, 0x65, 0x86,
        ];
        assert_eq!(&stream[..], &expected[..]);
    }
}
