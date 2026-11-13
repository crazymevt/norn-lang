//! Code-related objects: vars, bytecode, function prototypes, closures,
//! primitives, and stack segments.
//!
//! ```text
//! Var:        [ns: Symbol][name: Symbol][root][flags]
//! Code:       [n_instructions][u32 instructions, packed two per word]
//! Proto:      [name][nfixed][variadic][nregs][consts: Array][code: Code]
//! FnTemplate: [name][arities: Array<Proto>][nups][captures: Array<fixnum>]
//! Closure:    [template][upval0 upval1 …]
//! Primitive:  [name: Symbol][index][min-args][max-args | -1]
//! Segment:    [capacity][prev-segment][prev-fp][depth][frames …]
//! ```

use crate::heap::Mutator;
use crate::object::{Kind, alloc};
use crate::value::Value;
use std::sync::atomic::Ordering;

// ---- vars ----------------------------------------------------------------

pub const VAR_MACRO: u64 = 1;
pub const VAR_DYNAMIC: u64 = 2;

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

    pub fn var_is_macro(self) -> bool {
        self.var_flags() & VAR_MACRO != 0
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
}

pub fn proto_new(m: &mut Mutator, p: ProtoInfo) -> Value {
    let v = alloc(m, Kind::Proto, 6);
    v.set_field(0, p.name);
    v.set_word(1, p.nfixed as u64);
    v.set_word(2, p.variadic as u64);
    v.set_word(3, p.nregs as u64);
    v.set_field(4, p.consts);
    v.set_field(5, p.code);
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
    let c = alloc(m, Kind::Closure, 1 + n);
    c.set_field(0, template);
    for i in 0..n {
        c.set_field(1 + i, Value::NIL);
    }
    c
}

impl Value {
    #[inline]
    pub fn closure_template(self) -> Value {
        self.field(0)
    }
    #[inline]
    pub fn closure_upval(self, i: usize) -> Value {
        self.field(1 + i)
    }
    #[inline]
    pub fn closure_set_upval(self, i: usize, v: Value) {
        self.set_field(1 + i, v)
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

// ---- stack segments ---------------------------------------------------------

pub const SEG_HEADER: usize = 4;

pub fn segment_new(m: &mut Mutator, capacity: usize) -> Value {
    let s = alloc(m, Kind::Segment, SEG_HEADER + capacity);
    s.set_word(0, capacity as u64);
    s.set_field(1, Value::NIL);
    s.set_field(2, Value::fix(0));
    s.set_field(3, Value::fix(0));
    s
}

impl Value {
    #[inline]
    pub fn seg_capacity(self) -> usize {
        self.word(0) as usize
    }
    #[inline]
    pub fn seg_prev(self) -> Value {
        self.field(1)
    }
    #[inline]
    pub fn seg_prev_fp(self) -> usize {
        self.field(2).as_fixnum() as usize
    }
    #[inline]
    pub fn seg_depth(self) -> usize {
        self.field(3).as_fixnum() as usize
    }
    pub fn seg_link(self, prev: Value, prev_fp: usize, depth: usize) {
        self.set_field(1, prev);
        self.set_field(2, Value::fix(prev_fp as i64));
        self.set_field(3, Value::fix(depth as i64));
    }
    /// First frame word.
    #[inline]
    pub fn seg_data(self) -> *mut Value {
        // SAFETY: frames start after the segment header.
        unsafe { self.payload_ptr().add(SEG_HEADER) as *mut Value }
    }
}
