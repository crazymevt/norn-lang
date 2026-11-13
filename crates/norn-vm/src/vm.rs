//! The tier-0 interpreter, running fibers on heap-allocated segmented stacks
//! (runtime.md §3, §5).
//!
//! Frame layout: see `norn_rt::func` (fn, proto, resume, prev, registers).
//! Frames link by *offset*, never by address.
//!
//! The run loop never recurses on Norn→Norn calls, so a fiber can stop
//! between any two instructions:
//!
//! - **yield**: its time slice (reduction budget) ran out at a safepoint poll;
//! - **park**: a primitive registered a wait (channel, await, sleep) and asked
//!   to be resumed later with a value;
//! - **finish**: the bottom frame returned.
//!
//! Rust re-enters the interpreter only via `Vm::apply` (macro expansion and
//! boot). Such nested runs are *non-suspendable* and *GC-inhibited*: they
//! can't park or yield, and no collection happens while Rust code is holding
//! values across them (runtime.md §4.3).

use crate::bytecode::*;
use crate::prims::{APPLY, EVAL, PRIMS};
use norn_rt::conc::fiber;
use norn_rt::func::{
    Capture, F_FN, F_PREV, F_PROTO, F_RESUME, HDR, PREV_BOUNDARY, PREV_SEG_START, closure_new, segment_new,
};
use norn_rt::list::list_from_with_tail;
use norn_rt::print::pr_str;
use norn_rt::runtime::{Namespace, rt};
use norn_rt::safepoint;
use norn_rt::vector::vec_from_slice;
use norn_rt::{Kind, Mutator, Value};

// ---- errors ---------------------------------------------------------------

/// A Norn error. M0/M1 have no condition system yet (M3), so an error ends
/// the fiber it happens in and carries a Norn-level backtrace.
#[derive(Debug, Clone)]
pub struct Error {
    pub msg: String,
    pub trace: Vec<String>,
}

impl Error {
    pub fn new(msg: impl Into<String>) -> Error {
        Error { msg: msg.into(), trace: Vec::new() }
    }
}

impl From<String> for Error {
    fn from(msg: String) -> Error {
        Error::new(msg)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.msg)?;
        for t in &self.trace {
            write!(f, "\n  at {t}")?;
        }
        Ok(())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn cancel_error(code: i64) -> Error {
    Error::new(if code == fiber::INTERRUPTED { "Interrupted" } else { "Cancelled" })
}

// ---- stack sizes ---------------------------------------------------------------

/// First segment of a fiber (words). Small, so a million parked fibers fit.
pub const FIBER_SEG_WORDS: usize = 128;
/// Segments for nested runs from Rust.
const NESTED_SEG_WORDS: usize = 16 * 1024;
/// Segments grow by doubling up to this size.
const MAX_SEG_WORDS: usize = 32 * 1024;
/// Total stack depth limit per fiber (words) → `Stack overflow`.
const MAX_STACK_WORDS: usize = 32 << 20;
/// Reductions (calls + loop iterations) per time slice.
const BUDGET: i64 = 2000;

/// Interpreter registers. Everything here is derivable from the frame chain.
pub(crate) struct St {
    pub seg: Value,
    pub base: *mut Value,
    pub fp: usize,
    pub pc: usize,
    code: *const u32,
    consts: *const Value,
}

impl St {
    fn at(seg: Value, fp: usize) -> St {
        St { seg, base: seg.seg_data(), fp, pc: 0, code: std::ptr::null(), consts: std::ptr::null() }
    }
    #[inline]
    unsafe fn frame(&self) -> *mut Value {
        unsafe { self.base.add(self.fp) }
    }
    #[inline]
    unsafe fn regs(&self) -> *mut Value {
        unsafe { self.base.add(self.fp + HDR) }
    }
    #[inline]
    unsafe fn slot(&self, i: usize) -> Value {
        unsafe { *self.frame().add(i) }
    }
    /// Load code/constants/pc from the current frame.
    #[inline]
    unsafe fn enter(&mut self) {
        unsafe {
            let p = self.slot(F_PROTO);
            self.code = p.proto_code().code_ptr();
            self.consts = p.proto_consts().arr_ptr();
            self.pc = self.slot(F_RESUME).as_fixnum() as usize;
        }
    }
    #[inline]
    unsafe fn set_resume(&self, pc: usize) {
        unsafe { *self.frame().add(F_RESUME) = Value::fix(pc as i64) }
    }
}

/// What the interpreter does next.
pub(crate) enum Flow {
    Next,
    Done(Value),
    Park,
    Yield,
}

/// Why a fiber's run stopped.
pub enum Exit {
    Done(Value),
    Park,
    Yield,
}

/// What a primitive asked for besides its return value.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum Suspend {
    /// Park until woken; deliver WAKE_VALUE (or `[WAKE_VALUE WAKE_AUX]`).
    Park { pair: bool },
    /// Go to the back of the run queue.
    Yield,
}

