//! Primitives: Rust leaf functions bound to vars in norn.core.
//!
//! Rule (runtime.md §2.3): primitives never call Norn code. The exceptions
//! are the meta-level ones (`eval`, `load-file`, `macroexpand`), which
//! re-enter the VM through `Vm::apply` on a fresh stack: a continuation
//! barrier, acceptable for the seed system.
//!
//! Primitives are identified by *index* into `PRIMS`, not by function
//! pointer, so images can save and restore them (runtime.md §7).

use crate::vm::{Error, Result, Vm};
use norn_rt::equal::{equal, hash};
use norn_rt::func::{VAR_MACRO, primitive_new};
use norn_rt::list::{list_from, range_new};
use norn_rt::map::{map_assoc, map_dissoc, map_empty, set_conj, set_disj, set_empty};
use norn_rt::print::{pr_str, write_value};
use norn_rt::runtime::rt;
use norn_rt::seq;
use norn_rt::string::{float_new, str_new};
use norn_rt::vector::{vec_assoc, vec_conj, vec_empty, vec_from_slice, vec_pop};
use norn_rt::{Kind, Value};
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
    "%restart-frame" 2 Some(2) => |_, _| Err(Error::new("%restart-frame is only possible inside a fiber"));
    "%abort-fiber" 2 Some(2) => |_, a| Err(Error::fatal(pr_str(a[0], false), vec![]));
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
    // type predicates
    "nil?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_nil()));
    "number?" 1 Some(1) => |_, a| Ok(Value::bool(norn_rt::numbers::is_number(a[0])));
    "integer?" 1 Some(1) => |_, a| Ok(Value::bool(norn_rt::numbers::is_integer(a[0])));
    "ratio?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Ratio)));
    "rational?" 1 Some(1) => |_, a| Ok(Value::bool(norn_rt::numbers::is_integer(a[0]) || a[0].is(Kind::Ratio)));
    "float?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_float()));
    "string?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::String)));
    "symbol?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_symbol()));
    "keyword?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_keyword()));
    "char?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_char()));
    "boolean?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_bool()));
    "fn?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Closure) || a[0].is(Kind::Primitive)));
    "list?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is_cons() || a[0].is(Kind::EmptyList)));
    "vector?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Vector)));
    "map?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Map) || (a[0].is(Kind::Sorted) && !a[0].sorted_is_set())));
    "set?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Set) || (a[0].is(Kind::Sorted) && a[0].sorted_is_set())));
    "sorted?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Sorted)));
    "queue?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Queue)));
    "transient?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Transient)));
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
    "get" 2 Some(3) => |vm, a| { crate::owned::check(vm, a[0], false)?; if a[0].is(Kind::Sorted) { let r = if a[0].sorted_is_set() { crate::sorted::find(a[0].sorted_root(), a[1])?.map(|_| a[1]) } else { crate::sorted::get(a[0], a[1])? }; return Ok(r.unwrap_or(a.get(2).copied().unwrap_or(Value::NIL))); } Ok(get(a[0], a[1]).unwrap_or(a.get(2).copied().unwrap_or(Value::NIL))) };
    // sorted maps and sets, queues, transients (colls.rs, sorted.rs)
    "sorted-map" 0 VAR => |vm, a| { if a.len() % 2 != 0 { return Err(Error::new("sorted-map expects key/value pairs")); } let mut s = norn_rt::colls::sorted_new(&mut vm.m, 0, Value::NIL, false); for kv in a.chunks(2) { s = crate::sorted::assoc(&mut vm.m, s, kv[0], kv[1])?; } Ok(s) };
    "sorted-set" 0 VAR => |vm, a| { let mut s = norn_rt::colls::sorted_new(&mut vm.m, 0, Value::NIL, true); for &k in a { s = crate::sorted::assoc(&mut vm.m, s, k, k)?; } Ok(s) };
    "subseq" 3 Some(5) => |vm, a| sorted_range(vm, a, false);
    "rsubseq" 3 Some(5) => |vm, a| sorted_range(vm, a, true);
    "%empty-queue" 0 Some(0) => |vm, _| { let e = rt().empty_vector; Ok(norn_rt::colls::queue_new(&mut vm.m, 0, Value::NIL, e)) };
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
    "empty" 1 Some(1) => |vm, a| Ok(match a[0] { c if c.is(Kind::Sorted) => norn_rt::colls::sorted_new(&mut vm.m, 0, Value::NIL, c.sorted_is_set()), c if c.is(Kind::Queue) => { let e = rt().empty_vector; norn_rt::colls::queue_new(&mut vm.m, 0, Value::NIL, e) } c => empty_of(c) });
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
    "pr-str" 0 VAR => |vm, a| { forced(a)?; Ok(str_new(&mut vm.m, &join(a, true))) };
    "print" 0 VAR => |_, a| { forced(a)?; out(&join(a, false), false) };
    "println" 0 VAR => |_, a| { forced(a)?; out(&join(a, false), true) };
    "pr" 0 VAR => |_, a| { forced(a)?; out(&join(a, true), false) };
    "prn" 0 VAR => |_, a| { forced(a)?; out(&join(a, true), true) };
    // streams (design §3.3) and reduced (early exit from reduce)
    "%stream" 1 Some(1) => |vm, a| Ok(seq::stream_new(&mut vm.m, a[0]));
    "%stream-thunk" 1 Some(1) => |_, a| { expect(a[0], Kind::Stream, "%stream-thunk")?; Ok(if a[0].stream_realized() { Value::NIL } else { a[0].stream_value() }) };
    "%stream-set!" 2 Some(2) => |_, a| { expect(a[0], Kind::Stream, "%stream-set!")?; Ok(seq::stream_set(a[0], a[1])) };
    "%first-unrealized" 1 Some(1) => |_, a| Ok(seq::first_unrealized(a[0]).unwrap_or(Value::NIL));
    "stream?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Stream)));
    "realized?" 1 Some(1) => |_, a| Ok(Value::bool(!a[0].is(Kind::Stream) || a[0].stream_realized()));
    "reduced" 1 Some(1) => |vm, a| { let r = norn_rt::object::alloc(&mut vm.m, Kind::Reduced, 1); r.set_field(0, a[0]); Ok(r) };
    "reduced?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Reduced)));
    "unreduced" 1 Some(1) => |_, a| Ok(if a[0].is(Kind::Reduced) { a[0].field(0) } else { a[0] });
    "slurp" 1 Some(1) => |vm, a| crate::sys::read(vm, text(a[0]), true);
    "spit" 2 Some(2) => |vm, a| crate::sys::write(vm, text(a[0]), text(a[1]).into_bytes(), false);
    "read-string" 1 Some(1) => |vm, a| { let s = text(a[0]); let mut r = crate::reader::Reader::new(&s); match r.read(vm) { Ok(v) => Ok(v), Err(crate::reader::ReadError::Eof) => Ok(Value::NIL), Err(e) => Err(Error::new(e.to_string())) } };
    // evaluation and namespaces
    "macroexpand-1" 1 Some(1) => |vm, a| Ok(vm.macroexpand_1(a[0])?.0);
    "macroexpand" 1 Some(1) => |vm, a| vm.macroexpand(a[0]);
    "%reader" 2 Some(2) => |vm, a| { expect(a[0], Kind::String, "%reader")?; Ok(norn_rt::conc::reader_new(&mut vm.m, a[0], a[1])) };
    "%read" 1 Some(1) => |vm, a| { expect(a[0], Kind::Reader, "%read")?; crate::eval::read_next(vm, a[0]) };
    "%eof?" 1 Some(1) => |_, a| Ok(Value::bool(a[0] == Value::EOF));
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
    "heap-bytes" 0 Some(0) => |_, _| Ok(Value::fix(norn_rt::heap::heap_bytes() as i64));
    "gc" 0 Some(0) => |vm, _| { vm.want_gc = true; norn_rt::safepoint::bump_attention(); Ok(Value::NIL) };
    "gc-count" 0 Some(0) => |_, _| Ok(Value::fix(norn_rt::heap::gc_count() as i64));
    // fibers, scopes, channels, atoms
    "%spawn" 1 Some(1) => |vm, a| Ok(crate::sched::spawn(vm, a[0], Value::NIL));
    "%scope" 0 Some(0) => |vm, _| Ok(norn_rt::conc::scope_new(&mut vm.m));
    "%scope-spawn" 2 Some(2) => |vm, a| { expect(a[0], Kind::Scope, "spawn")?; Ok(crate::sched::spawn(vm, a[1], a[0])) };
    "%scope-join" 1 Some(1) => |vm, a| { expect(a[0], Kind::Scope, "%scope-join")?; crate::sched::scope_join(vm, a[0]) };
    "%scope-result" 2 Some(2) => scope_result;
    "await" 1 Some(1) => |vm, a| crate::conc::await_fiber(vm, a[0]);
    "%join" 1 Some(1) => |vm, a| crate::conc::wait_fiber(vm, a[0], false);
    "fiber-error" 1 Some(1) => |_, a| { expect(a[0], Kind::Fiber, "fiber-error")?; Ok(a[0].field(norn_rt::conc::fiber::ERROR)) };
    "yield" 0 Some(0) => |vm, _| { if vm.can_park() { vm.suspend = Some(crate::vm::Suspend::Yield); } Ok(Value::NIL) };
    "sleep" 1 Some(1) => |vm, a| { let ms = int(a[0], "sleep")?; crate::sched::sleep(vm, ms.max(0) as u64) };
    "current-fiber" 0 Some(0) => |vm, _| Ok(vm.current);
    "fiber?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Fiber)));
    "done?" 1 Some(1) => |_, a| { expect(a[0], Kind::Fiber, "done?")?; Ok(Value::bool(a[0].fiber_state().load(std::sync::atomic::Ordering::Acquire) == norn_rt::conc::fiber::DONE)) };
    "cancel!" 1 Some(1) => |_, a| { expect(a[0], Kind::Fiber, "cancel!")?; crate::sched::cancel(a[0], norn_rt::conc::fiber::CANCELLED); Ok(Value::NIL) };
    "chan" 0 Some(1) => |vm, a| { let n = match a.first() { Some(&v) => int(v, "chan")?.max(0) as usize, None => 0 }; Ok(norn_rt::conc::chan_new(&mut vm.m, n)) };
    "put!" 2 Some(2) => |vm, a| crate::conc::put(vm, a[0], a[1]);
    "take!" 1 Some(1) => |vm, a| crate::conc::take(vm, a[0]);
    "offer!" 2 Some(2) => |vm, a| crate::conc::offer(vm, a[0], a[1]);
    "poll!" 1 Some(1) => |vm, a| crate::conc::poll(vm, a[0]);
    "close!" 1 Some(1) => |_, a| { if !a[0].is(Kind::Chan) { return Err(type_err("close!", "a channel", a[0])); } crate::conc::close(a[0]); Ok(Value::NIL) };
    "closed?" 1 Some(1) => |_, a| crate::conc::closed(a[0]);
    "alts!" 1 VAR => crate::conc::alts;
    "timeout" 1 Some(1) => |vm, a| { let ms = int(a[0], "timeout")?; Ok(crate::sched::timeout(vm, ms.max(0) as u64)) };
    "atom" 1 Some(1) => |vm, a| Ok(norn_rt::conc::atom_new(&mut vm.m, a[0]));
    "deref" 1 Some(1) => deref;
    "reset!" 2 Some(2) => |_, a| { expect(a[0], Kind::Atom, "reset!")?; a[0].atom_set(a[1]); Ok(a[1]) };
    "compare-and-set!" 3 Some(3) => |_, a| { expect(a[0], Kind::Atom, "compare-and-set!")?; Ok(Value::bool(a[0].atom_cas(a[1], a[2]))) };
    "carrier-count" 0 Some(0) => |_, _| Ok(Value::fix(crate::sched::sched().n as i64));
    "fiber-stats" 0 Some(0) => fiber_stats;
    // conditions, dynamic environment, debugger support
    "%dyn" 0 Some(0) => |vm, _| Ok(vm.dyn_env);
    "%set-dyn!" 1 Some(1) => |vm, a| { vm.dyn_env = a[0]; Ok(a[0]) };
    "%mark-dynamic!" 1 Some(1) => |_, a| { expect(a[0], Kind::Var, "%mark-dynamic!")?; a[0].var_set_flag(norn_rt::func::VAR_DYNBOUND); Ok(a[0]) };
    "%register-frame-kinds!" 5 Some(5) => register_frame_kinds;
    "%retry-marker" 0 Some(0) => |_, _| Ok(Value::RETRY);
    "%die" 2 Some(2) => |vm, a| { let lines = to_vec(vm, a[1]).unwrap_or_default().iter().map(|&t| pr_str(t, false)).collect(); Err(Error::fatal(pr_str(a[0], false), lines)) };
    "%message" 0 VAR => |vm, a| Ok(str_new(&mut vm.m, &join(a, false)));
    "%frames" 1 Some(1) => frames;
    "%post-event" 1 Some(1) => |_, a| { crate::sched::post_event(text(a[0])); Ok(Value::NIL) };
    "fiber-id" 1 Some(1) => |_, a| { expect(a[0], Kind::Fiber, "fiber-id")?; Ok(Value::fix(a[0].fiber_id() as i64)) };
    "%location" 0 Some(0) => |vm, _| Ok(if vm.current.is_nil() { Value::NIL } else { vm.current.field(norn_rt::conc::fiber::LOC) });
    "%set-location!" 1 Some(1) => |vm, a| { if !vm.current.is_nil() { vm.current.set_field(norn_rt::conc::fiber::LOC, a[0]); } Ok(a[0]) };
    "%reader-line" 1 Some(1) => |_, a| { expect(a[0], Kind::Reader, "%reader-line")?; Ok(Value::fix(a[0].word(3) as i64)) };
    "%reader-name" 1 Some(1) => |_, a| { expect(a[0], Kind::Reader, "%reader-name")?; Ok(a[0].field(2)) };
    // building and inspecting compiled code (the Norn compiler, lib/compiler)
    "%make-proto" 7 Some(7) => make_proto;
    "%make-template" 3 Some(3) => |vm, a| { let ps = to_vec(vm, a[1])?; let cs = to_vec(vm, a[2])?; let arities = norn_rt::array::array_from(&mut vm.m, &ps); let caps = norn_rt::array::array_from(&mut vm.m, &cs); Ok(norn_rt::func::template_new(&mut vm.m, a[0], arities, caps)) };
    "%make-closure" 1 Some(1) => |vm, a| { expect(a[0], Kind::FnTemplate, "%make-closure")?; Ok(norn_rt::func::closure_new(&mut vm.m, a[0])) };
    "%intern" 2 Some(2) => |vm, a| { let ns = rt().find_or_create_ns(a[0]); Ok(ns.intern(&mut vm.m, a[1])) };
    "%closure-template" 1 Some(1) => |_, a| { expect(a[0], Kind::Closure, "%closure-template")?; Ok(a[0].closure_template()) };
    "%template?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::FnTemplate)));
    "%template-parts" 1 Some(1) => template_parts;
    "%proto-parts" 1 Some(1) => proto_parts;
    "%seed-compile" 1 Some(1) => |vm, a| { vm.nested += 1; let r = crate::compiler::compile_toplevel(vm, a[0]); vm.nested -= 1; r };
    "%compiler-source" 0 Some(0) => |vm, _| Ok(str_new(&mut vm.m, crate::eval::COMPILER_SRC));
    // bytes (immutable byte strings)
    "bytes?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Bytes)));
    "bytes" 1 Some(1) => |vm, a| to_bytes(vm, a[0]);
    "bytes->string" 1 Some(2) => |vm, a| { expect(a[0], Kind::Bytes, "bytes->string")?; let b = a[0].as_bytes(); let s = if a.get(1).is_some_and(|x| x.truthy()) { String::from_utf8_lossy(b).into_owned() } else { std::str::from_utf8(b).map_err(|e| Error::new(format!("Invalid UTF-8: {e}")))?.to_owned() }; Ok(str_new(&mut vm.m, &s)) };
    "subbytes" 2 Some(3) => |vm, a| { expect(a[0], Kind::Bytes, "subbytes")?; let b = a[0].as_bytes(); let s = int(a[1], "subbytes")?; let e = match a.get(2) { Some(&x) => int(x, "subbytes")?, None => b.len() as i64 }; if s < 0 || e < s || e as usize > b.len() { return Err(Error::new(format!("subbytes: range {s}..{e} out of bounds for {} bytes", b.len()))); } Ok(norn_rt::string::bytes_new(&mut vm.m, &b[s as usize..e as usize])) };
    "bytes-concat" 0 VAR => |vm, a| { let mut out = Vec::new(); for &x in a { expect(x, Kind::Bytes, "bytes-concat")?; out.extend_from_slice(x.as_bytes()); } Ok(norn_rt::string::bytes_new(&mut vm.m, &out)) };
    "%hex-encode" 1 Some(1) => |vm, a| { expect(a[0], Kind::Bytes, "hex")?; Ok(str_new(&mut vm.m, &crate::codec::hex_encode(a[0].as_bytes()))) };
    "%hex-decode" 1 Some(1) => |vm, a| { expect(a[0], Kind::String, "unhex")?; let b = crate::codec::hex_decode(a[0].as_str()).map_err(Error::new)?; Ok(norn_rt::string::bytes_new(&mut vm.m, &b)) };
    "%base64-encode" 2 Some(2) => |vm, a| { expect(a[0], Kind::Bytes, "base64")?; Ok(str_new(&mut vm.m, &crate::codec::base64_encode(a[0].as_bytes(), a[1].truthy()))) };
    "%base64-decode" 1 Some(1) => |vm, a| { expect(a[0], Kind::String, "unbase64")?; let b = crate::codec::base64_decode(a[0].as_str()).map_err(Error::new)?; Ok(norn_rt::string::bytes_new(&mut vm.m, &b)) };
    // I/O handles (the reactor and the blocking pool; runtime.md §5.5)
    "%pipe" 0 Some(0) => |vm, _| { let (tx, rx) = mio::unix::pipe::new().map_err(|e| Error::new(e.to_string()))?; let r = crate::reactor::register(&mut vm.m, crate::reactor::Io::PipeIn(rx), "pipe-in", Value::NIL).map_err(Error::new)?; let w = crate::reactor::register(&mut vm.m, crate::reactor::Io::PipeOut(tx), "pipe-out", Value::NIL).map_err(Error::new)?; Ok(vec_from_slice(&mut vm.m, &[r, w])) };
    "%io-seq" 2 Some(2) => |_, a| Ok(Value::fix(crate::reactor::seq(a[0], io_dir(a[1])?)? as i64));
    "%io-wait" 3 Some(3) => |vm, a| crate::reactor::wait(vm, a[0], io_dir(a[1])?, int(a[2], "%io-wait")? as u64);
    "%io-read" 2 Some(2) => |vm, a| io_read(vm, a[0], int(a[1], "read")?);
    "%io-write" 3 Some(3) => |vm, a| io_write(vm, a[0], a[1], int(a[2], "write")?);
    "%io-close" 1 Some(1) => |_, a| crate::reactor::close(a[0]);
    "%io-close-id" 1 Some(1) => |_, a| { crate::reactor::close_id(int(a[0], "%io-close-id")? as u64); Ok(Value::NIL) };
    "handle?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Handle)));
    "%handle-id" 1 Some(1) => |_, a| { expect(a[0], Kind::Handle, "%handle-id")?; Ok(Value::fix(a[0].word(norn_rt::conc::handle::ID) as i64)) };
    "%handle-live?" 1 Some(1) => |_, a| Ok(Value::bool(crate::reactor::entry(a[0]).is_ok()));
    "%handle-current?" 1 Some(1) => |_, a| { expect(a[0], Kind::Handle, "%handle-current?")?; Ok(Value::bool(a[0].word(norn_rt::conc::handle::EPOCH) == norn_rt::conc::process_epoch())) };
    "handle-kind" 1 Some(1) => |_, a| { expect(a[0], Kind::Handle, "handle-kind")?; Ok(a[0].field(norn_rt::conc::handle::KIND)) };
    "handle-info" 1 Some(1) => |_, a| { expect(a[0], Kind::Handle, "handle-info")?; Ok(a[0].field(norn_rt::conc::handle::INFO)) };
    "%bytes-index-of" 2 Some(2) => |_, a| { expect(a[0], Kind::Bytes, "%bytes-index-of")?; let x = int(a[1], "%bytes-index-of")? as u8; Ok(Value::fix(a[0].as_bytes().iter().position(|&b| b == x).map_or(-1, |i| i as i64))) };
    "%glob-match" 2 Some(2) => |_, a| Ok(Value::bool(crate::sys::glob_match(&text(a[0]), &text(a[1]))));
    "%glob-base" 1 Some(1) => |vm, a| Ok(str_new(&mut vm.m, &crate::sys::glob_base(&text(a[0]))));
    "%io-stats" 0 Some(0) => |vm, _| { let kv = [rt().keyword(&mut vm.m, None, "handles"), Value::fix(crate::reactor::open_count() as i64), rt().keyword(&mut vm.m, None, "blocking-threads"), Value::fix(crate::blocking::threads() as i64), rt().keyword(&mut vm.m, None, "carrier-threads"), Value::fix(crate::sched::carrier_threads() as i64)]; hash_map(vm, &kv) };
    // files, paths and the system (sys.rs; norn.io, norn.fs, norn.sys)
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
    "%stdin-line" 0 Some(0) => |vm, _| crate::sys::stdin_line(vm);
    "%fs-watch" 2 Some(2) => |vm, a| crate::sys::watch(vm, text(a[0]), a[1].truthy());
    "%path-join" 1 Some(1) => |_vm, a| { let parts: Vec<String> = seq::SeqIter::new(a[0]).map(|it| it.map(text).collect()).unwrap_or_default(); Ok(str_new(&mut _vm.m, &crate::sys::path_join(&parts))) };
    "%path-part" 2 Some(2) => |vm, a| { let part = if a[1].is_keyword() { a[1].sym_name().to_owned() } else { text(a[1]) }; crate::sys::path_part(&mut vm.m, &text(a[0]), &part) };
    "%getenv" 1 Some(1) => |vm, a| Ok(crate::sys::getenv(&mut vm.m, &text(a[0])));
    "%env" 0 Some(0) => |vm, _| Ok(crate::sys::env(&mut vm.m));
    "%args" 0 Some(0) => |vm, _| Ok(crate::sys::args(&mut vm.m));
    "%sys-info" 0 Some(0) => |vm, _| Ok(crate::sys::system(&mut vm.m));
    // string helpers for norn.rope: character and line arithmetic on a chunk
    "%str-stats" 1 Some(1) => |vm, a| { expect(a[0], Kind::String, "%str-stats")?; let t = a[0].as_str(); let c = t.chars().count() as i64; let n = t.bytes().filter(|&b| b == b'\n').count() as i64; Ok(vec_from_slice(&mut vm.m, &[Value::fix(c), Value::fix(n)])) };
    "%nth-newline-end" 2 Some(2) => |_, a| { expect(a[0], Kind::String, "%nth-newline-end")?; let k = int(a[1], "%nth-newline-end")?; let mut seen = 0; for (i, ch) in a[0].as_str().chars().enumerate() { if ch == '\n' { seen += 1; if seen == k { return Ok(Value::fix(i as i64 + 1)); } } } Ok(Value::NIL) };
    "%count-newlines" 2 Some(2) => |_, a| { expect(a[0], Kind::String, "%count-newlines")?; let i = int(a[1], "%count-newlines")?.max(0) as usize; Ok(Value::fix(a[0].as_str().chars().take(i).filter(|&c| c == '\n').count() as i64)) };
    "%split-lines" 1 Some(1) => |vm, a| { expect(a[0], Kind::String, "lines")?; let ls: Vec<Value> = a[0].as_str().lines().map(|l| str_new(&mut vm.m, l)).collect(); Ok(vec_from_slice(&mut vm.m, &ls)) };
    // sockets and TLS (net.rs; norn.net)
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
    // the basic FFI (ffi.rs; norn.ffi)
    "%ffi-open" 1 Some(1) => |vm, a| crate::ffi::open(vm, &text(a[0]));
    "%ffi-fn" 5 Some(5) => |vm, a| crate::ffi::function(vm, a[0], &text(a[1]), a[2], a[3], a[4].truthy());
    "%ffi-call" 1 VAR => |vm, a| crate::ffi::call(vm, a[0], &a[1..]);
    "%ffi-libc-path" 0 Some(0) => |vm, _| Ok(str_new(&mut vm.m, if cfg!(target_os = "macos") { "/usr/lib/libSystem.B.dylib" } else { "libc.so.6" }));
    "%ffi-libm-path" 0 Some(0) => |vm, _| Ok(str_new(&mut vm.m, if cfg!(target_os = "macos") { "/usr/lib/libSystem.B.dylib" } else { "libm.so.6" }));
    // processes, time, signals, fiber dumps (os.rs; norn.process, norn.time, norn.signal)
    "%process-start" 6 Some(6) => |vm, a| process_start(vm, a);
    "%process-wait" 1 Some(1) => |vm, a| crate::os::wait(vm, a[0]);
    "%process-kill" 2 Some(2) => |vm, a| { let n = signal_arg(a[1])?; crate::os::kill(vm, a[0], n) };
    "%signal-listen" 1 Some(1) => |vm, a| { let names: Vec<String> = seq::SeqIter::new(a[0]).map(|it| it.map(|k| if k.is_keyword() { k.sym_name().to_owned() } else { text(k) }).collect()).unwrap_or_default(); crate::os::listen(vm, &names) };
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
    "%blocking-sleep" 1 Some(1) => |vm, a| { let ms = int(a[0], "%blocking-sleep")?.max(0) as u64; crate::blocking::run(vm, move || { std::thread::sleep(std::time::Duration::from_millis(ms)); Box::new(move |_: &mut norn_rt::Mutator| Ok(Value::fix(ms as i64))) }) };
    // weak references and cleanups
    "weak-ref" 1 Some(1) => |vm, a| Ok(norn_rt::weak::weak_new(&mut vm.m, a[0], Value::NIL));
    "weak-get" 1 Some(1) => |_, a| { expect(a[0], Kind::Weak, "weak-get")?; Ok(a[0].weak_get()) };
    "add-cleanup!" 2 Some(2) => |vm, a| Ok(norn_rt::weak::weak_new(&mut vm.m, a[0], a[1]));
    "cancel-cleanup!" 1 Some(1) => |_, a| { expect(a[0], Kind::Weak, "cancel-cleanup!")?; Ok(Value::bool(a[0].weak_cancel())) };
    // libraries and namespaces
    "%find-lib" 1 Some(1) => |vm, a| { expect(a[0], Kind::String, "%find-lib")?; Ok(match crate::eval::find_lib(a[0].as_str()) { Some((src, origin)) => { let s = str_new(&mut vm.m, &src); let o = str_new(&mut vm.m, &origin); vec_from_slice(&mut vm.m, &[s, o]) } None => Value::NIL }) };
    "%alias" 2 Some(2) => |vm, a| { expect(a[0], Kind::Symbol, "alias")?; expect(a[1], Kind::Symbol, "alias")?; if rt().find_ns(a[1]).is_none() { return Err(Error::new(format!("No namespace: {}", text(a[1])))); } vm.ns.add_alias(a[0], a[1]); Ok(Value::NIL) };
    "%refer" 2 Some(2) => |vm, a| { expect(a[0], Kind::Symbol, "refer")?; expect(a[1], Kind::Var, "refer")?; vm.ns.refer(a[0], a[1]); Ok(Value::NIL) };
    "ns-aliases" 1 Some(1) => |vm, a| { expect(a[0], Kind::Symbol, "ns-aliases")?; let pairs: Vec<Value> = rt().find_ns(a[0]).map(|ns| ns.aliases().into_iter().flat_map(|(k, v)| [k, v]).collect()).unwrap_or_default(); hash_map(vm, &pairs) };
    // images
    "%save-image" 4 Some(4) => |vm, a| { expect(a[0], Kind::String, "save-image")?; crate::image::save_image(vm, a[0], a[1], a[2].truthy(), a[3].truthy()) };
    "%save-image-outcome" 0 Some(0) => |vm, _| crate::image::save_outcome(vm);
    "image-info" 0 Some(0) => |vm, _| crate::image::info(vm);
    "jit-stats" 0 Some(0) => |vm, _| { use std::sync::atomic::Ordering::Relaxed; let k = |vm: &mut Vm, n: &str| rt().keyword(&mut vm.m, None, n); let kvs = [k(vm, "tier"), Value::fix(if crate::jit::enabled() { 1 } else { 0 }), k(vm, "compiled"), Value::fix(crate::jit::COMPILED.load(Relaxed) as i64), k(vm, "rejected"), Value::fix(crate::jit::REJECTED.load(Relaxed) as i64), k(vm, "code-bytes"), Value::fix(crate::jit::CODE_BYTES.load(Relaxed) as i64)]; hash_map(vm, &kvs) };
    "%core-source" 0 Some(0) => |vm, _| Ok(str_new(&mut vm.m, crate::eval::CORE_SRC));
    // classes, instances, generic functions (runtime half of CLOS; policy is in core.norn)
    "class-of" 1 Some(1) => |_, a| Ok(norn_rt::clos::class_of(a[0]));
    "class?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Class)));
    "generic?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Generic)));
    "instance?" 2 Some(2) => |_, a| { expect(a[0], Kind::Class, "instance?")?; let c = norn_rt::clos::class_of(a[1]); Ok(Value::bool(!c.is_nil() && norn_rt::clos::is_subclass(c, a[0]))) };
    "subclass?" 2 Some(2) => |_, a| { expect(a[0], Kind::Class, "subclass?")?; expect(a[1], Kind::Class, "subclass?")?; Ok(Value::bool(norn_rt::clos::is_subclass(a[0], a[1]))) };
    "%class-new" 1 Some(1) => |vm, a| Ok(norn_rt::clos::class_new(&mut vm.m, a[0]));
    "%class-get" 2 Some(2) => |vm, a| { expect(a[0], Kind::Class, "%class-get")?; let f = class_field(vm, a[1])?; let v = a[0].class_field(f); Ok(if f == norn_rt::clos::class::LAYOUT { vec_from_slice(&mut vm.m, norn_rt::clos::layout_keys(v)) } else { v }) };
    "%class-set-layout!" 3 Some(3) => |vm, a| { expect(a[0], Kind::Class, "%class-set-layout!")?; let k = to_vec(vm, a[1])?; let d = to_vec(vm, a[2])?; if k.len() != d.len() { return Err(Error::new("%class-set-layout!: keys and defaults differ in length")); } let l = norn_rt::clos::layout_new(&mut vm.m, &k, &d); a[0].class_set_field(norn_rt::clos::class::LAYOUT, l); Ok(a[0]) };
    "%bound-value" 1 Some(1) => |vm, a| { Ok(match vm.resolve(a[0]) { Some(v) if v.var_root() != Value::UNBOUND => v.var_root(), _ => Value::NIL }) };
    "bit-and" 2 VAR => |vm, a| crate::numeric::bits(&mut vm.m, a, crate::numeric::Bit::And, "bit-and");
    "bit-or" 2 VAR => |vm, a| crate::numeric::bits(&mut vm.m, a, crate::numeric::Bit::Or, "bit-or");
    "bit-xor" 2 VAR => |vm, a| crate::numeric::bits(&mut vm.m, a, crate::numeric::Bit::Xor, "bit-xor");
    "bit-not" 1 Some(1) => |vm, a| crate::numeric::bit_not(&mut vm.m, a[0]);
    "bit-shift-left" 2 Some(2) => |vm, a| crate::numeric::shift(&mut vm.m, a[0], a[1], true);
    "bit-shift-right" 2 Some(2) => |vm, a| crate::numeric::shift(&mut vm.m, a[0], a[1], false);
    "%class-set!" 3 Some(3) => class_set;
    "%register-builtin!" 2 Some(2) => |_, a| { expect(a[1], Kind::Class, "%register-builtin!")?; let n = text(a[0]); if norn_rt::clos::register_builtin(&n, a[1]) { Ok(a[1]) } else { Err(Error::new(format!("Unknown builtin class: {n}"))) } };
    "%bump-class-epoch" 0 Some(0) => |_, _| { norn_rt::clos::bump_class_epoch(); Ok(Value::NIL) };
    "%instance" 2 Some(2) => instance_new;
    "slot-value" 2 Some(2) => |vm, a| slot_value(vm, a[0], a[1]);
    "with" 1 VAR => |vm, a| { expect(a[0], Kind::Instance, "with")?; if a.len() % 2 != 1 { return Err(Error::new("with expects an instance and slot/value pairs")); } crate::owned::check(vm, a[0], false)?; Ok(norn_rt::clos::instance_with(&mut vm.m, a[0], &a[1..], vm.current)?) };
    // owned mutable objects (owned.rs; design §4.2)
    "%set-slot!" 3 Some(3) => |vm, a| crate::owned::set_slot(vm, a[0], a[1], a[2]);
    "%release!" 1 Some(1) => |vm, a| crate::owned::release(vm, a[0]);
    "%share!" 2 Some(2) => |vm, a| crate::owned::share(vm, a[0], a[1]);
    "%owner" 1 Some(1) => |_, a| Ok(crate::owned::owner(a[0]));
    "mutable?" 1 Some(1) => |_, a| Ok(Value::bool(a[0].is(Kind::Instance) && a[0].instance_mutable()));
    "%gf-new" 5 Some(5) => |vm, a| { let nreq = int(a[1], "%gf-new")?.max(0) as usize; Ok(norn_rt::clos::generic_new(&mut vm.m, a[0], nreq, a[2].truthy(), a[3], a[4])) };
    "%gf-get" 2 Some(2) => gf_get;
    "%gf-set!" 3 Some(3) => gf_set;
    "%gf-cas-methods!" 5 Some(5) => |vm, a| { expect(a[0], Kind::Generic, "%gf-cas-methods!")?; let mask = int(a[3], "%gf-cas-methods!")? as u64; Ok(Value::bool(norn_rt::clos::gf_set_methods(&mut vm.m, a[0], a[1], a[2], mask, a[4]))) };
    "%gf-token" 1 Some(1) => |vm, a| { expect(a[0], Kind::Generic, "%gf-token")?; Ok(vec_from_slice(&mut vm.m, &[Value::fix(a[0].gf_epoch() as i64), Value::fix(norn_rt::clos::class_epoch() as i64)])) };
    "%gf-key" 2 Some(2) => |vm, a| { expect(a[0], Kind::Generic, "%gf-key")?; let args = to_vec(vm, a[1])?; let mut key = Vec::new(); norn_rt::clos::dispatch_key(a[0].gf_state(), &args, &mut key); Ok(vec_from_slice(&mut vm.m, &key)) };
    "%gf-cache-get" 2 Some(2) => gf_cache_get;
    "%gf-cache-put!" 4 Some(4) => gf_cache_put;
    "exit" 0 Some(1) => |_, a| { let _ = std::io::stdout().flush(); std::process::exit(a.first().map(|v| v.as_fixnum() as i32).unwrap_or(0)) };
}

