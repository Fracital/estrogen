//! HTML shell and SVG cloak pass.
//!
//! Spec: the `obfuscate-build.mjs` header and `neutralizeTitleMeta`,
//! `processHtmlFile`, `obfuscateInlineScript`, `obfuscateSvg`, `xmlScriptToJs`,
//! `decodeXmlEntities`, `wrapCdata`, `insertSvgMarker`. For an HTML shell it
//! neutralizes a canary `<title>` or description `<meta>` and encrypts the
//! canary-bearing inline `<script>` the same way as a JS chunk. For an SVG cloak
//! served as `image/svg+xml` it does the same, with two XML twists: the inline
//! `<script>` body is XML, so entities are decoded (`&amp;` to `&`) and any
//! CDATA section is taken literally before obfuscating, which recovers the exact
//! JS the browser runs; and the obfuscated body, whose decoder uses `<`, `<<`
//! and `&`, is re-emitted inside `<![CDATA[ ... ]]>`, with any `]]>` split across
//! CDATA sections.
//!
//! The inline script is obfuscated by reusing [`crate::pass::literals`]: the
//! recovered JS text is run through the literal pool pass and the result is
//! spliced back. Proxy-name renaming in markup (`mapProxyMarkup`) and the proxy
//! transform for inline scripts are the names pass ([`crate::pass::names`]), not
//! here, so a script that carries only a proxy name and no canary is left for
//! that pass.
//!
//! crack edits the source by byte range; it does not rebuild the document. Tag
//! scanning is case-insensitive through an ASCII-lowercased copy of the source,
//! whose byte offsets map one to one onto the original.

use std::ops::Range;
use std::path::Path;

use crate::edit::{Edit, EditSet};
use crate::error::{Error, Result};
use crate::format::MARKER;
use crate::options::Options;
use crate::pass::literals::LiteralsPass;
use crate::pass::{FileContext, Pass};
use crate::walk::FileKind;

/// Neutral shell text used when a static `<title>` carried a canary.
const NEUTRAL_TITLE: &str = "Workspace";
/// Neutral shell text used when a description `<meta>` carried a canary.
const NEUTRAL_DESC: &str = "A fast, private workspace.";

/// The HTML and SVG markup pass.
#[derive(Debug, Default)]
pub struct MarkupPass;

impl Pass for MarkupPass {
    fn name(&self) -> &'static str {
        "markup"
    }

    fn collect_edits(&self, ctx: &FileContext) -> Result<EditSet> {
        // Already processed: the decoder a first run added must not be re-wrapped.
        if ctx.source.contains(MARKER) {
            return Ok(EditSet::new());
        }
        let is_svg = ctx.kind == FileKind::Svg;
        let source = ctx.source;
        let lower = source.to_ascii_lowercase();

        let mut edits = EditSet::new();
        let mut changed = false;

        // Title: the first <title>...</title> whose text carries a canary.
        if let Some((inner, _close)) = find_title_inner(&lower) {
            if has_canary(&source[inner.clone()], ctx.options) {
                edits.push(Edit::replace(inner.start, inner.end, NEUTRAL_TITLE));
                changed = true;
            }
        }

        // Description <meta>: replace a canary content value with neutral text.
        for tag in find_tags(&lower, "<meta") {
            let tag_src = &source[tag.clone()];
            let tag_low = &lower[tag.clone()];
            if attr_value(tag_low, "name").map(|r| tag_src[r].eq_ignore_ascii_case("description"))
                != Some(true)
            {
                continue;
            }
            if let Some(val) = attr_value(tag_low, "content") {
                if has_canary(&tag_src[val.clone()], ctx.options) {
                    let start = tag.start + val.start;
                    let end = tag.start + val.end;
                    edits.push(Edit::replace(start, end, NEUTRAL_DESC));
                    changed = true;
                }
            }
        }

        // Inline <script>s that leak a canary.
        for script in find_scripts(source, &lower) {
            let attrs_src = &source[script.attrs.clone()];
            let attrs_low = &lower[script.attrs.clone()];
            if attr_value(attrs_low, "src").is_some() {
                continue; // external script
            }
            let body = &source[script.body.clone()];
            if body.trim().is_empty() {
                continue;
            }
            // Milestone 5 handles the canary case (app path). A script that only
            // carries a proxy name is left for the names pass.
            if !has_canary(body, ctx.options) {
                continue;
            }
            let is_module = attr_value(attrs_low, "type")
                .map(|r| attrs_src[r].eq_ignore_ascii_case("module"))
                == Some(true);

            let js = if is_svg {
                xml_script_to_js(body)
            } else {
                body.to_string()
            };
            let obf = obfuscate_inline_js(&js, is_module, ctx.options, &ctx.path)?;
            if obf != js {
                let replacement = if is_svg { wrap_cdata(&obf) } else { obf };
                edits.push(Edit::replace(
                    script.body.start,
                    script.body.end,
                    replacement,
                ));
                changed = true;
            }
        }

        if !changed {
            return Ok(EditSet::new());
        }

        // Marker: an XML comment, after an optional <?xml ...?> declaration for
        // SVG so the declaration stays first; at the head for HTML.
        let marker = format!("<!--{MARKER}-->");
        let marker_at = if is_svg {
            xml_decl_end(source, &lower)
        } else {
            0
        };
        edits.push(Edit::insert(marker_at, marker));

        // Fail closed: no canary may survive anywhere in the shell.
        let applied = edits.apply(source)?;
        for canary in &ctx.options.canaries {
            if !canary.value.is_empty() && applied.contains(&canary.value) {
                return Err(Error::Canary {
                    path: ctx.path.clone(),
                    literal: canary.value.clone(),
                });
            }
        }

        Ok(edits)
    }
}

