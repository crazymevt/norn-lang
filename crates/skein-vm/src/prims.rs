//! Primitives: Rust leaf functions bound to vars in skein.core.
//!
//! Rule (runtime.md §2.3): primitives never call Skein code. The exceptions
//! are the meta-level ones (`eval`, `load-file`, `macroexpand`), which
//! re-enter the VM through `Vm::apply` on a fresh stack: a continuation
//! barrier, acceptable for the seed system.
//!
//! Primitives are identified by *index* into `PRIMS`, not by function
//! pointer, so images can save and restore them (runtime.md §7).

use crate::vm::{Error, Result, Vm};
use skein_rt::equal::{equal, hash};
use skein_rt::func::{VAR_MACRO, primitive_new};
use skein_rt::list::{list_from, range_new};
use skein_rt::map::{map_assoc, map_dissoc, map_empty, set_conj, set_disj, set_empty};
use skein_rt::print::{pr_str, write_value};
use skein_rt::runtime::rt;
use skein_rt::seq;
use skein_rt::string::{float_new, str_new};
use skein_rt::vector::{vec_assoc, vec_conj, vec_empty, vec_from_slice, vec_pop};
use skein_rt::{Kind, Value};
use std::cmp::Ordering;
use std::io::Write;

pub type PrimFn = fn(&mut Vm, &[Value]) -> Result<Value>;

pub struct PrimDef {
    pub name: &'static str,
    pub min: usize,
    pub max: Option<usize>,
    pub f: PrimFn,
}

/// Indices of primitives the interpreter handles itself (they need its state).
pub const APPLY: usize = 0;
pub const EVAL: usize = 1;
pub const THROW: usize = 2;
pub const RESTART_FRAME: usize = 3;
pub const ABORT_FIBER: usize = 4;

const VAR: Option<usize> = None;

macro_rules! prims {
    ($($name:literal $min:literal $max:expr => $f:expr;)*) => {
        pub static PRIMS: &[PrimDef] = &[$(PrimDef { name: $name, min: $min, max: $max, f: $f }),*];
    };
}

