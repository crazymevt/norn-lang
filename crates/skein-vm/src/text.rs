//! Text and data (M7 part 5): the Rust side of `skein.string`, `skein.regex`,
//! `skein.json` and `skein.format`. The libraries themselves are Skein code
//! (lib/skein/*.skn) over these primitives.
//!
//! Indexes are characters (code points), as `subs` and `nth` count; the
//! conversions to and from byte offsets happen here. Graphemes and display
//! widths use `unicode-segmentation` and `unicode-width`.
//!
//! Regexes are a heap kind holding their pattern string, so they print,
//! compare and save in images like any value. Each carrier keeps its own
//! cache of compiled patterns (no lock on the hot path).

use crate::prims::{expect, int};
use crate::vm::{Error, Result, Vm};
use skein_rt::map::{map_assoc, map_empty};
use skein_rt::numbers::{int_of, int_value, parse_int};
use skein_rt::print::{pr_str, write_value};
use skein_rt::runtime::rt;
use skein_rt::seq;
use skein_rt::string::{float_new, str_new};
use skein_rt::vector::vec_from_slice;
use skein_rt::{Kind, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

// ---- character indexes ----------------------------------------------------------------------

/// The byte offset of character `ci` of string v (the end for `ci ==
/// count`), or None. (Through the runtime's position cache, so stepping
/// through a long non-ASCII string doesn't recount from its start.)
pub(crate) fn byte_of(v: Value, ci: usize) -> Option<usize> {
    v.str_byte_of(ci)
}

/// The character index of byte offset `b` (on a character boundary) of string v.
pub(crate) fn char_of(v: Value, b: usize) -> usize {
    v.str_char_of(b)
}

fn string(v: Value, op: &str) -> Result<&'static str> {
    expect(v, Kind::String, op)?;
    Ok(v.as_str())
}

fn strs(vm: &mut Vm, xs: impl IntoIterator<Item = impl AsRef<str>>) -> Value {
    let vals: Vec<Value> = xs.into_iter().map(|x| str_new(&mut vm.m, x.as_ref())).collect();
    vec_from_slice(&mut vm.m, &vals)
}

// ---- strings --------------------------------------------------------------------------------

/// `(%str-case s mode)`: 0 upper, 1 lower, 2 capitalize (first upper, rest lower).
pub(crate) fn case(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = string(a[0], "case")?;
    let out = match int(a[1], "case")? {
        0 => s.to_uppercase(),
        1 => s.to_lowercase(),
        _ => {
            let mut c = s.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c.as_str().to_lowercase().chars()).collect(),
                None => String::new(),
            }
        }
    };
    Ok(str_new(&mut vm.m, &out))
}

/// `(%str-trim s mode)`: 0 both ends, 1 the start, 2 the end, 3 trailing newlines.
pub(crate) fn trim(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = string(a[0], "trim")?;
    let out = match int(a[1], "trim")? {
        0 => s.trim(),
        1 => s.trim_start(),
        2 => s.trim_end(),
        _ => s.trim_end_matches(['\n', '\r']),
    };
    Ok(if out.len() == s.len() { a[0] } else { str_new(&mut vm.m, out) })
}

fn needle(v: Value, op: &str) -> Result<String> {
    if v.is_char() { Ok(v.as_char().to_string()) } else { Ok(string(v, op)?.to_string()) }
}

/// `(%str-index s needle from last?)`: the character index of the first
/// (or last) occurrence at or after (before) `from`, or nil.
pub(crate) fn index(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = string(a[0], "index-of")?;
    let n = needle(a[1], "index-of")?;
    let last = a[3].truthy();
    let from = if a[2].is_nil() { None } else { Some(int(a[2], "index-of")?.max(0) as usize) };
    let found = if last {
        let end = match from {
            Some(f) => byte_of(a[0], f).map(|b| (b + n.len()).min(s.len())).unwrap_or(s.len()),
            None => s.len(),
        };
        s[..end].rfind(&n)
    } else {
        let start = match from {
            Some(f) => match byte_of(a[0], f) {
                Some(b) => b,
                None => return Ok(Value::NIL),
            },
            None => 0,
        };
        s[start..].find(&n).map(|i| i + start)
    };
    Ok(found.map(|b| Value::fix(char_of(a[0], b) as i64)).unwrap_or(Value::NIL))
}

