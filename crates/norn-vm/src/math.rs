//! Math and randomness (M7 part 6): the Rust side of `norn.math` and
//! `norn.random`.
//!
//! - Floating-point functions take any real number and return a float.
//! - Rounding to an integer is exact at any size: a float's integer part
//!   becomes a bignum when it doesn't fit a fixnum, and ratios round
//!   without going through floating point.
//! - Random numbers come from SplitMix64, which is counter-based: output n
//!   of the generator seeded s is `mix(s + n·γ)`. A generator is then just a
//!   seed and a counter (`norn.random` keeps the counter in an atom), so it
//!   is deterministic, cheap to split and safe to share between fibers.
//!   The default generator is per carrier, seeded from the OS clock and
//!   address-space randomness.

use crate::numeric::{Num, num, value};
use crate::prims::int;
use crate::vm::{Error, Result, Vm};
use norn_rt::string::float_new;
use norn_rt::Value;
use num_bigint::BigInt;
use num_integer::Integer;
use num_rational::BigRational;
use num_traits::{FromPrimitive, Signed};
use std::cell::Cell;

fn real(v: Value, op: &str) -> Result<f64> {
    num(v).map(|n| n.as_f64()).map_err(|_| Error::new(format!("{op} expects a number, got {}", norn_rt::print::pr_str(v, true))))
}

/// `(%math1 op x)`: a one-argument floating-point function.
pub(crate) fn math1(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let op = int(a[0], "math")?;
    let x = real(a[1], "math")?;
    let r = match op {
        0 => x.sqrt(),
        1 => x.cbrt(),
        2 => x.exp(),
        3 => x.ln(),
        4 => x.log10(),
        5 => x.log2(),
        6 => x.sin(),
        7 => x.cos(),
        8 => x.tan(),
        9 => x.asin(),
        10 => x.acos(),
        11 => x.atan(),
        12 => x.sinh(),
        13 => x.cosh(),
        14 => x.tanh(),
        15 => x.ln_1p(),
        16 => x.exp_m1(),
        _ => return Err(Error::new("unknown math function")),
    };
    Ok(float_new(&mut vm.m, r))
}

/// `(%math2 op x y)`: a two-argument floating-point function.
pub(crate) fn math2(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let op = int(a[0], "math")?;
    let x = real(a[1], "math")?;
    let y = real(a[2], "math")?;
    let r = match op {
        0 => x.powf(y),
        1 => x.atan2(y),
        2 => x.hypot(y),
        _ => return Err(Error::new("unknown math function")),
    };
    Ok(float_new(&mut vm.m, r))
}

/// `(%round mode x)`: x rounded to an integer: 0 floor, 1 ceiling,
/// 2 toward zero, 3 half away from zero, 4 half to even.
pub(crate) fn round(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let mode = int(a[0], "round")?;
    let n = num(a[1])?;
    let r: BigRational = match n {
        Num::I(_) | Num::B(_) => return Ok(a[1]),
        Num::R(r) => r,
        Num::F(f) => {
            if !f.is_finite() {
                return Err(Error::new(format!("Can't round {} to an integer", norn_rt::print::pr_str(a[1], true))));
            }
            BigRational::from_f64(f).unwrap()
        }
    };
    let out = match mode {
        0 => r.floor(),
        1 => r.ceil(),
        2 => r.trunc(),
        3 => r.round(),
        _ => {
            let fl = r.floor();
            let diff = &r - &fl;
            let half = BigRational::new(1.into(), 2.into());
            if diff > half || (diff == half && fl.to_integer().is_odd()) { fl + BigRational::from_integer(1.into()) } else { fl }
        }
    };
    Ok(value(&mut vm.m, Num::B(out.to_integer())))
}

fn integer(v: Value, op: &str) -> Result<BigInt> {
    norn_rt::numbers::int_of(v).ok_or_else(|| Error::new(format!("{op} expects an integer, got {}", norn_rt::print::pr_str(v, true))))
}

/// `(%gcd a b)` (non-negative).
pub(crate) fn gcd(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let x = integer(a[0], "gcd")?;
    let y = integer(a[1], "gcd")?;
    Ok(value(&mut vm.m, Num::B(x.gcd(&y))))
}

/// `(%isqrt n)`: the largest integer whose square is at most n.
pub(crate) fn isqrt(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let x = integer(a[0], "isqrt")?;
    if x.is_negative() {
        return Err(Error::new("isqrt of a negative number"));
    }
    Ok(value(&mut vm.m, Num::B(x.sqrt())))
}

