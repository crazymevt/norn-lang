//! Arithmetic over the numeric tower (design §3.4): fixnums, big integers,
//! ratios and floats.
//!
//! Operands are promoted to the wider of the two (fixnum < bigint < ratio
//! < float), and results are normalised on the way back into Norn values: an
//! integer that fits is a fixnum, and a ratio with denominator 1 is an
//! integer. Fixnum operations that overflow promote to bigints instead of
//! failing. Dividing integers gives a ratio unless it divides exactly.

use crate::vm::{Error, Result};
use norn_rt::numbers::{int_of, int_value, ratio_of, ratio_value};
use norn_rt::print::pr_str;
use norn_rt::string::float_new;
use norn_rt::{Kind, Mutator, Value};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{FromPrimitive, Signed, ToPrimitive, Zero};
use std::cmp::Ordering;

#[derive(Clone, Debug)]
pub(crate) enum Num {
    I(i64),
    B(BigInt),
    R(BigRational),
    F(f64),
}

impl Num {
    fn level(&self) -> u8 {
        match self {
            Num::I(_) => 0,
            Num::B(_) => 1,
            Num::R(_) => 2,
            Num::F(_) => 3,
        }
    }

    pub(crate) fn as_f64(&self) -> f64 {
        match self {
            Num::I(i) => *i as f64,
            Num::B(b) => b.to_f64().unwrap_or(if b.is_negative() { f64::NEG_INFINITY } else { f64::INFINITY }),
            Num::R(r) => r.to_f64().unwrap_or(f64::NAN),
            Num::F(f) => *f,
        }
    }

    fn to_big(&self) -> BigInt {
        match self {
            Num::I(i) => BigInt::from(*i),
            Num::B(b) => b.clone(),
            _ => unreachable!("not an integer"),
        }
    }

    fn to_ratio(&self) -> BigRational {
        match self {
            Num::R(r) => r.clone(),
            _ => BigRational::from_integer(self.to_big()),
        }
    }

    fn is_zero(&self) -> bool {
        match self {
            Num::I(i) => *i == 0,
            Num::B(b) => b.is_zero(),
            Num::R(r) => r.is_zero(),
            Num::F(f) => *f == 0.0,
        }
    }
}

pub(crate) fn num(v: Value) -> Result<Num> {
    if v.is_fixnum() {
        Ok(Num::I(v.as_fixnum()))
    } else if v.is_float() {
        Ok(Num::F(v.as_f64()))
    } else if v.is(Kind::BigInt) {
        Ok(Num::B(int_of(v).unwrap()))
    } else if v.is(Kind::Ratio) {
        Ok(Num::R(ratio_of(v).unwrap()))
    } else {
        Err(Error::of("TypeError", format!("Expected a number, got {}: {}", v.type_name(), pr_str(v, true)))
            .with("expected", crate::vm::ErrData::Str("a number".into()))
            .with("value", crate::vm::ErrData::Val(v)))
    }
}

/// A number as a normalised Norn value.
pub(crate) fn value(m: &mut Mutator, n: Num) -> Value {
    match n {
        Num::I(i) => Value::fixnum(i).unwrap_or_else(|| int_value(m, &BigInt::from(i))),
        Num::B(b) => int_value(m, &b),
        Num::R(r) => ratio_value(m, &r),
        Num::F(f) => float_new(m, f),
    }
}

#[derive(Copy, Clone)]
pub(crate) enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

fn divide_by_zero() -> Error {
    Error::of("ArithmeticError", "Divide by zero")
}

pub(crate) fn arith2(x: Num, y: Num, op: Op) -> Result<Num> {
    if let (Num::I(a), Num::I(b)) = (&x, &y) {
        let (a, b) = (*a, *b);
        let r = match op {
            Op::Add => a.checked_add(b),
            Op::Sub => a.checked_sub(b),
            Op::Mul => a.checked_mul(b),
            Op::Div => {
                if b == 0 {
                    return Err(divide_by_zero());
                }
                match a.checked_rem(b) {
                    Some(0) => a.checked_div(b),
                    Some(_) => return Ok(Num::R(BigRational::new(a.into(), b.into()))),
                    None => None,
                }
            }
        };
        if let Some(r) = r {
            return Ok(Num::I(r));
        }
        // Overflow: redo it with big integers.
        return arith2(Num::B(BigInt::from(a)), Num::B(BigInt::from(b)), op);
    }
    Ok(match x.level().max(y.level()) {
        3 => {
            let (a, b) = (x.as_f64(), y.as_f64());
            Num::F(match op {
                Op::Add => a + b,
                Op::Sub => a - b,
                Op::Mul => a * b,
                Op::Div => a / b,
            })
        }
        2 => {
            let (a, b) = (x.to_ratio(), y.to_ratio());
            Num::R(match op {
                Op::Add => a + b,
                Op::Sub => a - b,
                Op::Mul => a * b,
                Op::Div => {
                    if b.is_zero() {
                        return Err(divide_by_zero());
                    }
                    a / b
                }
            })
        }
        _ => {
            let (a, b) = (x.to_big(), y.to_big());
            match op {
                Op::Add => Num::B(a + b),
                Op::Sub => Num::B(a - b),
                Op::Mul => Num::B(a * b),
                Op::Div => {
                    if b.is_zero() {
                        return Err(divide_by_zero());
                    }
                    let (q, r) = norn_rt::numbers::div_rem(&a, &b);
                    if r.is_zero() { Num::B(q) } else { Num::R(BigRational::new(a, b)) }
                }
            }
        }
    })
}

