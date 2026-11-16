//! Channels (CSP, design.md §6.4) and the blocking operations built on the
//! scheduler.
//!
//! A channel is a heap object guarded by a spinlock in its first word. It
//! holds an optional ring buffer and two FIFO queues of waiters (takers and
//! putters), each kept as a pair of lists. Unbuffered channels hand values
//! directly from putter to taker (rendezvous).
//!
//! Waiters whose claim was already taken (cancelled, or an `alts!` that fired
//! elsewhere) stay in the queues and are skipped when popped.

use crate::sched::{deliver, park_on};
use crate::vm::{Error, Vm};
use norn_rt::conc::{SpinGuard, chan, claim_new, spin_lock, try_claim, waiter, waiter_new};
use norn_rt::object::cons;
use norn_rt::print::pr_str;
use norn_rt::runtime::rt;
use norn_rt::vector::vec_from_slice;
use norn_rt::{Kind, Mutator, Value};

type Result<T> = std::result::Result<T, Error>;

fn expect_chan(c: Value, op: &str) -> Result<Value> {
    if c.is(Kind::Chan) {
        Ok(c)
    } else {
        Err(Error::of("TypeError", format!("{op} expects a channel, got {}: {}", c.type_name(), pr_str(c, true))))
    }
}

// ---- waiter queues ------------------------------------------------------------------

fn q_push(m: &mut Mutator, c: Value, back: usize, w: Value) {
    let l = cons(m, w, c.field(back));
    c.set_field(back, l);
}

fn q_pop(m: &mut Mutator, c: Value, front: usize, back: usize) -> Option<Value> {
    let mut f = c.field(front);
    if f.is_nil() {
        let mut b = c.field(back);
        if b.is_nil() {
            return None;
        }
        // Reverse the back list into the front list (amortised O(1)).
        while b.is_cons() {
            f = cons(m, b.car(), f);
            b = b.cdr();
        }
        c.set_field(back, Value::NIL);
    }
    c.set_field(front, f.cdr());
    Some(f.car())
}

/// Pop waiters until one whose claim we win.
fn q_claim(m: &mut Mutator, c: Value, front: usize, back: usize) -> Option<Value> {
    while let Some(w) = q_pop(m, c, front, back) {
        if try_claim(w.field(waiter::CLAIM)) {
            return Some(w);
        }
    }
    None
}

fn takers(m: &mut Mutator, c: Value) -> Option<Value> {
    q_claim(m, c, chan::TAKERS_FRONT, chan::TAKERS_BACK)
}

fn putters(m: &mut Mutator, c: Value) -> Option<Value> {
    q_claim(m, c, chan::PUTTERS_FRONT, chan::PUTTERS_BACK)
}

// ---- operations with the lock held ------------------------------------------------------

/// Try to put without blocking. `None`: would block.
fn try_put_locked(m: &mut Mutator, c: Value, v: Value) -> Option<bool> {
    if c.word(chan::CLOSED) != 0 {
        return Some(false);
    }
    if let Some(w) = takers(m, c) {
        deliver(w, v, c);
        return Some(true);
    }
    let (word, len) = (c.word(chan::CAP), c.word(chan::LEN));
    let cap = word & chan::CAP_MASK;
    let buf = c.field(chan::BUF);
    if len < cap {
        buf.arr_set(((c.word(chan::HEAD) + len) % cap) as usize, v);
        c.set_word(chan::LEN, len + 1);
        return Some(true);
    }
    if word & chan::DROPPING != 0 {
        return Some(true);
    }
    if word & chan::SLIDING != 0 {
        // Overwrite the oldest value and move the head past it.
        let head = c.word(chan::HEAD);
        buf.arr_set(head as usize, v);
        c.set_word(chan::HEAD, (head + 1) % cap);
        return Some(true);
    }
    None
}

/// Try to take without blocking. `None`: would block. A closed, drained
/// channel yields `nil`.
fn try_take_locked(m: &mut Mutator, c: Value) -> Option<Value> {
    let (cap, len) = (c.word(chan::CAP) & chan::CAP_MASK, c.word(chan::LEN));
    if len > 0 {
        let buf = c.field(chan::BUF);
        let head = c.word(chan::HEAD);
        let v = buf.arr_get(head as usize);
        buf.arr_set(head as usize, Value::NIL);
        c.set_word(chan::HEAD, (head + 1) % cap);
        c.set_word(chan::LEN, len - 1);
        // A parked putter can now move its value into the buffer.
        if let Some(w) = putters(m, c) {
            let tail = (c.word(chan::HEAD) + len - 1) % cap;
            buf.arr_set(tail as usize, w.field(waiter::VALUE));
            c.set_word(chan::LEN, len);
            deliver(w, Value::TRUE, c);
        }
        return Some(v);
    }
    if let Some(w) = putters(m, c) {
        deliver(w, Value::TRUE, c);
        return Some(w.field(waiter::VALUE));
    }
    if c.word(chan::CLOSED) != 0 {
        return Some(Value::NIL);
    }
    None
}

