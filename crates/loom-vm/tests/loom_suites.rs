//! Runs the `.loom` test suites under tests/loom/ and checks REPL-level
//! behaviour that the suites can't express (errors, printing).

use loom_rt::print::pr_str;
use loom_vm::Vm;

fn eval(vm: &mut Vm, src: &str) -> String {
    match vm.eval_str(src) {
        Ok(v) => pr_str(v, true),
        Err(e) => format!("ERROR: {}", e.msg),
    }
}

#[test]
fn core_suite() {
    let mut vm = Vm::new().unwrap();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/loom/core_test.loom");
    if let Err(e) = vm.load_file(path) {
        panic!("{e}");
    }
}

#[test]
fn printing_and_errors() {
    let mut vm = Vm::new().unwrap();
    vm.eval_str("(ns printing-test)").unwrap();
    let cases = [
        ("(list 1 \"two\" \\c :k 'sym 2.0 nil true)", "(1 \"two\" \\c :k sym 2.0 nil true)"),
        ("[(range 3) (rest [1 2 3]) ()]", "[(0 1 2) (2 3) ()]"),
        ("#{}", "#{}"),
        ("(str \"a\\nb\")", "\"a\\nb\""),
        ("##Inf", "##Inf"),
        ("(var map)", "#'loom.core/map"),
        ("(defn named [] 1)", "#'printing-test/named"),
        ("named", "#<fn printing-test/named>"),
        ("first", "#<primitive loom.core/first>"),
        ("(/ 1 0)", "ERROR: <input>:1: Divide by zero"),
        ("(undefined-thing)", "ERROR: <input>:1: Unable to resolve symbol: undefined-thing in this context"),
        ("(4611686018427387903 1)", "ERROR: <input>:1: Integer is not a function: 4611686018427387903"),
        ("(+ 4611686018427387903 1)", "ERROR: <input>:1: Integer overflow (bignums are not implemented yet)"),
        ("((fn [x] x))", "ERROR: <input>:1: Wrong number of args (0) passed to: #<fn fn>"),
        ("(loop [] (recur 1))", "ERROR: <input>:1: Mismatched argument count to recur, expected: 0 args, got: 1"),
        ("(fn [] (inc (recur)))", "ERROR: <input>:1: Can only recur from tail position"),
    ];
    for (src, want) in cases {
        assert_eq!(eval(&mut vm, src), want, "evaluating {src}");
    }
}

#[test]
fn deep_recursion_grows_the_heap_stack() {
    let mut vm = Vm::new().unwrap();
    vm.eval_str("(ns deep-test)").unwrap();
    let r = eval(&mut vm, "(defn deep [n] (if (zero? n) 0 (inc (deep (dec n))))) (deep 200000)");
    assert_eq!(r, "200000");
    // Runaway recursion is an error, not a crash.
    let r = eval(&mut vm, "(defn forever [n] (inc (forever n))) (forever 1)");
    assert!(r.contains("Stack overflow"), "{r}");
}