/// `+ - * /` over any number of arguments.
pub(crate) fn arith(m: &mut Mutator, a: &[Value], op: Op) -> Result<Value> {
    let (init, rest) = match (op, a.len()) {
        (Op::Add, 0) => return Ok(Value::fix(0)),
        (Op::Mul, 0) => return Ok(Value::fix(1)),
        // ((- x) negates: for a float, -0.0 from 0.0, as 0 - 0.0 isn't)
        (Op::Sub, 1) => match num(a[0])? {
            Num::F(f) => return Ok(float_new(m, -f)),
            _ => (Num::I(0), a),
        },
        (Op::Div, 1) => (Num::I(1), a),
        _ => (num(a[0])?, &a[1..]),
    };
    let mut acc = init;
    for &x in rest {
        acc = arith2(acc, num(x)?, op)?;
    }
    Ok(value(m, acc))
}

/// Numeric order: exact, and None when either is NaN (so `<`, `==`… are
/// false). Two floats compare as floats. A float and an exact number
/// compare exactly, by the float's exact value, not by rounding the exact
/// number to a float: otherwise 2^53 and 2^53 + 1 would both equal 2^53 as
/// a float while differing from each other, and no sort could order them.
pub(crate) fn partial_cmp(a: &Num, b: &Num) -> Option<Ordering> {
    match (a, b) {
        (Num::I(x), Num::I(y)) => Some(x.cmp(y)),
        (Num::F(x), Num::F(y)) => x.partial_cmp(y),
        (Num::F(x), exact) => exact_cmp(exact, *x).map(Ordering::reverse),
        (exact, Num::F(y)) => exact_cmp(exact, *y),
        _ if a.level().max(b.level()) == 2 => Some(a.to_ratio().cmp(&b.to_ratio())),
        _ => Some(a.to_big().cmp(&b.to_big())),
    }
}

/// An exact number (integer or ratio) against float f, exactly.
fn exact_cmp(exact: &Num, f: f64) -> Option<Ordering> {
    const EXACT_IN_F64: i64 = 1 << 53;
    if f.is_nan() {
        return None;
    }
    if f.is_infinite() {
        return Some(if f > 0.0 { Ordering::Less } else { Ordering::Greater });
    }
    // (an integer this small is exactly a float)
    if let Num::I(i) = exact
        && i.abs() <= EXACT_IN_F64
    {
        return (*i as f64).partial_cmp(&f);
    }
    let f = BigRational::from_f64(f)?;
    Some(exact.to_ratio().cmp(&f))
}

/// A total order for `compare` and `sort`: numeric, with NaN after every
/// other number (and equal to itself).
pub(crate) fn cmp(a: &Num, b: &Num) -> Ordering {
    partial_cmp(a, b).unwrap_or_else(|| {
        let nan = |n: &Num| matches!(n, Num::F(f) if f.is_nan());
        nan(a).cmp(&nan(b))
    })
}

/// `<`, `>`, `==` … over a chain of numbers.
pub(crate) fn compare_chain(a: &[Value], ok: fn(Ordering) -> bool) -> Result<Value> {
    let mut prev = num(a[0])?;
    for &x in &a[1..] {
        let n = num(x)?;
        if !partial_cmp(&prev, &n).is_some_and(ok) {
            // Still type-check the rest, as Clojure does.
            for &y in a {
                num(y)?;
            }
            return Ok(Value::FALSE);
        }
        prev = n;
    }
    Ok(Value::TRUE)
}

#[derive(Copy, Clone)]
pub(crate) enum IntDiv {
    Quot,
    Rem,
    Mod,
}

fn integer(v: Value, op: &str) -> Result<Num> {
    match num(v)? {
        n @ (Num::I(_) | Num::B(_)) => Ok(n),
        _ => Err(Error::of("TypeError", format!("{op} expects integers, got {}: {}", v.type_name(), pr_str(v, true)))
            .with("expected", crate::vm::ErrData::Str("an integer".into()))
            .with("value", crate::vm::ErrData::Val(v))),
    }
}

