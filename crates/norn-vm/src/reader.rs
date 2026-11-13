//! The bootstrap reader (design.md §2). Reads text into Norn data.
//!
//! M0 limits, each reported as a clear error rather than silently misread:
//! ratios that don't reduce to integers, bignums, regex literals, complex
//! numbers. Metadata (`^…`) is read and discarded. Source locations are
//! tracked only for error messages; syntax objects arrive with hygiene (M6).

use crate::vm::Vm;
use norn_rt::list::list_from;
use norn_rt::map::{map_assoc, map_empty, set_conj, set_empty};
use norn_rt::runtime::rt;
use norn_rt::seq::SeqIter;
use norn_rt::string::str_new;
use norn_rt::vector::vec_from_slice;
use norn_rt::{Kind, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, PartialEq)]
pub enum ReadError {
    /// No more forms (clean end of input).
    Eof,
    /// Input ended in the middle of a form; the REPL reads another line.
    Incomplete(String),
    Syntax(String),
    /// Internal: hit a closing delimiter (not consumed).
    Closer(char),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Eof => write!(f, "EOF"),
            ReadError::Incomplete(s) | ReadError::Syntax(s) => write!(f, "{s}"),
            ReadError::Closer(c) => write!(f, "Unmatched delimiter: {c}"),
        }
    }
}

type RResult<T> = Result<T, ReadError>;

static GENSYM_ID: AtomicU64 = AtomicU64::new(1);

/// Process-unique number for generated symbols.
pub fn next_id() -> u64 {
    GENSYM_ID.fetch_add(1, Ordering::Relaxed)
}

/// The gensym counter, saved in images so that names generated after a
/// restore don't repeat ones already in the image.
pub fn gensym_counter() -> u64 {
    GENSYM_ID.load(Ordering::Relaxed)
}

pub fn set_gensym_counter(n: u64) {
    GENSYM_ID.fetch_max(n, Ordering::Relaxed);
}

#[derive(Default)]
struct FnArgs {
    max: usize,
    args: HashMap<usize, Value>,
    rest: Option<Value>,
}

pub struct Reader<'a> {
    src: &'a str,
    pos: usize,
    fn_args: Option<FnArgs>,
}

const SPECIALS: &[&str] = &["quote", "if", "do", "def", "var", "fn*", "let*", "letrec*", "loop*", "recur", "&"];

pub fn is_special(name: &str) -> bool {
    SPECIALS.contains(&name)
}

fn is_terminator(c: char) -> bool {
    c.is_whitespace() || matches!(c, ',' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | ';' | '`' | '~' | '^' | '@' | '\\')
}

impl<'a> Reader<'a> {
    pub fn new(src: &'a str) -> Self {
        Reader { src, pos: 0, fn_args: None }
    }

    /// A reader starting at byte offset `pos`.
    pub fn new_at(src: &'a str, pos: usize) -> Self {
        Reader { src, pos: pos.min(src.len()), fn_args: None }
    }

    /// Current byte offset.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// 1-based line number of the current position.
    pub fn line(&self) -> usize {
        self.src[..self.pos].matches('\n').count() + 1
    }

