//! Code-related objects: vars, bytecode, function prototypes, closures,
//! primitives, and stack segments.
//!
//! ```text
//! Var:        [ns: Symbol][name: Symbol][root][flags]
//! Code:       [n_instructions][u32 instructions, packed two per word]
//! Proto:      [name][nfixed][variadic][nregs][consts: Array][code: Code][locals: Array][native]
//! FnTemplate: [name][arities: Array<Proto>][nups][captures: Array<fixnum>]
//! Closure:    [template][nups][upval0 upval1 …]   (size readable without the template)
//! Primitive:  [name: Symbol][index][min-args][max-args | -1]
//! Segment:    [capacity][prev-segment][prev-fp][depth][top][frames …]
//! ```

use crate::heap::Mutator;
use crate::object::{Kind, alloc};
use crate::value::Value;
use std::sync::atomic::Ordering;

// ---- vars ----------------------------------------------------------------

pub const VAR_MACRO: u64 = 1;
pub const VAR_DYNAMIC: u64 = 2;
/// Has ever been bound with `binding`: reads must consult the fiber's
/// dynamic environment.
pub const VAR_DYNBOUND: u64 = 4;
/// Private to its namespace (defn-, ^:private): other code can't name it.
pub const VAR_PRIVATE: u64 = 8;
/// Has a contract (a signature, or defcontract): calls from other
/// namespaces go through a wrapper that checks it (M9 part 9.4).
pub const VAR_CONTRACTED: u64 = 16;
/// The var is part of the raw FFI (M9 part 9.8): only a module that says
/// `(:unsafe)` may refer to it.
pub const VAR_UNSAFE: u64 = 32;

pub fn var_new(m: &mut Mutator, ns: Value, name: Value) -> Value {
    let v = alloc(m, Kind::Var, 4);
    v.set_field(0, ns);
    v.set_field(1, name);
    v.set_field(2, Value::UNBOUND);
    v.set_word(3, 0);
    v
}

impl Value {
    pub fn var_ns(self) -> Value {
        self.field(0)
    }

    pub fn var_name(self) -> Value {
        self.field(1)
    }

    /// Root value (acquire), or `UNBOUND`.
    #[inline]
    pub fn var_root(self) -> Value {
        Value(self.atomic_word(2).load(Ordering::Acquire))
    }

    /// Publish a new root (release): every core sees it on its next read.
    #[inline]
    pub fn var_set_root(self, v: Value) {
        self.atomic_word(2).store(v.0, Ordering::Release)
    }

    pub fn var_flags(self) -> u64 {
        self.atomic_word(3).load(Ordering::Relaxed)
    }

    pub fn var_set_flag(self, flag: u64) {
        self.atomic_word(3).fetch_or(flag, Ordering::Relaxed);
    }

    pub fn var_clear_flag(self, flag: u64) {
        self.atomic_word(3).fetch_and(!flag, Ordering::Relaxed);
    }

    pub fn var_is_macro(self) -> bool {
        self.var_flags() & VAR_MACRO != 0
    }

    pub fn var_is_private(self) -> bool {
        self.var_flags() & VAR_PRIVATE != 0
    }
}

// ---- code ----------------------------------------------------------------

pub fn code_new(m: &mut Mutator, ins: &[u32]) -> Value {
    let c = alloc(m, Kind::Code, 1 + ins.len().div_ceil(2));
    c.set_word(0, ins.len() as u64);
    // SAFETY: room for ins.len() u32s after the length word.
    unsafe {
        std::ptr::copy_nonoverlapping(ins.as_ptr(), c.payload_ptr().add(1) as *mut u32, ins.len());
    }
    c
}

impl Value {
    pub fn code_len(self) -> usize {
        self.word(0) as usize
    }

    #[inline]
    pub fn code_ptr(self) -> *const u32 {
        // SAFETY: instructions start after the length word.
        unsafe { self.payload_ptr().add(1) as *const u32 }
    }
}

// ---- protos and templates ---------------------------------------------------

pub struct ProtoInfo {
    pub name: Value,
    pub nfixed: usize,
    pub variadic: bool,
    pub nregs: usize,
    pub consts: Value,
    pub code: Value,
    /// Array [sym, reg, sym, reg …]: local variable names, for the debugger.
    pub locals: Value,
}

