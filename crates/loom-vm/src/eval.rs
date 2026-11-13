//! Evaluation entry points: `eval`, macro expansion, loading source, and VM
//! start-up (which loads lib/core/core.loom).

use crate::compiler::compile_toplevel;
use crate::reader::{ReadError, Reader};
use crate::vm::{Error, Result, Vm};
use loom_rt::runtime::{init, rt};
use loom_rt::seq::SeqIter;
use loom_rt::{Mutator, Value};
use std::sync::OnceLock;

pub const CORE_SRC: &str = include_str!("../../../lib/core/core.loom");

static CORE_LOADED: OnceLock<std::result::Result<(), String>> = OnceLock::new();

impl Vm {
    /// A VM with loom.core loaded, positioned in the `user` namespace.
    pub fn new() -> Result<Vm> {
        let mut m = Mutator::new();
        init(&mut m);
        let core_sym = rt().symbol(&mut m, None, "loom.core");
        let core = rt().find_or_create_ns(core_sym);
        let mut vm = Vm::bare(m, core);
        // The core namespace is process-wide; load it once.
        CORE_LOADED
            .get_or_init(|| {
                crate::prims::install(&mut vm);
                vm.load_str(CORE_SRC, "core.loom").map(|_| ()).map_err(|e| e.to_string())
            })
            .clone()
            .map_err(|e| Error::new(format!("loading core.loom failed: {e}")))?;
        let user = vm.sym("user");
        vm.ns = rt().find_or_create_ns(user);
        Ok(vm)
    }

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

    /// Evaluate one form. A top-level `do` is evaluated form by form, so a
    /// macro defined early in it can be used later in it.
    pub fn eval(&mut self, form: Value) -> Result<Value> {
        let form = self.macroexpand(form)?;
        if form.is_cons() && form.car().is_symbol() && form.car() == self.sym("do") {
            let body: Vec<Value> = SeqIter::new(form.cdr()).map(|it| it.collect()).unwrap_or_default();
            let mut last = Value::NIL;
            for f in body {
                last = self.eval(f)?;
            }
            return Ok(last);
        }
        let thunk = compile_toplevel(self, form)?;
        self.apply(thunk, &[])
    }

    /// Read and evaluate every form in `src`. Returns the last value.
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

    pub fn load_file(&mut self, path: &str) -> Result<Value> {
        let src = std::fs::read_to_string(path).map_err(|e| Error::new(format!("Can't read {path}: {e}")))?;
        let saved = self.ns;
        let r = self.load_str(&src, path);
        self.ns = saved;
        r
    }

    /// Read and evaluate a string, returning the last value (REPL, `-e`).
    pub fn eval_str(&mut self, src: &str) -> Result<Value> {
        self.load_str(src, "<input>")
    }
}
