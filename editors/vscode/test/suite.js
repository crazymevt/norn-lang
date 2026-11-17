// The extension's test, run inside the editor by test/run.js: each
// feature's answer, a line each, to the file NORN_TEST_RESULTS names.

'use strict';

const fs = require('fs');
const path = require('path');
const vscode = require('vscode');

const say = (line) => fs.appendFileSync(process.env.NORN_TEST_RESULTS, line + '\n');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Wait (at most ms) for f to give something truthy. */
async function until(f, ms = 15000) {
  for (const end = Date.now() + ms; Date.now() < end; await sleep(100)) {
    const v = await f();
    if (v) return v;
  }
  return undefined;
}

/** The position of the start of text on (1-based) line n of doc. */
function at(doc, n, text) {
  const line = doc.lineAt(n - 1).text;
  return new vscode.Position(n - 1, line.indexOf(text) + 1);
}

async function replaceAll(editor, text) {
  const doc = editor.document;
  await editor.edit((e) => e.replace(new vscode.Range(0, 0, doc.lineCount, 0), text));
}

const hoverText = (hs) =>
  (hs || [])
    .flatMap((h) => h.contents)
    .map((c) => (typeof c === 'string' ? c : c.value))
    .join(' ')
    .replace(/```\w*/g, '')
    .replace(/\s+/g, ' ')
    .trim();

async function run() {
  try {
    await steps();
  } catch (e) {
    say(`error: ${e && e.stack ? e.stack : e}`);
    throw e;
  }
}

async function steps() {
  await vscode.workspace.getConfiguration('norn').update('path', process.env.NORN_TEST_NORN, vscode.ConfigurationTarget.Global);
  const file = vscode.Uri.file(path.join(process.env.NORN_TEST_PROJECT, 'src/geo/shapes.norn'));
  const doc = await vscode.workspace.openTextDocument(file);
  let editor = await vscode.window.showTextDocument(doc);
  const ext = vscode.extensions.getExtension('norn.norn');
  const api = await ext.activate();
  await api.clientFor(file);
  say('activated');

  // (the server loads the file's module when it's opened: hover waits for it)
  const hover = await until(async () => {
    const t = hoverText(await vscode.commands.executeCommand('vscode.executeHoverProvider', file, at(doc, 11, '(area')));
    return t.includes('geo.shapes/area') && t;
  });
  say(`hover: ${hover}`);

  const defs = await vscode.commands.executeCommand('vscode.executeDefinitionProvider', file, at(doc, 11, '(area'));
  say(`definition: ${(defs || []).map((d) => (d.range || d.targetRange).start.line + 1).join(' ')}`);

  const refs = await vscode.commands.executeCommand('vscode.executeReferenceProvider', file, at(doc, 3, 'area'));
  say(`references: ${(refs || []).map((r) => r.range.start.line + 1).join(' ')}`);

  await editor.edit((e) => e.insert(new vscode.Position(doc.lineCount, 0), '\n(defn oops [x] (nope x))\n'));
  const diags = await until(() => {
    const ds = vscode.languages.getDiagnostics(file);
    return ds.length && ds.map((d) => d.message).join('; ');
  });
  say(`diagnostics: ${diags}`);

  await editor.edit((e) => e.insert(new vscode.Position(doc.lineCount, 0), '(redu'));
  const end = doc.lineAt(doc.lineCount - 1).range.end;
  const items = await vscode.commands.executeCommand('vscode.executeCompletionItemProvider', file, end);
  const labels = (items ? items.items : []).map((i) => (typeof i.label === 'string' ? i.label : i.label.label));
  say(`completion: ${labels.filter((l) => l.startsWith('redu')).sort().join(' ')}`);

  await replaceAll(editor, '(ns geo.shapes)\n(defn  f [x]\n      (inc x))\n');
  const edits = await vscode.commands.executeCommand('vscode.executeFormatDocumentProvider', file, { tabSize: 2, insertSpaces: true });
  const we = new vscode.WorkspaceEdit();
  we.set(file, edits || []);
  await vscode.workspace.applyEdit(we);
  say(`formatted: ${JSON.stringify(doc.getText())}`);

  const syms = await until(async () => {
    const ss = await vscode.commands.executeCommand('vscode.executeDocumentSymbolProvider', file);
    return ss && ss.length && ss;
  });
  say(`outline: ${(syms || []).map((s) => s.name).join(' ')}`);

  await replaceAll(editor, '(ns geo.shapes)\n\n(+ 40\n   2)\n');
  editor.selection = new vscode.Selection(3, 2, 3, 2);
  say(`eval: ${await vscode.commands.executeCommand('norn.evalTopLevel')}`);

  // (a second project: a client of its own)
  const file2 = vscode.Uri.file(path.join(process.env.NORN_TEST_OTHER, 'src/geo/shapes.norn'));
  const doc2 = await vscode.workspace.openTextDocument(file2);
  await vscode.window.showTextDocument(doc2);
  await api.clientFor(file2);
  const hover2 = await until(async () => {
    const t = hoverText(await vscode.commands.executeCommand('vscode.executeHoverProvider', file2, at(doc2, 11, '(area')));
    return t.includes('geo.shapes/area') && t;
  });
  say(`other project: ${hover2}`);

  await vscode.commands.executeCommand('workbench.action.revertAndCloseActiveEditor');
  editor = await vscode.window.showTextDocument(doc);

  // (part 5: the REPL; evaluations go to it once it runs)
  await api.startReplFor(file);
  await replaceAll(editor, '(ns geo.shapes)\n\n(+ 40 2)\n\n(defn half [x] (when (odd? x) (error "odd:" x)) (quot x 2))\n\n(half 3)\n\n(half 5)\n');
  const evalLine = async (n) => {
    editor.selection = new vscode.Selection(n - 1, 1, n - 1, 1);
    return vscode.commands.executeCommand('norn.evalTopLevel');
  };
  say(`repl eval: ${await evalLine(3)}`);
  say(`repl define: ${await evalLine(5)}`);

  // (part 6: an error stops in the debugger)
  const started = new Promise((r) => vscode.debug.onDidStartDebugSession((s) => s.type === 'norn' && r(s)));
  const failing = evalLine(7);
  const session = await Promise.race([started, sleep(15000).then(() => null)]);
  if (!session) throw new Error('no debug session started');
  const threads = await until(async () => {
    const t = await session.customRequest('threads');
    return t.threads.length && t.threads;
  });
  const info = await session.customRequest('exceptionInfo', { threadId: threads[0].id });
  say(`debug stopped: ${info.description}`);
  const stack = await session.customRequest('stackTrace', { threadId: threads[0].id });
  const frame = stack.stackFrames.find((f) => f.name === 'geo.shapes/half');
  say(`debug frame: ${frame && frame.name} at line ${frame && frame.line}`);
  const scopes = await session.customRequest('scopes', { frameId: frame.id });
  const vars = await session.customRequest('variables', { variablesReference: scopes.scopes[0].variablesReference });
  say(`debug locals: ${vars.variables.map((v) => `${v.name}=${v.value}`).join(' ')}`);
  const inFrame = await session.customRequest('evaluate', { expression: '(* x 2)', frameId: frame.id, context: 'repl' });
  say(`debug evaluate: ${inFrame.result}`);
  await replaceAll(editor, '(ns geo.shapes)\n\n(defn half [x] (quot x 2))\n');
  say(`debug redefine: ${await evalLine(3)}`);
  await session.customRequest('restartFrame', { frameId: frame.id });
  say(`debug restarted: ${await failing}`);

  // (Stop aborts what's waiting)
  await replaceAll(editor, '(ns geo.shapes)\n\n(defn half [x] (when (odd? x) (error "odd:" x)) (quot x 2))\n\n(half 5)\n');
  await evalLine(3);
  const started2 = new Promise((r) => vscode.debug.onDidStartDebugSession((s) => s.type === 'norn' && r(s)));
  const failing2 = evalLine(5);
  const session2 = await Promise.race([started2, sleep(15000).then(() => null)]);
  if (session2) {
    await until(async () => (await session2.customRequest('threads')).threads.length);
    await vscode.debug.stopDebugging(session2);
  }
  say(`debug stop: ${await Promise.race([failing2, sleep(10000).then(() => 'still waiting')])}`);

  await vscode.commands.executeCommand('workbench.action.revertAndCloseActiveEditor');
  say('done');
}

module.exports = { run };
