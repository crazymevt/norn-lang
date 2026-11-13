//! Primitives: Rust leaf functions bound to vars in loom.core.
//!
//! Rule (runtime.md §2.3): primitives never call Loom code. The exceptions
//! are the meta-level ones (`eval`, `load-file`, `macroexpand`), which
//! re-enter the VM through `Vm::apply` on a fresh stack: a continuation
//! barrier, acceptable for the seed system.
//!
//! Primitives are identified by *index* into `PRIMS`, not by function
//! pointer, so images can save and restore them (runtime.md §7).

use crate::vm::{Error, Result, Vm};
use loom_rt::equal::{equal, hash};
use loom_rt::func::{VAR_MACRO, primitive_new};
use loom_rt::list::{list_from, range_new};
use loom_rt::map::{map_assoc, map_dissoc, map_empty, set_conj, set_disj, set_empty};
use loom_rt::print::{pr_str, write_value};
use loom_rt::runtime::rt;
use loom_rt::seq;
use loom_rt::string::{float_new, str_new};
use loom_rt::vector::{vec_assoc, vec_conj, vec_empty, vec_from_slice, vec_pop};
use loom_rt::{Kind, Value};
use std::cmp::Ordering;
use std::io::Write;

pub type PrimFn = fn(&mut Vm, &[Value]) -> Result<Value>;

pub struct PrimDef {
    pub name: &'static str,
    pub min: usize,
    pub max: Option<usize>,
    pub f: PrimFn,
}

/// Index of `apply`, which the interpreter handles itself.
pub const APPLY: usize = 0;

const VAR: Option<usize> = None;

macro_rules! prims {
    ($($name:literal $min:literal $max:expr => $f:expr;)*) => {
        pub static PRIMS: &[PrimDef] = &[$(PrimDef { name: $name, min: $min, max: $max, f: $f }),*];
    };
}