    pub fn at_end(&mut self) -> bool {
        self.skip_ws();
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() || c == ',' {
                self.bump();
            } else if c == ';' {
                self.skip_line();
            } else {
                break;
            }
        }
    }

    fn skip_line(&mut self) {
        while let Some(c) = self.bump() {
            if c == '\n' {
                break;
            }
        }
    }

    fn skip_block_comment(&mut self) -> RResult<()> {
        // Already consumed "#|". Block comments nest.
        let mut depth = 1;
        while depth > 0 {
            match self.bump() {
                None => return Err(ReadError::Incomplete("EOF in #| comment".into())),
                Some('|') if self.peek() == Some('#') => {
                    self.bump();
                    depth -= 1;
                }
                Some('#') if self.peek() == Some('|') => {
                    self.bump();
                    depth += 1;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn token(&mut self) -> &'a str {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if is_terminator(c) {
                break;
            }
            self.bump();
        }
        &self.src[start..self.pos]
    }

    /// Read the next form. `Err(Eof)` when input is exhausted.
    pub fn read(&mut self, vm: &mut Vm) -> RResult<Value> {
        match self.read_inner(vm) {
            Err(ReadError::Closer(c)) => {
                self.bump();
                Err(ReadError::Syntax(format!("Unmatched delimiter: {c}")))
            }
            r => r,
        }
    }

    fn read_required(&mut self, vm: &mut Vm) -> RResult<Value> {
        match self.read_inner(vm) {
            Err(ReadError::Eof) => Err(ReadError::Incomplete("EOF while reading".into())),
            Err(ReadError::Closer(c)) => Err(ReadError::Syntax(format!("Unexpected {c}"))),
            r => r,
        }
    }

    fn read_inner(&mut self, vm: &mut Vm) -> RResult<Value> {
        loop {
            self.skip_ws();
            let Some(c) = self.peek() else { return Err(ReadError::Eof) };
            match c {
                ')' | ']' | '}' => return Err(ReadError::Closer(c)),
                '(' => {
                    self.bump();
                    let xs = self.read_delimited(vm, ')')?;
                    return Ok(list_from(&mut vm.m, &xs));
                }
                '[' => {
                    self.bump();
                    let xs = self.read_delimited(vm, ']')?;
                    return Ok(vec_from_slice(&mut vm.m, &xs));
                }
                '{' => {
                    self.bump();
                    let xs = self.read_delimited(vm, '}')?;
                    if xs.len() % 2 != 0 {
                        return Err(ReadError::Syntax("Map literal must contain an even number of forms".into()));
                    }
                    let mut map = map_empty();
                    for kv in xs.chunks(2) {
                        map = map_assoc(&mut vm.m, map, kv[0], kv[1]);
                    }
                    return Ok(map);
                }
                '"' => {
                    self.bump();
                    return self.read_string(vm);
                }
                '\'' => {
                    self.bump();
                    return self.wrap(vm, "quote");
                }
                '`' => {
                    self.bump();
                    let form = self.read_required(vm)?;
                    let mut gensyms = HashMap::new();
                    return syntax_quote(vm, form, &mut gensyms).map_err(ReadError::Syntax);
                }
                '~' => {
                    self.bump();
                    if self.peek() == Some('@') {
                        self.bump();
                        return self.wrap(vm, "unquote-splicing");
                    }
                    return self.wrap(vm, "unquote");
                }
                '@' => {
                    self.bump();
                    return self.wrap(vm, "deref");
                }
                '^' => {
                    // Metadata is read and dropped in M0.
                    self.bump();
                    self.read_required(vm)?;
                    return self.read_required(vm);
                }
                '\\' => {
                    self.bump();
                    return self.read_char();
                }
                '#' => {
                    if let Some(v) = self.read_dispatch(vm)? {
                        return Ok(v);
                    }
                    // A comment or discarded form: keep going.
                }
                _ => {
                    let tok = self.token();
                    return self.parse_token(vm, tok);
                }
            }
        }
    }

    fn wrap(&mut self, vm: &mut Vm, head: &str) -> RResult<Value> {
        let form = self.read_required(vm)?;
        let h = vm.sym(head);
        Ok(list_from(&mut vm.m, &[h, form]))
    }

    fn read_delimited(&mut self, vm: &mut Vm, close: char) -> RResult<Vec<Value>> {
        let mut xs = Vec::new();
        loop {
            match self.read_inner(vm) {
                Ok(v) => xs.push(v),
                Err(ReadError::Closer(c)) if c == close => {
                    self.bump();
                    return Ok(xs);
                }
                Err(ReadError::Closer(c)) => {
                    return Err(ReadError::Syntax(format!("Mismatched delimiter: expected {close}, found {c}")));
                }
                Err(ReadError::Eof) => return Err(ReadError::Incomplete(format!("EOF while reading, expected {close}"))),
                Err(e) => return Err(e),
            }
        }
    }

    /// `#…` forms. `Ok(None)` for comments and discards.
    fn read_dispatch(&mut self, vm: &mut Vm) -> RResult<Option<Value>> {
        self.bump(); // '#'
        let Some(c) = self.peek() else {
            return Err(ReadError::Incomplete("EOF after #".into()));
        };
        match c {
            '{' => {
                self.bump();
                let xs = self.read_delimited(vm, '}')?;
                let mut s = set_empty();
                for x in xs {
                    s = set_conj(&mut vm.m, s, x);
                }
                Ok(Some(s))
            }
            '(' => {
                self.bump();
                self.read_fn_literal(vm).map(Some)
            }
            '\'' => {
                self.bump();
                self.wrap(vm, "var").map(Some)
            }
            '_' | ';' => {
                self.bump();
                self.read_required(vm)?;
                Ok(None)
            }
            '|' => {
                self.bump();
                self.skip_block_comment()?;
                Ok(None)
            }
            '!' => {
                self.skip_line();
                Ok(None)
            }
            '#' => {
                self.bump();
                match self.token() {
                    "Inf" => Ok(Some(norn_rt::string::float_new(&mut vm.m, f64::INFINITY))),
                    "-Inf" => Ok(Some(norn_rt::string::float_new(&mut vm.m, f64::NEG_INFINITY))),
                    "NaN" => Ok(Some(norn_rt::string::float_new(&mut vm.m, f64::NAN))),
                    t => Err(ReadError::Syntax(format!("Unknown symbolic value: ##{t}"))),
                }
            }
            '"' => {
                // #"pattern": backslashes stay as written; \" is a quote.
                self.bump();
                let mut pat = String::new();
                loop {
                    match self.peek() {
                        None => return Err(ReadError::Incomplete("Unterminated regex literal".into())),
                        Some('"') => {
                            self.bump();
                            break;
                        }
                        Some('\\') => {
                            self.bump();
                            match self.peek() {
                                Some('"') => pat.push('"'),
                                Some(c) => {
                                    pat.push('\\');
                                    pat.push(c);
                                }
                                None => return Err(ReadError::Incomplete("Unterminated regex literal".into())),
                            }
                            self.bump();
                        }
                        Some(c) => {
                            pat.push(c);
                            self.bump();
                        }
                    }
                }
                let s = norn_rt::string::str_new(&mut vm.m, &pat);
                crate::text::regex_value(&mut vm.m, s).map(Some).map_err(|e| ReadError::Syntax(e.msg))
            }
            _ => {
                let tok = self.token();
                if tok == "bytes" {
                    // #bytes "68656c6c6f"
                    let form = self.read_required(vm)?;
                    if !form.is(Kind::String) {
                        return Err(ReadError::Syntax("#bytes must be followed by a hex string".into()));
                    }
                    let b = crate::codec::hex_decode(form.as_str()).map_err(ReadError::Syntax)?;
                    return Ok(Some(norn_rt::string::bytes_new(&mut vm.m, &b)));
                }
                if tok == "lang" {
                    // `#lang norn` selects the base language (design.md §8.1).
                    let start = self.pos;
                    self.skip_line();
                    let lang = self.src[start..self.pos].trim();
                    if lang != "norn" {
                        return Err(ReadError::Syntax(format!("Unknown #lang: {lang} (only `norn` exists so far)")));
                    }
                    Ok(None)
                } else {
                    Err(ReadError::Syntax(format!("Unknown reader dispatch: #{tok}")))
                }
            }
        }
    }

    fn read_fn_literal(&mut self, vm: &mut Vm) -> RResult<Value> {
        if self.fn_args.is_some() {
            return Err(ReadError::Syntax("Nested #()s are not allowed".into()));
        }
        self.fn_args = Some(FnArgs::default());
        let body = self.read_delimited(vm, ')');
        let args = self.fn_args.take().unwrap();
        let body = body?;
        let mut params = Vec::new();
        for i in 1..=args.max {
            let p = match args.args.get(&i) {
                Some(&s) => s,
                None => vm.sym(&format!("p{i}__{}", next_id())),
            };
            params.push(p);
        }
        if let Some(r) = args.rest {
            params.push(vm.sym("&"));
            params.push(r);
        }
        let params = vec_from_slice(&mut vm.m, &params);
        let body = list_from(&mut vm.m, &body);
        let fn_star = vm.sym("fn*");
        Ok(list_from(&mut vm.m, &[fn_star, params, body]))
    }

    fn fn_arg(&mut self, vm: &mut Vm, tok: &str) -> RResult<Option<Value>> {
        let Some(args) = self.fn_args.as_mut() else { return Ok(None) };
        let rest = &tok[1..];
        if rest == "&" {
            let s = match args.rest {
                Some(s) => s,
                None => {
                    let s = rt().symbol(&mut vm.m, None, &format!("rest__{}", next_id()));
                    args.rest = Some(s);
                    s
                }
            };
            return Ok(Some(s));
        }
        let n: usize = if rest.is_empty() {
            1
        } else {
            match rest.parse() {
                Ok(n) if n >= 1 => n,
                _ => return Ok(None),
            }
        };
        args.max = args.max.max(n);
        if let Some(&s) = args.args.get(&n) {
            return Ok(Some(s));
        }
        let s = rt().symbol(&mut vm.m, None, &format!("p{n}__{}", next_id()));
        args.args.insert(n, s);
        Ok(Some(s))
    }

    fn read_string(&mut self, vm: &mut Vm) -> RResult<Value> {
        let mut s = String::new();
        loop {
            match self.bump() {
                None => return Err(ReadError::Incomplete("EOF while reading string".into())),
                Some('"') => return Ok(str_new(&mut vm.m, &s)),
                Some('\\') => match self.bump() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('r') => s.push('\r'),
                    Some('b') => s.push('\u{8}'),
                    Some('f') => s.push('\u{c}'),
                    Some('0') => s.push('\0'),
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some('u') => {
                        let start = self.pos;
                        for _ in 0..4 {
                            self.bump();
                        }
                        let hex = &self.src[start..self.pos];
                        let c = u32::from_str_radix(hex, 16)
                            .ok()
                            .and_then(char::from_u32)
                            .ok_or_else(|| ReadError::Syntax(format!("Bad unicode escape: \\u{hex}")))?;
                        s.push(c);
                    }
                    Some(c) => return Err(ReadError::Syntax(format!("Unsupported escape character: \\{c}"))),
                    None => return Err(ReadError::Incomplete("EOF while reading string".into())),
                },
                Some(c) => s.push(c),
            }
        }
    }

    fn read_char(&mut self) -> RResult<Value> {
        let Some(first) = self.bump() else {
            return Err(ReadError::Incomplete("EOF while reading character".into()));
        };
        let start = self.pos - first.len_utf8();
        while let Some(c) = self.peek() {
            if is_terminator(c) {
                break;
            }
            self.bump();
        }
        let tok = &self.src[start..self.pos];
        if tok.chars().count() == 1 {
            return Ok(Value::char(first));
        }
        let c = match tok {
            "newline" => '\n',
            "space" => ' ',
            "tab" => '\t',
            "return" => '\r',
            "backspace" => '\u{8}',
            "formfeed" => '\u{c}',
            "nul" => '\0',
            _ if tok.starts_with('u') && tok.len() == 5 => u32::from_str_radix(&tok[1..], 16)
                .ok()
                .and_then(char::from_u32)
                .ok_or_else(|| ReadError::Syntax(format!("Invalid character: \\{tok}")))?,
            _ => return Err(ReadError::Syntax(format!("Unsupported character: \\{tok}"))),
        };
        Ok(Value::char(c))
    }

    fn parse_token(&mut self, vm: &mut Vm, tok: &str) -> RResult<Value> {
        if tok.is_empty() {
            let c = self.bump().unwrap_or(' ');
            return Err(ReadError::Syntax(format!("Unexpected character: {c}")));
        }
        let mut chars = tok.chars();
        let c0 = chars.next().unwrap();
        let c1 = chars.next();
        if c0.is_ascii_digit() || ((c0 == '+' || c0 == '-') && c1.is_some_and(|c| c.is_ascii_digit())) {
            return parse_number(vm, tok);
        }
        match tok {
            "nil" => return Ok(Value::NIL),
            "true" => return Ok(Value::TRUE),
            "false" => return Ok(Value::FALSE),
            _ => {}
        }
        if let Some(name) = tok.strip_prefix("::") {
            let ns = vm.ns.name.sym_name().to_owned();
            return Ok(rt().keyword(&mut vm.m, Some(&ns), name));
        }
        if let Some(name) = tok.strip_prefix(':') {
            if name.is_empty() {
                return Err(ReadError::Syntax("Invalid token: :".into()));
            }
            return Ok(rt().keyword_from(&mut vm.m, name));
        }
        if c0 == '%'
            && let Some(s) = self.fn_arg(vm, tok)?
        {
            return Ok(s);
        }
        Ok(vm.sym(tok))
    }
}

