//! Evaluation: boot (loading lib/core/core.skn), macro expansion, and
//! compiling forms for the `eval` intrinsic.
//!
//! After boot, all evaluation happens in fibers: `eval` compiles a form to a
//! thunk and calls it *in the current fiber* (so evaluated code can park), and
//! `load-string` / `load-file` / the REPL are Skein loops over `%read` + `eval`.
//! Only boot uses the Rust-side `Vm::eval` (nested, non-suspendable runs).

use crate::compiler::compile_toplevel;
use crate::reader::{ReadError, Reader};
use crate::vm::{Error, Result, Vm};
use skein_rt::list::list_from;
use skein_rt::runtime::rt;
use skein_rt::seq::SeqIter;
use skein_rt::{Mutator, Value};

pub const CORE_SRC: &str = include_str!("../../../lib/core/core.skn");
pub const COMPILER_SRC: &str = include_str!("../../../lib/compiler/compiler.skn");
pub const EXPANDER_SRC: &str = include_str!("../../../lib/compiler/expander.skn");

/// The standard libraries (`lib/skein/*.skn`), embedded so `skein` is a
/// single file. `(require 'skein.x)` loads one the first time it is needed.
pub const LIBS: &[(&str, &str)] = &[
    ("skein.bytes", include_str!("../../../lib/skein/bytes.skn")),
    ("skein.buffer", include_str!("../../../lib/skein/buffer.skn")),
    ("skein.io", include_str!("../../../lib/skein/io.skn")),
    ("skein.fs", include_str!("../../../lib/skein/fs.skn")),
    ("skein.sys", include_str!("../../../lib/skein/sys.skn")),
    ("skein.process", include_str!("../../../lib/skein/process.skn")),
    ("skein.time", include_str!("../../../lib/skein/time.skn")),
    ("skein.signal", include_str!("../../../lib/skein/signal.skn")),
    ("skein.net", include_str!("../../../lib/skein/net.skn")),
    ("skein.ffi", include_str!("../../../lib/skein/ffi.skn")),
    ("skein.rope", include_str!("../../../lib/skein/rope.skn")),
    ("skein.markdown", include_str!("../../../lib/skein/markdown.skn")),
    ("skein.string", include_str!("../../../lib/skein/string.skn")),
    ("skein.regex", include_str!("../../../lib/skein/regex.skn")),
    ("skein.json", include_str!("../../../lib/skein/json.skn")),
    ("skein.ndjson", include_str!("../../../lib/skein/ndjson.skn")),
    ("skein.xml", include_str!("../../../lib/skein/xml.skn")),
    ("skein.yaml", include_str!("../../../lib/skein/yaml.skn")),
    ("skein.cbor", include_str!("../../../lib/skein/cbor.skn")),
    ("skein.edn", include_str!("../../../lib/skein/edn.skn")),
    ("skein.format", include_str!("../../../lib/skein/format.skn")),
    ("skein.pprint", include_str!("../../../lib/skein/pprint.skn")),
    ("skein.math", include_str!("../../../lib/skein/math.skn")),
    ("skein.decimal", include_str!("../../../lib/skein/decimal.skn")),
    ("skein.set", include_str!("../../../lib/skein/set.skn")),
    ("skein.walk", include_str!("../../../lib/skein/walk.skn")),
    ("skein.xref", include_str!("../../../lib/skein/xref.skn")),
    ("skein.shake", include_str!("../../../lib/skein/shake.skn")),
    ("skein.prof", include_str!("../../../lib/skein/prof.skn")),
    ("skein.cover", include_str!("../../../lib/skein/cover.skn")),
    ("skein.lsp", include_str!("../../../lib/skein/lsp.skn")),
    ("skein.random", include_str!("../../../lib/skein/random.skn")),
    ("skein.async", include_str!("../../../lib/skein/async.skn")),
    ("skein.sync", include_str!("../../../lib/skein/sync.skn")),
    ("skein.actor", include_str!("../../../lib/skein/actor.skn")),
    ("skein.stm", include_str!("../../../lib/skein/stm.skn")),
    ("skein.sqlite", include_str!("../../../lib/skein/sqlite.skn")),
    ("skein.bin", include_str!("../../../lib/skein/bin.skn")),
    ("skein.schedule", include_str!("../../../lib/skein/schedule.skn")),
    ("skein.zip", include_str!("../../../lib/skein/zip.skn")),
    ("skein.pe", include_str!("../../../lib/skein/pe.skn")),
    ("skein.toml", include_str!("../../../lib/skein/toml.skn")),
    ("skein.csv", include_str!("../../../lib/skein/csv.skn")),
    ("skein.data", include_str!("../../../lib/skein/data.skn")),
    ("skein.uuid", include_str!("../../../lib/skein/uuid.skn")),
    ("skein.crypto", include_str!("../../../lib/skein/crypto.skn")),
    ("skein.compress", include_str!("../../../lib/skein/compress.skn")),
    ("skein.cache", include_str!("../../../lib/skein/cache.skn")),
    ("skein.log", include_str!("../../../lib/skein/log.skn")),
    ("skein.test", include_str!("../../../lib/skein/test.skn")),
    ("skein.property", include_str!("../../../lib/skein/property.skn")),
    ("skein.cli", include_str!("../../../lib/skein/cli.skn")),
    ("skein.terminal", include_str!("../../../lib/skein/terminal.skn")),
    ("skein.package", include_str!("../../../lib/skein/package.skn")),
    ("skein.new", include_str!("../../../lib/skein/new.skn")),
    ("skein.run", include_str!("../../../lib/skein/run.skn")),
    ("skein.dev", include_str!("../../../lib/skein/dev.skn")),
    ("skein.core-docs", include_str!("../../../lib/skein/core-docs.skn")),
    ("skein.api-diff", include_str!("../../../lib/skein/api-diff.skn")),
    ("skein.fmt", include_str!("../../../lib/skein/fmt.skn")),
    ("skein.lint", include_str!("../../../lib/skein/lint.skn")),
    ("skein.bench", include_str!("../../../lib/skein/bench.skn")),
    ("skein.doc", include_str!("../../../lib/skein/doc.skn")),
    ("skein.image", include_str!("../../../lib/skein/image.skn")),
    ("skein.check", include_str!("../../../lib/skein/check.skn")),
    ("skein.syntax", include_str!("../../../lib/skein/syntax.skn")),
    ("skein.validate", include_str!("../../../lib/skein/validate.skn")),
    ("skein.expand", include_str!("../../../lib/skein/expand.skn")),
    ("skein.deps", include_str!("../../../lib/skein/deps.skn")),
    ("skein.uri", include_str!("../../../lib/skein/uri.skn")),
    ("skein.http", include_str!("../../../lib/skein/http.skn")),
];

