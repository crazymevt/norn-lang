//! M4 done-check (second half): tier 0 (interpreter) and tier 1 (Cranelift
//! native code) agree on every test suite. Each suite runs in a fresh `norn`
//! process per tier, since the tier is chosen at start-up (NORN_TIER).

use std::process::Command;

fn run(suite: &str, tier: &str) -> String {
    let path = format!("{}/../../tests/norn/{suite}", env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(env!("CARGO_BIN_EXE_norn"))
        .arg(&path)
        .env("NORN_TIER", tier)
        .current_dir(format!("{}/../..", env!("CARGO_MANIFEST_DIR")))
        .output()
        .expect("run norn");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{suite} failed at tier {tier}:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    // The summary line, e.g. "core_test: 101 checks, 0 failures".
    stdout.lines().find(|l| l.contains(" checks, ")).unwrap_or("").to_owned()
}

#[test]
fn tier0_and_tier1_agree_on_every_suite() {
    for suite in ["core_test.norn", "conc_test.norn", "clos_test.norn", "cond_test.norn", "compiler_test.norn"] {
        let t0 = run(suite, "0");
        let t1 = run(suite, "1");
        assert!(t0.ends_with(" 0 failures"), "{suite} tier 0: {t0}");
        assert_eq!(t0, t1, "{suite}: tiers disagree");
    }
}