// ---- the VM ----------------------------------------------------------------------

/// Per-carrier interpreter state. One `Vm` per carrier thread.
pub struct Vm {
    pub m: Mutator,
    pub ns: &'static Namespace,
    pub core: &'static Namespace,
    pub(crate) carrier: usize,
    /// The fiber this carrier is running, or nil.
    pub(crate) current: Value,
    pub(crate) suspend: Option<Suspend>,
    /// Depth of nested (non-suspendable, GC-inhibited) runs.
    pub(crate) nested: u32,
    pub(crate) want_gc: bool,
    spare: Value,
    scratch: Vec<Value>,
    budget: i64,
    seen_attention: u64,
}

fn arity_error(f: Value, n: usize) -> Error {
    Error::new(format!("Wrong number of args ({n}) passed to: {}", pr_str(f, true)))
}

/// The arity of closure `f` that accepts `argc` arguments.
fn select_arity(f: Value, argc: usize) -> Result<Value> {
    let arities = f.closure_template().template_arities();
    let ps = arities.arr_slice();
    if let Some(&p) = ps.iter().find(|p| !p.proto_variadic() && p.proto_nfixed() == argc) {
        return Ok(p);
    }
    if let Some(&p) = ps.iter().find(|p| p.proto_variadic() && argc >= p.proto_nfixed()) {
        return Ok(p);
    }
    Err(arity_error(f, argc))
}

impl Vm {
    pub(crate) fn bare(m: Mutator, core: &'static Namespace, carrier: usize) -> Vm {
        Vm {
            m,
            ns: core,
            core,
            carrier,
            current: Value::NIL,
            suspend: None,
            nested: 0,
            want_gc: false,
            spare: Value::NIL,
            scratch: Vec::new(),
            budget: BUDGET,
            seen_attention: 0,
        }
    }

    /// Resolve a symbol to a var from the current namespace.
    pub fn resolve(&mut self, sym: Value) -> Option<Value> {
        rt().resolve(&mut self.m, self.ns, self.core, sym)
    }

    pub fn sym(&mut self, s: &str) -> Value {
        rt().symbol_from(&mut self.m, s)
    }

    /// May the running code park (block) here?
    pub(crate) fn can_park(&self) -> bool {
        self.nested == 0 && !self.current.is_nil()
    }

    pub(crate) fn park_error() -> Error {
        Error::new("Can't block here: not in a fiber, or inside macro expansion")
    }

    // ---- segments ----

    fn take_segment(&mut self, need: usize, size: usize) -> Value {
        let s = self.spare;
        if !s.is_nil() && s.seg_capacity() >= need {
            self.spare = Value::NIL;
            return s;
        }
        segment_new(&mut self.m, need.max(size))
    }

    /// Keep one free segment around so call/return across a segment boundary
    /// in a loop doesn't allocate every time (the "hot split" problem).
    pub(crate) fn give_segment(&mut self, s: Value) {
        s.seg_link(Value::NIL, 0, 0);
        s.seg_set_top(-1);
        if self.spare.is_nil() || s.seg_capacity() > self.spare.seg_capacity() {
            self.spare = s;
        }
    }

    /// Drop the cached segment (it must not survive a GC unrooted).
    pub(crate) fn drop_spare(&mut self) {
        self.spare = Value::NIL;
    }