prims! {
    "apply" 2 VAR => |_, _| unreachable!("apply is handled by the interpreter");
    "eval" 1 Some(1) => |_, _| unreachable!("eval is handled by the interpreter");
    "%throw" 2 Some(2) => |_, _| Err(Error::new("throw is only possible inside a fiber"));
    "%restart-frame" 2 Some(3) => |_, _| Err(Error::new("%restart-frame is only possible inside a fiber"));
    "%abort-fiber" 2 Some(3) => |_, a| { let mut e = Error::fatal(pr_str(a[0], false), vec![]); if let Some(&c) = a.get(2) && !c.is_nil() { e.class = Some(crate::vm::ErrClass::Value(c)); } Err(e) };
    // numbers
    "+" 0 VAR => |vm, a| arith(vm, a, Op::Add);
    "-" 1 VAR => |vm, a| arith(vm, a, Op::Sub);
    "*" 0 VAR => |vm, a| arith(vm, a, Op::Mul);
    "/" 1 VAR => |vm, a| arith(vm, a, Op::Div);
    "quot" 2 Some(2) => |vm, a| crate::numeric::int_div(&mut vm.m, a[0], a[1], IntDiv::Quot);
    "rem" 2 Some(2) => |vm, a| crate::numeric::int_div(&mut vm.m, a[0], a[1], IntDiv::Rem);
    "mod" 2 Some(2) => |vm, a| crate::numeric::int_div(&mut vm.m, a[0], a[1], IntDiv::Mod);
    "inc" 1 Some(1) => |vm, a| arith(vm, &[a[0], Value::fix(1)], Op::Add);
    "dec" 1 Some(1) => |vm, a| arith(vm, &[a[0], Value::fix(1)], Op::Sub);
    "<" 1 VAR => |_, a| compare_chain(a, |o| o == Ordering::Less);
    ">" 1 VAR => |_, a| compare_chain(a, |o| o == Ordering::Greater);
    "<=" 1 VAR => |_, a| compare_chain(a, |o| o != Ordering::Greater);
    ">=" 1 VAR => |_, a| compare_chain(a, |o| o != Ordering::Less);
    "==" 1 VAR => |_, a| compare_chain(a, |o| o == Ordering::Equal);
    "float" 1 Some(1) => |vm, a| Ok(float_new(&mut vm.m, crate::numeric::num(a[0])?.as_f64()));
    "int" 1 Some(1) => |vm, a| crate::numeric::to_int(&mut vm.m, a[0]);
    "numerator" 1 Some(1) => |vm, a| crate::numeric::numerator(&mut vm.m, a[0]);
    "denominator" 1 Some(1) => |_, a| crate::numeric::denominator(a[0]);
    // equality, identity, ordering
    "=" 1 VAR => |_, a| { forced(a)?; Ok(Value::bool(a.windows(2).all(|w| equal(w[0], w[1])))) };
    "not=" 1 VAR => |_, a| { forced(a)?; Ok(Value::bool(!a.windows(2).all(|w| equal(w[0], w[1])))) };
    "identical?" 2 Some(2) => |_, a| Ok(Value::bool(a[0] == a[1]));
    "hash" 1 Some(1) => |_, a| { forced(a)?; Ok(Value::fix((hash(a[0]) >> 2) as i64)) };
    "compare" 2 Some(2) => |_, a| Ok(Value::fix(match compare(a[0], a[1])? { Ordering::Less => -1, Ordering::Equal => 0, Ordering::Greater => 1 }));
    "%sort-keyed" 2 Some(2) => sort_keyed;
    // type predicates
    "nil?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_nil()));
    "number?" 1 Some(1) => |_, a| Ok(Value::bool(skein_rt::numbers::is_number(a[0])));
    "integer?" 1 Some(1) => |_, a| Ok(Value::bool(skein_rt::numbers::is_integer(a[0])));
    "ratio?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Ratio)));
    "rational?" 1 Some(1) => |_, a| Ok(Value::bool(skein_rt::numbers::is_integer(a[0]) || a[0].is(Kind::Ratio)));
    "float?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_float()));
    "string?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::String)));
    "symbol?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_symbol()));
    "keyword?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_keyword()));
    "char?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_char()));
    "boolean?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_bool()));
    "fn?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Closure) || a[0].is(Kind::Primitive)));
    "%fn-vars" 1 Some(1) => |vm, a| Ok(fn_vars(vm, a[0]));
    "%fn-captured" 1 Some(1) => |vm, a| Ok(fn_captured(vm, a[0]));
    "%fn-symbols" 1 Some(1) => |vm, a| Ok(fn_symbols(vm, a[0]));
    "%prof-start!" 1 Some(1) => |_, a| crate::prof::start(int(a[0], "%prof-start!")?);
    "%prof-stop!" 0 Some(0) => |vm, _| Ok(crate::prof::stop(&mut vm.m));
    "%prof-active?" 0 Some(0) => |_, _| Ok(Value::bool(crate::prof::active()));
    "%cover-on!" 0 Some(0) => |_, _| { crate::cover::turn_on(); Ok(Value::NIL) };
    "%cover-on?" 0 Some(0) => |_, _| Ok(Value::bool(crate::cover::on()));
    "%cover-line!" 2 Some(2) => |_, a| { expect(a[0], Kind::String, "%cover-line!")?; Ok(crate::cover::line_id(a[0].as_str(), int(a[1], "%cover-line!")?.max(0) as u32)) };
    "%cover!" 1 Some(1) => |_, a| { crate::cover::hit(a[0].as_fixnum()); Ok(Value::NIL) };
    "%cover-counts" 0 Some(0) => |vm, _| Ok(crate::cover::counts(&mut vm.m));
    "list?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_cons() || a[0].is(Kind::EmptyList)));
    "vector?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Vector)));
    "map?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Map) || (a[0].is(Kind::Sorted) && !a[0].sorted_is_set())));
    "set?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Set) || (a[0].is(Kind::Sorted) && a[0].sorted_is_set())));
    "sorted?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Sorted)));
    "queue?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Queue)));
    "transient?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Transient)));
    "seq?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_cons() || (a[0].is_obj() && matches!(a[0].obj_kind(), Kind::EmptyList | Kind::VecSeq | Kind::Range))));
    "sequential?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_cons() || (a[0].is_obj() && matches!(a[0].obj_kind(), Kind::EmptyList | Kind::VecSeq | Kind::Range | Kind::Vector | Kind::Stream | Kind::Queue))));
    "coll?" 1 Some(1) => |_, a| Ok(Value::bool(seq::is_seqable(a[0]) && !a[0].is_nil() && !a[0].is(Kind::String)));
    "seqable?" 1 Some(1) => |_, a| Ok(Value::bool(seq::is_seqable(a[0])));
    "var?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Var)));
    "%indexed?" 1 Some(1) => |_, a| Ok(Value::bool(seq::is_indexed(a[0])));
    "type" 1 Some(1) => |vm, a| { let n = a[0].type_name().to_lowercase(); Ok(rt().keyword(&mut vm.m, None, &n)) };
    // collections
    "list" 0 VAR => |vm, a| Ok(list_from(&mut vm.m, a));
    "vector" 0 VAR => |vm, a| Ok(vec_from_slice(&mut vm.m, a));
    "hash-map" 0 VAR => |vm, a| hash_map(vm, a);
    // (a rest-argument seq of keyword arguments as a map: what
    // (apply hash-map x) gave, without apply, which stopped tier-1 code
    // each call; M10 part 6.5)
    "%as-map" 1 Some(1) => |vm, a| { let x = a[0]; let is_seq = x.is_cons() || (x.is_obj() && matches!(x.obj_kind(), Kind::EmptyList | Kind::VecSeq | Kind::Range)); if !is_seq { return Ok(x); } let items = to_vec(vm, x)?; hash_map(vm, &items) };
    "hash-set" 0 VAR => |vm, a| Ok(a.iter().fold(set_empty(), |s, &x| set_conj(&mut vm.m, s, x)));
    "vec" 1 Some(1) => |vm, a| { if a[0].is(Kind::Vector) { return Ok(a[0]); }
        if a[0].is(Kind::Instance) { return collection_op(vm, "vec", a); }
        let xs = to_vec(vm, a[0])?; Ok(vec_from_slice(&mut vm.m, &xs)) };
    "set" 1 Some(1) => |vm, a| { if a[0].is(Kind::Instance) { return collection_op(vm, "set", a); } let xs = to_vec(vm, a[0])?; Ok(xs.into_iter().fold(set_empty(), |s, x| set_conj(&mut vm.m, s, x))) };
    "cons" 2 Some(2) => |vm, a| Ok(seq::cons_onto(&mut vm.m, a[0], a[1])?);
    "first" 1 Some(1) => |vm, a| { if a[0].is(Kind::Instance) { return collection_op(vm, "first", a); } Ok(seq::first(&mut vm.m, a[0])?) };
    "rest" 1 Some(1) => |vm, a| { if a[0].is(Kind::Instance) { return collection_op(vm, "rest", a); } Ok(seq::rest(&mut vm.m, a[0])?) };
    "next" 1 Some(1) => |vm, a| { if a[0].is(Kind::Instance) { return collection_op(vm, "next", a); } Ok(seq::next(&mut vm.m, a[0])?) };
    "seq" 1 Some(1) => |vm, a| { if a[0].is(Kind::Instance) { return collection_op(vm, "seq", a); } Ok(seq::seq(&mut vm.m, a[0])?) };
    "count" 1 Some(1) => |vm, a| { if a[0].is(Kind::Instance) { return collection_op(vm, "count", a); } Ok(Value::fix(seq::count(a[0])? as i64)) };
    "nth" 2 Some(3) => nth;
    "get" 2 Some(3) => get_prim;
    // (a map pattern's key, without making a keyword-argument seq a map
    // first: M10 part 6.5)
    "%kw-get" 2 Some(3) => kw_get_prim;
    // sorted maps and sets, queues, transients (colls.rs, sorted.rs)
    "sorted-map" 0 VAR => |vm, a| { if a.len() % 2 != 0 { return Err(Error::of("TypeError", "sorted-map expects key/value pairs")); } let mut s = skein_rt::colls::sorted_new(&mut vm.m, 0, Value::NIL, false); for kv in a.chunks(2) { s = crate::sorted::assoc(&mut vm.m, s, kv[0], kv[1])?; } Ok(s) };
    "sorted-set" 0 VAR => |vm, a| { let mut s = skein_rt::colls::sorted_new(&mut vm.m, 0, Value::NIL, true); for &k in a { s = crate::sorted::assoc(&mut vm.m, s, k, k)?; } Ok(s) };
    "subseq" 3 Some(5) => |vm, a| sorted_range(vm, a, false);
    "rsubseq" 3 Some(5) => |vm, a| sorted_range(vm, a, true);
    "%empty-queue" 0 Some(0) => |vm, _| { let e = rt().empty_vector; Ok(skein_rt::colls::queue_new(&mut vm.m, 0, Value::NIL, e)) };
    "transient" 1 Some(1) => |vm, a| transient(vm, a[0]);
    "persistent!" 1 Some(1) => |vm, a| persistent(vm, a[0]);
    "conj!" 2 Some(2) => |vm, a| transient_op(vm, a[0], TOp::Conj(a[1]));
    "assoc!" 3 Some(3) => |vm, a| transient_op(vm, a[0], TOp::Assoc(a[1], a[2]));
    "dissoc!" 2 Some(2) => |vm, a| transient_op(vm, a[0], TOp::Dissoc(a[1]));
    "disj!" 2 Some(2) => |vm, a| transient_op(vm, a[0], TOp::Disj(a[1]));
    "pop!" 1 Some(1) => |vm, a| transient_op(vm, a[0], TOp::Pop);
    "assoc" 3 VAR => assoc;
    "dissoc" 1 VAR => |vm, a| { let mut m = a[0]; if m.is_nil() { return Ok(m); } else if m.is(Kind::Sorted) { for &k in &a[1..] { m = crate::sorted::dissoc(&mut vm.m, m, k)?; } return Ok(m); } expect(m, Kind::Map, "dissoc")?; for &k in &a[1..] { m = map_dissoc(&mut vm.m, m, k); } Ok(m) };
    "conj" 0 VAR => conj;
    "disj" 1 VAR => |vm, a| { let mut s = a[0]; if s.is_nil() { return Ok(s); } else if s.is(Kind::Sorted) { for &k in &a[1..] { s = crate::sorted::dissoc(&mut vm.m, s, k)?; } return Ok(s); } expect(s, Kind::Set, "disj")?; for &k in &a[1..] { s = set_disj(&mut vm.m, s, k); } Ok(s) };
    "contains?" 2 Some(2) => contains;
    "keys" 1 Some(1) => |vm, a| map_part(vm, a[0], true);
    "vals" 1 Some(1) => |vm, a| map_part(vm, a[0], false);
    "empty" 1 Some(1) => |vm, a| Ok(match a[0] { c if c.is(Kind::Sorted) => skein_rt::colls::sorted_new(&mut vm.m, 0, Value::NIL, c.sorted_is_set()), c if c.is(Kind::Queue) => { let e = rt().empty_vector; skein_rt::colls::queue_new(&mut vm.m, 0, Value::NIL, e) } c => empty_of(c) });
    "peek" 1 Some(1) => peek;
    "%subvec" 3 Some(3) => subvec;
    "pop" 1 Some(1) => pop;
    "concat" 0 VAR => |vm, a| { let mut out = Vec::new(); for &x in a { out.extend(to_vec(vm, x)?); } Ok(if out.is_empty() { rt().empty_list } else { list_from(&mut vm.m, &out) }) };
    "range" 0 Some(3) => range;
    // strings, symbols, keywords
    "str" 0 VAR => |vm, a| { let o = print_overrides(vm, a, false)?; let s = skein_rt::print::with_overrides(o, || { let mut s = String::new(); for &x in a { if !x.is_nil() { write_value(&mut s, x, false); } } s }); Ok(str_new(&mut vm.m, &s)) };
    "subs" 2 Some(3) => subs;
    "name" 1 Some(1) => name;
    "namespace" 1 Some(1) => |vm, a| { let x = a[0]; if !(x.is_symbol() || x.is_keyword()) { return Err(type_err("namespace", "a symbol or keyword", x)); } Ok(match x.sym_ns() { Some(ns) => str_new(&mut vm.m, ns), None => Value::NIL }) };
    "symbol" 1 Some(2) => |vm, a| name_like(vm, a, false);
    "keyword" 1 Some(2) => |vm, a| name_like(vm, a, true);
    "gensym" 0 Some(1) => |vm, a| { let p = if a.is_empty() { "G__".to_owned() } else { text(a[0]) }; Ok(rt().symbol(&mut vm.m, None, &format!("{p}{}", crate::reader::next_id()))) };
    "char" 1 Some(1) => |_, a| { let n = int(a[0], "char")?; char::from_u32(n as u32).map(Value::char).ok_or_else(|| Error::new(format!("Invalid code point: {n}"))) };
    // printing and I/O
    "pr-str" 0 VAR => |vm, a| { forced(a)?; let s = printed(vm, a, true)?; Ok(str_new(&mut vm.m, &s)) };
    "print" 0 VAR => |vm, a| { forced(a)?; let s = printed(vm, a, false)?; out_to(vm, &s, false) };
    "println" 0 VAR => |vm, a| { forced(a)?; let s = printed(vm, a, false)?; out_to(vm, &s, true) };
    "pr" 0 VAR => |vm, a| { forced(a)?; let s = printed(vm, a, true)?; out_to(vm, &s, false) };
    "prn" 0 VAR => |vm, a| { forced(a)?; let s = printed(vm, a, true)?; out_to(vm, &s, true) };
    // streams (design §3.3) and reduced (early exit from reduce)
    "%stream" 1 Some(1) => |vm, a| Ok(seq::stream_new(&mut vm.m, a[0]));
    "%stream-thunk" 1 Some(1) => |_, a| { expect(a[0], Kind::Stream, "%stream-thunk")?; Ok(if a[0].stream_realized() { Value::NIL } else { a[0].stream_value() }) };
    "%stream-set!" 2 Some(2) => |_, a| { expect(a[0], Kind::Stream, "%stream-set!")?; Ok(seq::stream_set(a[0], a[1])) };
    "%first-unrealized" 1 Some(1) => |_, a| Ok(seq::first_unrealized(a[0]).unwrap_or(Value::NIL));
    "stream?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Stream)));
    "realized?" 1 Some(1) => |_, a| Ok(Value::bool(!a[0].is(Kind::Stream) || a[0].stream_realized()));
    "reduced" 1 Some(1) => |vm, a| { let r = skein_rt::object::alloc(&mut vm.m, Kind::Reduced, 1); r.set_field(0, a[0]); Ok(r) };
    "reduced?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Reduced)));
    "unreduced" 1 Some(1) => |_, a| Ok(if a[0].is(Kind::Reduced) { a[0].field(0) } else { a[0] });
    "slurp" 1 Some(1) => |vm, a| crate::sys::read(vm, text(a[0]), true);
    // (Bytes are written as they are; anything else as str gives it)
    "spit" 2 Some(2) => |vm, a| { let data = if a[1].is(Kind::Bytes) { a[1].as_bytes().to_vec() } else { let o = print_overrides(vm, &a[1..2], false)?; skein_rt::print::with_overrides(o, || text(a[1])).into_bytes() }; crate::sys::write(vm, text(a[0]), data, false) };
    "read-string" 1 Some(1) => |vm, a| { let s = text(a[0]); let mut r = crate::reader::Reader::new(&s); match r.read(vm) { Ok(v) => Ok(v), Err(crate::reader::ReadError::Eof) => Ok(Value::NIL), Err(e) => Err(crate::eval::parse_error(e.to_string(), "<string>", r.line(), r.column())) } };
    // evaluation and namespaces
    "macroexpand-1" 1 Some(1) => |vm, a| Ok(vm.macroexpand_1(a[0])?.0);
    "macroexpand" 1 Some(1) => |vm, a| vm.macroexpand(a[0]);
    "%reader" 2 Some(2) => |vm, a| { expect(a[0], Kind::String, "%reader")?; Ok(skein_rt::conc::reader_new(&mut vm.m, a[0], a[1])) };
    "%read" 1 Some(1) => |vm, a| { expect(a[0], Kind::Reader, "%read")?; crate::eval::read_next(vm, a[0]) };
    "%read-syntax" 1 Some(1) => |vm, a| { expect(a[0], Kind::Reader, "%read-syntax")?; crate::eval::read_syntax_next(vm, a[0]) };
    "%eof?" 1 Some(1) => |_, a| Ok(Value::bool(a[0] == Value::EOF));
    "in-ns" 1 Some(1) => |vm, a| { if !a[0].is_symbol() { return Err(type_err("in-ns", "a symbol", a[0])); } vm.ns = rt().find_or_create_ns(a[0]); Ok(a[0]) };
    "current-ns" 0 Some(0) => |vm, _| Ok(vm.ns.name);
    "all-ns" 0 Some(0) => |vm, _| { let names: Vec<Value> = rt().all_namespaces().iter().map(|n| n.name).collect(); Ok(list_from(&mut vm.m, &names)) };
    "ns-publics" 1 Some(1) => |vm, a| { let ns = rt().find_ns(a[0]).ok_or_else(|| Error::new(format!("No namespace: {}", pr_str(a[0], true))))?; let mut m = map_empty(); for (k, v) in ns.mappings() { if v.var_ns() == ns.name && !v.var_is_private() { m = map_assoc(&mut vm.m, m, k, v); } } Ok(m) };
    "%ns-unmap" 2 Some(2) => |_, a| { expect(a[1], Kind::Symbol, "%ns-unmap")?; let ns = rt().find_ns(a[0]).ok_or_else(|| Error::new(format!("No namespace: {}", pr_str(a[0], true))))?; Ok(Value::bool(ns.unmap_own(a[1]))) };
    "ns-interns" 1 Some(1) => |vm, a| { let ns = rt().find_ns(a[0]).ok_or_else(|| Error::new(format!("No namespace: {}", pr_str(a[0], true))))?; let mut m = map_empty(); for (k, v) in ns.mappings() { if v.var_ns() == ns.name { m = map_assoc(&mut vm.m, m, k, v); } } Ok(m) };
    "resolve" 1 Some(1) => |vm, a| {
        if !a[0].is_symbol() { return Err(Error::of("TypeError", format!("resolve expects a symbol, got {}", pr_str(a[0], true)))); }
        Ok(vm.resolve(a[0]).unwrap_or(Value::NIL)) };
    "%resolve-in" 2 Some(2) => |vm, a| { expect(a[0], Kind::Symbol, "%resolve-in")?; expect(a[1], Kind::Symbol, "%resolve-in")?; let core = vm.core; Ok(rt().find_ns(a[0]).and_then(|ns| rt().resolve(&mut vm.m, ns, core, a[1])).unwrap_or(Value::NIL)) };
    "%identity-hash" 1 Some(1) => |_, a| Ok(Value::fix(((a[0].0 >> 3) & 0x3fff_ffff_ffff_ffff) as i64));
    "%uninterned-symbol" 1 Some(1) => |vm, a| { let n = text(a[0]); Ok(rt().symbol_uninterned(&mut vm.m, &n)) };
    "var-get" 1 Some(1) => |_, a| { expect(a[0], Kind::Var, "var-get")?; Ok(a[0].var_root()) };
    "%set-root!" 2 Some(2) => |_, a| { expect(a[0], Kind::Var, "%set-root!")?; a[0].var_set_root(a[1]); Ok(a[1]) };
    "%set-macro!" 1 Some(1) => |_, a| { expect(a[0], Kind::Var, "%set-macro!")?; a[0].var_set_flag(VAR_MACRO); Ok(a[0]) };
    "macro?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Var) && a[0].var_is_macro()));
    "%set-private!" 1 Some(1) => |_, a| { expect(a[0], Kind::Var, "%set-private!")?; a[0].var_set_flag(skein_rt::func::VAR_PRIVATE); Ok(a[0]) };
    "private?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Var) && a[0].var_is_private()));
    "%var-bound?" 1 Some(1) => |_, a| { expect(a[0], Kind::Var, "bound?")?; Ok(Value::bool(a[0].var_root() != Value::UNBOUND)) };
    "%set-dynamic!" 1 Some(1) => |_, a| { expect(a[0], Kind::Var, "%set-dynamic!")?; a[0].var_set_flag(skein_rt::func::VAR_DYNAMIC); Ok(a[0]) };
    "%var-dynamic?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Var) && a[0].var_flags() & skein_rt::func::VAR_DYNAMIC != 0));
    "%set-contracted!" 2 Some(2) => |_, a| { expect(a[0], Kind::Var, "%set-contracted!")?; if a[1].truthy() { a[0].var_set_flag(skein_rt::func::VAR_CONTRACTED); } else { a[0].var_clear_flag(skein_rt::func::VAR_CONTRACTED); } Ok(a[0]) };
    "%set-unsafe!" 1 Some(1) => |_, a| { expect(a[0], Kind::Var, "%set-unsafe!")?; a[0].var_set_flag(skein_rt::func::VAR_UNSAFE); Ok(a[0]) };
    "%unsafe?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Var) && a[0].var_flags() & skein_rt::func::VAR_UNSAFE != 0));
    "%contracted?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Var) && a[0].var_flags() & skein_rt::func::VAR_CONTRACTED != 0));
    "error" 1 VAR => |_, a| Err(Error::new(join(a, false)));
    "disassemble" 1 Some(1) => disassemble;
    // system
    "nano-time" 0 Some(0) => |_, _| { use std::sync::OnceLock; use std::time::Instant; static T0: OnceLock<Instant> = OnceLock::new(); Ok(Value::fix(T0.get_or_init(Instant::now).elapsed().as_nanos() as i64)) };
    "heap-bytes" 0 Some(0) => |_, _| Ok(Value::fix(skein_rt::heap::heap_bytes() as i64));
    "gc" 0 Some(0) => |vm, _| { vm.want_gc = true; skein_rt::safepoint::bump_attention(); Ok(Value::NIL) };
    "gc-count" 0 Some(0) => |_, _| Ok(Value::fix(skein_rt::heap::gc_count() as i64));
    // fibers, scopes, channels, atoms
    "%spawn" 1 Some(1) => |vm, a| Ok(crate::sched::spawn(vm, a[0], Value::NIL));
    "%scope" 0 Some(0) => |vm, _| Ok(skein_rt::conc::scope_new(&mut vm.m));
    "%scope-spawn" 2 Some(2) => |vm, a| { expect(a[0], Kind::Scope, "spawn")?; Ok(crate::sched::spawn(vm, a[1], a[0])) };
    "%scope-spawn-body" 2 Some(2) => |vm, a| { expect(a[0], Kind::Scope, "%scope-spawn-body")?; Ok(crate::sched::spawn_body(vm, a[1], a[0])) };
    "%scope-join" 1 Some(1) => |vm, a| { expect(a[0], Kind::Scope, "%scope-join")?; crate::sched::scope_join(vm, a[0]) };
    "%scope-result" 2 Some(2) => scope_result;
    "await" 1 Some(1) => |vm, a| crate::conc::await_fiber(vm, a[0]);
    "%join" 1 Some(1) => |vm, a| crate::conc::wait_fiber(vm, a[0], false);
    "fiber-error" 1 Some(1) => |_, a| { expect(a[0], Kind::Fiber, "fiber-error")?; Ok(a[0].field(skein_rt::conc::fiber::ERROR)) };
    "yield" 0 Some(0) => |vm, _| { if vm.can_park() { vm.suspend = Some(crate::vm::Suspend::Yield); } Ok(Value::NIL) };
    "sleep" 1 Some(1) => |vm, a| { let ms = int(a[0], "sleep")?; crate::sched::sleep(vm, ms.max(0) as u64) };
    "current-fiber" 0 Some(0) => |vm, _| Ok(vm.current);
    "fiber?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Fiber)));
    "done?" 1 Some(1) => |_, a| { expect(a[0], Kind::Fiber, "done?")?; Ok(Value::bool(a[0].fiber_state().load(std::sync::atomic::Ordering::Acquire) == skein_rt::conc::fiber::DONE)) };
    "cancel!" 1 Some(1) => |_, a| { expect(a[0], Kind::Fiber, "cancel!")?; crate::sched::cancel(a[0], skein_rt::conc::fiber::CANCELLED); Ok(Value::NIL) };
    "%interrupt!" 1 Some(1) => |_, a| { expect(a[0], Kind::Fiber, "%interrupt!")?; crate::sched::cancel(a[0], skein_rt::conc::fiber::INTERRUPTED); Ok(Value::NIL) };
    "chan" 0 Some(2) => |vm, a| {
        let n = match a.first() {
            Some(&v) => match int(v, "chan")? {
                n if n < 0 => return Err(type_err("chan", "a buffer size of 0 or more", v)),
                n => n as usize,
            },
            None => 0,
        };
        let c = skein_rt::conc::chan_new(&mut vm.m, n);
        if let Some(&p) = a.get(1) {
            let policy = if p.is_keyword() { p.sym_name() } else { "" };
            let bits = match policy { "dropping" => skein_rt::conc::chan::DROPPING, "sliding" => skein_rt::conc::chan::SLIDING, "fixed" => 0, _ => return Err(Error::new(format!("chan: the buffer policy must be :fixed, :dropping or :sliding, got {}", pr_str(p, true)))) };
            if bits != 0 && n == 0 { return Err(Error::new("chan: a :dropping or :sliding channel needs a buffer size above 0")); }
            c.set_word(skein_rt::conc::chan::CAP, n as u64 | bits);
        }
        Ok(c)
    };
    "put!" 2 Some(2) => |vm, a| crate::conc::put(vm, a[0], a[1]);
    "take!" 1 Some(1) => |vm, a| crate::conc::take(vm, a[0]);
    "offer!" 2 Some(2) => |vm, a| crate::conc::offer(vm, a[0], a[1]);
    "poll!" 1 Some(1) => |vm, a| crate::conc::poll(vm, a[0]);
    "close!" 1 Some(1) => |_, a| { if !a[0].is(Kind::Chan) { return Err(type_err("close!", "a channel", a[0])); } crate::conc::close(a[0]); Ok(Value::NIL) };
    "closed?" 1 Some(1) => |_, a| crate::conc::closed(a[0]);
    "alts!" 1 VAR => crate::conc::alts;
    "timeout" 1 Some(1) => |vm, a| { let ms = int(a[0], "timeout")?; Ok(crate::sched::timeout(vm, ms.max(0) as u64)) };
    "atom" 1 Some(1) => |vm, a| Ok(skein_rt::conc::atom_new(&mut vm.m, a[0]));
    "deref" 1 Some(3) => deref;
    "reset!" 2 Some(2) => |_, a| { expect(a[0], Kind::Atom, "reset!")?; a[0].atom_set(a[1]); Ok(a[1]) };
    "compare-and-set!" 3 Some(3) => |_, a| { expect(a[0], Kind::Atom, "compare-and-set!")?; Ok(Value::bool(a[0].atom_cas(a[1], a[2]))) };
    "carrier-count" 0 Some(0) => |_, _| Ok(Value::fix(crate::sched::sched().n as i64));
    "fiber-stats" 0 Some(0) => fiber_stats;
    // conditions, dynamic environment, debugger support
    "%dyn" 0 Some(0) => |vm, _| Ok(vm.dyn_env);
    "%bind-map" 0 VAR => bind_map;
    "%set-dyn!" 1 Some(1) => |vm, a| { vm.dyn_env = a[0]; Ok(a[0]) };
    "%mark-dynamic!" 1 Some(1) => |_, a| { expect(a[0], Kind::Var, "%mark-dynamic!")?; a[0].var_set_flag(skein_rt::func::VAR_DYNBOUND); Ok(a[0]) };
    "%register-frame-kinds!" 5 Some(5) => register_frame_kinds;
    "%retry-marker" 0 Some(0) => |_, _| Ok(Value::RETRY);
    "%nested-raise" 5 Some(5) => |vm, a| Err(nested_raise(vm, a));
    // (how: [trace condition]; the condition, if any, is kept by the fiber)
    "%die" 2 Some(2) => |vm, a| { let (trace, cond) = if a[1].is(Kind::Vector) && a[1].vec_count() == 2 { (a[1].vec_nth(0), a[1].vec_nth(1)) } else { (a[1], Value::NIL) }; let lines = to_vec(vm, trace).unwrap_or_default().iter().map(|&t| pr_str(t, false)).collect(); let mut e = Error::fatal(pr_str(a[0], false), lines); if !cond.is_nil() { e.class = Some(crate::vm::ErrClass::Value(cond)); } Err(e) };
    "%message" 0 VAR => |vm, a| Ok(str_new(&mut vm.m, &join(a, false)));
    "%frames" 1 Some(1) => frames;
    "%post-event" 1 Some(1) => |_, a| { crate::sched::post_event(text(a[0])); Ok(Value::NIL) };
    "fiber-id" 1 Some(1) => |_, a| { expect(a[0], Kind::Fiber, "fiber-id")?; Ok(Value::fix(a[0].fiber_id() as i64)) };
    "%location" 0 Some(0) => |vm, _| Ok(if vm.current.is_nil() { Value::NIL } else { vm.current.field(skein_rt::conc::fiber::LOC) });
    "%set-location!" 1 Some(1) => |vm, a| { if !vm.current.is_nil() { vm.current.set_field(skein_rt::conc::fiber::LOC, a[0]); } Ok(a[0]) };
    "%reader-line" 1 Some(1) => |_, a| { expect(a[0], Kind::Reader, "%reader-line")?; Ok(Value::fix(a[0].word(3) as i64)) };
    "%reader-name" 1 Some(1) => |_, a| { expect(a[0], Kind::Reader, "%reader-name")?; Ok(a[0].field(2)) };
    "add-lib-path!" 1 Some(1) => |_, a| { crate::eval::add_lib_path(text(a[0])); Ok(Value::NIL) };
    "%build-id" 0 Some(0) => |vm, _| Ok(str_new(&mut vm.m, &format!("{:016x}", crate::image::build_id())));
    "%hash-text" 1 Some(1) => |vm, a| { let t = text(a[0]); let mut h: u64 = 0xcbf2_9ce4_8422_2325; for &b in t.as_bytes() { h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3); } Ok(str_new(&mut vm.m, &format!("{h:016x}"))) };
    "%cache-dir" 0 Some(0) => |vm, _| Ok(match crate::eval::cache_dir() { Some(d) => str_new(&mut vm.m, &d), None => Value::NIL });
    "%reader-text" 1 Some(1) => |_, a| { expect(a[0], Kind::Reader, "%reader-text")?; Ok(a[0].field(0)) };
    "%next-form-text" 1 Some(1) => |vm, a| { expect(a[0], Kind::Reader, "%next-form-text")?; crate::eval::next_form_text(vm, a[0]) };
    // building and inspecting compiled code (the Skein compiler, lib/compiler)
    "%make-proto" 7 Some(7) => make_proto;
    "%make-template" 3 Some(3) => |vm, a| { let ps = to_vec(vm, a[1])?; let cs = to_vec(vm, a[2])?; let arities = skein_rt::array::array_from(&mut vm.m, &ps); let caps = skein_rt::array::array_from(&mut vm.m, &cs); let t = skein_rt::func::template_new(&mut vm.m, a[0], arities, caps); crate::verify::template(t, false).map_err(|e| Error::new(format!("%make-template: bad code: {e}")))?; Ok(t) };
    "%make-closure" 1 Some(1) => |vm, a| { expect(a[0], Kind::FnTemplate, "%make-closure")?; Ok(skein_rt::func::closure_new(&mut vm.m, a[0])) };
    "%ensure-ns" 1 Some(1) => |_, a| { expect(a[0], Kind::Symbol, "%ensure-ns")?; rt().find_or_create_ns(a[0]); Ok(a[0]) };
    "%intern" 2 Some(2) => |vm, a| { let ns = rt().find_or_create_ns(a[0]); Ok(ns.intern(&mut vm.m, a[1])) };
    "%closure?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Closure)));
    "%closure-template" 1 Some(1) => |_, a| { expect(a[0], Kind::Closure, "%closure-template")?; Ok(a[0].closure_template()) };
    "%template?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::FnTemplate)));
    "%template-parts" 1 Some(1) => template_parts;
    "%proto-parts" 1 Some(1) => proto_parts;
    "%seed-compile" 1 Some(1) => |vm, a| { vm.nested += 1; let r = crate::compiler::compile_toplevel(vm, a[0]); vm.nested -= 1; r };
    "%compiler-source" 0 Some(0) => |vm, _| Ok(str_new(&mut vm.m, crate::eval::COMPILER_SRC));
    "%expander-source" 0 Some(0) => |vm, _| Ok(str_new(&mut vm.m, crate::eval::EXPANDER_SRC));
    // bytes (immutable byte strings)
    "bytes?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Bytes)));
    "bytes" 1 Some(1) => |vm, a| to_bytes(vm, a[0]);
    "bytes->string" 1 Some(2) => |vm, a| { expect(a[0], Kind::Bytes, "bytes->string")?; let b = a[0].as_bytes(); let s = if a.get(1).is_some_and(|x| x.truthy()) { String::from_utf8_lossy(b).into_owned() } else { std::str::from_utf8(b).map_err(|e| Error::new(format!("Invalid UTF-8: {e}")))?.to_owned() }; Ok(str_new(&mut vm.m, &s)) };
    "subbytes" 2 Some(3) => |vm, a| { expect(a[0], Kind::Bytes, "subbytes")?; let b = a[0].as_bytes(); let s = int(a[1], "subbytes")?; let e = match a.get(2) { Some(&x) => int(x, "subbytes")?, None => b.len() as i64 }; if s < 0 || e < s || e as usize > b.len() { return Err(Error::new(format!("subbytes: range {s}..{e} out of bounds for {} bytes", b.len()))); } Ok(skein_rt::string::bytes_new(&mut vm.m, &b[s as usize..e as usize])) };
    "bytes-concat" 0 VAR => |vm, a| { let mut out = Vec::new(); for &x in a { expect(x, Kind::Bytes, "bytes-concat")?; out.extend_from_slice(x.as_bytes()); } Ok(skein_rt::string::bytes_new(&mut vm.m, &out)) };
    "%float-bytes" 2 Some(2) => float_bytes;
    "%byte-array" 1 Some(1) => |vm, a| { let n = int(a[0], "%byte-array")?; if !(0..=(1i64 << 40)).contains(&n) { return Err(Error::new(format!("A mutable buffer can't hold {n} bytes"))); } Ok(skein_rt::string::byte_array_new(&mut vm.m, n as usize)) };
    "%byte-array-from" 1 Some(1) => |vm, a| { if !a[0].is(Kind::Bytes) && !a[0].is(Kind::ByteArray) { return Err(type_err("%byte-array-from", "Bytes", a[0])); } let src = a[0].as_bytes().to_vec(); let v = skein_rt::string::byte_array_new(&mut vm.m, src.len()); unsafe { v.byte_array_mut().copy_from_slice(&src) }; Ok(v) };
    "%mbuf-load" 4 Some(4) => mbuf_load;
    "%mbuf-store!" 5 Some(5) => mbuf_store;
    "%mbuf-fill!" 4 Some(4) => mbuf_fill;
    "%mbuf-blit!" 3 Some(3) => mbuf_blit;
    "%mbuf-freeze!" 1 Some(1) => mbuf_freeze;
    "%mbuf-snapshot" 1 Some(1) => |vm, a| { let d = mbuf_storage(vm, a[0], false)?; let copy = d.as_bytes().to_vec(); Ok(skein_rt::string::bytes_new(&mut vm.m, &copy)) };
    "%bytes-float" 1 Some(1) => bytes_float;
    "%utf8-valid-up-to" 1 Some(1) => |_vm, a| { expect(a[0], Kind::Bytes, "%utf8-valid-up-to")?; Ok(match std::str::from_utf8(a[0].as_bytes()) { Ok(_) => Value::NIL, Err(e) => Value::fix(e.valid_up_to() as i64) }) };
    "%hex-encode" 1 Some(1) => |vm, a| { expect(a[0], Kind::Bytes, "hex")?; Ok(str_new(&mut vm.m, &crate::codec::hex_encode(a[0].as_bytes()))) };
    "%hex-decode" 1 Some(1) => |vm, a| { expect(a[0], Kind::String, "unhex")?; let b = crate::codec::hex_decode(a[0].as_str()).map_err(Error::new)?; Ok(skein_rt::string::bytes_new(&mut vm.m, &b)) };
    "%base64-encode" 2 Some(2) => |vm, a| { expect(a[0], Kind::Bytes, "base64")?; Ok(str_new(&mut vm.m, &crate::codec::base64_encode(a[0].as_bytes(), a[1].truthy()))) };
    "%base64-decode" 1 Some(1) => |vm, a| { expect(a[0], Kind::String, "unbase64")?; let b = crate::codec::base64_decode(a[0].as_str()).map_err(Error::new)?; Ok(skein_rt::string::bytes_new(&mut vm.m, &b)) };
    // I/O handles (the reactor and the blocking pool; runtime.md §5.5)
    "%pipe" 0 Some(0) => |vm, _| { let (tx, rx) = mio::unix::pipe::new().map_err(|e| Error::new(e.to_string()))?; let r = crate::reactor::register(&mut vm.m, crate::reactor::Io::PipeIn(rx), "pipe-in", Value::NIL).map_err(Error::new)?; let w = crate::reactor::register(&mut vm.m, crate::reactor::Io::PipeOut(tx), "pipe-out", Value::NIL).map_err(Error::new)?; Ok(vec_from_slice(&mut vm.m, &[r, w])) };
    "%io-seq" 2 Some(2) => |_, a| Ok(Value::fix(crate::reactor::seq(a[0], io_dir(a[1])?)? as i64));
    "%io-wait" 3 Some(3) => |vm, a| crate::reactor::wait(vm, a[0], io_dir(a[1])?, int(a[2], "%io-wait")? as u64);
    "%io-read" 2 Some(2) => |vm, a| io_read(vm, a[0], int(a[1], "read")?);
    "%io-write" 3 Some(3) => |vm, a| io_write(vm, a[0], a[1], int(a[2], "write")?);
    "%io-close" 1 Some(1) => |_, a| crate::reactor::close(a[0]);
    "%io-close-id" 1 Some(1) => |_, a| { crate::reactor::close_id(int(a[0], "%io-close-id")? as u64); Ok(Value::NIL) };
    "handle?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Handle)));
    "%handle-id" 1 Some(1) => |_, a| { expect(a[0], Kind::Handle, "%handle-id")?; Ok(Value::fix(a[0].word(skein_rt::conc::handle::ID) as i64)) };
    "%handle-live?" 1 Some(1) => |_, a| Ok(Value::bool(crate::reactor::entry(a[0]).is_ok()));
    "%handle-current?" 1 Some(1) => |_, a| { expect(a[0], Kind::Handle, "%handle-current?")?; Ok(Value::bool(a[0].word(skein_rt::conc::handle::EPOCH) == skein_rt::conc::process_epoch())) };
    "handle-kind" 1 Some(1) => |_, a| { expect(a[0], Kind::Handle, "handle-kind")?; Ok(a[0].field(skein_rt::conc::handle::KIND)) };
    "handle-info" 1 Some(1) => |_, a| { expect(a[0], Kind::Handle, "handle-info")?; Ok(a[0].field(skein_rt::conc::handle::INFO)) };
    "%bytes-find" 3 Some(3) => |_, a| { expect(a[0], Kind::Bytes, "%bytes-find")?; expect(a[1], Kind::Bytes, "%bytes-find")?; let (h, n) = (a[0].as_bytes(), a[1].as_bytes()); let from = (int(a[2], "%bytes-find")?.max(0) as usize).min(h.len()); Ok(Value::fix(if n.is_empty() { from as i64 } else { h[from..].windows(n.len()).position(|w| w == n).map_or(-1, |i| (i + from) as i64) })) };
    "%bytes-index-of" 2 Some(2) => |_, a| { expect(a[0], Kind::Bytes, "%bytes-index-of")?; let x = int(a[1], "%bytes-index-of")? as u8; Ok(Value::fix(a[0].as_bytes().iter().position(|&b| b == x).map_or(-1, |i| i as i64))) };
    "%glob-match" 2 Some(2) => |_, a| Ok(Value::bool(crate::sys::glob_match(&text(a[0]), &text(a[1]))));
    "%glob-base" 1 Some(1) => |vm, a| Ok(str_new(&mut vm.m, &crate::sys::glob_base(&text(a[0]))));
    "%io-stats" 0 Some(0) => |vm, _| { let kv = [rt().keyword(&mut vm.m, None, "handles"), Value::fix(crate::reactor::open_count() as i64), rt().keyword(&mut vm.m, None, "blocking-threads"), Value::fix(crate::blocking::threads() as i64), rt().keyword(&mut vm.m, None, "carrier-threads"), Value::fix(crate::sched::carrier_threads() as i64)]; hash_map(vm, &kv) };
    // files, paths and the system (sys.rs; skein.io, skein.fs, skein.sys)
    "%fs-read" 2 Some(2) => |vm, a| crate::sys::read(vm, text(a[0]), a[1].truthy());
    "%fs-write" 3 Some(3) => |vm, a| { let d = data_bytes(a[1])?; crate::sys::write(vm, text(a[0]), d, a[2].truthy()) };
    "%fs-info" 1 Some(1) => |vm, a| crate::sys::info(vm, text(a[0]));
    "%fs-list" 1 Some(1) => |vm, a| crate::sys::list(vm, text(a[0]));
    "%fs-walk" 1 Some(1) => |vm, a| crate::sys::walk(vm, text(a[0]));
    "%fs-mkdirs" 1 Some(1) => |vm, a| crate::sys::mkdirs(vm, text(a[0]));
    "%fs-delete" 2 Some(2) => |vm, a| crate::sys::delete(vm, text(a[0]), a[1].truthy());
    "%fs-rename" 2 Some(2) => |vm, a| crate::sys::rename(vm, text(a[0]), text(a[1]));
    "%fs-copy" 2 Some(2) => |vm, a| crate::sys::copy(vm, text(a[0]), text(a[1]));
    "%fs-open" 2 Some(2) => |vm, a| { let mode = if a[1].is_keyword() { a[1].sym_name().to_owned() } else { text(a[1]) }; crate::sys::open(vm, text(a[0]), &mode) };
    "%file-read" 2 Some(2) => |vm, a| { let n = int(a[1], "read")?.max(1) as usize; crate::sys::file_read(vm, a[0], n) };
    "%file-write" 2 Some(2) => |vm, a| { let d = data_bytes(a[1])?; crate::sys::file_write(vm, a[0], d) };
    "%file-sync" 1 Some(1) => |vm, a| crate::sys::file_sync(vm, a[0]);
    "%fs-mkdir" 1 Some(1) => |vm, a| crate::sys::mkdir(vm, text(a[0]));
    "%fs-sync-dir" 1 Some(1) => |vm, a| crate::sys::sync_dir(vm, text(a[0]));
    "%stdin-line" 0 Some(0) => |vm, _| crate::sys::stdin_line(vm);
    "%stdin-read" 1 Some(1) => |vm, a| { let n = int(a[0], "%stdin-read")?.max(0) as usize; crate::sys::stdin_read(vm, n) };
    "%write-stdout" 1 Some(1) => |_, a| { expect(a[0], Kind::String, "%write-stdout")?; crate::sys::write_stdout(a[0].as_str()); Ok(Value::NIL) };
    "%fs-watch" 2 Some(2) => |vm, a| crate::sys::watch(vm, text(a[0]), a[1].truthy());
    "%path-join" 1 Some(1) => |_vm, a| { let parts: Vec<String> = seq::SeqIter::new(a[0]).map(|it| it.map(text).collect()).unwrap_or_default(); Ok(str_new(&mut _vm.m, &crate::sys::path_join(&parts))) };
    "%path-part" 2 Some(2) => |vm, a| { let part = if a[1].is_keyword() { a[1].sym_name().to_owned() } else { text(a[1]) }; crate::sys::path_part(&mut vm.m, &text(a[0]), &part) };
    "%getenv" 1 Some(1) => |vm, a| Ok(crate::sys::getenv(&mut vm.m, &text(a[0])));
    "%env" 0 Some(0) => |vm, _| Ok(crate::sys::env(&mut vm.m));
    "%args" 0 Some(0) => |vm, _| Ok(crate::sys::args(&mut vm.m));
    "%program-args" 0 Some(0) => |vm, _| Ok(crate::sys::program_args(&mut vm.m));
    "%tty?" 1 Some(1) => |_, a| crate::sys::is_tty(a[0]);
    "%tty-size" 1 Some(1) => |vm, a| crate::sys::tty_size(&mut vm.m, a[0]);
    "%tty-raw!" 1 Some(1) => |_, a| crate::sys::tty_raw(a[0]);
    "%tty-restore!" 1 Some(1) => |_, a| crate::sys::tty_restore(a[0]);
    "%tty-read" 3 Some(3) => |vm, a| { let (max, t) = (int(a[1], "%tty-read")?, int(a[2], "%tty-read")?); crate::sys::tty_read(vm, a[0], max, t) };
    "%sys-info" 0 Some(0) => |vm, _| Ok(crate::sys::system(&mut vm.m));
    // string helpers for skein.rope: character and line arithmetic on a chunk
    "%str-stats" 1 Some(1) => |vm, a| { expect(a[0], Kind::String, "%str-stats")?; let t = a[0].as_str(); let c = a[0].str_char_count() as i64; let n = t.bytes().filter(|&b| b == b'\n').count() as i64; Ok(vec_from_slice(&mut vm.m, &[Value::fix(c), Value::fix(n)])) };
    "%nth-newline-end" 2 Some(2) => |_, a| { expect(a[0], Kind::String, "%nth-newline-end")?; let k = int(a[1], "%nth-newline-end")?; let mut seen = 0; for (i, ch) in a[0].as_str().chars().enumerate() { if ch == '\n' { seen += 1; if seen == k { return Ok(Value::fix(i as i64 + 1)); } } } Ok(Value::NIL) };
    "%count-newlines" 2 Some(2) => |_, a| { expect(a[0], Kind::String, "%count-newlines")?; let i = int(a[1], "%count-newlines")?.max(0) as usize; Ok(Value::fix(a[0].as_str().chars().take(i).filter(|&c| c == '\n').count() as i64)) };
    // text (text.rs): skein.string, skein.regex, skein.json, format
    "%str-case" 2 Some(2) => crate::text::case;
    "%str-trim" 2 Some(2) => crate::text::trim;
    "%str-index" 4 Some(4) => crate::text::index;
    "%str-split" 3 Some(3) => crate::text::split;
    "%str-replace" 4 Some(4) => crate::text::replace;
    "%graphemes" 1 Some(1) => crate::text::graphemes;
    "%grapheme-count" 1 Some(1) => crate::text::grapheme_count;
    "%str-width" 1 Some(1) => crate::text::width;
    "%str-reverse" 1 Some(1) => crate::text::reverse;
    "%str-truncate" 2 Some(2) => crate::text::truncate;
    "%instance?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Instance)));
    "regex?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Regex)));
    "re-pattern" 1 Some(1) => crate::text::re_pattern;
    "re-quote" 1 Some(1) => crate::text::re_quote;
    "%re-search" 4 Some(4) => crate::text::re_search;
    "%re-all" 2 Some(2) => crate::text::re_all;
    "%re-replace" 4 Some(4) => crate::text::re_replace;
    "%re-split" 3 Some(3) => crate::text::re_split;
    "%json-read" 2 Some(2) => crate::text::json_read;
    "%json-write" 3 Some(3) => crate::text::json_write;
    // (%s and %S print instances as print-object says, as str and pr-str do)
    "%format" 2 Some(2) => |vm, a| { let o = print_overrides(vm, &a[1..2], false)?; skein_rt::print::with_overrides(o, || crate::text::format(vm, a)) };
    "%parse-number" 2 Some(2) => crate::text::parse_number;
    "%read-cst" 1 Some(1) => crate::cst::read_cst;
    "%form-at" 2 Some(2) => |vm, a| { expect(a[0], Kind::String, "%form-at")?; let line = int(a[1], "%form-at")?.max(1) as usize; crate::eval::form_at(vm, a[0].as_str(), line) };
    // math and randomness (math.rs): skein.math, skein.random, rand
    "%math1" 2 Some(2) => crate::math::math1;
    "%math2" 3 Some(3) => crate::math::math2;
    "%round" 2 Some(2) => crate::math::round;
    "%gcd" 2 Some(2) => crate::math::gcd;
    "%isqrt" 1 Some(1) => crate::math::isqrt;
    "%float-class" 1 Some(1) => crate::math::float_class;
    "%rand" 1 Some(1) => crate::math::rand;
    "%rand-below" 1 Some(1) => crate::math::rand_below;
    "%rand-at" 3 Some(3) => crate::math::rand_at;
    "%rand-below-at" 3 Some(3) => crate::math::rand_below_at;
    "%rand-seed" 0 Some(0) => crate::math::rand_seed;
    "%write-stderr" 1 Some(1) => |_, a| { use std::io::Write; expect(a[0], Kind::String, "%write-stderr")?; let mut e = std::io::stderr().lock(); let _ = e.write_all(a[0].as_str().as_bytes()); let _ = e.flush(); Ok(Value::NIL) };
    "%split-lines" 1 Some(1) => |vm, a| { expect(a[0], Kind::String, "lines")?; let ls: Vec<Value> = a[0].as_str().lines().map(|l| str_new(&mut vm.m, l)).collect(); Ok(vec_from_slice(&mut vm.m, &ls)) };
    // sockets and TLS (net.rs; skein.net)
    "%resolve" 2 Some(2) => |vm, a| { let port = int(a[1], "resolve")?; crate::net::resolve(vm, text(a[0]), port.clamp(0, 65535) as u16) };
    "%tcp-connect" 2 Some(2) => |vm, a| crate::net::tcp_connect(vm, &text(a[0]), a[1]);
    "%tcp-check" 1 Some(1) => |vm, a| crate::net::tcp_check(vm, a[0]);
    "%tcp-listen" 2 Some(2) => |vm, a| crate::net::tcp_listen(vm, &text(a[0]), a[1]);
    "%tcp-accept" 1 Some(1) => |vm, a| crate::net::tcp_accept(vm, a[0]);
    "%socket-address" 2 Some(2) => |vm, a| crate::net::address(vm, a[0], a[1].truthy());
    "%shutdown-write" 1 Some(1) => |_, a| crate::net::shutdown_write(a[0]);
    "%udp-bind" 2 Some(2) => |vm, a| crate::net::udp_bind(vm, &text(a[0]), a[1]);
    "%udp-send" 3 Some(3) => |vm, a| { let d = data_bytes(a[1])?; crate::net::udp_send(vm, a[0], &d, &text(a[2])) };
    "%udp-recv" 2 Some(2) => |vm, a| { let n = int(a[1], "receive")?.max(1) as usize; crate::net::udp_recv(vm, a[0], n) };
    "%tls-client" 4 Some(4) => |vm, a| { let ca = if a[2].is_nil() { None } else { Some(data_bytes(a[2])?) }; crate::net::tls_client(vm, a[0], &text(a[1]), ca.as_deref(), a[3]) };
    "%tls-server" 4 Some(4) => |vm, a| { let c = data_bytes(a[1])?; let k = data_bytes(a[2])?; crate::net::tls_server(vm, a[0], &c, &k, a[3]) };
    "%tls-shutdown" 1 Some(1) => |vm, a| tls_shutdown(vm, a[0]);
    "%tls-handshake" 1 Some(1) => |vm, a| tls_handshake(vm, a[0]);
    // the basic FFI (ffi.rs; skein.ffi)
    "%ffi-open" 1 Some(1) => |vm, a| crate::ffi::open(vm, &text(a[0]));
    "%ffi-fn" 5 Some(5) => |vm, a| { let name = if a[0].is_nil() { a[1] } else { let t = text(a[1]); str_new(&mut vm.m, &t) }; let mode = if a[4].is_keyword() && a[4].sym_name() == "main" { crate::ffi::CallMode::Main } else if a[4].truthy() { crate::ffi::CallMode::Blocking } else { crate::ffi::CallMode::Carrier }; crate::ffi::function(vm, a[0], name, a[2], a[3], mode) };
    "%ffi-call" 1 VAR => |vm, a| crate::ffi::call(vm, a[0], &a[1..]);
    "%mem-alloc" 1 Some(1) => |_, a| crate::ffi::mem_alloc(a);
    "%mem-free" 1 Some(1) => |_, a| crate::ffi::mem_free(a);
    "%mem-get" 3 Some(3) => |vm, a| crate::ffi::mem_get(&mut vm.m, a);
    "%mem-set!" 4 Some(4) => |_, a| crate::ffi::mem_set(a);
    "%mem-string" 1 Some(1) => |vm, a| crate::ffi::mem_string(&mut vm.m, a);
    "%mem-bytes" 2 Some(2) => |vm, a| crate::ffi::mem_bytes(&mut vm.m, a);
    "%mem-write-bytes!" 2 Some(2) => |_, a| crate::ffi::mem_write_bytes(a);
    "%ffi-callback" 4 Some(4) => |vm, a| crate::ffi::make_callback(vm, a);
    "%main-thread-served?" 0 Some(0) => |_, _| Ok(Value::bool(crate::main_lane::served()));
    "%ffi-callback-reply!" 3 Some(3) => |_, a| crate::ffi::callback_reply(a);
    "%ffi-callback-free!" 1 Some(1) => |_, a| crate::ffi::callback_free(a);
    "%ffi-libc-path" 0 Some(0) => |vm, _| Ok(str_new(&mut vm.m, if cfg!(target_os = "macos") { "/usr/lib/libSystem.B.dylib" } else { "libc.so.6" }));
    "%ffi-libm-path" 0 Some(0) => |vm, _| Ok(str_new(&mut vm.m, if cfg!(target_os = "macos") { "/usr/lib/libSystem.B.dylib" } else { "libm.so.6" }));
    // processes, time, signals, fiber dumps (os.rs; skein.process, skein.time, skein.signal)
    "%process-start" 6 Some(6) => |vm, a| process_start(vm, a);
    "%deflate" 3 Some(3) => |vm, a| crate::pack::deflate(&mut vm.m, a);
    "%inflate" 3 Some(3) => |vm, a| crate::pack::inflate(&mut vm.m, a);
    "%inflater" 2 Some(2) => |_, a| crate::pack::inflater(a);
    "%inflate-chunk" 2 Some(2) => |vm, a| crate::pack::inflate_chunk(&mut vm.m, a);
    "%inflater-free" 1 Some(1) => |_, a| crate::pack::inflater_free(a);
    "%crc32" 1 Some(1) => |_, a| crate::pack::crc32(a);
    "%digest" 2 Some(2) => |vm, a| crate::pack::digest(&mut vm.m, a);
    "%hmac" 3 Some(3) => |vm, a| crate::pack::hmac(&mut vm.m, a);
    "%hmac-verify" 4 Some(4) => |_, a| crate::pack::hmac_verify(a);
    "%pbkdf2" 5 Some(5) => |vm, a| crate::pack::pbkdf2(&mut vm.m, a);
    "%secure-random" 1 Some(1) => |vm, a| crate::pack::secure_random(&mut vm.m, a);
    "%constant-time=" 2 Some(2) => |_, a| crate::pack::constant_time_eq(a);
    "%bin-write" 1 Some(1) => |vm, a| crate::binfmt::write(&mut vm.m, a[0]);
    "%bin-read" 3 Some(3) => |vm, a| crate::binfmt::read(&mut vm.m, a[0], a[1], int(a[2], "skein.bin")?.clamp(1, 10_000) as usize);
    "%pty-start" 6 Some(6) => |vm, a| pty_start(vm, a);
    "%pty-resize" 3 Some(3) => |_, a| crate::os::pty_resize(a[0], int(a[1], "resize")?.clamp(1, 65535) as u16, int(a[2], "resize")?.clamp(1, 65535) as u16);
    "%process-wait" 1 Some(1) => |vm, a| crate::os::wait(vm, a[0]);
    "%process-kill" 2 Some(2) => |vm, a| { let n = signal_arg(a[1])?; crate::os::kill(vm, a[0], n) };
    "%signal-listen" 1 Some(1) => |vm, a| { let names: Vec<String> = seq::SeqIter::new(a[0]).map(|it| it.map(|k| if k.is_keyword() { k.sym_name().to_owned() } else { text(k) }).collect()).unwrap_or_default(); crate::os::listen(vm, &names) };
    "%signal-listeners" 1 Some(1) => |_, a| { let n = signal_arg(a[0])?; Ok(Value::fix(crate::os::listeners(n) as i64)) };
    "%raise-signal" 1 Some(1) => |_, a| { let n = signal_arg(a[0])?; unsafe { libc::raise(n) }; Ok(Value::NIL) };
    "%now-nanos" 0 Some(0) => |_, _| Ok(Value::fix(crate::os::now_nanos()));
    "%local-offset" 1 Some(1) => |_, a| Ok(Value::fix(crate::os::local_offset(int(a[0], "%local-offset")?)));
    "%civil" 2 Some(2) => |vm, a| { let c = crate::os::civil(int(a[0], "civil")?, int(a[1], "civil")?); let vs: Vec<Value> = c.iter().map(|&x| Value::fix(x)).collect(); Ok(vec_from_slice(&mut vm.m, &vs)) };
    "%from-civil" 2 Some(2) => |_, a| { let xs: Vec<i64> = seq::SeqIter::new(a[0]).map(|it| it.map(|v| if v.is_fixnum() { v.as_fixnum() } else { 0 }).collect()).unwrap_or_default(); if xs.len() != 7 { return Err(Error::new("%from-civil needs [y m d h mi s ns]")); } let f = [xs[0], xs[1], xs[2], xs[3], xs[4], xs[5], xs[6]]; Ok(Value::fix(crate::os::from_civil(f, int(a[1], "from-civil")?).map_err(Error::new)?)) };
    "%format-iso" 2 Some(2) => |vm, a| Ok(str_new(&mut vm.m, &crate::os::format_iso(int(a[0], "format")?, int(a[1], "format")?)));
    "%parse-iso" 1 Some(1) => |vm, a| { let (n, o) = crate::os::parse_iso(&text(a[0])).map_err(Error::new)?; Ok(vec_from_slice(&mut vm.m, &[Value::fix(n), Value::fix(o)])) };
    "fiber-dump" 0 Some(0) => |vm, _| crate::os::fiber_dump(vm);
    // test helpers: a native call that blocks its carrier, and one on the pool
    "%native-stall" 1 Some(1) => |vm, a| { let ms = int(a[0], "%native-stall")?.max(0) as u64; vm.native_region(&[], || std::thread::sleep(std::time::Duration::from_millis(ms))); Ok(Value::fix(ms as i64)) };
    "%blocking-sleep" 1 Some(1) => |vm, a| { let ms = int(a[0], "%blocking-sleep")?.max(0) as u64; crate::blocking::run(vm, move || { std::thread::sleep(std::time::Duration::from_millis(ms)); Box::new(move |_: &mut skein_rt::Mutator| Ok(Value::fix(ms as i64))) }) };
    // weak references and cleanups
    "weak-ref" 1 Some(1) => |vm, a| Ok(skein_rt::weak::weak_new(&mut vm.m, a[0], Value::NIL));
    "weak-get" 1 Some(1) => |_, a| { expect(a[0], Kind::Weak, "weak-get")?; Ok(a[0].weak_get()) };
    "add-cleanup!" 2 Some(2) => |vm, a| Ok(skein_rt::weak::weak_new(&mut vm.m, a[0], a[1]));
    "cancel-cleanup!" 1 Some(1) => |_, a| { expect(a[0], Kind::Weak, "cancel-cleanup!")?; Ok(Value::bool(a[0].weak_cancel())) };
    // libraries and namespaces
    "%embedded-libs" 0 Some(0) => |vm, _| { let names: Vec<Value> = crate::eval::LIBS.iter().map(|(n, _)| str_new(&mut vm.m, n)).collect(); Ok(vec_from_slice(&mut vm.m, &names)) };
    "%find-lib" 1 Some(1) => |vm, a| { expect(a[0], Kind::String, "%find-lib")?; Ok(match crate::eval::find_lib(a[0].as_str()) { Some((src, origin)) => { let s = str_new(&mut vm.m, &src); let o = str_new(&mut vm.m, &origin); vec_from_slice(&mut vm.m, &[s, o]) } None => Value::NIL }) };
    "%alias" 2 Some(2) => |vm, a| { expect(a[0], Kind::Symbol, "alias")?; expect(a[1], Kind::Symbol, "alias")?; if rt().find_ns(a[1]).is_none() { return Err(Error::new(format!("No namespace: {}", text(a[1])))); } vm.ns.add_alias(a[0], a[1]); Ok(Value::NIL) };
    "%refer" 2 Some(2) => |vm, a| { expect(a[0], Kind::Symbol, "refer")?; expect(a[1], Kind::Var, "refer")?; vm.ns.refer(a[0], a[1]); Ok(Value::NIL) };
    "ns-aliases" 1 Some(1) => |vm, a| { expect(a[0], Kind::Symbol, "ns-aliases")?; let pairs: Vec<Value> = rt().find_ns(a[0]).map(|ns| ns.aliases().into_iter().flat_map(|(k, v)| [k, v]).collect()).unwrap_or_default(); hash_map(vm, &pairs) };
    // images
    "%save-image" 4 Some(5) => |vm, a| { expect(a[0], Kind::String, "save-image")?; crate::image::save_image(vm, a[0], a[1], a[2].truthy(), a[3].truthy(), a.get(4).copied().unwrap_or(Value::NIL)) };
    "%save-image-outcome" 0 Some(0) => |vm, _| crate::image::save_outcome(vm);
    "image-info" 0 Some(0) => |vm, _| crate::image::info(vm);
    "jit-stats" 0 Some(0) => |vm, _| { use std::sync::atomic::Ordering::Relaxed; let k = |vm: &mut Vm, n: &str| rt().keyword(&mut vm.m, None, n); let kvs = [k(vm, "tier"), Value::fix(if crate::jit::enabled() { 1 } else { 0 }), k(vm, "compiled"), Value::fix(crate::jit::COMPILED.load(Relaxed) as i64), k(vm, "rejected"), Value::fix(crate::jit::REJECTED.load(Relaxed) as i64), k(vm, "panicked"), Value::fix(crate::jit::PANICKED.load(Relaxed) as i64), k(vm, "code-bytes"), Value::fix(crate::jit::CODE_BYTES.load(Relaxed) as i64), k(vm, "linked"), Value::fix(crate::jit::LINKED_SITES.load(Relaxed) as i64), k(vm, "relinked"), Value::fix(crate::jit::RELINKED.load(Relaxed) as i64), k(vm, "despecialised"), Value::fix(crate::jit::DESPECIALISED.load(Relaxed) as i64), k(vm, "pending"), Value::fix(crate::jit::COMPILES_PENDING.load(Relaxed) as i64)]; hash_map(vm, &kvs) };
    "%core-source" 0 Some(0) => |vm, _| Ok(str_new(&mut vm.m, crate::eval::CORE_SRC));
    // classes, instances, generic functions (runtime half of CLOS; policy is in core.skn)
    "class-of" 1 Some(1) => |_, a| Ok(skein_rt::clos::class_of(a[0]));
    "class?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Class)));
    "generic?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Generic)));
    "instance?" 2 Some(2) => |_, a| { expect(a[0], Kind::Class, "instance?")?; let c = skein_rt::clos::class_of(a[1]); Ok(Value::bool(!c.is_nil() && skein_rt::clos::is_subclass(c, a[0]))) };
    "subclass?" 2 Some(2) => |_, a| { expect(a[0], Kind::Class, "subclass?")?; expect(a[1], Kind::Class, "subclass?")?; Ok(Value::bool(skein_rt::clos::is_subclass(a[0], a[1]))) };
    "%class-new" 1 Some(1) => |vm, a| Ok(skein_rt::clos::class_new(&mut vm.m, a[0]));
    "%class-get" 2 Some(2) => |vm, a| { expect(a[0], Kind::Class, "%class-get")?; let f = class_field(vm, a[1])?; let v = a[0].class_field(f); Ok(if f == skein_rt::clos::class::LAYOUT { vec_from_slice(&mut vm.m, skein_rt::clos::layout_keys(v)) } else { v }) };
    "%class-set-layout!" 3 Some(3) => |vm, a| { expect(a[0], Kind::Class, "%class-set-layout!")?; let k = to_vec(vm, a[1])?; let d = to_vec(vm, a[2])?; if k.len() != d.len() { return Err(Error::new("%class-set-layout!: keys and defaults differ in length")); } let l = skein_rt::clos::layout_new(&mut vm.m, &k, &d); a[0].class_set_field(skein_rt::clos::class::LAYOUT, l); Ok(a[0]) };
    "%bound-value" 1 Some(1) => |vm, a| { Ok(match vm.resolve(a[0]) { Some(v) if v.var_root() != Value::UNBOUND => v.var_root(), _ => Value::NIL }) };
    "bit-and" 2 VAR => |vm, a| crate::numeric::bits(&mut vm.m, a, crate::numeric::Bit::And, "bit-and");
    "bit-or" 2 VAR => |vm, a| crate::numeric::bits(&mut vm.m, a, crate::numeric::Bit::Or, "bit-or");
    "bit-xor" 2 VAR => |vm, a| crate::numeric::bits(&mut vm.m, a, crate::numeric::Bit::Xor, "bit-xor");
    "bit-not" 1 Some(1) => |vm, a| crate::numeric::bit_not(&mut vm.m, a[0]);
    "bit-shift-left" 2 Some(2) => |vm, a| crate::numeric::shift(&mut vm.m, a[0], a[1], true);
    "bit-shift-right" 2 Some(2) => |vm, a| crate::numeric::shift(&mut vm.m, a[0], a[1], false);
    "%class-set!" 3 Some(3) => class_set;
    "%register-builtin!" 2 Some(2) => |_, a| { expect(a[1], Kind::Class, "%register-builtin!")?; let n = text(a[0]); if skein_rt::clos::register_builtin(&n, a[1]) { Ok(a[1]) } else { Err(Error::new(format!("Unknown builtin class: {n}"))) } };
    "%bump-class-epoch" 0 Some(0) => |_, _| { skein_rt::clos::bump_class_epoch(); Ok(Value::NIL) };
    "%instance" 2 Some(2) => instance_new;
    "%make-fast" 2 Some(2) => make_fast;
    // (MOP hooks, M9 part 9.5: bits of HOOK_*, set on a class, seen by its subclasses)
    // (x printed as a record, whatever print-object says: its default)
    "%pr-plain" 1 Some(1) => |vm, a| { forced(a)?; Ok(str_new(&mut vm.m, &pr_str(a[0], true))) };
    "%class-hook!" 2 Some(2) => |_, a| { expect(a[0], Kind::Class, "%class-hook!")?; skein_rt::clos::class_hook(a[0], int(a[1], "%class-hook!")? as u64); Ok(a[0]) };
    "%class-unhook!" 2 Some(2) => |_, a| { expect(a[0], Kind::Class, "%class-unhook!")?; skein_rt::clos::class_unhook(a[0], int(a[1], "%class-unhook!")? as u64); Ok(a[0]) };
    "%class-flag?" 2 Some(2) => |_, a| Ok(Value::bool(skein_rt::clos::class_flag(a[0], int(a[1], "%class-flag?")? as u64)));
    "%class-defaults" 1 Some(1) => |vm, a| { expect(a[0], Kind::Class, "%class-defaults")?; let l = a[0].class_field(skein_rt::clos::class::LAYOUT); Ok(vec_from_slice(&mut vm.m, skein_rt::clos::layout_defaults(l))) };
    "%class-hooked?" 2 Some(2) => |_, a| Ok(Value::bool(skein_rt::clos::class_hooked(a[0], int(a[1], "%class-hooked?")? as u64)));
    "slot-value" 2 Some(2) => |vm, a| slot_value_prim(vm, a[0], a[1]);
    // (slot-value without the MOP's hook: the standard method's)
    "%slot-value-basic" 2 Some(2) => |vm, a| slot_value(vm, a[0], a[1]);
    "%slot-ref" 2 Some(2) => |vm, a| slot_ref(vm, a[0], a[1]);
    "with" 1 VAR => |vm, a| { expect(a[0], Kind::Instance, "with")?; if typed(a[0]) { return Err(typed_call(vm, "%with-typed", &WITH_TYPED, a)); } with_raw(vm, a) };
    // (with, not checking the slots' :type: %with-typed's, after it has)
    "%with" 1 VAR => |vm, a| { expect(a[0], Kind::Instance, "with")?; with_raw(vm, a) };
    // owned mutable objects (owned.rs; design §4.2)
    "%set-slot!" 3 Some(3) => |vm, a| { if typed(a[0]) { return Err(typed_call(vm, "%set-slot-typed", &SET_SLOT_TYPED, a)); } crate::owned::set_slot(vm, a[0], a[1], a[2]) };
    // (%set-slot!, not checking the slot's :type: %set-slot-typed's, after it has)
    "%set-slot-unchecked!" 3 Some(3) => |vm, a| crate::owned::set_slot(vm, a[0], a[1], a[2]);
    "%release!" 1 Some(1) => |vm, a| crate::owned::release(vm, a[0]);
    "%share!" 2 Some(2) => |vm, a| crate::owned::share(vm, a[0], a[1]);
    "%owner" 1 Some(1) => |_, a| Ok(crate::owned::owner(a[0]));
    "mutable?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Instance) && a[0].instance_mutable()));
    "%gf-new" 5 Some(5) => |vm, a| { let nreq = int(a[1], "%gf-new")?.max(0) as usize; Ok(skein_rt::clos::generic_new(&mut vm.m, a[0], nreq, a[2].truthy(), a[3], a[4])) };
    "%gf-get" 2 Some(2) => gf_get;
    "%gf-set!" 3 Some(3) => gf_set;
    "%gf-cas-methods!" 5 Some(5) => |vm, a| { expect(a[0], Kind::Generic, "%gf-cas-methods!")?; let mask = int(a[3], "%gf-cas-methods!")? as u64; Ok(Value::bool(skein_rt::clos::gf_set_methods(&mut vm.m, a[0], a[1], a[2], mask, a[4]))) };
    "%gf-token" 1 Some(1) => |vm, a| { expect(a[0], Kind::Generic, "%gf-token")?; Ok(vec_from_slice(&mut vm.m, &[Value::fix(a[0].gf_epoch() as i64), Value::fix(skein_rt::clos::class_epoch() as i64)])) };
    "%gf-key" 2 Some(2) => |vm, a| { expect(a[0], Kind::Generic, "%gf-key")?; let args = to_vec(vm, a[1])?; let mut key = Vec::new(); skein_rt::clos::dispatch_key(a[0].gf_state(), &args, &mut key); Ok(vec_from_slice(&mut vm.m, &key)) };
    "%gf-cache-get" 2 Some(2) => gf_cache_get;
    "%gf-cache-put!" 4 Some(4) => gf_cache_put;
    "exit" 0 Some(1) => |_, a| {
        if let Some(v) = a.first() && !v.is_fixnum() { return Err(Error::of("TypeError", format!("exit expects an integer status, got {}", pr_str(*v, true)))); }
        let _ = std::io::stdout().flush(); std::process::exit(a.first().map(|v| v.as_fixnum() as i32).unwrap_or(0)) };
}

