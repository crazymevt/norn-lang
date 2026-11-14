//! Runs the `.norn` test suites under tests/norn/ and checks REPL-level
//! behaviour that the suites can't express (errors, printing).
//!
//! All tests share one runtime per process (carriers + norn.core), and each
//! works in its own namespace.

use norn_vm::{RootTask, start};

/// Run one suite with norn.test; fail with its report if any check failed.
fn suite(name: &str) {
    let norn = start().unwrap();
    let path = format!("{}/../../tests/norn/{name}", env!("CARGO_MANIFEST_DIR"));
    let call = format!(
        r#"(do (require 'norn.test)
             (let [r (norn.test/run-files ["{path}"] :serial true)
                   t (:totals r)]
               (if (zero? (+ (:fail t) (:error t))) :ok (do (print (norn.test/%human r)) :failed))))"#
    );
    match norn.run(RootTask::Eval(call), "user").result {
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
    suite("core_test.norn");
}

#[test]
fn conc_suite() {
    suite("conc_test.norn");
}

#[test]
fn clos_suite() {
    suite("clos_test.norn");
}

#[test]
fn cond_suite() {
    suite("cond_test.norn");
}

#[test]
fn compiler_suite() {
    suite("compiler_test.norn");
}

#[test]
fn lib_suite() {
    suite("lib_test.norn");
}

#[test]
fn test_suite() {
    suite("test_test.norn");
}

#[test]
fn syntax_suite() {
    suite("syntax_test.norn");
}

#[test]
fn expander_suite() {
    suite("expander_test.norn");
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
        ("(var map)", "#'norn.core/map"),
        ("(defn named [] 1)", "#'printing-test/named"),
        ("named", "#<fn printing-test/named>"),
        ("first", "#<primitive norn.core/first>"),
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
    let norn = start().unwrap();
    let done = norn.run(RootTask::Eval("(ns ns-switch-test) (def x 1)".into()), "user");
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
    let norn = start().unwrap();
    let t = norn.submit(RootTask::Eval("(loop [] (recur))".into()), "interrupt-test");
    std::thread::sleep(std::time::Duration::from_millis(100));
    norn.interrupt(t);
    let r = norn.wait(t).result;
    assert!(matches!(&r, Err(e) if e.msg == "Interrupted"), "{r:?}");
    // And a blocked one.
    let t = norn.submit(RootTask::Eval("(take! (chan))".into()), "interrupt-test");
    std::thread::sleep(std::time::Duration::from_millis(50));
    norn.interrupt(t);
    let r = norn.wait(t).result;
    assert!(matches!(&r, Err(e) if e.msg == "Interrupted"), "{r:?}");
}
