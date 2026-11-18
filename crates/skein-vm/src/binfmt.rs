//! skein.bin (M9 part 9.11): Skein values as bytes, and back.
//!
//! A value is `NRNB`, a version byte (1), then the value: a tag byte and
//! what that tag takes. Counts and lengths are unsigned LEB128 varints;
//! integers that fit 64 bits are zigzag varints, larger ones a sign and
//! their magnitude's bytes. A keyword or symbol is written once: later
//! ones are a reference to the first (its index in the order they came).
//!
//! Reading is for untrusted input, as skein.edn's is: it only builds data.
//! Every count is checked against the bytes left (each item takes at least
//! one), nesting is limited, text must be UTF-8, a map or set may not
//! repeat a key, and nothing may follow the value. An instance is made
//! only of a class the reader is given (`classes`), from its slots, with
//! no code of the class's run.
//!
//! Not written: functions, vars, atoms, channels, fibers, handles, mutable
//! objects (values only), and metadata.

use crate::vm::{Error, Result};
use num_bigint::{BigInt, Sign};
use skein_rt::map::{map_assoc, map_empty, set_conj, set_empty};
use skein_rt::runtime::rt;
use skein_rt::string::{bytes_new, float_new, str_new};
use skein_rt::vector::vec_from_slice;
use skein_rt::{Kind, Mutator, Value, clos, colls, seq};
use std::collections::HashMap;

const MAGIC: &[u8; 4] = b"NRNB";
const VERSION: u8 = 1;

const NIL: u8 = 0;
const FALSE: u8 = 1;
const TRUE: u8 = 2;
const INT: u8 = 3;
const BIG: u8 = 4;
const FLOAT: u8 = 5;
const RATIO: u8 = 6;
const CHAR: u8 = 7;
const STRING: u8 = 8;
const KEYWORD: u8 = 9;
const SYMBOL: u8 = 10;
const LIST: u8 = 11;
const VECTOR: u8 = 12;
const MAP: u8 = 13;
const SET: u8 = 14;
const SORTED_MAP: u8 = 15;
const SORTED_SET: u8 = 16;
const QUEUE: u8 = 17;
const BYTES: u8 = 18;
const REGEX: u8 = 19;
const INSTANCE: u8 = 20;
const REF: u8 = 21;

// ---- writing ----

/// How deeply a value written may nest (writing recurses).
const MAX_WRITE_DEPTH: usize = 10_000;

struct Enc {
    out: Vec<u8>,
    depth: usize,
    /// Keywords and symbols written: (keyword?, namespace, name) → index.
    names: HashMap<(bool, Option<String>, String), u64>,
}

