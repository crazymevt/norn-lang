//! M9b part 3: `skein test --coverage` and `--lcov`, on a project of its own,
//! at both tiers. Coverage changes what the expander emits, so the project
//! also checks that what it touches still works: a function with a
//! contract, letrec (letfn), a loop's recur, and a macro's expansion.

use std::path::{Path, PathBuf};
use std::process::Command;

fn project(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skein-coverage-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("src/geo")).unwrap();
    std::fs::create_dir_all(d.join("tests")).unwrap();
    std::fs::write(d.join("skein.edn"), "{:name geo :paths [\"src\"]}\n").unwrap();
    std::fs::write(
        d.join("src/geo/shapes.skn"),
        r#"(ns geo.shapes)

(defn area
  "An area."
  [kind x]
  (case kind
    :square (* x x)
    :circle (* 3 x x)
    (throw (ex-info "no such shape" {:kind kind}))))

(defn ^Integer twice [^Integer n] (* 2 n))

(defn evens [xs]
  (letfn [(ev? [n] (zero? (mod n 2)))]
    (filter ev? xs)))

(defn total [n]
  (loop [i 0 acc 0]
    (if (< i n)
      (recur (inc i) (+ acc i))
      acc)))

(defn unused [x]
  (when (pos? x)
    (println "never")))
"#,
    )
    .unwrap();
    std::fs::write(
        d.join("tests/shapes_test.skn"),
        r#"(ns shapes-test (:require [skein.test :refer [deftest is]] [geo.shapes :as s]))
(deftest shapes
  (is (= 25 (s/area :square 5)))
  (is (= [2 4] (s/evens [1 2 3 4])))
  (is (= 10 (s/total 5)))
  (is (= 6 (s/twice 3)))
  (is (thrown? ContractError (s/twice "3")) "the contract is still checked"))
"#,
    )
    .unwrap();
    d
}

fn skein_test(dir: &Path, tier: &str, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_skein")).arg("test").args(args).current_dir(dir).env("SKEIN_TIER", tier).output().unwrap();
    (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

#[test]
fn skein_test_reports_the_lines_that_ran() {
    for tier in ["0", "1"] {
        let d = project(&format!("t{tier}"));
        let (ok, out) = skein_test(&d, tier, &["--coverage", "--lcov", "cov.info"]);
        assert!(ok, "tier {tier}:\n{out}");
        // (paths with /, as on Windows too once its \ are turned)
        let out = out.replace('\\', "/");
        assert!(out.contains("1 test, 5 checks: 5 passed"), "tier {tier}:\n{out}");
        // Counted: the defs (run as the module loads) and the code that ran;
        // missed: the other case branches and unused's body.
        let line = out.lines().find(|l| l.trim_start().starts_with("src/geo/shapes.skn")).unwrap_or_else(|| panic!("{out}"));
        assert!(line.contains("missed: 8-9, 24-25"), "tier {tier}: {line}");
        assert!(out.contains("in 1 file"), "only the project's own file, not its test: {out}");
        let info = std::fs::read_to_string(d.join("cov.info")).unwrap().replace('\\', "/");
        // (absolute: /… or, on Windows, C:/…)
        let sf = info.strip_prefix("TN:\nSF:").unwrap_or_else(|| panic!("{info}"));
        assert!(sf.starts_with('/') || sf.as_bytes().get(1) == Some(&b':'), "{info}");
        assert!(info.contains("/src/geo/shapes.skn\nDA:1,"), "{info}");
        assert!(info.contains("DA:19,6\n"), "the loop's line ran six times: {info}");
        assert!(info.contains("DA:8,0\n") && info.ends_with("end_of_record\n"), "{info}");
    }
}

#[test]
fn without_coverage_nothing_is_counted() {
    let d = project("off");
    let (ok, out) = skein_test(&d, "0", &[]);
    assert!(ok && !out.contains("coverage:"), "{out}");
}
