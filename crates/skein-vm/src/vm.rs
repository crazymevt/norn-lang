//! The tier-0 interpreter, running fibers on heap-allocated segmented stacks
//! (runtime.md §3, §5).
//!
//! Frame layout: see `skein_rt::func` (fn, proto, resume, prev, registers).
//! Frames link by *offset*, never by address.
//!
//! The run loop never recurses on Skein→Skein calls, so a fiber can stop
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
use crate::prims::{ABORT_FIBER, APPLY, EVAL, RESTART_FRAME, THROW};
use skein_rt::conc::fiber;
use skein_rt::func::{
    VAR_DYNBOUND, Capture, F_FN, F_PREV, F_PROTO, F_RESUME, HDR, PREV_BOUNDARY, PREV_SEG_START, closure_new, segment_new,
};
use skein_rt::list::list_from_with_tail;
use skein_rt::print::pr_str;
use skein_rt::runtime::{Namespace, rt};
use skein_rt::safepoint;
use skein_rt::string::str_new;
use skein_rt::vector::vec_from_slice;
use skein_rt::{Kind, Mutator, Value};

// ---- errors ---------------------------------------------------------------

/// How an error raised by Rust code is handled when it reaches a fiber's
/// interpreter loop (runtime.md §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrKind {
    /// Signalled as a Skein condition (`RuntimeError`) at the failing
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
    /// Not an error: a primitive asks for function f to be called with the
    /// arguments in vector args, in its place (so in the fiber, where it
    /// can park), its value the call's. deref hands objects to
    /// `deref-object` this way (M9 part 9.14).
    Call(Value, Value),
    /// A `throw-tag` of a value to a tag whose catch isn't on the stack of
    /// the nested run it was thrown in: carried out of the run to the Rust
    /// that started it, and thrown again where that returns to (a
    /// handler-case outside a `print-object` method, a tag reader or a
    /// macro's function catching what's signalled inside).
    Throw(Value, Value),
    /// A primitive's error that no handler inside its nested run took
    /// (`%nested-raise`): the run fails with it, as a `Runtime` error for
    /// whoever started the run.
    Declined,
}

/// A Skein error as seen from Rust, with a Skein-level backtrace.
#[derive(Debug, Clone)]
pub struct Error {
    pub msg: String,
    pub trace: Vec<String>,
    pub kind: ErrKind,
    /// The condition it becomes (M9 part 9.13); none: a `RuntimeError`.
    pub class: Option<ErrClass>,
}

/// Which condition a Rust error becomes.
#[derive(Debug, Clone)]
pub enum ErrClass {
    /// A condition class of skein.core, by name, with values for its slots.
    Named(&'static str, Vec<(&'static str, ErrData)>),
    /// A Skein value already: a condition to signal again (a failed
    /// fiber's), or `[message class-symbol data-map]` (from the blocking
    /// pool).
    Value(Value),
}

/// A slot's value in an `ErrClass::Named`.
#[derive(Debug, Clone)]
pub enum ErrData {
    Str(String),
    Int(i64),
    /// A keyword, by name.
    Kw(&'static str),
    Val(Value),
}

impl Error {
    pub fn new(msg: impl Into<String>) -> Error {
        Error { msg: msg.into(), trace: Vec::new(), kind: ErrKind::Runtime, class: None }
    }
    /// An error that becomes condition class `class` of skein.core.
    pub fn of(class: &'static str, msg: impl Into<String>) -> Error {
        Error { class: Some(ErrClass::Named(class, Vec::new())), ..Error::new(msg) }
    }
    /// The same, with slot `k` set to `v`.
    pub fn with(mut self, k: &'static str, v: ErrData) -> Error {
        if let Some(ErrClass::Named(_, data)) = &mut self.class {
            data.push((k, v));
        }
        self
    }
    pub fn abort(msg: impl Into<String>) -> Error {
        Error { msg: msg.into(), trace: Vec::new(), kind: ErrKind::Abort, class: None }
    }
    pub fn fatal(msg: impl Into<String>, trace: Vec<String>) -> Error {
        Error { msg: msg.into(), trace, kind: ErrKind::Fatal, class: None }
    }
    /// Ask the interpreter to call `f` with the arguments in vector `args`
    /// in place of the primitive that returns this.
    pub fn call(f: Value, args: Value) -> Error {
        Error { msg: "A call had to be made outside a fiber".into(), trace: Vec::new(), kind: ErrKind::Call(f, args), class: None }
    }
    /// Ask the interpreter to realize stream `s` (one cell, or `all` of it).
    pub fn force(s: Value, all: bool) -> Error {
        Error {
            msg: "A stream had to be realized outside a fiber".into(),
            trace: Vec::new(),
            kind: ErrKind::Force(s, all),
            class: None,
        }
    }
    /// An I/O error, as its class (by its kind) with :reason, and :path or
    /// :address when given.
    pub fn io(msg: impl Into<String>, e: &std::io::Error) -> Error {
        let (class, reason) = io_kind(e);
        Error::of(class, msg).with("reason", ErrData::Kw(reason))
    }
}

/// The condition class and :reason of an I/O error.
pub fn io_kind(e: &std::io::Error) -> (&'static str, &'static str) {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::NotFound => ("FileNotFound", "not-found"),
        K::PermissionDenied => ("PermissionDenied", "permission-denied"),
        K::AlreadyExists => ("FileExists", "exists"),
        K::ConnectionRefused => ("ConnectionError", "refused"),
        K::ConnectionReset => ("ConnectionError", "reset"),
        K::ConnectionAborted => ("ConnectionError", "aborted"),
        K::NotConnected => ("ConnectionError", "not-connected"),
        K::BrokenPipe => ("ConnectionError", "broken-pipe"),
        K::HostUnreachable | K::NetworkUnreachable | K::NetworkDown => ("ConnectionError", "unreachable"),
        K::AddrInUse => ("ConnectionError", "address-in-use"),
        K::AddrNotAvailable => ("ConnectionError", "address-not-available"),
        K::TimedOut => ("TimeoutError", "timed-out"),
        K::UnexpectedEof => ("IOError", "unexpected-end"),
        K::InvalidData => ("IOError", "invalid-data"),
        _ => ("IOError", "other"),
    }
}

/// A blocking-pool job's failure (it's made on a pool thread, with no
/// heap access): a message, and the condition it becomes.
#[derive(Debug, Clone)]
pub struct Failure {
    pub msg: String,
    pub class: Option<(&'static str, Vec<(&'static str, FailData)>)>,
}

