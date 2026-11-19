//! Runs the `.skn` test suites under tests/skein/ and checks REPL-level
//! behaviour that the suites can't express (errors, printing).
//!
//! All tests share one runtime per process (carriers + skein.core), and each
//! works in its own namespace.

use skein_vm::{RootTask, start};

/// Run one suite with skein.test; fail with its report if any check failed.
fn suite(name: &str) {
    let skein = start().unwrap();
    // (escaped as a Skein string: a Windows path is full of \)
    let path = format!("{}/../../tests/skein/{name}", env!("CARGO_MANIFEST_DIR")).replace('\\', "\\\\");
    let call = format!(
        r#"(do (require 'skein.test)
             (let [r (skein.test/run-files ["{path}"] :serial true)
                   t (:totals r)]
               (if (zero? (+ (:fail t) (:error t))) :ok (do (print (skein.test/%human r)) :failed))))"#
    );
    match skein.run(RootTask::Eval(call), "user").result {
        Ok(v) if v == ":ok" => {}
        Ok(_) => panic!("{name} failed (the report is above)"),
        Err(e) => panic!("{e}"),
    }
}

fn eval(src: &str, ns: &str) -> String {
    match start().unwrap().eval(src, ns) {
        Ok(v) => v,
        Err(e) => format!("ERROR: {}", e.msg),
    }
}

#[test]
fn core_suite() {
    suite("core_test.skn");
}

#[test]
fn conc_suite() {
    suite("conc_test.skn");
}

#[test]
fn clos_suite() {
    suite("clos_test.skn");
}

#[test]
fn cond_suite() {
    suite("cond_test.skn");
}

#[test]
fn compiler_suite() {
    suite("compiler_test.skn");
}

#[test]
fn lib_suite() {
    suite("lib_test.skn");
}

#[test]
fn test_suite() {
    suite("test_test.skn");
}

#[test]
fn syntax_suite() {
    suite("syntax_test.skn");
}

#[test]
fn expander_suite() {
    suite("expander_test.skn");
}

#[test]
fn module_suite() {
    suite("module_test.skn");
}

#[test]
fn phase_suite() {
    suite("phase_test.skn");
}

#[test]
fn cache_suite() {
    suite("cache_test.skn");
}

#[test]
fn pattern_suite() {
    suite("pattern_test.skn");
}

#[test]
fn deps_suite() {
    suite("deps_test.skn");
}

#[test]
fn verify_suite() {
    suite("verify_test.skn");
}

#[test]
fn http_suite() {
    suite("http_test.skn");
}

#[test]
fn errors_suite() {
    suite("errors_test.skn");
}

#[test]
fn later_suite() {
    suite("later_test.skn");
}

#[test]
fn deadlock_suite() {
    suite("deadlock_test.skn");
}

#[test]
fn match_suite() {
    suite("match_test.skn");
}

#[test]
fn contracts_suite() {
    suite("contracts_test.skn");
}

#[test]
fn mop_suite() {
    suite("mop_test.skn");
}