/// `(%str-split s sep limit)`: split on a literal string; `limit` > 0 caps
/// the number of parts. An empty separator splits into characters.
pub(crate) fn split(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = string(a[0], "split")?;
    let sep = needle(a[1], "split")?;
    let limit = if a[2].is_nil() { 0 } else { int(a[2], "split")?.max(0) as usize };
    let parts: Vec<&str> = if sep.is_empty() {
        let mut v: Vec<&str> = s.char_indices().map(|(i, c)| &s[i..i + c.len_utf8()]).collect();
        if limit > 0 && v.len() > limit {
            let b = byte_of(a[0], limit - 1).unwrap();
            v.truncate(limit - 1);
            v.push(&s[b..]);
        }
        v
    } else if limit > 0 {
        s.splitn(limit, sep.as_str()).collect()
    } else {
        s.split(sep.as_str()).collect()
    };
    Ok(strs(vm, parts))
}

/// `(%str-replace s from to all?)`: literal replacement.
pub(crate) fn replace(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = string(a[0], "replace")?;
    let from = needle(a[1], "replace")?;
    let to = needle(a[2], "replace")?;
    if from.is_empty() {
        return Err(Error::new("replace: the text to replace is empty"));
    }
    let out = if a[3].truthy() { s.replace(&from, &to) } else { s.replacen(&from, &to, 1) };
    Ok(str_new(&mut vm.m, &out))
}

/// `(%graphemes s)`: the user-perceived characters, as strings.
pub(crate) fn graphemes(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = string(a[0], "graphemes")?;
    Ok(strs(vm, s.graphemes(true)))
}

/// `(%grapheme-count s)`.
pub(crate) fn grapheme_count(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = string(a[0], "grapheme-count")?;
    Ok(Value::fix(if s.is_ascii() { s.len() } else { s.graphemes(true).count() } as i64))
}

/// `(%str-width s)`: columns in a terminal or monospace editor (East Asian
/// wide characters take two; combining marks take none).
pub(crate) fn width(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = if a[0].is_char() { a[0].as_char().to_string() } else { string(a[0], "width")?.to_string() };
    Ok(Value::fix(s.width() as i64))
}

/// `(%str-reverse s)`: reversed by grapheme, so accents stay on their letters.
pub(crate) fn reverse(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = string(a[0], "reverse")?;
    let out: String = s.graphemes(true).rev().collect();
    Ok(str_new(&mut vm.m, &out))
}

/// `(%str-truncate s cols)`: the longest prefix (whole graphemes) at most
/// `cols` columns wide.
pub(crate) fn truncate(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = string(a[0], "truncate")?;
    let cols = int(a[1], "truncate")?.max(0) as usize;
    let mut w = 0;
    let mut end = 0;
    for (i, g) in s.grapheme_indices(true) {
        let gw = g.width();
        if w + gw > cols {
            break;
        }
        w += gw;
        end = i + g.len();
    }
    Ok(if end == s.len() { a[0] } else { str_new(&mut vm.m, &s[..end]) })
}

/// `(%parse-number s integer?)`: an integer (base 10, optional sign, any
/// size) or a float, or nil when s isn't exactly one.
pub(crate) fn parse_number(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = string(a[0], "parse")?;
    if a[1].truthy() {
        let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Ok(Value::NIL);
        }
        let n = parse_int(s.strip_prefix('+').unwrap_or(s), 10).unwrap();
        Ok(int_value(&mut vm.m, &n))
    } else {
        let ok = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b) || b"InfinityNaN".contains(&b));
        match s.parse::<f64>() {
            Ok(f) if ok => Ok(float_new(&mut vm.m, f)),
            _ => Ok(Value::NIL),
        }
    }
}