fn varint(out: &mut Vec<u8>, mut n: u64) {
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn write_int(out: &mut Vec<u8>, b: &BigInt) {
    use num_traits::ToPrimitive;
    if let Some(i) = b.to_i64() {
        out.push(INT);
        varint(out, ((i << 1) ^ (i >> 63)) as u64);
    } else {
        let (sign, mag) = b.to_bytes_le();
        out.push(BIG);
        out.push(u8::from(sign == Sign::Minus));
        varint(out, mag.len() as u64);
        out.extend_from_slice(&mag);
    }
}

fn not_written(v: Value) -> Error {
    Error::new(format!("skein.bin can't write {}: only data (values) can be written", v.type_name()))
}

impl Enc {
    fn text(&mut self, s: &str) {
        varint(&mut self.out, s.len() as u64);
        self.out.extend_from_slice(s.as_bytes());
    }

    fn name(&mut self, v: Value, keyword: bool) {
        let ns = v.sym_ns().map(|s| s.to_owned());
        let key = (keyword, ns.clone(), v.sym_name().to_owned());
        if let Some(&i) = self.names.get(&key) {
            self.out.push(REF);
            varint(&mut self.out, i);
            return;
        }
        let i = self.names.len() as u64;
        self.names.insert(key, i);
        self.out.push(if keyword { KEYWORD } else { SYMBOL });
        match &ns {
            None => varint(&mut self.out, 0),
            Some(n) => {
                varint(&mut self.out, n.len() as u64 + 1);
                self.out.extend_from_slice(n.as_bytes());
            }
        }
        self.text(v.sym_name());
    }

    fn write(&mut self, v: Value) -> Result<()> {
        if self.depth >= MAX_WRITE_DEPTH {
            return Err(Error::new(format!("skein.bin: the value nests more than {MAX_WRITE_DEPTH} deep")));
        }
        self.depth += 1;
        let r = self.write_one(v);
        self.depth -= 1;
        r
    }

    /// v, then what it holds. `head` writes v's tag and count (or all of v,
    /// if it holds nothing) and hands back the values it holds, in order. It
    /// is kept out of line because this frame is paid once per level of
    /// nesting, and an unoptimized build gives every branch's locals their
    /// own slots: with them inline, 10000 levels needed over 80 MB of stack.
    fn write_one(&mut self, v: Value) -> Result<()> {
        for x in self.head(v)? {
            self.write(x)?;
        }
        Ok(())
    }

    fn counted(&mut self, tag: u8, n: usize) {
        self.out.push(tag);
        varint(&mut self.out, n as u64);
    }

    #[inline(never)]
    fn head(&mut self, v: Value) -> Result<Vec<Value>> {
        let mut xs = Vec::new();
        if v.is(Kind::Vector) {
            v.vec_for_each(|x| {
                xs.push(x);
                true
            });
            self.counted(VECTOR, xs.len());
        } else if v.is(Kind::Map) {
            v.map_for_each(|k, x| {
                xs.push(k);
                xs.push(x);
                true
            });
            self.counted(MAP, xs.len() / 2);
        } else if v.is(Kind::Set) {
            v.set_for_each(|x| {
                xs.push(x);
                true
            });
            self.counted(SET, xs.len());
        } else if v.is(Kind::Sorted) {
            let entries = colls::sorted_entries(v);
            if v.sorted_is_set() {
                xs.extend(entries.into_iter().map(|e| e.0));
                self.counted(SORTED_SET, xs.len());
            } else {
                self.counted(SORTED_MAP, entries.len());
                for (k, x) in entries {
                    xs.push(k);
                    xs.push(x);
                }
            }
        } else if v.is(Kind::Queue) {
            xs = colls::queue_items(v);
            self.counted(QUEUE, xs.len());
        } else if v.is(Kind::Instance) {
            if v.instance_mutable() {
                return Err(Error::new("skein.bin can't write a mutable object: only values"));
            }
            let class = v.instance_class();
            self.out.push(INSTANCE);
            self.name(class.class_field(clos::class::NAME), false);
            let keys = clos::layout_keys(v.instance_layout()).to_vec();
            varint(&mut self.out, keys.len() as u64);
            for (i, k) in keys.into_iter().enumerate() {
                xs.push(k);
                xs.push(v.instance_raw(i));
            }
        } else if v.is_cons() || v.is(Kind::EmptyList) || v.is(Kind::Stream) || v.is(Kind::VecSeq) || v.is(Kind::Range) {
            if let Some(s) = seq::first_unrealized(v) {
                return Err(Error::force(s, true));
            }
            match seq::SeqIter::new(seq::stream_items(v)) {
                Some(it) => xs.extend(it),
                None => return Err(not_written(v)),
            }
            self.counted(LIST, xs.len());
        } else {
            self.write_leaf(v)?;
        }
        Ok(xs)
    }

    fn write_leaf(&mut self, v: Value) -> Result<()> {
        if v.is_nil() {
            self.out.push(NIL);
        } else if v == Value::TRUE {
            self.out.push(TRUE);
        } else if v == Value::FALSE {
            self.out.push(FALSE);
        } else if v.is_fixnum() {
            write_int(&mut self.out, &BigInt::from(v.as_fixnum()));
        } else if v.is_char() {
            self.out.push(CHAR);
            varint(&mut self.out, v.as_char() as u64);
        } else if v.is_keyword() {
            self.name(v, true);
        } else if v.is_symbol() {
            self.name(v, false);
        } else if v.is(Kind::BigInt) {
            let b = skein_rt::numbers::int_of(v).ok_or_else(|| not_written(v))?;
            write_int(&mut self.out, &b);
        } else if v.is(Kind::Float) {
            self.out.push(FLOAT);
            self.out.extend_from_slice(&v.as_f64().to_le_bytes());
        } else if v.is(Kind::Ratio) {
            let r = skein_rt::numbers::ratio_of(v).ok_or_else(|| not_written(v))?;
            self.out.push(RATIO);
            write_int(&mut self.out, r.numer());
            write_int(&mut self.out, r.denom());
        } else if v.is(Kind::String) {
            self.out.push(STRING);
            self.text(v.as_str());
        } else if v.is(Kind::Bytes) {
            self.out.push(BYTES);
            varint(&mut self.out, v.as_bytes().len() as u64);
            self.out.extend_from_slice(v.as_bytes());
        } else if v.is(Kind::Regex) {
            self.out.push(REGEX);
            self.text(v.field(0).as_str());
        } else {
            return Err(not_written(v));
        }
        Ok(())
    }
}

/// `(%bin-write x)`: x as Bytes.
pub(crate) fn write(m: &mut Mutator, v: Value) -> Result<Value> {
    let mut e = Enc { out: Vec::with_capacity(64), depth: 0, names: HashMap::new() };
    e.out.extend_from_slice(MAGIC);
    e.out.push(VERSION);
    e.write(v)?;
    Ok(bytes_new(m, &e.out))
}

// ---- reading ----

struct Dec<'a> {
    b: &'a [u8],
    i: usize,
    names: Vec<Value>,
    max_depth: usize,
    /// The classes instances may be of: class name → class.
    classes: Value,
}