/// `(%float-bits x)`: classify a float: 0 finite, 1 infinite, 2 NaN (and
/// any non-float number is finite).
pub(crate) fn float_class(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let f = if a[0].is_float() { a[0].as_f64() } else { real(a[0], "finite?")? };
    Ok(Value::fix(if f.is_nan() {
        2
    } else if f.is_infinite() {
        1
    } else {
        0
    }))
}

// ---- random numbers -------------------------------------------------------------------------

const GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Output `n` of the generator seeded `seed`.
fn nth(seed: u64, n: u64) -> u64 {
    mix(seed.wrapping_add(n.wrapping_mul(GAMMA)))
}

thread_local! {
    static DEFAULT: Cell<(u64, u64)> = Cell::new((initial_seed(), 0));
}

fn initial_seed() -> u64 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    let local = 0u8;
    mix(t ^ (&local as *const u8 as u64).rotate_left(32) ^ std::process::id() as u64)
}

fn next_default() -> u64 {
    DEFAULT.with(|c| {
        let (s, n) = c.get();
        c.set((s, n.wrapping_add(1)));
        nth(s, n)
    })
}

/// 64 random bits as a Norn value: `kind` 0 a fixnum in [0, 2^62), 1 a
/// float in [0, 1).
fn bits_value(vm: &mut Vm, bits: u64, kind: i64) -> Value {
    if kind == 1 {
        float_new(&mut vm.m, (bits >> 11) as f64 * (1.0 / (1u64 << 53) as f64))
    } else {
        Value::fix((bits >> 2) as i64)
    }
}

/// `(%rand kind)`: from this carrier's default generator.
pub(crate) fn rand(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let kind = int(a[0], "rand")?;
    Ok(bits_value(vm, next_default(), kind))
}

/// `(%rand-below n)`: a uniform integer in [0, n) from the default
/// generator (n a positive fixnum), without modulo bias.
pub(crate) fn rand_below(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let n = int(a[0], "rand-int")?;
    if n <= 0 {
        return Err(Error::new("rand-int needs a positive bound"));
    }
    Ok(Value::fix(below(n as u64, next_default) as i64))
}

/// Lemire's method: a uniform integer in [0, n).
fn below(n: u64, mut next: impl FnMut() -> u64) -> u64 {
    loop {
        let m = (next() as u128) * (n as u128);
        let lo = m as u64;
        if lo >= n.wrapping_neg() % n {
            return (m >> 64) as u64;
        }
    }
}

fn seed_of(v: Value) -> Result<u64> {
    if v.is_fixnum() {
        return Ok(v.as_fixnum() as u64);
    }
    let b = integer(v, "generator")?;
    let (_, digits) = b.to_u64_digits();
    Ok(digits.iter().fold(0u64, |h, &d| mix(h ^ d)))
}

/// `(%rand-at seed n kind)`: output n of the generator seeded `seed`.
pub(crate) fn rand_at(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let seed = seed_of(a[0])?;
    let n = int(a[1], "random")? as u64;
    let kind = int(a[2], "random")?;
    Ok(bits_value(vm, nth(mix(seed), n), kind))
}

/// `(%rand-below-at seed n bound)`: a uniform integer in [0, bound) from
/// the seeded generator, using outputs n, n+1, … as needed; returns
/// `[value next-n]`.
pub(crate) fn rand_below_at(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let seed = mix(seed_of(a[0])?);
    let mut n = int(a[1], "random")? as u64;
    let bound = int(a[2], "random")?;
    if bound <= 0 {
        return Err(Error::new("random: the bound must be positive"));
    }
    let v = below(bound as u64, || {
        let r = nth(seed, n);
        n = n.wrapping_add(1);
        r
    });
    Ok(norn_rt::vector::vec_from_slice(&mut vm.m, &[Value::fix(v as i64), Value::fix((n & ((1 << 62) - 1)) as i64)]))
}

/// `(%rand-seed)`: a fresh seed from the default generator.
pub(crate) fn rand_seed(_: &mut Vm, _: &[Value]) -> Result<Value> {
    Ok(Value::fix((next_default() >> 2) as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_is_in_range_and_covers_it() {
        let mut n = 0u64;
        let mut seen = [false; 7];
        for _ in 0..1000 {
            let v = below(7, || {
                n += 1;
                nth(42, n)
            });
            assert!(v < 7);
            seen[v as usize] = true;
        }
        assert!(seen.iter().all(|&s| s));
    }

    #[test]
    fn generators_are_deterministic() {
        assert_eq!(nth(1, 5), nth(1, 5));
        assert_ne!(nth(1, 5), nth(2, 5));
    }
}