/// `(%find-lib "a.b-c")`: the source of library `a.b-c` and where it came
/// from — embedded, or `a/b-c.skn` under a folder added at run time, a
/// directory on `SKEIN_PATH` (separated as the system's `PATH` is) or the
/// current directory.
/// Folders added at run time (`(add-lib-path! dir)`): a project's source
/// folders and its packages' (M8 part 8.10), searched before SKEIN_PATH,
/// which is the fallback.
static LIB_PATH: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

pub(crate) fn add_lib_path(dir: String) {
    let mut p = LIB_PATH.write().unwrap();
    if !p.contains(&dir) {
        p.push(dir);
    }
}

/// Where compiled modules are cached (M8 part 8.6): SKEIN_CACHE (a folder,
/// or `off`), else the system's cache folder.
pub(crate) fn cache_dir() -> Option<String> {
    match std::env::var("SKEIN_CACHE") {
        Ok(v) if v == "off" => return None,
        Ok(v) if !v.is_empty() => return Some(v),
        _ => {}
    }
    if let Ok(x) = std::env::var("XDG_CACHE_HOME")
        && !x.is_empty()
    {
        return Some(format!("{x}/skein"));
    }
    if cfg!(windows) {
        let local = std::env::var("LOCALAPPDATA").ok().filter(|d| !d.is_empty())?;
        return Some(format!("{local}\\skein\\cache"));
    }
    let home = std::env::var("HOME").ok()?;
    Some(if cfg!(target_os = "macos") { format!("{home}/Library/Caches/skein") } else { format!("{home}/.cache/skein") })
}

