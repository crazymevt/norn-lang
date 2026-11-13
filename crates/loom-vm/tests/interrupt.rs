//! Its own test binary: INTERRUPT is process-global, so this must not run
//! in parallel with other tests (cargo runs test binaries one at a time).

use loom_vm::Vm;

#[test]
fn interrupt_stops_a_runaway_loop() {
    use std::sync::atomic::Ordering;
    let mut vm = Vm::new().unwrap();
    let t = std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(100));
        loom_vm::INTERRUPT.store(true, Ordering::Relaxed);
    });
    let r = vm.eval_str("(loop [] (recur))").map(|_| String::new()).unwrap_or_else(|e| e.msg);
    t.join().unwrap();
    assert!(r.contains("Interrupted"), "{r}");
}