    unsafe fn write_frame(&mut self, base: *mut Value, fp: usize, f: Value, proto: Value, prev: i64, args: &[Value]) {
        unsafe {
            let fr = base.add(fp);
            *fr.add(F_FN) = f;
            *fr.add(F_PROTO) = proto;
            *fr.add(F_RESUME) = Value::fix(0);
            *fr.add(F_PREV) = Value::fix(prev);
            let regs = fr.add(HDR);
            let nfixed = proto.proto_nfixed();
            let mut next = nfixed;
            std::ptr::copy_nonoverlapping(args.as_ptr(), regs, nfixed);
            if proto.proto_variadic() {
                let rest = if args.len() > nfixed {
                    list_from_with_tail(&mut self.m, &args[nfixed..], Value::NIL)
                } else {
                    Value::NIL
                };
                *regs.add(nfixed) = rest;
                next += 1;
            }
            for i in next..proto.proto_nregs() {
                *regs.add(i) = Value::NIL;
            }
        }
    }

    /// Push a callee frame above the current one (or in a new segment), then
    /// poll: the safepoint is *after* the frame is complete, so a GC there
    /// sees the arguments in the frame.
    unsafe fn push_frame(&mut self, st: &mut St, f: Value, proto: Value, args: &[Value]) -> Result<Flow> {
        unsafe {
            let need = HDR + proto.proto_nregs();
            let cur_end = st.fp + HDR + st.slot(F_PROTO).proto_nregs();
            if cur_end + need <= st.seg.seg_capacity() {
                self.write_frame(st.base, cur_end, f, proto, st.fp as i64, args);
                st.fp = cur_end;
            } else {
                let depth = st.seg.seg_depth() + cur_end;
                if depth > MAX_STACK_WORDS {
                    return Err(Error::new("Stack overflow"));
                }
                let size = (st.seg.seg_capacity() * 2).min(MAX_SEG_WORDS);
                let s = self.take_segment(need, size);
                st.seg.seg_set_top(st.fp as i64);
                s.seg_link(st.seg, st.fp, depth);
                self.write_frame(s.seg_data(), 0, f, proto, PREV_SEG_START, args);
                st.seg = s;
                st.base = s.seg_data();
                st.fp = 0;
            }
            st.enter();
            self.poll(st)
        }
    }

    /// Replace the current frame with a callee frame (proper tail call).
    unsafe fn tail_frame(&mut self, st: &mut St, f: Value, proto: Value, args: &[Value]) -> Result<Flow> {
        unsafe {
            let need = HDR + proto.proto_nregs();
            let prev = st.slot(F_PREV).as_fixnum();
            if st.fp + need <= st.seg.seg_capacity() {
                self.write_frame(st.base, st.fp, f, proto, prev, args);
            } else {
                let size = (st.seg.seg_capacity() * 2).min(MAX_SEG_WORDS);
                let s = self.take_segment(need, size);
                let new_prev = match prev {
                    PREV_BOUNDARY => PREV_BOUNDARY,
                    PREV_SEG_START => {
                        s.seg_link(st.seg.seg_prev(), st.seg.seg_prev_fp(), st.seg.seg_depth());
                        PREV_SEG_START
                    }
                    p => {
                        st.seg.seg_set_top(p);
                        s.seg_link(st.seg, p as usize, st.seg.seg_depth() + st.fp);
                        PREV_SEG_START
                    }
                };
                self.write_frame(s.seg_data(), 0, f, proto, new_prev, args);
                st.seg = s;
                st.base = s.seg_data();
                st.fp = 0;
            }
            st.enter();
            self.poll(st)
        }
    }

    /// Pop the current frame and deliver `val` to the caller.
    #[inline]
    unsafe fn do_return(&mut self, st: &mut St, val: Value) -> Flow {
        unsafe {
            match st.slot(F_PREV).as_fixnum() {
                PREV_BOUNDARY => {
                    self.give_segment(st.seg);
                    st.seg = Value::NIL;
                    return Flow::Done(val);
                }
                PREV_SEG_START => {
                    let old = st.seg;
                    st.seg = old.seg_prev();
                    st.base = st.seg.seg_data();
                    st.fp = old.seg_prev_fp();
                    self.give_segment(old);
                }
                p => st.fp = p as usize,
            }
            st.enter();
            // The instruction before the resume point is the CALL; its A
            // operand names the destination register.
            let call = *st.code.add(st.pc - 1);
            *st.regs().add(a(call)) = val;
        }
        Flow::Next
    }

    // ---- safepoints ----