/// Bind every primitive to a var in skein.core.
pub fn install(vm: &mut Vm) {
    for (i, p) in PRIMS.iter().enumerate() {
        let sym = rt().symbol(&mut vm.m, None, p.name);
        let qualified = rt().symbol(&mut vm.m, Some("skein.core"), p.name);
        let prim = primitive_new(&mut vm.m, qualified, i, p.min, p.max);
        let var = vm.core.intern(&mut vm.m, sym);
        var.var_set_root(prim);
    }
}

// ---- helpers ------------------------------------------------------------------

fn type_err(op: &str, wanted: &str, got: Value) -> Error {
    Error::of("TypeError", format!("{op} expects {wanted}, got {}: {}", got.type_name(), pr_str(got, true)))
        .with("expected", crate::vm::ErrData::Str(wanted.to_owned()))
        .with("value", crate::vm::ErrData::Val(got))
}

pub(crate) fn expect(v: Value, k: Kind, op: &str) -> Result<()> {
    if v.is(k) { Ok(()) } else { Err(type_err(op, skein_rt::object::LAYOUTS[k as usize].name, v)) }
}

pub(crate) fn int(v: Value, op: &str) -> Result<i64> {
    if v.is_fixnum() { Ok(v.as_fixnum()) } else { Err(type_err(op, "an integer", v)) }
}

