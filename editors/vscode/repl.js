// Part 5: a REPL, "jack-in". Skein: Start REPL runs `skein --server PORT` in
// a terminal in the project (the terminal REPL itself, with its own
// debugger prompt) and connects to its editor protocol (skein --server, one
// Skein map per line). Evaluations from files then go to it: their values
// show beside the form, what they print goes to the "Skein Evaluation"
// output, and an error stops in VS Code's debugger (debug.js).

'use strict';

const net = require('net');
const path = require('path');
const vscode = require('vscode');
const edn = require('./edn');

/** A free port on the loopback interface (the REPL binds it a moment later). */
function freePort() {
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.unref();
    s.on('error', reject);
    s.listen(0, '127.0.0.1', () => {
      const { port } = s.address();
      s.close(() => resolve(port));
    });
  });
}

/** A socket connected to port, trying until ms have passed (the REPL starts). */
async function connect(port, ms) {
  const end = Date.now() + ms;
  for (;;) {
    try {
      return await new Promise((resolve, reject) => {
        const s = net.connect(port, '127.0.0.1', () => resolve(s));
        s.once('error', reject);
      });
    } catch (e) {
      if (Date.now() > end) throw new Error(`no Skein REPL answered on port ${port}: ${e.message}`);
      await new Promise((r) => setTimeout(r, 150));
    }
  }
}

/** One project's REPL: its terminal and its editor-protocol connection. */
class Repl {
  constructor(root, terminal, socket) {
    this.root = root;
    this.terminal = terminal;
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map(); // request id → resolve
    this.listeners = new Set(); // (event map) → void
    /** Fibers waiting in the debugger: fiber id → its :debug event. */
    this.stopped = new Map();
    this.closed = false;
    let buf = '';
    socket.setEncoding('utf8');
    socket.on('data', (chunk) => {
      buf += chunk;
      let nl;
      while ((nl = buf.indexOf('\n')) >= 0) {
        const line = buf.slice(0, nl).trim();
        buf = buf.slice(nl + 1);
        if (line) this.receive(line);
      }
    });
    socket.on('close', () => this.close());
    socket.on('error', () => this.close());
  }

  receive(line) {
    let m;
    try {
      m = edn.read(line);
    } catch (e) {
      return;
    }
    if (m.event) {
      if (m.event === 'debug') this.stopped.set(m.fiber, m);
      if (m.event === 'resumed') this.stopped.delete(m.fiber);
      this.listeners.forEach((f) => f(m));
      return;
    }
    const resolve = this.pending.get(m.id);
    if (resolve) {
      this.pending.delete(m.id);
      resolve(m);
    }
  }

  /** Send request m ({op: "eval", …}: op and keyword values as {kw}); its response. */
  request(m) {
    if (this.closed) return Promise.reject(new Error('the Skein REPL has ended'));
    const id = this.nextId++;
    const line = edn.request({ op: { kw: m.op }, id, ...Object.fromEntries(Object.entries(m).filter(([k]) => k !== 'op')) });
    return new Promise((resolve) => {
      this.pending.set(id, resolve);
      this.socket.write(line + '\n');
    });
  }

  onEvent(f) {
    this.listeners.add(f);
    return { dispose: () => this.listeners.delete(f) };
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.socket.destroy();
    for (const resolve of this.pending.values()) resolve({ error: 'the Skein REPL has ended' });
    this.pending.clear();
    this.listeners.forEach((f) => f({ event: 'closed' }));
  }
}

/** Start a REPL for the project at root: `skein --server PORT` in a terminal. */
async function startRepl(root, skeinPath) {
  const port = await freePort();
  const terminal = vscode.window.createTerminal({
    name: `Skein REPL (${path.basename(root)})`,
    cwd: root,
    shellPath: skeinPath,
    shellArgs: ['--server', String(port)],
  });
  terminal.show(true);
  const socket = await connect(port, 30000);
  return new Repl(root, terminal, socket);
}

// ---- the forms of a file ----

/** Each top-level bracketed form of text: {start, end} offsets (with a
 * reader prefix such as ' or #). Strings, comments and characters skipped. */
