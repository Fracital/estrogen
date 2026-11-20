//! End-to-end check that the proxy transform produces running code and that the
//! app-shared global veil rebuilds the real name.
//!
//! The proxy path renames proxy names and veils the two app-shared globals into
//! a computed lookup rebuilt from an encrypted pool. This runs estrogen's output in
//! `node` and asserts the veiled `$scramjetController` still resolves to the real
//! global at runtime, which only holds if the pool, the decoder and the veil
//! agree. If `node` is absent it prints a clear SKIP rather than passing as if it
//! had verified anything.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use estrogen_core::{Obfuscator, Options};

fn node_available(test: &str) -> bool {
    match Command::new("node").arg("--version").output() {
        Ok(out) if out.status.success() => true,
        _ => {
            eprintln!("SKIP: node not found on PATH, cannot verify {test}");
            false
        }
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("estrogen-proxy-node-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(p.join("vendor")).unwrap();
    p
}

#[test]
fn veiled_shared_global_resolves_in_node() {
    if !node_available("veiled_shared_global_resolves_in_node") {
        return;
    }
    let dir = tmp_dir("veil");
    let vendor_file = dir.join("vendor").join("ctl.js");
    // A proxy vendor file: the shared global is read, its method called, and the
    // result published on a plain (non-proxy) global so the harness can read it.
    let source = "globalThis.OUT = $scramjetController.load();\n";
    std::fs::write(&vendor_file, source).unwrap();

    let opts = Options {
        seed: Some(b"proxy-node-seed".to_vec()),
        ..Options::default()
    };
    let obf = Obfuscator::new(opts).unwrap();
    let report = obf.obfuscate_dir(&dir).unwrap();
    assert!(report.obfuscated() >= 1, "vendor file was not obfuscated");

    let obfuscated = std::fs::read_to_string(&vendor_file).unwrap();
    // The plaintext shared name must be gone.
    assert!(
        !obfuscated.contains("$scramjetController"),
        "shared name survived in the output"
    );

    // Stub the real global, run the obfuscated chunk, print what it resolved to.
    let harness = format!(
        "globalThis['$scramjetController'] = {{ load: () => 'RESOLVED' }};\n{obfuscated}\nconsole.log(globalThis.OUT);\n"
    );
    let mut script = dir.join("run.js");
    script = script.with_extension("mjs");
    {
        let mut f = std::fs::File::create(&script).unwrap();
        f.write_all(harness.as_bytes()).unwrap();
    }
    let out = Command::new("node").arg(&script).output().unwrap();
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        "RESOLVED",
        "the veiled global did not rebuild the real name; stdout={stdout}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
