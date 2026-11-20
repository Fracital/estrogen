//! Wasm name ranges and base64, the byte half of the proxy transform.
//!
//! The proxy transports embed a wasm core module whose import, export and name
//! strings carry the proxy names, sometimes as a base64 literal. To keep those
//! names out of the shipped bytes, estrogen renames them in place with a
//! same-length substitution, but only inside the bytes that hold NAMES or DATA:
//! import and export names, data segment contents, and the custom `name`
//! section entry by entry. Code, types, LEB128 lengths and constant expressions
//! are never touched, so the module's structure and instructions stay byte for
//! byte the input's and the result is valid wherever the input is. A name
//! spelled by chance in an instruction stream or a length is not a name and is
//! left alone.
//!
//! Spec: `obfuscate-build.mjs` (`wasmNameRanges`, `mapWasmBytes`,
//! `mapWasmBase64`). The same-length substitution itself is the names pass
//! (milestone 3); this module takes it as a parameter so the byte and base64
//! handling can be built and tested on its own. The substitution must return a
//! replacement of the same length, or mapping fails closed.
//!
//! This raises the cost of reading a build; it is not secrecy. Nothing here is
//! secret.

// The names pass (milestone 3) is what calls these: it supplies the same-length
// proxy substitution and runs the mapping over the proxy transports' wasm. Until
// that pass lands, the module is reached only by its tests, so the non-test
// build sees it as unused. Remove this when the names pass wires it in.

use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use base64::Engine;

use crate::error::{Error, Result};

/// Build a wasm walk error with the byte offset where the walk stopped.
fn wasm_err(what: &str, at: usize) -> Error {
    Error::Wasm {
        message: format!("{what} at byte {at}"),
    }
}

/// A fail-closed reader over the module bytes. Every read checks the bound, so a
/// truncated module stops the walk with an error instead of a panic.
struct Cursor<'a> {
    buf: &'a [u8],
    p: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Cursor { buf, p: 0 }
    }

    /// Read one byte and advance.
    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .buf
            .get(self.p)
            .ok_or_else(|| wasm_err("truncated module", self.p))?;
        self.p += 1;
        Ok(b)
    }

    /// Skip a LEB128 of either sign; only its length matters here.
    fn skip_leb(&mut self) -> Result<()> {
        for _ in 0..10 {
            if self.byte()? & 0x80 == 0 {
                return Ok(());
            }
        }
        Err(wasm_err("overlong LEB128", self.p))
    }

    /// Read an unsigned LEB128 as a length or count.
    fn uint(&mut self) -> Result<usize> {
        let mut r: u64 = 0;
        let mut shift = 0;
        while shift < 35 {
            let b = self.byte()?;
            r += u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(r as usize);
            }
            shift += 7;
        }
        Err(wasm_err("overlong u32", self.p))
    }

    /// Read a length-prefixed string, pushing its byte range when non-empty.
    fn string(&mut self, ranges: &mut Vec<(usize, usize)>) -> Result<()> {
        let len = self.uint()?;
        if self.p + len > self.buf.len() {
            return Err(wasm_err("string past the end", self.p));
        }
        if len > 0 {
            ranges.push((self.p, self.p + len));
        }
        self.p += len;
        Ok(())
    }

    /// Read a length-prefixed byte run, returning its range.
    fn byte_run(&mut self) -> Result<(usize, usize)> {
        let len = self.uint()?;
        if self.p + len > self.buf.len() {
            return Err(wasm_err("bytes past the end", self.p));
        }
        let a = self.p;
        self.p += len;
        Ok((a, self.p))
    }

    fn valtype(&mut self) -> Result<()> {
        let t = self.byte()?;
        if t == 0x63 || t == 0x64 {
            self.skip_leb()?; // (ref null? <heaptype>)
        }
        Ok(())
    }

    fn limits(&mut self) -> Result<()> {
        let flags = self.byte()?;
        if flags > 0x0f {
            return Err(wasm_err(&format!("limits flags 0x{flags:x}"), self.p));
        }
        self.skip_leb()?;
        if flags & 1 != 0 {
            self.skip_leb()?;
        }
        if flags & 8 != 0 {
            self.skip_leb()?; // custom page size
        }
        Ok(())
    }

    fn const_expr(&mut self) -> Result<()> {
        loop {
            let op = self.byte()?;
            if op == 0x0b {
                return Ok(());
            }
            if matches!(op, 0x41 | 0x42 | 0x23 | 0xd0 | 0xd2) {
                self.skip_leb()?;
            } else if !matches!(op, 0x6a | 0x6b | 0x6c | 0x7c | 0x7d | 0x7e) {
                return Err(wasm_err(
                    &format!("constant expression opcode 0x{op:x}"),
                    self.p,
                ));
            }
        }
    }

    /// A name map: pairs of (index, string).
    fn name_map(&mut self, ranges: &mut Vec<(usize, usize)>) -> Result<()> {
        let mut k = self.uint()?;
        while k > 0 {
            self.skip_leb()?;
            self.string(ranges)?;
            k -= 1;
        }
        Ok(())
    }
}