prims! {
    "apply" 2 VAR => |_, _| unreachable!("apply is handled by the interpreter");
    // numbers
    "+" 0 VAR => |vm, a| arith(vm, a, Op::Add);
    "-" 1 VAR => |vm, a| arith(vm, a, Op::Sub);
    "*" 0 VAR => |vm, a| arith(vm, a, Op::Mul);
    "/" 1 VAR => |vm, a| arith(vm, a, Op::Div);
    "quot" 2 Some(2) => |_, a| int_op(a, "quot", i64::checked_div);
    "rem" 2 Some(2) => |_, a| int_op(a, "rem", i64::checked_rem);
    "mod" 2 Some(2) => |_, a| int_op(a, "mod", |x, y| x.checked_rem_euclid(y).map(|r| if y < 0 && r != 0 { r + y } else { r }));
    "inc" 1 Some(1) => |vm, a| arith(vm, &[a[0], Value::fix(1)], Op::Add);
    "dec" 1 Some(1) => |vm, a| arith(vm, &[a[0], Value::fix(1)], Op::Sub);
    "<" 1 VAR => |_, a| compare_chain(a, |o| o == Ordering::Less);
    ">" 1 VAR => |_, a| compare_chain(a, |o| o == Ordering::Greater);
    "<=" 1 VAR => |_, a| compare_chain(a, |o| o != Ordering::Greater);
    ">=" 1 VAR => |_, a| compare_chain(a, |o| o != Ordering::Less);
    "==" 1 VAR => |_, a| compare_chain(a, |o| o == Ordering::Equal);
    "float" 1 Some(1) => |vm, a| Ok(float_new(&mut vm.m, num(a[0])?.as_f64()));
    "int" 1 Some(1) => |_, a| to_int(a[0]);
    // equality, identity, ordering
    "=" 1 VAR => |_, a| Ok(Value::bool(a.windows(2).all(|w| equal(w[0], w[1]))));
    "not=" 1 VAR => |_, a| Ok(Value::bool(!a.windows(2).all(|w| equal(w[0], w[1]))));
    "identical?" 2 Some(2) => |_, a| Ok(Value::bool(a[0] == a[1]));
    "hash" 1 Some(1) => |_, a| Ok(Value::fix((hash(a[0]) >> 2) as i64));
    "compare" 2 Some(2) => |_, a| Ok(Value::fix(match compare(a[0], a[1])? { Ordering::Less => -1, Ordering::Equal => 0, Ordering::Greater => 1 }));
    // type predicates
    "nil?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_nil()));
    "number?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_fixnum() || a[0].is_float()));
    "integer?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_fixnum()));
    "float?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_float()));
    "string?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::String)));
    "symbol?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_symbol()));
    "keyword?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_keyword()));
    "char?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_char()));
    "boolean?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_bool()));
    "fn?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Closure) || a[0].is(Kind::Primitive)));
    "list?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_cons() || a[0].is(Kind::EmptyList)));
    "vector?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Vector)));
    "map?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Map)));
    "set?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Set)));
    "seq?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_cons() || (a[0].is_obj() && matches!(a[0].obj_kind(), Kind::EmptyList | Kind::VecSeq | Kind::Range))));
    "coll?" 1 Some(1) => |_, a| Ok(Value::bool(seq::is_seqable(a[0]) && !a[0].is_nil() && !a[0].is(Kind::String)));
    "seqable?" 1 Some(1) => |_, a| Ok(Value::bool(seq::is_seqable(a[0])));
    "var?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Var)));
    "%indexed?" 1 Some(1) => |_, a| Ok(Value::bool(seq::is_indexed(a[0])));
    "type" 1 Some(1) => |vm, a| { let n = a[0].type_name().to_lowercase(); Ok(rt().keyword(&mut vm.m, None, &n)) };
    // collections
    "list" 0 VAR => |vm, a| Ok(list_from(&mut vm.m, a));
    "vector" 0 VAR => |vm, a| Ok(vec_from_slice(&mut vm.m, a));
    "hash-map" 0 VAR => |vm, a| hash_map(vm, a);
    "hash-set" 0 VAR => |vm, a| Ok(a.iter().fold(set_empty(), |s, &x| set_conj(&mut vm.m, s, x)));
    "vec" 1 Some(1) => |vm, a| { if a[0].is(Kind::Vector) { return Ok(a[0]); } let xs = to_vec(vm, a[0])?; Ok(vec_from_slice(&mut vm.m, &xs)) };
    "set" 1 Some(1) => |vm, a| { let xs = to_vec(vm, a[0])?; Ok(xs.into_iter().fold(set_empty(), |s, x| set_conj(&mut vm.m, s, x))) };
    "cons" 2 Some(2) => |vm, a| Ok(seq::cons_onto(&mut vm.m, a[0], a[1])?);
    "first" 1 Some(1) => |vm, a| Ok(seq::first(&mut vm.m, a[0])?);
    "rest" 1 Some(1) => |vm, a| Ok(seq::rest(&mut vm.m, a[0])?);
    "next" 1 Some(1) => |vm, a| Ok(seq::next(&mut vm.m, a[0])?);
    "seq" 1 Some(1) => |vm, a| Ok(seq::seq(&mut vm.m, a[0])?);
    "count" 1 Some(1) => |_, a| Ok(Value::fix(seq::count(a[0])? as i64));
    "nth" 2 Some(3) => nth;
    "get" 2 Some(3) => |_, a| Ok(get(a[0], a[1]).unwrap_or(a.get(2).copied().unwrap_or(Value::NIL)));
    "assoc" 3 VAR => assoc;
    "dissoc" 1 VAR => |vm, a| { let mut m = a[0]; if m.is_nil() { return Ok(m); } expect(m, Kind::Map, "dissoc")?; for &k in &a[1..] { m = map_dissoc(&mut vm.m, m, k); } Ok(m) };
    "conj" 0 VAR => conj;
    "disj" 1 VAR => |vm, a| { let mut s = a[0]; if s.is_nil() { return Ok(s); } expect(s, Kind::Set, "disj")?; for &k in &a[1..] { s = set_disj(&mut vm.m, s, k); } Ok(s) };
    "contains?" 2 Some(2) => contains;
    "keys" 1 Some(1) => |vm, a| map_part(vm, a[0], true);
    "vals" 1 Some(1) => |vm, a| map_part(vm, a[0], false);
    "empty" 1 Some(1) => |_, a| Ok(empty_of(a[0]));
    "peek" 1 Some(1) => peek;
    "pop" 1 Some(1) => pop;
    "concat" 0 VAR => |vm, a| { let mut out = Vec::new(); for &x in a { out.extend(to_vec(vm, x)?); } Ok(if out.is_empty() { rt().empty_list } else { list_from(&mut vm.m, &out) }) };
    "range" 0 Some(3) => range;
    // strings, symbols, keywords
    "str" 0 VAR => |vm, a| { let mut s = String::new(); for &x in a { if !x.is_nil() { write_value(&mut s, x, false); } } Ok(str_new(&mut vm.m, &s)) };
    "subs" 2 Some(3) => subs;
    "name" 1 Some(1) => name;
    "namespace" 1 Some(1) => |vm, a| { let x = a[0]; if !(x.is_symbol() || x.is_keyword()) { return Err(type_err("namespace", "a symbol or keyword", x)); } Ok(match x.sym_ns() { Some(ns) => str_new(&mut vm.m, ns), None => Value::NIL }) };
    "symbol" 1 Some(2) => |vm, a| name_like(vm, a, false);
    "keyword" 1 Some(2) => |vm, a| name_like(vm, a, true);
    "gensym" 0 Some(1) => |vm, a| { let p = if a.is_empty() { "G__".to_owned() } else { text(a[0]) }; Ok(rt().symbol(&mut vm.m, None, &format!("{p}{}", crate::reader::next_id()))) };
    "char" 1 Some(1) => |_, a| { let n = int(a[0], "char")?; char::from_u32(n as u32).map(Value::char).ok_or_else(|| Error::new(format!("Invalid code point: {n}"))) };
    // printing and I/O
    "pr-str" 0 VAR => |vm, a| Ok(str_new(&mut vm.m, &join(a, true)));
    "print" 0 VAR => |_, a| out(&join(a, false), false);
    "println" 0 VAR => |_, a| out(&join(a, false), true);
    "pr" 0 VAR => |_, a| out(&join(a, true), false);
    "prn" 0 VAR => |_, a| out(&join(a, true), true);
    "slurp" 1 Some(1) => |vm, a| { let p = text(a[0]); let s = std::fs::read_to_string(&p).map_err(|e| Error::new(format!("Can't read {p}: {e}")))?; Ok(str_new(&mut vm.m, &s)) };
    "spit" 2 Some(2) => |_, a| { let p = text(a[0]); std::fs::write(&p, text(a[1])).map_err(|e| Error::new(format!("Can't write {p}: {e}")))?; Ok(Value::NIL) };
    "read-string" 1 Some(1) => |vm, a| { let s = text(a[0]); let mut r = crate::reader::Reader::new(&s); match r.read(vm) { Ok(v) => Ok(v), Err(crate::reader::ReadError::Eof) => Ok(Value::NIL), Err(e) => Err(Error::new(e.to_string())) } };
    // evaluation and namespaces
    "eval" 1 Some(1) => |vm, a| vm.eval(a[0]);
    "macroexpand-1" 1 Some(1) => |vm, a| Ok(vm.macroexpand_1(a[0])?.0);
    "macroexpand" 1 Some(1) => |vm, a| vm.macroexpand(a[0]);
    "load-file" 1 Some(1) => |vm, a| vm.load_file(&text(a[0]));
    "load-string" 1 Some(1) => |vm, a| vm.load_str(&text(a[0]), "<string>");
    "in-ns" 1 Some(1) => |vm, a| { if !a[0].is_symbol() { return Err(type_err("in-ns", "a symbol", a[0])); } vm.ns = rt().find_or_create_ns(a[0]); Ok(a[0]) };
    "current-ns" 0 Some(0) => |vm, _| Ok(vm.ns.name);
    "all-ns" 0 Some(0) => |vm, _| { let names: Vec<Value> = rt().all_namespaces().iter().map(|n| n.name).collect(); Ok(list_from(&mut vm.m, &names)) };
    "ns-publics" 1 Some(1) => |vm, a| { let ns = rt().find_ns(a[0]).ok_or_else(|| Error::new(format!("No namespace: {}", pr_str(a[0], true))))?; let mut m = map_empty(); for (k, v) in ns.mappings() { if v.var_ns() == ns.name { m = map_assoc(&mut vm.m, m, k, v); } } Ok(m) };
    "resolve" 1 Some(1) => |vm, a| Ok(vm.resolve(a[0]).unwrap_or(Value::NIL));
    "var-get" 1 Some(1) => |_, a| { expect(a[0], Kind::Var, "var-get")?; Ok(a[0].var_root()) };
    "%set-root!" 2 Some(2) => |_, a| { expect(a[0], Kind::Var, "%set-root!")?; a[0].var_set_root(a[1]); Ok(a[1]) };
    "%set-macro!" 1 Some(1) => |_, a| { expect(a[0], Kind::Var, "%set-macro!")?; a[0].var_set_flag(VAR_MACRO); Ok(a[0]) };
    "macro?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Var) && a[0].var_is_macro()));
    "error" 1 VAR => |_, a| Err(Error::new(join(a, false)));
    "disassemble" 1 Some(1) => disassemble;
    // system
    "nano-time" 0 Some(0) => |_, _| { use std::sync::OnceLock; use std::time::Instant; static T0: OnceLock<Instant> = OnceLock::new(); Ok(Value::fix(T0.get_or_init(Instant::now).elapsed().as_nanos() as i64)) };
    "heap-bytes" 0 Some(0) => |_, _| Ok(Value::fix(loom_rt::heap::heap_bytes() as i64));
    "exit" 0 Some(1) => |_, a| { let _ = std::io::stdout().flush(); std::process::exit(a.first().map(|v| v.as_fixnum() as i32).unwrap_or(0)) };
}