/// `quot`, `rem`, `mod` on integers of any size.
pub(crate) fn int_div(m: &mut Mutator, x: Value, y: Value, kind: IntDiv) -> Result<Value> {
    let name = match kind {
        IntDiv::Quot => "quot",
        IntDiv::Rem => "rem",
        IntDiv::Mod => "mod",
    };
    let (a, b) = (integer(x, name)?, integer(y, name)?);
    if b.is_zero() {
        return Err(divide_by_zero());
    }
    if let (Num::I(a), Num::I(b)) = (&a, &b) {
        let r = match kind {
            IntDiv::Quot => a.checked_div(*b),
            IntDiv::Rem => a.checked_rem(*b),
            IntDiv::Mod => a.checked_rem_euclid(*b).map(|r| if *b < 0 && r != 0 { r + b } else { r }),
        };
        if let Some(r) = r {
            return Ok(value(m, Num::I(r)));
        }
    }
    let (a, b) = (a.to_big(), b.to_big());
    let r = match kind {
        IntDiv::Quot => norn_rt::numbers::div_rem(&a, &b).0,
        IntDiv::Rem => norn_rt::numbers::div_rem(&a, &b).1,
        IntDiv::Mod => norn_rt::numbers::modulo(&a, &b),
    };
    Ok(int_value(m, &r))
}

/// `(int x)`: an integer, truncating toward zero (characters give their code).
pub(crate) fn to_int(m: &mut Mutator, v: Value) -> Result<Value> {
    if v.is_char() {
        return Ok(Value::fix(v.as_char() as i64));
    }
    match num(v).map_err(|_| Error::new(format!("int expects a number or character, got {}", v.type_name())))? {
        Num::I(_) | Num::B(_) => Ok(v),
        Num::R(r) => Ok(int_value(m, &r.to_integer())),
        Num::F(f) => {
            let t = f.trunc();
            match BigInt::from_f64(t) {
                Some(b) if t.is_finite() => Ok(int_value(m, &b)),
                _ => Err(Error::new(format!("Can't make an integer of {f}"))),
            }
        }
    }
}

pub(crate) fn numerator(m: &mut Mutator, v: Value) -> Result<Value> {
    match num(v)? {
        Num::R(r) => Ok(int_value(m, r.numer())),
        Num::I(_) | Num::B(_) => Ok(v),
        Num::F(_) => Err(Error::new("numerator expects a rational, got a Float")),
    }
}

pub(crate) fn denominator(v: Value) -> Result<Value> {
    match num(v)? {
        Num::R(_) => Ok(v.field(norn_rt::numbers::RATIO_DEN)),
        Num::I(_) | Num::B(_) => Ok(Value::fix(1)),
        Num::F(_) => Err(Error::new("denominator expects a rational, got a Float")),
    }
}

#[derive(Copy, Clone)]
pub(crate) enum Bit {
    And,
    Or,
    Xor,
}

/// `bit-and`, `bit-or`, `bit-xor` (two's complement, any size).
pub(crate) fn bits(m: &mut Mutator, a: &[Value], op: Bit, name: &str) -> Result<Value> {
    let mut acc = integer(a[0], name)?;
    for &x in &a[1..] {
        let y = integer(x, name)?;
        acc = match (&acc, &y) {
            (Num::I(p), Num::I(q)) => Num::I(match op {
                Bit::And => p & q,
                Bit::Or => p | q,
                Bit::Xor => p ^ q,
            }),
            _ => {
                let (p, q) = (acc.to_big(), y.to_big());
                Num::B(match op {
                    Bit::And => p & q,
                    Bit::Or => p | q,
                    Bit::Xor => p ^ q,
                })
            }
        };
    }
    Ok(value(m, acc))
}

pub(crate) fn bit_not(m: &mut Mutator, v: Value) -> Result<Value> {
    Ok(match integer(v, "bit-not")? {
        Num::I(i) => value(m, Num::I(!i)),
        n => int_value(m, &!n.to_big()),
    })
}

/// `bit-shift-left` (promotes instead of overflowing) and `bit-shift-right`
/// (arithmetic).
pub(crate) fn shift(m: &mut Mutator, v: Value, n: Value, left: bool) -> Result<Value> {
    let name = if left { "bit-shift-left" } else { "bit-shift-right" };
    let x = integer(v, name)?;
    if !n.is_fixnum() || !(0..=65536).contains(&n.as_fixnum()) {
        return Err(Error::new(format!("{name}: the shift must be an integer from 0 to 65536")));
    }
    let k = n.as_fixnum() as usize;
    if let Num::I(i) = x {
        if !left {
            return Ok(Value::fix(i >> k.min(63)));
        }
        if k < 63 && (i << k) >> k == i {
            return Ok(value(m, Num::I(i << k)));
        }
    }
    let b = x.to_big();
    Ok(int_value(m, &if left { b << k } else { b >> k }))
}