/// The byte ranges of a wasm core module that hold names or data: import and
/// export names, data segment contents, and the custom `name` section entry by
/// entry. Returns the ranges in increasing, non-overlapping order (the walk is
/// linear). Fails closed on a module it cannot walk.
pub fn wasm_name_ranges(buf: &[u8]) -> Result<Vec<(usize, usize)>> {
    if buf.len() < 8
        || u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) != 0x6d73_6100
        || u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) != 1
    {
        return Err(Error::Wasm {
            message: "not a wasm core module (version 1)".to_string(),
        });
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut c = Cursor::new(buf);
    c.p = 8;

    while c.p < buf.len() {
        let id = c.byte()?;
        let size = c.uint()?;
        let end = c.p + size;
        if end > buf.len() {
            return Err(wasm_err("section past the end", c.p));
        }
        match id {
            0 => {
                // Custom section: its name, then (for "name") its subsections.
                let len = c.uint()?;
                if c.p + len > buf.len() {
                    return Err(wasm_err("custom section name past the end", c.p));
                }
                let sec_name = latin1(&buf[c.p..c.p + len]);
                if len > 0 {
                    ranges.push((c.p, c.p + len));
                }
                c.p += len;
                if sec_name == "name" {
                    while c.p < end {
                        let sub = c.byte()?;
                        let sub_size = c.uint()?;
                        let sub_end = c.p + sub_size;
                        if sub_end > end {
                            return Err(wasm_err("name subsection past the end", c.p));
                        }
                        if sub == 0 {
                            c.string(&mut ranges)?;
                        } else if matches!(sub, 1 | 4 | 5 | 6 | 7 | 8 | 9 | 11) {
                            c.name_map(&mut ranges)?;
                        } else if sub == 2 || sub == 3 || sub == 10 {
                            let mut k = c.uint()?;
                            while k > 0 {
                                c.skip_leb()?;
                                c.name_map(&mut ranges)?;
                                k -= 1;
                            }
                        } else {
                            c.p = sub_end; // unknown subsection: left as it is
                        }
                        if c.p != sub_end {
                            return Err(wasm_err(
                                &format!("name subsection {sub} length mismatch"),
                                c.p,
                            ));
                        }
                    }
                } else {
                    // Other custom sections are opaque: take the content as one range.
                    if end > c.p {
                        ranges.push((c.p, end));
                    }
                    c.p = end;
                }
            }
            2 => {
                // Imports: module name, field name, then the kind-specific tail.
                let mut k = c.uint()?;
                while k > 0 {
                    c.string(&mut ranges)?;
                    c.string(&mut ranges)?;
                    let kind = c.byte()?;
                    match kind {
                        0x00 => c.skip_leb()?,
                        0x01 => {
                            c.valtype()?;
                            c.limits()?;
                        }
                        0x02 => c.limits()?,
                        0x03 => {
                            c.valtype()?;
                            c.byte()?; // mutability
                        }
                        0x04 => {
                            c.byte()?; // tag attribute
                            c.skip_leb()?;
                        }
                        other => return Err(wasm_err(&format!("import kind 0x{other:x}"), c.p)),
                    }
                    k -= 1;
                }
            }
            7 => {
                // Exports: name, kind byte, index.
                let mut k = c.uint()?;
                while k > 0 {
                    c.string(&mut ranges)?;
                    c.byte()?; // kind
                    c.skip_leb()?;
                    k -= 1;
                }
            }
            11 => {
                // Data segments: the content byte run is data.
                let mut k = c.uint()?;
                while k > 0 {
                    let flags = c.uint()?;
                    if flags == 0 {
                        c.const_expr()?;
                    } else if flags == 2 {
                        c.skip_leb()?;
                        c.const_expr()?;
                    } else if flags != 1 {
                        return Err(wasm_err(&format!("data segment flags {flags}"), c.p));
                    }
                    let (a, b) = c.byte_run()?;
                    if b > a {
                        ranges.push((a, b));
                    }
                    k -= 1;
                }
            }
            _ => {
                c.p = end; // code, types and the rest: never mapped
            }
        }
        if c.p != end {
            return Err(wasm_err(&format!("section {id} length mismatch"), c.p));
        }
    }
    Ok(ranges)
}

