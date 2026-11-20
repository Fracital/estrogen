//! Argument parsing and option assembly for the `estrogen` binary.
//!
//! [`Cli`] is the clap-derived argument set. The flags live in [`Common`], shared
//! by the default obfuscate-only run and the `build` subcommand that vendors the
//! configured inputs into the dist first. [`Cli::build_options`] loads the config
//! file when one is given, then overlays the flags that override it (`--canary`,
//! `--names`, `--sourcemap`, `--seed`, `--jobs`, `--check`) and returns the
//! [`Options`] the run uses.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use estrogen_core::options::{load_name_map, CanaryList};
use estrogen_core::{Error, Options};

/// estrogen: a post-build web obfuscator. It rewrites a built directory in place,
/// and with `build` it vendors the configured inputs into the dist first.
#[derive(Debug, Parser)]
#[command(name = "estrogen", version, about, long_about = None)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Cli {
    /// The subcommand, or none for an obfuscate-only run.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Flags for the default obfuscate-only run.
    #[command(flatten)]
    pub common: Common,
}

/// The subcommands beyond the default obfuscate-only run.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Vendor the configured `[[vendor]]` inputs into the dist roots, then
    /// obfuscate. Needs `[[vendor]]` tables in the config.
    Build {
        /// Flags for the build run.
        #[command(flatten)]
        common: Common,
    },
}

/// The arguments shared by every run mode.
#[derive(Debug, Clone, Args)]
pub struct Common {
    /// One or more built directories to rewrite in place.
    #[arg(value_name = "DIR")]
    pub dirs: Vec<PathBuf>,

    /// estrogen config file (TOML): per-site names, canary list, vendor inputs.
    #[arg(short = 'c', long = "config", value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Canary list (JSON). Overrides the config's canaries.
    #[arg(long = "canary", value_name = "FILE")]
    pub canary: Option<PathBuf>,

    /// Proxy and store name map (JSON). Overrides the config's names.
    #[arg(long = "names", value_name = "FILE")]
    pub names: Option<PathBuf>,

    /// Write private source maps here. The default is to write none.
    #[arg(long = "sourcemap", value_name = "DIR")]
    pub sourcemap: Option<PathBuf>,

    /// Deterministic seed, hex, for tests only. The default is random.
    #[arg(long = "seed", value_name = "HEX")]
    pub seed: Option<String>,

    /// How many files to process in parallel. The default is the CPU count.
    #[arg(short = 'j', long = "jobs", value_name = "N")]
    pub jobs: Option<usize>,

    /// Report only, write nothing, and exit nonzero if anything is unprotected.
    #[arg(long = "check")]
    pub check: bool,

    /// Watch the directories and re-run on change. Not implemented yet.
    #[arg(long = "watch")]
    pub watch: bool,

    /// Print nothing but errors.
    #[arg(long = "quiet")]
    pub quiet: bool,

    /// Print a line per file.
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,
}

impl Cli {
    /// The active flags, from the subcommand or the top-level run.
    pub fn common(&self) -> &Common {
        match &self.command {
            Some(Command::Build { common }) => common,
            None => &self.common,
        }
    }

    /// Whether this is a `build` run (vendor, then obfuscate).
    pub fn is_build(&self) -> bool {
        matches!(self.command, Some(Command::Build { .. }))
    }

    /// The directories to process.
    pub fn dirs(&self) -> Vec<PathBuf> {
        self.common().dirs.clone()
    }

    /// Assemble the run options: load the config, then overlay the flags.
    pub fn build_options(&self) -> Result<Options, Error> {
        let common = self.common();
        let mut options = match &common.config {
            Some(path) => Options::from_config(path)?,
            None => Options::default(),
        };

        if let Some(path) = &common.canary {
            options.canaries = CanaryList::from_json_file(path)?.canaries;
        }
        if let Some(path) = &common.names {
            options.names = load_name_map(path)?;
        }
        if let Some(dir) = &common.sourcemap {
            options.sourcemap_dir = Some(dir.clone());
        }
        if let Some(hex) = &common.seed {
            options.seed = Some(parse_hex(hex).map_err(|message| Error::Config {
                path: PathBuf::from("--seed"),
                message,
            })?);
        }
        if let Some(jobs) = common.jobs {
            if jobs > 0 {
                options.jobs = jobs;
            }
        }
        if common.check {
            options.check = true;
        }

        Ok(options)
    }
}