/// Whether any canary value is a substring of `s`.
fn has_canary(s: &str, options: &Options) -> bool {
    options
        .canaries
        .iter()
        .any(|c| !c.value.is_empty() && s.contains(&c.value))
}

/// Obfuscate an inline script body by reusing the literal pool pass. Mirrors the
/// reference `obfuscateInlineScript` / `tryBoth`: parse as the preferred source
/// type, falling back to the other on a parse error. Proxy handling is deferred
/// to the names pass, so this always takes the app path.
fn obfuscate_inline_js(
    body: &str,
    is_module: bool,
    options: &Options,
    hint: &Path,
) -> Result<String> {
    let order = if is_module {
        [true, false]
    } else {
        [false, true]
    };
    let mut last_err = None;
    for &as_module in &order {
        match run_literals(body, as_module, options, hint) {
            Ok(code) => return Ok(code),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("at least one attempt ran"))
}

/// Run the literal pass over one JS string and return the rewritten source. The
/// source type is chosen by giving the file context a `.mjs` or `.cjs` path.
fn run_literals(body: &str, as_module: bool, options: &Options, hint: &Path) -> Result<String> {
    let stem = hint
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("inline");
    let name = format!("{stem}.{}", if as_module { "mjs" } else { "cjs" });
    let path = Path::new(&name);
    let ctx = FileContext::new(path, FileKind::Js, body, options);
    let edits = LiteralsPass.collect_edits(&ctx)?;
    edits.apply(body)
}

/// The byte range of the first `<title>`'s inner text, and the range the closing
/// tag occupies, or `None` if there is no complete title element.
fn find_title_inner(lower: &str) -> Option<(Range<usize>, Range<usize>)> {
    let open = lower.find("<title")?;
    let gt = lower[open..].find('>').map(|i| open + i)?;
    let inner_start = gt + 1;
    let close = lower[inner_start..]
        .find("</title>")
        .map(|i| inner_start + i)?;
    Some((inner_start..close, close..close + "</title>".len()))
}

/// Every full tag that opens with `open` (for example `<meta`), as byte ranges
/// from the `<` through the closing `>`.
fn find_tags(lower: &str, open: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(s) = lower[pos..].find(open).map(|i| i + pos) {
        match lower[s..].find('>').map(|i| s + i) {
            Some(gt) => {
                out.push(s..gt + 1);
                pos = gt + 1;
            }
            None => break,
        }
    }
    out
}

/// One inline script element: the attribute range (between `<script` and `>`)
/// and the body range (between `>` and `</script>`).
struct ScriptSpan {
    attrs: Range<usize>,
    body: Range<usize>,
}

/// Every `<script ...>...</script>` in document order.
fn find_scripts(_source: &str, lower: &str) -> Vec<ScriptSpan> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(s) = lower[pos..].find("<script").map(|i| i + pos) {
        let attrs_start = s + "<script".len();
        let gt = match lower[attrs_start..].find('>').map(|i| attrs_start + i) {
            Some(g) => g,
            None => break,
        };
        let body_start = gt + 1;
        let close = match lower[body_start..]
            .find("</script>")
            .map(|i| body_start + i)
        {
            Some(c) => c,
            None => break,
        };
        out.push(ScriptSpan {
            attrs: attrs_start..gt,
            body: body_start..close,
        });
        pos = close + "</script>".len();
    }
    out
}