    /// Make the stack exactly describable to the GC (runtime.md §4.2).
    pub(crate) fn save(&self, st: &St) {
        if st.seg.is_nil() {
            return;
        }
        st.seg.seg_set_top(st.fp as i64);
        if !self.current.is_nil() && self.nested == 0 {
            self.current.set_field(fiber::SEG, st.seg);
            self.current.set_field(fiber::FP, Value::fix(st.fp as i64));
        }
    }

    /// The safepoint poll at function entry and loop back-edges: one
    /// decrement and one load on the fast path.
    #[inline]
    unsafe fn poll(&mut self, st: &mut St) -> Result<Flow> {
        self.budget -= 1;
        if self.budget > 0 && safepoint::attention() == self.seen_attention {
            return Ok(Flow::Next);
        }
        self.poll_slow(st)
    }

    #[cold]
    fn poll_slow(&mut self, st: &mut St) -> Result<Flow> {
        self.seen_attention = safepoint::attention();
        if self.nested == 0 {
            let gc = std::mem::take(&mut self.want_gc) || safepoint::stress_due();
            if gc || safepoint::stop_requested() {
                self.save(st);
                self.drop_spare();
                if gc {
                    safepoint::collect(&mut self.m);
                } else {
                    safepoint::block_for_stop();
                }
            }
            crate::sched::process_external(self);
        }
        if !self.current.is_nil() {
            let c = self.current.fiber_cancel();
            if c != 0 {
                return Err(cancel_error(c as i64));
            }
        }
        if self.budget <= 0 {
            self.budget = BUDGET;
            if self.can_park() && crate::sched::has_other_work(self.carrier) {
                // Time slice over: continue exactly here later.
                // SAFETY: st describes the current frame.
                unsafe { st.set_resume(st.pc) };
                self.current.set_word(fiber::MODE, fiber::CONTINUE);
                return Ok(Flow::Yield);
            }
        }
        Ok(Flow::Next)
    }

    // ---- calls ----

    /// Call a primitive after checking its arity.
    pub(crate) fn call_prim(&mut self, f: Value, args: &[Value]) -> Result<Value> {
        if args.len() < f.prim_min() || args.len() > f.prim_max() {
            return Err(arity_error(f, args.len()));
        }
        (PRIMS[f.prim_index()].f)(self, args)
    }

    /// Calling a non-function value: keywords, maps, sets and vectors are
    /// functions of their keys (Clojure semantics).
    fn call_data(&mut self, f: Value, args: &[Value]) -> Result<Value> {
        let not_fn = || Error::new(format!("{} is not a function: {}", f.type_name(), pr_str(f, true)));
        if f.is_keyword() {
            if args.is_empty() || args.len() > 2 {
                return Err(arity_error(f, args.len()));
            }
            let d = args.get(1).copied().unwrap_or(Value::NIL);
            return Ok(crate::prims::get(args[0], f).unwrap_or(d));
        }
        if !f.is_obj() {
            return Err(not_fn());
        }
        match f.obj_kind() {
            Kind::Map | Kind::Set => {
                if args.is_empty() || args.len() > 2 {
                    return Err(arity_error(f, args.len()));
                }
                let d = args.get(1).copied().unwrap_or(Value::NIL);
                Ok(crate::prims::get(f, args[0]).unwrap_or(d))
            }
            Kind::Vector => {
                if args.len() != 1 {
                    return Err(arity_error(f, args.len()));
                }
                let i = args[0];
                if i.is_fixnum() && i.as_fixnum() >= 0 && (i.as_fixnum() as usize) < f.vec_count() {
                    Ok(f.vec_nth(i.as_fixnum() as usize))
                } else {
                    Err(Error::new(format!("Index out of bounds: {}", pr_str(i, true))))
                }
            }
            _ => Err(not_fn()),
        }
    }

    fn deref_var(f: Value) -> Result<Value> {
        let v = f.var_root();
        if v == Value::UNBOUND {
            return Err(Error::new(format!("Unbound var: {}", pr_str(f, true))));
        }
        Ok(v)
    }

    /// `(apply f a b [c d])` → (f, [a b c d]).
    pub(crate) fn spread_apply(&mut self, args: &[Value]) -> Result<(Value, Vec<Value>)> {
        let (f, mid) = (args[0], &args[1..args.len() - 1]);
        let last = args[args.len() - 1];
        let mut v = mid.to_vec();
        v.extend(norn_rt::seq::to_vec(&mut self.m, last)?);
        Ok((f, v))
    }