/// A slot's value in a Failure (no heap values: it crosses threads).
#[derive(Debug, Clone)]
pub enum FailData {
    Str(String),
    Int(i64),
    Kw(&'static str),
}

impl Failure {
    pub fn of(class: &'static str, msg: impl Into<String>) -> Failure {
        Failure { msg: msg.into(), class: Some((class, Vec::new())) }
    }
    pub fn with(mut self, k: &'static str, v: FailData) -> Failure {
        if let Some((_, data)) = &mut self.class {
            data.push((k, v));
        }
        self
    }
    /// An I/O error about `path`: "op path: what happened", as its class.
    pub fn io_path(op: &str, path: &str, e: &std::io::Error) -> Failure {
        let (class, reason) = io_kind(e);
        Failure::of(class, format!("{op} {path}: {e}")).with("path", FailData::Str(path.to_owned())).with("reason", FailData::Kw(reason))
    }
    /// As an Error (on the thread that raises it).
    pub fn into_error(self) -> Error {
        match self.class {
            None => Error::new(self.msg),
            Some((c, data)) => {
                let data = data
                    .into_iter()
                    .map(|(k, v)| {
                        (k, match v {
                            FailData::Str(s) => ErrData::Str(s),
                            FailData::Int(n) => ErrData::Int(n),
                            FailData::Kw(w) => ErrData::Kw(w),
                        })
                    })
                    .collect();
                Error { class: Some(ErrClass::Named(c, data)), ..Error::new(self.msg) }
            }
        }
    }
}

/// Failure f as a fiber's WAKE_ERR: its message, or [message class data]
/// when it has a class.
pub fn failure_value(m: &mut Mutator, f: &Failure) -> Value {
    let msg = str_new(m, &f.msg);
    match &f.class {
        None => msg,
        Some((c, data)) => {
            let cls = rt().symbol(m, None, c);
            let mut map = skein_rt::map::map_empty();
            for (k, v) in data {
                let key = rt().keyword(m, None, k);
                let val = match v {
                    FailData::Str(s) => str_new(m, s),
                    FailData::Int(n) => Value::fix(*n),
                    FailData::Kw(w) => rt().keyword(m, None, w),
                };
                map = skein_rt::map::map_assoc(m, map, key, val);
            }
            vec_from_slice(m, &[msg, cls, map])
        }
    }
}

impl From<String> for Failure {
    fn from(msg: String) -> Failure {
        Failure { msg, class: None }
    }
}

impl From<&str> for Failure {
    fn from(msg: &str) -> Failure {
        Failure { msg: msg.to_owned(), class: None }
    }
}

impl From<String> for Error {
    fn from(msg: String) -> Error {
        // (the runtime's sequence functions say a value isn't a collection
        // in text: that's a TypeError, as (conj 1 2) is)
        if msg.starts_with("Don't know how to create a seq from") || msg.contains(" not supported on ") {
            return Error::of("TypeError", msg);
        }
        Error::new(msg)
    }
}

/// The frames of a trace an error's report shows: the program's. Left out
/// are the runtime's own (skein.core's `%…` functions: the frames of a
/// `binding`, a `try`, a scope) and everything under `<toplevel>` or from
/// the evaluator's first frame (what read and ran the form), which made a one-line mistake's
/// report ten lines long. The trace itself (a condition's `:trace`, the
/// debugger's frames) keeps them all, and `SKEIN_FULL_TRACE=1` shows them.
pub fn shown_trace(trace: &[String]) -> Vec<&String> {
    static FULL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *FULL.get_or_init(|| std::env::var("SKEIN_FULL_TRACE").is_ok_and(|v| !v.is_empty() && v != "0")) {
        return trace.iter().collect();
    }
    // (where the evaluator's frames begin: after the form's own, or at the
    // function that reads and runs a file's or the REPL's forms)
    let evaluator = |t: &String| {
        t.starts_with("skein.expander/eval-") || t.starts_with("skein.core/%repl-eval") || t == "skein.core/%eval-root" || t == "skein.core/load-file"
    };
    let end = trace.iter().position(|t| t == "<toplevel>").map(|i| i + 1).or_else(|| trace.iter().position(evaluator)).unwrap_or(trace.len());
    trace[..end].iter().filter(|t| !t.starts_with("skein.core/%")).collect()
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.msg)?;
        for t in shown_trace(&self.trace) {
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

/// Skein functions whose frames mark dynamic extent (core.skn). The unwinder
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

/// The var of skein.core/%force-stream (its frame's return can mean "retry").
static FORCE_VAR: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

pub(crate) enum Target {
    Catch(Value),
    /// A frame (segment, offset), and which call inlined into it, if the
    /// frame to restart is an inlined call's (its record's index).
    Frame(Value, usize, Option<usize>),
    Bottom,
}

/// Every frame of a stack, top first: (segment, frame offset).
/// The calls inlined (M10 part 4) at instruction pc of proto, innermost
/// first: each function's name and its debug locals ([sym reg …]). The
/// compiler records them after the debug pairs, as [start end fname debug]
/// with register -1.
///
/// regs: the frame's registers, to tell a tail call of a closure (which
/// would have replaced the inlined function's frame: a hole) from one of a
/// primitive (which runs within it).
pub(crate) fn inlined_at(proto: Value, pc: usize, regs: *const Value) -> Vec<(Value, Value)> {
    inlined_at_k(proto, pc, regs).into_iter().map(|(_, n, d)| (n, d)).collect()
}

/// inlined_at, with each call's record index (for restarting it).
pub(crate) fn inlined_at_k(proto: Value, pc: usize, regs: *const Value) -> Vec<(usize, Value, Value)> {
    // (holes count only where a closure is being called)
    let closure_called = {
        let code = proto.proto_code();
        pc < code.code_len() && {
            // SAFETY: pc is an instruction of the proto; regs its frame's.
            let i = unsafe { *code.code_ptr().add(pc) };
            matches!(op(i), CALL | TAILCALL) && !regs.is_null() && unsafe { (*regs.add(a(i))).is(Kind::Closure) }
        }
    };
    let mut found: Vec<(usize, usize, Value, Value)> = inline_records(proto)
        .enumerate()
        .filter(|(_, r)| covers(*r, pc, closure_called))
        .map(|(k, r)| (r.vec_nth(1).as_fixnum() as usize - r.vec_nth(0).as_fixnum() as usize, k, r.vec_nth(2), r.vec_nth(3)))
        .collect();
    found.sort_by_key(|x| x.0);
    found.into_iter().map(|(_, k, n, d)| (k, n, d)).collect()
}

/// Is instruction pc of proto code inlined from another function (holes or
/// not: see inlined_at)?
pub(crate) fn in_inlined(proto: Value, pc: usize) -> bool {
    inline_records(proto).any(|r| covers(r, pc, false))
}

/// A proto's inline records: [start end fname debug holes], the holes
/// [[from to]…] where the inlined function would have tail-called (its
/// frame gone, so it isn't shown).
fn inline_records(proto: Value) -> impl Iterator<Item = Value> {
    let dbg = proto.proto_locals();
    let pairs: &[Value] = if dbg.is(Kind::Array) { dbg.arr_slice() } else { &[] };
    pairs.chunks(2).map(|p| p[0]).filter(|r| r.is(Kind::Vector) && r.vec_count() >= 5)
}

fn covers(r: Value, pc: usize, holes: bool) -> bool {
    let (s, e) = (r.vec_nth(0).as_fixnum() as usize, r.vec_nth(1).as_fixnum() as usize);
    if pc < s || pc >= e {
        return false;
    }
    let hs = r.vec_nth(4);
    !(holes
        && (0..hs.vec_count()).any(|i| {
            let h = hs.vec_nth(i);
            (h.vec_nth(0).as_fixnum() as usize..h.vec_nth(1).as_fixnum() as usize).contains(&pc)
        }))
}

/// The instruction a frame is at: the one before its resume point (its
/// call, or the one the top frame was running).
pub(crate) fn frame_pc(resume: Value) -> usize {
    (resume.as_fixnum().max(1) - 1) as usize
}

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
/// Reductions (calls + loop iterations) between looks at whether the time
/// slice is over.
const BUDGET: i64 = 2000;
/// A time slice's least length. Reductions alone would give fast code
/// (tier 1's native calls: 2000 of them in about 100 µs) a smaller share of
/// a busy carrier than slow code; so a fiber yields once its budget is
/// spent *and* it has run this long.
const MIN_SLICE: std::time::Duration = std::time::Duration::from_micros(500);

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
    /// Switch to the frame whose registers are at `regs` in this segment
    /// (where native code stopped), keeping the pc to be set.
    #[inline]
    unsafe fn switch_to(&mut self, regs: *mut Value) {
        unsafe {
            self.fp = regs.offset_from(self.base) as usize - HDR;
            let p = self.slot(F_PROTO);
            self.code = p.proto_code().code_ptr();
            self.consts = p.proto_consts().arr_ptr();
            self.native = 0;
        }
    }
    /// Make this the frame whose registers are at `regs` (in this segment)
    /// if it isn't: tier-1 code pushes and pops frames inline, within the
    /// segment, without telling the interpreter (jit.rs, linked calls).
    #[inline]
    unsafe fn sync(&mut self, regs: *mut Value) {
        unsafe {
            if regs != self.regs() || (*regs.sub(HDR).add(F_PROTO)).proto_code().code_ptr() != self.code {
                self.switch_to(regs);
            }
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
    /// For each run `apply` has under way: its depth and the handlers
    /// (`*handlers*`) in force as it began. Handlers before those in the
    /// list were established inside the run.
    nested_base: Vec<(u32, Value)>,
    pub(crate) want_gc: bool,
    /// The running fiber's dynamic environment (map var → value, or nil).
    pub(crate) dyn_env: Value,
    spare: Value,
    scratch: Vec<Value>,
    key_buf: Vec<Value>,
    budget: i64,
    /// When the running fiber's time slice began (sched::run_one).
    pub(crate) slice_start: std::time::Instant,
    seen_attention: u64,
    /// The profiler's sample epoch this carrier last sampled at.
    prof_seen: u64,
    jit: crate::jit::JitCtx,
    /// The interpreter state of the innermost `run`, so a primitive can save
    /// the stack before a native region (null outside `run`).
    cur_st: *mut St,
    /// What a primitive called from tier-1 code left for the interpreter:
    /// its error, or its request to park (jit_call).
    jit_pending: Option<JitPending>,
}

/// See Vm::jit_pending.
pub(crate) enum JitPending {
    Error(Error),
    Suspend(Suspend),
}

/// The arity of closure f that takes argc arguments, as select_arity
/// chooses it, when f is a closure and has one: the calls' fast path
/// (jit_resolve does the rest, errors included).
#[inline]
fn closure_arity(f: Value, argc: usize) -> Option<Value> {
    if !f.is(Kind::Closure) {
        return None;
    }
    let ps = f.closure_template().template_arities();
    let ps = ps.arr_slice();
    ps.iter().copied().find(|p| !p.proto_variadic() && p.proto_nfixed() == argc).or_else(|| {
        ps.iter().copied().find(|p| p.proto_variadic() && argc >= p.proto_nfixed())
    })
}

/// Whether calling f means working out what it calls (jit_resolve): not
/// for a primitive or a data function (a keyword, a map…), called as is.
#[inline]
fn needs_resolving(f: Value) -> bool {
    f.is(Kind::Closure) || f.is(Kind::Var) || f.is(Kind::Generic) || f.is(Kind::Class)
}

/// What a call from tier-1 code reaches, worked out as Vm::invoke does
/// (through vars, generic functions' dispatch and classes): a closure and
/// the arity to push, or a primitive or data function to call; with the
/// arguments when dispatch changed them. Nothing has happened yet, so the
/// interpreter can still make the call instead.
enum Callee {
    Frame(Value, Value, Option<Vec<Value>>),
    Value(Value, Option<Vec<Value>>),
    Failed(Error),
}

#[inline(never)]
fn jit_resolve(vm: &mut Vm, mut f: Value, args: &[Value]) -> Callee {
    let mut owned: Option<Vec<Value>> = None;
    loop {
        if f.is(Kind::Closure) {
            let n = owned.as_ref().map_or(args.len(), Vec::len);
            return match select_arity(f, n) {
                Ok(p) => Callee::Frame(f, p, owned),
                Err(e) => Callee::Failed(e),
            };
        }
        if f.is(Kind::Var) {
            match Vm::deref_var(f) {
                Ok(g) => f = g,
                Err(e) => return Callee::Failed(e),
            }
            continue;
        }
        if f.is(Kind::Generic) || f.is(Kind::Class) {
            match vm.redirect(f, owned.as_deref().unwrap_or(args)) {
                Ok((g, None)) => f = g,
                Ok((g, Some(v))) => {
                    f = g;
                    owned = Some(v);
                }
                Err(e) => return Callee::Failed(e),
            }
            continue;
        }
        return Callee::Value(f, owned);
    }
}

/// The Vm and the interpreter state of the run tier-1 code was entered
/// from. The helpers below keep that state right as they push and pop
/// frames, so when native code stops the interpreter carries on from it.
unsafe fn jit_vm<'a>(ctx: *mut crate::jit::JitCtx) -> (&'a mut Vm, &'a mut St) {
    unsafe {
        let vm = &mut *((*ctx).vm as *mut Vm);
        let st = &mut *vm.cur_st;
        (vm, st)
    }
}

/// Where native code goes on in the frame st is now in.
unsafe fn jit_next(ctx: *mut crate::jit::JitCtx, st: &St) {
    unsafe {
        (*ctx).next_regs = st.regs() as u64;
        (*ctx).next_native = crate::jit::entry_for(st.slot(F_PROTO)) as u64;
        (*ctx).next_consts = st.consts as u64;
        (*ctx).next_pc = st.pc as u64;
        jit_segment(ctx, st);
    }
}

/// The current segment's bounds, for tier-1 code's inline frames.
#[inline]
unsafe fn jit_segment(ctx: *mut crate::jit::JitCtx, st: &St) {
    unsafe {
        (*ctx).seg_base = st.base as u64;
        (*ctx).seg_end = st.base.add(st.seg.seg_capacity()) as u64;
    }
}

/// CALL ra argc from tier-1 code (jit.rs): push a closure's frame (PUSHED),
/// as Vm::push_frame does; or call a primitive or a data function (VALUE,
/// ctx.value; PENDING when it failed or asked to park, or the call failed).
/// Vars, generic functions and classes are followed to what they call.
/// NOT_MINE: the interpreter makes the call (a primitive that needs the
/// interpreter: apply, eval, throw…; a stack overflow to raise).
///
/// # Safety
/// From native code only: ctx is the carrier's, regs the running frame's.
pub(crate) unsafe extern "C" fn jit_call(ctx: *mut crate::jit::JitCtx, regs: *mut Value, ra: u64, argc: u64, resume: u64) -> u64 {
    unsafe {
        let (vm, st) = jit_vm(ctx);
        st.sync(regs);
        let (ra, argc) = (ra as usize, argc as usize);
        let args = std::slice::from_raw_parts(regs.add(ra + 1), argc);
        let f = *regs.add(ra);
        if let Some(proto) = closure_arity(f, argc) {
            st.pc = resume as usize;
            if !vm.native_push(st, f, proto, args) {
                return crate::jit::NOT_MINE;
            }
            jit_next(ctx, st);
            return crate::jit::PUSHED;
        }
        if !needs_resolving(f) {
            return jit_call_value(ctx, f, args, resume as usize);
        }
        match jit_resolve(vm, f, args) {
            Callee::Frame(f, proto, owned) => {
                st.pc = resume as usize;
                if !vm.native_push(st, f, proto, owned.as_deref().unwrap_or(args)) {
                    return crate::jit::NOT_MINE;
                }
                jit_next(ctx, st);
                crate::jit::PUSHED
            }
            Callee::Value(f, owned) => jit_call_value(ctx, f, owned.as_deref().unwrap_or(args), resume as usize),
            Callee::Failed(e) => {
                vm.jit_pending = Some(JitPending::Error(e));
                crate::jit::PENDING
            }
        }
    }
}

/// CLOSURE ra bx from tier-1 code: make the closure of template consts[bx]
/// into register ra, as the interpreter does. The running function is in
/// the frame header below regs.
///
/// # Safety
/// As jit_call's.
pub(crate) unsafe extern "C" fn jit_closure(ctx: *mut crate::jit::JitCtx, regs: *mut Value, ra: u64, bx: u64) -> u64 {
    unsafe {
        let (vm, _) = jit_vm(ctx);
        let fr = regs.sub(HDR);
        let tmpl = *(*fr.add(F_PROTO)).proto_consts().arr_ptr().add(bx as usize);
        let c = closure_new(&mut vm.m, tmpl);
        let cur = *fr.add(F_FN);
        for (i, &cap) in tmpl.template_captures().arr_slice().iter().enumerate() {
            let v = match Capture::decode(cap) {
                Capture::Local(r) => *regs.add(r as usize),
                Capture::Up(j) => cur.closure_upval(j as usize),
                Capture::SelfRef => cur,
            };
            c.closure_set_upval(i, v);
        }
        *regs.add(ra as usize) = c;
        0
    }
}

/// A float for tier-1 code, from its bits (M10 part 5.2). Allocating never
/// collects, so native code's other values stay where they are.
///
/// # Safety
/// As jit_call's.
pub(crate) unsafe extern "C" fn jit_box_float(ctx: *mut crate::jit::JitCtx, bits: u64) -> u64 {
    unsafe {
        let (vm, _) = jit_vm(ctx);
        skein_rt::string::float_new(&mut vm.m, f64::from_bits(bits)).0
    }
}

/// TAILCALL ra argc from tier-1 code: as jit_call, the closure's frame in
/// place of this one (Vm::tail_frame); a primitive's value is returned by
/// the native code.
///
/// # Safety
/// As jit_call's.
pub(crate) unsafe extern "C" fn jit_tail(ctx: *mut crate::jit::JitCtx, regs: *mut Value, ra: u64, argc: u64, resume: u64) -> u64 {
    unsafe {
        let (vm, st) = jit_vm(ctx);
        st.sync(regs);
        let (ra, argc) = (ra as usize, argc as usize);
        let args = std::slice::from_raw_parts(regs.add(ra + 1), argc);
        let f = *regs.add(ra);
        let callee = match closure_arity(f, argc) {
            Some(proto) => Callee::Frame(f, proto, None),
            None if !needs_resolving(f) => Callee::Value(f, None),
            None => jit_resolve(vm, f, args),
        };
        match callee {
            Callee::Frame(f, proto, owned) => {
                // (the arguments are in the frame being overwritten)
                let mut scratch = std::mem::take(&mut vm.scratch);
                scratch.clear();
                scratch.extend_from_slice(owned.as_deref().unwrap_or(args));
                let done = vm.native_tail(st, f, proto, &scratch);
                vm.scratch = scratch;
                if !done {
                    return crate::jit::NOT_MINE;
                }
                jit_next(ctx, st);
                crate::jit::PUSHED
            }
            Callee::Value(f, owned) => jit_call_value(ctx, f, owned.as_deref().unwrap_or(args), resume as usize),
            Callee::Failed(e) => {
                vm.jit_pending = Some(JitPending::Error(e));
                crate::jit::PENDING
            }
        }
    }
}

/// RETURN from tier-1 code: pop the frame and deliver v to its caller, as
/// Vm::do_return does (back across a segment boundary too). The caller's
/// registers (ctx says where to resume), or 0 when the interpreter must do
/// it (the bottom of the run, or a retry).
///
/// # Safety
/// As jit_call's.
pub(crate) unsafe extern "C" fn jit_return(ctx: *mut crate::jit::JitCtx, regs: *mut Value, v: u64) -> u64 {
    unsafe {
        let (vm, st) = jit_vm(ctx);
        st.sync(regs);
        let v = Value(v);
        if v == Value::RETRY {
            return 0;
        }
        match st.slot(F_PREV).as_fixnum() {
            PREV_BOUNDARY => return 0,
            PREV_SEG_START => {
                let old = st.seg;
                st.seg = old.seg_prev();
                st.base = st.seg.seg_data();
                st.fp = old.seg_prev_fp();
                vm.give_segment(old);
            }
            p => st.fp = p as usize,
        }
        st.enter();
        let call = *st.code.add(st.pc - 1);
        *st.regs().add(a(call)) = v;
        jit_next(ctx, st);
        st.regs() as u64
    }
}

/// Call a primitive or a data function (a keyword, a map…) for tier-1 code,
/// as Vm::invoke does (st is at this frame already, for a primitive that
/// looks at the stack or saves it before a native region).
unsafe fn jit_call_value(ctx: *mut crate::jit::JitCtx, f: Value, args: &[Value], pc: usize) -> u64 {
    unsafe {
        let (vm, st) = jit_vm(ctx);
        let prim = f.is(Kind::Primitive);
        if prim && matches!(f.prim_index(), APPLY | EVAL | THROW | RESTART_FRAME | ABORT_FIBER) {
            return crate::jit::NOT_MINE;
        }
        st.pc = pc;
        let r = if prim { vm.call_prim(f, args) } else { vm.call_data(f, args) };
        match r {
            Ok(v) => {
                if let Some(s) = vm.suspend.take() {
                    vm.jit_pending = Some(JitPending::Suspend(s));
                    return crate::jit::PENDING;
                }
                (*ctx).value = v.0;
                // (a nested run on this carrier has set the bounds to its own)
                jit_segment(ctx, st);
                crate::jit::VALUE
            }
            Err(e) => {
                vm.jit_pending = Some(JitPending::Error(e));
                crate::jit::PENDING
            }
        }
    }
}

fn arity_error(f: Value, n: usize) -> Error {
    Error::of("ArityError", format!("Wrong number of args ({n}) passed to: {}", pr_str(f, true)))
        .with("function", ErrData::Val(f))
        .with("given", ErrData::Int(n as i64))
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
            nested_base: Vec::new(),
            want_gc: false,
            dyn_env: Value::NIL,
            spare: Value::NIL,
            scratch: Vec::new(),
            key_buf: Vec::new(),
            budget: BUDGET,
            slice_start: std::time::Instant::now(),
            seen_attention: 0,
            prof_seen: 0,
            cur_st: std::ptr::null_mut(),
            jit_pending: None,
            jit: crate::jit::JitCtx::new(),
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
            self.poll(st, true)
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
            self.poll(st, true)
        }
    }

