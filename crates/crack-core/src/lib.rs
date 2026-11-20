//! crack-core: the library behind the crack web-build obfuscator.
//!
//! crack rewrites an already-built output directory (a `dist`). It does not run
//! the bundler. It walks the built tree, and for every JS chunk, HTML shell and
//! SVG cloak it rewrites the sensitive plaintext: string and template literals
//! into an encrypted per-file pool, the proxy and store names into per-build
//! substitutes, and the embedded wasm names to match. It leaves a build with no
//! verbatim canary literal, and it does not change what the code does.
//!
//! This raises the cost of reading a build. It is not secrecy. A key ships in
//! each file, and anyone with devtools can run the code. Nothing crack writes is
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
//! use crack_core::{Obfuscator, Options};
//! use std::path::Path;
//!
//! let options = Options::default();
//! let obf = Obfuscator::new(options)?;
//! let report = obf.obfuscate_dir(Path::new("dist"))?;
//! println!("{} files, {} obfuscated", report.total(), report.obfuscated());
//! # Ok::<(), crack_core::Error>(())
//! ```
//!
//! # The edit-and-splice rule
//!
//! crack edits the original source text; it does not reprint the AST. A pass
//! parses with OXC, walks the tree to find byte ranges, and records each change
//! as an edit. [`edit::EditSet`] splices the edits into the original string, so
//! every byte a pass does not touch stays exactly as the bundler emitted it. See
//! [`edit`] for the detail and [`pass`] for the passes.
//!
//! # State of the engine
//!
//! The obfuscation engine is not built yet. The passes in [`pass`] are identity
//! stubs: they parse (so a parse failure still fails closed) and return no
//! edits. The edit-and-splice core, config loading, the file walk and
//! eligibility gate, and reporting are real and tested. The parts that encrypt
//! the pool and emit the decoder ([`format`]) return [`Error::Engine`] until the
//! engine lands. The spec for all of it is Fracital's
//! `scripts/obfuscate-build.mjs`, cited in each module.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod edit;
pub mod error;
pub mod format;
pub mod obfuscator;
pub mod options;
pub mod pass;
pub mod report;
pub mod walk;

pub use error::{Error, Result};
pub use format::FORMAT_VERSION;
pub use obfuscator::Obfuscator;
pub use options::{Canary, CanaryList, NameMap, Options};
pub use report::{FileReport, FileStatus, Report};
pub use walk::{Eligibility, FileKind, SkipReason};