pub(crate) fn find_lib(name: &str) -> Option<(String, String)> {
    if let Some((_, src)) = LIBS.iter().chain(crate::ext::libs()).find(|(n, _)| *n == name) {
        return Some(((*src).to_owned(), format!("<{name}>")));
    }
    let rel = format!("{}.skn", name.replace('.', "/"));
    // (`;` between folders on Windows, where a folder may hold a `:`)
    let path: Vec<String> = std::env::var_os("SKEIN_PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.to_string_lossy().into_owned()).filter(|d| !d.is_empty()).collect())
        .unwrap_or_default();
    let added = LIB_PATH.read().unwrap().clone();
    added.iter().chain(path.iter()).map(String::as_str).chain(std::iter::once(".")).find_map(|dir| {
        let p = std::path::Path::new(dir).join(&rel);
        std::fs::read_to_string(&p).ok().map(|src| (src, p.to_string_lossy().into_owned()))
    })
}

/// A reading error, as a ParseError with :source, :line and :column.
pub(crate) fn parse_error(msg: String, source: &str, line: usize, column: usize) -> Error {
    Error::of("ParseError", msg)
        .with("source", crate::vm::ErrData::Str(source.to_owned()))
        .with("line", crate::vm::ErrData::Int(line as i64))
        .with("column", crate::vm::ErrData::Int(column as i64))
}

/// A VM for carrier `id`, positioned in skein.core.
pub(crate) fn carrier_vm(id: usize) -> Vm {
    let mut m = Mutator::new();
    skein_rt::runtime::init(&mut m);
    let core_sym = rt().symbol(&mut m, None, "skein.core");
    let core = rt().find_or_create_ns(core_sym);
    Vm::bare(m, core, id)
}

/// Install primitives and load core.skn (carrier 0, before anything runs).
pub(crate) fn boot(vm: &mut Vm) -> Result<()> {
    crate::prims::install(vm);
    crate::ext::install(vm);
    vm.load_str(CORE_SRC, "core.skn")?;
    // The real compiler, compiled by the seed compiler. From here on it
    // compiles everything (unless SKEIN_COMPILER=seed, kept for comparison).
    vm.load_str(COMPILER_SRC, "compiler.skn")?;
    if std::env::var("SKEIN_COMPILER").as_deref() != Ok("seed") {
        vm.load_str("(skein.compiler/%install!)", "<boot>")?;
        // The hygienic expander (M8), compiled by the compiler; from here
        // on every form goes through it (unless SKEIN_EXPANDER=off).
        if std::env::var("SKEIN_EXPANDER").as_deref() != Ok("off") {
            vm.load_str(EXPANDER_SRC, "expander.skn")?;
            vm.load_str("(skein.expander/%install!)", "<boot>")?;
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
            e.msg = format!("while expanding {}: {}", skein_rt::print::pr_str(var, true), e.msg);
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
    /// becomes `(skein.core/%eval-all '(forms…))`, so its forms are compiled
    /// one at a time — a macro defined early in it can be used later in it.
    ///
    /// Macro expansion and compilation run nested: non-suspendable and
    /// GC-inhibited, because the compiler holds values in Rust.
    pub(crate) fn compile_for_eval(&mut self, form: Value) -> Result<Value> {
        self.nested += 1;
        let r = (|| {
            let form = self.macroexpand(form)?;
            let form = if is_do(self, form) {
                let eval_all = rt().symbol(&mut self.m, Some("skein.core"), "%eval-all");
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
                Err(e) => return Err(parse_error(format!("{source_name}:{}: {e}", r.line()), source_name, r.line(), r.column())),
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
        Ok(_) => Ok(skein_rt::string::str_new(&mut vm.m, &src[start..rd.pos()])),
        Err(_) => Ok(Value::NIL),
    }
}

/// `(%next-form-text reader)`: the text of the form reader will read next,
/// as written (comments and line breaks kept), without reading it; nil at
/// the end or if it doesn't read.
pub(crate) fn next_form_text(vm: &mut Vm, r: Value) -> Result<Value> {
    let src = r.field(0).as_str();
    let mut rd = Reader::new_at(src, r.word(1) as usize);
    if rd.at_end() {
        return Ok(Value::NIL);
    }
    let start = rd.pos();
    match rd.read(vm) {
        Ok(_) => Ok(skein_rt::string::str_new(&mut vm.m, &src[start..rd.pos()])),
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
            Ok(skein_rt::vector::vec_from_slice(&mut vm.m, &[v, loc]))
        }
        Err(ReadError::Eof) => Ok(Value::EOF),
        Err(e) => {
            let name = skein_rt::print::pr_str(r.field(2), false);
            Err(parse_error(format!("{name}:{}: {e}", rd.line()), &name, rd.line(), rd.column()))
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
            let name = skein_rt::print::pr_str(r.field(2), false);
            Err(parse_error(format!("{name}:{}: {e}", rd.line()), &name, rd.line(), rd.column()))
        }
    }
}