#[test]
fn printing_and_errors() {
    let ns = "printing-test";
    let cases = [
        ("(list 1 \"two\" \\c :k 'sym 2.0 nil true)", "(1 \"two\" \\c :k sym 2.0 nil true)"),
        ("[(range 3) (rest [1 2 3]) ()]", "[(0 1 2) (2 3) ()]"),
        ("#{}", "#{}"),
        ("(str \"a\\nb\")", "\"a\\nb\""),
        ("##Inf", "##Inf"),
        ("(var map)", "#'skein.core/map"),
        ("(defn named [] 1)", "#'printing-test/named"),
        ("named", "#<fn printing-test/named>"),
        ("first", "#<primitive skein.core/first>"),
        ("(atom [1])", "#<atom [1]>"),
        ("(/ 1 0)", "ERROR: <string>:1: Divide by zero"),
        ("(undefined-thing)", "ERROR: <string>:1: Unable to resolve symbol: undefined-thing in this context"),
        ("(4611686018427387903 1)", "ERROR: <string>:1: Integer is not a function: 4611686018427387903"),
        ("(+ 4611686018427387903 1)", "4611686018427387904"),
        ("[1/3 (/ 6 4) 100000000000000000000]", "[1/3 3/2 100000000000000000000]"),
        ("((fn [x] x))", "ERROR: <string>:1: Wrong number of args (0) passed to: #<fn fn>"),
        ("(loop [] (recur 1))", "ERROR: <string>:1: Mismatched argument count to recur, expected: 0 args, got: 1"),
        ("(fn [] (inc (recur)))", "ERROR: <string>:1: Can only recur from tail position"),
        ("(defmacro waits [] (sleep 1) :waited) (waits)", ":waited"),
        ("(defmacro breaks [] (/ 1 0)) (breaks)", "ERROR: <string>:1: Divide by zero"),
        ("(", "ERROR: <string>:1: EOF while reading, expected )"),
    ];
    for (src, want) in cases {
        assert_eq!(eval(src, ns), want, "evaluating {src}");
    }
}

#[test]
fn namespaces_persist_per_root() {
    let skein = start().unwrap();
    let done = skein.run(RootTask::Eval("(ns ns-switch-test) (def x 1)".into()), "user");
    assert_eq!(done.ns, "ns-switch-test");
    assert_eq!(eval("x", "ns-switch-test"), "1");
}

#[test]
fn deep_recursion_grows_the_heap_stack() {
    let ns = "deep-test";
    let r = eval("(defn deep [n] (if (zero? n) 0 (inc (deep (dec n))))) (deep 200000)", ns);
    assert_eq!(r, "200000");
    // Runaway recursion is an error, not a crash.
    let r = eval("(defn forever [n] (inc (forever n))) (forever 1)", ns);
    assert!(r.contains("Stack overflow"), "{r}");
}

#[test]
fn interrupt_stops_a_runaway_root_fiber() {
    let skein = start().unwrap();
    let t = skein.submit(RootTask::Eval("(loop [] (recur))".into()), "interrupt-test");
    std::thread::sleep(std::time::Duration::from_millis(100));
    skein.interrupt(t);
    let r = skein.wait(t).result;
    assert!(matches!(&r, Err(e) if e.msg == "Interrupted"), "{r:?}");
    // And a blocked one. (On a channel something holds: on one nothing
    // did, a collection meanwhile would find it deadlocked first.)
    let t = skein.submit(RootTask::Eval("(do (def held (chan)) (take! held))".into()), "interrupt-test");
    std::thread::sleep(std::time::Duration::from_millis(50));
    skein.interrupt(t);
    let r = skein.wait(t).result;
    assert!(matches!(&r, Err(e) if e.msg == "Interrupted"), "{r:?}");
}

#[test]
fn actor_suite() {
    suite("actor_test.skn");
}

#[test]
fn stm_suite() {
    suite("stm_test.skn");
}

#[test]
fn ffi_suite() {
    suite("ffi_test.skn");
}

#[test]
fn sqlite_suite() {
    suite("sqlite_test.skn");
}

#[test]
fn pty_suite() {
    suite("pty_test.skn");
}

#[test]
fn bin_suite() {
    suite("bin_test.skn");
}

#[test]
fn small_libs_suite() {
    suite("small_libs_test.skn");
}

#[test]
fn xml_suite() {
    suite("xml_test.skn");
}

#[test]
fn data_formats_suite() {
    suite("data_formats_test.skn");
}

#[test]
fn io_suite() {
    suite("io_test.skn");
}

#[test]
fn property_suite() {
    suite("property_test.skn");
}

#[test]
fn cli_suite() {
    suite("cli_test.skn");
}

#[test]
fn time_suite() {
    suite("time_test.skn");
}

#[test]
fn tags_suite() {
    suite("tags_test.skn");
}

#[test]
fn terminal_suite() {
    suite("terminal_test.skn");
}
