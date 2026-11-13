//! Strings, floats, symbols and keywords.
//!
//! - String:  `[byte_len][utf-8 bytes, padded to 8]` — immutable.
//! - Bytes:   `[len][bytes, padded to 8]` — immutable, the same layout.
//! - Float:   `[f64 bits]`
//! - Symbol / Keyword: `[ns: String|nil][name: String][hash]` — interned, so
//!   equality is identity (see runtime.rs).

use crate::heap::Mutator;
use crate::object::{Kind, alloc};
use crate::value::Value;

pub fn str_new(m: &mut Mutator, s: &str) -> Value {
    let len = s.len();
    let v = alloc(m, Kind::String, 1 + len.div_ceil(8));
    v.set_word(0, len as u64);
    // SAFETY: payload has room for len bytes after the length word.
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), v.payload_ptr().add(1) as *mut u8, len);
    }
    v
}

pub fn bytes_new(m: &mut Mutator, b: &[u8]) -> Value {
    let len = b.len();
    let v = alloc(m, Kind::Bytes, 1 + len.div_ceil(8));
    v.set_word(0, len as u64);
    // SAFETY: payload has room for len bytes after the length word.
    unsafe {
        std::ptr::copy_nonoverlapping(b.as_ptr(), v.payload_ptr().add(1) as *mut u8, len);
    }
    v
}

impl Value {
    /// The contents of a Bytes object.
    pub fn as_bytes<'a>(self) -> &'a [u8] {
        // SAFETY: Bytes objects hold `len` bytes after the length word.
        unsafe { std::slice::from_raw_parts(self.payload_ptr().add(1) as *const u8, self.word(0) as usize) }
    }
}

pub fn float_new(m: &mut Mutator, f: f64) -> Value {
    let v = alloc(m, Kind::Float, 1);
    v.set_word(0, f.to_bits());
    v
}

pub(crate) const SYM_NS: usize = 0;
pub(crate) const SYM_NAME: usize = 1;
pub(crate) const SYM_HASH: usize = 2;

impl Value {
    /// The string's contents. Valid until the next safepoint (NoGC: forever).
    #[inline]
    pub fn as_str<'a>(self) -> &'a str {
        debug_assert!(self.is(Kind::String));
        let len = self.word(0) as usize;
        // SAFETY: strings are only ever created from valid UTF-8.
        unsafe {
            let bytes = std::slice::from_raw_parts(self.payload_ptr().add(1) as *const u8, len);
            std::str::from_utf8_unchecked(bytes)
        }
    }

    #[inline]
    pub fn is_float(self) -> bool {
        self.is(Kind::Float)
    }

    #[inline]
    pub fn as_f64(self) -> f64 {
        f64::from_bits(self.word(0))
    }

    #[inline]
    pub fn is_symbol(self) -> bool {
        self.is(Kind::Symbol)
    }

    #[inline]
    pub fn is_keyword(self) -> bool {
        self.is(Kind::Keyword)
    }

    /// Name part of a symbol or keyword.
    pub fn sym_name<'a>(self) -> &'a str {
        self.field(SYM_NAME).as_str()
    }

    /// Namespace part of a symbol or keyword, if qualified.
    pub fn sym_ns<'a>(self) -> Option<&'a str> {
        let ns = self.field(SYM_NS);
        if ns.is_nil() { None } else { Some(ns.as_str()) }
    }

    pub fn sym_hash(self) -> u64 {
        self.word(SYM_HASH)
    }
}
