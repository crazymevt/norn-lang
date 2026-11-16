//! The bootstrap reader (design.md §2). Reads text into Norn data.
//!
//! M0 limits, each reported as a clear error rather than silently misread:
//! ratios that don't reduce to integers, bignums, regex literals, complex
//! numbers. Metadata (`^…`) is read and discarded.
//!
//! With locations on (`Reader::with_locations`), it also records where each
//! form came from, as a tree that parallels the form: a node is
//! `[line column start end kids elements]` (1-based line and column,
//! character offsets, `kids` the nodes of its parts, `elements` a map's or
//! set's parts in source order). A form the reader rewrites (syntax-quote,
//! `#(…)`) has kids that don't line up with its parts; `norn.syntax` gives
//! those parts the whole form's place.

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
    locs: Option<Locs>,
    /// How deeply the form being read nests (MAX_DEPTH at most).
    depth: usize,
}

/// How deeply a form may nest: reading recurses, and text nested deeper
/// (untrusted input, for norn.edn) would overflow the native stack.
const MAX_DEPTH: usize = 10_000;

/// Where a form came from: 1-based line and column, character offsets.
#[derive(Clone, Copy, Default)]
struct Place {
    line: usize,
    col: usize,
    at: usize,
}

/// A form being read: where it began, and its parts' nodes so far.
struct Frame {
    start: Place,
    kids: Vec<LocNode>,
    elems: Option<Vec<Value>>,
}

pub struct LocNode {
    start: Place,
    end: Place,
    kids: Vec<LocNode>,
    elems: Option<Vec<Value>>,
    /// The form's metadata (`^:private x`): a map, or nil.
    meta: Value,
}

impl LocNode {
    /// As Norn data: `[line column start end kids elements]`.
    pub fn to_value(&self, vm: &mut Vm) -> Value {
        let kids: Vec<Value> = self.kids.iter().map(|k| k.to_value(vm)).collect();
        let kids = vec_from_slice(&mut vm.m, &kids);
        let elems = match &self.elems {
            Some(xs) => vec_from_slice(&mut vm.m, xs),
            None => Value::NIL,
        };
        let n = |x: usize| Value::fix(x as i64);
        vec_from_slice(&mut vm.m, &[n(self.start.line), n(self.start.col), n(self.start.at), n(self.end.at), kids, elems, self.meta])
    }
}