fn float_bytes(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    let value = crate::numeric::num(args[0])?.as_f64();
    match int(args[1], "%float-bytes")? {
        4 => {
            let narrow = value as f32;
            if value.is_finite() && narrow.is_infinite() {
                return Err(Error::new(format!("Float {} is outside the 4-byte float range", skein_rt::print::fmt_float(value))));
            }
            Ok(skein_rt::string::bytes_new(&mut vm.m, &narrow.to_be_bytes()))
        }
        8 => Ok(skein_rt::string::bytes_new(&mut vm.m, &value.to_be_bytes())),
        _ => Err(Error::new("Float width must be 4 or 8 bytes")),
    }
}

fn bytes_float(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    expect(args[0], Kind::Bytes, "%bytes-float")?;
    let value = match args[0].as_bytes() {
        &[a, b, c, d] => f32::from_be_bytes([a, b, c, d]) as f64,
        &[a, b, c, d, e, f, g, h] => f64::from_be_bytes([a, b, c, d, e, f, g, h]),
        _ => return Err(Error::new("Float width must be 4 or 8 bytes")),
    };
    Ok(float_new(&mut vm.m, value))
}

// ---- mutable buffers (skein.buffer/MutableBuffer) ------------------------------------
//
// A MutableBuffer is a (:mutable) instance whose :data slot holds a
// ByteArray. Every primitive here checks the instance's ownership first
// (writes as a write), so only its owner fiber touches the bytes. Element
// codes: 0 u8, 1 i8, 2 u16, 3 i16, 4 u32, 5 i32, 6 u64, 7 i64, 8 f32, 9 f64.