// ---- regexes --------------------------------------------------------------------------------

// (shared, not cloned: a cloned regex::Regex starts a pool of search caches
// of its own, so every match paid for building one, ~2 µs)
thread_local! {
    static REGEXES: RefCell<HashMap<String, std::rc::Rc<regex::Regex>>> = RefCell::new(HashMap::new());
}

fn compile(pat: &str) -> Result<std::rc::Rc<regex::Regex>> {
    REGEXES.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(r) = c.get(pat) {
            return Ok(std::rc::Rc::clone(r));
        }
        let r = std::rc::Rc::new(regex::Regex::new(pat).map_err(|e| Error::new(format!("Invalid regex: {e}")))?);
        if c.len() >= 512 {
            c.clear();
        }
        c.insert(pat.to_string(), std::rc::Rc::clone(&r));
        Ok(r)
    })
}

/// A regex value or a pattern string, compiled.
fn regex_arg(v: Value, op: &str) -> Result<std::rc::Rc<regex::Regex>> {
    if v.is(Kind::Regex) {
        compile(v.field(0).as_str())
    } else if v.is(Kind::String) {
        compile(v.as_str())
    } else {
        Err(Error::of("TypeError", format!("{op} expects a regex, got {}: {}", v.type_name(), pr_str(v, true))))
    }
}

/// A Regex value for pattern `pat` (checked).
pub(crate) fn regex_value(m: &mut skein_rt::heap::Mutator, pat: Value) -> Result<Value> {
    compile(pat.as_str())?;
    let r = skein_rt::object::alloc(m, Kind::Regex, 1);
    r.set_field(0, pat);
    Ok(r)
}

/// `(re-pattern s)`.
pub(crate) fn re_pattern(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    if a[0].is(Kind::Regex) {
        return Ok(a[0]);
    }
    expect(a[0], Kind::String, "re-pattern")?;
    regex_value(&mut vm.m, a[0])
}

/// One match as `[start end groups names]`: character indexes, the groups'
/// text (nil when a group didn't take part), and their names (nil when
/// unnamed).
fn match_value(vm: &mut Vm, re: &regex::Regex, sv: Value, c: &regex::Captures, names: Value) -> Value {
    let m0 = c.get(0).unwrap();
    let groups: Vec<Value> = (0..re.captures_len())
        .map(|i| match c.get(i) {
            Some(g) => str_new(&mut vm.m, g.as_str()),
            None => Value::NIL,
        })
        .collect();
    let groups = vec_from_slice(&mut vm.m, &groups);
    let start = Value::fix(char_of(sv, m0.start()) as i64);
    let end = Value::fix(char_of(sv, m0.end()) as i64);
    vec_from_slice(&mut vm.m, &[start, end, groups, names])
}

/// The groups' names, as keywords (nil for an unnamed group): skein.regex's
/// :named map's keys.
fn group_names(vm: &mut Vm, re: &regex::Regex) -> Value {
    let names: Vec<Value> = re.capture_names().map(|n| n.map(|n| rt().keyword(&mut vm.m, None, n)).unwrap_or(Value::NIL)).collect();
    vec_from_slice(&mut vm.m, &names)
}

/// `(%re-search re s from anchored?)`: the first match at or after character
/// `from`, or nil. Anchored means it must cover the whole string.
pub(crate) fn re_search(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let re = regex_arg(a[0], "re-find")?;
    let s = string(a[1], "re-find")?;
    let from = int(a[2], "re-find")?.max(0) as usize;
    let Some(start) = byte_of(a[1], from) else { return Ok(Value::NIL) };
    let Some(c) = re.captures_at(s, start) else { return Ok(Value::NIL) };
    let m0 = c.get(0).unwrap();
    if a[3].truthy() && (m0.start() != 0 || m0.end() != s.len()) {
        // The leftmost-first match isn't the whole string; try anchoring.
        let anchored = compile(&format!("^(?:{})$", re.as_str()))?;
        let Some(c) = anchored.captures(s) else { return Ok(Value::NIL) };
        let names = group_names(vm, &re);
        let inner = shift_groups(vm, &c, a[1], names);
        return Ok(inner);
    }
    let names = group_names(vm, &re);
    Ok(match_value(vm, &re, a[1], &c, names))
}

