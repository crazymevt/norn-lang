//! The seed compiler: Skein forms → tier-0 bytecode (runtime.md §9).
//!
//! It is intentionally small and will be replaced by the real compiler,
//! written in Skein, in M4. It handles the kernel forms
//!
//! ```text
//! quote if do def var fn* let* letrec* loop* recur
//! ```
//!
//! and expands macros. Everything else (`let`, `fn`, `defn`, `cond`, …) is a
//! macro in lib/core/core.skn.
//!
//! Locals are immutable, so closures capture values (flat closures).
//! `letrec*` patches captures of not-yet-created siblings with `SETUP`.

use crate::bytecode::*;
use crate::vm::{Error, Result, Vm};
use skein_rt::array::array_from;
use skein_rt::func::{Capture, ProtoInfo, closure_new, code_new, proto_new, template_new};
use skein_rt::runtime::rt;
use skein_rt::seq::SeqIter;
use skein_rt::{Kind, Value};

#[derive(Clone, Copy)]
struct Pos {
    /// In tail position of the function: emit RETURN / TAILCALL.
    tail: bool,
    /// `recur` is allowed here.
    recur: bool,
}

const NT: Pos = Pos { tail: false, recur: false };

struct RecurTarget {
    start: usize,
    regs: Vec<u8>,
}

#[derive(Default)]
struct Arity {
    code: Vec<u32>,
    consts: Vec<Value>,
    locals: Vec<(Value, u8)>,
    /// Every local ever bound in this arity (name, register), for the debugger.
    debug: Vec<(Value, u8)>,
    next_reg: usize,
    max_reg: usize,
    recur: Vec<RecurTarget>,
}

struct FnCtx {
    /// The fn's own name, bound to the closure inside its body (or nil).
    self_name: Value,
    /// Its code's name (ns/name, or outer/fn for an anonymous one), or nil.
    fname: Value,
    /// Upvalues, shared by all arities of the fn.
    upvals: Vec<(Value, Capture)>,
    ar: Arity,
}

enum Loc {
    Reg(u8),
    Up(u8),
    SelfRef,
}

pub struct Compiler<'v> {
    vm: &'v mut Vm,
    fns: Vec<FnCtx>,
    /// The var a def being compiled defines (anonymous code in its value is
    /// named after it), or nil.
    def_name: Value,
}

fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::new(msg))
}

fn items(form: Value) -> Vec<Value> {
    SeqIter::new(form).map(|it| it.collect()).unwrap_or_default()
}

fn vec_items(v: Value) -> Vec<Value> {
    let mut out = Vec::with_capacity(v.vec_count());
    v.vec_for_each(|x| {
        out.push(x);
        true
    });
    out
}

/// Literal collections whose elements need no evaluation become constants.
fn is_constant(v: Value) -> bool {
    if v.is_symbol() || v.is_cons() {
        return false;
    }
    if !v.is_obj() {
        return true;
    }
    match v.obj_kind() {
        Kind::Vector => v.vec_for_each(is_constant),
        Kind::Map => v.map_for_each(|k, x| is_constant(k) && is_constant(x)),
        Kind::Set => v.set_for_each(is_constant),
        _ => true,
    }
}

/// Compile a top-level form into a zero-argument closure.
pub fn compile_toplevel(vm: &mut Vm, form: Value) -> Result<Value> {
    let name = rt().symbol(&mut vm.m, None, "<toplevel>");
    let mut c = Compiler { vm, fns: Vec::new(), def_name: Value::NIL };
    c.fns.push(FnCtx { self_name: Value::NIL, fname: Value::NIL, upvals: Vec::new(), ar: Arity::default() });
    let dest = c.alloc()?;
    c.compile(form, dest, Pos { tail: true, recur: false })?;
    let f = c.fns.pop().unwrap();
    let proto = c.finish_arity(f.ar, name, 0, false, dest);
    let m = &mut c.vm.m;
    let arities = array_from(m, &[proto]);
    let caps = array_from(m, &[]);
    let tmpl = template_new(m, name, arities, caps);
    Ok(closure_new(m, tmpl))
}

