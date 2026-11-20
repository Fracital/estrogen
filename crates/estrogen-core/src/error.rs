//! Error type for estrogen-core.
//!
//! estrogen fails closed. When a file cannot be fully handled, the pass returns an
//! [`Error`] that names the file and the reason, and the caller writes no
//! partial output for that file. The CLI turns any error into a nonzero exit.

use std::path::PathBuf;

/// Result alias used across estrogen-core.
pub type Result<T> = std::result::Result<T, Error>;

/// What went wrong while loading config or obfuscating a build.
///
/// Every variant that concerns one file carries that file's path, so the CLI
/// can name it in the exit message. [`Error::Engine`] marks an internal
/// invariant that should not happen on well-formed input.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file or directory could not be read or written.
    #[error("io error at {path}: {source}")]
    Io {
        /// The path estrogen was reading or writing.
        path: PathBuf,
        /// The underlying std::io error.
        source: std::io::Error,
    },

    /// A config or canary or names file did not parse.
    #[error("config error in {path}: {message}")]
    Config {
        /// The config file that failed to load.
        path: PathBuf,
        /// What the parser reported.
        message: String,
    },

    /// A JS, HTML or SVG file did not parse. estrogen never ships a file it cannot
    /// read, so a parse failure is fatal for that file.
    #[error("parse error in {path}: {message}")]
    Parse {
        /// The source file that failed to parse.
        path: PathBuf,
        /// The first parser diagnostic.
        message: String,
    },

    /// A canary literal survived in the output. The build must not ship.
    #[error("canary still present in {path}: {literal:?}")]
    Canary {
        /// The file the canary was found in.
        path: PathBuf,
        /// The plaintext literal that must not survive verbatim.
        literal: String,
    },

    /// Two edits from the passes cover the same bytes. The passes must produce
    /// non-overlapping byte ranges, so this is a bug in a pass, not bad input.
    #[error("overlapping edits at bytes {a_start}..{a_end} and {b_start}..{b_end}")]
    OverlappingEdits {
        /// Start of the first edit.
        a_start: usize,
        /// End of the first edit.
        a_end: usize,
        /// Start of the second edit.
        b_start: usize,
        /// End of the second edit.
        b_end: usize,
    },

    /// An edit points outside the source string. Also a pass bug.
    #[error("edit range {start}..{end} is outside the {len}-byte source")]
    EditOutOfBounds {
        /// Start byte of the bad edit.
        start: usize,
        /// End byte of the bad edit.
        end: usize,
        /// Length of the source the edit was meant for.
        len: usize,
    },

    /// An internal engine invariant failed: canary-free sampling that did not
    /// converge, or a pool entry or key of the wrong length or bad base64. It
    /// does not happen on well-formed input, so it marks a bug rather than bad
    /// input.
    #[error("engine error: {0}")]
    Engine(&'static str),

    /// A wasm module could not be walked, or mapping it would have changed its
    /// structure. estrogen fails closed on a module it cannot handle rather than
    /// ship a corrupt binary. The message names what went wrong and where.
    #[error("wasm: {message}")]
    Wasm {
        /// What went wrong, including the byte offset where the walk stopped.
        message: String,
    },

    /// The proxy transform could not rename a file: a store opened under a name
    /// not on the allow-list, a regular expression that spells a proxy name
    /// across an escape or class, or an app-shared name in an unsupported
    /// position. estrogen fails closed rather than leave a proxy name in the clear.
    #[error("{message}")]
    Proxy {
        /// What went wrong, naming the file where the caller has it.
        message: String,
    },
}

impl Error {
    /// Build an [`Error::Io`] from a path and a std::io error.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}
