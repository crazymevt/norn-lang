//! The tier-0 interpreter, running on heap-allocated segmented stacks
//! (runtime.md §3).
//!
//! Frame layout (all words are tagged values, so the GC can scan frames
//! without stack maps):
//!
//! ```text
//!  +0  fn       closure being executed
//!  +1  proto    the arity being executed (→ code, constants)
//!  +2  resume   pc to continue at when the callee returns (fixnum)
//!  +3  prev     caller's frame offset in this segment (fixnum), or
//!               -1 = caller is in the previous segment, -2 = return to Rust
//!  +4… registers
//! ```
//!
//! Frames link by *offset*, never by address, so segments stay relocatable
//! for a moving GC and for images.
//!
//! The run loop never recurses on Loom→Loom calls. Rust re-enters it only via
//! `Vm::apply` (macro expansion, `eval`), and each such entry gets a fresh
//! segment chain: a continuation barrier, as runtime.md §2.3 allows for the
//! seed compiler.

use crate::bytecode::*;
use crate::prims::{APPLY, PRIMS};
use loom_rt::func::{Capture, closure_new, segment_new};
use loom_rt::list::list_from_with_tail;
use loom_rt::print::pr_str;
use loom_rt::runtime::{Namespace, rt};
use loom_rt::{Kind, Mutator, Value};
use std::sync::atomic::{AtomicBool, Ordering};

// ---- errors ---------------------------------------------------------------

/// A Loom error. M0 has no condition system yet (M3), so errors unwind to the
/// nearest Rust caller and carry a Loom-level backtrace.
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

// ---- safepoint poll ----------------------------------------------------------

/// Set by the Ctrl-C handler; observed at every function entry and loop
/// back-edge. In M1 the same poll serves preemption and GC (runtime.md §4.2).
pub static INTERRUPT: AtomicBool = AtomicBool::new(false);

#[inline]
fn poll() -> Result<()> {
    if INTERRUPT.load(Ordering::Relaxed) {
        INTERRUPT.store(false, Ordering::Relaxed);
        return Err(Error::new("Interrupted"));
    }
    Ok(())
}

// ---- frames -------------------------------------------------------------------

const HDR: usize = 4;
const F_FN: usize = 0;
const F_PROTO: usize = 1;
const F_RESUME: usize = 2;
const F_PREV: usize = 3;
const PREV_SEG_START: i64 = -1;
const PREV_BOUNDARY: i64 = -2;

/// Words per stack segment for entries from Rust. Fibers (M1) start at 256.
const DEFAULT_SEG_WORDS: usize = 32 * 1024;
/// Total stack depth limit per entry (words) → `Stack overflow`.
const MAX_STACK_WORDS: usize = 32 << 20;

/// Interpreter registers. Everything here is derivable from the frame, which
/// is what makes fibers and debugger stacks cheap.
struct St {
    seg: Value,
    base: *mut Value,
    fp: usize,
    pc: usize,
    code: *const u32,
    consts: *const Value,
}

impl St {
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
}

// ---- the VM ----------------------------------------------------------------------

/// One VM per carrier thread (M0 has one carrier).
pub struct Vm {
    pub m: Mutator,
    pub ns: &'static Namespace,
    pub core: &'static Namespace,
    spare: Value,
    scratch: Vec<Value>,
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
    pub(crate) fn bare(m: Mutator, core: &'static Namespace) -> Vm {
        Vm { m, ns: core, core, spare: Value::NIL, scratch: Vec::new() }
    }

    /// Resolve a symbol to a var from the current namespace.
    pub fn resolve(&mut self, sym: Value) -> Option<Value> {
        rt().resolve(&mut self.m, self.ns, self.core, sym)
    }

    pub fn sym(&mut self, s: &str) -> Value {
        rt().symbol_from(&mut self.m, s)
    }

    fn take_segment(&mut self, need: usize) -> Value {
        let s = self.spare;
        if !s.is_nil() && s.seg_capacity() >= need {
            self.spare = Value::NIL;
            return s;
        }
        segment_new(&mut self.m, need.max(DEFAULT_SEG_WORDS))
    }