/// The anchored pattern wraps the original in a non-capturing group, so its
/// groups line up with the original's.
fn shift_groups(vm: &mut Vm, c: &regex::Captures, sv: Value, names: Value) -> Value {
    let groups: Vec<Value> = (0..c.len())
        .map(|i| match c.get(i) {
            Some(g) => str_new(&mut vm.m, g.as_str()),
            None => Value::NIL,
        })
        .collect();
    let groups = vec_from_slice(&mut vm.m, &groups);
    vec_from_slice(&mut vm.m, &[Value::fix(0), Value::fix(sv.str_char_count() as i64), groups, names])
}

/// `(%re-all re s)`: every non-overlapping match, in order.
pub(crate) fn re_all(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let re = regex_arg(a[0], "re-seq")?;
    let s = string(a[1], "re-seq")?;
    let names = group_names(vm, &re);
    let all: Vec<Value> = re.captures_iter(s).map(|c| match_value(vm, &re, a[1], &c, names)).collect();
    Ok(vec_from_slice(&mut vm.m, &all))
}

/// `(%re-replace re s replacement all?)`: `$1` and `${name}` in the
/// replacement refer to groups (`$$` is a dollar sign).
pub(crate) fn re_replace(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let re = regex_arg(a[0], "replace")?;
    let s = string(a[1], "replace")?;
    let rep = needle(a[2], "replace")?;
    let out = if a[3].truthy() { re.replace_all(s, rep.as_str()) } else { re.replace(s, rep.as_str()) };
    Ok(match out {
        std::borrow::Cow::Borrowed(_) => a[1],
        std::borrow::Cow::Owned(o) => str_new(&mut vm.m, &o),
    })
}

/// `(%re-split re s limit)`.
pub(crate) fn re_split(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let re = regex_arg(a[0], "split")?;
    let s = string(a[1], "split")?;
    let limit = if a[2].is_nil() { 0 } else { int(a[2], "split")?.max(0) as usize };
    let parts: Vec<&str> = if limit > 0 { re.splitn(s, limit).collect() } else { re.split(s).collect() };
    Ok(strs(vm, parts))
}

/// `(re-quote s)`: a pattern matching `s` literally.
pub(crate) fn re_quote(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let s = string(a[0], "re-quote")?;
    let q = str_new(&mut vm.m, &regex::escape(s));
    regex_value(&mut vm.m, q)
}

// ---- JSON -----------------------------------------------------------------------------------

struct JsonReader<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
    keywords: bool,
}

