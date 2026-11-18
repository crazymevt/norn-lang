//! Arrays of values: `[len][v0 v1 …]`. Internal building block for vector
//! nodes, constant pools, closure templates.

use crate::heap::Mutator;
use crate::object::{Kind, alloc};
use crate::value::Value;

pub fn array_new(m: &mut Mutator, len: usize, fill: Value) -> Value {
    let a = alloc(m, Kind::Array, 1 + len);
    a.set_word(0, len as u64);
    for i in 0..len {
        a.set_field(1 + i, fill);
    }
    a
}

pub fn array_from(m: &mut Mutator, xs: &[Value]) -> Value {
    let a = alloc(m, Kind::Array, 1 + xs.len());
    a.set_word(0, xs.len() as u64);
    // SAFETY: payload has room for len + xs.len() words.
    unsafe {
        std::ptr::copy_nonoverlapping(xs.as_ptr() as *const u64, a.payload_ptr().add(1), xs.len());
    }
    a
}

/// A copy of array `src` that the collector leaves where it is: a proto's
/// constants, which the interpreter and compiled code read through a
/// pointer (`Kind::stays_put`).
pub fn array_staying(m: &mut Mutator, src: Value) -> Value {
    let n = src.arr_len();
    let a = crate::object::alloc_staying(m, Kind::Array, 1 + n);
    a.set_word(0, n as u64);
    // SAFETY: both arrays hold n words after their length words.
    unsafe {
        std::ptr::copy_nonoverlapping(src.payload_ptr().add(1), a.payload_ptr().add(1), n);
    }
    // (filled without set_field, and old from the start: say so)
    crate::heap::barrier(a);
    a
}

/// Copy of `src` with length `len` (truncating, or padding with nil).
pub fn array_resized(m: &mut Mutator, src: Value, len: usize) -> Value {
    let a = array_new(m, len, Value::NIL);
    let n = src.arr_len().min(len);
    // SAFETY: both arrays have at least n elements.
    unsafe {
        std::ptr::copy_nonoverlapping(src.payload_ptr().add(1), a.payload_ptr().add(1), n);
    }
    a
}

pub fn array_copy(m: &mut Mutator, src: Value) -> Value {
    array_resized(m, src, src.arr_len())
}

impl Value {
    #[inline]
    pub fn arr_len(self) -> usize {
        debug_assert!(self.is(Kind::Array));
        self.word(0) as usize
    }

    #[inline]
    pub fn arr_get(self, i: usize) -> Value {
        debug_assert!(i < self.arr_len());
        self.field(1 + i)
    }

    #[inline]
    pub fn arr_set(self, i: usize, v: Value) {
        debug_assert!(i < self.arr_len());
        self.set_field(1 + i, v)
    }

    /// Borrow the elements. Valid until the next safepoint (NoGC: forever).
    #[inline]
    pub fn arr_slice<'a>(self) -> &'a [Value] {
        // SAFETY: Value is repr(transparent) over u64; length from header.
        unsafe { std::slice::from_raw_parts(self.payload_ptr().add(1) as *const Value, self.arr_len()) }
    }

    #[inline]
    pub fn arr_ptr(self) -> *mut Value {
        // SAFETY: element 0 starts after the length word.
        unsafe { self.payload_ptr().add(1) as *mut Value }
    }
}
