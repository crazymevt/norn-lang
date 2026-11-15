//! Evaluation: boot (loading lib/core/core.norn), macro expansion, and
//! compiling forms for the `eval` intrinsic.
//!
//! After boot, all evaluation happens in fibers: `eval` compiles a form to a
//! thunk and calls it *in the current fiber* (so evaluated code can park), and
//! `load-string` / `load-file` / the REPL are Norn loops over `%read` + `eval`.
//! Only boot uses the Rust-side `Vm::eval` (nested, non-suspendable runs).

use crate::compiler::compile_toplevel;
use crate::reader::{ReadError, Reader};
use crate::vm::{Error, Result, Vm};
use norn_rt::list::list_from;
use norn_rt::runtime::rt;
use norn_rt::seq::SeqIter;
use norn_rt::{Mutator, Value};

pub const CORE_SRC: &str = include_str!("../../../lib/core/core.norn");
pub const COMPILER_SRC: &str = include_str!("../../../lib/compiler/compiler.norn");
pub const EXPANDER_SRC: &str = include_str!("../../../lib/compiler/expander.norn");

/// The standard libraries (`lib/norn/*.norn`), embedded so `norn` is a
/// single file. `(require 'norn.x)` loads one the first time it is needed.
pub const LIBS: &[(&str, &str)] = &[
    ("norn.bytes", include_str!("../../../lib/norn/bytes.norn")),
    ("norn.buffer", include_str!("../../../lib/norn/buffer.norn")),
    ("norn.io", include_str!("../../../lib/norn/io.norn")),
    ("norn.fs", include_str!("../../../lib/norn/fs.norn")),
    ("norn.sys", include_str!("../../../lib/norn/sys.norn")),
    ("norn.process", include_str!("../../../lib/norn/process.norn")),
    ("norn.time", include_str!("../../../lib/norn/time.norn")),
    ("norn.signal", include_str!("../../../lib/norn/signal.norn")),
    ("norn.net", include_str!("../../../lib/norn/net.norn")),
    ("norn.ffi", include_str!("../../../lib/norn/ffi.norn")),
    ("norn.rope", include_str!("../../../lib/norn/rope.norn")),
    ("norn.markdown", include_str!("../../../lib/norn/markdown.norn")),
    ("norn.string", include_str!("../../../lib/norn/string.norn")),
    ("norn.regex", include_str!("../../../lib/norn/regex.norn")),
    ("norn.json", include_str!("../../../lib/norn/json.norn")),
    ("norn.ndjson", include_str!("../../../lib/norn/ndjson.norn")),
    ("norn.xml", include_str!("../../../lib/norn/xml.norn")),
    ("norn.yaml", include_str!("../../../lib/norn/yaml.norn")),
    ("norn.cbor", include_str!("../../../lib/norn/cbor.norn")),
    ("norn.edn", include_str!("../../../lib/norn/edn.norn")),
    ("norn.format", include_str!("../../../lib/norn/format.norn")),
    ("norn.pprint", include_str!("../../../lib/norn/pprint.norn")),
    ("norn.math", include_str!("../../../lib/norn/math.norn")),
    ("norn.decimal", include_str!("../../../lib/norn/decimal.norn")),
    ("norn.set", include_str!("../../../lib/norn/set.norn")),
    ("norn.walk", include_str!("../../../lib/norn/walk.norn")),
    ("norn.random", include_str!("../../../lib/norn/random.norn")),
    ("norn.async", include_str!("../../../lib/norn/async.norn")),
    ("norn.sync", include_str!("../../../lib/norn/sync.norn")),
    ("norn.actor", include_str!("../../../lib/norn/actor.norn")),
    ("norn.stm", include_str!("../../../lib/norn/stm.norn")),
    ("norn.sqlite", include_str!("../../../lib/norn/sqlite.norn")),
    ("norn.bin", include_str!("../../../lib/norn/bin.norn")),
    ("norn.schedule", include_str!("../../../lib/norn/schedule.norn")),
    ("norn.zip", include_str!("../../../lib/norn/zip.norn")),
    ("norn.toml", include_str!("../../../lib/norn/toml.norn")),
    ("norn.csv", include_str!("../../../lib/norn/csv.norn")),
    ("norn.data", include_str!("../../../lib/norn/data.norn")),
    ("norn.uuid", include_str!("../../../lib/norn/uuid.norn")),
    ("norn.crypto", include_str!("../../../lib/norn/crypto.norn")),
    ("norn.compress", include_str!("../../../lib/norn/compress.norn")),
    ("norn.cache", include_str!("../../../lib/norn/cache.norn")),
    ("norn.log", include_str!("../../../lib/norn/log.norn")),
    ("norn.test", include_str!("../../../lib/norn/test.norn")),
    ("norn.property", include_str!("../../../lib/norn/property.norn")),
    ("norn.cli", include_str!("../../../lib/norn/cli.norn")),
    ("norn.terminal", include_str!("../../../lib/norn/terminal.norn")),
    ("norn.package", include_str!("../../../lib/norn/package.norn")),
    ("norn.fmt", include_str!("../../../lib/norn/fmt.norn")),
    ("norn.lint", include_str!("../../../lib/norn/lint.norn")),
    ("norn.bench", include_str!("../../../lib/norn/bench.norn")),
    ("norn.doc", include_str!("../../../lib/norn/doc.norn")),
    ("norn.image", include_str!("../../../lib/norn/image.norn")),
    ("norn.check", include_str!("../../../lib/norn/check.norn")),
    ("norn.syntax", include_str!("../../../lib/norn/syntax.norn")),
    ("norn.validate", include_str!("../../../lib/norn/validate.norn")),
    ("norn.expand", include_str!("../../../lib/norn/expand.norn")),
    ("norn.deps", include_str!("../../../lib/norn/deps.norn")),
    ("norn.uri", include_str!("../../../lib/norn/uri.norn")),
    ("norn.http", include_str!("../../../lib/norn/http.norn")),
];

