//! M4 done-check (second half): tier 0 (interpreter) and tier 1 (Cranelift
//! native code) agree on every test suite. Each suite runs in a fresh `norn`
//! process per tier, since the tier is chosen at start-up (NORN_TIER).

use std::process::Command;

fn run(suite: &str, tier: &str) -> String {
    let path = format!("{}/../../tests/norn/{suite}", env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(env!("CARGO_BIN_EXE_norn"))
        .arg("test")
        .arg(&path)
        .env("NORN_TIER", tier)
        .current_dir(format!("{}/../..", env!("CARGO_MANIFEST_DIR")))
        .output()
        .expect("run norn");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{suite} failed at tier {tier}:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    // The summary line, e.g. "1 tests, 251 checks: 251 passed, 0 failed, 0 errors".
    stdout.lines().rev().find(|l| l.contains(" checks: ")).unwrap_or("").to_owned()
}

#[test]
fn tier0_and_tier1_agree_on_every_suite() {
    for suite in ["core_test.norn", "conc_test.norn", "clos_test.norn", "cond_test.norn", "compiler_test.norn", "lib_test.norn", "test_test.norn", "syntax_test.norn"] {
        let t0 = run(suite, "0");
        let t1 = run(suite, "1");
        assert!(t0.ends_with(" 0 failed, 0 errors"), "{suite} tier 0: {t0}");
        assert_eq!(t0, t1, "{suite}: tiers disagree");
    }
}

/// M7 done-check: arithmetic that promotes past 62 bits (to bignums and
/// ratios) gives the same results at tier 0 and tier 1.
#[test]
fn promotion_past_62_bits_is_identical_at_both_tiers() {
    let code = r#"
(defn mix [n] (loop [i 0 acc 1 r 1/3] (if (< i n) (recur (inc i) (+ (* acc 3) i) (+ r (/ i 7))) [acc r])))
(defn edges [] [(+ 4611686018427387903 1) (- -4611686018427387904 1) (* 3037000500 3037000500)
                (bit-shift-left 1 80) (inc 4611686018427387903) (dec -4611686018427387904)
                (quot (* 99999999999 99999999999) 7) (mod (- (* 99999999999 99999999999)) 13)
                (/ (* 4611686018427387903 2) 4611686018427387903) (- (+ 4611686018427387903 5) 5)])
(dotimes [_ 3] (mix 100) (edges))
(prn [(mix 100) (edges) (reduce + (map (fn [i] (* i i i i i)) (range 2000)))])
"#;
    let run = |tier: &str| {
        let out = Command::new(env!("CARGO_BIN_EXE_norn"))
            .args(["-e", code])
            .env("NORN_TIER", tier)
            .output()
            .expect("run norn");
        assert!(out.status.success(), "tier {tier}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").to_owned()
    };
    let (t0, t1) = (run("0"), run("1"));
    assert!(t0.contains("4611686018427387904"), "{t0}");
    assert_eq!(t0, t1, "tiers disagree on promotion");
}

/// M7 done-check: lib/ and tests/ are formatted (so formatting them again
/// changes nothing) and lint-clean.
#[test]
fn the_tree_is_formatted_and_lint_clean() {
    let root = format!("{}/../..", env!("CARGO_MANIFEST_DIR"));
    for tool in [&["fmt", "--check", "lib", "tests", "bench"][..], &["lint", "lib", "tests", "bench"][..]] {
        let out = Command::new(env!("CARGO_BIN_EXE_norn")).args(tool).current_dir(&root).output().expect("run norn");
        assert!(out.status.success(), "norn {tool:?}:\n{}", String::from_utf8_lossy(&out.stdout));
    }
}