fn parse_number(vm: &mut Vm, tok: &str) -> RResult<Value> {
    let invalid = || ReadError::Syntax(format!("Invalid number: {tok}"));
    // An integer of any size: decimal, or hex with 0x.
    let parse_int = |s: &str| -> RResult<num_bigint::BigInt> {
        let (neg, body) = match s.strip_prefix('-') {
            Some(b) => (true, b),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        let (digits, radix) = match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            Some(h) => (h, 16),
            None => (body, 10),
        };
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return Err(invalid());
        }
        let n = norn_rt::numbers::parse_int(digits, radix).ok_or_else(invalid)?;
        Ok(if neg { -n } else { n })
    };
    if let Some((n, d)) = tok.split_once('/') {
        let (n, d) = (parse_int(n)?, parse_int(d)?);
        if norn_rt::numbers::is_zero(&d) {
            return Err(ReadError::Syntax(format!("Divide by zero in ratio: {tok}")));
        }
        return Ok(norn_rt::numbers::make_ratio(&mut vm.m, &n, &d));
    }
    let is_hex = tok.contains("0x") || tok.contains("0X");
    if !is_hex && (tok.contains('.') || tok.contains('e') || tok.contains('E')) {
        return tok.parse::<f64>().map(|f| norn_rt::string::float_new(&mut vm.m, f)).map_err(|_| invalid());
    }
    Ok(norn_rt::numbers::int_value(&mut vm.m, &parse_int(tok)?))
}

