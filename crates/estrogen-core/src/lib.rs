//! estrogen-core: the library behind the estrogen web-build obfuscator.
//!
//! estrogen rewrites an already-built output directory (a `dist`). It does not run
//! the bundler. It walks the built tree, and for every JS chunk, HTML shell and
//! SVG cloak it rewrites the sensitive plaintext: string and template literals
//! into an encrypted per-file pool, the proxy and store names into per-build
//! substitutes, and the embedded wasm names to match. It leaves a build with no
//! verbatim canary literal, and it does not change what the code does.
//!
//! This raises the cost of reading a build. It is not secrecy. A key ships in
//! each file, and anyone with devtools can run the code. Nothing estrogen writes is
//! secret.
//!
//! # How a run is shaped
//!
//! Build an [`Options`] (from a config file with [`Options::from_config`], or
//! from [`Options::default`] with fields set), make an [`Obfuscator`], and call
//! [`Obfuscator::obfuscate_dir`] on each built directory. Each call returns a
//! [`Report`] of every file seen, as [`FileReport`] entries.
//!
//! ```no_run
//! use estrogen_core::{Obfuscator, Options};
//! use std::path::Path;
//!
//! let options = Options::default();
//! let obf = Obfuscator::new(options)?;
//! let report = obf.obfuscate_dir(Path::new("dist"))?;
//! println!("{} files, {} obfuscated", report.total(), report.obfuscated());
//! # Ok::<(), estrogen_core::Error>(())
//! ```
//!
//! # The edit-and-splice rule
//!
//! estrogen edits the original source text; it does not reprint the AST. A pass
//! parses with OXC, walks the tree to find byte ranges, and records each change
//! as an edit. [`edit::EditSet`] splices the edits into the original string, so
//! every byte a pass does not touch stays exactly as the bundler emitted it. See
//! [`edit`] for the detail and [`pass`] for the passes.
//!
//! # What the engine does
//!
//! The engine is built and tested against Fracital's
//! `scripts/obfuscate-build.mjs`, cited per module. The passes in [`pass`]
//! encrypt string and template literals into the ChaCha20 pool and emit its
//! decoder ([`mod@format`]), shorten local bindings in app scripts, rename proxy
//! and store names in vendor files, map embedded wasm names, and rewrite HTML
//! and SVG markup. The obfuscator
//! also vendors files into a build ([`Obfuscator::build`]) and emits private V3
//! source maps. The edit-and-splice core, config loading, the file walk and
//! eligibility gate, and reporting sit underneath. [`Error::Engine`] now marks
//! an internal invariant, such as canary-free sampling that did not converge,
//! not unbuilt work.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod api;
mod chacha;
pub mod edit;
pub mod error;
pub mod format;
mod keys;
pub mod obfuscator;
pub mod options;
pub mod pass;
mod proxy;
pub mod report;
mod sourcemap;
mod vendor;
pub mod vm;
pub mod walk;
mod wasm;
mod watermark;

pub use api::{obfuscate_js, Obfuscated};
pub use error::{Error, Result};
pub use format::FORMAT_VERSION;
pub use obfuscator::Obfuscator;
pub use options::{
    Canary, CanaryList, NameMap, Options, ProxyConfig, Rewrite, VendorInput, VendorMode,
};
pub use report::{FileReport, FileStatus, Report};
pub use walk::{Eligibility, FileKind, SkipReason};