pub fn proto_new(m: &mut Mutator, p: ProtoInfo) -> Value {
    let v = alloc(m, Kind::Proto, 8);
    v.set_field(0, p.name);
    v.set_word(1, p.nfixed as u64);
    v.set_word(2, p.variadic as u64);
    v.set_word(3, p.nregs as u64);
    v.set_field(4, p.consts);
    v.set_field(5, p.code);
    v.set_field(6, p.locals);
    v.set_word(7, 0);
    v
}

impl Value {
    pub fn proto_name(self) -> Value {
        self.field(0)
    }
    #[inline]
    pub fn proto_nfixed(self) -> usize {
        self.word(1) as usize
    }
    #[inline]
    pub fn proto_variadic(self) -> bool {
        self.word(2) != 0
    }
    #[inline]
    pub fn proto_nregs(self) -> usize {
        self.word(3) as usize
    }
    #[inline]
    pub fn proto_consts(self) -> Value {
        self.field(4)
    }
    #[inline]
    pub fn proto_code(self) -> Value {
        self.field(5)
    }
    pub fn proto_locals(self) -> Value {
        self.field(6)
    }
    /// Tier-1 entry point: 0 = not compiled yet, 1 = can't be compiled,
    /// 2 up to tier 1's MIN_ENTRY = its count of entries (+1) or, queued to
    /// compile, its count since (skein-vm's jit.rs, entry_for), otherwise
    /// the address of the native function (runtime.md §8.2).
    #[inline]
    pub fn proto_native(self) -> usize {
        self.atomic_word(7).load(Ordering::Acquire) as usize
    }
    /// Set the native entry if none is set yet (first compiler wins).
    pub fn proto_set_native(self, entry: usize) -> bool {
        self.atomic_word(7).compare_exchange(0, entry as u64, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }
    /// Replace the native word (an entry, or tier 1's count of entries
    /// before compiling: see jit.rs) if it's still `old`.
    pub fn proto_native_cas(self, old: usize, new: usize) -> bool {
        self.atomic_word(7).compare_exchange(old as u64, new as u64, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }
}

/// How a closure captures upvalue `i` from its creating frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Capture {
    /// Register of the creating frame.
    Local(u8),
    /// Upvalue of the creating closure.
    Up(u8),
    /// The creating closure itself (its self-name).
    SelfRef,
}

impl Capture {
    pub fn encode(self) -> Value {
        match self {
            Capture::Local(r) => Value::fix((r as i64) << 2),
            Capture::Up(i) => Value::fix(((i as i64) << 2) | 1),
            Capture::SelfRef => Value::fix(2),
        }
    }