/// The byte offset just past an optional leading `<?xml ...?>` declaration.
fn xml_decl_end(source: &str, lower: &str) -> usize {
    let trimmed = source.len() - source.trim_start().len();
    if lower[trimmed..].starts_with("<?xml") {
        if let Some(end) = lower[trimmed..].find("?>") {
            return trimmed + end + "?>".len();
        }
    }
    0
}

/// Whether a byte can be part of an attribute name.
fn is_attr_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b':'
}

/// The byte range of attribute `name`'s value within `attrs`, quoted or bare.
/// `attrs_lower` is the ASCII-lowercased `attrs`, so matching is
/// case-insensitive while the range indexes the original.
fn attr_value(attrs_lower: &str, name: &str) -> Option<Range<usize>> {
    let bytes = attrs_lower.as_bytes();
    let mut search = 0;
    while let Some(p) = attrs_lower[search..].find(name).map(|i| i + search) {
        let after = p + name.len();
        let before_ok = p == 0 || !is_attr_name_char(bytes[p - 1]);
        search = after;
        if !before_ok {
            continue;
        }
        let mut j = after;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() {
            return None;
        }
        if bytes[j] == b'"' || bytes[j] == b'\'' {
            let quote = bytes[j];
            j += 1;
            let start = j;
            while j < bytes.len() && bytes[j] != quote {
                j += 1;
            }
            return Some(start..j);
        }
        let start = j;
        while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        return Some(start..j);
    }
    None
}

