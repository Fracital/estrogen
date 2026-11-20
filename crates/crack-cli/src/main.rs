//! The `crack` binary.
//!
//! crack rewrites an already-built output directory in place, hiding the
//! sensitive plaintext a site commits to not shipping verbatim. This front end
//! parses the arguments, loads the config and its overrides, walks each
//! directory through [`crack_core`], prints a report, and sets the exit code. It
//! fails closed: any file crack cannot handle aborts the run with a nonzero
//! exit.
//!
//! The obfuscation engine is not built yet. The passes are identity stubs, so a
//! normal run parses every eligible file (failing closed on a parse error) and
//! writes it back unchanged. `--check` then reports those files as unprotected
//! and exits nonzero, which is the honest state until the engine lands.

mod cli;

use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use crack_core::report::skip_reason_label;
use crack_core::report::FileStatus;
use crack_core::{Obfuscator, Report};

use cli::Cli;

/// Exit code for a run that completed with everything protected.
const EXIT_OK: u8 = 0;
/// Exit code for a failure: a parse error, a file that cannot be handled, or bad
/// arguments reaching the run.
const EXIT_FAILURE: u8 = 1;
/// Exit code for `--check` when something is unprotected.
const EXIT_UNPROTECTED: u8 = 2;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(message) => {
            eprintln!("crack: {message}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// Load options, run the obfuscator over every directory, print the report, and
/// return the exit code. Errors come back as a message for `main` to print.
fn run(cli: &Cli) -> Result<u8, String> {
    let options = cli.build_options().map_err(|e| e.to_string())?;
    let check = options.check;
    let quiet = cli.quiet;
    let verbose = cli.verbose;

    let obf = Obfuscator::new(options).map_err(|e| e.to_string())?;

    let dirs = cli.dirs();
    let report = obf.obfuscate_dirs(&dirs).map_err(|e| e.to_string())?;

    if !quiet {
        print_report(&report, verbose);
    }

    if check && report.has_unprotected() {
        if !quiet {
            eprintln!(
                "crack: {} eligible file(s) are unprotected",
                report.unprotected().len()
            );
        }
        return Ok(EXIT_UNPROTECTED);
    }

    Ok(EXIT_OK)
}

/// Print the run summary, and per-file lines when `verbose` is set.
fn print_report(report: &Report, verbose: bool) {
    if verbose {
        for file in &report.files {
            let path = display_path(&file.path);
            match &file.status {
                FileStatus::Obfuscated { edits } => {
                    println!("  obfuscated  {path}  ({edits} edits)");
                }
                FileStatus::Unchanged => {
                    println!("  unchanged   {path}");
                }
                FileStatus::Skipped(reason) => {
                    println!("  skipped     {path}  ({})", skip_reason_label(*reason));
                }
            }
        }
    }

    println!(
        "crack: {} file(s): {} obfuscated, {} unchanged, {} skipped",
        report.total(),
        report.obfuscated(),
        report.unchanged(),
        report.skipped()
    );

    if report.obfuscated() == 0 && report.unchanged() > 0 {
        println!(
            "crack: the obfuscation engine is stubbed, so {} eligible file(s) were \
             left unchanged, not protected",
            report.unchanged()
        );
    }
}

/// A path for display, relative where it reads cleanly, else as given.
fn display_path(path: &Path) -> String {
    path.display().to_string()
}