    /// push_frame for tier-1 code (jit_call), without the poll: native code
    /// checks the safepoint. (Allocating, a rest list or a segment, is
    /// safe here: allocation never collects.) False, changing nothing, on a
    /// stack overflow, which the interpreter raises.
    unsafe fn native_push(&mut self, st: &mut St, f: Value, proto: Value, args: &[Value]) -> bool {
        unsafe {
            let need = HDR + proto.proto_nregs();
            let cur_end = st.fp + HDR + st.slot(F_PROTO).proto_nregs();
            if cur_end + need <= st.seg.seg_capacity() {
                st.set_resume(st.pc);
                self.write_frame(st.base, cur_end, f, proto, st.fp as i64, args);
                st.fp = cur_end;
            } else {
                let depth = st.seg.seg_depth() + cur_end;
                if depth > MAX_STACK_WORDS {
                    return false;
                }
                st.set_resume(st.pc);
                let s = self.take_segment(need, (st.seg.seg_capacity() * 2).min(MAX_SEG_WORDS));
                st.seg.seg_set_top(st.fp as i64);
                s.seg_link(st.seg, st.fp, depth);
                self.write_frame(s.seg_data(), 0, f, proto, PREV_SEG_START, args);
                st.seg = s;
                st.base = s.seg_data();
                st.fp = 0;
            }
            st.enter();
            true
        }
    }