fn bad(what: impl std::fmt::Display) -> Error {
    Error::of("ParseError", format!("Not skein.bin data: {what}")).with("source", crate::vm::ErrData::Str("skein.bin".into()))
}

fn unsorted(e: Error) -> Error {
    bad(format!("a sorted collection whose keys don't compare ({})", e.msg))
}

impl Dec<'_> {
    fn left(&self) -> usize {
        self.b.len() - self.i
    }

    fn byte(&mut self) -> Result<u8> {
        let x = *self.b.get(self.i).ok_or_else(|| bad("it ends too soon"))?;
        self.i += 1;
        Ok(x)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut n = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            n |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(n);
            }
        }
        Err(bad("a number runs on too long"))
    }

    /// A count of items, each at least `each` bytes: no more than what's left.
    fn count(&mut self, each: usize) -> Result<usize> {
        let n = self.varint()?;
        if n > (self.left() / each.max(1)) as u64 {
            return Err(bad(format!("it claims {n} items, more than the {} bytes left hold", self.left())));
        }
        Ok(n as usize)
    }

    fn slice(&mut self, n: usize) -> Result<&[u8]> {
        if n > self.left() {
            return Err(bad("it ends too soon"));
        }
        let s = &self.b[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }

    fn text(&mut self) -> Result<String> {
        let n = self.count(1)?;
        let s = self.slice(n)?;
        std::str::from_utf8(s).map(|s| s.to_owned()).map_err(|_| bad("text that isn't UTF-8"))
    }

    fn int(&mut self, m: &mut Mutator, tag: u8) -> Result<Value> {
        match tag {
            INT => {
                let z = self.varint()?;
                let i = ((z >> 1) as i64) ^ -((z & 1) as i64);
                Ok(skein_rt::numbers::int_value(m, &BigInt::from(i)))
            }
            BIG => {
                let neg = self.byte()? == 1;
                let n = self.count(1)?;
                let mag = self.slice(n)?;
                let b = BigInt::from_bytes_le(if neg { Sign::Minus } else { Sign::Plus }, mag);
                Ok(skein_rt::numbers::int_value(m, &b))
            }
            _ => Err(bad("an integer expected")),
        }
    }

    fn name(&mut self, m: &mut Mutator, keyword: bool) -> Result<Value> {
        let nsn = self.varint()?;
        let ns = if nsn == 0 {
            None
        } else {
            let n = (nsn - 1) as usize;
            let s = self.slice(n)?;
            Some(std::str::from_utf8(s).map_err(|_| bad("a name that isn't UTF-8"))?.to_owned())
        };
        let name = self.text()?;
        let v = if keyword { rt().keyword(m, ns.as_deref(), &name) } else { rt().symbol(m, ns.as_deref(), &name) };
        self.names.push(v);
        Ok(v)
    }

    fn read(&mut self, m: &mut Mutator, depth: usize) -> Result<Value> {
        if depth > self.max_depth {
            return Err(bad(format!("it nests deeper than {}", self.max_depth)));
        }
        let tag = self.byte()?;
        Ok(match tag {
            NIL => Value::NIL,
            FALSE => Value::FALSE,
            TRUE => Value::TRUE,
            INT | BIG => self.int(m, tag)?,
            FLOAT => {
                let s = self.slice(8)?;
                float_new(m, f64::from_le_bytes(s.try_into().unwrap()))
            }
            RATIO => {
                let t = self.byte()?;
                let n = self.int(m, t)?;
                let t = self.byte()?;
                let d = self.int(m, t)?;
                let (n, d) = (skein_rt::numbers::int_of(n).unwrap(), skein_rt::numbers::int_of(d).unwrap());
                if d.sign() != Sign::Plus {
                    return Err(bad("a ratio's denominator isn't positive"));
                }
                skein_rt::numbers::ratio_value(m, &num_rational::BigRational::new(n, d))
            }
            CHAR => {
                let c = self.varint()?;
                Value::char(char::from_u32(c as u32).filter(|_| c <= u32::MAX as u64).ok_or_else(|| bad("a character that isn't one"))?)
            }
            STRING => {
                let s = self.text()?;
                str_new(m, &s)
            }
            KEYWORD => self.name(m, true)?,
            SYMBOL => self.name(m, false)?,
            REF => {
                let i = self.varint()? as usize;
                *self.names.get(i).ok_or_else(|| bad("a reference to a name not yet seen"))?
            }
            LIST | VECTOR | SET | SORTED_SET | QUEUE => {
                let n = self.count(1)?;
                let mut xs = Vec::with_capacity(n);
                for _ in 0..n {
                    xs.push(self.read(m, depth + 1)?);
                }
                match tag {
                    LIST => skein_rt::list::list_from(m, &xs),
                    VECTOR => vec_from_slice(m, &xs),
                    SET => {
                        let mut s = set_empty();
                        for x in xs {
                            s = set_conj(m, s, x);
                        }
                        if s.set_count() != n {
                            return Err(bad("a set with an item twice"));
                        }
                        s
                    }
                    SORTED_SET => {
                        let mut s = colls::sorted_new(m, 0, Value::NIL, true);
                        for x in xs {
                            s = crate::sorted::assoc(m, s, x, x).map_err(unsorted)?; // (a set maps each item to itself)
                        }
                        if s.sorted_count() != n {
                            return Err(bad("a set with an item twice"));
                        }
                        s
                    }
                    _ => {
                        let e = rt().empty_vector;
                        let mut q = colls::queue_new(m, 0, Value::NIL, e);
                        for x in xs {
                            q = colls::queue_conj(m, q, x);
                        }
                        q
                    }
                }
            }
            MAP | SORTED_MAP => {
                let n = self.count(2)?;
                let mut out = if tag == MAP { map_empty() } else { colls::sorted_new(m, 0, Value::NIL, false) };
                for _ in 0..n {
                    let k = self.read(m, depth + 1)?;
                    let v = self.read(m, depth + 1)?;
                    out = if tag == MAP { map_assoc(m, out, k, v) } else { crate::sorted::assoc(m, out, k, v).map_err(unsorted)? };
                }
                let got = if tag == MAP { out.map_count() } else { out.sorted_count() };
                if got != n {
                    return Err(bad("a map with a key twice"));
                }
                out
            }
            BYTES => {
                let n = self.count(1)?;
                let s = self.slice(n)?.to_vec();
                bytes_new(m, &s)
            }
            REGEX => {
                let s = self.text()?;
                let p = str_new(m, &s);
                crate::text::regex_value(m, p).map_err(|e| bad(format!("a pattern that isn't one: {}", e.msg)))?
            }
            INSTANCE => {
                let t = self.byte()?;
                let name = match t {
                    SYMBOL => self.name(m, false)?,
                    REF => {
                        let i = self.varint()? as usize;
                        *self.names.get(i).ok_or_else(|| bad("a reference to a name not yet seen"))?
                    }
                    _ => return Err(bad("an instance's class name expected")),
                };
                let class = crate::prims::get(self.classes, name).filter(|c| c.is(Kind::Class)).ok_or_else(|| {
                    Error::new(format!(
                        "skein.bin: an instance of {}, which the reader wasn't given (:classes)",
                        skein_rt::print::pr_str(name, true)
                    ))
                })?;
                let n = self.count(2)?;
                let mut slots = HashMap::with_capacity(n);
                for _ in 0..n {
                    let k = self.read(m, depth + 1)?;
                    let v = self.read(m, depth + 1)?;
                    if !k.is_keyword() {
                        return Err(bad("a slot name that isn't a keyword"));
                    }
                    slots.insert(k.sym_name().to_owned(), v);
                }
                instance(m, class, slots, name)?
            }
            t => return Err(bad(format!("an unknown tag {t}"))),
        })
    }
}

