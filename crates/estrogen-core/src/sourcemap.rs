//! Source map V3 emission.
//!
//! When `--sourcemap <dir>` is set, estrogen writes a standards-valid V3 source
//! map per processed file into that directory, which is meant to sit outside the
//! served tree. The map traces the obfuscated output back to the chunk estrogen
//! read. It is built from the pass edit list, the same magic-string model the
//! design in `docs/HANDOFF.md` describes: an unedited run maps one to one, and an
//! edit maps its replacement back to the byte range it replaced.
//!
//! The served build never gains a map or a `sourceMappingURL`. estrogen strips a
//! bundler `sourceMappingURL` from the chunks it processes and does not add one
//! pointing at the emitted map. The map is for private debugging; a map that
//! reaches a served dir by accident is still never served, because the edge
//! returns 403 for `.map`.
//!
//! The columns a V3 map records are UTF-16 code units, which is what a browser
//! debugger counts, so the walk advances columns by `char::len_utf16`.
//!
//! This raises the cost of reading a build; it is not secrecy. A map that traces
//! the output back to source is exactly what the obfuscation withholds from the
//! served build, which is why it is off by default and kept out of the tree.

use crate::edit::Edit;

/// The base64 alphabet for VLQ digits, in source-map order.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// A position in a text: a 0-based line and a 0-based UTF-16 column.
#[derive(Clone, Copy, Default)]
struct Pos {
    line: u32,
    col: u32,
}

/// One mapping token: a generated position and the source position it traces to.
/// The source index is always 0, since a map covers one file.
type Token = (u32, u32, u32, u32); // gen_line, gen_col, src_line, src_col

/// Build the V3 source map JSON for one file.
///
/// `source_name` is what the map lists in `sources` (the input chunk's path).
/// `source` is the original text, embedded in `sourcesContent` so the map is
/// self-contained even though estrogen rewrote the chunk in place. `edits` are
/// the pass edits in [`crate::edit::EditSet::sorted`] order, the same order the
/// output was assembled in.
pub fn build_v3(source_name: &str, source: &str, edits: &[&Edit]) -> String {
    let mut tokens: Vec<Token> = Vec::new();
    let mut gen = Pos::default();
    let mut src = Pos::default();
    let mut src_byte = 0usize;

    for edit in edits {
        // The unedited run copied verbatim before this edit: generated and
        // source advance together and map one to one.
        walk_unedited(
            &source[src_byte..edit.start],
            &mut gen,
            &mut src,
            &mut tokens,
        );
        // The replacement: generated advances, source stays at the edit start,
        // so the whole replacement traces back to the range it replaced.
        let fixed = src;
        walk_replacement(&edit.replacement, &mut gen, fixed, &mut tokens);
        // Step the source cursor over the replaced bytes, without generated
        // output, so the next unedited run lines up.
        advance(&source[edit.start..edit.end], &mut src);
        src_byte = edit.end;
    }
    walk_unedited(&source[src_byte..], &mut gen, &mut src, &mut tokens);

    // One token per generated position wins; a later token at the same spot (a
    // zero-width run meeting an edit) replaces an earlier one.
    tokens.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    tokens.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);

    let mappings = encode_mappings(&tokens);
    let doc = serde_json::json!({
        "version": 3,
        "sources": [source_name],
        "sourcesContent": [source],
        "names": [],
        "mappings": mappings,
    });
    doc.to_string()
}

/// Walk an unedited run: generated and source move together. Emit a token at the
/// run start and at each line start inside it, since the text is identical in
/// both.
fn walk_unedited(text: &str, gen: &mut Pos, src: &mut Pos, tokens: &mut Vec<Token>) {
    tokens.push((gen.line, gen.col, src.line, src.col));
    for ch in text.chars() {
        if ch == '\n' {
            gen.line += 1;
            gen.col = 0;
            src.line += 1;
            src.col = 0;
            tokens.push((gen.line, gen.col, src.line, src.col));
        } else {
            let w = ch.len_utf16() as u32;
            gen.col += w;
            src.col += w;
        }
    }
}

/// Walk a replacement: generated advances, the source position is fixed at the
/// edit start. Emit a token at the start and at each line start, all tracing to
/// the fixed source position.
fn walk_replacement(text: &str, gen: &mut Pos, fixed: Pos, tokens: &mut Vec<Token>) {
    tokens.push((gen.line, gen.col, fixed.line, fixed.col));
    for ch in text.chars() {
        if ch == '\n' {
            gen.line += 1;
            gen.col = 0;
            tokens.push((gen.line, gen.col, fixed.line, fixed.col));
        } else {
            gen.col += ch.len_utf16() as u32;
        }
    }
}

/// Advance a position over `text` without emitting anything, to keep the source
/// cursor aligned past a replaced range.
fn advance(text: &str, pos: &mut Pos) {
    for ch in text.chars() {
        if ch == '\n' {
            pos.line += 1;
            pos.col = 0;
        } else {
            pos.col += ch.len_utf16() as u32;
        }
    }
}