/// Bind every primitive to a var in loom.core.
pub fn install(vm: &mut Vm) {
    for (i, p) in PRIMS.iter().enumerate() {
        let sym = rt().symbol(&mut vm.m, None, p.name);
        let qualified = rt().symbol(&mut vm.m, Some("loom.core"), p.name);
        let prim = primitive_new(&mut vm.m, qualified, i, p.min, p.max);
        let var = vm.core.intern(&mut vm.m, sym);
        var.var_set_root(prim);
    }
}

// ---- helpers ------------------------------------------------------------------

fn type_err(op: &str, wanted: &str, got: Value) -> Error {
    Error::new(format!("{op} expects {wanted}, got {}: {}", got.type_name(), pr_str(got, true)))
}

fn expect(v: Value, k: Kind, op: &str) -> Result<()> {
    if v.is(k) { Ok(()) } else { Err(type_err(op, loom_rt::object::LAYOUTS[k as usize].name, v)) }
}

fn int(v: Value, op: &str) -> Result<i64> {
    if v.is_fixnum() { Ok(v.as_fixnum()) } else { Err(type_err(op, "an integer", v)) }
}

/// Display text of a value (strings raw).
fn text(v: Value) -> String {
    pr_str(v, false)
}

fn join(a: &[Value], readably: bool) -> String {
    let mut s = String::new();
    for (i, &x) in a.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        write_value(&mut s, x, readably);
    }
    s
}

