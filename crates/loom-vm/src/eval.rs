//! Evaluation: boot (loading lib/core/core.loom), macro expansion, and
//! compiling forms for the `eval` intrinsic.
//!
//! After boot, all evaluation happens in fibers: `eval` compiles a form to a
//! thunk and calls it *in the current fiber* (so evaluated code can park), and
//! `load-string` / `load-file` / the REPL are Loom loops over `%read` + `eval`.
//! Only boot uses the Rust-side `Vm::eval` (nested, non-suspendable runs).

use crate::compiler::compile_toplevel;
use crate::reader::{ReadError, Reader};
use crate::vm::{Error, Result, Vm};
use loom_rt::list::list_from;
use loom_rt::runtime::rt;
use loom_rt::seq::SeqIter;
use loom_rt::{Mutator, Value};

pub const CORE_SRC: &str = include_str!("../../../lib/core/core.loom");

/// A VM for carrier `id`, positioned in loom.core.
pub(crate) fn carrier_vm(id: usize) -> Vm {
    let mut m = Mutator::new();
    loom_rt::runtime::init(&mut m);
    let core_sym = rt().symbol(&mut m, None, "loom.core");
    let core = rt().find_or_create_ns(core_sym);
    Vm::bare(m, core, id)
}

/// Install primitives and load core.loom (carrier 0, before anything runs).
pub(crate) fn boot(vm: &mut Vm) -> Result<()> {
    crate::prims::install(vm);
    vm.load_str(CORE_SRC, "core.loom")?;
    let user = vm.sym("user");
    rt().find_or_create_ns(user);
    Ok(())
}

fn is_do(vm: &mut Vm, form: Value) -> bool {
    form.is_cons() && form.car().is_symbol() && form.car() == vm.sym("do")
}

impl Vm {
    /// Call macro `var` on `form`'s arguments.
    pub fn expand_macro(&mut self, var: Value, form: Value) -> Result<Value> {
        let args: Vec<Value> = SeqIter::new(form.cdr()).map(|it| it.collect()).unwrap_or_default();
        self.apply(var.var_root(), &args).map_err(|mut e| {
            e.msg = format!("while expanding {}: {}", loom_rt::print::pr_str(var, true), e.msg);
            e
        })
    }

    /// One step of top-level macro expansion. Returns (form, expanded?).
    pub fn macroexpand_1(&mut self, form: Value) -> Result<(Value, bool)> {
        if form.is_cons() && form.car().is_symbol() {
            let head = form.car();
            let special = head.sym_ns().is_none() && crate::reader::is_special(head.sym_name());
            if !special
                && let Some(var) = self.resolve(head)
                && var.var_is_macro()
            {
                return Ok((self.expand_macro(var, form)?, true));
            }
        }
        Ok((form, false))
    }

    pub fn macroexpand(&mut self, mut form: Value) -> Result<Value> {
        loop {
            let (f, expanded) = self.macroexpand_1(form)?;
            if !expanded {
                return Ok(f);
            }
            form = f;
        }
    }

    /// Compile a form for `eval` into a zero-argument thunk. A top-level `do`
    /// becomes `(loom.core/%eval-all '(forms…))`, so its forms are compiled
    /// one at a time — a macro defined early in it can be used later in it.
    ///
    /// Macro expansion and compilation run nested: non-suspendable and
    /// GC-inhibited, because the compiler holds values in Rust.
    pub(crate) fn compile_for_eval(&mut self, form: Value) -> Result<Value> {
        self.nested += 1;
        let r = (|| {
            let form = self.macroexpand(form)?;
            let form = if is_do(self, form) {
                let eval_all = rt().symbol(&mut self.m, Some("loom.core"), "%eval-all");
                let quote = self.sym("quote");
                let body = list_from(&mut self.m, &[quote, form.cdr()]);
                list_from(&mut self.m, &[eval_all, body])
            } else {
                form
            };
            compile_toplevel(self, form)
        })();
        self.nested -= 1;
        r
    }

    /// Evaluate a form from Rust (boot only): a nested run.
    pub fn eval(&mut self, form: Value) -> Result<Value> {
        let form = self.macroexpand(form)?;
        if is_do(self, form) {
            let body: Vec<Value> = SeqIter::new(form.cdr()).map(|it| it.collect()).unwrap_or_default();
            let mut last = Value::NIL;
            for f in body {
                last = self.eval(f)?;
            }
            return Ok(last);
        }
        self.nested += 1;
        let thunk = compile_toplevel(self, form);
        self.nested -= 1;
        self.apply(thunk?, &[])
    }

    /// Read and evaluate every form in `src` from Rust (boot only).
    pub fn load_str(&mut self, src: &str, source_name: &str) -> Result<Value> {
        let mut r = Reader::new(src);
        let mut last = Value::NIL;
        loop {
            let _ = r.at_end();
            let line = r.line();
            let form = match r.read(self) {
                Ok(f) => f,
                Err(ReadError::Eof) => return Ok(last),
                Err(e) => return Err(Error::new(format!("{source_name}:{}: {e}", r.line()))),
            };
            last = self.eval(form).map_err(|mut e| {
                e.msg = format!("{source_name}:{line}: {}", e.msg);
                e
            })?;
        }
    }
}

/// `(%read r)`: the next form from a reader object, or the EOF marker.
pub(crate) fn read_next(vm: &mut Vm, r: Value) -> Result<Value> {
    let src = r.field(0).as_str();
    let pos = r.word(1) as usize;
    let mut rd = Reader::new_at(src, pos);
    let res = rd.read(vm);
    r.set_word(1, rd.pos() as u64);
    match res {
        Ok(v) => Ok(v),
        Err(ReadError::Eof) => Ok(Value::EOF),
        Err(e) => {
            let name = loom_rt::print::pr_str(r.field(2), false);
            Err(Error::new(format!("{name}:{}: {e}", rd.line())))
        }
    }
}
