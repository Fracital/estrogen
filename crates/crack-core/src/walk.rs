//! File walk and eligibility gate.
//!
//! crack rewrites an already-built directory in place. This module decides which
//! files in that tree it touches. The rules are ported from
//! `obfuscate-build.mjs`: process JS, HTML and SVG (and the proxy wasm), never a
//! `.map` file, and never a file crack has already processed.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::format::MARKER;

/// What kind of file this is, by extension. crack handles each kind with a
/// different set of passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// A JS chunk or inline script source (`.js`, `.mjs`, `.cjs`).
    Js,
    /// An HTML shell (`.html`, `.htm`).
    Html,
    /// An SVG cloak served as `image/svg+xml` (`.svg`).
    Svg,
    /// A WebAssembly module (`.wasm`), handled only for the proxy transports.
    Wasm,
    /// Any other file, which crack leaves alone.
    Other,
}

impl FileKind {
    /// Classify a path by its extension.
    ///
    /// The `.cjs` extension still classifies as [`FileKind::Js`]; the script vs
    /// module distinction is a parse concern, handled later, not an eligibility
    /// one.
    pub fn classify(path: &Path) -> FileKind {
        match extension_lower(path).as_deref() {
            Some("js") | Some("mjs") | Some("cjs") => FileKind::Js,
            Some("html") | Some("htm") => FileKind::Html,
            Some("svg") => FileKind::Svg,
            Some("wasm") => FileKind::Wasm,
            _ => FileKind::Other,
        }
    }

    /// Whether crack has any pass for this kind. [`FileKind::Other`] and
    /// [`FileKind::Wasm`] are not processed on their own here; wasm is reached
    /// through the proxy transport handling.
    pub fn is_processed(self) -> bool {
        matches!(self, FileKind::Js | FileKind::Html | FileKind::Svg)
    }
}

/// Lowercased extension of a path, without the dot.
fn extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Whether a path is a source map crack must never touch.
///
/// crack writes no map next to a served build and strips `sourceMappingURL`
/// comments from the chunks it processes; a `.map` file is never input.
pub fn is_source_map(path: &Path) -> bool {
    extension_lower(path).as_deref() == Some("map")
}

/// Whether crack should obfuscate this JS file, given its content.
///
/// Ported from `shouldProcessJs` in `obfuscate-build.mjs`: every JS chunk is in
/// scope, with no size cap and no vendor exemption, including `sw.js` and the
/// proxy runtime. The one gate is the already-processed marker: a file that
/// carries [`MARKER`] has been obfuscated, and a second pass would encrypt the
/// decoder the first pass added, so it is skipped.
pub fn should_process_js(_path: &Path, content: &str) -> bool {
    !content.contains(MARKER)
}

/// Why a file in the walked tree is not obfuscated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// A `.map` file. Never input.
    SourceMap,
    /// An extension with no pass (anything not JS, HTML or SVG).
    Policy,
    /// Already carries the obfuscation marker.
    AlreadyProcessed,
}

/// The decision for one path: process it as a kind, or skip it with a reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eligibility {
    /// Obfuscate this file with the passes for the given kind.
    Process(FileKind),
    /// Leave this file alone, for the given reason.
    Skip(SkipReason),
}

/// Decide whether to process a path, reading its content only when the extension
/// could be processed.
///
/// `content` is the file's text when it has been read, or `None` to decide on
/// the path alone. The marker check for JS needs the content; when it is `None`
/// the JS gate is treated as open and the real check happens once the file is
/// read.
pub fn eligibility(path: &Path, content: Option<&str>) -> Eligibility {
    if is_source_map(path) {
        return Eligibility::Skip(SkipReason::SourceMap);
    }
    let kind = FileKind::classify(path);
    if !kind.is_processed() {
        return Eligibility::Skip(SkipReason::Policy);
    }
    if kind == FileKind::Js {
        if let Some(text) = content {
            if !should_process_js(path, text) {
                return Eligibility::Skip(SkipReason::AlreadyProcessed);
            }
        }
    } else if let Some(text) = content {
        if text.contains(MARKER) {
            return Eligibility::Skip(SkipReason::AlreadyProcessed);
        }
    }
    Eligibility::Process(kind)
}

/// Every regular file under `dir`, depth first, following crack's own order
/// (directory entries sorted by name for deterministic reports).
///
/// This returns every file, processed or not, so the caller can report skips.
/// Filtering by [`eligibility`] happens per file, after the content is read.
pub fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = WalkDir::new(dir)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn classify_by_extension() {
        assert_eq!(FileKind::classify(Path::new("a/b.js")), FileKind::Js);
        assert_eq!(FileKind::classify(Path::new("a/b.mjs")), FileKind::Js);
        assert_eq!(FileKind::classify(Path::new("a/b.cjs")), FileKind::Js);
        assert_eq!(FileKind::classify(Path::new("a/b.HTML")), FileKind::Html);
        assert_eq!(FileKind::classify(Path::new("a/b.svg")), FileKind::Svg);
        assert_eq!(FileKind::classify(Path::new("a/b.wasm")), FileKind::Wasm);
        assert_eq!(FileKind::classify(Path::new("a/b.css")), FileKind::Other);
        assert_eq!(FileKind::classify(Path::new("a/b")), FileKind::Other);
    }

    #[test]
    fn map_files_are_skipped() {
        assert!(is_source_map(Path::new("app.js.map")));
        assert_eq!(
            eligibility(Path::new("app.js.map"), None),
            Eligibility::Skip(SkipReason::SourceMap)
        );
    }

    #[test]
    fn non_processed_extensions_are_policy_skips() {
        assert_eq!(
            eligibility(Path::new("styles.css"), None),
            Eligibility::Skip(SkipReason::Policy)
        );
        assert_eq!(
            eligibility(Path::new("logo.wasm"), None),
            Eligibility::Skip(SkipReason::Policy)
        );
    }

    #[test]
    fn js_without_marker_is_processed() {
        assert_eq!(
            eligibility(Path::new("app.js"), Some("const a = 1;")),
            Eligibility::Process(FileKind::Js)
        );
    }

    #[test]
    fn js_with_marker_is_already_processed() {
        let body = format!("/* {MARKER} */ const a = 1;");
        assert_eq!(
            eligibility(Path::new("app.js"), Some(&body)),
            Eligibility::Skip(SkipReason::AlreadyProcessed)
        );
    }

    #[test]
    fn should_process_js_honours_marker() {
        assert!(should_process_js(Path::new("sw.js"), "x"));
        assert!(!should_process_js(
            Path::new("sw.js"),
            &format!("{MARKER}x")
        ));
    }

    #[test]
    fn html_without_content_is_processed() {
        assert_eq!(
            eligibility(Path::new("index.html"), None),
            Eligibility::Process(FileKind::Html)
        );
    }
}