/// Map a wasm module's name and data bytes with `subst`, leaving every other
/// byte identical, and return the mapped bytes (or the input unchanged when
/// nothing matched). `subst` is applied to each name or data range as a latin1
/// string and must return a replacement of the same length, or mapping fails
/// closed. Same length keeps section sizes and offsets valid, so the module
/// stays structurally the input's.
pub fn map_wasm_bytes(buf: &[u8], subst: &impl Fn(&str) -> String) -> Result<Vec<u8>> {
    let ranges = wasm_name_ranges(buf)?;
    let mut out: Option<Vec<u8>> = None;
    for (a, b) in ranges {
        let s = latin1(&buf[a..b]);
        let mapped = subst(&s);
        if mapped == s {
            continue;
        }
        if mapped.chars().count() != b - a {
            return Err(Error::Wasm {
                message: "name mapping changed a wasm string's length".to_string(),
            });
        }
        let out = out.get_or_insert_with(|| buf.to_vec());
        for (i, ch) in mapped.chars().enumerate() {
            let code = ch as u32;
            if code > 0xff {
                return Err(Error::Wasm {
                    message: "name mapping produced a non-latin1 byte".to_string(),
                });
            }
            out[a + i] = code as u8;
        }
    }
    Ok(out.unwrap_or_else(|| buf.to_vec()))
}

/// Which base64 alphabet a literal uses, decided by the characters it shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Alphabet {
    /// Shows `+` or `/`.
    Standard,
    /// Shows `-` or `_`.
    UrlSafe,
    /// Shows neither, so the alphabet cannot be told from the text.
    Unmarked,
}

/// Map a wasm module carried as a base64 literal, writing it back in the
/// literal's own alphabet and padding. Returns `None` when the literal is not a
/// wasm module or mapping changed nothing. A literal that mixes the two
/// alphabets, is not canonical, or whose alphabet cannot be told while the
/// mapped bytes need one, is refused.
pub fn map_wasm_base64(value: &str, subst: &impl Fn(&str) -> String) -> Result<Option<String>> {
    let standard = value.contains(['+', '/']);
    let url_safe = value.contains(['-', '_']);
    if standard && url_safe {
        return Err(Error::Wasm {
            message: "base64 literal mixes the standard and URL-safe alphabets".to_string(),
        });
    }
    let alphabet = if standard {
        Alphabet::Standard
    } else if url_safe {
        Alphabet::UrlSafe
    } else {
        Alphabet::Unmarked
    };

    let buf = match decode(value, alphabet) {
        Some(b) => b,
        None => return Ok(None),
    };
    if buf.len() < 4 || &buf[..4] != b"\0asm" {
        return Ok(None);
    }
    let padded = value.ends_with('=');
    if encode(&buf, alphabet, padded) != value {
        return Err(Error::Wasm {
            message: "base64 literal is not canonical; cannot map it safely".to_string(),
        });
    }

    let out = map_wasm_bytes(&buf, subst)?;
    if out == buf {
        return Ok(None);
    }
    let mapped = encode(&out, alphabet, padded);
    if alphabet == Alphabet::Unmarked && mapped.contains(['-', '_', '+', '/']) {
        return Err(Error::Wasm {
            message: "base64 literal does not show its alphabet, and the mapped module needs one"
                .to_string(),
        });
    }
    Ok(Some(mapped))
}