impl Compiler<'_> {
    fn cur(&mut self) -> &mut FnCtx {
        self.fns.last_mut().unwrap()
    }

    fn ar(&mut self) -> &mut Arity {
        &mut self.cur().ar
    }

    fn emit(&mut self, ins: u32) -> usize {
        let code = &mut self.ar().code;
        code.push(ins);
        code.len() - 1
    }

    fn pc(&mut self) -> usize {
        self.ar().code.len()
    }

    fn konst(&mut self, v: Value) -> Result<u16> {
        let consts = &mut self.ar().consts;
        if let Some(i) = consts.iter().position(|&k| k == v) {
            return Ok(i as u16);
        }
        if consts.len() > u16::MAX as usize {
            return err("Too many constants in one function");
        }
        consts.push(v);
        Ok((consts.len() - 1) as u16)
    }

    fn alloc(&mut self) -> Result<u8> {
        let a = self.ar();
        if a.next_reg >= 255 {
            return err("Too many registers in one function (expression too complex)");
        }
        let r = a.next_reg;
        a.next_reg += 1;
        a.max_reg = a.max_reg.max(a.next_reg);
        Ok(r as u8)
    }

    fn free_to(&mut self, r: u8) {
        self.ar().next_reg = r as usize;
    }

    fn jump_offset(from: usize, to: usize) -> Result<i32> {
        let off = to as i64 - (from as i64 + 1);
        if off.abs() >= SBX_BIAS as i64 {
            return err("Function too large (jump out of range)");
        }
        Ok(off as i32)
    }

    fn patch_jump(&mut self, at: usize, to: usize) -> Result<()> {
        let off = Self::jump_offset(at, to)?;
        let ins = self.ar().code[at];
        self.ar().code[at] = asbx(op(ins), a(ins) as u8, off);
        Ok(())
    }

    fn finish(&mut self, dest: u8, pos: Pos) {
        if pos.tail {
            self.emit(abc(RETURN, dest, 0, 0));
        }
    }

    fn finish_arity(&mut self, mut ar: Arity, name: Value, nfixed: usize, variadic: bool, dest: u8) -> Value {
        // Safety net: falling off the end returns the body register.
        ar.code.push(abc(RETURN, dest, 0, 0));
        let m = &mut self.vm.m;
        let consts = array_from(m, &ar.consts);
        let code = code_new(m, &ar.code);
        let dbg: Vec<Value> = ar.debug.iter().flat_map(|&(s, r)| [s, Value::fix(r as i64)]).collect();
        let locals = array_from(m, &dbg);
        let nregs = ar.max_reg.max(nfixed + variadic as usize);
        proto_new(m, ProtoInfo { name, nfixed, variadic, nregs, consts, code, locals })
    }

    fn load_const(&mut self, dest: u8, v: Value) -> Result<()> {
        match v {
            Value::NIL => self.emit(abc(LOADNIL, dest, 0, 0)),
            Value::TRUE => self.emit(abc(LOADTRUE, dest, 0, 0)),
            Value::FALSE => self.emit(abc(LOADFALSE, dest, 0, 0)),
            _ if v.is_fixnum() && (-32768..32768).contains(&v.as_fixnum()) => {
                self.emit(asbx(LOADI, dest, v.as_fixnum() as i32))
            }
            _ => {
                let k = self.konst(v)?;
                self.emit(abx(LOADK, dest, k))
            }
        };
        Ok(())
    }

    // ---- name resolution -------------------------------------------------------

    /// Find `sym` lexically from fn level `level`, threading captures through
    /// intermediate closures.
    fn lookup(&mut self, level: usize, sym: Value) -> Option<Loc> {
        let f = &self.fns[level];
        if let Some(&(_, r)) = f.ar.locals.iter().rev().find(|(s, _)| *s == sym) {
            return Some(Loc::Reg(r));
        }
        if f.self_name == sym {
            return Some(Loc::SelfRef);
        }
        if let Some(i) = f.upvals.iter().position(|(s, _)| *s == sym) {
            return Some(Loc::Up(i as u8));
        }
        if level == 0 {
            return None;
        }
        let cap = match self.lookup(level - 1, sym)? {
            Loc::Reg(r) => Capture::Local(r),
            Loc::Up(i) => Capture::Up(i),
            Loc::SelfRef => Capture::SelfRef,
        };
        let ups = &mut self.fns[level].upvals;
        if ups.len() >= 255 {
            return None;
        }
        ups.push((sym, cap));
        Some(Loc::Up((ups.len() - 1) as u8))
    }

    fn is_lexical(&self, sym: Value) -> bool {
        self.fns.iter().any(|f| {
            f.self_name == sym || f.ar.locals.iter().any(|(s, _)| *s == sym) || f.upvals.iter().any(|(s, _)| *s == sym)
        })
    }

    fn macro_var(&mut self, form: Value) -> Option<Value> {
        if !form.is_cons() {
            return None;
        }
        let head = form.car();
        if !head.is_symbol() || (head.sym_ns().is_none() && crate::reader::is_special(head.sym_name())) {
            return None;
        }
        if head.sym_ns().is_none() && self.is_lexical(head) {
            return None;
        }
        self.vm.resolve(head).filter(|v| v.var_is_macro())
    }

    fn macroexpand(&mut self, mut form: Value) -> Result<Value> {
        while let Some(var) = self.macro_var(form) {
            form = self.vm.expand_macro(var, form)?;
        }
        Ok(form)
    }

    // ---- forms --------------------------------------------------------------------

    fn compile(&mut self, form: Value, dest: u8, pos: Pos) -> Result<()> {
        if form.is_symbol() {
            return self.compile_symbol(form, dest, pos);
        }
        if form.is_cons() {
            return self.compile_list(form, dest, pos);
        }
        if form.is_obj() && !is_constant(form) {
            let (ctor, elems) = match form.obj_kind() {
                Kind::Vector => ("vector", vec_items(form)),
                Kind::Map => {
                    let mut kvs = Vec::new();
                    form.map_for_each(|k, v| {
                        kvs.push(k);
                        kvs.push(v);
                        true
                    });
                    ("hash-map", kvs)
                }
                Kind::Set => {
                    let mut xs = Vec::new();
                    form.set_for_each(|x| {
                        xs.push(x);
                        true
                    });
                    ("hash-set", xs)
                }
                _ => unreachable!(),
            };
            let sym = rt().symbol(&mut self.vm.m, Some("skein.core"), ctor);
            let var = self.vm.resolve(sym).ok_or_else(|| Error::new(format!("skein.core/{ctor} is not defined")))?;
            return self.compile_call_with(|c, r| {
                let k = c.konst(var)?;
                c.emit(abx(GETVAR, r, k));
                Ok(())
            }, &elems, dest, pos);
        }
        self.load_const(dest, form)?;
        self.finish(dest, pos);
        Ok(())
    }

    fn compile_symbol(&mut self, sym: Value, dest: u8, pos: Pos) -> Result<()> {
        let level = self.fns.len() - 1;
        let loc = if sym.sym_ns().is_none() { self.lookup(level, sym) } else { None };
        match loc {
            Some(Loc::Reg(r)) => {
                if r != dest {
                    self.emit(abc(MOVE, dest, r, 0));
                }
            }
            Some(Loc::Up(i)) => {
                self.emit(abc(GETUP, dest, i, 0));
            }
            Some(Loc::SelfRef) => {
                self.emit(abc(SELF, dest, 0, 0));
            }
            None => {
                let Some(var) = self.vm.resolve(sym) else {
                    return err(format!(
                        "Unable to resolve symbol: {} in this context",
                        skein_rt::print::pr_str(sym, true)
                    ));
                };
                if var.var_is_macro() {
                    return err(format!("Can't take the value of a macro: {}", skein_rt::print::pr_str(var, true)));
                }
                let k = self.konst(var)?;
                self.emit(abx(GETVAR, dest, k));
            }
        }
        self.finish(dest, pos);
        Ok(())
    }

    fn compile_list(&mut self, form: Value, dest: u8, pos: Pos) -> Result<()> {
        let head = form.car();
        if head.is_symbol() && head.sym_ns().is_none() {
            let xs = || items(form);
            match head.sym_name() {
                "quote" => {
                    let xs = xs();
                    if xs.len() != 2 {
                        return err("quote takes exactly one argument");
                    }
                    self.load_const(dest, xs[1])?;
                    self.finish(dest, pos);
                    return Ok(());
                }
                "if" => return self.compile_if(&xs(), dest, pos),
                "do" => return self.compile_do(&xs()[1..], dest, pos),
                "def" => return self.compile_def(&xs(), dest, pos),
                "var" => return self.compile_var(&xs(), dest, pos),
                "fn*" => return self.compile_fn(&xs(), dest, pos).map(|_| ()),
                "let*" => return self.compile_let(&xs(), dest, pos, false),
                "loop*" => return self.compile_let(&xs(), dest, pos, true),
                "letrec*" => return self.compile_letrec(&xs(), dest, pos),
                "recur" => return self.compile_recur(&xs()[1..], pos),
                _ => {}
            }
        }
        if let Some(var) = self.macro_var(form) {
            let expanded = self.vm.expand_macro(var, form)?;
            return self.compile(expanded, dest, pos);
        }
        let xs = items(form);
        self.compile_call_with(|c, r| c.compile(xs[0], r, NT), &xs[1..], dest, pos)
    }

    /// Emit a call: `load_fn` puts the callee in a register, then the args.
    fn compile_call_with(
        &mut self,
        load_fn: impl FnOnce(&mut Self, u8) -> Result<()>,
        args: &[Value],
        dest: u8,
        pos: Pos,
    ) -> Result<()> {
        if args.len() > 250 {
            return err("Too many arguments in one call (max 250)");
        }
        let base = self.alloc()?;
        load_fn(self, base)?;
        for &arg in args {
            let r = self.alloc()?;
            self.compile(arg, r, NT)?;
        }
        if pos.tail {
            self.emit(abc(TAILCALL, base, args.len() as u8, 0));
            // Never reached by a successful tail call. If the call *fails*,
            // the error handler runs as an ordinary call returning here, so
            // this frame stays on the stack (debuggable, restartable) and a
            // `use-value` is returned from it; a `retry` re-runs the TAILCALL.
            self.emit(abc(RETURN, base, 0, 0));
        } else {
            self.emit(abc(CALL, base, args.len() as u8, 0));
            if dest != base {
                self.emit(abc(MOVE, dest, base, 0));
            }
        }
        self.free_to(base);
        Ok(())
    }

    fn compile_if(&mut self, xs: &[Value], dest: u8, pos: Pos) -> Result<()> {
        if xs.len() < 3 || xs.len() > 4 {
            return err("if takes a test, a then-branch and an optional else-branch");
        }
        let t = self.alloc()?;
        self.compile(xs[1], t, NT)?;
        self.free_to(t);
        let jf = self.emit(asbx(JMPF, t, 0));
        self.compile(xs[2], dest, pos)?;
        let jend = if pos.tail { None } else { Some(self.emit(asbx(JMP, 0, 0))) };
        let here = self.pc();
        self.patch_jump(jf, here)?;
        self.compile(xs.get(3).copied().unwrap_or(Value::NIL), dest, pos)?;
        if let Some(j) = jend {
            let here = self.pc();
            self.patch_jump(j, here)?;
        }
        Ok(())
    }

    fn compile_do(&mut self, body: &[Value], dest: u8, pos: Pos) -> Result<()> {
        let Some((&last, init)) = body.split_last() else {
            self.load_const(dest, Value::NIL)?;
            self.finish(dest, pos);
            return Ok(());
        };
        for &f in init {
            let t = self.alloc()?;
            self.compile(f, t, NT)?;
            self.free_to(t);
        }
        self.compile(last, dest, pos)
    }

    fn binding_pairs(xs: &[Value], what: &str) -> Result<Vec<(Value, Value)>> {
        let Some(&bv) = xs.get(1).filter(|b| b.is(Kind::Vector)) else {
            return err(format!("{what} requires a vector for its bindings"));
        };
        let b = vec_items(bv);
        if !b.len().is_multiple_of(2) {
            return err(format!("{what} requires an even number of forms in its binding vector"));
        }
        let pairs: Vec<(Value, Value)> = b.chunks(2).map(|p| (p[0], p[1])).collect();
        for &(s, _) in &pairs {
            if !s.is_symbol() || s.sym_ns().is_some() {
                return err(format!(
                    "Bad binding form in {what}: {} (destructuring arrives with the real compiler)",
                    skein_rt::print::pr_str(s, true)
                ));
            }
        }
        Ok(pairs)
    }

    /// `let*`, and `loop*` (a `let*` whose body is a `recur` target).
    fn compile_let(&mut self, xs: &[Value], dest: u8, pos: Pos, is_loop: bool) -> Result<()> {
        let pairs = Self::binding_pairs(xs, if is_loop { "loop" } else { "let" })?;
        let mark = self.ar().next_reg as u8;
        let nlocals = self.ar().locals.len();
        let mut regs = Vec::new();
        for (s, init) in pairs {
            let r = self.alloc()?;
            self.compile(init, r, NT)?;
            self.ar().locals.push((s, r));
            self.ar().debug.push((s, r));
            regs.push(r);
        }
        let body_pos = if is_loop {
            let start = self.pc();
            self.ar().recur.push(RecurTarget { start, regs });
            Pos { tail: pos.tail, recur: true }
        } else {
            pos
        };
        self.compile_do(&xs[2..], dest, body_pos)?;
        if is_loop {
            self.ar().recur.pop();
        }
        self.ar().locals.truncate(nlocals);
        self.free_to(mark);
        Ok(())
    }

    fn compile_letrec(&mut self, xs: &[Value], dest: u8, pos: Pos) -> Result<()> {
        let pairs = Self::binding_pairs(xs, "letrec*")?;
        let mark = self.ar().next_reg as u8;
        let nlocals = self.ar().locals.len();
        let mut regs = Vec::new();
        for &(s, _) in &pairs {
            let r = self.alloc()?;
            self.emit(abc(LOADNIL, r, 0, 0));
            self.ar().locals.push((s, r));
            self.ar().debug.push((s, r));
            regs.push(r);
        }
        let mut fixups = Vec::new();
        for (i, &(_, init)) in pairs.iter().enumerate() {
            let init = self.macroexpand(init)?;
            let is_fn = init.is_cons() && init.car().is_symbol() && init.car().sym_name() == "fn*";
            if !is_fn {
                return err("letrec* bindings must be functions");
            }
            let caps = self.compile_fn(&items(init), regs[i], NT)?;
            for (up, cap) in caps.into_iter().enumerate() {
                if let Capture::Local(r) = cap
                    && regs.contains(&r)
                {
                    fixups.push((regs[i], up as u8, r));
                }
            }
        }
        for (closure, up, src) in fixups {
            self.emit(abc(SETUP, closure, up, src));
        }
        self.compile_do(&xs[2..], dest, pos)?;
        self.ar().locals.truncate(nlocals);
        self.free_to(mark);
        Ok(())
    }

    fn compile_recur(&mut self, args: &[Value], pos: Pos) -> Result<()> {
        if !pos.recur {
            return err("Can only recur from tail position");
        }
        let Some(target) = self.ar().recur.last() else {
            return err("recur outside of loop or fn");
        };
        let (start, regs) = (target.start, target.regs.clone());
        if args.len() != regs.len() {
            return err(format!("Mismatched argument count to recur, expected: {} args, got: {}", regs.len(), args.len()));
        }
        let base = self.ar().next_reg as u8;
        for &arg in args {
            let r = self.alloc()?;
            self.compile(arg, r, NT)?;
        }
        for (i, &r) in regs.iter().enumerate() {
            let src = base + i as u8;
            if src != r {
                self.emit(abc(MOVE, r, src, 0));
            }
        }
        let at = self.pc();
        let off = Self::jump_offset(at, start)?;
        self.emit(asbx(LOOP, 0, off));
        self.free_to(base);
        Ok(())
    }

    fn compile_def(&mut self, xs: &[Value], dest: u8, pos: Pos) -> Result<()> {
        let Some(&sym) = xs.get(1).filter(|s| s.is_symbol()) else {
            return err("First argument to def must be a symbol");
        };
        let ns = self.vm.ns;
        if let Some(q) = sym.sym_ns()
            && q != ns.name.sym_name()
        {
            return err("Can't create a var in another namespace with def");
        }
        let local = rt().symbol(&mut self.vm.m, None, sym.sym_name());
        let var = ns.intern(&mut self.vm.m, local);
        let init = match xs.len() {
            2 => None,
            3 => Some(xs[2]),
            4 if xs[2].is(Kind::String) => Some(xs[3]),
            _ => return err("Too many arguments to def"),
        };
        let k = self.konst(var)?;
        if let Some(init) = init {
            // (anonymous code in the value is named after the var)
            let outer = self.def_name;
            let ns = self.vm.ns.name.sym_name().to_owned();
            self.def_name = rt().symbol(&mut self.vm.m, Some(&ns), sym.sym_name());
            let r = self.compile(init, dest, NT);
            self.def_name = outer;
            r?;
            self.emit(abx(DEFVAR, dest, k));
        }
        self.emit(abx(LOADK, dest, k));
        self.finish(dest, pos);
        Ok(())
    }

    fn compile_var(&mut self, xs: &[Value], dest: u8, pos: Pos) -> Result<()> {
        let Some(&sym) = xs.get(1).filter(|s| s.is_symbol()) else {
            return err("var takes a symbol");
        };
        let Some(var) = self.vm.resolve(sym) else {
            return err(format!("Unable to resolve var: {}", skein_rt::print::pr_str(sym, true)));
        };
        self.load_const(dest, var)?;
        self.finish(dest, pos);
        Ok(())
    }

    /// Compile `(fn* name? [params] body…)` or `(fn* name? ([params] body…)…)`.
    /// Returns how the new closure captures each of its upvalues.
    fn compile_fn(&mut self, xs: &[Value], dest: u8, pos: Pos) -> Result<Vec<Capture>> {
        let mut i = 1;
        let mut self_name = Value::NIL;
        if xs.get(1).is_some_and(|s| s.is_symbol()) {
            self_name = xs[1];
            i = 2;
        }
        let rest = &xs[i..];
        let sigs: Vec<(Value, Vec<Value>)> = match rest.first() {
            Some(p) if p.is(Kind::Vector) => vec![(*p, rest[1..].to_vec())],
            Some(_) => rest
                .iter()
                .map(|sig| {
                    let s = items(*sig);
                    match s.first() {
                        Some(p) if sig.is_cons() && p.is(Kind::Vector) => Ok((*p, s[1..].to_vec())),
                        _ => err("fn* arity must look like ([params] body...)"),
                    }
                })
                .collect::<Result<_>>()?,
            None => return err("fn* requires a parameter vector"),
        };
        // (an anonymous one's code is named for where it is, outer/fn, for
        // backtraces and profiles; the function itself still prints as fn)
        let outer = self.fns.last().map(|f| f.fname).filter(|n| !n.is_nil()).unwrap_or(self.def_name);
        let name = if self_name.is_symbol() {
            let ns = self.vm.ns.name.sym_name().to_owned();
            rt().symbol(&mut self.vm.m, Some(&ns), self_name.sym_name())
        } else if outer.is_symbol() {
            let o = skein_rt::print::pr_str(outer, false);
            rt().symbol(&mut self.vm.m, Some(&o), "fn")
        } else {
            Value::NIL
        };
        let tname = if self_name.is_symbol() { name } else { Value::NIL };
        self.fns.push(FnCtx { self_name, fname: name, upvals: Vec::new(), ar: Arity::default() });
        let mut protos = Vec::new();
        let mut seen_fixed = Vec::new();
        let mut seen_variadic = false;
        for (params, body) in sigs {
            self.cur().ar = Arity::default();
            let ps = vec_items(params);
            let mut nfixed = 0;
            let mut variadic = false;
            let mut regs = Vec::new();
            let mut k = 0;
            while k < ps.len() {
                let p = ps[k];
                if !p.is_symbol() || p.sym_ns().is_some() {
                    self.fns.pop();
                    return err(format!(
                        "Bad parameter: {} (destructuring arrives with the real compiler)",
                        skein_rt::print::pr_str(p, true)
                    ));
                }
                if p.sym_name() == "&" {
                    if k + 2 != ps.len() {
                        self.fns.pop();
                        return err("& must be followed by exactly one parameter");
                    }
                    variadic = true;
                    k += 1;
                    continue;
                }
                let r = self.alloc()?;
                self.ar().locals.push((ps[k], r));
            self.ar().debug.push((ps[k], r));
                regs.push(r);
                if !variadic {
                    nfixed += 1;
                }
                k += 1;
            }
            if (variadic && seen_variadic) || (!variadic && seen_fixed.contains(&nfixed)) {
                self.fns.pop();
                return err("Can't have two overloads with the same arity");
            }
            if variadic {
                seen_variadic = true;
            } else {
                seen_fixed.push(nfixed);
            }
            self.ar().recur.push(RecurTarget { start: 0, regs });
            let d = self.alloc()?;
            self.compile_do(&body, d, Pos { tail: true, recur: true })?;
            let ar = std::mem::take(&mut self.cur().ar);
            protos.push(self.finish_arity(ar, name, nfixed, variadic, d));
        }
        let f = self.fns.pop().unwrap();
        let caps: Vec<Capture> = f.upvals.iter().map(|(_, c)| *c).collect();
        let m = &mut self.vm.m;
        let arities = array_from(m, &protos);
        let encoded: Vec<Value> = caps.iter().map(|c| c.encode()).collect();
        let caps_arr = array_from(m, &encoded);
        let tmpl = template_new(m, tname, arities, caps_arr);
        let k = self.konst(tmpl)?;
        self.emit(abx(CLOSURE, dest, k));
        self.finish(dest, pos);
        Ok(caps)
    }
}