/// Location tracking: the frames open, the finished top form, and a cache
/// of the last place found (places are asked for in increasing order).
struct Locs {
    frames: Vec<Frame>,
    done: Option<LocNode>,
    cache: (usize, Place),
    /// Metadata read (`^…`), each for the next form at its depth (`.1`):
    /// a stack, since a collection's waits while its parts take their own
    /// (`^String [^Integer x]`).
    meta: Vec<(Value, usize)>,
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
        Reader { src, pos: 0, fn_args: None, locs: None, depth: 0 }
    }

    /// A reader starting at byte offset `pos`.
    pub fn new_at(src: &'a str, pos: usize) -> Self {
        Reader { src, pos: pos.min(src.len()), fn_args: None, locs: None, depth: 0 }
    }

    /// This reader, recording where each form comes from (`take_locations`).
    pub fn with_locations(mut self) -> Self {
        self.locs = Some(Locs { frames: Vec::new(), done: None, cache: (0, Place { line: 1, col: 1, at: 0 }), meta: Vec::new() });
        self
    }

    /// The location tree of the form just read, if locations are on.
    pub fn take_locations(&mut self) -> Option<LocNode> {
        self.locs.as_mut().and_then(|l| l.done.take())
    }

    /// The place of byte offset `b`, going on from the last one asked for.
    fn place(&mut self, b: usize) -> Place {
        let src = self.src;
        let Some(l) = self.locs.as_mut() else { return Place::default() };
        let (from, mut p) = l.cache;
        if b < from {
            // (not expected: start again from the top)
            l.cache = (0, Place { line: 1, col: 1, at: 0 });
            return self.place(b);
        }
        for c in src[from..b].chars() {
            p.at += 1;
            if c == '\n' {
                p.line += 1;
                p.col = 1;
            } else {
                p.col += 1;
            }
        }
        l.cache = (b, p);
        p
    }

    fn open_frame(&mut self) {
        if self.locs.is_some() {
            let start = self.place(self.pos);
            self.locs.as_mut().unwrap().frames.push(Frame { start, kids: Vec::new(), elems: None });
        }
    }

    /// The form whose frame is on top is finished: it becomes a node of
    /// its parent (or the finished top form).
    fn close_frame(&mut self) {
        if self.locs.is_some() {
            let end = self.place(self.pos);
            let l = self.locs.as_mut().unwrap();
            let depth = l.frames.len() - 1;
            let f = l.frames.pop().unwrap();
            let meta = match l.meta.last() {
                Some(&(m, d)) if d == depth => {
                    l.meta.pop();
                    m
                }
                _ => Value::NIL,
            };
            let node = LocNode { start: f.start, end, kids: f.kids, elems: f.elems, meta };
            match l.frames.last_mut() {
                Some(parent) => parent.kids.push(node),
                None => l.done = Some(node),
            }
        }
    }

    /// The form whose frame is on top was discarded (a comment, `#_`).
    fn drop_frame(&mut self) {
        if let Some(l) = self.locs.as_mut() {
            l.frames.pop();
        }
    }

    fn frame(&mut self) -> Option<&mut Frame> {
        self.locs.as_mut().and_then(|l| l.frames.last_mut())
    }

    /// The form on top was rewritten (syntax-quote, `#(…)`): its parts
    /// don't match the source's, so it keeps no kids, and they all get its
    /// place.
    fn rewritten(&mut self) {
        if let Some(f) = self.frame() {
            f.kids.clear();
        }
    }

    /// Current byte offset.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// 1-based line number of the current position.
    pub fn line(&self) -> usize {
        self.src[..self.pos].matches('\n').count() + 1
    }

    /// 1-based column (in characters) of the current position.
    pub fn column(&self) -> usize {
        let start = self.src[..self.pos].rfind('\n').map_or(0, |i| i + 1);
        self.src[start..self.pos].chars().count() + 1
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
        if let Some(l) = self.locs.as_mut() {
            l.frames.clear();
            l.done = None;
        }
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
        if self.depth >= MAX_DEPTH {
            return Err(ReadError::Syntax(format!("Nested more than {MAX_DEPTH} deep")));
        }
        self.depth += 1;
        let r = self.read_nested(vm);
        self.depth -= 1;
        r
    }

    fn read_nested(&mut self, vm: &mut Vm) -> RResult<Value> {
        loop {
            self.skip_ws();
            let Some(c) = self.peek() else { return Err(ReadError::Eof) };
            if matches!(c, ')' | ']' | '}') {
                return Err(ReadError::Closer(c));
            }
            self.open_frame();
            match self.read_one(vm, c) {
                Ok(Some(v)) => {
                    self.close_frame();
                    return Ok(v);
                }
                // a comment, a discarded form or metadata: keep going
                Ok(None) => self.drop_frame(),
                Err(e) => {
                    self.drop_frame();
                    return Err(e);
                }
            }
        }
    }

    /// One form starting with `c`, or None for something that isn't one.
    fn read_one(&mut self, vm: &mut Vm, c: char) -> RResult<Option<Value>> {
        {
            match c {
                '(' => {
                    self.bump();
                    let xs = self.read_delimited(vm, ')')?;
                    return Ok(Some(list_from(&mut vm.m, &xs)));
                }
                '[' => {
                    self.bump();
                    let xs = self.read_delimited(vm, ']')?;
                    return Ok(Some(vec_from_slice(&mut vm.m, &xs)));
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
                    if let Some(f) = self.frame() {
                        f.elems = Some(xs);
                    }
                    return Ok(Some(map));
                }
                '"' => {
                    self.bump();
                    return self.read_string(vm).map(Some);
                }
                '\'' => {
                    self.bump();
                    return self.wrap(vm, "quote").map(Some);
                }
                '`' => {
                    self.bump();
                    let form = self.read_required(vm)?;
                    let mut gensyms = HashMap::new();
                    self.rewritten();
                    return syntax_quote(vm, form, &mut gensyms).map(Some).map_err(ReadError::Syntax);
                }
                '~' => {
                    self.bump();
                    if self.peek() == Some('@') {
                        self.bump();
                        return self.wrap(vm, "unquote-splicing").map(Some);
                    }
                    return self.wrap(vm, "unquote").map(Some);
                }
                '@' => {
                    self.bump();
                    return self.wrap(vm, "deref").map(Some);
                }
                '^' => {
                    // Metadata is read and dropped (and its place with it);
                    // the form after it is read as the next form.
                    self.bump();
                    let m = self.read_required(vm)?;
                    if let Some(f) = self.frame() {
                        f.kids.pop();
                    }
                    if self.locs.is_some() {
                        // kept for the next form at this depth (the frame
                        // for the ^ itself is dropped)
                        let m = meta_map(vm, m);
                        let l = self.locs.as_mut().unwrap();
                        let depth = l.frames.len() - 1;
                        match l.meta.last_mut() {
                            Some((old, d)) if *d == depth => *old = merge_meta(vm, *old, m),
                            _ => l.meta.push((m, depth)),
                        }
                    }
                    if self.at_end() {
                        return Err(ReadError::Incomplete("EOF after metadata".into()));
                    }
                    return Ok(None);
                }
                '\\' => {
                    self.bump();
                    return self.read_char().map(Some);
                }
                '#' => self.read_dispatch(vm),
                _ => {
                    let tok = self.token();
                    self.parse_token(vm, tok).map(Some)
                }
            }
        }
    }

    fn wrap(&mut self, vm: &mut Vm, head: &str) -> RResult<Value> {
        // The marker (' @ ~ #') stands for the head symbol: its place is the
        // marker's.
        let marker_end = if self.locs.is_some() { Some(self.place(self.pos)) } else { None };
        let form = self.read_required(vm)?;
        if let Some(end) = marker_end
            && let Some(f) = self.frame()
        {
            let start = f.start;
            f.kids.insert(0, LocNode { start, end, kids: Vec::new(), elems: None, meta: Value::NIL });
        }
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
                for &x in &xs {
                    s = set_conj(&mut vm.m, s, x);
                }
                if let Some(f) = self.frame() {
                    f.elems = Some(xs);
                }
                Ok(Some(s))
            }
            '(' => {
                self.bump();
                let f = self.read_fn_literal(vm);
                self.rewritten();
                f.map(Some)
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
                if tok == "queue" {
                    // #queue [1 2 3], as the printer writes a queue (its
                    // items are data, not evaluated, as #bytes's are)
                    let form = self.read_required(vm)?;
                    if !form.is(Kind::Vector) {
                        return Err(ReadError::Syntax("#queue must be followed by a vector".into()));
                    }
                    let e = rt().empty_vector;
                    let mut q = norn_rt::colls::queue_new(&mut vm.m, 0, Value::NIL, e);
                    let mut items = Vec::new();
                    form.vec_for_each(|x| {
                        items.push(x);
                        true
                    });
                    for x in items {
                        q = norn_rt::colls::queue_conj(&mut vm.m, q, x);
                    }
                    return Ok(Some(q));
                }
                if tok == "inst" || tok == "uuid" || (tok.contains('/') && !tok.starts_with('/') && !tok.ends_with('/')) {
                    // #tag form: a tagged literal, its value what the tag's
                    // reader makes of form (norn.core/%read-tagged:
                    // *tag-readers*, Norn's own tags and EDN's)
                    let form = self.read_required(vm)?;
                    static READ_TAGGED: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
                    let f = vm.core_fn("%read-tagged", &READ_TAGGED);
                    let f = if f.is(Kind::Var) { f.var_root() } else { f };
                    if f.is_nil() {
                        return Err(ReadError::Syntax(format!("#{tok}: tagged literals are read once norn.core has loaded")));
                    }
                    let tag = match tok.split_once('/') {
                        Some((ns, name)) => rt().symbol(&mut vm.m, Some(ns), name),
                        None => rt().symbol(&mut vm.m, None, &tok),
                    };
                    // ([true value], or [false message]: an error is the
                    // reader's to raise, not signalled from inside it)
                    let r = vm.apply(f, &[tag, form]).map_err(|e| ReadError::Syntax(e.msg))?;
                    let ok = r.is(Kind::Vector) && r.vec_count() == 2 && r.vec_nth(0) == Value::TRUE;
                    let x = if r.is(Kind::Vector) && r.vec_count() == 2 { r.vec_nth(1) } else { Value::NIL };
                    return if ok { Ok(Some(x)) } else { Err(ReadError::Syntax(if x.is(Kind::String) { x.as_str().to_owned() } else { format!("#{tok}: no value") })) };
                }
                if tok == "lang" {
                    Err(ReadError::Syntax("#lang isn't part of Norn (design.md §8.1): a file's language is Norn".into()))
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
            // ::k is the current namespace's; ::alias/k the aliased one's
            // (as in Clojure), and an alias that isn't one is an error.
            if let Some((alias, kname)) = name.split_once('/')
                && !alias.is_empty()
                && !kname.is_empty()
            {
                let a = rt().symbol(&mut vm.m, None, alias);
                return match vm.ns.alias_target(a) {
                    Some(target) => {
                        let ns = target.sym_name().to_owned();
                        Ok(rt().keyword(&mut vm.m, Some(&ns), kname))
                    }
                    None => Err(ReadError::Syntax(format!(
                        "Invalid token: {tok} ({alias} isn't an alias in {})",
                        vm.ns.name.sym_name()
                    ))),
                };
            }
            if name.is_empty() || name.contains('/') {
                return Err(ReadError::Syntax(format!("Invalid token: {tok}")));
            }
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

/// What source reads as, versioned: bump it when that changes (the build
/// fingerprint takes it in, so compiled modules cached by an older reader
/// aren't loaded). 2: stacked metadata (`^:a ^:b x`) keeps every part.
pub const READER_VERSION: u32 = 2;

/// Metadata as a map: `^:k` is {:k true}, `^Type` or `^"t"` {:tag …}.
fn meta_map(vm: &mut Vm, m: Value) -> Value {
    if m.is(Kind::Map) {
        m
    } else if m.is_keyword() {
        map_assoc(&mut vm.m, map_empty(), m, Value::TRUE)
    } else {
        let tag = rt().keyword(&mut vm.m, None, "tag");
        map_assoc(&mut vm.m, map_empty(), tag, m)
    }
}

/// Metadata a, with b's entries added (`^:a ^:b x` is both). (SeqIter
/// doesn't walk a map, which once lost every ^ after the first.)
fn merge_meta(vm: &mut Vm, a: Value, b: Value) -> Value {
    let mut entries = Vec::new();
    b.map_for_each(|k, v| {
        entries.push((k, v));
        true
    });
    let mut out = a;
    for (k, v) in entries {
        out = map_assoc(&mut vm.m, out, k, v);
    }
    out
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
