//! The watch loop for `estrogen --watch`.
//!
//! estrogen rewrites a built directory in place and is not idempotent, but it
//! skips a file that already carries the marker. So watching is simple: the
//! bundler rebuilds the dist, and estrogen re-runs its pass on a debounced
//! change, obfuscating the new chunks and skipping the ones it already did. A
//! pass triggered by estrogen's own writes finds everything marked and does
//! nothing, so the loop settles instead of re-wrapping its output.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use estrogen_core::{Obfuscator, Report};
use notify_debouncer_mini::new_debouncer;
use notify_debouncer_mini::notify::RecursiveMode;

/// How long a burst of writes is coalesced before a run. A bundler writes many
/// files per rebuild, so a quiet period keeps estrogen to one pass per rebuild.
pub const DEBOUNCE: Duration = Duration::from_millis(250);

/// How often the loop wakes to check the stop flag while no events arrive.
const POLL: Duration = Duration::from_millis(100);

/// Run one pass over `dirs`. `build` vendors the configured inputs into each
/// dist, then obfuscates; otherwise it obfuscates what is already there.
pub fn run_pass(obf: &Obfuscator, dirs: &[PathBuf], is_build: bool) -> Result<Report, String> {
    if is_build {
        let mut combined = Report::new(dirs.to_vec());
        for dir in dirs {
            combined.absorb(obf.build(dir).map_err(|e| e.to_string())?);
        }
        Ok(combined)
    } else {
        obf.obfuscate_dirs(dirs).map_err(|e| e.to_string())
    }
}

