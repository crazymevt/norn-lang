//! Strings, floats, symbols and keywords.
//!
//! - String:  `[byte_len | ASCII][utf-8 bytes, padded to 8]` — immutable.
//!   The top bit of the length word says the string is all ASCII, so
//!   indexing by character (nth, subs, count) is constant time for it. For
//!   other strings a per-thread cache of positions (`positions`, below)
//!   makes a walk through the string linear rather than quadratic.
//! - Bytes:   `[len][bytes, padded to 8]` — immutable, the same layout.
//! - ByteArray: the Bytes layout, mutable: the storage of skein.buffer's
//!   MutableBuffer, written only through its owner. Freezing retags it as
//!   Bytes in place, so the bytes are handed over without copying.
//! - Float:   `[f64 bits]`
//! - Symbol / Keyword: `[ns: String|nil][name: String][hash]` — interned, so
//!   equality is identity (see runtime.rs).

use crate::heap::Mutator;
use crate::object::{Kind, alloc};
use crate::value::Value;

/// The length word's flag for an all-ASCII string.
const ASCII: u64 = 1 << 63;

pub fn str_new(m: &mut Mutator, s: &str) -> Value {
    let len = s.len();
    let v = alloc(m, Kind::String, 1 + len.div_ceil(8));
    v.set_word(0, len as u64 | if s.is_ascii() { ASCII } else { 0 });
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
    /// The contents of a Bytes (or ByteArray) object.
    pub fn as_bytes<'a>(self) -> &'a [u8] {
        // SAFETY: Bytes objects hold `len` bytes after the length word.
        unsafe { std::slice::from_raw_parts(self.payload_ptr().add(1) as *const u8, self.word(0) as usize) }
    }

    /// The contents of a ByteArray, to write.
    ///
    /// # Safety
    /// The caller writes only as the array's owner (skein.buffer checks its
    /// MutableBuffer's ownership first), so no one else writes it meanwhile.
    pub unsafe fn byte_array_mut<'a>(self) -> &'a mut [u8] {
        debug_assert!(self.is(Kind::ByteArray));
        // SAFETY: as as_bytes; the caller excludes other writers.
        unsafe { std::slice::from_raw_parts_mut(self.payload_ptr().add(1) as *mut u8, self.word(0) as usize) }
    }
}

/// A ByteArray of `len` zero bytes.
pub fn byte_array_new(m: &mut Mutator, len: usize) -> Value {
    let v = alloc(m, Kind::ByteArray, 1 + len.div_ceil(8));
    v.set_word(0, len as u64);
    // SAFETY: the payload has room for len bytes (padded) after the length word.
    unsafe { std::ptr::write_bytes(v.payload_ptr().add(1) as *mut u8, 0, len.div_ceil(8) * 8) };
    v
}