impl JsonReader<'_> {
    fn err(&self, what: &str) -> Error {
        let before = &self.src[..self.i.min(self.src.len())];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0) + 1;
        Error::of("ParseError", format!("JSON: {what} at line {line}, column {col}"))
            .with("source", crate::vm::ErrData::Str("JSON".into()))
            .with("line", crate::vm::ErrData::Int(line as i64))
            .with("column", crate::vm::ErrData::Int(col as i64))
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn lit(&mut self, word: &str, v: Value) -> Result<Value> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(self.err("unexpected text"))
        }
    }

    fn value(&mut self, vm: &mut Vm, depth: usize) -> Result<Value> {
        if depth > 512 {
            return Err(self.err("nesting too deep"));
        }
        self.ws();
        match self.peek() {
            None => Err(self.err("unexpected end of input")),
            Some(b'{') => {
                self.i += 1;
                let mut map = map_empty();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(map);
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"') {
                        return Err(self.err("expected a string key"));
                    }
                    let k = self.string()?;
                    let key = if self.keywords { rt().keyword(&mut vm.m, None, &k) } else { str_new(&mut vm.m, &k) };
                    self.ws();
                    if self.peek() != Some(b':') {
                        return Err(self.err("expected ':'"));
                    }
                    self.i += 1;
                    let v = self.value(vm, depth + 1)?;
                    map = map_assoc(&mut vm.m, map, key, v);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(map);
                        }
                        _ => return Err(self.err("expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(vec_from_slice(&mut vm.m, &items));
                }
                loop {
                    items.push(self.value(vm, depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(vec_from_slice(&mut vm.m, &items));
                        }
                        _ => return Err(self.err("expected ',' or ']'")),
                    }
                }
            }
            Some(b'"') => {
                let s = self.string()?;
                Ok(str_new(&mut vm.m, &s))
            }
            Some(b't') => self.lit("true", Value::bool(true)),
            Some(b'f') => self.lit("false", Value::bool(false)),
            Some(b'n') => self.lit("null", Value::NIL),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(vm),
            Some(_) => Err(self.err("unexpected character")),
        }
    }

    fn number(&mut self, vm: &mut Vm) -> Result<Value> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        let digits = |r: &mut Self| {
            let d = r.i;
            while r.peek().is_some_and(|c| c.is_ascii_digit()) {
                r.i += 1;
            }
            r.i > d
        };
        if !digits(self) {
            return Err(self.err("expected a digit"));
        }
        let int_part = &self.src[start..self.i];
        if int_part.trim_start_matches('-').len() > 1 && int_part.trim_start_matches('-').starts_with('0') {
            return Err(self.err("leading zero in a number"));
        }
        let mut float = false;
        if self.peek() == Some(b'.') {
            self.i += 1;
            float = true;
            if !digits(self) {
                return Err(self.err("expected a digit after '.'"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            float = true;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return Err(self.err("expected an exponent"));
            }
        }
        let text = &self.src[start..self.i];
        if float {
            let f: f64 = text.parse().map_err(|_| self.err("bad number"))?;
            Ok(float_new(&mut vm.m, f))
        } else {
            match text.parse::<i64>().ok().and_then(Value::fixnum) {
                Some(v) => Ok(v),
                None => Ok(int_value(&mut vm.m, &parse_int(text, 10).ok_or_else(|| self.err("bad number"))?)),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32> {
        let h = self.src.get(self.i..self.i + 4).ok_or_else(|| self.err("short \\u escape"))?;
        let n = u32::from_str_radix(h, 16).map_err(|_| self.err("bad \\u escape"))?;
        self.i += 4;
        Ok(n)
    }

    fn string(&mut self) -> Result<String> {
        self.i += 1; // the opening quote
        let mut out = String::new();
        loop {
            let run = self.i;
            while self.i < self.s.len() && !matches!(self.s[self.i], b'"' | b'\\') && self.s[self.i] >= 0x20 {
                self.i += 1;
            }
            out.push_str(&self.src[run..self.i]);
            match self.peek() {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let e = self.peek().ok_or_else(|| self.err("unterminated string"))?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut c = self.hex4()?;
                            if (0xD800..0xDC00).contains(&c) {
                                if !self.s[self.i..].starts_with(b"\\u") {
                                    return Err(self.err("unpaired surrogate"));
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("unpaired surrogate"));
                                }
                                c = 0x10000 + ((c - 0xD800) << 10) + (lo - 0xDC00);
                            }
                            out.push(char::from_u32(c).ok_or_else(|| self.err("unpaired surrogate"))?);
                        }
                        _ => return Err(self.err("bad escape")),
                    }
                }
                Some(_) => return Err(self.err("control character in a string")),
            }
        }
    }
}

/// `(%json-read s keywords?)`: one JSON value (objects become maps, arrays
/// vectors).
pub(crate) fn json_read(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let src = string(a[0], "json/read")?;
    let mut r = JsonReader { s: src.as_bytes(), src, i: 0, keywords: a[1].truthy() };
    let v = r.value(vm, 0)?;
    r.ws();
    if r.i < r.s.len() {
        return Err(r.err("extra text after the value"));
    }
    Ok(v)
}

struct JsonWriter {
    out: String,
    pretty: bool,
    ascii: bool,
}

