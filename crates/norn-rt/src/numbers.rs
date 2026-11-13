//! The numeric tower above fixnums: big integers and ratios (design §3.4,
//! runtime.md §2.5).
//!
//! ```text
//! BigInt: [sign << 63 | limb count][limbs, u64, least significant first]
//! Ratio:  [numerator][denominator]   (integers: fixnums or BigInts)
//! ```
//!
//! **Always normalised**, so `=` and `hash` can stay structural:
//! - a BigInt never holds a value that fits a fixnum;
//! - a Ratio is in lowest terms, with a denominator above 1.
//!
//! The arithmetic itself uses `num-bigint` and `num-rational`; these
//! functions convert at the boundary.

use crate::heap::Mutator;
use crate::object::{Kind, alloc};
use crate::value::Value;
use num_bigint::{BigInt, BigUint, Sign};
use num_integer::Integer;
use num_rational::BigRational;
use num_traits::{One, ToPrimitive, Zero};

pub const BIG_LEN: usize = 0;
const NEG: u64 = 1 << 63;
pub const RATIO_NUM: usize = 0;
pub const RATIO_DEN: usize = 1;

/// An integer as a Norn value: a fixnum when it fits, else a BigInt.
pub fn int_value(m: &mut Mutator, n: &BigInt) -> Value {
    if let Some(v) = n.to_i64().and_then(Value::fixnum) {
        return v;
    }
    let (sign, limbs) = n.to_u64_digits();
    let v = alloc(m, Kind::BigInt, 1 + limbs.len());
    v.set_word(BIG_LEN, limbs.len() as u64 | if sign == Sign::Minus { NEG } else { 0 });
    for (i, &l) in limbs.iter().enumerate() {
        v.set_word(1 + i, l);
    }
    v
}

/// The integer value of a fixnum or BigInt.
pub fn int_of(v: Value) -> Option<BigInt> {
    if v.is_fixnum() {
        return Some(BigInt::from(v.as_fixnum()));
    }
    if !v.is(Kind::BigInt) {
        return None;
    }
    let w = v.word(BIG_LEN);
    let n = (w & !NEG) as usize;
    let mut digits = Vec::with_capacity(n * 2);
    for i in 0..n {
        let l = v.word(1 + i);
        digits.push(l as u32);
        digits.push((l >> 32) as u32);
    }
    let mag = BigUint::new(digits);
    Some(BigInt::from_biguint(if w & NEG != 0 { Sign::Minus } else { Sign::Plus }, mag))
}

/// A rational as a Norn value: an integer when the denominator is 1.
pub fn ratio_value(m: &mut Mutator, r: &BigRational) -> Value {
    // BigRational keeps lowest terms with a positive denominator.
    if r.denom().is_one() {
        return int_value(m, r.numer());
    }
    let num = int_value(m, r.numer());
    let den = int_value(m, r.denom());
    let v = alloc(m, Kind::Ratio, 2);
    v.set_field(RATIO_NUM, num);
    v.set_field(RATIO_DEN, den);
    v
}

/// The rational value of an integer or Ratio.
pub fn ratio_of(v: Value) -> Option<BigRational> {
    if v.is(Kind::Ratio) {
        return Some(BigRational::new_raw(int_of(v.field(RATIO_NUM))?, int_of(v.field(RATIO_DEN))?));
    }
    int_of(v).map(BigRational::from_integer)
}

/// n/d in lowest terms (d non-zero).
pub fn make_ratio(m: &mut Mutator, n: &BigInt, d: &BigInt) -> Value {
    ratio_value(m, &BigRational::new(n.clone(), d.clone()))
}

pub fn is_integer(v: Value) -> bool {
    v.is_fixnum() || v.is(Kind::BigInt)
}

pub fn is_number(v: Value) -> bool {
    v.is_fixnum() || v.is_float() || v.is(Kind::BigInt) || v.is(Kind::Ratio)
}

/// Value equality of two BigInts (both normalised).
pub fn big_equal(a: Value, b: Value) -> bool {
    let n = (a.word(BIG_LEN) & !NEG) as usize;
    a.word(BIG_LEN) == b.word(BIG_LEN) && (0..n).all(|i| a.word(1 + i) == b.word(1 + i))
}

pub fn big_hash(v: Value) -> u64 {
    let n = (v.word(BIG_LEN) & !NEG) as usize;
    let mut h = v.word(BIG_LEN) ^ 0xb16_b00b;
    for i in 0..n {
        h = crate::equal::mix64(h ^ v.word(1 + i));
    }
    h
}

/// Decimal text of a BigInt.
pub fn big_string(v: Value) -> String {
    int_of(v).map(|n| n.to_string()).unwrap_or_default()
}

/// Is a BigInt negative?
pub fn big_negative(v: Value) -> bool {
    v.word(BIG_LEN) & NEG != 0
}

/// Parse an integer literal of any size (`radix` 10 or 16).
pub fn parse_int(s: &str, radix: u32) -> Option<BigInt> {
    BigInt::parse_bytes(s.as_bytes(), radix)
}

/// Integer division of a by b, truncating toward zero, and its remainder.
pub fn div_rem(a: &BigInt, b: &BigInt) -> (BigInt, BigInt) {
    a.div_rem(b)
}

/// The modulus with the sign of the divisor (floored division).
pub fn modulo(a: &BigInt, b: &BigInt) -> BigInt {
    a.mod_floor(b)
}

pub fn is_zero(n: &BigInt) -> bool {
    n.is_zero()
}
