// Norn for VS Code: the language (highlighting, brackets, comments), and a
// client for `norn lsp`, one per Norn project open: the nearest folder with
// norn.edn above a file, where `norn lsp` must run to see the project's
// paths and packages.

'use strict';

const fs = require('fs');
const path = require('path');
const vscode = require('vscode');
const { LanguageClient, TransportKind } = require('vscode-languageclient/node');
const { startRepl, formAt, evaluate, watchResults } = require('./repl');
const { registerDebugging } = require('./debug');

/** Project root → its LanguageClient (started, or starting). */
const clients = new Map();
/** One output channel for every server: what code prints, and traces. */
let output;
/** What evaluations in a REPL print. */
let evalOutput;
/** Project root → its REPL (part 5), while it runs; or a promise of it. */
const repls = new Map();
/** Watches a new REPL for the debugger (part 6). */
let watchForDebugging;
/** Said once: the norn command can't be started. */
let toldMissing = false;

/** The project folder for uri: the nearest with norn.edn, no higher than
 * its workspace folder; else that folder, or the file's own. */
function projectRoot(uri) {
  const folder = vscode.workspace.getWorkspaceFolder(uri);
  const top = folder ? folder.uri.fsPath : null;
  for (let d = path.dirname(uri.fsPath); ; d = path.dirname(d)) {
    if (fs.existsSync(path.join(d, 'norn.edn'))) return d;
    if (d === top || path.dirname(d) === d) break;
  }
  return top || path.dirname(uri.fsPath);
}

/** A glob for everything under dir (forward slashes, glob characters escaped). */
function under(dir) {
  return dir.replace(/\\/g, '/').replace(/[[\]{}*?]/g, '[$&]') + '/**/*';
}

/** The client of the project uri is in, started if it isn't yet. */
function clientFor(uri) {
  const root = projectRoot(uri);
  let entry = clients.get(root);
  if (!entry) {
    entry = startClient(root);
    clients.set(root, entry);
  }
  return entry.ready.then(() => entry.client);
}

function startClient(root) {
  const command = vscode.workspace.getConfiguration('norn').get('path') || 'norn';
  const serverOptions = { command, args: ['lsp'], options: { cwd: root }, transport: TransportKind.stdio };
  const clientOptions = {
    documentSelector: [{ scheme: 'file', language: 'norn', pattern: under(root) }],
    workspaceFolder: vscode.workspace.getWorkspaceFolder(vscode.Uri.file(root)),
    outputChannel: output,
  };
  const client = new NornClient('norn', `Norn (${path.basename(root)})`, serverOptions, clientOptions);
  const ready = client.start().catch((e) => {
    clients.delete(root);
    if (!toldMissing) {
      toldMissing = true;
      vscode.window
        .showErrorMessage(`Norn: couldn't start \`${command} lsp\` in ${root}: ${e.message}. Set norn.path to the norn binary.`, 'Open Settings')
        .then((a) => a && vscode.commands.executeCommand('workbench.action.openSettings', 'norn.path'));
    }
    throw e;
  });
  return { client, ready };
}

/** A LanguageClient that doesn't register the server's commands as VS Code
 * commands: with two projects open, the second would register norn.eval
 * again and fail to start. The extension's own commands call them. */
class NornClient extends LanguageClient {
  registerFeature(feature) {
    if (feature.registrationType && feature.registrationType.method === 'workspace/executeCommand') return;
    super.registerFeature(feature);
  }
}

/** Norn: Start REPL: the project's, started if it isn't running (its
 * terminal shown either way). */
async function startReplFor(uri) {
  const root = projectRoot(uri);
  let r = repls.get(root);
  if (!r) {
    const command = vscode.workspace.getConfiguration('norn').get('path') || 'norn';
    r = startRepl(root, command).then(
      (repl) => {
        repls.set(root, repl);
        repl.onEvent((e) => e.event === 'closed' && repls.get(root) === repl && repls.delete(root));
        watchForDebugging(repl);
        return repl;
      },
      (e) => {
        repls.delete(root);
        vscode.window.showErrorMessage(`Norn: the REPL didn't start: ${e.message}`);
        throw e;
      },
    );
    repls.set(root, r);
  }
  const repl = await r;
  repl.terminal.show(true);
  return repl;
}

/** Norn: Evaluate Top-Level Form (or the selection, when there is one): in
 * the project's REPL when it runs, its value shown beside the form; else
 * by the project's norn lsp, which shows its value. Its value as text. */
async function evalTopLevel(editor) {
  editor = editor || vscode.window.activeTextEditor;
  if (!editor || editor.document.languageId !== 'norn') return undefined;
  const doc = editor.document;
  const running = repls.get(projectRoot(doc.uri));
  if (running) {
    const repl = await running;
    const sel = editor.selection;
    const text = doc.getText();
    const range = sel.isEmpty ? formAt(text, doc.offsetAt(sel.active)) : { start: doc.offsetAt(sel.start), end: doc.offsetAt(sel.end) };
    if (!range) return undefined;
    return evaluate(repl, editor, text.slice(range.start, range.end), range.start, doc.positionAt(range.end), evalOutput);
  }
  const client = await clientFor(doc.uri);
  const pos = editor.selection.active;
  return client.sendRequest('workspace/executeCommand', {
    command: 'norn.eval',
    arguments: [client.code2ProtocolConverter.asUri(doc.uri), { line: pos.line, character: pos.character }],
  });
}

function activate(context) {
  output = vscode.window.createOutputChannel('Norn', { log: true });
  evalOutput = vscode.window.createOutputChannel('Norn Evaluation');
  watchForDebugging = registerDebugging(context, (root) => {
    const r = repls.get(root);
    return r && !(r instanceof Promise) ? r : undefined;
  });
  watchResults(context);
  const open = (doc) => {
    if (doc.languageId === 'norn' && doc.uri.scheme === 'file') clientFor(doc.uri).catch(() => {});
  };
  vscode.workspace.textDocuments.forEach(open);
  context.subscriptions.push(
    output,
    vscode.workspace.onDidOpenTextDocument(open),
    evalOutput,
    vscode.commands.registerCommand('norn.evalTopLevel', evalTopLevel),
    vscode.commands.registerCommand('norn.startRepl', () => {
      const ed = vscode.window.activeTextEditor;
      const folder = vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders[0];
      const uri = ed && ed.document.uri.scheme === 'file' ? ed.document.uri : folder && vscode.Uri.joinPath(folder.uri, 'norn.edn');
      if (!uri) return vscode.window.showErrorMessage('Norn: open a file of the project first');
      return startReplFor(uri);
    }),
    vscode.window.onDidCloseTerminal((t) => {
      for (const [root, r] of repls) if (!(r instanceof Promise) && r.terminal === t) { r.close(); repls.delete(root); }
    }),
    vscode.commands.registerCommand('norn.restartServer', async () => {
      const roots = [...clients.keys()];
      await Promise.all([...clients.values()].map((e) => e.client.stop().catch(() => {})));
      clients.clear();
      toldMissing = false;
      roots.forEach((r) => clientFor(vscode.Uri.file(path.join(r, 'norn.edn'))).catch(() => {}));
    }),
    vscode.commands.registerCommand('norn.showOutput', () => output.show(true)),
  );
  // (for the tests: how they reach a project's client)
  return { clientFor, projectRoot, startReplFor, repls };
}

function deactivate() {
  for (const r of repls.values()) if (!(r instanceof Promise)) r.close();
  return Promise.all([...clients.values()].map((e) => e.client.stop().catch(() => {})));
}

module.exports = { activate, deactivate };
