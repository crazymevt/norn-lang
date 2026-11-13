//! I/O runtime checks that depend on timing, so each runs in a `norn`
//! process of its own with a known number of carriers (the shared suite
//! runner keeps every carrier busy with other suites).

use std::process::Command;

fn norn(threads: &str, expr: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_norn")).args(["-e", expr]).env("NORN_THREADS", threads).output().unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn the_blocking_pool_runs_calls_side_by_side() {
    // 16 calls blocking 300 ms each: 4.8 s one after another.
    let r = norn(
        "2",
        "(let [t0 (nano-time)
               r (mapv await (mapv (fn [_] (spawn-detached (%blocking-sleep 300))) (range 16)))]
           [(= r (vec (repeat 16 300))) (< (- (nano-time) t0) 2500000000)])",
    );
    assert_eq!(r, "[true true]");
}

#[test]
fn a_spare_carrier_runs_fibers_while_carriers_are_stuck_in_native_code() {
    // Both carriers block for 2 s in native code; the worker still finishes
    // first, on a spare carrier, which retires afterwards.
    let r = norn(
        "2",
        "(let [stalls (mapv (fn [_] (spawn-detached (%native-stall 2000))) (range 2))
               _ (sleep 50)
               worker (spawn-detached (reduce + (range 100000)))
               v (await worker)
               early (if (some (fn [f] (not (done? f))) stalls) true false)
               spares (:carrier-threads (%io-stats))]
           (mapv await stalls)
           (sleep 400)
           [v early (> spares 2) (:carrier-threads (%io-stats))])",
    );
    assert_eq!(r, "[4999950000 true true 2]");
}

#[test]
fn a_slow_fast_ffi_call_gets_a_spare_carrier() {
    // usleep declared as a fast (non-blocking) call: both carriers sit in C
    // for 1.5 s, and the worker still runs on a spare carrier.
    let r = norn(
        "2",
        r#"(do (require '[norn.ffi :as ffi])
               (ffi/defcfun slow-sleep (ffi/library :c) "usleep" [:uint] :int)
               (let [stalls (mapv (fn [_] (spawn-detached (slow-sleep 1500000))) (range 2))
                     _ (sleep 50)
                     worker (spawn-detached (reduce + (range 100000)))
                     v (await worker)
                     early (if (some (fn [f] (not (done? f))) stalls) true false)]
                 [v early (mapv await stalls)]))"#,
    );
    assert_eq!(r, "[4999950000 true [0 0]]");
}