fn out(s: &str, newline: bool) -> Result<Value> {
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(s.as_bytes());
    if newline {
        let _ = o.write_all(b"\n");
    }
    let _ = o.flush();
    Ok(Value::NIL)
}

fn to_vec(vm: &mut Vm, v: Value) -> Result<Vec<Value>> {
    Ok(seq::to_vec(&mut vm.m, v)?)
}

// ---- numbers -------------------------------------------------------------------

#[derive(Copy, Clone)]
enum Num {
    I(i64),
    F(f64),
}

impl Num {
    fn as_f64(self) -> f64 {
        match self {
            Num::I(i) => i as f64,
            Num::F(f) => f,
        }
    }
}

fn num(v: Value) -> Result<Num> {
    if v.is_fixnum() {
        Ok(Num::I(v.as_fixnum()))
    } else if v.is_float() {
        Ok(Num::F(v.as_f64()))
    } else {
        Err(Error::new(format!("Expected a number, got {}: {}", v.type_name(), pr_str(v, true))))
    }
}

fn overflow() -> Error {
    Error::new("Integer overflow (bignums are not implemented yet)")
}

fn num_value(vm: &mut Vm, n: Num) -> Result<Value> {
    match n {
        Num::I(i) => Value::fixnum(i).ok_or_else(overflow),
        Num::F(f) => Ok(float_new(&mut vm.m, f)),
    }
}