    /// Call `f` from the interpreter. `dest = Some(r)`: ordinary call whose
    /// result goes to register r; `None`: tail call.
    unsafe fn invoke(&mut self, st: &mut St, mut f: Value, args: &[Value], dest: Option<usize>) -> Result<Flow> {
        unsafe {
            loop {
                if f.is(Kind::Closure) {
                    let proto = select_arity(f, args.len())?;
                    return match dest {
                        Some(_) => {
                            st.set_resume(st.pc);
                            self.push_frame(st, f, proto, args)
                        }
                        None => self.tail_frame(st, f, proto, args),
                    };
                }
                if f.is(Kind::Var) {
                    f = Self::deref_var(f)?;
                    continue;
                }
                let val = if f.is(Kind::Primitive) {
                    match f.prim_index() {
                        APPLY => {
                            if args.len() < 2 {
                                return Err(arity_error(f, args.len()));
                            }
                            let (g, spread) = self.spread_apply(args)?;
                            return self.invoke(st, g, &spread, dest);
                        }
                        EVAL => {
                            if args.len() != 1 {
                                return Err(arity_error(f, args.len()));
                            }
                            // eval = compile, then call the thunk in this
                            // fiber: suspendable, unlike a nested run.
                            let thunk = self.compile_for_eval(args[0])?;
                            return self.invoke(st, thunk, &[], dest);
                        }
                        _ => self.call_prim(f, args)?,
                    }
                } else {
                    self.call_data(f, args)?
                };
                if let Some(s) = self.suspend.take() {
                    return Ok(self.suspend_here(st, s, dest));
                }
                return Ok(match dest {
                    Some(r) => {
                        *st.regs().add(r) = val;
                        Flow::Next
                    }
                    None => self.do_return(st, val),
                });
            }
        }
    }

    /// A primitive asked to park or yield. Record how to resume.
    unsafe fn suspend_here(&mut self, st: &mut St, s: Suspend, dest: Option<usize>) -> Flow {
        let f = self.current;
        let (mode, flow) = match (s, dest) {
            (Suspend::Park { pair: false }, Some(_)) => (fiber::DELIVER, Flow::Park),
            (Suspend::Park { pair: true }, Some(_)) => (fiber::DELIVER_PAIR, Flow::Park),
            (Suspend::Park { pair: false }, None) => (fiber::RETURN, Flow::Park),
            (Suspend::Park { pair: true }, None) => (fiber::RETURN_PAIR, Flow::Park),
            (Suspend::Yield, Some(_)) => (fiber::DELIVER, Flow::Yield),
            (Suspend::Yield, None) => (fiber::RETURN, Flow::Yield),
        };
        if dest.is_some() {
            // SAFETY: st describes the current frame.
            unsafe { st.set_resume(st.pc) };
        }
        f.set_word(fiber::MODE, mode);
        flow
    }

    /// Norn-level backtrace from the current frame to the bottom.
    unsafe fn backtrace(&self, st: &St) -> Vec<String> {
        let mut out = Vec::new();
        if st.seg.is_nil() {
            return out;
        }
        let (mut seg, mut fp) = (st.seg, st.fp);
        unsafe {
            for _ in 0..64 {
                let fr = seg.seg_data().add(fp);
                let name = (*fr.add(F_PROTO)).proto_name();
                out.push(if name.is_nil() { "fn".to_owned() } else { pr_str(name, true) });
                match (*fr.add(F_PREV)).as_fixnum() {
                    PREV_BOUNDARY => break,
                    PREV_SEG_START => {
                        fp = seg.seg_prev_fp();
                        seg = seg.seg_prev();
                    }
                    p => fp = p as usize,
                }
            }
        }
        out
    }

    fn fail(&self, st: &St, mut e: Error) -> Error {
        // SAFETY: st describes a valid frame chain.
        e.trace.extend(unsafe { self.backtrace(st) });
        e
    }

    // ---- entry points ----

    /// Call any callable from Rust: a *nested* run on a fresh stack. It can't
    /// park or yield, and GC is inhibited until it returns.
    pub fn apply(&mut self, f: Value, args: &[Value]) -> Result<Value> {
        self.nested += 1;
        let r = self.apply_inner(f, args);
        self.nested -= 1;
        r
    }