/// Decode a base64 string in the given alphabet, padding optional. Returns
/// `None` when the text is not valid base64.
fn decode(value: &str, alphabet: Alphabet) -> Option<Vec<u8>> {
    let cfg = GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    let engine = match alphabet {
        Alphabet::UrlSafe => GeneralPurpose::new(&alphabet::URL_SAFE, cfg),
        // An unmarked literal reads as standard, matching the reference.
        Alphabet::Standard | Alphabet::Unmarked => GeneralPurpose::new(&alphabet::STANDARD, cfg),
    };
    engine.decode(value).ok()
}

/// Encode bytes as base64 in the given alphabet and padding, the same form the
/// reference writes back: encode as standard, map to URL-safe characters when
/// that alphabet is in use, and drop the padding when the input had none.
fn encode(bytes: &[u8], alphabet: Alphabet, padded: bool) -> String {
    let mut text = base64::engine::general_purpose::STANDARD.encode(bytes);
    if alphabet == Alphabet::UrlSafe {
        text = text.replace('+', "-").replace('/', "_");
    }
    if !padded {
        while text.ends_with('=') {
            text.pop();
        }
    }
    text
}

/// A byte slice read as latin1: each byte becomes the code point of its value,
/// so the string has one character per byte and round-trips back to the same
/// bytes for any run of name or data bytes.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test stand-in for the names pass: rename the proxy tokens by a
    /// same-length, case-preserving letter run, like `mapProxyText` does.
    fn stub_subst(s: &str) -> String {
        // Longest first so "scramjet" is not shadowed by a shorter token.
        const TOKENS: &[&str] = &["scramjet", "stomp", "epoxy"];
        let mut out = s.to_string();
        for token in TOKENS {
            out = replace_same_length(&out, token);
        }
        out
    }

    /// Replace every case-insensitive occurrence of `token` with a run of the
    /// same length, keeping each position's case. The replacement letter is
    /// fixed per token so tests are deterministic. Works on characters, so a
    /// latin1 string with bytes above 0x7f (the `ÿ` of a 0xff data byte) passes
    /// through untouched.
    fn replace_same_length(s: &str, token: &str) -> String {
        let fill = token.as_bytes()[0] as char; // tokens are ascii
        let chars: Vec<char> = s.chars().collect();
        let tok: Vec<char> = token.chars().collect();
        let mut out = String::with_capacity(s.len());
        let mut i = 0;
        while i < chars.len() {
            if i + tok.len() <= chars.len()
                && (0..tok.len()).all(|j| chars[i + j].eq_ignore_ascii_case(&tok[j]))
            {
                for ch in chars.iter().skip(i).take(tok.len()) {
                    out.push(if ch.is_ascii_uppercase() {
                        fill.to_ascii_uppercase()
                    } else {
                        fill.to_ascii_lowercase()
                    });
                }
                i += tok.len();
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        out
    }

    /// `sec(id, body)` = one wasm section: id, LEB length (bodies here are short,
    /// so one byte), then the body.
    fn sec(id: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![id, body.len() as u8];
        v.extend_from_slice(body);
        v
    }

    fn wasm_header() -> Vec<u8> {
        vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
    }

    /// A module exporting one no-op function under `export_name`.
    fn module_exporting(export_name: &str) -> Vec<u8> {
        let name = export_name.as_bytes();
        let mut exp = vec![0x01, name.len() as u8];
        exp.extend_from_slice(name);
        exp.extend_from_slice(&[0x00, 0x00]);
        let mut wasm = wasm_header();
        wasm.extend(sec(0x01, &[0x01, 0x60, 0x00, 0x00])); // type: () -> ()
        wasm.extend(sec(0x03, &[0x01, 0x00])); // function
        wasm.extend(sec(0x07, &exp)); // export
        wasm.extend(sec(0x0a, &[0x01, 0x02, 0x00, 0x0b])); // code: one empty body
        wasm
    }

    #[test]
    fn export_name_is_a_range_and_is_mapped() {
        let wasm = module_exporting("scramjet_add");
        let ranges = wasm_name_ranges(&wasm).unwrap();
        // Exactly one name range, the export name.
        let name_ranges: Vec<_> = ranges
            .iter()
            .filter(|(a, b)| latin1(&wasm[*a..*b]) == "scramjet_add")
            .collect();
        assert_eq!(name_ranges.len(), 1);

        let out = map_wasm_bytes(&wasm, &stub_subst).unwrap();
        assert_eq!(out.len(), wasm.len());
        let mapped_name = stub_subst("scramjet_add");
        assert_ne!(mapped_name, "scramjet_add");
        assert_eq!(mapped_name.len(), "scramjet_add".len());
        assert!(contains_bytes(&out, mapped_name.as_bytes()));
        assert!(!contains_bytes(&out, b"scramjet_add"));
    }

    #[test]
    fn code_bytes_that_spell_a_name_are_left_alone() {
        // The code section holds bytes that spell "stomp" (as opcodes), while a
        // data section and the export name genuinely carry the name. Only the
        // genuine ones are renamed.
        let str_lp = |s: &str| {
            let mut v = vec![s.len() as u8];
            v.extend_from_slice(s.as_bytes());
            v
        };
        let opcodes = b"stomp";
        let mut code_inner = vec![0x41, 60, 0x41, 50, 0x41, 40, 0x41, 3, 0x41, 1, 0x41, 2];
        code_inner.extend_from_slice(opcodes);
        let mut body = vec![0x00];
        body.extend_from_slice(&code_inner);
        body.push(0x0b);

        let mut wasm = wasm_header();
        wasm.extend(sec(0x01, &[0x01, 0x60, 0x00, 0x01, 0x7f]));
        wasm.extend(sec(0x03, &[0x01, 0x00]));
        wasm.extend(sec(0x05, &[0x01, 0x00, 0x01]));
        let mut exp = vec![0x01];
        exp.extend(str_lp("stomp_value"));
        exp.extend_from_slice(&[0x00, 0x00]);
        wasm.extend(sec(0x07, &exp));
        let mut code_sec = vec![0x01, body.len() as u8];
        code_sec.extend_from_slice(&body);
        wasm.extend(sec(0x0a, &code_sec));
        let mut data_body = vec![0x01, 0x00, 0x41, 0x00, 0x0b];
        data_body.extend(str_lp("initCustomPrototype"));
        wasm.extend(sec(0x0b, &data_body));

        let data_subst = |s: &str| {
            // Rename stomp and the data string, same length.
            let s = replace_same_length(s, "stomp");
            replace_same_length(&s, "initCustomPrototype")
        };
        let out = map_wasm_bytes(&wasm, &data_subst).unwrap();
        assert_eq!(out.len(), wasm.len());

        // The code bytes are untouched.
        let at = find_bytes(&wasm, &code_inner).unwrap();
        assert_eq!(&out[at..at + code_inner.len()], &code_inner[..]);

        // The export name and the data string are renamed.
        assert!(!contains_bytes(&out, b"stomp_value"));
        let mapped_data = data_subst("initCustomPrototype");
        assert_ne!(mapped_data, "initCustomPrototype");
        assert!(contains_bytes(&out, mapped_data.as_bytes()));
        // Only the code opcodes still spell "stomp".
        let lower = out.to_ascii_lowercase();
        assert_eq!(count_occurrences(&lower, b"stomp"), 1);
    }

    #[test]
    fn base64_url_safe_round_trips_in_its_own_alphabet() {
        // A module plus a custom section of 0xff bytes, so the URL-safe text is
        // sure to hold `_`.
        let mut wasm = module_exporting("stomp_add");
        let mut custom_body = vec![0x01, 0x78];
        custom_body.extend_from_slice(&[0xff; 7]);
        wasm.extend(sec(0x00, &custom_body));

        let url = encode(&wasm, Alphabet::UrlSafe, false);
        assert!(url.contains('_'));
        assert!(!url.contains(['+', '/', '=']));

        let mapped = map_wasm_base64(&url, &stub_subst).unwrap().unwrap();
        assert_eq!(mapped.len(), url.len(), "same length, no padding added");
        assert!(!mapped.contains(['+', '/', '=']), "still URL-safe");
        let decoded = decode(&mapped, Alphabet::UrlSafe).unwrap();
        assert!(contains_bytes(&decoded, stub_subst("stomp_add").as_bytes()));

        // The standard alphabet, padded, keeps its form too.
        let std = encode(&wasm, Alphabet::Standard, true);
        let std_mapped = map_wasm_base64(&std, &stub_subst).unwrap().unwrap();
        assert_eq!(std_mapped.len(), std.len());
        assert!(!std_mapped.contains(['-', '_']));
        let std_decoded = decode(&std_mapped, Alphabet::Standard).unwrap();
        assert!(contains_bytes(
            &std_decoded,
            stub_subst("stomp_add").as_bytes()
        ));
    }

    #[test]
    fn mixed_alphabet_literal_is_refused() {
        let wasm = module_exporting("stomp_add");
        let url = encode(&wasm, Alphabet::UrlSafe, false);
        // Splice a `+` into a URL-safe string so it shows both alphabets.
        let mixed = format!("{}+{}", &url[..20], &url[21..]);
        let err = map_wasm_base64(&mixed, &stub_subst).unwrap_err();
        assert!(matches!(err, Error::Wasm { .. }));
    }

    #[test]
    fn non_wasm_base64_returns_none() {
        let not_wasm = base64::engine::general_purpose::STANDARD.encode(b"just some text here");
        assert_eq!(map_wasm_base64(&not_wasm, &stub_subst).unwrap(), None);
    }

    #[test]
    fn unchanged_module_returns_the_input() {
        let wasm = module_exporting("plain_export");
        let out = map_wasm_bytes(&wasm, &stub_subst).unwrap();
        assert_eq!(out, wasm);
    }

    #[test]
    fn a_truncated_module_fails_closed() {
        let mut wasm = module_exporting("stomp_add");
        wasm.truncate(wasm.len() - 2); // cut into the last section
        let err = wasm_name_ranges(&wasm).unwrap_err();
        assert!(matches!(err, Error::Wasm { .. }));
    }

    #[test]
    fn not_a_module_fails_closed() {
        assert!(wasm_name_ranges(b"not wasm").is_err());
        assert!(wasm_name_ranges(&[0x00, 0x61, 0x73, 0x6d, 0x09, 0, 0, 0]).is_err());
    }

    #[test]
    fn ranges_match_the_reference_walker() {
        // Cross-check the walk against the reference `wasmNameRanges` on the same
        // bytes. The ranges do not depend on any key, so they must be identical.
        // Gated: skips cleanly if node or the reference script is absent.
        let reference = format!(
            "{}/src/Fracital/scripts/obfuscate-build.mjs",
            std::env::var("HOME").unwrap_or_default()
        );
        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("SKIP ranges_match_the_reference_walker: node not found");
            return;
        }
        if !std::path::Path::new(&reference).exists() {
            eprintln!("SKIP ranges_match_the_reference_walker: reference script not found");
            return;
        }

        // A module with an export name, a data segment, and a custom section, so
        // several range kinds are exercised at once.
        let mut wasm = module_exporting("stomp_add");
        let mut data_body = vec![0x01, 0x00, 0x41, 0x00, 0x0b];
        data_body.push(b"epoxy_data".len() as u8);
        data_body.extend_from_slice(b"epoxy_data");
        wasm.extend(sec(0x0b, &data_body));
        let mut custom_body = vec![0x04];
        custom_body.extend_from_slice(b"metscramjet");
        wasm.extend(sec(0x00, &custom_body));

        let mine = wasm_name_ranges(&wasm).unwrap();

        let dir = std::env::temp_dir();
        let path = dir.join(format!("estrogen-wasm-xcheck-{}.wasm", std::process::id()));
        std::fs::write(&path, &wasm).unwrap();
        let script = format!(
            "const fs=require('node:fs');import('file://{reference}').then(m=>{{\
             const b=fs.readFileSync(process.argv[1]);\
             process.stdout.write(JSON.stringify(m.wasmNameRanges(b)));}});"
        );
        let out = std::process::Command::new("node")
            .arg("-e")
            .arg(&script)
            .arg(&path)
            .output()
            .expect("run node");
        std::fs::remove_file(&path).ok();
        assert!(
            out.status.success(),
            "node cross-check failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let reference_ranges: Vec<(usize, usize)> =
            serde_json::from_slice(&out.stdout).expect("parse reference ranges");
        assert_eq!(mine, reference_ranges, "ranges diverged from the reference");
    }

    // Test helpers.
    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        find_bytes(haystack, needle).is_some()
    }
    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }
    fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
        if needle.is_empty() {
            return 0;
        }
        let mut n = 0;
        let mut i = 0;
        while i + needle.len() <= haystack.len() {
            if &haystack[i..i + needle.len()] == needle {
                n += 1;
                i += needle.len();
            } else {
                i += 1;
            }
        }
        n
    }
}