#[derive(Copy, Clone)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

fn arith2(x: Num, y: Num, op: Op) -> Result<Num> {
    Ok(match (x, y) {
        (Num::I(a), Num::I(b)) => Num::I(match op {
            Op::Add => a.checked_add(b).ok_or_else(overflow)?,
            Op::Sub => a.checked_sub(b).ok_or_else(overflow)?,
            Op::Mul => a.checked_mul(b).ok_or_else(overflow)?,
            Op::Div => {
                if b == 0 {
                    return Err(Error::new("Divide by zero"));
                }
                if a % b != 0 {
                    return Err(Error::new(format!(
                        "(/ {a} {b}) is a ratio, and ratios are not implemented yet; use quot, or a float argument"
                    )));
                }
                a / b
            }
        }),
        _ => {
            let (a, b) = (x.as_f64(), y.as_f64());
            Num::F(match op {
                Op::Add => a + b,
                Op::Sub => a - b,
                Op::Mul => a * b,
                Op::Div => a / b,
            })
        }
    })
}

fn arith(vm: &mut Vm, a: &[Value], op: Op) -> Result<Value> {
    // Fast path: two fixnums.
    if let [x, y] = a
        && x.is_fixnum()
        && y.is_fixnum()
    {
        let (p, q) = (x.as_fixnum(), y.as_fixnum());
        let r = match op {
            Op::Add => p.checked_add(q),
            Op::Sub => p.checked_sub(q),
            Op::Mul => p.checked_mul(q),
            Op::Div => None,
        };
        if let Some(v) = r.and_then(Value::fixnum) {
            return Ok(v);
        }
    }
    let (init, rest) = match (op, a.len()) {
        (Op::Add, 0) => return Ok(Value::fix(0)),
        (Op::Mul, 0) => return Ok(Value::fix(1)),
        (Op::Sub, 1) => (Num::I(0), a),
        (Op::Div, 1) => (Num::I(1), a),
        _ => (num(a[0])?, &a[1..]),
    };
    let mut acc = init;
    for &x in rest {
        acc = arith2(acc, num(x)?, op)?;
    }
    num_value(vm, acc)
}

fn int_op(a: &[Value], name: &str, f: fn(i64, i64) -> Option<i64>) -> Result<Value> {
    let (x, y) = (int(a[0], name)?, int(a[1], name)?);
    if y == 0 {
        return Err(Error::new("Divide by zero"));
    }
    Value::fixnum(f(x, y).ok_or_else(overflow)?).ok_or_else(overflow)
}

