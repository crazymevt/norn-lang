//! The comment-keeping reader (M7 part 8): source text as a concrete syntax
//! tree, for `norn fmt` and the editor. Unlike the reader proper it builds
//! no values and keeps everything: whitespace, newlines and comments.
//!
//! Nodes are Norn vectors:
//!
//! ```text
//! [:token text]                         symbols, numbers, strings, chars, regexes, ##Inf…
//! [:space text]                         spaces, tabs and commas (never a newline)
//! [:newline]                            one line break (\r\n counts as one)
//! [:comment text]                       ; to the end of the line, or a #| … |# block
//! [:coll open close [children…]]        ( [ { #{ #( with their closer
//! [:prefix marker [children…]]          ' ` ~ ~@ @ ^ #' #_ and tagged #name: the
//!                                       children are any space, then the form
//! ```
//!
//! `concat` of every node's text reproduces the source exactly
//! (`(%cst-text nodes)`), which the tests check.

use crate::prims::expect;
use crate::vm::{Error, Result, Vm};
use norn_rt::runtime::rt;
use norn_rt::string::str_new;
use norn_rt::vector::vec_from_slice;
use norn_rt::{Kind, Value};

struct Cst<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
}

const DELIMS: &[u8] = b"()[]{}\";";

impl Cst<'_> {
    fn err(&self, msg: &str) -> Error {
        let line = self.s[..self.i.min(self.s.len())].matches('\n').count() + 1;
        Error::new(format!("Syntax error at line {line}: {msg}"))
    }

    fn kw(vm: &mut Vm, k: &str) -> Value {
        rt().keyword(&mut vm.m, None, k)
    }

    fn node(vm: &mut Vm, kind: &str, parts: &[Value]) -> Value {
        let mut v = vec![Self::kw(vm, kind)];
        v.extend_from_slice(parts);
        vec_from_slice(&mut vm.m, &v)
    }

    fn text(vm: &mut Vm, kind: &str, t: &str) -> Value {
        let s = str_new(&mut vm.m, t);
        Self::node(vm, kind, &[s])
    }

    fn peek(&self, k: usize) -> Option<u8> {
        self.b.get(self.i + k).copied()
    }

    /// The end of a token starting at `i` (symbols, numbers, keywords).
    fn token_end(&self, mut i: usize) -> usize {
        while i < self.b.len() {
            let c = self.b[i];
            if c.is_ascii_whitespace() || c == b',' || DELIMS.contains(&c) {
                break;
            }
            i += 1;
        }
        i
    }

    fn string_end(&self, mut i: usize) -> Result<usize> {
        i += 1;
        loop {
            match self.b.get(i) {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => return Ok(i + 1),
                Some(b'\\') => i += 2,
                Some(_) => i += 1,
            }
        }
    }

    /// Nodes until `close` (consumed), or to the end of input when `close`
    /// is None.
    fn seq(&mut self, vm: &mut Vm, close: Option<u8>) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        loop {
            match self.peek(0) {
                None if close.is_none() => return Ok(out),
                None => return Err(self.err(&format!("missing closing {}", close.unwrap() as char))),
                Some(c) if Some(c) == close => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b')' | b']' | b'}') => return Err(self.err(&format!("unmatched {}", self.b[self.i] as char))),
                _ => out.push(self.item(vm)?),
            }
        }
    }

    /// One node: space, a newline, a comment or a form.
    fn item(&mut self, vm: &mut Vm) -> Result<Value> {
        let c = self.b[self.i];
        if c == b'\n' || (c == b'\r' && self.peek(1) == Some(b'\n')) {
            self.i += if c == b'\r' { 2 } else { 1 };
            return Ok(Self::node(vm, "newline", &[]));
        }
        if c == b' ' || c == b'\t' || c == b',' || c == b'\r' {
            let start = self.i;
            while matches!(self.peek(0), Some(b' ' | b'\t' | b',')) || (self.peek(0) == Some(b'\r') && self.peek(1) != Some(b'\n')) {
                self.i += 1;
            }
            return Ok(Self::text(vm, "space", &self.s[start..self.i]));
        }
        if c == b';' || (c == b'#' && self.peek(1) == Some(b'!')) {
            let start = self.i;
            while !matches!(self.peek(0), None | Some(b'\n')) && !(self.peek(0) == Some(b'\r') && self.peek(1) == Some(b'\n')) {
                self.i += 1;
            }
            return Ok(Self::text(vm, "comment", &self.s[start..self.i]));
        }
        if c == b'#' && self.peek(1) == Some(b'|') {
            let start = self.i;
            let mut depth = 0;
            loop {
                if self.i >= self.b.len() {
                    return Err(self.err("unterminated #| comment"));
                }
                if self.b[self.i..].starts_with(b"#|") {
                    depth += 1;
                    self.i += 2;
                } else if self.b[self.i..].starts_with(b"|#") {
                    depth -= 1;
                    self.i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    self.i += 1;
                }
            }
            return Ok(Self::text(vm, "comment", &self.s[start..self.i]));
        }
        self.form(vm)
    }

    fn form(&mut self, vm: &mut Vm) -> Result<Value> {
        let c = self.b[self.i];
        let coll = |me: &mut Self, vm: &mut Vm, open: &str, close: u8| -> Result<Value> {
            me.i += open.len();
            let kids = me.seq(vm, Some(close))?;
            let kids = vec_from_slice(&mut vm.m, &kids);
            let o = str_new(&mut vm.m, open);
            let cl = str_new(&mut vm.m, std::str::from_utf8(&[close]).unwrap());
            Ok(Self::node(vm, "coll", &[o, cl, kids]))
        };
        match c {
            b'(' => return coll(self, vm, "(", b')'),
            b'[' => return coll(self, vm, "[", b']'),
            b'{' => return coll(self, vm, "{", b'}'),
            b'"' => {
                let start = self.i;
                self.i = self.string_end(self.i)?;
                return Ok(Self::text(vm, "token", &self.s[start..self.i]));
            }
            b'\\' => {
                // A character: the next character, then any letters/digits
                // (\newline, é).
                let start = self.i;
                self.i += 1;
                let ch = self.s[self.i..].chars().next().ok_or_else(|| self.err("EOF after \\"))?;
                self.i += ch.len_utf8();
                if ch.is_alphanumeric() {
                    while self.peek(0).is_some_and(|b| b.is_ascii_alphanumeric() || b == b'-') {
                        self.i += 1;
                    }
                }
                return Ok(Self::text(vm, "token", &self.s[start..self.i]));
            }
            b'\'' | b'`' | b'@' | b'^' => return self.prefix(vm, 1),
            b'~' => return self.prefix(vm, if self.peek(1) == Some(b'@') { 2 } else { 1 }),
            b'#' => match self.peek(1) {
                Some(b'{') => return coll(self, vm, "#{", b'}'),
                Some(b'(') => return coll(self, vm, "#(", b')'),
                Some(b'\'') | Some(b'_') => return self.prefix(vm, 2),
                Some(b'"') => {
                    let start = self.i;
                    self.i = self.string_end(self.i + 1)?;
                    return Ok(Self::text(vm, "token", &self.s[start..self.i]));
                }
                Some(b'#') => {}
                _ => {
                    let end = self.token_end(self.i + 1);
                    return self.prefix(vm, end - self.i);
                }
            },
            _ => {}
        }
        let start = self.i;
        self.i = self.token_end(self.i);
        if self.i == start {
            return Err(self.err(&format!("unexpected {}", c as char)));
        }
        Ok(Self::text(vm, "token", &self.s[start..self.i]))
    }

    /// A prefix of `n` bytes, then any space or comments, then one form.
    fn prefix(&mut self, vm: &mut Vm, n: usize) -> Result<Value> {
        let marker = str_new(&mut vm.m, &self.s[self.i..self.i + n]);
        self.i += n;
        let mut kids = Vec::new();
        loop {
            match self.peek(0) {
                None => return Err(self.err("a prefix with nothing after it")),
                Some(b')' | b']' | b'}') => return Err(self.err("a prefix with nothing after it")),
                Some(b' ' | b'\t' | b',' | b'\n' | b'\r' | b';') => kids.push(self.item(vm)?),
                _ => {
                    kids.push(self.form(vm)?);
                    break;
                }
            }
        }
        let kids = vec_from_slice(&mut vm.m, &kids);
        Ok(Self::node(vm, "prefix", &[marker, kids]))
    }
}

/// `(%read-cst text)`: the syntax tree of text, as a vector of top-level nodes.
pub(crate) fn read_cst(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    expect(a[0], Kind::String, "read-cst")?;
    let s = a[0].as_str();
    let mut c = Cst { s, b: s.as_bytes(), i: 0 };
    let nodes = c.seq(vm, None)?;
    Ok(vec_from_slice(&mut vm.m, &nodes))
}