impl JsonWriter {
    fn newline(&mut self, depth: usize) {
        if self.pretty {
            self.out.push('\n');
            for _ in 0..depth {
                self.out.push_str("  ");
            }
        }
    }

    fn string(&mut self, s: &str) {
        self.out.push('"');
        for c in s.chars() {
            match c {
                '"' => self.out.push_str("\\\""),
                '\\' => self.out.push_str("\\\\"),
                '\n' => self.out.push_str("\\n"),
                '\r' => self.out.push_str("\\r"),
                '\t' => self.out.push_str("\\t"),
                c if (c as u32) < 0x20 || (self.ascii && !c.is_ascii()) => {
                    let mut buf = [0u16; 2];
                    for u in c.encode_utf16(&mut buf) {
                        let _ = write!(self.out, "\\u{u:04x}");
                    }
                }
                c => self.out.push(c),
            }
        }
        self.out.push('"');
    }

    fn key(&mut self, k: Value) -> Result<()> {
        if k.is(Kind::String) {
            self.string(k.as_str());
        } else if k.is_keyword() || k.is_symbol() {
            self.string(&name_of(k));
        } else if crate::numeric::num(k).is_ok() {
            let t = pr_str(k, false);
            self.string(&t);
        } else {
            return Err(Error::new(format!("JSON: can't use {} as an object key", pr_str(k, true))));
        }
        Ok(())
    }

    fn entries(&mut self, vm: &mut Vm, pairs: Vec<(Value, Value)>, depth: usize) -> Result<()> {
        self.out.push('{');
        for (i, (k, v)) in pairs.iter().enumerate() {
            if i > 0 {
                self.out.push(',');
            }
            self.newline(depth + 1);
            self.key(*k)?;
            self.out.push_str(if self.pretty { ": " } else { ":" });
            self.value(vm, *v, depth + 1)?;
        }
        if !pairs.is_empty() {
            self.newline(depth);
        }
        self.out.push('}');
        Ok(())
    }

    fn value(&mut self, vm: &mut Vm, v: Value, depth: usize) -> Result<()> {
        if depth > 512 {
            return Err(Error::new("JSON: nesting too deep (a cycle?)"));
        }
        if v.is_nil() {
            self.out.push_str("null");
        } else if v.is_bool() {
            self.out.push_str(if v.truthy() { "true" } else { "false" });
        } else if v.is_fixnum() || v.is(Kind::BigInt) {
            write_value(&mut self.out, v, false);
        } else if v.is_float() || v.is(Kind::Ratio) {
            let f = crate::numeric::num(v)?.as_f64();
            if !f.is_finite() {
                return Err(Error::new(format!("JSON: can't write {}", pr_str(v, true))));
            }
            let _ = write!(self.out, "{f:?}");
        } else if v.is(Kind::String) {
            self.string(v.as_str());
        } else if v.is_char() {
            self.string(&v.as_char().to_string());
        } else if v.is_keyword() || v.is_symbol() {
            self.string(&name_of(v));
        } else if v.is(Kind::Map) {
            let mut pairs = Vec::with_capacity(v.map_count());
            v.map_for_each(|k, x| {
                pairs.push((k, x));
                true
            });
            self.entries(vm, pairs, depth)?;
        } else if v.is(Kind::Sorted) && !v.sorted_is_set() {
            let pairs = skein_rt::colls::sorted_entries(v);
            self.entries(vm, pairs, depth)?;
        } else if seq::is_seqable(v) && !v.is(Kind::Bytes) {
            if let Some(s) = seq::first_unrealized(v) {
                return Err(Error::force(s, true));
            }
            let items = seq::to_vec(&mut vm.m, v).map_err(Error::new)?;
            self.out.push('[');
            for (i, &x) in items.iter().enumerate() {
                if i > 0 {
                    self.out.push(',');
                }
                self.newline(depth + 1);
                self.value(vm, x, depth + 1)?;
            }
            if !items.is_empty() {
                self.newline(depth);
            }
            self.out.push(']');
        } else {
            return Err(Error::new(format!("JSON: can't write a {}: {}", v.type_name(), pr_str(v, true))));
        }
        Ok(())
    }
}

