//! `norn-ext-demo`: the `norn` command plus a demonstration extension, for
//! the M7.5 done-check (crates/norn/tests/ext.rs). It shows the three things
//! an extension like norn-ui needs:
//!
//! - primitives in a namespace of its own (`norn.demo`);
//! - native objects behind handles (a counter);
//! - the process's first thread, which posts events to a Norn channel
//!   through a `Port` (as a UI event loop would).

use norn_vm::ext::{Extension, Port, build, handle, with_native};
use norn_vm::prims::PrimDef;
use norn_vm::{Error, Result, Vm};
use norn_rt::Value;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};

/// The channel the main thread feeds, set by `(norn.demo/subscribe ch n)`.
static PORT: Mutex<Option<(Port, i64)>> = Mutex::new(None);
static POSTED: AtomicI64 = AtomicI64::new(0);

fn subscribe(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let n = if a[1].is_fixnum() { a[1].as_fixnum() } else { return Err(Error::new("subscribe: n must be an integer")) };
    *PORT.lock().unwrap() = Some((Port::new(a[0])?, n));
    Ok(Value::NIL)
}

fn counter(vm: &mut Vm, _: &[Value]) -> Result<Value> {
    let info = build::map(&mut vm.m, &[]);
    handle(&mut vm.m, "counter", Box::new(0i64), info)
}

fn bump(_: &mut Vm, a: &[Value]) -> Result<Value> {
    with_native::<i64, _>(a[0], "counter", |n| {
        *n += 1;
        Value::fix(*n)
    })
}

fn posted(_: &mut Vm, _: &[Value]) -> Result<Value> {
    Ok(Value::fix(POSTED.load(Ordering::SeqCst)))
}

fn main() {
    norn_vm::ext::register(vec![Extension {
        name: "demo",
        ns: "norn.demo",
        prims: vec![
            PrimDef { name: "subscribe", min: 2, max: Some(2), f: subscribe },
            PrimDef { name: "counter", min: 0, max: Some(0), f: counter },
            PrimDef { name: "bump!", min: 1, max: Some(1), f: bump },
            PrimDef { name: "posted", min: 0, max: Some(0), f: posted },
        ],
        libs: vec![("norn.demo.lib", "(ns norn.demo.lib)\n(defn greet [s] (str \"hello \" s))\n")],
    }])
    .expect("register the demo extension");

    // The "event loop": on the first thread, post n events to the
    // subscribed channel, retrying while it is full, then close it.
    norn::run_with_main_thread(|_norn| {
        let main = std::thread::current().name() == Some("main");
        loop {
            let job = PORT.lock().unwrap().take();
            let Some((port, n)) = job else {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            };
            for i in 0..n {
                while !port.post(|m| {
                    let ev = build::keyword(m, "tick");
                    build::map(m, &[("event", ev), ("n", build::int(i)), ("main", Value::bool(main))])
                }) {
                    std::thread::sleep(std::time::Duration::from_micros(200));
                }
                POSTED.fetch_add(1, Ordering::SeqCst);
            }
            port.close();
        }
    })
}
