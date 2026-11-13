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
    "bit-and" 2 VAR => |_, a| bits(a, "bit-and", |x, y| x & y);
    "bit-or" 2 VAR => |_, a| bits(a, "bit-or", |x, y| x | y);
    "bit-xor" 2 VAR => |_, a| bits(a, "bit-xor", |x, y| x ^ y);
    "bit-not" 1 Some(1) => |_, a| Ok(Value::fix(!int(a[0], "bit-not")?));
    "bit-shift-left" 2 Some(2) => |_, a| { let (x, n) = (int(a[0], "bit-shift-left")?, int(a[1], "bit-shift-left")?); if !(0..62).contains(&n) { return Err(Error::new("shift out of range")); } Value::fixnum(x.checked_shl(n as u32).ok_or_else(overflow)?).ok_or_else(overflow) };
    "bit-shift-right" 2 Some(2) => |_, a| { let (x, n) = (int(a[0], "bit-shift-right")?, int(a[1], "bit-shift-right")?); Ok(Value::fix(x >> n.clamp(0, 63))) };
    "%class-set!" 3 Some(3) => class_set;
    "%register-builtin!" 2 Some(2) => |_, a| { expect(a[1], Kind::Class, "%register-builtin!")?; let n = text(a[0]); if norn_rt::clos::register_builtin(&n, a[1]) { Ok(a[1]) } else { Err(Error::new(format!("Unknown builtin class: {n}"))) } };
    "%bump-class-epoch" 0 Some(0) => |_, _| { norn_rt::clos::bump_class_epoch(); Ok(Value::NIL) };
    "%instance" 2 Some(2) => instance_new;
    "slot-value" 2 Some(2) => |_, a| slot_value(a[0], a[1]);
    "with" 1 VAR => |vm, a| { expect(a[0], Kind::Instance, "with")?; if a.len() % 2 != 1 { return Err(Error::new("with expects an instance and slot/value pairs")); } Ok(norn_rt::clos::instance_with(&mut vm.m, a[0], &a[1..])?) };
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
            norn_rt::clos::instance_with(&mut vm.m, c, kv)?
        } else if c.is(Kind::Map) {
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
        "defaults" => DEFAULTS,
        "meta" => META,
        "subclasses" => SUBCLASSES,
        _ => return Err(type_err("%class-get", "a class field keyword", k)),
    })
}

fn class_set(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    use norn_rt::clos::class::*;
    expect(a[0], Kind::Class, "%class-set!")?;
    let f = class_field(vm, a[1])?;
    if f == LAYOUT || f == DEFAULTS {
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
    Ok(norn_rt::clos::instance_new(&mut vm.m, a[0], &vals))
}

fn slot_value(inst: Value, k: Value) -> Result<Value> {
    expect(inst, Kind::Instance, "slot-value")?;
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

fn bits(a: &[Value], op: &str, f: fn(i64, i64) -> i64) -> Result<Value> {
    let mut acc = int(a[0], op)?;
    for &x in &a[1..] {
        acc = f(acc, int(x, op)?);
    }
    Ok(Value::fix(acc))
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