    fn apply_inner(&mut self, mut f: Value, args: &[Value]) -> Result<Value> {
        loop {
            if f.is(Kind::Closure) {
                let proto = select_arity(f, args.len())?;
                let s = self.take_segment(HDR + proto.proto_nregs(), NESTED_SEG_WORDS);
                s.seg_link(Value::NIL, 0, 0);
                // SAFETY: fresh segment large enough for one frame.
                unsafe {
                    self.write_frame(s.seg_data(), 0, f, proto, PREV_BOUNDARY, args);
                    let mut st = St::at(s, 0);
                    st.enter();
                    return match self.run(&mut st)? {
                        Exit::Done(v) => Ok(v),
                        Exit::Park | Exit::Yield => Err(Self::park_error()),
                    };
                }
            }
            if f.is(Kind::Var) {
                f = Self::deref_var(f)?;
                continue;
            }
            if f.is(Kind::Primitive) {
                if f.prim_index() == APPLY {
                    if args.len() < 2 {
                        return Err(arity_error(f, args.len()));
                    }
                    let (g, spread) = self.spread_apply(args)?;
                    return self.apply_inner(g, &spread);
                }
                if f.prim_index() == EVAL {
                    return self.eval(args[0]);
                }
                return self.call_prim(f, args);
            }
            return self.call_data(f, args);
        }
    }

    /// Run fiber `f` on this carrier until it finishes, parks or yields.
    /// The caller has set `self.current = f` and `self.ns` from the fiber.
    pub(crate) fn run_fiber(&mut self, f: Value) -> Result<Exit> {
        let mode = f.word(fiber::MODE);
        let mut st;
        // SAFETY: frame chains in fibers are valid.
        unsafe {
            if mode == fiber::START {
                let thunk = f.field(fiber::THUNK);
                f.set_field(fiber::THUNK, Value::NIL);
                // A thunk is a callable, or an internal [f arg…] call spec.
                let (callee, args) = if thunk.is(Kind::Array) {
                    (thunk.arr_get(0), thunk.arr_slice()[1..].to_vec())
                } else {
                    (thunk, Vec::new())
                };
                let mut callee = callee;
                while callee.is(Kind::Var) {
                    callee = Self::deref_var(callee)?;
                }
                if !callee.is(Kind::Closure) {
                    // Primitives and data run to completion directly.
                    self.nested += 1;
                    let r = self.apply_inner(callee, &args);
                    self.nested -= 1;
                    return r.map(Exit::Done);
                }
                let proto = select_arity(callee, args.len())?;
                let s = self.take_segment(HDR + proto.proto_nregs(), FIBER_SEG_WORDS);
                s.seg_link(Value::NIL, 0, 0);
                self.write_frame(s.seg_data(), 0, callee, proto, PREV_BOUNDARY, &args);
                st = St::at(s, 0);
                st.enter();
            } else {
                st = St::at(f.field(fiber::SEG), f.field(fiber::FP).as_fixnum() as usize);
                st.enter();
                let v = f.field(fiber::WAKE_VALUE);
                let deliver = match mode {
                    fiber::DELIVER | fiber::RETURN => Some(v),
                    fiber::DELIVER_PAIR | fiber::RETURN_PAIR => {
                        let aux = f.field(fiber::WAKE_AUX);
                        Some(vec_from_slice(&mut self.m, &[v, aux]))
                    }
                    _ => None,
                };
                if let Some(v) = deliver {
                    if mode == fiber::RETURN || mode == fiber::RETURN_PAIR {
                        if let Flow::Done(v) = self.do_return(&mut st, v) {
                            return Ok(Exit::Done(v));
                        }
                    } else {
                        let call = *st.code.add(st.pc - 1);
                        *st.regs().add(a(call)) = v;
                    }
                }
            }
            f.set_field(fiber::WAKE_VALUE, Value::NIL);
            f.set_field(fiber::WAKE_AUX, Value::NIL);
            let err = f.field(fiber::WAKE_ERR);
            if !err.is_nil() {
                f.set_field(fiber::WAKE_ERR, Value::NIL);
                return Err(self.fail(&st, crate::sched::wake_error(err)));
            }
            let r = self.run(&mut st);
            if matches!(r, Ok(Exit::Park) | Ok(Exit::Yield)) {
                self.save(&st);
            }
            r
        }
    }