fn mbuf_storage(vm: &mut Vm, mb: Value, write: bool) -> Result<Value> {
    expect(mb, Kind::Instance, "mutable buffer")?;
    crate::owned::check(vm, mb, write)?;
    let key = rt().keyword(&mut vm.m, None, "data");
    match skein_rt::clos::slot_get(mb, key) {
        Some(d) if d.is(Kind::ByteArray) => Ok(d),
        _ => Err(Error::new("This mutable buffer was frozen: its storage is now an immutable Buffer")),
    }
}

fn elem_width(code: i64) -> Result<usize> {
    Ok(match code {
        0 | 1 => 1,
        2 | 3 => 2,
        4 | 5 | 8 => 4,
        6 | 7 | 9 => 8,
        _ => return Err(Error::new("Unknown buffer element type")),
    })
}

/// The bytes of element value v as type `code`, in order `le`.
fn encode_elem(code: i64, le: bool, v: Value) -> Result<Vec<u8>> {
    use num_traits::ToPrimitive;
    let width = elem_width(code)?;
    if code >= 8 {
        let x = crate::numeric::num(v)?.as_f64();
        let mut b = if code == 8 {
            let narrow = x as f32;
            if x.is_finite() && narrow.is_infinite() {
                return Err(Error::new(format!("Float {} is outside the 4-byte float range", skein_rt::print::fmt_float(x))));
            }
            narrow.to_be_bytes().to_vec()
        } else {
            x.to_be_bytes().to_vec()
        };
        if le {
            b.reverse();
        }
        return Ok(b);
    }
    let n: i128 = match crate::numeric::num(v)? {
        crate::numeric::Num::I(i) => i as i128,
        crate::numeric::Num::B(b) => b.to_i128().ok_or_else(|| Error::new("Value outside buffer element range"))?,
        _ => return Err(type_err("mutable buffer", "an integer", v)),
    };
    let bits = 8 * width as u32;
    let (lo, hi) = if code % 2 == 1 { (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1) } else { (0, (1i128 << bits) - 1) };
    if n < lo || n > hi {
        return Err(Error::new(format!("Value {n} outside buffer element range")));
    }
    let u = (n as u128) & ((1u128 << bits) - 1);
    let be = u.to_be_bytes();
    let mut b = be[16 - width..].to_vec();
    if le {
        b.reverse();
    }
    Ok(b)
}

fn mbuf_offset(d: Value, offset: Value, width: usize) -> Result<usize> {
    let o = int(offset, "mutable buffer")?;
    let len = d.as_bytes().len();
    if o < 0 || o as usize + width > len {
        return Err(Error::of("IndexError", format!("Mutable buffer offset {o} is outside its {len} bytes")).with("index", crate::vm::ErrData::Int(o)));
    }
    Ok(o as usize)
}

fn mbuf_load(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let d = mbuf_storage(vm, a[0], false)?;
    let code = int(a[2], "%mbuf-load")?;
    let width = elem_width(code)?;
    let at = mbuf_offset(d, a[1], width)?;
    let mut b = d.as_bytes()[at..at + width].to_vec();
    if a[3].truthy() {
        b.reverse(); // to big-endian
    }
    let mut word = [0u8; 8];
    word[8 - width..].copy_from_slice(&b);
    let u = u64::from_be_bytes(word);
    Ok(match code {
        8 => float_new(&mut vm.m, f32::from_bits(u as u32) as f64),
        9 => float_new(&mut vm.m, f64::from_bits(u)),
        6 => skein_rt::numbers::int_value(&mut vm.m, &num_bigint::BigInt::from(u)),
        _ if code % 2 == 1 => {
            let shift = 64 - 8 * width as u32;
            Value::fix(((u << shift) as i64) >> shift)
        }
        _ => Value::fix(u as i64),
    })
}

fn mbuf_store(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let code = int(a[2], "%mbuf-store!")?;
    let bytes = encode_elem(code, a[3].truthy(), a[4])?;
    let d = mbuf_storage(vm, a[0], true)?;
    let at = mbuf_offset(d, a[1], bytes.len())?;
    // SAFETY: the owner check above makes this fiber the only writer.
    unsafe { d.byte_array_mut()[at..at + bytes.len()].copy_from_slice(&bytes) };
    Ok(a[0])
}

fn mbuf_fill(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let code = int(a[1], "%mbuf-fill!")?;
    let bytes = encode_elem(code, a[2].truthy(), a[3])?;
    let d = mbuf_storage(vm, a[0], true)?;
    // SAFETY: as mbuf_store.
    let data = unsafe { d.byte_array_mut() };
    for chunk in data.chunks_exact_mut(bytes.len()) {
        chunk.copy_from_slice(&bytes);
    }
    Ok(a[0])
}

fn mbuf_blit(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    if !a[2].is(Kind::Bytes) && !a[2].is(Kind::ByteArray) {
        return Err(type_err("mutable buffer copy", "Bytes", a[2]));
    }
    let src = a[2].as_bytes().to_vec();
    let d = mbuf_storage(vm, a[0], true)?;
    let at = mbuf_offset(d, a[1], src.len())?;
    // SAFETY: as mbuf_store.
    unsafe { d.byte_array_mut()[at..at + src.len()].copy_from_slice(&src) };
    Ok(a[0])
}

/// Hand the storage over as Bytes, without copying; the buffer is finished.
fn mbuf_freeze(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let d = mbuf_storage(vm, a[0], true)?;
    let key = rt().keyword(&mut vm.m, None, "data");
    crate::owned::set_slot(vm, a[0], key, Value::NIL)?;
    skein_rt::string::byte_array_freeze(d);
    Ok(d)
}

/// Display text of a value (strings raw).
fn text(v: Value) -> String {
    pr_str(v, false)
}

static PRINT_OBJECT: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
static PRINT_READABLY: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

