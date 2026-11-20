//! The `estrogen` binary.
//!
//! estrogen rewrites an already-built output directory in place, hiding the
//! sensitive plaintext a site commits to not shipping verbatim. This front end
//! parses the arguments, loads the config and its overrides, walks each
//! directory through [`estrogen_core`], prints a report, and sets the exit code.
//! It fails closed: any file estrogen cannot handle aborts the run with a
//! nonzero exit.

mod cli;
mod watch;

use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use estrogen_core::report::skip_reason_label;
use estrogen_core::report::FileStatus;
use estrogen_core::{Obfuscator, Report};

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
            eprintln!("estrogen: {message}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// Load options, run the obfuscator over every directory, print the report, and
/// return the exit code. Errors come back as a message for `main` to print.
fn run(cli: &Cli) -> Result<u8, String> {
    let common = cli.common();
    let dirs = cli.dirs();
    if dirs.is_empty() {
        return Err("no directory given; pass one or more built directories".to_string());
    }

    let options = cli.build_options().map_err(|e| e.to_string())?;
    let check = options.check;
    let quiet = common.quiet;
    let verbose = common.verbose;

    let obf = Obfuscator::new(options).map_err(|e| e.to_string())?;
    let is_build = cli.is_build();

    // Watch mode re-runs the pass on each debounced change and does not return
    // until the process is stopped with Ctrl-C.
    if common.watch {
        let paths = watch::watch_paths(&obf, &dirs, is_build);
        if paths.is_empty() {
            return Err("no existing directory to watch".to_string());
        }
        if !quiet {
            println!(
                "estrogen: watching {} path(s); press Ctrl-C to stop",
                paths.len()
            );
        }
        let never_stop = AtomicBool::new(false);
        watch::watch(
            &obf,
            &dirs,
            is_build,
            &paths,
            watch::DEBOUNCE,
            &never_stop,
            |result| match result {
                Ok(report) => {
                    if !quiet {
                        print_report(report, verbose);
                    }
                }
                Err(message) => eprintln!("estrogen: {message}"),
            },
        )?;
        return Ok(EXIT_OK);
    }

    // `build` vendors the configured inputs into each dist, then obfuscates; the
    // default run only obfuscates what is already there.
    let report = if is_build {
        let mut combined = Report::new(dirs.clone());
        for dir in &dirs {
            combined.absorb(obf.build(dir).map_err(|e| e.to_string())?);
        }
        combined
    } else {
        obf.obfuscate_dirs(&dirs).map_err(|e| e.to_string())?
    };

    if !quiet {
        print_report(&report, verbose);
    }

    if check && report.has_unprotected() {
        if !quiet {
            eprintln!(
                "estrogen: {} eligible file(s) are unprotected",
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
                FileStatus::Vendored { obfuscated } => {
                    let how = if *obfuscated {
                        "obfuscated"
                    } else {
                        "verbatim"
                    };
                    println!("  vendored    {path}  ({how})");
                }
                FileStatus::Skipped(reason) => {
                    println!("  skipped     {path}  ({})", skip_reason_label(*reason));
                }
            }
        }
    }

    println!(
        "estrogen: {} file(s): {} obfuscated, {} vendored, {} unchanged, {} skipped",
        report.total(),
        report.obfuscated(),
        report.vendored(),
        report.unchanged(),
        report.skipped()
    );

    if report.obfuscated() == 0 && report.unchanged() > 0 {
        println!(
            "estrogen: {} eligible file(s) had nothing to rewrite and were left \
             unchanged, not protected",
            report.unchanged()
        );
    }
}

/// A path for display, relative where it reads cleanly, else as given.
fn display_path(path: &Path) -> String {
    path.display().to_string()
}