/// Turn ByteArray v into an immutable Bytes, in place: the two kinds share a
/// layout, so the object keeps its size and contents.
pub fn byte_array_freeze(v: Value) {
    assert!(v.is(Kind::ByteArray), "only a ByteArray freezes");
    let layout = &crate::object::LAYOUTS[Kind::Bytes as usize] as *const crate::object::Layout as u64;
    // SAFETY: word 0 of an object is its layout pointer.
    unsafe { v.ptr().write(layout) };
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
        let len = (self.word(0) & !ASCII) as usize;
        // SAFETY: strings are only ever created from valid UTF-8.
        unsafe {
            let bytes = std::slice::from_raw_parts(self.payload_ptr().add(1) as *const u8, len);
            std::str::from_utf8_unchecked(bytes)
        }
    }

    /// Is this string all ASCII (so a character index is a byte index)?
    /// Strings from images made before the flag existed say no, which is
    /// only slower.
    #[inline]
    pub fn str_is_ascii(self) -> bool {
        self.word(0) & ASCII != 0
    }

    /// The number of characters in this string.
    #[inline]
    pub fn str_char_count(self) -> usize {
        if self.str_is_ascii() { self.as_str().len() } else { positions::char_count(self.as_str()) }
    }

    /// Character i of this string.
    #[inline]
    pub fn str_char_at(self, i: usize) -> Option<char> {
        let s = self.as_str();
        if self.str_is_ascii() {
            return s.as_bytes().get(i).map(|&b| b as char);
        }
        let b = positions::byte_of(s, i)?;
        s[b..].chars().next()
    }

    /// The byte offset of character `ci` (the end for `ci` equal to the
    /// count), or None past the end.
    #[inline]
    pub fn str_byte_of(self, ci: usize) -> Option<usize> {
        let s = self.as_str();
        if self.str_is_ascii() { (ci <= s.len()).then_some(ci) } else { positions::byte_of(s, ci) }
    }

    /// The character index of byte offset `b`, which is on a character
    /// boundary.
    #[inline]
    pub fn str_char_of(self, b: usize) -> usize {
        if self.str_is_ascii() { b } else { positions::char_of(self.as_str(), b) }
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

/// Character positions in non-ASCII strings.
///
/// A non-ASCII string is UTF-8 with no index, so finding character i means
/// counting from a known position. Each thread remembers, for its last few
/// strings, the character count and the last position it found, and counts
/// from whichever known position is nearest. A pass through a string from
/// front to back (a parser, a loop over `nth` or `subs`) then costs what its
/// text does, instead of what the text before each step does.
///
/// Entries are keyed by the string's address and byte length. A string
/// moves, and memory is reused, only at a collection, so an entry holds
/// until the next one; each records the
/// collection count it was made in. Only heap strings, which these
/// functions are given through `Value`, may be looked up here.
mod positions {
    use std::cell::RefCell;

    #[derive(Clone, Copy)]
    struct Entry {
        addr: usize,
        len: usize,
        gc: u64,
        count: Option<usize>,
        /// A known position: character `ci` starts at byte `bi`.
        ci: usize,
        bi: usize,
    }

    const ENTRIES: usize = 4;

    thread_local! {
        /// Most recently used first.
        static CACHE: RefCell<[Option<Entry>; ENTRIES]> = const { RefCell::new([None; ENTRIES]) };
    }

    fn is_start(b: u8) -> bool {
        b & 0xC0 != 0x80
    }

    /// Characters in bytes.
    fn count_in(bytes: &[u8]) -> usize {
        bytes.iter().filter(|&&b| is_start(b)).count()
    }

    /// The byte n characters after the character starting at byte b.
    fn forward(bytes: &[u8], mut b: usize, mut n: usize) -> Option<usize> {
        while n > 0 {
            if b >= bytes.len() {
                return None;
            }
            b += 1;
            while b < bytes.len() && !is_start(bytes[b]) {
                b += 1;
            }
            n -= 1;
        }
        Some(b)
    }

    /// The byte n characters before the character starting at byte b.
    fn backward(bytes: &[u8], mut b: usize, mut n: usize) -> usize {
        while n > 0 {
            b -= 1;
            while !is_start(bytes[b]) {
                b -= 1;
            }
            n -= 1;
        }
        b
    }

    /// Run f on s's entry (a fresh one if there is none), moving it to the front.
    fn with_entry<R>(s: &str, f: impl FnOnce(&mut Entry) -> R) -> R {
        let addr = s.as_ptr() as usize;
        let gc = crate::heap::gc_count();
        CACHE.with(|c| {
            let mut c = c.borrow_mut();
            let found = c.iter().position(|e| e.is_some_and(|e| e.addr == addr && e.len == s.len() && e.gc == gc));
            let mut entry = match found {
                Some(i) => c[i].take().unwrap(),
                None => Entry { addr, len: s.len(), gc, count: None, ci: 0, bi: 0 },
            };
            let r = f(&mut entry);
            let at = found.unwrap_or(ENTRIES - 1);
            c[..=at].rotate_right(1);
            c[0] = Some(entry);
            r
        })
    }

    pub(super) fn char_count(s: &str) -> usize {
        with_entry(s, |e| *e.count.get_or_insert_with(|| count_in(s.as_bytes())))
    }

    pub(super) fn byte_of(s: &str, ci: usize) -> Option<usize> {
        let bytes = s.as_bytes();
        with_entry(s, |e| {
            if e.count.is_some_and(|n| ci > n) {
                return None;
            }
            // Count from the nearest known position: the start, the last
            // one found, or the end (when the count is known).
            let mut from = (0, 0);
            if ci.abs_diff(e.ci) < ci {
                from = (e.ci, e.bi);
            }
            if let Some(n) = e.count
                && n - ci < ci.abs_diff(from.0)
            {
                from = (n, s.len());
            }
            let b = if ci >= from.0 { forward(bytes, from.1, ci - from.0)? } else { backward(bytes, from.1, from.0 - ci) };
            (e.ci, e.bi) = (ci, b);
            Some(b)
        })
    }

    pub(super) fn char_of(s: &str, b: usize) -> usize {
        let bytes = s.as_bytes();
        with_entry(s, |e| {
            let mut from = (0, 0);
            if b.abs_diff(e.bi) < b {
                from = (e.ci, e.bi);
            }
            if let Some(n) = e.count
                && s.len() - b < b.abs_diff(from.1)
            {
                from = (n, s.len());
            }
            let ci = if b >= from.1 { from.0 + count_in(&bytes[from.1..b]) } else { from.0 - count_in(&bytes[b..from.1]) };
            (e.ci, e.bi) = (ci, b);
            ci
        })
    }
}