/// A keyword's or symbol's text, with its namespace if it has one.
fn name_of(v: Value) -> String {
    match v.sym_ns() {
        Some(ns) => format!("{ns}/{}", v.sym_name()),
        None => v.sym_name().to_string(),
    }
}

/// `(%json-write v pretty? ascii?)`.
pub(crate) fn json_write(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let mut w = JsonWriter { out: String::new(), pretty: a[1].truthy(), ascii: a[2].truthy() };
    w.value(vm, a[0], 0)?;
    Ok(str_new(&mut vm.m, &w.out))
}

// ---- format ---------------------------------------------------------------------------------

/// `(%format fmt args)`: printf-style directives.
///
/// `%[flags][width][.precision]conversion`, where flags are `-` (left
/// align), `0` (zero pad), `+` (always sign), ` ` (space for plus) and `,`
/// (group thousands). Conversions: `s` (display text), `S` (readable,
/// as `pr-str`), `d` (integer, any size), `f` `e` `g` (floating point; any
/// number), `x` `X` `o` `b` (integer in hex, octal, binary), `c`
/// (character), `%` and `n` (newline). Width and precision count display
/// columns for `s` and `S`.
pub(crate) fn format(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let fmt = string(a[0], "format")?;
    expect(a[1], Kind::Vector, "format")?;
    let args: Vec<Value> = (0..a[1].vec_count()).map(|i| a[1].vec_nth(i)).collect();
    forced_args(&args)?;
    let mut out = String::new();
    let mut next = 0;
    let mut it = fmt.chars().peekable();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut spec = Spec::default();
        while let Some(&f) = it.peek() {
            match f {
                '-' => spec.left = true,
                '0' => spec.zero = true,
                '+' => spec.plus = true,
                ' ' => spec.space = true,
                ',' => spec.group = true,
                _ => break,
            }
            it.next();
        }
        let num = |it: &mut std::iter::Peekable<std::str::Chars>| {
            let mut n: Option<usize> = None;
            while let Some(d) = it.peek().and_then(|c| c.to_digit(10)) {
                n = Some(n.unwrap_or(0) * 10 + d as usize);
                it.next();
            }
            n
        };
        spec.width = num(&mut it).unwrap_or(0);
        if it.peek() == Some(&'.') {
            it.next();
            spec.prec = Some(num(&mut it).unwrap_or(0));
        }
        let conv = it.next().ok_or_else(|| Error::new("format: the format ends inside a directive"))?;
        if conv == '%' {
            out.push('%');
            continue;
        }
        if conv == 'n' {
            out.push('\n');
            continue;
        }
        let arg = *args.get(next).ok_or_else(|| Error::new(format!("format: too few arguments for {fmt:?}")))?;
        next += 1;
        let body = convert(conv, arg, &spec)?;
        pad(&mut out, &body, &spec, matches!(conv, 'd' | 'f' | 'e' | 'g' | 'x' | 'X' | 'o' | 'b'));
    }
    Ok(str_new(&mut vm.m, &out))
}

fn forced_args(args: &[Value]) -> Result<()> {
    for &x in args {
        if let Some(s) = seq::first_unrealized(x) {
            return Err(Error::force(s, true));
        }
    }
    Ok(())
}

#[derive(Default)]
struct Spec {
    left: bool,
    zero: bool,
    plus: bool,
    space: bool,
    group: bool,
    width: usize,
    prec: Option<usize>,
}