/// `(%find-lib "a.b-c")`: the source of library `a.b-c` and where it came
/// from — embedded, or `a/b-c.norn` under a folder added at run time, a
/// directory on `NORN_PATH` (colon-separated) or the current directory.
/// Folders added at run time (`(add-lib-path! dir)`): a project's source
/// folders and its packages' (M8 part 8.10), searched before NORN_PATH,
/// which is the fallback.
static LIB_PATH: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

pub(crate) fn add_lib_path(dir: String) {
    let mut p = LIB_PATH.write().unwrap();
    if !p.contains(&dir) {
        p.push(dir);
    }
}

/// Where compiled modules are cached (M8 part 8.6): NORN_CACHE (a folder,
/// or `off`), else the system's cache folder.
pub(crate) fn cache_dir() -> Option<String> {
    match std::env::var("NORN_CACHE") {
        Ok(v) if v == "off" => return None,
        Ok(v) if !v.is_empty() => return Some(v),
        _ => {}
    }
    if let Ok(x) = std::env::var("XDG_CACHE_HOME")
        && !x.is_empty()
    {
        return Some(format!("{x}/norn"));
    }
    let home = std::env::var("HOME").ok()?;
    Some(if cfg!(target_os = "macos") { format!("{home}/Library/Caches/norn") } else { format!("{home}/.cache/norn") })
}

pub(crate) fn find_lib(name: &str) -> Option<(String, String)> {
    if let Some((_, src)) = LIBS.iter().chain(crate::ext::libs()).find(|(n, _)| *n == name) {
        return Some(((*src).to_owned(), format!("<{name}>")));
    }
    let rel = format!("{}.norn", name.replace('.', "/"));
    let path = std::env::var("NORN_PATH").unwrap_or_default();
    let added = LIB_PATH.read().unwrap().clone();
    added.iter().map(String::as_str).chain(path.split(':').filter(|d| !d.is_empty())).chain(std::iter::once(".")).find_map(|dir| {
        let p = std::path::Path::new(dir).join(&rel);
        std::fs::read_to_string(&p).ok().map(|src| (src, p.to_string_lossy().into_owned()))
    })
}

/// A reading error, as a ParseError with :source and :line.
fn parse_error(msg: String, source: &str, line: usize) -> Error {
    Error::of("ParseError", msg).with("source", crate::vm::ErrData::Str(source.to_owned())).with("line", crate::vm::ErrData::Int(line as i64))
}

/// A VM for carrier `id`, positioned in norn.core.
pub(crate) fn carrier_vm(id: usize) -> Vm {
    let mut m = Mutator::new();
    norn_rt::runtime::init(&mut m);
    let core_sym = rt().symbol(&mut m, None, "norn.core");
    let core = rt().find_or_create_ns(core_sym);
    Vm::bare(m, core, id)
}