fn to_int(v: Value) -> Result<Value> {
    if v.is_fixnum() {
        return Ok(v);
    }
    if v.is_char() {
        return Ok(Value::fix(v.as_char() as i64));
    }
    if v.is_float() {
        let f = v.as_f64().trunc();
        if f.is_finite() && f.abs() < (1u64 << 62) as f64 {
            return Ok(Value::fix(f as i64));
        }
        return Err(overflow());
    }
    Err(type_err("int", "a number or character", v))
}

fn num_cmp(a: Num, b: Num) -> Ordering {
    match (a, b) {
        (Num::I(x), Num::I(y)) => x.cmp(&y),
        _ => a.as_f64().partial_cmp(&b.as_f64()).unwrap_or(Ordering::Equal),
    }
}

fn compare_chain(a: &[Value], ok: fn(Ordering) -> bool) -> Result<Value> {
    if let [x, y] = a
        && x.is_fixnum()
        && y.is_fixnum()
    {
        return Ok(Value::bool(ok(x.as_fixnum().cmp(&y.as_fixnum()))));
    }
    let mut prev = num(a[0])?;
    for &x in &a[1..] {
        let n = num(x)?;
        if !ok(num_cmp(prev, n)) {
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

/// Total order used by `compare` and `sort`.
pub fn compare(a: Value, b: Value) -> Result<Ordering> {
    let is_num = |v: Value| v.is_fixnum() || v.is_float();
    if is_num(a) && is_num(b) {
        return Ok(num_cmp(num(a)?, num(b)?));
    }
    if a.is_nil() || b.is_nil() {
        return Ok(a.is_nil().cmp(&b.is_nil()).reverse());
    }
    if a.is(Kind::String) && b.is(Kind::String) {
        return Ok(a.as_str().cmp(b.as_str()));
    }
    if a.is_char() && b.is_char() {
        return Ok(a.as_char().cmp(&b.as_char()));
    }
    if a.is_bool() && b.is_bool() {
        return Ok((a == Value::TRUE).cmp(&(b == Value::TRUE)));
    }
    if (a.is_symbol() && b.is_symbol()) || (a.is_keyword() && b.is_keyword()) {
        return Ok(a.sym_ns().cmp(&b.sym_ns()).then(a.sym_name().cmp(b.sym_name())));
    }
    if a.is(Kind::Vector) && b.is(Kind::Vector) {
        let o = a.vec_count().cmp(&b.vec_count());
        if o != Ordering::Equal {
            return Ok(o);
        }
        for i in 0..a.vec_count() {
            let o = compare(a.vec_nth(i), b.vec_nth(i))?;
            if o != Ordering::Equal {
                return Ok(o);
            }
        }
        return Ok(Ordering::Equal);
    }
    Err(Error::new(format!("Can't compare {} with {}", a.type_name(), b.type_name())))
}

// ---- collections ------------------------------------------------------------------

fn hash_map(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    if !a.len().is_multiple_of(2) {
        return Err(Error::new("hash-map requires an even number of arguments"));
    }
    let mut m = map_empty();
    for kv in a.chunks(2) {
        m = map_assoc(&mut vm.m, m, kv[0], kv[1]);
    }
    Ok(m)
}

fn index_of(k: Value) -> Option<usize> {
    (k.is_fixnum() && k.as_fixnum() >= 0).then(|| k.as_fixnum() as usize)
}

/// `(get coll key)` without a default.
pub fn get(coll: Value, key: Value) -> Option<Value> {
    if !coll.is_obj() {
        return None;
    }
    match coll.obj_kind() {
        Kind::Map => coll.map_get(key),
        Kind::Set => coll.set_contains(key).then_some(key),
        Kind::Vector => index_of(key).filter(|&i| i < coll.vec_count()).map(|i| coll.vec_nth(i)),
        Kind::String => index_of(key).and_then(|i| coll.as_str().chars().nth(i)).map(Value::char),
        _ => None,
    }
}

fn nth(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let i = int(a[1], "nth")?;
    let r = if i < 0 { None } else { seq::nth(a[0], i as usize)? };
    match (r, a.get(2)) {
        (Some(v), _) => Ok(v),
        (None, Some(&d)) => Ok(d),
        (None, None) => Err(Error::new(format!("Index out of bounds: {i}"))),
    }
}

fn assoc(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    if a.len() % 2 != 1 {
        return Err(Error::new("assoc expects a collection and key/value pairs"));
    }
    let mut c = a[0];
    if c.is_nil() {
        c = map_empty();
    }
    for kv in a[1..].chunks(2) {
        c = if c.is(Kind::Map) {
            map_assoc(&mut vm.m, c, kv[0], kv[1])
        } else if c.is(Kind::Vector) {
            match index_of(kv[0]).filter(|&i| i <= c.vec_count()) {
                Some(i) => vec_assoc(&mut vm.m, c, i, kv[1]),
                None => return Err(Error::new(format!("Index out of bounds: {}", pr_str(kv[0], true)))),
            }
        } else {
            return Err(type_err("assoc", "a map or vector", c));
        };
    }
    Ok(c)
}

fn conj(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let Some((&first, xs)) = a.split_first() else { return Ok(vec_empty()) };
    let mut c = first;
    for &x in xs {
        c = if c.is_nil() || c.is_cons() || c.is(Kind::EmptyList) {
            let tail = if c.is(Kind::EmptyList) { Value::NIL } else { c };
            loom_rt::object::cons(&mut vm.m, x, tail)
        } else if c.is(Kind::Vector) {
            vec_conj(&mut vm.m, c, x)
        } else if c.is(Kind::Set) {
            set_conj(&mut vm.m, c, x)
        } else if c.is(Kind::Map) {
            if x.is(Kind::Vector) && x.vec_count() == 2 {
                map_assoc(&mut vm.m, c, x.vec_nth(0), x.vec_nth(1))
            } else if x.is(Kind::Map) {
                let mut acc = c;
                x.map_for_each(|k, v| {
                    acc = map_assoc(&mut vm.m, acc, k, v);
                    true
                });
                acc
            } else if x.is_nil() {
                c
            } else {
                return Err(Error::new("conj on a map expects a [key value] vector or a map"));
            }
        } else if c.is(Kind::VecSeq) || c.is(Kind::Range) {
            loom_rt::object::cons(&mut vm.m, x, c)
        } else {
            return Err(type_err("conj", "a collection", c));
        };
    }
    Ok(c)
}

fn contains(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let (c, k) = (a[0], a[1]);
    Ok(Value::bool(if c.is_nil() {
        false
    } else if c.is(Kind::Map) {
        c.map_get(k).is_some()
    } else if c.is(Kind::Set) {
        c.set_contains(k)
    } else if c.is(Kind::Vector) {
        index_of(k).is_some_and(|i| i < c.vec_count())
    } else {
        return Err(type_err("contains?", "a map, set or vector", c));
    }))
}

fn map_part(vm: &mut Vm, m: Value, keys: bool) -> Result<Value> {
    if m.is_nil() {
        return Ok(Value::NIL);
    }
    expect(m, Kind::Map, if keys { "keys" } else { "vals" })?;
    let mut xs = Vec::new();
    m.map_for_each(|k, v| {
        xs.push(if keys { k } else { v });
        true
    });
    Ok(if xs.is_empty() { Value::NIL } else { list_from(&mut vm.m, &xs) })
}

fn empty_of(c: Value) -> Value {
    if c.is_cons() {
        return rt().empty_list;
    }
    if !c.is_obj() {
        return Value::NIL;
    }
    match c.obj_kind() {
        Kind::Vector => vec_empty(),
        Kind::Map => map_empty(),
        Kind::Set => set_empty(),
        Kind::EmptyList | Kind::VecSeq | Kind::Range => rt().empty_list,
        _ => Value::NIL,
    }
}

fn peek(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let c = a[0];
    if c.is(Kind::Vector) {
        return Ok(if c.vec_count() == 0 { Value::NIL } else { c.vec_nth(c.vec_count() - 1) });
    }
    if c.is_cons() {
        return Ok(c.car());
    }
    if c.is_nil() || c.is(Kind::EmptyList) {
        return Ok(Value::NIL);
    }
    Err(type_err("peek", "a vector or list", c))
}

fn pop(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let c = a[0];
    if c.is(Kind::Vector) {
        if c.vec_count() == 0 {
            return Err(Error::new("Can't pop an empty vector"));
        }
        return Ok(vec_pop(&mut vm.m, c));
    }
    if c.is_cons() {
        return Ok(seq::rest(&mut vm.m, c)?);
    }
    if c.is_nil() {
        return Ok(Value::NIL);
    }
    if c.is(Kind::EmptyList) {
        return Err(Error::new("Can't pop an empty list"));
    }
    Err(type_err("pop", "a vector or list", c))
}

fn range(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (s, e, st) = match a.len() {
        0 => return Err(Error::new("(range) with no end needs lazy sequences, which are not implemented yet")),
        1 => (0, int(a[0], "range")?, 1),
        2 => (int(a[0], "range")?, int(a[1], "range")?, 1),
        _ => (int(a[0], "range")?, int(a[1], "range")?, int(a[2], "range")?),
    };
    if st == 0 {
        return Err(Error::new("range step must not be zero (infinite ranges need lazy sequences)"));
    }
    Ok(range_new(&mut vm.m, s, e, st))
}

fn subs(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    expect(a[0], Kind::String, "subs")?;
    let s = a[0].as_str();
    let n = s.chars().count();
    let start = int(a[1], "subs")?;
    let end = match a.get(2) {
        Some(&e) => int(e, "subs")?,
        None => n as i64,
    };
    if start < 0 || end < start || end as usize > n {
        return Err(Error::new(format!("String index out of range: {start}..{end} of {n}")));
    }
    let out: String = s.chars().skip(start as usize).take((end - start) as usize).collect();
    Ok(str_new(&mut vm.m, &out))
}

fn name(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let x = a[0];
    if x.is(Kind::String) {
        Ok(x)
    } else if x.is_symbol() || x.is_keyword() {
        Ok(str_new(&mut vm.m, x.sym_name()))
    } else {
        Err(type_err("name", "a symbol, keyword or string", x))
    }
}

fn name_like(vm: &mut Vm, a: &[Value], keyword: bool) -> Result<Value> {
    let r = rt();
    if a.len() == 2 {
        let ns = if a[0].is_nil() { None } else { Some(text(a[0])) };
        let name = text(a[1]);
        return Ok(if keyword { r.keyword(&mut vm.m, ns.as_deref(), &name) } else { r.symbol(&mut vm.m, ns.as_deref(), &name) });
    }
    let x = a[0];
    if keyword && x.is_keyword() || !keyword && x.is_symbol() {
        return Ok(x);
    }
    if x.is_symbol() || x.is_keyword() {
        return Ok(if keyword { r.keyword(&mut vm.m, x.sym_ns(), x.sym_name()) } else { r.symbol(&mut vm.m, x.sym_ns(), x.sym_name()) });
    }
    let s = text(x);
    Ok(if keyword { r.keyword_from(&mut vm.m, &s) } else { r.symbol_from(&mut vm.m, &s) })
}

fn disassemble(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let mut f = a[0];
    if f.is(Kind::Var) {
        f = f.var_root();
    }
    if !f.is(Kind::Closure) {
        return Err(type_err("disassemble", "a Loom function", f));
    }
    let mut s = String::new();
    for &p in f.closure_template().template_arities().arr_slice() {
        s.push_str(&crate::bytecode::disassemble_proto(p));
    }
    out(&s, false)
}
