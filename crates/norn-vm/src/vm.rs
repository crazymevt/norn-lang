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
use crate::prims::{ABORT_FIBER, APPLY, EVAL, PRIMS, RESTART_FRAME, THROW};
use norn_rt::conc::fiber;
use norn_rt::func::{
    VAR_DYNBOUND, Capture, F_FN, F_PREV, F_PROTO, F_RESUME, HDR, PREV_BOUNDARY, PREV_SEG_START, closure_new, segment_new,
};
use norn_rt::list::list_from_with_tail;
use norn_rt::print::pr_str;
use norn_rt::runtime::{Namespace, rt};
use norn_rt::safepoint;
use norn_rt::string::str_new;
use norn_rt::vector::vec_from_slice;
use norn_rt::{Kind, Mutator, Value};

// ---- errors ---------------------------------------------------------------

/// How an error raised by Rust code is handled when it reaches a fiber's
/// interpreter loop (runtime.md §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrKind {
    /// Signalled as a Norn condition (`RuntimeError`) at the failing
    /// instruction, with `use-value` and `retry` restarts.
    Runtime,
    /// Cancellation, stack overflow: unwind the fiber (running cleanups), then
    /// end it with this error.
    Abort,
    /// End the fiber now (the condition system already ran, or can't run).
    Fatal,
    /// An unrealized stream stands in the way: the interpreter calls
    /// `%force-stream` on it (all of it, if the flag is set) in place of the
    /// failed instruction, then runs the instruction again (design §3.3).
    Force(Value, bool),
}

/// A Norn error as seen from Rust, with a Norn-level backtrace.
#[derive(Debug, Clone)]
pub struct Error {
    pub msg: String,
    pub trace: Vec<String>,
    pub kind: ErrKind,
}

