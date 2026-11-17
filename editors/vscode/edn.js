// The editor protocol's data (norn --server): one Norn map per line. Its
// responses hold only maps, vectors, strings, keywords, numbers, symbols,
// nil and booleans (values come back printed, as strings), so this reads
// just those. Keywords and symbols read as strings (":k" without the
// colon for keywords, as map keys).

'use strict';

/** Read one form of text: maps become objects (keyword keys without the
 * colon), vectors and lists arrays, keywords and symbols strings. */
function read(text) {
  let i = 0;
  const ws = () => {
    while (i < text.length) {
      const c = text[i];
      if (c === ' ' || c === ',' || c === '\n' || c === '\t' || c === '\r') i++;
      else if (c === ';') while (i < text.length && text[i] !== '\n') i++;
      else break;
    }
  };
  const token = () => {
    const s = i;
    while (i < text.length && !/[\s,()[\]{}"]/.test(text[i])) i++;
    return text.slice(s, i);
  };
  const form = () => {
    ws();
    const c = text[i];
    if (c === '{') {
      i++;
      const m = {};
      for (;;) {
        ws();
        if (text[i] === '}') { i++; return m; }
        const k = form();
        m[typeof k === 'string' ? k : JSON.stringify(k)] = form();
      }
    }
    if (c === '[' || c === '(') {
      const close = c === '[' ? ']' : ')';
      i++;
      const v = [];
      for (;;) {
        ws();
        if (text[i] === close) { i++; return v; }
        if (i >= text.length) throw new Error('unterminated ' + c);
        v.push(form());
      }
    }
    if (c === '"') {
      i++;
      let s = '';
      while (i < text.length && text[i] !== '"') {
        if (text[i] === '\\') {
          i++;
          const e = text[i++];
          s += e === 'n' ? '\n' : e === 't' ? '\t' : e === 'r' ? '\r' : e;
        } else s += text[i++];
      }
      i++;
      return s;
    }
    if (c === ':') { i++; return token(); }
    if (c === '#' && text[i + 1] === '{') { i++; return form(); }
    const t = token();
    if (t === 'nil') return null;
    if (t === 'true') return true;
    if (t === 'false') return false;
    if (/^[-+]?\d+(\.\d*)?([eE][-+]?\d+)?$/.test(t)) return Number(t);
    if (t === '') throw new Error(`can't read ${JSON.stringify(text.slice(i, i + 20))}`);
    return t;
  };
  return form();
}

/** A string as Norn source. */
function str(s) {
  return '"' + s.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n').replace(/\r/g, '\\r').replace(/\t/g, '\\t') + '"';
}

/** A request map as Norn data: keys are keywords; values are numbers,
 * strings, or {kw: "name"} for a keyword. */
function request(m) {
  const val = (v) =>
    typeof v === 'number' ? String(v) : typeof v === 'string' ? str(v) : v && v.kw ? ':' + v.kw : v === null ? 'nil' : String(v);
  return '{' + Object.entries(m).filter(([, v]) => v !== undefined).map(([k, v]) => `:${k} ${val(v)}`).join(' ') + '}';
}

module.exports = { read, str, request };
