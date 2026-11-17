//! The universal value word. See docs/internals/runtime.md §2.1.
//!
//! ```text
//! ...xxxxxxx0   fixnum (63-bit)
//! ...pppp001    pointer to heap object (has a header)
//! ...pppp011    pointer to cons cell (headerless, two words)
//! ...ssss101    immediate: sub-tag in bits 3..7, payload in bits 8..63
//! ...xxxx111    reserved
//! ```

use std::fmt;

#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct Value(pub u64);

pub const TAG_MASK: u64 = 0b111;
pub const TAG_OBJ: u64 = 0b001;
pub const TAG_CONS: u64 = 0b011;
pub const TAG_IMM: u64 = 0b101;

const SUB_SHIFT: u32 = 3;
const SUB_MASK: u64 = 0x1f;
const PAYLOAD_SHIFT: u32 = 8;

pub const SUB_NIL: u64 = 0;
pub const SUB_TRUE: u64 = 1;
pub const SUB_FALSE: u64 = 2;
pub const SUB_CHAR: u64 = 3;
pub const SUB_UNBOUND: u64 = 4;
pub const SUB_EOF: u64 = 5;
/// Internal marker used inside HAMT nodes; never visible to Skein code.
pub const SUB_NODE: u64 = 6;
/// Returned by a `retry` restart: the interpreter re-executes the
/// instruction that failed (runtime.md §5.2). Never visible to Skein code.
pub const SUB_RETRY: u64 = 7;

const fn imm(sub: u64, payload: u64) -> u64 {
    (payload << PAYLOAD_SHIFT) | (sub << SUB_SHIFT) | TAG_IMM
}

pub const FIXNUM_MIN: i64 = -(1 << 62);
pub const FIXNUM_MAX: i64 = (1 << 62) - 1;

impl Value {
    pub const NIL: Value = Value(imm(SUB_NIL, 0));
    pub const TRUE: Value = Value(imm(SUB_TRUE, 0));
    pub const FALSE: Value = Value(imm(SUB_FALSE, 0));
    pub const UNBOUND: Value = Value(imm(SUB_UNBOUND, 0));
    pub const EOF: Value = Value(imm(SUB_EOF, 0));
    pub const NODE: Value = Value(imm(SUB_NODE, 0));
    pub const RETRY: Value = Value(imm(SUB_RETRY, 0));

    /// A fixnum, or `None` if `n` doesn't fit in 63 bits.
    #[inline]
    pub fn fixnum(n: i64) -> Option<Value> {
        if (FIXNUM_MIN..=FIXNUM_MAX).contains(&n) {
            Some(Value((n << 1) as u64))
        } else {
            None
        }
    }

    /// A fixnum from a value the caller knows is in range.
    #[inline]
    pub const fn fix(n: i64) -> Value {
        Value((n << 1) as u64)
    }

    #[inline]
    pub fn is_fixnum(self) -> bool {
        self.0 & 1 == 0
    }

    #[inline]
    pub fn as_fixnum(self) -> i64 {
        (self.0 as i64) >> 1
    }

    #[inline]
    pub fn tag(self) -> u64 {
        self.0 & TAG_MASK
    }

    #[inline]
    pub fn is_obj(self) -> bool {
        self.tag() == TAG_OBJ
    }

    #[inline]
    pub fn is_cons(self) -> bool {
        self.tag() == TAG_CONS
    }

    #[inline]
    pub fn is_imm(self) -> bool {
        self.tag() == TAG_IMM
    }

    #[inline]
    pub fn imm_kind(self) -> u64 {
        (self.0 >> SUB_SHIFT) & SUB_MASK
    }

    #[inline]
    pub fn bool(b: bool) -> Value {
        if b { Value::TRUE } else { Value::FALSE }
    }

    #[inline]
    pub fn is_bool(self) -> bool {
        self == Value::TRUE || self == Value::FALSE
    }

    #[inline]
    pub fn char(c: char) -> Value {
        Value(imm(SUB_CHAR, c as u64))
    }

    #[inline]
    pub fn is_char(self) -> bool {
        self.is_imm() && self.imm_kind() == SUB_CHAR
    }

    #[inline]
    pub fn as_char(self) -> char {
        char::from_u32((self.0 >> PAYLOAD_SHIFT) as u32).unwrap_or('\u{fffd}')
    }

    #[inline]
    pub fn is_nil(self) -> bool {
        self == Value::NIL
    }

    /// `nil` and `false` are falsey; everything else is truthy.
    #[inline]
    pub fn truthy(self) -> bool {
        self != Value::NIL && self != Value::FALSE
    }

    /// Untagged address of a heap object or cons.
    #[inline]
    pub fn ptr(self) -> *mut u64 {
        (self.0 & !TAG_MASK) as *mut u64
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", crate::print::pr_str(*self, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixnums_round_trip() {
        for n in [0, 1, -1, 42, FIXNUM_MAX, FIXNUM_MIN] {
            let v = Value::fixnum(n).unwrap();
            assert!(v.is_fixnum());
            assert_eq!(v.as_fixnum(), n);
        }
        assert!(Value::fixnum(FIXNUM_MAX + 1).is_none());
        assert!(Value::fixnum(FIXNUM_MIN - 1).is_none());
    }

    #[test]
    fn immediates_are_distinct() {
        let all = [Value::NIL, Value::TRUE, Value::FALSE, Value::UNBOUND, Value::EOF, Value::NODE];
        for (i, a) in all.iter().enumerate() {
            assert!(a.is_imm() && !a.is_fixnum());
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert!(!Value::NIL.truthy() && !Value::FALSE.truthy());
        assert!(Value::TRUE.truthy() && Value::fix(0).truthy());
        assert_eq!(Value::char('λ').as_char(), 'λ');
    }
}