/// Watch `watch_paths`, re-running a pass on each debounced change. It runs one
/// pass up front, then blocks until `stop` is set, which the process does on
/// Ctrl-C by exiting. `on_pass` receives each pass result for reporting. A
/// failed pass is reported and the watch continues, so a transient parse error
/// mid-rebuild does not end the session.
pub fn watch<F: Fn(&Result<Report, String>)>(
    obf: &Obfuscator,
    dirs: &[PathBuf],
    is_build: bool,
    watch_paths: &[PathBuf],
    debounce: Duration,
    stop: &AtomicBool,
    on_pass: F,
) -> Result<(), String> {
    // Process what is already on disk before watching, so the first run does not
    // wait for a change.
    on_pass(&run_pass(obf, dirs, is_build));

    let (tx, rx) = mpsc::channel();
    let mut debouncer =
        new_debouncer(debounce, tx).map_err(|e| format!("could not start the watcher: {e}"))?;
    for path in watch_paths {
        debouncer
            .watcher()
            .watch(path, RecursiveMode::Recursive)
            .map_err(|e| format!("cannot watch {}: {e}", path.display()))?;
    }

    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(POLL) {
            Ok(Ok(_events)) => on_pass(&run_pass(obf, dirs, is_build)),
            Ok(Err(e)) => eprintln!("estrogen: watch error: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

/// The paths to watch: each dist that exists, plus, in build mode, the base
/// directory of each `[[vendor]]` `from` source, so editing a vendored input
/// re-triggers the build.
pub fn watch_paths(obf: &Obfuscator, dirs: &[PathBuf], is_build: bool) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = dirs.iter().filter(|d| d.exists()).cloned().collect();
    if is_build {
        for input in &obf.options().vendor {
            let base = vendor_base(&input.from);
            if base.exists() && !paths.contains(&base) {
                paths.push(base);
            }
        }
    }
    paths
}

/// The directory to watch for a `from` pattern: the part before the first glob
/// character, as a directory (its parent when it names a file).
fn vendor_base(from: &str) -> PathBuf {
    let first_glob = from.find(['*', '?', '[']).unwrap_or(from.len());
    let head = Path::new(&from[..first_glob]);
    if head.is_dir() {
        head.to_path_buf()
    } else {
        head.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use estrogen_core::Options;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering::Relaxed;
    use std::time::Instant;
    use std::{fs, thread};

    fn tmp_dir(tag: &str) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "estrogen-watch-{}-{}-{tag}",
            std::process::id(),
            NEXT.fetch_add(1, Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn run_pass_obfuscates_new_files_and_skips_marked_ones() {
        let dir = tmp_dir("pass");
        fs::write(dir.join("fresh.js"), "const a = 'a secret value';\n").unwrap();
        let marked = format!("/*{}*/\nconst b = 'kept';\n", estrogen_core::format::MARKER);
        fs::write(dir.join("done.js"), &marked).unwrap();

        let obf = Obfuscator::new(Options::default()).unwrap();
        run_pass(&obf, std::slice::from_ref(&dir), false).unwrap();

        let fresh = fs::read_to_string(dir.join("fresh.js")).unwrap();
        assert!(fresh.contains(estrogen_core::format::MARKER));
        assert!(!fresh.contains("a secret value"));
        // The already-marked file is untouched (skipped, not re-wrapped).
        assert_eq!(fs::read_to_string(dir.join("done.js")).unwrap(), marked);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn watch_runs_an_initial_pass_then_stops_when_asked() {
        let dir = tmp_dir("init");
        fs::write(dir.join("app.js"), "const a = 'x';\n").unwrap();
        let obf = Obfuscator::new(Options::default()).unwrap();
        let dirs = vec![dir.clone()];
        let paths = vec![dir.clone()];
        let stop = AtomicBool::new(false);
        let count = AtomicUsize::new(0);

        thread::scope(|s| {
            let h = s.spawn(|| {
                watch(
                    &obf,
                    &dirs,
                    false,
                    &paths,
                    Duration::from_millis(100),
                    &stop,
                    |_| {
                        count.fetch_add(1, Relaxed);
                    },
                )
            });
            // The initial pass runs before the loop, so it has happened by now.
            thread::sleep(Duration::from_millis(300));
            stop.store(true, Relaxed);
            h.join().unwrap().unwrap();
        });

        assert!(count.load(Relaxed) >= 1, "the initial pass did not run");
        // The initial pass obfuscated the file.
        assert!(fs::read_to_string(dir.join("app.js"))
            .unwrap()
            .contains(estrogen_core::format::MARKER));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn watch_reruns_when_a_new_file_appears() {
        let dir = tmp_dir("rerun");
        fs::write(dir.join("first.js"), "const a = 'alpha';\n").unwrap();
        let obf = Obfuscator::new(Options::default()).unwrap();
        let dirs = vec![dir.clone()];
        let paths = vec![dir.clone()];
        let stop = AtomicBool::new(false);
        let count = AtomicUsize::new(0);

        thread::scope(|s| {
            s.spawn(|| {
                watch(
                    &obf,
                    &dirs,
                    false,
                    &paths,
                    Duration::from_millis(120),
                    &stop,
                    |_| {
                        count.fetch_add(1, Relaxed);
                    },
                )
            });
            // Let the initial pass finish and the watcher attach.
            thread::sleep(Duration::from_millis(400));
            fs::write(dir.join("second.js"), "const b = 'bravo';\n").unwrap();

            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                let seen = fs::read_to_string(dir.join("second.js"))
                    .map(|t| t.contains(estrogen_core::format::MARKER))
                    .unwrap_or(false);
                if seen || Instant::now() > deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            stop.store(true, Relaxed);
        });

        let second = fs::read_to_string(dir.join("second.js")).unwrap();
        assert!(
            second.contains(estrogen_core::format::MARKER),
            "the watcher did not obfuscate the new file"
        );
        assert!(
            count.load(Relaxed) >= 2,
            "expected the initial pass and at least one change pass"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn vendor_base_is_the_dir_before_a_glob() {
        let dir = tmp_dir("base");
        fs::create_dir_all(dir.join("pkg/dist")).unwrap();
        let from = format!("{}/pkg/dist/*.js", dir.display());
        assert_eq!(vendor_base(&from), dir.join("pkg/dist"));
        fs::remove_dir_all(&dir).ok();
    }
}