/// The text print-object gives for each instance in a (at any depth in
/// its collections and slots) whose class has a print-object method: the
/// printer prints it in their place (M9 part 9.5). Nothing to do, and no
/// walk, until some class has one. `*print-readably*` is `readably` while
/// the methods run: pr's printing (text to read back) or print's (for people).
fn print_overrides(vm: &mut Vm, a: &[Value], readably: bool) -> Result<Vec<(u64, String)>> {
    use skein_rt::clos::{ANY_PRINT_HOOK, class::HOOK_PRINT, class_hooked, class_of};
    if !ANY_PRINT_HOOK.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(Vec::new());
    }
    fn walk(v: Value, depth: usize, found: &mut Vec<Value>) {
        if depth > 64 || found.len() > 10_000 {
            return;
        }
        if v.is(Kind::Instance) {
            if class_hooked(class_of(v), HOOK_PRINT) {
                if !found.iter().any(|f| f.0 == v.0) {
                    found.push(v);
                }
                return;
            }
            for x in skein_rt::clos::instance_values(v) {
                walk(x, depth + 1, found);
            }
        } else if v.is(Kind::Vector) {
            v.vec_for_each(|x| {
                walk(x, depth + 1, found);
                true
            });
        } else if v.is(Kind::Map) {
            v.map_for_each(|k, x| {
                walk(k, depth + 1, found);
                walk(x, depth + 1, found);
                true
            });
        } else if v.is(Kind::Set) {
            v.set_for_each(|x| {
                walk(x, depth + 1, found);
                true
            });
        } else if v.is_cons() {
            let mut c = v;
            let mut n = 0;
            while c.is_cons() && n < 100_000 {
                walk(c.car(), depth + 1, found);
                c = c.cdr();
                n += 1;
            }
        }
    }
    let mut found = Vec::new();
    for &x in a {
        walk(x, 0, &mut found);
    }
    if found.is_empty() {
        return Ok(Vec::new());
    }
    let var = vm.core_fn("print-object", &PRINT_OBJECT);
    let f = if var.is(Kind::Var) { var.var_root() } else { var };
    let flag = vm.core_fn("*print-readably*", &PRINT_READABLY);
    let old = vm.dyn_env;
    if flag.is(Kind::Var) {
        // (as binding does: a var is looked up in the bindings once marked)
        flag.var_set_flag(skein_rt::func::VAR_DYNBOUND);
        let base = if old.is_nil() { skein_rt::map::map_empty() } else { old };
        vm.dyn_env = skein_rt::map::map_assoc(&mut vm.m, base, flag, Value::bool(readably));
    }
    let mut out = Vec::with_capacity(found.len());
    let mut failed = None;
    for inst in found {
        match vm.apply(f, &[inst]) {
            Ok(t) => out.push((inst.0, if t.is(Kind::String) { t.as_str().to_owned() } else { pr_str(t, false) })),
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    vm.dyn_env = old;
    match failed {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// The arguments printed (readably or not) as a print primitive does, with
/// print-object's text for instances that have it.
fn printed(vm: &mut Vm, a: &[Value], readably: bool) -> Result<String> {
    let o = print_overrides(vm, a, readably)?;
    Ok(skein_rt::print::with_overrides(o, || join(a, readably)))
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

/// Printed output goes to stdout, unless `*out*` is bound to an atom of a
/// vector: then each piece of text is added to it (with-out-str).
fn out_to(vm: &mut Vm, s: &str, newline: bool) -> Result<Value> {
    let sym = rt().symbol(&mut vm.m, None, "*out*");
    let capture = vm.core.lookup(sym).and_then(|var| if vm.dyn_env.is(Kind::Map) { vm.dyn_env.map_get(var) } else { None });
    // Bound to {:out-event id} (an editor request): each piece streams to
    // the editors as {:event :out :id id :text "…"}.
    if let Some(m) = capture.filter(|b| b.is(Kind::Map)) {
        let key = rt().keyword(&mut vm.m, None, "out-event");
        if let Some(id) = m.map_get(key) {
            let text = if newline { format!("{s}\n") } else { s.to_owned() };
            let mut line = String::from("{:event :out :id ");
            write_value(&mut line, id, true);
            // (the connection the request came on: its output goes there)
            let conn = rt().keyword(&mut vm.m, None, "conn");
            if let Some(c) = m.map_get(conn).filter(|c| !c.is_nil()) {
                line.push_str(" :conn ");
                write_value(&mut line, c, true);
            }
            line.push_str(" :text ");
            let t = str_new(&mut vm.m, &text);
            write_value(&mut line, t, true);
            line.push('}');
            crate::sched::post_event(line);
            return Ok(Value::NIL);
        }
    }
    // Bound to :stderr: printing goes to standard error (a program whose
    // standard output is a protocol's, as skein lsp's is).
    if let Some(k) = capture.filter(|k| k.is(Kind::Keyword) && k.sym_name() == "stderr" && k.sym_ns().is_none()) {
        let _ = k;
        let text = if newline { format!("{s}\n") } else { s.to_owned() };
        crate::sys::write_stderr(&text);
        return Ok(Value::NIL);
    }
    if let Some(buf) = capture.filter(|b| b.is(Kind::Atom)) {
        let text = if newline { format!("{s}\n") } else { s.to_owned() };
        let piece = str_new(&mut vm.m, &text);
        loop {
            let old = buf.atom_get();
            if !old.is(Kind::Vector) {
                return Err(Error::new("*out* must be bound to an atom holding a vector"));
            }
            let new = skein_rt::vector::vec_conj(&mut vm.m, old, piece);
            if buf.atom_cas(old, new) {
                return Ok(Value::NIL);
            }
        }
    }
    out(s, newline)
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
    let v = seqable(vm, v)?;
    Ok(seq::to_vec(&mut vm.m, v)?)
}

// ---- collections a library defines ---------------------------------------------------
//
// An instance is a collection when its class has collection-seq (and
// perhaps collection-count and collection-nth) methods, generic functions
// in skein.core. The primitives call them only for instances, so built-in
// collections never pay for the check beyond their kind.

static COLLECTION_OP: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
static COLLECTION_COUNT: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
static COLLECTION_NTH: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
static COLLECTION_SEQ: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

/// Hand primitive op's call on an instance to skein.core/%collection-op,
/// which the interpreter runs in its place, in the fiber: the methods it
/// calls can signal, be caught and park like any other call.
fn collection_op(vm: &mut Vm, op: &str, a: &[Value]) -> Result<Value> {
    // (count, and nth without a default: straight to the generic function,
    // M10 part 6.3; the rest, and nth with a default, which is checked
    // against the count first, through %collection-op)
    let direct: Option<(&str, &'static std::sync::OnceLock<Value>)> = match (op, a.len()) {
        ("count", 1) => Some(("collection-count", &COLLECTION_COUNT)),
        ("nth", 2) => Some(("collection-nth", &COLLECTION_NTH)),
        _ => None,
    };
    if let Some((name, cell)) = direct {
        let var = vm.core_fn(name, cell);
        let f = if var.is(Kind::Var) { var.var_root() } else { var };
        let args = vec_from_slice(&mut vm.m, a);
        return Err(Error::call(f, args));
    }
    let var = vm.core_fn("%collection-op", &COLLECTION_OP);
    let f = if var.is(Kind::Var) { var.var_root() } else { var };
    let mut args = Vec::with_capacity(a.len() + 1);
    args.push(rt().keyword(&mut vm.m, None, op));
    args.extend_from_slice(a);
    let args = vec_from_slice(&mut vm.m, &args);
    Err(Error::call(f, args))
}

/// v, or for an instance what its collection-seq method gives: something
/// the seq functions take.
fn seqable(vm: &mut Vm, v: Value) -> Result<Value> {
    if !v.is(Kind::Instance) {
        return Ok(v);
    }
    // (primitives that take any collection: made here, nested, so an
    // error in the method ends the call rather than reaching a handler)
    let var = vm.core_fn("collection-seq", &COLLECTION_SEQ);
    let f = if var.is(Kind::Var) { var.var_root() } else { var };
    let s = vm.apply(f, &[v])?;
    if s.is(Kind::Instance) {
        return Err(Error::new("collection-seq must give a seq or a built-in collection, not another instance"));
    }
    Ok(s)
}

// ---- numbers (the tower itself is in numeric.rs) ----------------------------------

use crate::numeric::{IntDiv, Op};

fn arith(vm: &mut Vm, a: &[Value], op: Op) -> Result<Value> {
    // Fast path: two fixnums that don't overflow.
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
    crate::numeric::arith(&mut vm.m, a, op)
}

fn compare_chain(a: &[Value], ok: fn(Ordering) -> bool) -> Result<Value> {
    if let [x, y] = a
        && x.is_fixnum()
        && y.is_fixnum()
    {
        return Ok(Value::bool(ok(x.as_fixnum().cmp(&y.as_fixnum()))));
    }
    crate::numeric::compare_chain(a, ok)
}

/// Total order used by `compare` and `sort`.
/// `(%sort-keyed keys vals)`: vals (a vector) ordered by keys (a vector of
/// the same length) under `compare`, stably. sort and sort-by use it when
/// the comparison is the default one.
fn sort_keyed(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let keys = to_vec(vm, a[0])?;
    let vals = to_vec(vm, a[1])?;
    if keys.len() != vals.len() {
        return Err(Error::new("%sort-keyed: keys and values differ in length"));
    }
    let mut order: Vec<usize> = (0..keys.len()).collect();
    let mut failed = None;
    order.sort_by(|&i, &j| match compare(keys[i], keys[j]) {
        Ok(o) => o,
        Err(e) => {
            failed.get_or_insert(e);
            Ordering::Equal
        }
    });
    if let Some(e) = failed {
        return Err(e);
    }
    let out: Vec<Value> = order.iter().map(|&i| vals[i]).collect();
    Ok(vec_from_slice(&mut vm.m, &out))
}

/// The total order of `compare` and `sort`. Vectors compare by length,
/// then element by element; nested ones are walked with a stack of our own
/// (vector pairs and the next index), so deep nesting can't exhaust the
/// native stack.
pub fn compare(a: Value, b: Value) -> Result<Ordering> {
    let mut stack: Vec<(Value, Value, usize)> = Vec::new();
    let (mut a, mut b) = (a, b);
    loop {
        if a.is(Kind::Vector) && b.is(Kind::Vector) {
            let o = a.vec_count().cmp(&b.vec_count());
            if o != Ordering::Equal {
                return Ok(o);
            }
            stack.push((a, b, 0));
        } else {
            let o = compare_scalar(a, b)?;
            if o != Ordering::Equal {
                return Ok(o);
            }
        }
        // the next pair of elements to compare, or done
        loop {
            let Some(top) = stack.last_mut() else { return Ok(Ordering::Equal) };
            if top.2 < top.0.vec_count() {
                (a, b) = (top.0.vec_nth(top.2), top.1.vec_nth(top.2));
                top.2 += 1;
                break;
            }
            stack.pop();
        }
    }
}

/// compare for anything but two vectors.
fn compare_scalar(a: Value, b: Value) -> Result<Ordering> {
    use skein_rt::numbers::is_number;
    if is_number(a) && is_number(b) {
        return Ok(crate::numeric::cmp(&crate::numeric::num(a)?, &crate::numeric::num(b)?));
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
    Err(Error::new(format!("Can't compare {} with {}", a.type_name(), b.type_name())))
}

// ---- collections ------------------------------------------------------------------

pub(crate) fn hash_map(vm: &mut Vm, a: &[Value]) -> Result<Value> {
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

/// A signal given as a keyword (:term) or a number.
fn signal_arg(v: Value) -> Result<i32> {
    if v.is_fixnum() {
        return Ok(v.as_fixnum() as i32);
    }
    if v.is_keyword()
        && let Some(n) = crate::os::signal_number(v.sym_name())
    {
        return Ok(n);
    }
    Err(Error::new(format!("Unknown signal {}", pr_str(v, true))))
}

/// `(%process-start cmd args dir env clear-env? [stdin stdout stderr])`.
/// `(%pty-start cmd args dir env clear-env [rows cols])`.
fn pty_start(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let strs = |v: Value| -> Vec<String> { seq::SeqIter::new(v).map(|it| it.map(text).collect()).unwrap_or_default() };
    let mut env = Vec::new();
    if a[3].is(Kind::Map) {
        a[3].map_for_each(|k, v| {
            env.push((text(k), text(v)));
            true
        });
    }
    let size: Vec<i64> = seq::SeqIter::new(a[5]).map(|it| it.map(|v| if v.is_fixnum() { v.as_fixnum() } else { 0 }).collect()).unwrap_or_default();
    let (rows, cols) = (size.first().copied().unwrap_or(24).clamp(1, 65535) as u16, size.get(1).copied().unwrap_or(80).clamp(1, 65535) as u16);
    let spawn = crate::os::Spawn {
        cmd: text(a[0]),
        args: strs(a[1]),
        dir: if a[2].is_nil() { None } else { Some(text(a[2])) },
        env,
        clear_env: a[4].truthy(),
        stdio: [crate::os::Stdio::Inherit; 3],
    };
    crate::os::pty_start(vm, spawn, rows, cols)
}

fn process_start(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let strs = |v: Value| -> Vec<String> { seq::SeqIter::new(v).map(|it| it.map(text).collect()).unwrap_or_default() };
    let mut env = Vec::new();
    if a[3].is(Kind::Map) {
        a[3].map_for_each(|k, v| {
            env.push((text(k), text(v)));
            true
        });
    }
    let modes: Vec<crate::os::Stdio> = seq::SeqIter::new(a[5])
        .map(|it| {
            it.map(|k| match if k.is_keyword() { k.sym_name() } else { "pipe" } {
                "inherit" => crate::os::Stdio::Inherit,
                "null" => crate::os::Stdio::Null,
                _ => crate::os::Stdio::Pipe,
            })
            .collect()
        })
        .unwrap_or_default();
    let mode = |i: usize| modes.get(i).copied().unwrap_or(crate::os::Stdio::Pipe);
    crate::os::start(
        vm,
        crate::os::Spawn {
            cmd: text(a[0]),
            args: strs(a[1]),
            dir: if a[2].is_nil() { None } else { Some(text(a[2])) },
            env,
            clear_env: a[4].truthy(),
            stdio: [mode(0), mode(1), mode(2)],
        },
    )
}

/// The bytes of a string (UTF-8) or of a Bytes value.
fn data_bytes(v: Value) -> Result<Vec<u8>> {
    if v.is(Kind::Bytes) {
        Ok(v.as_bytes().to_vec())
    } else if v.is(Kind::String) {
        Ok(v.as_str().as_bytes().to_vec())
    } else {
        Err(type_err("write", "a string or bytes", v))
    }
}

/// `:read` → 0, `:write` → 1.
fn io_dir(k: Value) -> Result<usize> {
    if k.is_keyword() {
        match k.sym_name() {
            "read" => return Ok(crate::reactor::READ),
            "write" => return Ok(crate::reactor::WRITE),
            _ => {}
        }
    }
    Err(Error::new(format!("Expected :read or :write, got {}", pr_str(k, true))))
}

/// The value I/O primitives return when they would block.
fn again(vm: &mut Vm) -> Value {
    rt().keyword(&mut vm.m, None, "again")
}

/// What a TLS operation is waiting for, as a keyword.
fn tls_wait(vm: &mut Vm, w: crate::net::Wait) -> Value {
    match w {
        crate::net::Wait::Read => again(vm),
        crate::net::Wait::Write => again_write(vm),
    }
}

/// The value write-side I/O primitives return when they would block.
fn again_write(vm: &mut Vm) -> Value {
    rt().keyword(&mut vm.m, None, "again-write")
}

/// `(%io-read h n)`: up to n bytes, nil at end of input, :again (wait until
/// readable) or :again-write (TLS: wait until writable).
fn io_read(vm: &mut Vm, h: Value, n: i64) -> Result<Value> {
    use std::io::Read;
    let mut buf = vec![0u8; n.clamp(1, 1 << 20) as usize];
    enum R {
        Plain(std::io::Result<usize>),
        Tls(std::result::Result<Option<Vec<u8>>, std::result::Result<crate::net::Wait, std::io::Error>>),
    }
    let r = crate::reactor::with_io(h, |io| match io {
        crate::reactor::Io::PipeIn(r) => R::Plain(r.read(&mut buf)),
        crate::reactor::Io::Tcp(s) => R::Plain(s.read(&mut buf)),
        crate::reactor::Io::Tls(t) => R::Tls(t.read(n.max(1) as usize)),
        _ => R::Plain(Err(std::io::Error::other("this handle can't be read"))),
    })?;
    match r {
        R::Plain(Ok(0)) => Ok(Value::NIL),
        // (a pseudo-terminal whose child has gone: the end of its output)
        R::Plain(Err(e)) if e.raw_os_error() == Some(libc::EIO) => Ok(Value::NIL),
        R::Plain(Ok(k)) => Ok(skein_rt::string::bytes_new(&mut vm.m, &buf[..k])),
        R::Plain(Err(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => Ok(again(vm)),
        R::Plain(Err(e)) => Err(Error::io(format!("read failed: {e}"), &e)),
        R::Tls(Ok(Some(b))) => Ok(skein_rt::string::bytes_new(&mut vm.m, &b)),
        R::Tls(Ok(None)) => Ok(Value::NIL),
        R::Tls(Err(Ok(w))) => Ok(tls_wait(vm, w)),
        R::Tls(Err(Err(e))) => Err(crate::net::tls_error("TLS read failed", &e)),
    }
}

/// `(%io-write h bytes start)`: bytes written from index start, :again-write
/// (wait until writable) or :again (TLS: wait until readable).
fn io_write(vm: &mut Vm, h: Value, b: Value, start: i64) -> Result<Value> {
    use std::io::Write;
    expect(b, Kind::Bytes, "write")?;
    let data = b.as_bytes().get(start.max(0) as usize..).unwrap_or(&[]);
    enum R {
        Plain(std::io::Result<usize>),
        Tls(std::result::Result<usize, std::result::Result<crate::net::Wait, std::io::Error>>),
    }
    let r = crate::reactor::with_io(h, |io| match io {
        crate::reactor::Io::PipeOut(w) => R::Plain(w.write(data)),
        crate::reactor::Io::Tcp(s) => R::Plain(s.write(data)),
        crate::reactor::Io::Tls(t) => R::Tls(t.write(data)),
        _ => R::Plain(Err(std::io::Error::other("this handle can't be written"))),
    })?;
    match r {
        R::Plain(Ok(k)) | R::Tls(Ok(k)) => Ok(Value::fix(k as i64)),
        R::Plain(Err(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => Ok(again_write(vm)),
        R::Plain(Err(e)) => Err(Error::io(format!("write failed: {e}"), &e)),
        R::Tls(Err(Ok(w))) => Ok(tls_wait(vm, w)),
        R::Tls(Err(Err(e))) => Err(crate::net::tls_error("TLS write failed", &e)),
    }
}

/// `(%tls-handshake h)`: nil once the handshake is done, or a wait.
fn tls_handshake(vm: &mut Vm, h: Value) -> Result<Value> {
    let r = crate::reactor::with_io(h, |io| match io {
        crate::reactor::Io::Tls(t) => t.handshake(),
        _ => Ok(()),
    })?;
    match r {
        Ok(()) => Ok(Value::NIL),
        Err(Ok(w)) => Ok(tls_wait(vm, w)),
        Err(Err(e)) => Err(crate::net::tls_error("TLS handshake failed", &e)),
    }
}

/// `(%tls-shutdown h)`: send close_notify and drain: nil once sent, or a wait.
fn tls_shutdown(vm: &mut Vm, h: Value) -> Result<Value> {
    let r = crate::reactor::with_io(h, |io| match io {
        crate::reactor::Io::Tls(t) => {
            t.conn.send_close_notify();
            t.drain()
        }
        _ => Ok(()),
    })?;
    match r {
        Ok(()) => Ok(Value::NIL),
        Err(Ok(w)) => Ok(tls_wait(vm, w)),
        Err(Err(e)) => Err(Error::new(format!("TLS close failed: {e}"))),
    }
}

/// Ask for any unrealized stream among `a` to be realized first (for the
/// operations that would otherwise just see it as unequal, or print it
/// unrealized).
fn forced(a: &[Value]) -> Result<()> {
    match a.iter().find_map(|&x| deep_unrealized(x)) {
        Some(s) => Err(Error::force(s, true)),
        None => Ok(()),
    }
}

/// An unrealized stream anywhere a print or comparison would look (in a
/// seq chain, or one level inside a vector or map entry).
fn deep_unrealized(x: Value) -> Option<Value> {
    if let Some(s) = seq::first_unrealized(x) {
        return Some(s);
    }
    if x.is(Kind::Vector) {
        return (0..x.vec_count()).find_map(|i| seq::first_unrealized(x.vec_nth(i)));
    }
    None
}

/// `(subseq sc test key)` / `(subseq sc test key test key)`: items of a sorted
/// collection within the bounds (tests are the functions `<` `<=` `>` `>=`).
fn sorted_range(vm: &mut Vm, a: &[Value], reverse: bool) -> Result<Value> {
    crate::sorted::check(a[0], "subseq")?;
    if a.len() != 3 && a.len() != 5 {
        return Err(Error::of("TypeError", "subseq expects a sorted collection and one or two test/key pairs"));
    }
    let mut bounds = Vec::new();
    for tk in a[1..].chunks(2) {
        let name = if tk[0].is(Kind::Primitive) { tk[0].prim_name().sym_name().to_owned() } else { String::new() };
        let (side, incl) = match name.as_str() {
            "<" => (Ordering::Less, false),
            "<=" => (Ordering::Less, true),
            ">" => (Ordering::Greater, false),
            ">=" => (Ordering::Greater, true),
            _ => return Err(Error::new("subseq tests must be <, <=, > or >=")),
        };
        bounds.push((side, incl, tk[1]));
    }
    crate::sorted::subseq(&mut vm.m, a[0], &bounds, reverse)
}

enum TOp {
    Conj(Value),
    Assoc(Value, Value),
    Dissoc(Value),
    Disj(Value),
    Pop,
}

/// `(transient coll)`: a builder for a vector, map or set, usable only by
/// this fiber until `persistent!`.
fn transient(vm: &mut Vm, c: Value) -> Result<Value> {
    use skein_rt::colls::*;
    if c.is(Kind::Vector) {
        let n = c.vec_count();
        let buf = skein_rt::array::array_new(&mut vm.m, n.max(8), Value::NIL);
        for i in 0..n {
            buf.arr_set(i, c.vec_nth(i));
        }
        return Ok(transient_new(&mut vm.m, TV, vm.current, n, buf));
    }
    let kind = if c.is(Kind::Map) || (c.is(Kind::Sorted) && !c.sorted_is_set()) {
        TM
    } else if c.is(Kind::Set) || c.is(Kind::Sorted) {
        TS
    } else {
        return Err(type_err("transient", "a vector, map or set", c));
    };
    Ok(transient_new(&mut vm.m, kind, vm.current, 0, c))
}

fn transient_check(vm: &Vm, t: Value) -> Result<()> {
    use skein_rt::colls::*;
    expect(t, Kind::Transient, "transient operation")?;
    if t.word(T_DONE) != 0 {
        return Err(Error::new("Transient used after persistent!"));
    }
    if t.field(T_OWNER) != vm.current {
        return Err(Error::of("OwnershipError", "Ownership: a transient belongs to the fiber that made it"));
    }
    Ok(())
}

fn transient_op(vm: &mut Vm, t: Value, op: TOp) -> Result<Value> {
    use skein_rt::colls::*;
    transient_check(vm, t)?;
    if t.word(T_KIND) == TV {
        let n = t.word(T_COUNT) as usize;
        let mut buf = t.field(T_DATA);
        match op {
            TOp::Conj(x) => {
                if n == buf.arr_len() {
                    let bigger = skein_rt::array::array_new(&mut vm.m, n * 2, Value::NIL);
                    for i in 0..n {
                        bigger.arr_set(i, buf.arr_get(i));
                    }
                    t.set_field(T_DATA, bigger);
                    buf = bigger;
                }
                buf.arr_set(n, x);
                t.set_word(T_COUNT, (n + 1) as u64);
            }
            TOp::Assoc(k, v) => match index_of(k).filter(|&i| i <= n) {
                Some(i) if i == n => return transient_op(vm, t, TOp::Conj(v)),
                Some(i) => buf.arr_set(i, v),
                None => return Err(Error::of("IndexError", format!("Index out of bounds: {}", pr_str(k, true))).with("index", crate::vm::ErrData::Val(k))),
            },
            TOp::Pop => {
                if n == 0 {
                    return Err(Error::new("Can't pop an empty vector"));
                }
                buf.arr_set(n - 1, Value::NIL);
                t.set_word(T_COUNT, (n - 1) as u64);
            }
            _ => return Err(Error::new("That operation doesn't apply to a transient vector")),
        }
        return Ok(t);
    }
    let c = t.field(T_DATA);
    let c2 = match op {
        TOp::Conj(x) => conj(vm, &[c, x])?,
        TOp::Assoc(k, v) if t.word(T_KIND) == TM => assoc(vm, &[c, k, v])?,
        TOp::Dissoc(k) if t.word(T_KIND) == TM => {
            if c.is(Kind::Sorted) { crate::sorted::dissoc(&mut vm.m, c, k)? } else { map_dissoc(&mut vm.m, c, k) }
        }
        TOp::Disj(k) if t.word(T_KIND) == TS => {
            if c.is(Kind::Sorted) { crate::sorted::dissoc(&mut vm.m, c, k)? } else { set_disj(&mut vm.m, c, k) }
        }
        _ => return Err(Error::new("That operation doesn't apply to this transient")),
    };
    t.set_field(T_DATA, c2);
    Ok(t)
}

/// `(get t k default?)` on a transient, which reads it as Clojure lets it
/// (as nth, contains? and count do): its items so far.
fn transient_get(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    use skein_rt::colls::*;
    let t = a[0];
    transient_check(vm, t)?;
    let dflt = a.get(2).copied().unwrap_or(Value::NIL);
    if t.word(T_KIND) == TV {
        let n = t.word(T_COUNT) as usize;
        return Ok(index_of(a[1]).filter(|&i| i < n).map(|i| t.field(T_DATA).arr_get(i)).unwrap_or(dflt));
    }
    let c = t.field(T_DATA);
    if c.is(Kind::Sorted) {
        let r = if c.sorted_is_set() { crate::sorted::find(c.sorted_root(), a[1])?.map(|n| n.field(0)) } else { crate::sorted::get(c, a[1])? };
        return Ok(r.unwrap_or(dflt));
    }
    Ok(get(c, a[1]).unwrap_or(dflt))
}

/// `(persistent! t)`: the finished collection; t can't be used again.
fn persistent(vm: &mut Vm, t: Value) -> Result<Value> {
    use skein_rt::colls::*;
    transient_check(vm, t)?;
    t.set_word(T_DONE, 1);
    if t.word(T_KIND) == TV {
        let n = t.word(T_COUNT) as usize;
        let buf = t.field(T_DATA);
        let items: Vec<Value> = (0..n).map(|i| buf.arr_get(i)).collect();
        return Ok(vec_from_slice(&mut vm.m, &items));
    }
    Ok(t.field(T_DATA))
}

/// `(bytes x)`: UTF-8 of a string, a seqable of integers 0–255, or bytes.
fn to_bytes(vm: &mut Vm, x: Value) -> Result<Value> {
    if x.is(Kind::Bytes) {
        return Ok(x);
    }
    if x.is(Kind::String) {
        return Ok(skein_rt::string::bytes_new(&mut vm.m, x.as_str().as_bytes()));
    }
    let mut out = Vec::new();
    if let Some(it) = seq::SeqIter::new(x) {
        for v in it {
            match v.is_fixnum().then(|| v.as_fixnum()) {
                Some(n @ 0..=255) => out.push(n as u8),
                _ => return Err(Error::new(format!("bytes: {} is not a byte (0-255)", pr_str(v, true)))),
            }
        }
        return Ok(skein_rt::string::bytes_new(&mut vm.m, &out));
    }
    Err(type_err("bytes", "a string, bytes or a collection of integers", x))
}

/// `(get coll key)` without a default.
pub fn get(coll: Value, key: Value) -> Option<Value> {
    if !coll.is_obj() {
        return None;
    }
    match coll.obj_kind() {
        Kind::Instance => skein_rt::clos::slot_get(coll, key),
        Kind::Map => coll.map_get(key),
        Kind::Set => coll.set_contains(key).then_some(key),
        Kind::Vector => index_of(key).filter(|&i| i < coll.vec_count()).map(|i| coll.vec_nth(i)),
        Kind::String => index_of(key).and_then(|i| coll.str_char_at(i)).map(Value::char),
        Kind::Bytes => index_of(key).and_then(|i| coll.as_bytes().get(i)).map(|&b| Value::fix(b as i64)),
        _ => None,
    }
}

fn nth(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    if a[0].is(Kind::Instance) {
        return collection_op(vm, "nth", a);
    }
    if a[0].is(Kind::Transient) {
        // (a transient vector reads its buffer; a map or set, as nth on one)
        use skein_rt::colls::*;
        let t = a[0];
        transient_check(vm, t)?;
        if t.word(T_KIND) != TV {
            let mut b = a.to_vec();
            b[0] = t.field(T_DATA);
            return nth(vm, &b);
        }
        let i = int(a[1], "nth")?;
        let n = t.word(T_COUNT) as i64;
        return match (i >= 0 && i < n, a.get(2)) {
            (true, _) => Ok(t.field(T_DATA).arr_get(i as usize)),
            (false, Some(&d)) => Ok(d),
            (false, None) => Err(Error::of("IndexError", format!("Index out of bounds: {i}")).with("index", crate::vm::ErrData::Int(i))),
        };
    }
    let i = int(a[1], "nth")?;
    let r = if i < 0 { None } else { seq::nth(a[0], i as usize)? };
    match (r, a.get(2)) {
        (Some(v), _) => Ok(v),
        (None, Some(&d)) => Ok(d),
        (None, None) => Err(Error::of("IndexError", format!("Index out of bounds: {i}")).with("index", crate::vm::ErrData::Int(i as i64))),
    }
}

/// `(%bind-map var value …)`: the dynamic environment with those bindings
/// added (binding's), each var marked as dynamically bound. Natively since
/// M10 part 6.1 (a Skein loop of assoc was two thirds of a binding). One
/// vector of the pairs is taken too: what binding expanded to before, which
/// the module cache may hold.
fn bind_map(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let pairs: Vec<Value> = if a.len() == 1 && a[0].is(Kind::Vector) { (0..a[0].vec_count()).map(|i| a[0].vec_nth(i)).collect() } else { a.to_vec() };
    if pairs.len() % 2 != 0 {
        return Err(Error::new("%bind-map expects var/value pairs"));
    }
    let mut m = if vm.dyn_env.is_nil() { map_empty() } else { vm.dyn_env };
    for kv in pairs.chunks(2) {
        expect(kv[0], Kind::Var, "%bind-map")?;
        kv[0].var_set_flag(skein_rt::func::VAR_DYNBOUND);
        m = map_assoc(&mut vm.m, m, kv[0], kv[1]);
    }
    Ok(m)
}

/// `(get coll key default?)`.
fn get_prim(vm: &mut Vm, a: &[Value]) -> Result<Value> { crate::owned::check(vm, a[0], false)?; if a[0].is(Kind::Transient) { return transient_get(vm, a); } if a[0].is(Kind::Sorted) { let r = if a[0].sorted_is_set() { crate::sorted::find(a[0].sorted_root(), a[1])?.map(|n| n.field(0)) } else { crate::sorted::get(a[0], a[1])? }; return Ok(r.unwrap_or(a.get(2).copied().unwrap_or(Value::NIL))); } Ok(get(a[0], a[1]).unwrap_or(a.get(2).copied().unwrap_or(Value::NIL))) }

/// `(%kw-get x key default?)`: what (get (%as-map x) key default) gives, a
/// map destructuring's key, without the map: a seq of keyword arguments
/// (a rest parameter's) is searched for key, the last pair with it winning
/// as hash-map's would, and an odd count is hash-map's error; anything
/// else is get's (M10 part 6.5).
fn kw_get_prim(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let x = a[0];
    let is_seq = x.is_cons() || (x.is_obj() && matches!(x.obj_kind(), Kind::EmptyList | Kind::VecSeq | Kind::Range));
    if !is_seq {
        return get_prim(vm, a);
    }
    let items = to_vec(vm, x)?;
    if !items.len().is_multiple_of(2) {
        return Err(Error::new("hash-map requires an even number of arguments"));
    }
    let found = items.chunks(2).rev().find(|kv| kv[0] == a[1] || equal(kv[0], a[1]));
    Ok(found.map(|kv| kv[1]).unwrap_or(a.get(2).copied().unwrap_or(Value::NIL)))
}

fn assoc(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    if a.len() % 2 != 1 {
        return Err(Error::of("TypeError", "assoc expects a collection and key/value pairs"));
    }
    if typed(a[0]) {
        return Err(typed_call(vm, "%with-typed", &WITH_TYPED, a));
    }
    let mut c = a[0];
    if c.is_nil() {
        c = map_empty();
    }
    for kv in a[1..].chunks(2) {
        c = if c.is(Kind::Instance) {
            {
                crate::owned::check(vm, c, false)?;
                skein_rt::clos::instance_with(&mut vm.m, c, kv, vm.current)?
            }
        } else if c.is(Kind::Map) {
            map_assoc(&mut vm.m, c, kv[0], kv[1])
        } else if c.is(Kind::Sorted) && !c.sorted_is_set() {
            crate::sorted::assoc(&mut vm.m, c, kv[0], kv[1])?
        } else if c.is(Kind::Vector) {
            match index_of(kv[0]).filter(|&i| i <= c.vec_count()) {
                Some(i) => vec_assoc(&mut vm.m, c, i, kv[1]),
                None => return Err(Error::of("IndexError", format!("Index out of bounds: {}", pr_str(kv[0], true))).with("index", crate::vm::ErrData::Val(kv[0]))),
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
        c = if c.is(Kind::Sorted) {
            if c.sorted_is_set() {
                crate::sorted::assoc(&mut vm.m, c, x, x)?
            } else if x.is(Kind::Vector) && x.vec_count() == 2 {
                crate::sorted::assoc(&mut vm.m, c, x.vec_nth(0), x.vec_nth(1))?
            } else if x.is_nil() {
                c
            } else {
                return Err(Error::new("conj on a sorted map expects a [key value] vector"));
            }
        } else if c.is(Kind::Queue) {
            skein_rt::colls::queue_conj(&mut vm.m, c, x)
        } else if c.is_nil() || c.is_cons() || c.is(Kind::EmptyList) {
            let tail = if c.is(Kind::EmptyList) { Value::NIL } else { c };
            skein_rt::object::cons(&mut vm.m, x, tail)
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
            skein_rt::object::cons(&mut vm.m, x, c)
        } else {
            return Err(type_err("conj", "a collection", c));
        };
    }
    Ok(c)
}

fn contains(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (c, k) = (a[0], a[1]);
    if c.is(Kind::Transient) {
        transient_check(vm, c)?;
        if c.word(skein_rt::colls::T_KIND) == skein_rt::colls::TV {
            return Ok(Value::bool(index_of(k).is_some_and(|i| i < c.word(skein_rt::colls::T_COUNT) as usize)));
        }
        return contains(vm, &[c.field(skein_rt::colls::T_DATA), k]);
    }
    Ok(Value::bool(if c.is_nil() {
        false
    } else if c.is(Kind::Map) {
        c.map_get(k).is_some()
    } else if c.is(Kind::Instance) {
        skein_rt::clos::slot_get(c, k).is_some()
    } else if c.is(Kind::Set) {
        c.set_contains(k)
    } else if c.is(Kind::Sorted) {
        crate::sorted::find(c.sorted_root(), k)?.is_some()
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
    if m.is(Kind::Instance) {
        let layout = m.instance_class().class_field(skein_rt::clos::class::LAYOUT);
        let xs: Vec<Value> = if keys { skein_rt::clos::layout_keys(layout).to_vec() } else { skein_rt::clos::instance_values(m) };
        return Ok(if xs.is_empty() { Value::NIL } else { list_from(&mut vm.m, &xs) });
    }
    if m.is(Kind::Sorted) && !m.sorted_is_set() {
        let xs: Vec<Value> = skein_rt::colls::sorted_entries(m).into_iter().map(|(k, v)| if keys { k } else { v }).collect();
        return Ok(if xs.is_empty() { Value::NIL } else { list_from(&mut vm.m, &xs) });
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

/// `(%subvec v start end)`: vector v's items start..end as a vector (the
/// indexes checked by `subvec`).
fn subvec(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let v = a[0];
    let (start, end) = (int(a[1], "subvec")?, int(a[2], "subvec")?);
    if !v.is(Kind::Vector) || start < 0 || end < start || end as usize > v.vec_count() {
        return Err(Error::of("TypeError", "subvec expects a vector and indexes in range"));
    }
    if start == 0 && end as usize == v.vec_count() {
        return Ok(v);
    }
    let items: Vec<Value> = (start as usize..end as usize).map(|i| v.vec_nth(i)).collect();
    Ok(vec_from_slice(&mut vm.m, &items))
}

fn peek(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let c = a[0];
    if c.is(Kind::Queue) {
        return Ok(skein_rt::colls::queue_peek(c));
    }
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
    if c.is(Kind::Queue) {
        return Ok(skein_rt::colls::queue_pop(&mut vm.m, c));
    }
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
    let ascii = a[0].str_is_ascii();
    let n = a[0].str_char_count();
    let start = int(a[1], "subs")?;
    let end = match a.get(2) {
        Some(&e) => int(e, "subs")?,
        None => n as i64,
    };
    if start < 0 || end < start || end as usize > n {
        let bad = if start < 0 || start as usize > n { start } else { end };
        return Err(Error::of("IndexError", format!("String index out of range: {start}..{end} of {n}")).with("index", crate::vm::ErrData::Int(bad)));
    }
    if ascii {
        let out = &s[start as usize..end as usize];
        return Ok(str_new(&mut vm.m, out));
    }
    // (in range, so both are character boundaries)
    let b0 = a[0].str_byte_of(start as usize).unwrap_or(s.len());
    let b1 = a[0].str_byte_of(end as usize).unwrap_or(s.len());
    let out = s[b0..b1].to_owned();
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
        return Err(type_err("disassemble", "a Skein function", f));
    }
    let mut s = String::new();
    for &p in f.closure_template().template_arities().arr_slice() {
        s.push_str(&crate::bytecode::disassemble_proto(p));
    }
    out(&s, false)
}

fn scope_result(_: &mut Vm, a: &[Value]) -> Result<Value> {
    expect(a[0], Kind::Scope, "%scope-result")?;
    let failed = a[0].field(skein_rt::conc::scope::FAILED);
    if !failed.is_nil() {
        return Err(crate::sched::error_from_fiber(failed));
    }
    crate::sched::fiber_outcome(a[1])
}

static DEREF_OBJECT: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

/// `(deref x)`, `(deref x ms timeout-value)`: an atom's, var's or fiber's
/// value here; an object's (a delay, promise or future, or a class of one's
/// own) from the generic function deref-object, called in this primitive's
/// place so it can park (M9 part 9.14).
fn deref(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let x = a[0];
    if x.is(Kind::Instance) {
        let var = vm.core_fn("deref-object", &DEREF_OBJECT);
        let f = if var.is(Kind::Var) { var.var_root() } else { var };
        let (ms, tv) = (a.get(1).copied().unwrap_or(Value::NIL), a.get(2).copied().unwrap_or(Value::NIL));
        let args = vec_from_slice(&mut vm.m, &[x, ms, tv]);
        return Err(Error::call(f, args));
    }
    if a.len() > 1 {
        return Err(Error::of("TypeError", format!("deref with a timeout needs a promise or a future, got {}", pr_str(x, true)))
            .with("expected", crate::vm::ErrData::Str("a promise or a future".into()))
            .with("value", crate::vm::ErrData::Val(x)));
    }
    if x.is(Kind::Atom) {
        Ok(x.atom_get())
    } else if x.is(Kind::Var) {
        Ok(x.var_root())
    } else if x.is(Kind::Fiber) {
        crate::conc::await_fiber(vm, x)
    } else {
        Err(type_err("deref", "an atom, var, fiber, delay, promise or future", x))
    }
}

fn fiber_stats(vm: &mut Vm, _: &[Value]) -> Result<Value> {
    use std::sync::atomic::Ordering;
    let s = crate::sched::sched();
    let spawned = s.spawned.load(Ordering::Relaxed) as i64;
    let finished = s.finished.load(Ordering::Relaxed) as i64;
    let k = |vm: &mut Vm, n: &str| rt().keyword(&mut vm.m, None, n);
    let kvs = [
        k(vm, "spawned"),
        Value::fix(spawned),
        k(vm, "finished"),
        Value::fix(finished),
        k(vm, "live"),
        Value::fix(spawned - finished),
    ];
    hash_map(vm, &kvs)
}

// ---- CLOS support ----------------------------------------------------------------------

fn class_field(vm: &mut Vm, k: Value) -> Result<usize> {
    use skein_rt::clos::class::*;
    let name = if k.is_keyword() { k.sym_name().to_owned() } else { String::new() };
    let _ = vm;
    Ok(match name.as_str() {
        "name" => NAME,
        "supers" => SUPERS,
        "cpl" => CPL,
        "slots" => SLOTS,
        "layout" => LAYOUT,
        "policy" => POLICY,
        "meta" => META,
        "subclasses" => SUBCLASSES,
        "metaclass" => METACLASS,
        _ => return Err(type_err("%class-get", "a class field keyword", k)),
    })
}

fn class_set(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    use skein_rt::clos::class::*;
    expect(a[0], Kind::Class, "%class-set!")?;
    let f = class_field(vm, a[1])?;
    if f == LAYOUT {
        return Err(Error::new("use %class-set-layout! to change a class's layout"));
    }
    a[0].class_set_field(f, a[2]);
    Ok(a[2])
}

static STANDARD_OBJECT: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
static K_INITARG: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
static K_INITVALUE: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
static K_INITFORM: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
static K_REQUIRED: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

/// `(%make-fast c initargs)`: an instance of standard class c from :initarg
/// value pairs (a seq), natively, when that's plain (M10 part 6.2): every
/// pair names a slot (the last one given wins), and each slot not given has
/// a constant default (`:initvalue`, from a literal :initform) or none and
/// isn't required. nil otherwise, and %make-from does it: it calls
/// initforms in the fiber (so their conditions reach its handlers) and says
/// what's wrong.
fn make_fast(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    use skein_rt::clos::class::*;
    let c = a[0];
    if !c.is(Kind::Class) || c.word(BUILTIN) != NOT_BUILTIN {
        return Ok(Value::NIL);
    }
    let so = vm.core_fn("StandardObject", &STANDARD_OBJECT);
    let so = if so.is(Kind::Var) { so.var_root() } else { so };
    let cpl = c.class_field(CPL);
    if !(0..cpl.vec_count()).any(|i| cpl.vec_nth(i) == so) {
        return Ok(Value::NIL);
    }
    let args = to_vec(vm, a[1])?;
    if args.len() % 2 != 0 {
        return Ok(Value::NIL);
    }
    let kw = |vm: &mut Vm, cell: &'static std::sync::OnceLock<Value>, name: &str| *cell.get_or_init(|| rt().keyword(&mut vm.m, None, name));
    let (k_initarg, k_initvalue) = (kw(vm, &K_INITARG, "initarg"), kw(vm, &K_INITVALUE, "initvalue"));
    let (k_initform, k_required) = (kw(vm, &K_INITFORM, "initform"), kw(vm, &K_REQUIRED, "required"));
    let slots = c.class_field(SLOTS);
    let n = slots.vec_count();
    if !slots.is(Kind::Vector) || skein_rt::clos::layout_len(c.class_field(LAYOUT)) != n {
        return Ok(Value::NIL);
    }
    let initargs: Vec<Value> = (0..n).map(|i| get(slots.vec_nth(i), k_initarg).unwrap_or(Value::NIL)).collect();
    if args.chunks(2).any(|kv| kv[0].is_nil() || !initargs.contains(&kv[0])) {
        return Ok(Value::NIL);
    }
    let mut vals = Vec::with_capacity(n);
    for (i, &ia) in initargs.iter().enumerate() {
        if !ia.is_nil()
            && let Some(kv) = args.chunks(2).rev().find(|kv| kv[0] == ia)
        {
            vals.push(kv[1]);
            continue;
        }
        let sd = slots.vec_nth(i);
        if let Some(v) = get(sd, k_initvalue) {
            vals.push(v);
        } else if get(sd, k_initform).is_some() || get(sd, k_required).is_some_and(|r| !r.is_nil() && r != Value::FALSE) {
            return Ok(Value::NIL);
        } else {
            vals.push(Value::NIL);
        }
    }
    Ok(skein_rt::clos::instance_new(&mut vm.m, c, &vals, vm.current))
}

fn instance_new(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    expect(a[0], Kind::Class, "%instance")?;
    let vals = to_vec(vm, a[1])?;
    let n = skein_rt::clos::layout_len(a[0].class_field(skein_rt::clos::class::LAYOUT));
    if vals.len() != n {
        return Err(Error::new(format!("%instance: expected {n} slot values, got {}", vals.len())));
    }
    Ok(skein_rt::clos::instance_new(&mut vm.m, a[0], &vals, vm.current))
}

/// `(%slot-ref inst i)`: slot i of the class's current layout, without
/// looking its name up (for hot paths like skein.syntax's accessors).
fn slot_ref(vm: &mut Vm, inst: Value, i: Value) -> Result<Value> {
    expect(inst, Kind::Instance, "%slot-ref")?;
    let i = int(i, "%slot-ref")? as usize;
    let current = inst.instance_class().class_field(skein_rt::clos::class::LAYOUT);
    if i >= skein_rt::clos::layout_len(current) {
        return Err(Error::new(format!("%slot-ref: no slot {i}")));
    }
    if inst.instance_layout() == current {
        crate::owned::check(vm, inst, false)?;
        return Ok(inst.instance_raw(i));
    }
    // (an instance from before the class was redefined)
    slot_value(vm, inst, skein_rt::clos::layout_keys(current)[i])
}

static SLOT_VALUE_USING_CLASS: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
static WITH_TYPED: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
static SET_SLOT_TYPED: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

/// Is x an instance of a class with a slot :type to check?
#[inline]
fn typed(x: Value) -> bool {
    x.is(Kind::Instance) && skein_rt::clos::class_flag(x.instance_class(), skein_rt::clos::class::FLAG_TYPED)
}

/// Core's function `name` called in the primitive's place with its
/// arguments (to check slot types, in Skein, before doing what it does).
fn typed_call(vm: &mut Vm, name: &str, cell: &'static std::sync::OnceLock<Value>, a: &[Value]) -> Error {
    let var = vm.core_fn(name, cell);
    let f = if var.is(Kind::Var) { var.var_root() } else { var };
    let args = vec_from_slice(&mut vm.m, a);
    Error::call(f, args)
}

/// `(with inst :k v …)`, the slots' types checked already (or none).
fn with_raw(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    if a.len() % 2 != 1 {
        return Err(Error::of("TypeError", "with expects an instance and slot/value pairs"));
    }
    crate::owned::check(vm, a[0], false)?;
    Ok(skein_rt::clos::instance_with(&mut vm.m, a[0], &a[1..], vm.current)?)
}

/// `(slot-value inst k)`; for a class whose metaclass hooks it (M9 part
/// 9.5), slot-value-using-class instead, called in this primitive's place.
fn slot_value_prim(vm: &mut Vm, inst: Value, k: Value) -> Result<Value> {
    if inst.is(Kind::Instance) {
        let c = inst.instance_class();
        if skein_rt::clos::class_hooked(c, skein_rt::clos::class::HOOK_SLOT) {
            let var = vm.core_fn("slot-value-using-class", &SLOT_VALUE_USING_CLASS);
            let f = if var.is(Kind::Var) { var.var_root() } else { var };
            let args = vec_from_slice(&mut vm.m, &[c, inst, k]);
            return Err(Error::call(f, args));
        }
    }
    slot_value(vm, inst, k)
}

fn slot_value(vm: &mut Vm, inst: Value, k: Value) -> Result<Value> {
    expect(inst, Kind::Instance, "slot-value")?;
    crate::owned::check(vm, inst, false)?;
    skein_rt::clos::slot_get(inst, k).ok_or_else(|| {
        Error::new(format!("{} has no slot {}", pr_str(inst.instance_class().class_field(skein_rt::clos::class::NAME), true), pr_str(k, true)))
    })
}

fn gf_field(k: Value) -> Result<usize> {
    use skein_rt::clos::generic::*;
    Ok(match if k.is_keyword() { k.sym_name() } else { "" } {
        "name" => NAME,
        "methods" => METHODS,
        "dispatch" => DISPATCH,
        "combination" => COMBINATION,
        "meta" => META,
        "nreq" => NREQ,
        "rest" => REST,
        "class" => CLASS,
        _ => return Err(type_err("%gf-get", "a generic-function field keyword", k)),
    })
}

fn gf_get(_: &mut Vm, a: &[Value]) -> Result<Value> {
    use skein_rt::clos::generic::*;
    expect(a[0], Kind::Generic, "%gf-get")?;
    let f = gf_field(a[1])?;
    Ok(match f {
        NREQ => Value::fix(a[0].gf_nreq() as i64),
        REST => Value::bool(a[0].gf_rest()),
        _ => Value(a[0].atomic_word(f).load(std::sync::atomic::Ordering::Acquire)),
    })
}

fn gf_set(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    use skein_rt::clos::generic::*;
    expect(a[0], Kind::Generic, "%gf-set!")?;
    let f = gf_field(a[1])?;
    if !matches!(f, DISPATCH | COMBINATION | META | CLASS) {
        return Err(Error::new("%gf-set! can only change :dispatch, :combination, :meta or :class"));
    }
    a[0].atomic_word(f).store(a[2].0, std::sync::atomic::Ordering::Release);
    // Cached effective methods were built under the old options.
    let methods = a[0].field(METHODS);
    let state = a[0].gf_state();
    let mask = state.arr_get(S_MASK).as_fixnum() as u64;
    let _ = skein_rt::clos::gf_set_methods(&mut vm.m, a[0], methods, methods, mask, state.arr_get(S_EQLS));
    Ok(a[2])
}

fn gf_cache_get(_: &mut Vm, a: &[Value]) -> Result<Value> {
    use skein_rt::clos::generic::*;
    expect(a[0], Kind::Generic, "%gf-cache-get")?;
    let state = a[0].gf_state();
    if state.arr_get(S_CLASS_EPOCH).as_fixnum() as u64 != skein_rt::clos::class_epoch() {
        return Ok(Value::NIL);
    }
    let key: Vec<Value> = if a[1].is(Kind::Vector) { (0..a[1].vec_count()).map(|i| a[1].vec_nth(i)).collect() } else { vec![a[1]] };
    Ok(skein_rt::clos::cache_lookup(state.arr_get(S_CACHE), &key).unwrap_or(Value::NIL))
}

fn gf_cache_put(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    expect(a[0], Kind::Generic, "%gf-cache-put!")?;
    let tok = a[1];
    if !tok.is(Kind::Vector) || tok.vec_count() != 2 {
        return Err(Error::new("%gf-cache-put!: bad token"));
    }
    let key: Vec<Value> = (0..a[2].vec_count()).map(|i| a[2].vec_nth(i)).collect();
    let (e, ce) = (tok.vec_nth(0).as_fixnum() as u64, tok.vec_nth(1).as_fixnum() as u64);
    Ok(Value::bool(skein_rt::clos::cache_put(&mut vm.m, a[0], e, ce, &key, a[3])))
}

// ---- conditions and the debugger ---------------------------------------------------

/// `(%nested-raise msg trace cls data cond)`: the error `%runtime-error`
/// was given, which no handler inside its nested run took: the run fails
/// with it, for the handlers outside (vm.rs's `handle_error`).
fn nested_raise(vm: &mut Vm, a: &[Value]) -> crate::vm::Error {
    use crate::vm::{ErrClass, ErrKind, Error};
    let msg = pr_str(a[0], false);
    let mut trace = Vec::new();
    if a[1].is(Kind::Vector) {
        a[1].vec_for_each(|t| {
            trace.push(pr_str(t, false));
            true
        });
    }
    let class = if !a[4].is_nil() {
        Some(ErrClass::Value(a[4]))
    } else if !a[2].is_nil() {
        Some(ErrClass::Value(vec_from_slice(&mut vm.m, &[a[0], a[2], a[3]])))
    } else {
        None
    };
    Error { msg, trace, kind: ErrKind::Declined, class }
}

fn register_frame_kinds(_: &mut Vm, a: &[Value]) -> Result<Value> {
    for &f in a {
        expect(f, Kind::Closure, "%register-frame-kinds!")?;
    }
    let _ = crate::vm::FRAME_KINDS.set(crate::vm::FrameKinds {
        catch: a[0],
        unwind: a[1],
        bind: a[2],
        finish: a[3],
        runtime_error: a[4],
    });
    Ok(Value::NIL)
}

/// `(%frames fiber)`: the stack of a parked fiber, top first, as maps
/// `{:fn name :id [segment offset] :locals {sym value}}`.
fn frames(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    use skein_rt::conc::fiber;
    use skein_rt::func::{F_FN, F_PROTO, HDR};
    let f = a[0];
    expect(f, Kind::Fiber, "%frames")?;
    if f.fiber_state().load(std::sync::atomic::Ordering::Acquire) != fiber::PARKED {
        return Err(Error::new("%frames needs a parked fiber"));
    }
    let k = |vm: &mut Vm, n: &str| rt().keyword(&mut vm.m, None, n);
    let (kfn, kid, kloc, kinl) = (k(vm, "fn"), k(vm, "id"), k(vm, "locals"), k(vm, "inlined"));
    let mut out = Vec::new();
    for (seg, fp) in crate::vm::walk_frames(f.field(fiber::SEG), f.field(fiber::FP).as_fixnum() as usize) {
        // SAFETY: walk_frames yields live frames of a parked fiber.
        let (func, proto, regs, resume) = unsafe {
            let fr = seg.seg_data().add(fp);
            (*fr.add(F_FN), *fr.add(F_PROTO), fr.add(HDR), *fr.add(skein_rt::func::F_RESUME))
        };
        // (each call inlined where it is, as a frame of its own: its
        // locals, the frame's id, and :inlined true)
        for (k, n, dbg) in crate::vm::inlined_at_k(proto, crate::vm::frame_pc(resume), regs) {
            let mut locals = map_empty();
            let pairs: Vec<Value> = (0..dbg.vec_count()).map(|i| dbg.vec_nth(i)).collect();
            for pair in pairs.chunks(2) {
                let r = pair[1].as_fixnum() as usize;
                if r < proto.proto_nregs() {
                    // SAFETY: r is a register of this frame.
                    let v = unsafe { *regs.add(r) };
                    locals = map_assoc(&mut vm.m, locals, pair[0], v);
                }
            }
            // (its id: the frame's, and which inlined call)
            let id = vec_from_slice(&mut vm.m, &[seg, Value::fix(fp as i64), Value::fix(k as i64)]);
            let m = hash_map(vm, &[kfn, n, kid, id, kloc, locals, kinl, Value::TRUE])?;
            out.push(m);
        }
        let name = proto.proto_name();
        let name = if name.is_nil() { str_new(&mut vm.m, "fn") } else { name };
        let mut locals = map_empty();
        let dbg = proto.proto_locals();
        if dbg.is(Kind::Array) {
            for pair in dbg.arr_slice().chunks(2) {
                let r = pair[1].as_fixnum() as usize;
                if r < proto.proto_nregs() {
                    // SAFETY: r is a register of this frame.
                    let v = unsafe { *regs.add(r) };
                    locals = map_assoc(&mut vm.m, locals, pair[0], v);
                }
            }
        }
        let id = vec_from_slice(&mut vm.m, &[seg, Value::fix(fp as i64)]);
        let _ = func;
        let m = hash_map(vm, &[kfn, name, kid, id, kloc, locals])?;
        out.push(m);
    }
    Ok(vec_from_slice(&mut vm.m, &out))
}

// ---- compiled code as data ---------------------------------------------------------

fn make_proto(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let nfixed = int(a[1], "%make-proto")? as usize;
    let nregs = int(a[3], "%make-proto")? as usize;
    let consts = to_vec(vm, a[4])?;
    let code: Vec<u32> = to_vec(vm, a[5])?
        .iter()
        .map(|&i| if i.is_fixnum() && (0..=u32::MAX as i64).contains(&i.as_fixnum()) { Ok(i.as_fixnum() as u32) } else { Err(Error::new("%make-proto: bad instruction")) })
        .collect::<Result<_>>()?;
    let locals = to_vec(vm, a[6])?;
    if nregs > 255 || nfixed > nregs {
        return Err(Error::new("%make-proto: bad register count"));
    }
    let m = &mut vm.m;
    let info = skein_rt::func::ProtoInfo {
        name: a[0],
        nfixed,
        variadic: a[2].truthy(),
        nregs,
        consts: skein_rt::array::array_from(m, &consts),
        code: skein_rt::func::code_new(m, &code),
        locals: skein_rt::array::array_from(m, &locals),
    };
    Ok(skein_rt::func::proto_new(m, info))
}

fn template_parts(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    expect(a[0], Kind::FnTemplate, "%template-parts")?;
    let t = a[0];
    let k = |vm: &mut Vm, n: &str| rt().keyword(&mut vm.m, None, n);
    let arities = vec_from_slice(&mut vm.m, t.template_arities().arr_slice());
    let caps = vec_from_slice(&mut vm.m, t.template_captures().arr_slice());
    let kvs = [k(vm, "name"), t.template_name(), k(vm, "arities"), arities, k(vm, "captures"), caps];
    hash_map(vm, &kvs)
}

fn proto_parts(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    expect(a[0], Kind::Proto, "%proto-parts")?;
    let p = a[0];
    let code = p.proto_code();
    // SAFETY: code_len instructions follow the length word.
    let ins: Vec<Value> = (0..code.code_len()).map(|i| Value::fix(unsafe { *code.code_ptr().add(i) } as i64)).collect();
    let k = |vm: &mut Vm, n: &str| rt().keyword(&mut vm.m, None, n);
    let code_v = vec_from_slice(&mut vm.m, &ins);
    let consts = vec_from_slice(&mut vm.m, p.proto_consts().arr_slice());
    let locals = vec_from_slice(&mut vm.m, p.proto_locals().arr_slice());
    let kvs = [
        k(vm, "name"), p.proto_name(),
        k(vm, "nfixed"), Value::fix(p.proto_nfixed() as i64),
        k(vm, "variadic"), Value::bool(p.proto_variadic()),
        k(vm, "nregs"), Value::fix(p.proto_nregs() as i64),
        k(vm, "consts"), consts,
        k(vm, "code"), code_v,
        k(vm, "locals"), locals,
        // (compiled to tier 1 and installed: what tests of optimised code
        // check they are testing)
        k(vm, "native"), Value::bool(p.proto_native() >= crate::jit::MIN_ENTRY),
    ];
    hash_map(vm, &kvs)
}

/// `(%fn-vars f)`: the vars function f's code refers to (its constants,
/// and its nested functions'), each once, in the order first met: what
/// skein.xref's callees are made of. Anything but a closure has none.
fn fn_vars(vm: &mut Vm, f: Value) -> Value {
    fn_consts(vm, f, Kind::Var, true)
}

/// `(%fn-symbols f)`: the symbols function f's code has as constants (quoted
/// names), nested functions' included, in the order first met, a vector.
/// skein.shake reads the libraries code loads lazily from them.
fn fn_symbols(vm: &mut Vm, f: Value) -> Value {
    fn_consts(vm, f, Kind::Symbol, false)
}

/// The constants of kind `want` in function f's prototypes, nested ones'
/// included, each once. `source`: only those the function's own code uses,
/// not code inlined into it from functions it calls (M10 part 4), nor an
/// inlined call's guard (what the source refers to, for skein.xref).
fn fn_consts(vm: &mut Vm, f: Value, want: Kind, source: bool) -> Value {
    fn walk(x: Value, want: Kind, source: bool, seen: &mut Vec<Value>, out: &mut Vec<Value>, depth: usize) {
        if depth > 64 || seen.iter().any(|s| s.0 == x.0) {
            return;
        }
        if x.is(Kind::Closure) {
            seen.push(x);
            walk(x.closure_template(), want, source, seen, out, depth + 1);
        } else if x.is(Kind::FnTemplate) {
            seen.push(x);
            let ar = x.template_arities();
            if ar.is(Kind::Array) {
                for i in 0..ar.arr_len() {
                    walk(ar.arr_get(i), want, source, seen, out, depth + 1);
                }
            }
        } else if x.is(Kind::Proto) {
            seen.push(x);
            let k = x.proto_consts();
            // (with source: the constants an instruction outside inlined
            // code names, a guard's not counted)
            let used: Option<Vec<bool>> = source.then(|| {
                use crate::bytecode::{CLOSURE, DEFVAR, GETVAR, LOADK, bx, op};
                let code = x.proto_code();
                let mut used = vec![false; if k.is(Kind::Array) { k.arr_len() } else { 0 }];
                // SAFETY: a code object holds code_len instructions.
                let ins = unsafe { std::slice::from_raw_parts(code.code_ptr(), code.code_len()) };
                for (pc, &i) in ins.iter().enumerate() {
                    if matches!(op(i), LOADK | GETVAR | DEFVAR | CLOSURE)
                        && !crate::vm::in_inlined(x, pc)
                        && let Some(u) = used.get_mut(bx(i))
                    {
                        *u = true;
                    }
                }
                used
            });
            if k.is(Kind::Array) {
                for i in 0..k.arr_len() {
                    if used.as_ref().is_some_and(|u| !u[i]) {
                        continue;
                    }
                    let c = k.arr_get(i);
                    if c.is(want) {
                        if !out.iter().any(|o| o.0 == c.0) {
                            out.push(c);
                        }
                    } else {
                        walk(c, want, source, seen, out, depth + 1);
                    }
                }
            }
        }
    }
    let mut seen = Vec::new();
    let mut out = Vec::new();
    walk(f, want, source, &mut seen, &mut out, 0);
    vec_from_slice(&mut vm.m, &out)
}

/// `(%fn-captured f)`: the values closure f captured (its upvalues), a
/// vector; empty for anything else. skein.xref looks in them for vars and
/// functions, as a contract's checking wrapper holds the var it checks.
fn fn_captured(vm: &mut Vm, f: Value) -> Value {
    let ups: Vec<Value> = if f.is(Kind::Closure) { (0..f.closure_nups()).map(|i| f.closure_upval(i)).collect() } else { Vec::new() };
    vec_from_slice(&mut vm.m, &ups)
}