// ---- syntax-quote ---------------------------------------------------------------

fn core_sym(vm: &mut Vm, name: &str) -> Value {
    rt().symbol(&mut vm.m, Some("norn.core"), name)
}

fn is_form(vm: &mut Vm, v: Value, head: &str) -> bool {
    v.is_cons() && v.car().is_symbol() && v.car() == vm.sym(head)
}

fn second(v: Value) -> Value {
    let d = v.cdr();
    if d.is_cons() { d.car() } else { Value::NIL }
}

/// Expand `` `form `` into code that builds it (Clojure semantics). Symbols are
/// namespace-qualified, `x#` becomes a per-template gensym, `~x` / `~@x` splice.
pub fn syntax_quote(vm: &mut Vm, form: Value, gensyms: &mut HashMap<String, Value>) -> Result<Value, String> {
    let quote = vm.sym("quote");
    let quoted = |vm: &mut Vm, v: Value| list_from(&mut vm.m, &[quote, v]);

    if form.is_symbol() {
        let name = form.sym_name();
        if let Some(q) = form.sym_ns() {
            // An alias of the current namespace stands for its full name.
            let qs = rt().symbol(&mut vm.m, None, q);
            let full = match vm.ns.alias_target(qs) {
                Some(target) => rt().symbol(&mut vm.m, Some(target.sym_name()), name),
                None => form,
            };
            return Ok(quoted(vm, full));
        }
        if is_special(name) {
            return Ok(quoted(vm, form));
        }
        if let Some(base) = name.strip_suffix('#') {
            let s = match gensyms.get(name) {
                Some(&s) => s,
                None => {
                    let s = vm.sym(&format!("{base}__{}__auto__", next_id()));
                    gensyms.insert(name.to_owned(), s);
                    s
                }
            };
            return Ok(quoted(vm, s));
        }
        let ns = match vm.resolve(form) {
            Some(var) => var.var_ns().sym_name().to_owned(),
            None => vm.ns.name.sym_name().to_owned(),
        };
        let q = rt().symbol(&mut vm.m, Some(&ns), name);
        return Ok(quoted(vm, q));
    }
    if is_form(vm, form, "unquote") {
        return Ok(second(form));
    }
    if is_form(vm, form, "unquote-splicing") {
        return Err("~@ used outside of a list".into());
    }
    let is_list = form.is_cons() || form.is(Kind::EmptyList);
    let is_coll = is_list || form.is(Kind::Vector) || form.is(Kind::Map) || form.is(Kind::Set);
    if !is_coll {
        // Self-evaluating (numbers, strings, keywords, …) or opaque.
        return Ok(if form.is_obj() && !matches!(form.obj_kind(), Kind::String | Kind::Keyword | Kind::Float) {
            quoted(vm, form)
        } else {
            form
        });
    }
    let items: Vec<Value> = if form.is(Kind::Map) {
        let mut v = Vec::new();
        form.map_for_each(|k, val| {
            v.push(k);
            v.push(val);
            true
        });
        v
    } else if form.is(Kind::Set) {
        let mut v = Vec::new();
        form.set_for_each(|k| {
            v.push(k);
            true
        });
        v
    } else {
        SeqIter::new(form).map(|it| it.collect()).unwrap_or_default()
    };
    let list_fn = core_sym(vm, "list");
    let mut parts = vec![core_sym(vm, "concat")];
    for x in items {
        if is_form(vm, x, "unquote-splicing") {
            parts.push(second(x));
        } else {
            let e = syntax_quote(vm, x, gensyms)?;
            parts.push(list_from(&mut vm.m, &[list_fn, e]));
        }
    }
    let concat = list_from(&mut vm.m, &parts);
    if is_list {
        let seq = core_sym(vm, "seq");
        if form.is(Kind::EmptyList) {
            return Ok(list_from(&mut vm.m, &[list_fn]));
        }
        return Ok(list_from(&mut vm.m, &[seq, concat]));
    }
    let ctor = match form.obj_kind() {
        Kind::Vector => "vector",
        Kind::Map => "hash-map",
        _ => "hash-set",
    };
    let apply = core_sym(vm, "apply");
    let ctor = core_sym(vm, ctor);
    Ok(list_from(&mut vm.m, &[apply, ctor, concat]))
}