/// An instance of class from its slots' values: as the class is defined
/// now, missing slots from its defaults; no code of the class's runs.
fn instance(m: &mut Mutator, class: Value, mut slots: HashMap<String, Value>, name: Value) -> Result<Value> {
    if clos::policy_flags(class) & clos::class::MUTABLE != 0 {
        return Err(Error::new("skein.bin reads values: not an instance of a mutable class"));
    }
    let layout = class.class_field(clos::class::LAYOUT);
    let keys = clos::layout_keys(layout).to_vec();
    let defaults = layout.arr_slice()[clos::layout_len(layout)..].to_vec();
    let vals: Vec<Value> = keys.iter().zip(defaults).map(|(k, d)| slots.remove(k.sym_name()).unwrap_or(d)).collect();
    if let Some(k) = slots.keys().next() {
        return Err(Error::new(format!("skein.bin: {} has no slot :{k}", skein_rt::print::pr_str(name, true))));
    }
    Ok(clos::instance_new(m, class, &vals, Value::NIL))
}

/// `(%bin-read bytes classes max-depth)`.
pub(crate) fn read(m: &mut Mutator, b: Value, classes: Value, max_depth: usize) -> Result<Value> {
    if !b.is(Kind::Bytes) {
        return Err(Error::new(format!("skein.bin reads bytes, not {}", b.type_name())));
    }
    let bytes = b.as_bytes();
    if bytes.len() < 5 || &bytes[..4] != MAGIC {
        return Err(bad("it doesn't start NRNB"));
    }
    if bytes[4] > VERSION {
        return Err(bad(format!("version {}, newer than this reader's ({VERSION})", bytes[4])));
    }
    let mut d = Dec { b: bytes, i: 5, names: Vec::new(), max_depth, classes };
    let v = d.read(m, 0)?;
    if d.left() > 0 {
        return Err(bad(format!("{} bytes after the value", d.left())));
    }
    Ok(v)
}
