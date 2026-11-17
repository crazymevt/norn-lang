//! The lang-tests (lang-tests/: five programs written as Norn's users write
//! them) as a check on this build: each project's tests at tier 0, tier 1
//! and with everything compiled from its first call, the programs' outputs
//! on fixed inputs against their recording, and the standard library's
//! coverage (lang-tests/run.norn). About a minute, so it runs only when
//! asked: `NORN_LANG_TESTS=1 cargo test --release -p norn --test lang_tests`.

use std::path::Path;
use std::process::Command;

#[test]
fn lang_tests_pass() {
    if std::env::var("NORN_LANG_TESTS").as_deref() != Ok("1") {
        return eprintln!("skipped: NORN_LANG_TESTS=1 runs it (about a minute)");
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let o = Command::new(env!("CARGO_BIN_EXE_norn"))
        .arg("lang-tests/run.norn")
        .current_dir(&root)
        .output()
        .expect("norn");
    let out = String::from_utf8_lossy(&o.stdout);
    eprintln!("{out}");
    assert!(o.status.success(), "--- out:\n{out}\n--- err:\n{}", String::from_utf8_lossy(&o.stderr));
}