/// Encode sorted tokens into the V3 `mappings` string: lines joined by `;`,
/// segments on a line by `,`, each field a VLQ delta from the previous.
fn encode_mappings(tokens: &[Token]) -> String {
    let mut out = String::new();
    let mut cur_line = 0u32;
    let mut prev_gen_col = 0i64;
    let mut prev_src_line = 0i64;
    let mut prev_src_col = 0i64;
    let mut first_in_line = true;

    for &(gl, gc, sl, sc) in tokens {
        while cur_line < gl {
            out.push(';');
            cur_line += 1;
            prev_gen_col = 0;
            first_in_line = true;
        }
        if !first_in_line {
            out.push(',');
        }
        vlq(&mut out, gc as i64 - prev_gen_col);
        vlq(&mut out, 0); // source index, always 0 (one source), so the delta is 0
        vlq(&mut out, sl as i64 - prev_src_line);
        vlq(&mut out, sc as i64 - prev_src_col);
        prev_gen_col = gc as i64;
        prev_src_line = sl as i64;
        prev_src_col = sc as i64;
        first_in_line = false;
    }
    out
}

/// Append one base64 VLQ value.
fn vlq(out: &mut String, value: i64) {
    let mut v = if value < 0 {
        ((-value) as u64) << 1 | 1
    } else {
        (value as u64) << 1
    };
    loop {
        let mut digit = (v & 0b1_1111) as usize;
        v >>= 5;
        if v > 0 {
            digit |= 0b10_0000;
        }
        out.push(BASE64[digit] as char);
        if v == 0 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_vlq(s: &[u8], i: &mut usize) -> i64 {
        let mut result: i64 = 0;
        let mut shift = 0;
        loop {
            let c = s[*i];
            *i += 1;
            let digit = BASE64.iter().position(|&b| b == c).unwrap() as i64;
            result |= (digit & 0b1_1111) << shift;
            if digit & 0b10_0000 == 0 {
                break;
            }
            shift += 5;
        }
        if result & 1 == 1 {
            -(result >> 1)
        } else {
            result >> 1
        }
    }

    /// Decode a mappings string back to (gen_line, gen_col, src_line, src_col).
    fn decode(mappings: &str) -> Vec<(i64, i64, i64, i64)> {
        let mut out = Vec::new();
        let (mut sidx, mut sl, mut sc) = (0i64, 0i64, 0i64);
        for (line, seg_line) in mappings.split(';').enumerate() {
            if seg_line.is_empty() {
                continue;
            }
            let mut gc = 0i64;
            for seg in seg_line.split(',') {
                let b = seg.as_bytes();
                let mut i = 0;
                gc += decode_vlq(b, &mut i);
                sidx += decode_vlq(b, &mut i);
                sl += decode_vlq(b, &mut i);
                sc += decode_vlq(b, &mut i);
                assert_eq!(sidx, 0, "single source");
                out.push((line as i64, gc, sl, sc));
            }
        }
        out
    }

    #[test]
    fn vlq_round_trips() {
        for v in [0i64, 1, -1, 16, -16, 1234, -1234, 123456] {
            let mut s = String::new();
            vlq(&mut s, v);
            let mut i = 0;
            assert_eq!(decode_vlq(s.as_bytes(), &mut i), v);
            assert_eq!(i, s.len());
        }
    }

    #[test]
    fn empty_edits_map_the_source_one_to_one() {
        let src = "const a = 1;\nconst b = 2;\n";
        let json = build_v3("app.js", src, &[]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["version"], 3);
        assert_eq!(v["sources"][0], "app.js");
        assert_eq!(v["sourcesContent"][0], src);
        let decoded = decode(v["mappings"].as_str().unwrap());
        // Each line starts mapped to its own start.
        assert!(decoded.contains(&(0, 0, 0, 0)));
        assert!(decoded.contains(&(1, 0, 1, 0)));
    }

    #[test]
    fn a_prepend_maps_the_body_back_to_its_start() {
        // Insert a two-line decoder at offset 0, then the body follows. The body
        // start in the output must trace back to source (0, 0).
        let src = "const a = 1;";
        let decoder = Edit::insert(0, "var D=1;\nvar E=2;\n".to_string());
        let json = build_v3("app.js", src, &[&decoder]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let decoded = decode(v["mappings"].as_str().unwrap());
        // The decoder occupies generated lines 0 and 1; the body begins at
        // generated line 2, column 0, and maps to the source origin.
        assert!(
            decoded.contains(&(2, 0, 0, 0)),
            "body start should map to source origin, got {decoded:?}"
        );
    }

    #[test]
    fn a_replacement_maps_back_to_the_replaced_range() {
        // Replace the literal 'x' (bytes 10..13) with a decode call.
        let src = "const a = 'x';";
        let call = Edit::replace(10, 13, "D(0)".to_string());
        let json = build_v3("app.js", src, &[&call]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let decoded = decode(v["mappings"].as_str().unwrap());
        // The replacement begins at generated column 10 and traces to source
        // column 10, where the literal started.
        assert!(
            decoded.contains(&(0, 10, 0, 10)),
            "replacement should map to the literal start, got {decoded:?}"
        );
        // The text after the replacement keeps its mapping: ';' sits at source
        // column 13, and its generated column is 10 + len("D(0)") = 14.
        assert!(
            decoded.contains(&(0, 14, 0, 13)),
            "tail should realign after the edit, got {decoded:?}"
        );
    }
}