    /// Keep one free segment around so call/return across a segment boundary
    /// in a loop doesn't allocate every time (the "hot split" problem).
    fn give_segment(&mut self, s: Value) {
        if self.spare.is_nil() || s.seg_capacity() > self.spare.seg_capacity() {
            self.spare = s;
        }
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

    /// Push a callee frame above the current one (or in a new segment).
    unsafe fn push_frame(&mut self, st: &mut St, f: Value, proto: Value, args: &[Value]) -> Result<()> {
        poll()?;
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
                let s = self.take_segment(need);
                s.seg_link(st.seg, st.fp, depth);
                self.write_frame(s.seg_data(), 0, f, proto, PREV_SEG_START, args);
                st.seg = s;
                st.base = s.seg_data();
                st.fp = 0;
            }
            st.enter();
        }
        Ok(())
    }

    /// Replace the current frame with a callee frame (proper tail call).
    unsafe fn tail_frame(&mut self, st: &mut St, f: Value, proto: Value, args: &[Value]) -> Result<()> {
        poll()?;
        unsafe {
            let need = HDR + proto.proto_nregs();
            let prev = st.slot(F_PREV).as_fixnum();
            if st.fp + need <= st.seg.seg_capacity() {
                self.write_frame(st.base, st.fp, f, proto, prev, args);
            } else {
                let s = self.take_segment(need);
                let new_prev = match prev {
                    PREV_BOUNDARY => PREV_BOUNDARY,
                    PREV_SEG_START => {
                        s.seg_link(st.seg.seg_prev(), st.seg.seg_prev_fp(), st.seg.seg_depth());
                        PREV_SEG_START
                    }
                    p => {
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
        }
        Ok(())
    }

    /// Pop the current frame and deliver `val` to the caller. `Some` means we
    /// reached the Rust boundary and the run is finished.
    #[inline]
    unsafe fn do_return(&mut self, st: &mut St, val: Value) -> Option<Value> {
        unsafe {
            match st.slot(F_PREV).as_fixnum() {
                PREV_BOUNDARY => {
                    self.give_segment(st.seg);
                    return Some(val);
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
        None
    }

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
        v.extend(loom_rt::seq::to_vec(&mut self.m, last)?);
        Ok((f, v))
    }

    /// Call `f` from the interpreter. `dest = Some(r)`: ordinary call whose
    /// result goes to register r; `None`: tail call.
    /// Returns `Some(v)` if this finished the whole run.
    unsafe fn invoke(&mut self, st: &mut St, mut f: Value, args: &[Value], dest: Option<usize>) -> Result<Option<Value>> {
        unsafe {
            loop {
                if f.is(Kind::Closure) {
                    let proto = select_arity(f, args.len())?;
                    match dest {
                        Some(_) => {
                            *st.frame().add(F_RESUME) = Value::fix(st.pc as i64);
                            self.push_frame(st, f, proto, args)?;
                        }
                        None => self.tail_frame(st, f, proto, args)?,
                    }
                    return Ok(None);
                }
                if f.is(Kind::Var) {
                    f = Self::deref_var(f)?;
                    continue;
                }
                let val = if f.is(Kind::Primitive) {
                    if f.prim_index() == APPLY {
                        if args.len() < 2 {
                            return Err(arity_error(f, args.len()));
                        }
                        let (g, spread) = self.spread_apply(args)?;
                        return self.invoke(st, g, &spread, dest);
                    }
                    self.call_prim(f, args)?
                } else {
                    self.call_data(f, args)?
                };
                return Ok(match dest {
                    Some(r) => {
                        *st.regs().add(r) = val;
                        None
                    }
                    None => self.do_return(st, val),
                });
            }
        }
    }

    /// Loom-level backtrace from the current frame to the Rust boundary.
    unsafe fn backtrace(&self, st: &St) -> Vec<String> {
        let mut out = Vec::new();
        let (mut seg, mut fp) = (st.seg, st.fp);
        unsafe {
            for _ in 0..64 {
                let fr = seg.seg_data().add(fp);
                out.push(pr_str((*fr.add(F_PROTO)).proto_name(), true));
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

    /// Call any callable from Rust. Closures run on a fresh segment chain.
    pub fn apply(&mut self, f: Value, args: &[Value]) -> Result<Value> {
        let mut f = f;
        loop {
            if f.is(Kind::Closure) {
                let proto = select_arity(f, args.len())?;
                let s = self.take_segment(HDR + proto.proto_nregs());
                s.seg_link(Value::NIL, 0, 0);
                // SAFETY: fresh segment large enough for one frame.
                unsafe {
                    self.write_frame(s.seg_data(), 0, f, proto, PREV_BOUNDARY, args);
                    let mut st = St {
                        seg: s,
                        base: s.seg_data(),
                        fp: 0,
                        pc: 0,
                        code: std::ptr::null(),
                        consts: std::ptr::null(),
                    };
                    st.enter();
                    return self.run(&mut st);
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
                    return self.apply(g, &spread);
                }
                return self.call_prim(f, args);
            }
            return self.call_data(f, args);
        }
    }

    /// The interpreter loop.
    unsafe fn run(&mut self, st: &mut St) -> Result<Value> {
        unsafe {
            loop {
                let ins = *st.code.add(st.pc);
                st.pc += 1;
                let ra = a(ins);
                let regs = st.regs();
                match op(ins) {
                    MOVE => *regs.add(ra) = *regs.add(b(ins)),
                    LOADK => *regs.add(ra) = *st.consts.add(bx(ins)),
                    LOADI => *regs.add(ra) = Value::fix(sbx(ins) as i64),
                    LOADNIL => *regs.add(ra) = Value::NIL,
                    LOADTRUE => *regs.add(ra) = Value::TRUE,
                    LOADFALSE => *regs.add(ra) = Value::FALSE,
                    GETVAR => {
                        let var = *st.consts.add(bx(ins));
                        let v = var.var_root();
                        if v == Value::UNBOUND {
                            let e = Error::new(format!("Unbound var: {}", pr_str(var, true)));
                            return Err(self.fail(st, e));
                        }
                        *regs.add(ra) = v;
                    }
                    DEFVAR => (*st.consts.add(bx(ins))).var_set_root(*regs.add(ra)),
                    GETUP => *regs.add(ra) = st.slot(F_FN).closure_upval(b(ins)),
                    SELF => *regs.add(ra) = st.slot(F_FN),
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
                    }
                    SETUP => (*regs.add(ra)).closure_set_upval(b(ins), *regs.add(c(ins))),
                    JMP => st.pc = (st.pc as isize + sbx(ins)) as usize,
                    JMPF => {
                        if !(*regs.add(ra)).truthy() {
                            st.pc = (st.pc as isize + sbx(ins)) as usize;
                        }
                    }
                    LOOP => {
                        if let Err(e) = poll() {
                            return Err(self.fail(st, e));
                        }
                        st.pc = (st.pc as isize + sbx(ins)) as usize;
                    }
                    CALL => {
                        let f = *regs.add(ra);
                        let args = std::slice::from_raw_parts(regs.add(ra + 1), b(ins));
                        if let Err(e) = self.invoke(st, f, args, Some(ra)) {
                            return Err(self.fail(st, e));
                        }
                    }
                    TAILCALL => {
                        let f = *regs.add(ra);
                        // The callee frame overwrites ours, so copy args out first.
                        let mut scratch = std::mem::take(&mut self.scratch);
                        scratch.clear();
                        scratch.extend_from_slice(std::slice::from_raw_parts(regs.add(ra + 1), b(ins)));
                        let r = self.invoke(st, f, &scratch, None);
                        self.scratch = scratch;
                        match r {
                            Ok(Some(v)) => return Ok(v),
                            Ok(None) => {}
                            Err(e) => return Err(self.fail(st, e)),
                        }
                    }
                    RETURN => {
                        if let Some(v) = self.do_return(st, *regs.add(ra)) {
                            return Ok(v);
                        }
                    }
                    other => return Err(self.fail(st, Error::new(format!("bad opcode {other}")))),
                }
            }
        }
    }
}