/// Decode the XML predefined entities and numeric char refs the SVG build uses
/// to escape JS into XML. Anything else is left verbatim.
fn decode_xml_entities(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            // Copy one UTF-8 char.
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        match s[i..].find(';') {
            Some(semi) => {
                let ent = &s[i + 1..i + semi];
                if let Some(decoded) = decode_one_entity(ent) {
                    out.push_str(&decoded);
                    i += semi + 1;
                } else {
                    out.push('&');
                    i += 1;
                }
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

/// Decode one entity body (the text between `&` and `;`), or `None` if it is not
/// one crack recognizes.
fn decode_one_entity(ent: &str) -> Option<String> {
    let b = ent.as_bytes();
    if b.first() == Some(&b'#') {
        let code = if b.get(1) == Some(&b'x') || b.get(1) == Some(&b'X') {
            u32::from_str_radix(&ent[2..], 16).ok()?
        } else {
            ent[1..].parse::<u32>().ok()?
        };
        return char::from_u32(code).map(|c| c.to_string());
    }
    let c = match ent {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => return None,
    };
    Some(c.to_string())
}

/// Reconstruct the JavaScript an XML parser hands the engine for a `<script>`
/// body: CDATA sections are literal, all other text has its entities decoded.
fn xml_script_to_js(body: &str) -> String {
    const OPEN: &str = "<![CDATA[";
    const CLOSE: &str = "]]>";
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        match body[i..].find(OPEN).map(|x| x + i) {
            None => {
                out.push_str(&decode_xml_entities(&body[i..]));
                break;
            }
            Some(start) => {
                out.push_str(&decode_xml_entities(&body[i..start]));
                let content_start = start + OPEN.len();
                match body[content_start..].find(CLOSE).map(|x| x + content_start) {
                    Some(end) => {
                        out.push_str(&body[content_start..end]);
                        i = end + CLOSE.len();
                    }
                    None => {
                        out.push_str(&body[content_start..]);
                        break;
                    }
                }
            }
        }
    }
    out
}

/// Wrap JS in a CDATA section, splitting any `]]>` across sections so it cannot
/// end the section early. The reconstituted text is identical.
fn wrap_cdata(js: &str) -> String {
    let safe = js.replace("]]>", "]]]]><![CDATA[>");
    format!("<![CDATA[{safe}]]>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::{Canary, Options};
    use std::process::Command;

    fn opts() -> Options {
        Options {
            seed: Some(b"markup-test-seed".to_vec()),
            canaries: ["Scramjet", "Fracital", "Go ad-free for $1"]
                .iter()
                .map(|v| Canary {
                    value: (*v).to_string(),
                    reason: String::new(),
                })
                .collect(),
            ..Options::default()
        }
    }

    fn run(kind: FileKind, src: &str) -> String {
        let o = opts();
        let name = if kind == FileKind::Svg {
            "cloak.svg"
        } else {
            "index.html"
        };
        let ctx = FileContext::new(name, kind, src, &o);
        let edits = MarkupPass.collect_edits(&ctx).unwrap();
        edits.apply(src).unwrap()
    }

    #[test]
    fn html_title_and_meta_canaries_are_neutralized() {
        let src = r#"<html><head><title>Scramjet proxy</title><meta name="description" content="Powered by Fracital"></head><body><p>hi</p></body></html>"#;
        let out = run(FileKind::Html, src);
        assert!(out.contains("<title>Workspace</title>"));
        assert!(out.contains(r#"content="A fast, private workspace.""#));
        assert!(!out.contains("Scramjet"));
        assert!(!out.contains("Fracital"));
        assert!(out.contains(MARKER));
    }

    #[test]
    fn html_inline_script_canary_is_encrypted() {
        let src = r#"<html><head><script>var labels = {a: "Scramjet", b: "Go ad-free for $1"};</script></head></html>"#;
        let out = run(FileKind::Html, src);
        assert!(!out.contains("\"Scramjet\""));
        assert!(!out.contains("Go ad-free for $1"));
        assert!(out.contains("__frx$"));
        assert!(out.starts_with(&format!("<!--{MARKER}-->")));
    }

    #[test]
    fn external_script_is_left_alone() {
        let src = r#"<html><script src="app.js"></script><title>Scramjet</title></html>"#;
        let out = run(FileKind::Html, src);
        // The title canary is still neutralized, but the external script tag stays.
        assert!(out.contains(r#"<script src="app.js"></script>"#));
    }

    #[test]
    fn no_canary_document_is_unchanged() {
        let o = Options {
            seed: Some(b"s".to_vec()),
            canaries: vec![Canary {
                value: "Scramjet".to_string(),
                reason: String::new(),
            }],
            ..Options::default()
        };
        let clean = r#"<html><head><title>Workspace</title><script>var x = "plain";</script></head></html>"#;
        let ctx = FileContext::new("index.html", FileKind::Html, clean, &o);
        let edits = MarkupPass.collect_edits(&ctx).unwrap();
        assert!(edits.is_empty());
        assert_eq!(edits.apply(clean).unwrap(), clean);
    }

    #[test]
    fn svg_output_is_well_formed_and_cdata_wrapped() {
        let src = r#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"><foreignObject><script>var l = {k: "Scramjet"};</script></foreignObject></svg>"#;
        let out = run(FileKind::Svg, src);
        assert!(!out.contains("Scramjet"));
        assert!(out.contains("<![CDATA["));
        assert!(out.contains("]]>"));
        // The marker sits after the xml declaration, which stays first.
        assert!(out.starts_with(r#"<?xml version="1.0"?><!--"#));
        assert_well_formed(&out);
    }

    #[test]
    fn svg_decodes_entities_before_obfuscating() {
        // The body escapes the string with &amp; and wraps it in CDATA.
        let src = r#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"><script>var s = "Scramjet &amp; co";</script></svg>"#;
        let out = run(FileKind::Svg, src);
        assert!(!out.contains("Scramjet"));
        assert_well_formed(&out);
    }

    #[test]
    fn idempotent_second_run_is_a_no_op() {
        let src = r#"<html><head><title>Scramjet</title><script>var a = "Scramjet";</script></head></html>"#;
        let once = run(FileKind::Html, src);
        let o = opts();
        let ctx = FileContext::new("index.html", FileKind::Html, &once, &o);
        let edits = MarkupPass.collect_edits(&ctx).unwrap();
        assert!(
            edits.is_empty(),
            "a marked document must not be re-processed"
        );
    }

    #[test]
    fn wrap_cdata_splits_embedded_terminator() {
        let wrapped = wrap_cdata("a]]>b");
        assert!(wrapped.starts_with("<![CDATA["));
        assert!(wrapped.ends_with("]]>"));
        // The only full ]]> is the final terminator; the embedded one is split.
        assert_eq!(wrapped.matches("]]>").count(), 2); // the split marker + the close
        assert!(wrapped.contains("]]]]><![CDATA[>"));
    }

    #[test]
    fn xml_script_to_js_round_trips_cdata_and_entities() {
        let body = "a &amp; b<![CDATA[ c < d ]]>e &lt;";
        assert_eq!(xml_script_to_js(body), "a & b c < d e <");
    }

    #[test]
    fn svg_inline_script_decodes_and_runs_in_node() {
        if Command::new("node").arg("--version").output().is_err() {
            eprintln!("SKIP svg_inline_script_decodes_and_runs_in_node: node not found");
            return;
        }
        // An SVG whose inline script builds a map including a canary key, with
        // the body CDATA-wrapped as the build emits it.
        let src = r#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"><script><![CDATA[globalThis.__OUT__ = {label: "Scramjet", price: "Go ad-free for $1"};]]></script></svg>"#;
        let out = run(FileKind::Svg, src);
        // Extract the obfuscated script body (inside CDATA) and run it in node.
        let open = out.find("<script>").unwrap() + "<script>".len();
        let close = out[open..].find("</script>").unwrap() + open;
        let body = &out[open..close];
        let js = xml_script_to_js(body); // undo the CDATA wrap to get the JS
        let harness = format!("{js}\nconsole.log(JSON.stringify(globalThis.__OUT__));\n");
        let dir = std::env::temp_dir().join(format!("crack-markup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("run.js");
        std::fs::write(&file, harness).unwrap();
        let output = Command::new("node").arg(&file).output().unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert!(
            output.status.success(),
            "node failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.contains("Scramjet"),
            "decoded label missing: {stdout}"
        );
        assert!(
            stdout.contains("Go ad-free for $1"),
            "decoded price missing: {stdout}"
        );
    }

    /// A small well-formedness check: tags nest and close, and no stray `<` or
    /// `]]>` sits in text (mirrors the reference's XML checker).
    fn assert_well_formed(xml: &str) {
        let mut stack: Vec<String> = Vec::new();
        let bytes = xml.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'<' {
                if xml[i..].starts_with("<![CDATA[") {
                    let end = xml[i..].find("]]>").expect("unterminated CDATA") + i;
                    i = end + "]]>".len();
                    continue;
                }
                if xml[i..].starts_with("<?") {
                    let end = xml[i..].find("?>").expect("unterminated PI") + i;
                    i = end + "?>".len();
                    continue;
                }
                if xml[i..].starts_with("<!--") {
                    let end = xml[i..].find("-->").expect("unterminated comment") + i;
                    i = end + "-->".len();
                    continue;
                }
                let gt = xml[i..].find('>').expect("unterminated tag") + i;
                let inner = &xml[i + 1..gt];
                if let Some(nm) = inner.strip_prefix('/') {
                    let name = nm.trim().to_string();
                    assert_eq!(
                        stack.pop().as_deref(),
                        Some(name.as_str()),
                        "mismatched close"
                    );
                } else if !inner.ends_with('/') {
                    let name: String = inner.split([' ', '\t', '\n']).next().unwrap().to_string();
                    stack.push(name);
                }
                i = gt + 1;
            } else {
                let next = xml[i..].find('<').map(|x| x + i).unwrap_or(xml.len());
                let text = &xml[i..next];
                assert!(!text.contains("]]>"), "stray ]]> in text");
                i = next;
            }
        }
        assert!(stack.is_empty(), "unclosed tags: {stack:?}");
    }
}