fn convert(conv: char, v: Value, spec: &Spec) -> Result<String> {
    let bad = |what: &str| Error::new(format!("format: %{conv} expects {what}, got {}", pr_str(v, true)));
    let sign = |neg: bool| {
        if neg {
            "-"
        } else if spec.plus {
            "+"
        } else if spec.space {
            " "
        } else {
            ""
        }
    };
    Ok(match conv {
        's' | 'S' => {
            let t = pr_str(v, conv == 'S');
            match spec.prec {
                Some(p) => {
                    let mut w = 0;
                    t.graphemes(true)
                        .take_while(|g| {
                            w += g.width();
                            w <= p
                        })
                        .collect()
                }
                None => t,
            }
        }
        'c' => {
            if v.is_char() {
                v.as_char().to_string()
            } else {
                return Err(bad("a character"));
            }
        }
        'd' => {
            let n = int_of(v).ok_or_else(|| bad("an integer"))?;
            let digits = n.magnitude().to_string();
            let digits = if spec.group { group3(&digits) } else { digits };
            format!("{}{digits}", sign(n.sign() == num_bigint::Sign::Minus))
        }
        'x' | 'X' | 'o' | 'b' => {
            let n = int_of(v).ok_or_else(|| bad("an integer"))?;
            let radix = match conv {
                'o' => 8,
                'b' => 2,
                _ => 16,
            };
            let mut digits = n.magnitude().to_str_radix(radix);
            if conv == 'X' {
                digits = digits.to_uppercase();
            }
            format!("{}{digits}", sign(n.sign() == num_bigint::Sign::Minus))
        }
        'f' | 'e' | 'g' => {
            let f = crate::numeric::num(v).map_err(|_| bad("a number"))?.as_f64();
            let p = spec.prec.unwrap_or(6);
            if !f.is_finite() {
                return Ok(pr_str(v, false));
            }
            let mag = f.abs();
            let body = match conv {
                'f' => {
                    let t = format!("{mag:.p$}");
                    if spec.group {
                        let (i, frac) =
                            t.split_once('.').map(|(i, f)| (i.to_string(), format!(".{f}"))).unwrap_or((t.clone(), String::new()));
                        format!("{}{frac}", group3(&i))
                    } else {
                        t
                    }
                }
                'e' => exp_form(mag, p),
                _ => {
                    // %g: the shorter of %f and %e, without trailing zeros.
                    let p = p.max(1);
                    let x = if mag == 0.0 { 0 } else { mag.log10().floor() as i32 };
                    let t = if x < -4 || x >= p as i32 {
                        exp_form(mag, p - 1)
                    } else {
                        format!("{mag:.*}", (p as i32 - 1 - x).max(0) as usize)
                    };
                    trim_zeros(&t)
                }
            };
            format!("{}{body}", sign(f.is_sign_negative() && f != 0.0))
        }
        _ => return Err(Error::new(format!("format: unknown directive %{conv}"))),
    })
}

/// `1.234560e+03`, as C's printf writes it.
fn exp_form(f: f64, p: usize) -> String {
    let t = format!("{f:.p$e}");
    let (m, e) = t.split_once('e').unwrap();
    let e: i32 = e.parse().unwrap();
    format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
}

fn trim_zeros(t: &str) -> String {
    let (m, e) = match t.find('e') {
        Some(i) => (&t[..i], &t[i..]),
        None => (t, ""),
    };
    let m = if m.contains('.') { m.trim_end_matches('0').trim_end_matches('.') } else { m };
    format!("{m}{e}")
}

fn group3(digits: &str) -> String {
    let n = digits.len();
    let mut out = String::with_capacity(n + n / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn pad(out: &mut String, body: &str, spec: &Spec, numeric: bool) {
    let w = body.width();
    if w >= spec.width {
        out.push_str(body);
        return;
    }
    let fill = spec.width - w;
    if spec.left {
        out.push_str(body);
        out.extend(std::iter::repeat_n(' ', fill));
    } else if spec.zero && numeric {
        // Zeros go after the sign.
        let (sign, rest) = match body.chars().next() {
            Some(c @ ('-' | '+' | ' ')) => (c.to_string(), &body[1..]),
            _ => (String::new(), body),
        };
        out.push_str(&sign);
        out.extend(std::iter::repeat_n('0', fill));
        out.push_str(rest);
    } else {
        out.extend(std::iter::repeat_n(' ', fill));
        out.push_str(body);
    }
}