impl Error {
    pub fn new(msg: impl Into<String>) -> Error {
        Error { msg: msg.into(), trace: Vec::new(), kind: ErrKind::Runtime }
    }
    pub fn abort(msg: impl Into<String>) -> Error {
        Error { msg: msg.into(), trace: Vec::new(), kind: ErrKind::Abort }
    }
    pub fn fatal(msg: impl Into<String>, trace: Vec<String>) -> Error {
        Error { msg: msg.into(), trace, kind: ErrKind::Fatal }
    }
    /// Ask the interpreter to realize stream `s` (one cell, or `all` of it).
    pub fn force(s: Value, all: bool) -> Error {
        Error { msg: "A stream had to be realized outside a fiber".into(), trace: Vec::new(), kind: ErrKind::Force(s, all) }
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
    Error::abort(if code == fiber::INTERRUPTED { "Interrupted" } else { "Cancelled" })
}

// ---- frames the runtime recognises -------------------------------------------------

/// Norn functions whose frames mark dynamic extent (core.norn). The unwinder
/// recognises them by closure identity:
///
/// - `%catch* [tag thunk dyn]`          — a throw target
/// - `%unwind-protect* [body cleanup dyn]` — cleanup to run when unwound
/// - `%with-bindings* [new thunk old]`  — dynamic environment to restore
/// - `%finish-unwind [cleanups dyn mode x args]` — runs cleanups, then returns,
///   calls, or ends the fiber
/// - `%runtime-error [msg trace]` — a `retry` returning through it re-executes
///   the failed instruction
pub(crate) struct FrameKinds {
    pub catch: Value,
    pub unwind: Value,
    pub bind: Value,
    pub finish: Value,
    pub runtime_error: Value,
}

pub(crate) static FRAME_KINDS: std::sync::OnceLock<FrameKinds> = std::sync::OnceLock::new();

/// The var of norn.core/%force-stream (its frame's return can mean "retry").
static FORCE_VAR: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

pub(crate) enum Target {
    Catch(Value),
    Frame(Value, usize),
    Bottom,
}

/// Every frame of a stack, top first: (segment, frame offset).
pub(crate) fn walk_frames(mut seg: Value, mut fp: usize) -> Vec<(Value, usize)> {
    let mut out = Vec::new();
    if seg.is_nil() {
        return out;
    }
    loop {
        out.push((seg, fp));
        // SAFETY: (seg, fp) is a live frame.
        let prev = unsafe { (*seg.seg_data().add(fp + F_PREV)).as_fixnum() };
        match prev {
            PREV_BOUNDARY => return out,
            PREV_SEG_START => {
                fp = seg.seg_prev_fp();
                seg = seg.seg_prev();
            }
            p => fp = p as usize,
        }
    }
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
    /// Tier-1 entry of the current frame's proto (0 = not looked up yet).
    native: usize,
}

impl St {
    fn at(seg: Value, fp: usize) -> St {
        St { seg, base: seg.seg_data(), fp, pc: 0, code: std::ptr::null(), consts: std::ptr::null(), native: 0 }
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
            self.native = 0;
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
    /// The running fiber's dynamic environment (map var → value, or nil).
    pub(crate) dyn_env: Value,
    spare: Value,
    scratch: Vec<Value>,
    key_buf: Vec<Value>,
    budget: i64,
    seen_attention: u64,
    jit: crate::jit::JitCtx,
    /// The interpreter state of the innermost `run`, so a primitive can save
    /// the stack before a native region (null outside `run`).
    cur_st: *mut St,
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
            dyn_env: Value::NIL,
            spare: Value::NIL,
            scratch: Vec::new(),
            key_buf: Vec::new(),
            budget: BUDGET,
            seen_attention: 0,
            cur_st: std::ptr::null_mut(),
            jit: crate::jit::JitCtx {
                budget: std::ptr::null_mut(),
                attention: safepoint::attention_ptr(),
                seen: std::ptr::null(),
                prims: [0; crate::jit::NPRIMS],
            },
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
                    return Err(Error::abort("Stack overflow"));
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
            // A `retry` restart returning through %runtime-error, or
            // %force-stream having realized a stream: re-execute the
            // instruction that failed instead of delivering a value.
            let retry = val == Value::RETRY
                && (FRAME_KINDS.get().is_some_and(|k| st.slot(F_FN) == k.runtime_error)
                    || FORCE_VAR.get().is_some_and(|v| st.slot(F_FN) == v.var_root()));
            match st.slot(F_PREV).as_fixnum() {
                PREV_BOUNDARY => {
                    self.give_segment(st.seg);
                    st.seg = Value::NIL;
                    return Flow::Done(if retry { Value::NIL } else { val });
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
            if retry {
                st.pc -= 1;
                return Flow::Next;
            }
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
            self.current.set_field(fiber::DYN, self.dyn_env);
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
        // An image save is waiting for every fiber to stop: yield now.
        let pausing = crate::sched::pausing();
        if self.budget <= 0 || pausing {
            if self.budget <= 0 {
                self.budget = BUDGET;
            }
            if self.can_park() && (pausing || crate::sched::has_other_work(self.carrier)) {
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

    /// A loom.core function the interpreter redirects to (resolved once).
    fn core_fn(&mut self, name: &str, cell: &'static std::sync::OnceLock<Value>) -> Value {
        *cell.get_or_init(|| {
            let sym = rt().symbol(&mut self.m, None, name);
            self.core.lookup(sym).unwrap_or(Value::NIL)
        })
    }

    /// Generic functions and classes are callable. Work out what to call
    /// instead: `(target, None)` calls target with the same arguments;
    /// `(target, Some(args))` with new ones.
    ///
    /// The dispatch hot path (runtime.md §6): compute the key from the
    /// specialised arguments' classes and probe the generic function's
    /// cache, lock-free. A hit calls the cached effective method directly; a
    /// miss calls `%gf-miss` (Norn) in this fiber, which computes the
    /// effective method, caches it and calls it.
    fn redirect(&mut self, f: Value, args: &[Value]) -> Result<(Value, Option<Vec<Value>>)> {
        use norn_rt::clos::generic;
        static MAKE: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
        static MISS: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
        static CUSTOM: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
        let with_self = |target: Value| {
            let mut v = Vec::with_capacity(args.len() + 1);
            v.push(f);
            v.extend_from_slice(args);
            (target, Some(v))
        };
        if f.is(Kind::Class) {
            let make = self.core_fn("make", &MAKE);
            return Ok(with_self(make));
        }
        let nreq = f.gf_nreq();
        if args.len() < nreq || (!f.gf_rest() && args.len() != nreq) {
            return Err(arity_error(f, args.len()));
        }
        if !f.field(generic::DISPATCH).is_nil() {
            let custom = self.core_fn("%gf-dispatch-custom", &CUSTOM);
            return Ok(with_self(custom));
        }
        let mut key = std::mem::take(&mut self.key_buf);
        let hit = norn_rt::clos::dispatch_cached(f, args, &mut key);
        self.key_buf = key;
        match hit {
            Some(em) => Ok((em, None)),
            None => {
                let miss = self.core_fn("%gf-miss", &MISS);
                Ok(with_self(miss))
            }
        }
    }

    /// Call a primitive after checking its arity. A primitive that fails
    /// because an argument holds an unrealized stream gets the stream forced
    /// and runs again: one cell for the one-step operations, which must
    /// work on infinite streams, otherwise all of it.
    pub(crate) fn call_prim(&mut self, f: Value, args: &[Value]) -> Result<Value> {
        if args.len() < f.prim_min() || args.len() > f.prim_max() {
            return Err(arity_error(f, args.len()));
        }
        match (PRIMS[f.prim_index()].f)(self, args) {
            Err(e) if e.kind == ErrKind::Runtime => match args.iter().find_map(|&a| norn_rt::seq::first_unrealized(a)) {
                Some(s) => {
                    let one_step = matches!(PRIMS[f.prim_index()].name, "first" | "rest" | "next" | "seq" | "nth" | "empty?");
                    Err(Error::force(s, !one_step))
                }
                None => Err(e),
            },
            r => r,
        }
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
            crate::owned::check(self, args[0], false)?;
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
                if f.is(Kind::Generic) || f.is(Kind::Class) {
                    match self.redirect(f, args)? {
                        (g, None) => {
                            f = g;
                            continue;
                        }
                        (g, Some(v)) => return self.invoke(st, g, &v, dest),
                    }
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
                        // Stack surgery: these need the interpreter state.
                        THROW => {
                            let kw = rt().keyword(&mut self.m, None, "return");
                            return self.unwind(st, Target::Catch(args[0]), kw, args[1], Value::NIL);
                        }
                        RESTART_FRAME => {
                            if !args[0].is(Kind::Segment) || !args[1].is_fixnum() {
                                return Err(Error::new("%restart-frame expects a frame id"));
                            }
                            let kw = rt().keyword(&mut self.m, None, "call");
                            let target = Target::Frame(args[0], args[1].as_fixnum() as usize);
                            return self.unwind(st, target, kw, Value::NIL, Value::NIL);
                        }
                        ABORT_FIBER => {
                            let kw = rt().keyword(&mut self.m, None, "die");
                            return self.unwind(st, Target::Bottom, kw, args[0], args[1]);
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
        if e.kind == ErrKind::Fatal && !e.trace.is_empty() {
            return e; // the condition system already recorded where it happened
        }
        // SAFETY: st describes a valid frame chain.
        e.trace.extend(unsafe { self.backtrace(st) });
        e
    }

    /// Unwind the stack to `target`, collecting the cleanups of every
    /// `%unwind-protect*` frame passed and the dynamic environment in force at
    /// the target. Then replace the target frame with a call to
    /// `%finish-unwind`, which runs the cleanups (innermost first) and then:
    /// `:return` x, `:call`s the frame's function again (newest definition)
    /// with its original arguments, or `:die`s with message x and trace args.
    unsafe fn unwind(&mut self, st: &mut St, target: Target, mode: Value, x: Value, xargs: Value) -> Result<Flow> {
        let Some(fk) = FRAME_KINDS.get() else {
            return Err(Error::fatal("unwinding before core.norn is loaded", vec![]));
        };
        let mut cleanups = Vec::new();
        let mut dyn_at = self.dyn_env;
        let mut found = None;
        unsafe {
            for (seg, fp) in walk_frames(st.seg, st.fp) {
                let fr = seg.seg_data().add(fp);
                let f = *fr.add(F_FN);
                let regs = fr.add(HDR);
                let hit = match target {
                    Target::Catch(tag) => f == fk.catch && *regs == tag,
                    Target::Frame(s, p) => s == seg && p == fp,
                    Target::Bottom => (*fr.add(F_PREV)).as_fixnum() == PREV_BOUNDARY,
                };
                // Aborting unwinds the bottom frame too (it may itself be an
                // unwind-protect, e.g. when a fiber's thunk tail-calls one); a
                // catch or restarted frame survives.
                let passed = !hit || matches!(target, Target::Bottom);
                if hit && matches!(target, Target::Catch(_)) {
                    dyn_at = *regs.add(2);
                }
                if passed && f == fk.unwind {
                    cleanups.push(vec_from_slice(&mut self.m, &[*regs.add(1), *regs.add(2)]));
                } else if passed && f == fk.bind {
                    dyn_at = *regs.add(2);
                }
                if hit {
                    found = Some((seg, fp));
                    break;
                }
            }
            let Some((seg, fp)) = found else {
                return Err(match target {
                    Target::Catch(tag) => Error::new(format!("No catch for tag {}", pr_str(tag, true))),
                    _ => Error::new("That frame is not on this fiber's stack"),
                });
            };
            let (mut x, mut xargs) = (x, xargs);
            if let Target::Frame(..) = target {
                // Restart the frame: its function's *current* definition, with
                // the arguments it was called with.
                let fr = seg.seg_data().add(fp);
                let (f, proto, regs) = (*fr.add(F_FN), *fr.add(F_PROTO), fr.add(HDR));
                let mut a: Vec<Value> = (0..proto.proto_nfixed()).map(|i| *regs.add(i)).collect();
                if proto.proto_variadic() {
                    a.extend(norn_rt::seq::to_vec(&mut self.m, *regs.add(proto.proto_nfixed()))?);
                }
                let name = proto.proto_name();
                let newest = if name.is_symbol() {
                    self.resolve(name).map(|v| v.var_root()).filter(|v| v.is(Kind::Closure))
                } else {
                    None
                };
                x = newest.unwrap_or(f);
                xargs = vec_from_slice(&mut self.m, &a);
            }
            let cl = vec_from_slice(&mut self.m, &cleanups);
            st.seg = seg;
            st.base = seg.seg_data();
            st.fp = fp;
            st.enter();
            let finish = fk.finish;
            let proto = select_arity(finish, 5)?;
            self.tail_frame(st, finish, proto, &[cl, dyn_at, mode, x, xargs])
        }
    }

    /// An error reached the interpreter loop of a fiber. `dest`: where the
    /// failing instruction's value goes — `Some(Some(r))` register r,
    /// `Some(None)` returned from the frame (tail call), `None` nowhere.
    ///
    /// Runtime errors with a destination become a call to `%runtime-error`
    /// (Norn) *in place of the failed operation*: it signals a condition, and
    /// whatever a handler or restart returns becomes the operation's value
    /// (`use-value`); a `retry` re-executes it. Everything else unwinds the
    /// fiber and ends it.
    unsafe fn handle_error(&mut self, st: &mut St, e: Error, dest: Option<Option<usize>>) -> Result<Flow> {
        let Some(fk) = FRAME_KINDS.get() else { return Err(self.fail(st, e)) };
        if self.nested > 0 || self.current.is_nil() || e.kind == ErrKind::Fatal || st.seg.is_nil() {
            return Err(self.fail(st, e));
        }
        // SAFETY: st describes a valid frame chain.
        let mut lines = e.trace.clone();
        lines.extend(unsafe { self.backtrace(st) });
        let lines: Vec<Value> = lines.iter().map(|t| str_new(&mut self.m, t)).collect();
        let trace = vec_from_slice(&mut self.m, &lines);
        let msg = str_new(&mut self.m, &e.msg);
        let r = unsafe {
            match (e.kind, dest) {
                (ErrKind::Runtime, Some(d)) => self.invoke(st, fk.runtime_error, &[msg, trace], d),
                (ErrKind::Force(s, all), Some(d)) => {
                    let var = self.core_fn("%force-stream", &FORCE_VAR);
                    let force = if var.is(Kind::Var) { var.var_root() } else { var };
                    self.invoke(st, force, &[s, Value::bool(all)], d)
                }
                _ => {
                    // Honoured: let cleanups run without being cancelled again.
                    self.current.atomic_word(fiber::CANCEL).store(0, std::sync::atomic::Ordering::SeqCst);
                    let kw = rt().keyword(&mut self.m, None, "die");
                    self.unwind(st, Target::Bottom, kw, msg, trace)
                }
            }
        };
        r.map_err(|mut e2| {
            e2.kind = ErrKind::Fatal;
            self.fail(st, e2)
        })
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
            if f.is(Kind::Generic) || f.is(Kind::Class) {
                match self.redirect(f, args)? {
                    (g, None) => {
                        f = g;
                        continue;
                    }
                    (g, Some(v)) => return self.apply_inner(g, &v),
                }
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
        let pending: Option<(Error, Option<Option<usize>>)>;
        // SAFETY: frame chains in fibers are valid.
        unsafe {
            if mode == fiber::START {
                if f.fiber_cancel() != 0 {
                    return Err(cancel_error(f.fiber_cancel() as i64));
                }
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
                pending = None;
            } else {
                st = St::at(f.field(fiber::SEG), f.field(fiber::FP).as_fixnum() as usize);
                st.enter();
                let err = f.field(fiber::WAKE_ERR);
                f.set_field(fiber::WAKE_ERR, Value::NIL);
                let cancel = f.fiber_cancel();
                // Where the value of the operation the fiber parked in goes.
                let dest = match mode {
                    fiber::DELIVER | fiber::DELIVER_PAIR => Some(Some(a(*st.code.add(st.pc - 1)))),
                    fiber::RETURN | fiber::RETURN_PAIR => Some(None),
                    _ => None,
                };
                pending = if cancel != 0 {
                    Some((cancel_error(cancel as i64), None))
                } else if !err.is_nil() {
                    Some((crate::sched::wake_error(err), dest))
                } else {
                    None
                };
                let v = f.field(fiber::WAKE_VALUE);
                let deliver = match mode {
                    fiber::DELIVER | fiber::RETURN => Some(v),
                    fiber::DELIVER_PAIR | fiber::RETURN_PAIR => {
                        let aux = f.field(fiber::WAKE_AUX);
                        Some(vec_from_slice(&mut self.m, &[v, aux]))
                    }
                    _ => None,
                };
                if let Some(v) = deliver.filter(|_| pending.is_none()) {
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
            let r = match pending {
                Some((e, dest)) => match self.handle_error(&mut st, e, dest) {
                    Ok(Flow::Next) => self.run(&mut st),
                    Ok(Flow::Done(v)) => Ok(Exit::Done(v)),
                    Ok(Flow::Park) => Ok(Exit::Park),
                    Ok(Flow::Yield) => Ok(Exit::Yield),
                    Err(e) => Err(e),
                },
                None => self.run(&mut st),
            };
            if matches!(r, Ok(Exit::Park) | Ok(Exit::Yield)) {
                self.save(&st);
            }
            r
        }
    }

    /// The tier-1 entry for `proto`, compiling it on first use. 1 = none.
    fn native_for(&mut self, proto: Value) -> usize {
        let n = proto.proto_native();
        if n != 0 {
            return n;
        }
        if self.jit.prims[0] == 0 {
            for (i, name) in crate::jit::FAST_PRIMS.iter().enumerate() {
                let sym = rt().symbol(&mut self.m, None, name);
                self.jit.prims[i] = self.core.lookup(sym).map(|v| v.var_root().0).unwrap_or(u64::MAX);
            }
        }
        let entry = crate::jit::compile(proto).unwrap_or(1);
        if proto.proto_set_native(entry) { entry } else { proto.proto_native() }
    }

    /// The interpreter loop. With tier 1 on, each step first runs the frame's
    /// native code from the current pc; it stops at the first instruction it
    /// doesn't handle, which the interpreter then executes.
    unsafe fn run(&mut self, st: &mut St) -> Result<Exit> {
        let prev = std::mem::replace(&mut self.cur_st, st as *mut St);
        // SAFETY: as for run_inner.
        let r = unsafe { self.run_inner(st) };
        self.cur_st = prev;
        r
    }

    /// Run `f` in a native region: other carriers may collect meanwhile, so
    /// `f` must not touch the heap. The stack is saved first, so the current
    /// frame (and the arguments in it) stays alive; `keep` holds values the
    /// frame doesn't (tail-call arguments live in a Rust vector).
    pub(crate) fn native_region<R>(&mut self, keep: &[Value], f: impl FnOnce() -> R) -> R {
        if !self.cur_st.is_null() {
            // SAFETY: cur_st points at the live state of the enclosing run.
            unsafe { self.save(&*self.cur_st) };
        }
        let key = (!keep.is_empty()).then(|| crate::sched::root_many(keep));
        crate::sched::enter_native(self.carrier);
        let r = f();
        crate::sched::leave_native(self.carrier);
        if let Some(k) = key {
            crate::sched::unroot_many(k);
        }
        r
    }

    unsafe fn run_inner(&mut self, st: &mut St) -> Result<Exit> {
        let tier1 = crate::jit::enabled();
        unsafe {
            loop {
                if tier1 {
                    if st.native == 0 {
                        st.native = self.native_for(st.slot(F_PROTO));
                    }
                    if st.native > 1 {
                        self.jit.budget = &mut self.budget;
                        self.jit.seen = &self.seen_attention;
                        let f: crate::jit::NativeFn = std::mem::transmute(st.native);
                        st.pc = f(st.regs(), st.consts, &self.jit, st.pc as u64) as usize;
                    }
                }
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
                        let mut v = var.var_root();
                        if var.var_flags() & VAR_DYNBOUND != 0
                            && !self.dyn_env.is_nil()
                            && let Some(x) = self.dyn_env.map_get(var)
                        {
                            v = x;
                        }
                        if v == Value::UNBOUND {
                            Err(Error::new(format!("Unbound var: {}", pr_str(var, true))))
                        } else {
                            *regs.add(ra) = v;
                            continue;
                        }
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
                let flow = match flow {
                    Err(e) => {
                        // A failed TAILCALL is followed by `RETURN ra` (see the
                        // compiler), so it is handled like a failed CALL.
                        let dest = match op(ins) {
                            CALL | GETVAR | TAILCALL => Some(Some(ra)),
                            _ => None,
                        };
                        self.handle_error(st, e, dest)?
                    }
                    Ok(f) => f,
                };
                match flow {
                    Flow::Next => {}
                    Flow::Done(v) => return Ok(Exit::Done(v)),
                    Flow::Park => return Ok(Exit::Park),
                    Flow::Yield => return Ok(Exit::Yield),
                }
            }
        }
    }
}