// ---- public operations ---------------------------------------------------------------------

/// `(put! c v)`: true once delivered or buffered, false if the channel is
/// closed. Parks while the channel is full.
pub fn put(vm: &mut Vm, c: Value, v: Value) -> Result<Value> {
    let c = expect_chan(c, "put!")?;
    if v.is_nil() {
        return Err(Error::new("Can't put nil on a channel"));
    }
    crate::owned::release_on_send(vm, v);
    let _g = spin_lock(c, chan::LOCK);
    if let Some(r) = try_put_locked(&mut vm.m, c, v) {
        return Ok(Value::bool(r));
    }
    if !vm.can_park() {
        return Err(Vm::park_error());
    }
    let claim = claim_new(&mut vm.m);
    let w = waiter_new(&mut vm.m, vm.current, v, claim, c, 0);
    q_push(&mut vm.m, c, chan::PUTTERS_BACK, w);
    park_on(vm, w, false);
    Ok(Value::NIL)
}

/// `(take! c)`: the next value, or nil once the channel is closed and empty.
pub fn take(vm: &mut Vm, c: Value) -> Result<Value> {
    let c = expect_chan(c, "take!")?;
    let _g = spin_lock(c, chan::LOCK);
    if let Some(v) = try_take_locked(&mut vm.m, c) {
        return Ok(v);
    }
    if !vm.can_park() {
        return Err(Vm::park_error());
    }
    let claim = claim_new(&mut vm.m);
    let w = waiter_new(&mut vm.m, vm.current, Value::NIL, claim, c, 0);
    q_push(&mut vm.m, c, chan::TAKERS_BACK, w);
    park_on(vm, w, false);
    Ok(Value::NIL)
}

/// `(offer! c v)`: put only if it can happen now. Returns true/false.
pub fn offer(vm: &mut Vm, c: Value, v: Value) -> Result<Value> {
    let c = expect_chan(c, "offer!")?;
    if v.is_nil() {
        return Err(Error::new("Can't put nil on a channel"));
    }
    crate::owned::release_on_send(vm, v);
    let _g = spin_lock(c, chan::LOCK);
    Ok(Value::bool(try_put_locked(&mut vm.m, c, v).unwrap_or(false)))
}

/// Offer `v` on channel `c` from a thread that isn't running a fiber (a
/// file watcher, the signal thread): the caller is a running participant
/// with its own mutator. True if it went in.
pub fn offer_from(m: &mut Mutator, c: Value, v: Value) -> bool {
    let _g = spin_lock(c, chan::LOCK);
    try_put_locked(m, c, v).unwrap_or(false)
}

/// `(poll! c)`: take only if a value is available now, else nil.
pub fn poll(vm: &mut Vm, c: Value) -> Result<Value> {
    let c = expect_chan(c, "poll!")?;
    let _g = spin_lock(c, chan::LOCK);
    Ok(try_take_locked(&mut vm.m, c).unwrap_or(Value::NIL))
}

fn drain(c: Value, front: usize, back: usize, value: Value) {
    for f in [front, back] {
        let mut l = c.field(f);
        while l.is_cons() {
            let w = l.car();
            if try_claim(w.field(waiter::CLAIM)) {
                deliver(w, value, c);
            }
            l = l.cdr();
        }
        c.set_field(f, Value::NIL);
    }
}

/// `(close! c)`: parked takers get nil, parked putters get false. Buffered
/// values can still be taken. Allocation-free (the timer thread uses it).
pub fn close(c: Value) {
    let _g = spin_lock(c, chan::LOCK);
    c.set_word(chan::CLOSED, 1);
    drain(c, chan::TAKERS_FRONT, chan::TAKERS_BACK, Value::NIL);
    drain(c, chan::PUTTERS_FRONT, chan::PUTTERS_BACK, Value::FALSE);
}

pub fn closed(c: Value) -> Result<Value> {
    let c = expect_chan(c, "closed?")?;
    Ok(Value::bool(c.word(chan::CLOSED) != 0))
}

// ---- alts! ------------------------------------------------------------------------------------------

thread_local! {
    static RNG: std::cell::Cell<u64> = const { std::cell::Cell::new(0x9e37_79b9_7f4a_7c15) };
}

fn random(n: usize) -> usize {
    RNG.with(|r| {
        let mut x = r.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        r.set(x);
        (x % n as u64) as usize
    })
}