    /// The interpreter loop.
    unsafe fn run(&mut self, st: &mut St) -> Result<Exit> {
        unsafe {
            loop {
                let ins = *st.code.add(st.pc);
                st.pc += 1;
                let ra = a(ins);
                let regs = st.regs();
                let flow = match op(ins) {
                    MOVE => {
                        *regs.add(ra) = *regs.add(b(ins));
                        continue;
                    }
                    LOADK => {
                        *regs.add(ra) = *st.consts.add(bx(ins));
                        continue;
                    }
                    LOADI => {
                        *regs.add(ra) = Value::fix(sbx(ins) as i64);
                        continue;
                    }
                    LOADNIL => {
                        *regs.add(ra) = Value::NIL;
                        continue;
                    }
                    LOADTRUE => {
                        *regs.add(ra) = Value::TRUE;
                        continue;
                    }
                    LOADFALSE => {
                        *regs.add(ra) = Value::FALSE;
                        continue;
                    }
                    GETVAR => {
                        let var = *st.consts.add(bx(ins));
                        let v = var.var_root();
                        if v == Value::UNBOUND {
                            let e = Error::new(format!("Unbound var: {}", pr_str(var, true)));
                            return Err(self.fail(st, e));
                        }
                        *regs.add(ra) = v;
                        continue;
                    }
                    DEFVAR => {
                        (*st.consts.add(bx(ins))).var_set_root(*regs.add(ra));
                        continue;
                    }
                    GETUP => {
                        *regs.add(ra) = st.slot(F_FN).closure_upval(b(ins));
                        continue;
                    }
                    SELF => {
                        *regs.add(ra) = st.slot(F_FN);
                        continue;
                    }
                    CLOSURE => {
                        let tmpl = *st.consts.add(bx(ins));
                        let c = closure_new(&mut self.m, tmpl);
                        let cur = st.slot(F_FN);
                        let caps = tmpl.template_captures();
                        for (i, &cap) in caps.arr_slice().iter().enumerate() {
                            let v = match Capture::decode(cap) {
                                Capture::Local(r) => *regs.add(r as usize),
                                Capture::Up(j) => cur.closure_upval(j as usize),
                                Capture::SelfRef => cur,
                            };
                            c.closure_set_upval(i, v);
                        }
                        *regs.add(ra) = c;
                        continue;
                    }
                    SETUP => {
                        (*regs.add(ra)).closure_set_upval(b(ins), *regs.add(c(ins)));
                        continue;
                    }
                    JMP => {
                        st.pc = (st.pc as isize + sbx(ins)) as usize;
                        continue;
                    }
                    JMPF => {
                        if !(*regs.add(ra)).truthy() {
                            st.pc = (st.pc as isize + sbx(ins)) as usize;
                        }
                        continue;
                    }
                    LOOP => {
                        st.pc = (st.pc as isize + sbx(ins)) as usize;
                        self.poll(st)
                    }
                    CALL => {
                        let f = *regs.add(ra);
                        let args = std::slice::from_raw_parts(regs.add(ra + 1), b(ins));
                        self.invoke(st, f, args, Some(ra))
                    }
                    TAILCALL => {
                        let f = *regs.add(ra);
                        // The callee frame overwrites ours, so copy args out first.
                        let mut scratch = std::mem::take(&mut self.scratch);
                        scratch.clear();
                        scratch.extend_from_slice(std::slice::from_raw_parts(regs.add(ra + 1), b(ins)));
                        let r = self.invoke(st, f, &scratch, None);
                        self.scratch = scratch;
                        r
                    }
                    RETURN => Ok(self.do_return(st, *regs.add(ra))),
                    other => Err(Error::new(format!("bad opcode {other}"))),
                };
                match flow {
                    Ok(Flow::Next) => {}
                    Ok(Flow::Done(v)) => return Ok(Exit::Done(v)),
                    Ok(Flow::Park) => return Ok(Exit::Park),
                    Ok(Flow::Yield) => return Ok(Exit::Yield),
                    Err(e) => return Err(self.fail(st, e)),
                }
            }
        }
    }
}