/// Install primitives and load core.norn (carrier 0, before anything runs).
pub(crate) fn boot(vm: &mut Vm) -> Result<()> {
    crate::prims::install(vm);
    crate::ext::install(vm);
    vm.load_str(CORE_SRC, "core.norn")?;
    // The real compiler, compiled by the seed compiler. From here on it
    // compiles everything (unless NORN_COMPILER=seed, kept for comparison).
    vm.load_str(COMPILER_SRC, "compiler.norn")?;
    if std::env::var("NORN_COMPILER").as_deref() != Ok("seed") {
        vm.load_str("(norn.compiler/%install!)", "<boot>")?;
        // The hygienic expander (M8), compiled by the compiler; from here
        // on every form goes through it (unless NORN_EXPANDER=off).
        if std::env::var("NORN_EXPANDER").as_deref() != Ok("off") {
            vm.load_str(EXPANDER_SRC, "expander.norn")?;
            vm.load_str("(norn.expander/%install!)", "<boot>")?;
        }
    }
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
            e.msg = format!("while expanding {}: {}", norn_rt::print::pr_str(var, true), e.msg);
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
    /// becomes `(norn.core/%eval-all '(forms…))`, so its forms are compiled
    /// one at a time — a macro defined early in it can be used later in it.
    ///
    /// Macro expansion and compilation run nested: non-suspendable and
    /// GC-inhibited, because the compiler holds values in Rust.
    pub(crate) fn compile_for_eval(&mut self, form: Value) -> Result<Value> {
        self.nested += 1;
        let r = (|| {
            let form = self.macroexpand(form)?;
            let form = if is_do(self, form) {
                let eval_all = rt().symbol(&mut self.m, Some("norn.core"), "%eval-all");
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
                Err(e) => return Err(parse_error(format!("{source_name}:{}: {e}", r.line()), source_name, r.line())),
            };
            last = self.eval(form).map_err(|mut e| {
                e.msg = format!("{source_name}:{line}: {}", e.msg);
                e
            })?;
        }
    }
}

/// `(%read r)`: the next form from a reader object, or the EOF marker.
/// `(%form-at src line)`: the source text of the first form starting on or
/// after line `line` (1-based) of `src`, or nil. For `source`.
pub(crate) fn form_at(vm: &mut Vm, src: &str, line: usize) -> Result<Value> {
    let mut off = 0;
    for _ in 1..line.max(1) {
        match src[off..].find('\n') {
            Some(i) => off += i + 1,
            None => return Ok(Value::NIL),
        }
    }
    let mut rd = Reader::new_at(src, off);
    if rd.at_end() {
        return Ok(Value::NIL);
    }
    let start = rd.pos();
    match rd.read(vm) {
        Ok(_) => Ok(norn_rt::string::str_new(&mut vm.m, &src[start..rd.pos()])),
        Err(_) => Ok(Value::NIL),
    }
}

/// `(%read-syntax reader)`: the next form and where its parts came from,
/// `[form location-tree]` (reader.rs), or the EOF marker.
pub(crate) fn read_syntax_next(vm: &mut Vm, r: Value) -> Result<Value> {
    let src = r.field(0).as_str();
    let pos = r.word(1) as usize;
    let mut rd = Reader::new_at(src, pos).with_locations();
    let _ = rd.at_end();
    r.set_word(3, rd.line() as u64);
    let res = rd.read(vm);
    r.set_word(1, rd.pos() as u64);
    match res {
        Ok(v) => {
            let loc = match rd.take_locations() {
                Some(l) => l.to_value(vm),
                None => Value::NIL,
            };
            Ok(norn_rt::vector::vec_from_slice(&mut vm.m, &[v, loc]))
        }
        Err(ReadError::Eof) => Ok(Value::EOF),
        Err(e) => {
            let name = norn_rt::print::pr_str(r.field(2), false);
            Err(parse_error(format!("{name}:{}: {e}", rd.line()), &name, rd.line()))
        }
    }
}

pub(crate) fn read_next(vm: &mut Vm, r: Value) -> Result<Value> {
    let src = r.field(0).as_str();
    let pos = r.word(1) as usize;
    let mut rd = Reader::new_at(src, pos);
    let _ = rd.at_end();
    r.set_word(3, rd.line() as u64);
    let res = rd.read(vm);
    r.set_word(1, rd.pos() as u64);
    match res {
        Ok(v) => Ok(v),
        Err(ReadError::Eof) => Ok(Value::EOF),
        Err(e) => {
            let name = norn_rt::print::pr_str(r.field(2), false);
            Err(parse_error(format!("{name}:{}: {e}", rd.line()), &name, rd.line()))
        }
    }
}