/// `(alts! ops & {:default d :priority p})`. Each op is a channel (take) or
/// `[channel value]` (put). Performs exactly one ready op — chosen at random
/// unless `:priority true` — and returns `[result channel]`; with `:default`
/// returns `[d :default]` if none is ready; otherwise parks on all of them.
pub fn alts(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let ops_v = args[0];
    if !ops_v.is(Kind::Vector) || ops_v.vec_count() == 0 {
        return Err(Error::of("TypeError", "alts! expects a non-empty vector of operations"));
    }
    if !(args.len() - 1).is_multiple_of(2) {
        return Err(Error::new("alts! options must be :key value pairs"));
    }
    let mut default = None;
    let mut priority = false;
    for kv in args[1..].chunks(2) {
        match kv[0] {
            k if k == rt().keyword(&mut vm.m, None, "default") => default = Some(kv[1]),
            k if k == rt().keyword(&mut vm.m, None, "priority") => priority = kv[1].truthy(),
            k => return Err(Error::new(format!("Unknown alts! option: {}", pr_str(k, true)))),
        }
    }
    let mut ops: Vec<(Value, Option<Value>)> = Vec::new();
    for i in 0..ops_v.vec_count() {
        let op = ops_v.vec_nth(i);
        if op.is(Kind::Vector) && op.vec_count() == 2 {
            let v = op.vec_nth(1);
            if v.is_nil() {
                return Err(Error::new("Can't put nil on a channel"));
            }
            ops.push((expect_chan(op.vec_nth(0), "alts!")?, Some(v)));
        } else {
            ops.push((expect_chan(op, "alts!")?, None));
        }
    }
    // Lock every channel, in address order so concurrent alts! can't deadlock.
    let mut chans: Vec<Value> = ops.iter().map(|(c, _)| *c).collect();
    chans.sort_by_key(|c| c.0);
    chans.dedup();
    let _guards: Vec<SpinGuard> = chans.iter().map(|&c| spin_lock(c, chan::LOCK)).collect();

    let n = ops.len();
    let start = if priority { 0 } else { random(n) };
    for k in 0..n {
        let (c, put) = ops[(start + k) % n];
        let r = match put {
            Some(v) => try_put_locked(&mut vm.m, c, v).map(Value::bool),
            None => try_take_locked(&mut vm.m, c),
        };
        if let Some(r) = r {
            return Ok(vec_from_slice(&mut vm.m, &[r, c]));
        }
    }
    if let Some(d) = default {
        let kw = rt().keyword(&mut vm.m, None, "default");
        return Ok(vec_from_slice(&mut vm.m, &[d, kw]));
    }
    if !vm.can_park() {
        return Err(Vm::park_error());
    }
    let claim = claim_new(&mut vm.m);
    let mut first = Value::NIL;
    for &(c, put) in &ops {
        let w = waiter_new(&mut vm.m, vm.current, put.unwrap_or(Value::NIL), claim, c, 0);
        if first.is_nil() {
            first = w;
        }
        if put.is_some() {
            q_push(&mut vm.m, c, chan::PUTTERS_BACK, w);
        } else {
            q_push(&mut vm.m, c, chan::TAKERS_BACK, w);
        }
    }
    park_on(vm, first, true);
    Ok(Value::NIL)
}

// ---- await --------------------------------------------------------------------------------------------

/// `(await f)`: the fiber's result, or its error re-raised here.
/// With `raise = false` (`%join`): wait for it to finish, return nil.
pub fn await_fiber(vm: &mut Vm, f: Value) -> Result<Value> {
    wait_fiber(vm, f, true)
}

pub fn wait_fiber(vm: &mut Vm, f: Value, raise: bool) -> Result<Value> {
    use norn_rt::conc::fiber;
    if !f.is(Kind::Fiber) {
        return Err(Error::of("TypeError", format!("await expects a fiber, got {}", f.type_name())));
    }
    if f == vm.current {
        return Err(Error::new("A fiber can't await itself"));
    }
    // (receiving a finished fiber's error here counts as seeing it: its
    // scope won't raise it again)
    let done = |vm: &mut Vm, f: Value| {
        crate::sched::observe(vm, f);
        if raise { crate::sched::fiber_outcome(f) } else { Ok(Value::NIL) }
    };
    if f.fiber_state().load(std::sync::atomic::Ordering::Acquire) == fiber::DONE {
        return done(vm, f);
    }
    if !vm.can_park() {
        return Err(Vm::park_error());
    }
    let claim = claim_new(&mut vm.m);
    let flags = if raise { 0 } else { waiter::NO_RAISE };
    let w = waiter_new(&mut vm.m, vm.current, Value::NIL, claim, Value::NIL, flags);
    {
        let _g = spin_lock(f, fiber::LOCK);
        if f.fiber_state().load(std::sync::atomic::Ordering::Acquire) == fiber::DONE {
            drop(_g);
            return done(vm, f);
        }
        let j = cons(&mut vm.m, w, f.field(fiber::JOINERS));
        f.set_field(fiber::JOINERS, j);
    }
    park_on(vm, w, false);
    Ok(Value::NIL)
}