    pub fn decode(v: Value) -> Capture {
        let n = v.as_fixnum();
        match n & 3 {
            0 => Capture::Local((n >> 2) as u8),
            1 => Capture::Up((n >> 2) as u8),
            _ => Capture::SelfRef,
        }
    }
}

pub fn template_new(m: &mut Mutator, name: Value, arities: Value, captures: Value) -> Value {
    let t = alloc(m, Kind::FnTemplate, 4);
    t.set_field(0, name);
    t.set_field(1, arities);
    t.set_word(2, captures.arr_len() as u64);
    t.set_field(3, captures);
    t
}

impl Value {
    pub fn template_name(self) -> Value {
        self.field(0)
    }
    pub fn template_arities(self) -> Value {
        self.field(1)
    }
    pub fn template_nups(self) -> usize {
        self.word(2) as usize
    }
    pub fn template_captures(self) -> Value {
        self.field(3)
    }
}

pub fn closure_new(m: &mut Mutator, template: Value) -> Value {
    let n = template.template_nups();
    let c = alloc(m, Kind::Closure, 2 + n);
    c.set_field(0, template);
    c.set_word(1, n as u64);
    for i in 0..n {
        c.set_field(2 + i, Value::NIL);
    }
    c
}

impl Value {
    #[inline]
    pub fn closure_template(self) -> Value {
        self.field(0)
    }
    #[inline]
    pub fn closure_nups(self) -> usize {
        self.word(1) as usize
    }
    #[inline]
    pub fn closure_upval(self, i: usize) -> Value {
        self.field(2 + i)
    }
    #[inline]
    pub fn closure_set_upval(self, i: usize, v: Value) {
        self.set_field(2 + i, v)
    }
}

// ---- primitives -------------------------------------------------------------

pub fn primitive_new(m: &mut Mutator, name: Value, index: usize, min: usize, max: Option<usize>) -> Value {
    let p = alloc(m, Kind::Primitive, 4);
    p.set_field(0, name);
    p.set_word(1, index as u64);
    p.set_word(2, min as u64);
    p.set_word(3, max.map(|x| x as u64).unwrap_or(u64::MAX));
    p
}

impl Value {
    pub fn prim_name(self) -> Value {
        self.field(0)
    }
    #[inline]
    pub fn prim_index(self) -> usize {
        self.word(1) as usize
    }
    #[inline]
    pub fn prim_min(self) -> usize {
        self.word(2) as usize
    }
    #[inline]
    pub fn prim_max(self) -> usize {
        self.word(3) as usize
    }
}

// ---- stack segments and frames ---------------------------------------------
//
// Frame layout (runtime.md §3.3). Every word is a tagged value, so the GC can
// scan a frame without side tables.
//
//  +0 fn      closure being executed
//  +1 proto   arity being executed; frame size = HDR + proto.nregs
//  +2 resume  pc to continue at (fixnum)
//  +3 prev    caller frame offset in this segment, or PREV_SEG_START / PREV_BOUNDARY
//  +4 … registers

pub const HDR: usize = 4;
pub const F_FN: usize = 0;
pub const F_PROTO: usize = 1;
pub const F_RESUME: usize = 2;
pub const F_PREV: usize = 3;
pub const PREV_SEG_START: i64 = -1;
pub const PREV_BOUNDARY: i64 = -2;

pub const SEG_HEADER: usize = 5;
const SEG_CAP: usize = 0;
const SEG_PREV: usize = 1;
const SEG_PREV_FP: usize = 2;
const SEG_DEPTH: usize = 3;
const SEG_TOP: usize = 4;

pub fn segment_new(m: &mut Mutator, capacity: usize) -> Value {
    let s = alloc(m, Kind::Segment, SEG_HEADER + capacity);
    s.set_word(SEG_CAP, capacity as u64);
    s.set_field(SEG_PREV, Value::NIL);
    s.set_field(SEG_PREV_FP, Value::fix(0));
    s.set_field(SEG_DEPTH, Value::fix(0));
    s.set_field(SEG_TOP, Value::fix(-1));
    s
}

impl Value {
    #[inline]
    pub fn seg_capacity(self) -> usize {
        self.word(SEG_CAP) as usize
    }
    #[inline]
    pub fn seg_prev(self) -> Value {
        self.field(SEG_PREV)
    }
    #[inline]
    pub fn seg_prev_fp(self) -> usize {
        self.field(SEG_PREV_FP).as_fixnum() as usize
    }
    #[inline]
    pub fn seg_depth(self) -> usize {
        self.field(SEG_DEPTH).as_fixnum() as usize
    }
    pub fn seg_link(self, prev: Value, prev_fp: usize, depth: usize) {
        self.set_field(SEG_PREV, prev);
        self.set_field(SEG_PREV_FP, Value::fix(prev_fp as i64));
        self.set_field(SEG_DEPTH, Value::fix(depth as i64));
    }
    /// Offset of the topmost live frame, or -1 if the segment holds none.
    /// Kept exact whenever a GC can run: at safepoints, parks, and when a
    /// newer segment is pushed on top.
    #[inline]
    pub fn seg_top(self) -> i64 {
        self.field(SEG_TOP).as_fixnum()
    }
    #[inline]
    pub fn seg_set_top(self, top: i64) {
        self.set_field(SEG_TOP, Value::fix(top))
    }
    /// First frame word.
    #[inline]
    pub fn seg_data(self) -> *mut Value {
        // SAFETY: frames start after the segment header.
        unsafe { self.payload_ptr().add(SEG_HEADER) as *mut Value }
    }
    /// Number of live words at the start of the segment's frame area.
    pub fn seg_live_words(self) -> usize {
        let top = self.seg_top();
        if top < 0 {
            return 0;
        }
        // SAFETY: top is a valid frame offset.
        let proto = unsafe { *self.seg_data().add(top as usize + F_PROTO) };
        top as usize + HDR + proto.proto_nregs()
    }
}
