//! Argument parsing and option assembly for the `estrogen` binary.
//!
//! [`Cli`] is the clap-derived argument set. [`Cli::build_options`] loads the
//! config file when one is given, then overlays the flags that override it
//! (`--canary`, `--names`, `--sourcemap`, `--seed`, `--jobs`, `--check`) and
//! returns the [`Options`] the run uses.

use std::path::PathBuf;

use clap::Parser;
use estrogen_core::options::{load_name_map, CanaryList};
use estrogen_core::{Error, Options};

/// estrogen: a post-build web obfuscator. It rewrites a built directory in place.
#[derive(Debug, Parser)]
#[command(name = "estrogen", version, about, long_about = None)]
pub struct Cli {
    /// One or more built directories to rewrite in place.
    #[arg(value_name = "DIR", required = true)]
    pub dirs: Vec<PathBuf>,

    /// estrogen config file (TOML): per-site names, canary list, options.
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
    /// The directories to process.
    pub fn dirs(&self) -> Vec<PathBuf> {
        self.dirs.clone()
    }

    /// Assemble the run options: load the config, then overlay the flags.
    pub fn build_options(&self) -> Result<Options, Error> {
        let mut options = match &self.config {
            Some(path) => Options::from_config(path)?,
            None => Options::default(),
        };

        if let Some(path) = &self.canary {
            options.canaries = CanaryList::from_json_file(path)?.canaries;
        }
        if let Some(path) = &self.names {
            options.names = load_name_map(path)?;
        }
        if let Some(dir) = &self.sourcemap {
            options.sourcemap_dir = Some(dir.clone());
        }
        if let Some(hex) = &self.seed {
            options.seed = Some(parse_hex(hex).map_err(|message| Error::Config {
                path: PathBuf::from("--seed"),
                message,
            })?);
        }
        if let Some(jobs) = self.jobs {
            if jobs > 0 {
                options.jobs = jobs;
            }
        }
        if self.check {
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
        assert_eq!(cli.dirs, vec![PathBuf::from("dist")]);
        assert!(!cli.check);
        assert!(!cli.quiet);
        assert!(!cli.verbose);
    }

    #[test]
    fn several_dirs_are_accepted() {
        let cli = parse(&["estrogen", "dist", "aws/dist", "svg/jsdelivr"]);
        assert_eq!(cli.dirs.len(), 3);
    }

    #[test]
    fn dir_is_required() {
        assert!(Cli::try_parse_from(["estrogen"]).is_err());
    }

    #[test]
    fn watch_flag_parses() {
        let cli = parse(&["estrogen", "--watch", "dist"]);
        assert!(cli.watch);
        assert!(!parse(&["estrogen", "dist"]).watch);
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
        assert_eq!(cli.config, Some(PathBuf::from("estrogen.toml")));
        assert_eq!(cli.canary, Some(PathBuf::from("canary.json")));
        assert_eq!(cli.names, Some(PathBuf::from("names.json")));
        assert_eq!(cli.sourcemap, Some(PathBuf::from("maps")));
        assert_eq!(cli.seed.as_deref(), Some("00ff"));
        assert_eq!(cli.jobs, Some(4));
        assert!(cli.check);
        assert!(cli.verbose);
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