/// Parse a hex string into bytes. The seed is tests-only, so the only rules are
/// an even length and hex digits.
fn parse_hex(input: &str) -> Result<Vec<u8>, String> {
    let s = input.strip_prefix("0x").unwrap_or(input);
    if s.is_empty() {
        return Err("seed is empty".to_string());
    }
    if !s.len().is_multiple_of(2) {
        return Err(format!(
            "seed has an odd number of hex digits ({})",
            s.len()
        ));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| format!("seed has a non-hex digit near {:?}", &s[i..i + 2]))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).unwrap()
    }

    #[test]
    fn minimal_args_take_one_dir() {
        let cli = parse(&["estrogen", "dist"]);
        assert_eq!(cli.dirs(), vec![PathBuf::from("dist")]);
        assert!(!cli.is_build());
        assert!(!cli.common().check);
        assert!(!cli.common().quiet);
        assert!(!cli.common().verbose);
    }

    #[test]
    fn several_dirs_are_accepted() {
        let cli = parse(&["estrogen", "dist", "aws/dist", "svg/jsdelivr"]);
        assert_eq!(cli.dirs().len(), 3);
    }

    #[test]
    fn no_dir_parses_with_an_empty_list() {
        // dirs is validated in the front end, not by clap, so both run modes can
        // share the flag set. An empty list is rejected at run time.
        let cli = parse(&["estrogen"]);
        assert!(cli.dirs().is_empty());
    }

    #[test]
    fn build_subcommand_takes_dirs_and_flags() {
        let cli = parse(&["estrogen", "build", "-c", "estrogen.toml", "dist"]);
        assert!(cli.is_build());
        assert_eq!(cli.dirs(), vec![PathBuf::from("dist")]);
        assert_eq!(cli.common().config, Some(PathBuf::from("estrogen.toml")));
    }

    #[test]
    fn watch_flag_parses() {
        let cli = parse(&["estrogen", "--watch", "dist"]);
        assert!(cli.common().watch);
        assert!(!parse(&["estrogen", "dist"]).common().watch);
    }

    #[test]
    fn short_and_long_flags_parse() {
        let cli = parse(&[
            "estrogen",
            "-c",
            "estrogen.toml",
            "--canary",
            "canary.json",
            "--names",
            "names.json",
            "--sourcemap",
            "maps",
            "--seed",
            "00ff",
            "-j",
            "4",
            "--check",
            "-v",
            "dist",
        ]);
        let c = cli.common();
        assert_eq!(c.config, Some(PathBuf::from("estrogen.toml")));
        assert_eq!(c.canary, Some(PathBuf::from("canary.json")));
        assert_eq!(c.names, Some(PathBuf::from("names.json")));
        assert_eq!(c.sourcemap, Some(PathBuf::from("maps")));
        assert_eq!(c.seed.as_deref(), Some("00ff"));
        assert_eq!(c.jobs, Some(4));
        assert!(c.check);
        assert!(c.verbose);
    }

    #[test]
    fn seed_overlays_into_options() {
        let cli = parse(&["estrogen", "--seed", "0xdeadbeef", "dist"]);
        let opts = cli.build_options().unwrap();
        assert_eq!(opts.seed, Some(vec![0xde, 0xad, 0xbe, 0xef]));
    }

    #[test]
    fn default_options_without_config() {
        let cli = parse(&["estrogen", "dist"]);
        let opts = cli.build_options().unwrap();
        assert!(opts.names.is_empty());
        assert!(opts.canaries.is_empty());
        assert!(opts.seed.is_none());
        assert!(!opts.check);
        assert!(opts.vendor.is_empty());
    }

    #[test]
    fn check_flag_sets_option() {
        let cli = parse(&["estrogen", "--check", "dist"]);
        let opts = cli.build_options().unwrap();
        assert!(opts.check);
    }

    #[test]
    fn jobs_overlays_into_options() {
        let cli = parse(&["estrogen", "-j", "2", "dist"]);
        let opts = cli.build_options().unwrap();
        assert_eq!(opts.jobs, 2);
    }

    #[test]
    fn bad_seed_is_rejected() {
        let cli = parse(&["estrogen", "--seed", "xyz", "dist"]);
        assert!(cli.build_options().is_err());
    }

    #[test]
    fn odd_length_seed_is_rejected() {
        assert!(parse_hex("abc").is_err());
    }

    #[test]
    fn good_hex_parses() {
        assert_eq!(parse_hex("00ff10").unwrap(), vec![0x00, 0xff, 0x10]);
    }
}