    /// tail_frame for tier-1 code (jit_tail), as native_push is push_frame's.
    unsafe fn native_tail(&mut self, st: &mut St, f: Value, proto: Value, args: &[Value]) -> bool {
        unsafe {
            let need = HDR + proto.proto_nregs();
            let prev = st.slot(F_PREV).as_fixnum();
            if st.fp + need <= st.seg.seg_capacity() {
                self.write_frame(st.base, st.fp, f, proto, prev, args);
            } else {
                let s = self.take_segment(need, (st.seg.seg_capacity() * 2).min(MAX_SEG_WORDS));
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
            true
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

    /// The safepoint poll at function entry (`entry`) and loop back-edges:
    /// one decrement and one load on the fast path.
    #[inline]
    unsafe fn poll(&mut self, st: &mut St, entry: bool) -> Result<Flow> {
        self.budget -= 1;
        if self.budget > 0 && safepoint::attention() == self.seen_attention {
            return Ok(Flow::Next);
        }
        self.poll_slow(st, entry)
    }

    #[cold]
    fn poll_slow(&mut self, st: &mut St, entry: bool) -> Result<Flow> {
        self.seen_attention = safepoint::attention();
        if crate::prof::due(&mut self.prof_seen) {
            // (at a function's entry it has run nothing yet: the time since
            // the sample was asked for was its caller's)
            crate::prof::sample(st.seg, st.fp, entry, st.pc);
        }
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
            if self.can_park()
                && (pausing || (crate::sched::has_other_work(self.carrier) && self.slice_start.elapsed() >= MIN_SLICE))
            {
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
    pub(crate) fn core_fn(&mut self, name: &str, cell: &'static std::sync::OnceLock<Value>) -> Value {
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
    /// miss calls `%gf-miss` (Skein) in this fiber, which computes the
    /// effective method, caches it and calls it.
    fn redirect(&mut self, f: Value, args: &[Value]) -> Result<(Value, Option<Vec<Value>>)> {
        use skein_rt::clos::generic;
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
        let hit = skein_rt::clos::dispatch_cached(f, args, &mut key);
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
        let r = self.call_prim_inner(f, args);
        // (a profile's sample due while it ran is its time: the calling
        // frame's, not the next to reach a safepoint)
        if crate::prof::due(&mut self.prof_seen) && !self.cur_st.is_null() {
            // SAFETY: cur_st is the running St while a run is under way.
            let st = unsafe { &*self.cur_st };
            crate::prof::sample(st.seg, st.fp, false, st.pc.saturating_sub(1));
        }
        r
    }

    fn call_prim_inner(&mut self, f: Value, args: &[Value]) -> Result<Value> {
        if args.len() < f.prim_min() || args.len() > f.prim_max() {
            return Err(arity_error(f, args.len()));
        }
        loop {
            return match (crate::ext::prim(f.prim_index()).f)(self, args) {
                // (a run from Rust can't hand the call to the interpreter: make
                // it here, nested; it can't park)
                Err(Error { kind: ErrKind::Call(g, a), .. }) if self.nested > 0 => {
                    let mut argv = Vec::new();
                    a.vec_for_each(|x| {
                        argv.push(x);
                        true
                    });
                    self.apply(g, &argv)
                }
                Err(e) if e.kind == ErrKind::Runtime => match args.iter().find_map(|&a| skein_rt::seq::first_unrealized(a)) {
                    Some(s) => {
                        let one_step = matches!(crate::ext::prim(f.prim_index()).name, "first" | "rest" | "next" | "seq" | "nth" | "empty?");
                        if self.nested > 0 {
                            // A run from Rust realizes the stream here and
                            // calls the primitive again. (Handed up, the
                            // fiber's interpreter would run its own
                            // instruction again, and so this whole run, with
                            // a new stream each time: a loop that never ends.)
                            let var = self.core_fn("%force-stream", &FORCE_VAR);
                            let force = if var.is(Kind::Var) { var.var_root() } else { var };
                            self.apply(force, &[s, Value::bool(!one_step)])?;
                            continue;
                        }
                        Err(Error::force(s, !one_step))
                    }
                    None => Err(e),
                },
                r => r,
            };
        }
    }

    /// Calling a non-function value: keywords, maps, sets (sorted ones
    /// too) and vectors are functions of their keys (Clojure semantics).
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
            Kind::Sorted => {
                // (a sorted map or set, as a map or set is)
                if args.is_empty() || args.len() > 2 {
                    return Err(arity_error(f, args.len()));
                }
                let d = args.get(1).copied().unwrap_or(Value::NIL);
                Ok(crate::sorted::get(f, args[0])?.unwrap_or(d))
            }
            Kind::Vector => {
                if args.len() != 1 {
                    return Err(arity_error(f, args.len()));
                }
                let i = args[0];
                if i.is_fixnum() && i.as_fixnum() >= 0 && (i.as_fixnum() as usize) < f.vec_count() {
                    Ok(f.vec_nth(i.as_fixnum() as usize))
                } else {
                    Err(Error::of("IndexError", format!("Index out of bounds: {}", pr_str(i, true))).with("index", ErrData::Val(i)))
                }
            }
            _ => Err(not_fn()),
        }
    }

    fn deref_var(f: Value) -> Result<Value> {
        let v = f.var_root();
        if v == Value::UNBOUND {
            return Err(Error::of("UnboundVarError", format!("Unbound var: {}", pr_str(f, true))).with("var", ErrData::Val(f)));
        }
        Ok(v)
    }

    /// `(apply f a b [c d])` → (f, [a b c d]).
    pub(crate) fn spread_apply(&mut self, args: &[Value]) -> Result<(Value, Vec<Value>)> {
        let (f, mid) = (args[0], &args[1..args.len() - 1]);
        let last = args[args.len() - 1];
        let mut v = mid.to_vec();
        v.extend(skein_rt::seq::to_vec(&mut self.m, last)?);
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
                            let k = args.get(2).filter(|k| k.is_fixnum()).map(|k| k.as_fixnum() as usize);
                            let target = Target::Frame(args[0], args[1].as_fixnum() as usize, k);
                            return self.unwind(st, target, kw, Value::NIL, Value::NIL);
                        }
                        ABORT_FIBER => {
                            // (dying: the message, and [trace condition], the
                            // condition kept for whoever awaits the fiber)
                            let kw = rt().keyword(&mut self.m, None, "die");
                            let cond = args.get(2).copied().unwrap_or(Value::NIL);
                            // (a condition error made has no trace of its
                            // own: the stack here, but the frames that are
                            // the condition system's handing it on)
                            let given = args[1];
                            let trace = if given.is_nil() || (given.is(Kind::Vector) && given.vec_count() == 0) {
                                let lines: Vec<String> = self.backtrace(st)
                                    .into_iter()
                                    .skip_while(|t| {
                                        matches!(
                                            t.as_str(),
                                            "skein.core/%abort" | "skein.core/invoke-debugger" | "skein.core/error" | "skein.core/throw" | "skein.core/signal"
                                        )
                                    })
                                    .collect();
                                let lines: Vec<Value> = lines.iter().map(|t| str_new(&mut self.m, t)).collect();
                                vec_from_slice(&mut self.m, &lines)
                            } else {
                                given
                            };
                            let how = vec_from_slice(&mut self.m, &[trace, cond]);
                            return self.unwind(st, Target::Bottom, kw, args[0], how);
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

    /// Skein-level backtrace from the current frame to the bottom.
    unsafe fn backtrace(&self, st: &St) -> Vec<String> {
        let mut out = Vec::new();
        if st.seg.is_nil() {
            return out;
        }
        let (mut seg, mut fp) = (st.seg, st.fp);
        unsafe {
            for k in 0..64 {
                let fr = seg.seg_data().add(fp);
                let proto = *fr.add(F_PROTO);
                // (the calls inlined where it is, as frames of their own)
                let pc = if k == 0 { st.pc.max(1) - 1 } else { frame_pc(*fr.add(F_RESUME)) };
                for (n, _) in inlined_at(proto, pc, fr.add(HDR)) {
                    out.push(pr_str(n, true));
                }
                let name = proto.proto_name();
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
            return Err(Error::fatal("unwinding before core.skn is loaded", vec![]));
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
                    Target::Frame(s, p, _) => s == seg && p == fp,
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
                    // (carried out of a nested run: its catch may be outside)
                    Target::Catch(tag) if self.nested > 0 => {
                        Error { kind: ErrKind::Throw(tag, x), ..Error::new(format!("No catch for tag {}", pr_str(tag, true))) }
                    }
                    Target::Catch(tag) => Error::new(format!("No catch for tag {}", pr_str(tag, true))),
                    _ => Error::new("That frame is not on this fiber's stack"),
                });
            };
            let (mut x, mut xargs) = (x, xargs);
            if let Target::Frame(_, _, Some(k)) = target {
                // Restart a call inlined into the frame: the frame stays, and
                // goes on as if its ordinary call of the function (the code
                // after the inlined one) had been made, to the var's current
                // definition with the arguments still in its registers: the
                // call is pushed above it, its value delivered to that call's
                // register, the frame resuming after it.
                let fr = seg.seg_data().add(fp);
                let (proto, regs) = (*fr.add(F_PROTO), fr.add(HDR));
                let Some(r) = inline_records(proto).nth(k).filter(|r| r.vec_count() == 9) else {
                    return Err(Error::new("That inlined call can't be restarted"));
                };
                let (slow, base, var, n) =
                    (r.vec_nth(5).as_fixnum() as usize, r.vec_nth(6).as_fixnum() as usize, r.vec_nth(7), r.vec_nth(8).as_fixnum() as usize);
                let a: Vec<Value> = (0..n).map(|i| *regs.add(base + 1 + i)).collect();
                x = if var.is(Kind::Var) { var.var_root() } else { *regs.add(base) };
                xargs = vec_from_slice(&mut self.m, &a);
                let cl = vec_from_slice(&mut self.m, &cleanups);
                st.seg = seg;
                st.base = seg.seg_data();
                st.fp = fp;
                st.enter();
                st.pc = slow + 1;
                st.set_resume(st.pc);
                let finish = fk.finish;
                let proto = select_arity(finish, 5)?;
                return self.push_frame(st, finish, proto, &[cl, dyn_at, mode, x, xargs]);
            }
            if let Target::Frame(..) = target {
                // Restart the frame: its function's *current* definition, with
                // the arguments it was called with.
                let fr = seg.seg_data().add(fp);
                let (f, proto, regs) = (*fr.add(F_FN), *fr.add(F_PROTO), fr.add(HDR));
                let mut a: Vec<Value> = (0..proto.proto_nfixed()).map(|i| *regs.add(i)).collect();
                if proto.proto_variadic() {
                    a.extend(skein_rt::seq::to_vec(&mut self.m, *regs.add(proto.proto_nfixed()))?);
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
    /// (Skein) *in place of the failed operation*: it signals a condition, and
    /// whatever a handler or restart returns becomes the operation's value
    /// (`use-value`); a `retry` re-executes it. Everything else unwinds the
    /// fiber and ends it.
    unsafe fn handle_error(&mut self, st: &mut St, mut e: Error, dest: Option<Option<usize>>) -> Result<Flow> {
        let Some(fk) = FRAME_KINDS.get() else { return Err(self.fail(st, e)) };
        // A throw carried out of a nested run: thrown again here, where
        // the Rust that started the run was called from. With no catch on
        // this stack either, it's carried on out of this run, or (in a
        // fiber's own run) is the error a throw without a catch always was.
        if let ErrKind::Throw(tag, x) = e.kind {
            if st.seg.is_nil() || (self.nested == 0 && self.current.is_nil()) {
                e.kind = ErrKind::Runtime;
            } else {
                let kw = rt().keyword(&mut self.m, None, "return");
                // SAFETY: st describes a valid frame chain.
                match unsafe { self.unwind(st, Target::Catch(tag), kw, x, Value::NIL) } {
                    Ok(flow) => return Ok(flow),
                    Err(e2) if matches!(e2.kind, ErrKind::Throw(..)) => {
                        // SAFETY: st describes a valid frame chain.
                        unsafe { self.nested_cleanups(st, fk) };
                        return Err(e2);
                    }
                    Err(e2) => e = e2,
                }
            }
        }
        if e.kind == ErrKind::Declined {
            e.kind = ErrKind::Runtime;
            if self.nested > 0 && !st.seg.is_nil() {
                // (its trace was made where it happened)
                // SAFETY: st describes a valid frame chain.
                unsafe { self.nested_cleanups(st, fk) };
                return Err(e);
            }
        }
        // A primitive's error in a nested run with handlers of its own (a
        // `try` in a print-object method): signalled to those, in place of
        // the failed operation as in a fiber's own run, so they can catch
        // it or give a value. If none takes it the run fails with it
        // (`%nested-raise`), and the handlers outside see it where the run
        // was started from.
        if self.nested > 0
            && !st.seg.is_nil()
            && e.kind == ErrKind::Runtime
            && let (Some(d), Some(&(depth, base))) = (dest, self.nested_base.last())
            && depth == self.nested
            && self.handlers_now() != base
        {
            let mut lines = e.trace.clone();
            // SAFETY: st describes a valid frame chain.
            lines.extend(unsafe { self.backtrace(st) });
            let lines: Vec<Value> = lines.iter().map(|t| str_new(&mut self.m, t)).collect();
            let trace = vec_from_slice(&mut self.m, &lines);
            let msg = str_new(&mut self.m, &e.msg);
            let (cls, data, cond) = self.class_values(&e);
            // SAFETY: st describes the current frame.
            let r = unsafe { self.invoke(st, fk.runtime_error, &[msg, trace, cls, data, cond, base], d) };
            return r.map_err(|e2| {
                let e2 = self.fail(st, e2);
                // SAFETY: st describes a valid frame chain.
                unsafe { self.nested_cleanups(st, fk) };
                e2
            });
        }
        if self.nested > 0 && !st.seg.is_nil() {
            let e = self.fail(st, e);
            // SAFETY: st describes a valid frame chain.
            unsafe { self.nested_cleanups(st, fk) };
            return Err(e);
        }
        if self.nested > 0 || self.current.is_nil() || e.kind == ErrKind::Fatal || st.seg.is_nil() {
            return Err(self.fail(st, e));
        }
        // (a primitive handing a call over: no error, so no trace to make)
        let handed = unsafe {
            match (&e.kind, dest) {
                (ErrKind::Call(f, args), Some(d)) => {
                    let mut argv = Vec::new();
                    args.vec_for_each(|a| {
                        argv.push(a);
                        true
                    });
                    Some(self.invoke(st, *f, &argv, d))
                }
                (ErrKind::Force(s, all), Some(d)) => {
                    let var = self.core_fn("%force-stream", &FORCE_VAR);
                    let force = if var.is(Kind::Var) { var.var_root() } else { var };
                    Some(self.invoke(st, force, &[*s, Value::bool(*all)], d))
                }
                _ => None,
            }
        };
        if let Some(r) = handed {
            return r.map_err(|mut e2| {
                e2.kind = ErrKind::Fatal;
                self.fail(st, e2)
            });
        }
        // SAFETY: st describes a valid frame chain.
        let mut lines = e.trace.clone();
        lines.extend(unsafe { self.backtrace(st) });
        let lines: Vec<Value> = lines.iter().map(|t| str_new(&mut self.m, t)).collect();
        let trace = vec_from_slice(&mut self.m, &lines);
        let msg = str_new(&mut self.m, &e.msg);
        let r = unsafe {
            match (e.kind, dest) {
                (ErrKind::Runtime, Some(d)) => {
                    let (cls, data, cond) = self.class_values(&e);
                    self.invoke(st, fk.runtime_error, &[msg, trace, cls, data, cond], d)
                }
                _ => {
                    // Honoured: let cleanups run without being cancelled again
                    // (remembering it was a cancel, for the fiber's scope).
                    let code = self.current.atomic_word(fiber::CANCEL).swap(0, std::sync::atomic::Ordering::SeqCst);
                    if code != 0 {
                        self.current.set_word(fiber::CANCELLED_WITH, code);
                    }
                    let kw = rt().keyword(&mut self.m, None, "die");
                    let how = vec_from_slice(&mut self.m, &[trace, Value::NIL]);
                    self.unwind(st, Target::Bottom, kw, msg, how)
                }
            }
        };
        r.map_err(|mut e2| {
            e2.kind = ErrKind::Fatal;
            self.fail(st, e2)
        })
    }

    /// A nested run failed (no handler inside it took the error), or a
    /// throw is leaving it. The error goes back to the Rust that called
    /// it, past the run's frames. Run the cleanups of their `%unwind-protect*` frames
    /// (`finally`, a library load's bookkeeping), innermost first, each in
    /// its own dynamic environment, as unwinding does. An error in one is
    /// dropped: the run's own is the one reported.
    unsafe fn nested_cleanups(&mut self, st: &St, fk: &FrameKinds) {
        let mut cleanups = Vec::new();
        for (seg, fp) in walk_frames(st.seg, st.fp) {
            // SAFETY: (seg, fp) is a live frame.
            unsafe {
                let fr = seg.seg_data().add(fp);
                if *fr.add(F_FN) == fk.unwind {
                    let regs = fr.add(HDR);
                    cleanups.push((*regs.add(1), *regs.add(2)));
                }
            }
        }
        for (cleanup, dyn_env) in cleanups {
            self.dyn_env = dyn_env;
            let _ = self.apply(cleanup, &[]);
        }
    }

    /// Error e's class as Skein values for `%runtime-error`: (class symbol,
    /// data map, condition to signal again), nil where there's none.
    fn class_values(&mut self, e: &Error) -> (Value, Value, Value) {
        match &e.class {
            None => (Value::NIL, Value::NIL, Value::NIL),
            Some(ErrClass::Named(name, data)) => {
                let cls = rt().symbol(&mut self.m, None, name);
                let mut map = skein_rt::map::map_empty();
                for (k, v) in data {
                    let key = rt().keyword(&mut self.m, None, k);
                    let val = match v {
                        ErrData::Str(s) => str_new(&mut self.m, s),
                        ErrData::Int(n) => Value::fix(*n),
                        ErrData::Kw(w) => rt().keyword(&mut self.m, None, w),
                        ErrData::Val(v) => *v,
                    };
                    map = skein_rt::map::map_assoc(&mut self.m, map, key, val);
                }
                (cls, map, Value::NIL)
            }
            Some(ErrClass::Value(v)) if v.is(Kind::Vector) && v.vec_count() == 3 => (v.vec_nth(1), v.vec_nth(2), Value::NIL),
            Some(ErrClass::Value(v)) => (Value::NIL, Value::NIL, *v),
        }
    }

    /// `*handlers*` as it's bound now: the condition handlers in force.
    fn handlers_now(&mut self) -> Value {
        static HANDLERS: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
        let var = match HANDLERS.get() {
            Some(&v) => v,
            None => {
                let sym = rt().symbol(&mut self.m, None, "*handlers*");
                match self.core.lookup(sym) {
                    Some(v) if v.is(Kind::Var) => *HANDLERS.get_or_init(|| v),
                    _ => return Value::NIL, // (boot: before core.skn defines it)
                }
            }
        };
        match self.dyn_env.is(Kind::Map).then(|| self.dyn_env.map_get(var)).flatten() {
            Some(v) => v,
            None => var.var_root(),
        }
    }

    // ---- entry points ----

    /// Call any callable from Rust: a *nested* run on a fresh stack. It can't
    /// park or yield, and GC is inhibited until it returns.
    ///
    /// A primitive's error is signalled to the handlers established inside
    /// the run (`handle_error`); one that none of them takes fails the run,
    /// for the handlers outside. A failure leaves the run's frames without
    /// running them on: their cleanups run (`nested_cleanups`), but no
    /// `binding` or `in-ns` is undone. So the dynamic environment and the namespace are put back
    /// as they were, or the caller would go on in the failed code's (with
    /// its handlers, whose catches are gone: a condition signalled then
    /// found one, couldn't throw to it, signalled that, and so on without
    /// end).
    pub fn apply(&mut self, f: Value, args: &[Value]) -> Result<Value> {
        let (dyn_env, ns) = (self.dyn_env, self.ns);
        self.nested += 1;
        let base = self.handlers_now();
        self.nested_base.push((self.nested, base));
        let r = self.apply_inner(f, args);
        self.nested_base.pop();
        self.nested -= 1;
        if r.is_err() {
            self.dyn_env = dyn_env;
            self.ns = ns;
        }
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
        // (each carrier's own table of the fast primitives, for its native
        // code: filled the first time, before any return, since the code
        // may have been compiled on another carrier)
        if self.jit.prims[0] == 0 {
            for (i, name) in crate::jit::FAST_PRIMS.iter().enumerate() {
                let sym = rt().symbol(&mut self.m, None, name);
                self.jit.prims[i] = self.core.lookup(sym).map(|v| v.var_root().0).unwrap_or(u64::MAX);
            }
        }
        // (below MIN_ENTRY, other than 1, the word counts entries: entry_for)
        let n = proto.proto_native();
        if n >= crate::jit::MIN_ENTRY || n == 1 {
            return n;
        }
        crate::jit::entry_for(proto)
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
        // (the cached segment isn't a root: a collection meanwhile would free
        // it, and the next frames would be written into whatever came next)
        self.drop_spare();
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
                        self.jit.vm = self as *mut Vm as u64;
                        jit_segment(&mut self.jit, st);
                        let r = crate::jit::enter(st.native, st.regs(), st.consts, &mut self.jit, st.pc as u64);
                        // (calls and returns made natively: the frame it stopped
                        // in; a tail call's is where its caller's was)
                        let now = self.jit.cur_regs as *mut Value;
                        if now != st.regs() || (*now.sub(HDR).add(F_PROTO)).proto_code().code_ptr() != st.code {
                            st.switch_to(now);
                        }
                        st.pc = (r & !(crate::jit::POLL_FLAG | crate::jit::PENDING_FLAG)) as usize;
                        if r & crate::jit::PENDING_FLAG != 0 {
                            // a primitive called natively at this instruction
                            // failed or asked to park: as if the interpreter
                            // had called it
                            let ins = *st.code.add(st.pc);
                            st.pc += 1;
                            let ra = a(ins);
                            let flow = match self.jit_pending.take() {
                                Some(JitPending::Error(e)) => self.handle_error(st, e, Some(Some(ra)))?,
                                Some(JitPending::Suspend(s)) => self.suspend_here(st, s, if op(ins) == CALL { Some(ra) } else { None }),
                                None => return Err(Error::fatal("tier 1 stopped for a primitive's result that isn't there", vec![])),
                            };
                            match flow {
                                Flow::Next => continue,
                                Flow::Done(v) => return Ok(Exit::Done(v)),
                                Flow::Park => return Ok(Exit::Park),
                                Flow::Yield => return Ok(Exit::Yield),
                            }
                        }
                        if r & crate::jit::POLL_FLAG != 0 {
                            // a callee's entry safepoint, as push_frame's
                            let flow = match self.poll_slow(st, true) {
                                Err(e) => self.handle_error(st, e, None)?,
                                Ok(f) => f,
                            };
                            match flow {
                                Flow::Next => continue,
                                Flow::Done(v) => return Ok(Exit::Done(v)),
                                Flow::Park => return Ok(Exit::Park),
                                Flow::Yield => return Ok(Exit::Yield),
                            }
                        }
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
                            Err(Error::of("UnboundVarError", format!("Unbound var: {}", pr_str(var, true))).with("var", ErrData::Val(var)))
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
                        // (the closure is only known here: the verifier
                        // can't check its upvalue, so this does)
                        let f = *regs.add(ra);
                        if f.is(Kind::Closure) && b(ins) < f.closure_nups() {
                            f.closure_set_upval(b(ins), *regs.add(c(ins)));
                            continue;
                        }
                        Err(Error::new(format!("SETUP of upvalue {} of {}", b(ins), pr_str(f, true))))
                    }
                    JMP => {
                        st.pc = (st.pc as isize + sbx(ins)) as usize;
                        continue;
                    }
                    GUARD => {
                        // (an inlined call's: the var still holds the function
                        // inlined, so skip the jump to the general call)
                        if *regs.add(ra) == *st.consts.add(bx(ins)) {
                            st.pc += 1;
                        }
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
                        // (a loop's turns count towards compiling its proto)
                        if tier1 && st.native == 1 {
                            st.native = crate::jit::entry_for(st.slot(F_PROTO));
                        }
                        self.poll(st, false)
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
                    // (with attention since the last poll, poll here first: a
                    // profile's sample counts for the function returning,
                    // whose time it was, though it has no loop or call)
                    RETURN if safepoint::attention() != self.seen_attention => {
                        st.pc -= 1;
                        match self.poll_slow(st, false) {
                            Ok(Flow::Next) => {
                                st.pc += 1;
                                Ok(self.do_return(st, *regs.add(ra)))
                            }
                            other => other,
                        }
                    }
                    RETURN => Ok(self.do_return(st, *regs.add(ra))),
                    other => Err(Error::new(format!("bad opcode {other}"))),
                };
                let flow = match flow {
                    Err(e) => {
                        // A failed TAILCALL is followed by `RETURN ra` (see the
                        // compiler), so it is handled like a failed CALL.
                        let dest = match op(ins) {
                            CALL | GETVAR | TAILCALL | SETUP => Some(Some(ra)),
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