function topLevelForms(text) {
  const forms = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < text.length; ) {
    const c = text[i];
    if (c === ';') {
      while (i < text.length && text[i] !== '\n') i++;
    } else if (c === '"') {
      for (i++; i < text.length && text[i] !== '"'; i++) if (text[i] === '\\') i++;
      i++;
    } else if (c === '\\') {
      i += 2;
      while (i < text.length && /[a-z0-9]/i.test(text[i])) i++;
    } else if (c === '(' || c === '[' || c === '{') {
      if (depth === 0) {
        start = i;
        while (start > 0 && "'`#@~^".includes(text[start - 1])) start--;
      }
      depth++;
      i++;
    } else if (c === ')' || c === ']' || c === '}') {
      i++;
      if (depth > 0 && --depth === 0) forms.push({ start, end: i });
    } else i++;
  }
  return forms;
}

/** The top-level form around offset, or ending on its line before it. */
function formAt(text, offset) {
  const forms = topLevelForms(text);
  const inside = forms.find((f) => f.start <= offset && offset <= f.end);
  if (inside) return inside;
  const lineStart = text.lastIndexOf('\n', offset - 1) + 1;
  return forms.filter((f) => f.end <= offset && f.end > lineStart).pop();
}

/** The namespace text's ns form names ("user" without one). */
function namespaceOf(text) {
  const m = /^\s*\(ns\s+([^\s()[\]{}]+)/m.exec(text);
  return m ? m[1] : 'user';
}

// ---- values beside forms ----

const resultStyle = vscode.window.createTextEditorDecorationType({
  after: { margin: '0 0 0 1em', color: new vscode.ThemeColor('editorCodeLens.foreground'), fontStyle: 'italic' },
  rangeBehavior: vscode.DecorationRangeBehavior.ClosedClosed,
});
const errorStyle = vscode.window.createTextEditorDecorationType({
  after: { margin: '0 0 0 1em', color: new vscode.ThemeColor('errorForeground'), fontStyle: 'italic' },
  rangeBehavior: vscode.DecorationRangeBehavior.ClosedClosed,
});
/** Document URI → its results shown: [{pos, text, error}]. */
const shown = new Map();

function showResult(editor, pos, text, error) {
  const key = editor.document.uri.toString();
  const rs = (shown.get(key) || []).filter((r) => !r.pos.isEqual(pos));
  rs.push({ pos, text, error });
  shown.set(key, rs);
  paint(editor);
}

function paint(editor) {
  const rs = shown.get(editor.document.uri.toString()) || [];
  const deco = (r) => {
    const one = r.text.replace(/\s+/g, ' ');
    return {
      range: new vscode.Range(r.pos, r.pos),
      renderOptions: { after: { contentText: '⇒ ' + (one.length > 120 ? one.slice(0, 117) + '…' : one) } },
      hoverMessage: new vscode.MarkdownString().appendCodeblock(r.text, 'skein'),
    };
  };
  editor.setDecorations(resultStyle, rs.filter((r) => !r.error).map(deco));
  editor.setDecorations(errorStyle, rs.filter((r) => r.error).map(deco));
}

/** Results go when their document changes; they come back with its editor. */
function watchResults(context) {
  context.subscriptions.push(
    vscode.workspace.onDidChangeTextDocument((e) => {
      const key = e.document.uri.toString();
      if (e.contentChanges.length && shown.has(key)) {
        shown.delete(key);
        vscode.window.visibleTextEditors.filter((ed) => ed.document === e.document).forEach(paint);
      }
    }),
    vscode.window.onDidChangeVisibleTextEditors((eds) => eds.forEach(paint)),
  );
}

/** Evaluate code from editor's file (from offset start of it) in repl:
 * shown at pos, printed output to out; its value or error as text. */
async function evaluate(repl, editor, code, start, pos, out) {
  const doc = editor.document;
  showResult(editor, pos, '…', false);
  const id = repl.nextId; // (the request about to be made: its output events have it)
  const sub = repl.onEvent((e) => {
    if (e.event === 'out' && e.id === id) {
      out.append(e.text);
      out.show(true);
    }
  });
  const r = await repl.request({
    op: 'eval',
    code,
    ns: namespaceOf(doc.getText()),
    file: doc.uri.scheme === 'file' ? doc.uri.fsPath : undefined,
    line: doc.positionAt(start).line + 1,
  });
  sub.dispose();
  const error = r.error !== undefined && r.error !== null;
  const text = error ? `Error: ${r.error}` : r.value;
  showResult(editor, pos, text, error);
  return text;
}

module.exports = { startRepl, formAt, namespaceOf, evaluate, watchResults };