/// Bind every primitive to a var in norn.core.
pub fn install(vm: &mut Vm) {
    for (i, p) in PRIMS.iter().enumerate() {
        let sym = rt().symbol(&mut vm.m, None, p.name);
        let qualified = rt().symbol(&mut vm.m, Some("norn.core"), p.name);
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
    if v.is(k) { Ok(()) } else { Err(type_err(op, norn_rt::object::LAYOUTS[k as usize].name, v)) }
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
pub fn compare(a: Value, b: Value) -> Result<Ordering> {
    use norn_rt::numbers::is_number;
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
        R::Plain(Ok(k)) => Ok(norn_rt::string::bytes_new(&mut vm.m, &buf[..k])),
        R::Plain(Err(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => Ok(again(vm)),
        R::Plain(Err(e)) => Err(Error::new(format!("read failed: {e}"))),
        R::Tls(Ok(Some(b))) => Ok(norn_rt::string::bytes_new(&mut vm.m, &b)),
        R::Tls(Ok(None)) => Ok(Value::NIL),
        R::Tls(Err(Ok(w))) => Ok(tls_wait(vm, w)),
        R::Tls(Err(Err(e))) => Err(Error::new(format!("TLS read failed: {e}"))),
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
        R::Plain(Err(e)) => Err(Error::new(format!("write failed: {e}"))),
        R::Tls(Err(Ok(w))) => Ok(tls_wait(vm, w)),
        R::Tls(Err(Err(e))) => Err(Error::new(format!("TLS write failed: {e}"))),
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
        Err(Err(e)) => Err(Error::new(format!("TLS handshake failed: {e}"))),
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
        return Err(Error::new("subseq expects a sorted collection and one or two test/key pairs"));
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
    use norn_rt::colls::*;
    if c.is(Kind::Vector) {
        let n = c.vec_count();
        let buf = norn_rt::array::array_new(&mut vm.m, n.max(8), Value::NIL);
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
    use norn_rt::colls::*;
    expect(t, Kind::Transient, "transient operation")?;
    if t.word(T_DONE) != 0 {
        return Err(Error::new("Transient used after persistent!"));
    }
    if t.field(T_OWNER) != vm.current {
        return Err(Error::new("Ownership: a transient belongs to the fiber that made it"));
    }
    Ok(())
}

fn transient_op(vm: &mut Vm, t: Value, op: TOp) -> Result<Value> {
    use norn_rt::colls::*;
    transient_check(vm, t)?;
    if t.word(T_KIND) == TV {
        let n = t.word(T_COUNT) as usize;
        let mut buf = t.field(T_DATA);
        match op {
            TOp::Conj(x) => {
                if n == buf.arr_len() {
                    let bigger = norn_rt::array::array_new(&mut vm.m, n * 2, Value::NIL);
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
                None => return Err(Error::new(format!("Index out of bounds: {}", pr_str(k, true)))),
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

/// `(persistent! t)`: the finished collection; t can't be used again.
fn persistent(vm: &mut Vm, t: Value) -> Result<Value> {
    use norn_rt::colls::*;
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
        return Ok(norn_rt::string::bytes_new(&mut vm.m, x.as_str().as_bytes()));
    }
    let mut out = Vec::new();
    if let Some(it) = seq::SeqIter::new(x) {
        for v in it {
            match v.is_fixnum().then(|| v.as_fixnum()) {
                Some(n @ 0..=255) => out.push(n as u8),
                _ => return Err(Error::new(format!("bytes: {} is not a byte (0-255)", pr_str(v, true)))),
            }
        }
        return Ok(norn_rt::string::bytes_new(&mut vm.m, &out));
    }
    Err(type_err("bytes", "a string, bytes or a collection of integers", x))
}

/// `(get coll key)` without a default.
pub fn get(coll: Value, key: Value) -> Option<Value> {
    if !coll.is_obj() {
        return None;
    }
    match coll.obj_kind() {
        Kind::Instance => norn_rt::clos::slot_get(coll, key),
        Kind::Map => coll.map_get(key),
        Kind::Set => coll.set_contains(key).then_some(key),
        Kind::Vector => index_of(key).filter(|&i| i < coll.vec_count()).map(|i| coll.vec_nth(i)),
        Kind::String => index_of(key).and_then(|i| coll.as_str().chars().nth(i)).map(Value::char),
        Kind::Bytes => index_of(key).and_then(|i| coll.as_bytes().get(i)).map(|&b| Value::fix(b as i64)),
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
        c = if c.is(Kind::Instance) {
            {
                crate::owned::check(vm, c, false)?;
                norn_rt::clos::instance_with(&mut vm.m, c, kv, vm.current)?
            }
        } else if c.is(Kind::Map) {
            map_assoc(&mut vm.m, c, kv[0], kv[1])
        } else if c.is(Kind::Sorted) && !c.sorted_is_set() {
            crate::sorted::assoc(&mut vm.m, c, kv[0], kv[1])?
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
            norn_rt::colls::queue_conj(&mut vm.m, c, x)
        } else if c.is_nil() || c.is_cons() || c.is(Kind::EmptyList) {
            let tail = if c.is(Kind::EmptyList) { Value::NIL } else { c };
            norn_rt::object::cons(&mut vm.m, x, tail)
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
            norn_rt::object::cons(&mut vm.m, x, c)
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
    } else if c.is(Kind::Instance) {
        norn_rt::clos::slot_get(c, k).is_some()
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
        let layout = m.instance_class().class_field(norn_rt::clos::class::LAYOUT);
        let xs: Vec<Value> = if keys { norn_rt::clos::layout_keys(layout).to_vec() } else { norn_rt::clos::instance_values(m) };
        return Ok(if xs.is_empty() { Value::NIL } else { list_from(&mut vm.m, &xs) });
    }
    if m.is(Kind::Sorted) && !m.sorted_is_set() {
        let xs: Vec<Value> = norn_rt::colls::sorted_entries(m).into_iter().map(|(k, v)| if keys { k } else { v }).collect();
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

fn peek(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let c = a[0];
    if c.is(Kind::Queue) {
        return Ok(norn_rt::colls::queue_peek(c));
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
        return Ok(norn_rt::colls::queue_pop(&mut vm.m, c));
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
        return Err(type_err("disassemble", "a Norn function", f));
    }
    let mut s = String::new();
    for &p in f.closure_template().template_arities().arr_slice() {
        s.push_str(&crate::bytecode::disassemble_proto(p));
    }
    out(&s, false)
}

fn scope_result(_: &mut Vm, a: &[Value]) -> Result<Value> {
    expect(a[0], Kind::Scope, "%scope-result")?;
    let failed = a[0].field(norn_rt::conc::scope::FAILED);
    if !failed.is_nil() {
        return Err(crate::sched::error_from_fiber(failed));
    }
    crate::sched::fiber_outcome(a[1])
}

fn deref(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let x = a[0];
    if x.is(Kind::Atom) {
        Ok(x.atom_get())
    } else if x.is(Kind::Var) {
        Ok(x.var_root())
    } else if x.is(Kind::Fiber) {
        crate::conc::await_fiber(vm, x)
    } else {
        Err(type_err("deref", "an atom, var or fiber", x))
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
    use norn_rt::clos::class::*;
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
        _ => return Err(type_err("%class-get", "a class field keyword", k)),
    })
}

fn class_set(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    use norn_rt::clos::class::*;
    expect(a[0], Kind::Class, "%class-set!")?;
    let f = class_field(vm, a[1])?;
    if f == LAYOUT {
        return Err(Error::new("use %class-set-layout! to change a class's layout"));
    }
    a[0].class_set_field(f, a[2]);
    Ok(a[2])
}

fn instance_new(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    expect(a[0], Kind::Class, "%instance")?;
    let vals = to_vec(vm, a[1])?;
    let n = norn_rt::clos::layout_len(a[0].class_field(norn_rt::clos::class::LAYOUT));
    if vals.len() != n {
        return Err(Error::new(format!("%instance: expected {n} slot values, got {}", vals.len())));
    }
    Ok(norn_rt::clos::instance_new(&mut vm.m, a[0], &vals, vm.current))
}

fn slot_value(vm: &mut Vm, inst: Value, k: Value) -> Result<Value> {
    expect(inst, Kind::Instance, "slot-value")?;
    crate::owned::check(vm, inst, false)?;
    norn_rt::clos::slot_get(inst, k).ok_or_else(|| {
        Error::new(format!("{} has no slot {}", pr_str(inst.instance_class().class_field(norn_rt::clos::class::NAME), true), pr_str(k, true)))
    })
}

fn gf_field(k: Value) -> Result<usize> {
    use norn_rt::clos::generic::*;
    Ok(match if k.is_keyword() { k.sym_name() } else { "" } {
        "name" => NAME,
        "methods" => METHODS,
        "dispatch" => DISPATCH,
        "combination" => COMBINATION,
        "meta" => META,
        "nreq" => NREQ,
        "rest" => REST,
        _ => return Err(type_err("%gf-get", "a generic-function field keyword", k)),
    })
}

fn gf_get(_: &mut Vm, a: &[Value]) -> Result<Value> {
    use norn_rt::clos::generic::*;
    expect(a[0], Kind::Generic, "%gf-get")?;
    let f = gf_field(a[1])?;
    Ok(match f {
        NREQ => Value::fix(a[0].gf_nreq() as i64),
        REST => Value::bool(a[0].gf_rest()),
        _ => Value(a[0].atomic_word(f).load(std::sync::atomic::Ordering::Acquire)),
    })
}

fn gf_set(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    use norn_rt::clos::generic::*;
    expect(a[0], Kind::Generic, "%gf-set!")?;
    let f = gf_field(a[1])?;
    if !matches!(f, DISPATCH | COMBINATION | META) {
        return Err(Error::new("%gf-set! can only change :dispatch, :combination or :meta"));
    }
    a[0].atomic_word(f).store(a[2].0, std::sync::atomic::Ordering::Release);
    // Cached effective methods were built under the old options.
    let methods = a[0].field(METHODS);
    let state = a[0].gf_state();
    let mask = state.arr_get(S_MASK).as_fixnum() as u64;
    let _ = norn_rt::clos::gf_set_methods(&mut vm.m, a[0], methods, methods, mask, state.arr_get(S_EQLS));
    Ok(a[2])
}

fn gf_cache_get(_: &mut Vm, a: &[Value]) -> Result<Value> {
    use norn_rt::clos::generic::*;
    expect(a[0], Kind::Generic, "%gf-cache-get")?;
    let state = a[0].gf_state();
    if state.arr_get(S_CLASS_EPOCH).as_fixnum() as u64 != norn_rt::clos::class_epoch() {
        return Ok(Value::NIL);
    }
    let key: Vec<Value> = if a[1].is(Kind::Vector) { (0..a[1].vec_count()).map(|i| a[1].vec_nth(i)).collect() } else { vec![a[1]] };
    Ok(norn_rt::clos::cache_lookup(state.arr_get(S_CACHE), &key).unwrap_or(Value::NIL))
}

fn gf_cache_put(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    expect(a[0], Kind::Generic, "%gf-cache-put!")?;
    let tok = a[1];
    if !tok.is(Kind::Vector) || tok.vec_count() != 2 {
        return Err(Error::new("%gf-cache-put!: bad token"));
    }
    let key: Vec<Value> = (0..a[2].vec_count()).map(|i| a[2].vec_nth(i)).collect();
    let (e, ce) = (tok.vec_nth(0).as_fixnum() as u64, tok.vec_nth(1).as_fixnum() as u64);
    Ok(Value::bool(norn_rt::clos::cache_put(&mut vm.m, a[0], e, ce, &key, a[3])))
}

// ---- conditions and the debugger ---------------------------------------------------

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
    use norn_rt::conc::fiber;
    use norn_rt::func::{F_FN, F_PROTO, HDR};
    let f = a[0];
    expect(f, Kind::Fiber, "%frames")?;
    if f.fiber_state().load(std::sync::atomic::Ordering::Acquire) != fiber::PARKED {
        return Err(Error::new("%frames needs a parked fiber"));
    }
    let k = |vm: &mut Vm, n: &str| rt().keyword(&mut vm.m, None, n);
    let (kfn, kid, kloc) = (k(vm, "fn"), k(vm, "id"), k(vm, "locals"));
    let mut out = Vec::new();
    for (seg, fp) in crate::vm::walk_frames(f.field(fiber::SEG), f.field(fiber::FP).as_fixnum() as usize) {
        // SAFETY: walk_frames yields live frames of a parked fiber.
        let (func, proto, regs) = unsafe {
            let fr = seg.seg_data().add(fp);
            (*fr.add(F_FN), *fr.add(F_PROTO), fr.add(HDR))
        };
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
    let info = norn_rt::func::ProtoInfo {
        name: a[0],
        nfixed,
        variadic: a[2].truthy(),
        nregs,
        consts: norn_rt::array::array_from(m, &consts),
        code: norn_rt::func::code_new(m, &code),
        locals: norn_rt::array::array_from(m, &locals),
    };
    Ok(norn_rt::func::proto_new(m, info))
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
    ];
    hash_map(vm, &kvs)
}
