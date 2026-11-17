// Part 6: Skein's debugger in VS Code's, through the Debug Adapter Protocol,
// an adapter inside the extension. A REPL's evaluation that fails parks
// its fiber in Skein's debugger (a :debug event); this starts a debug
// session for the REPL, in which each parked fiber is a thread stopped on
// an exception:
//
//   call stack  the fiber's backtrace (:backtrace); a frame opens its
//               function's definition (frames carry no line yet)
//   variables   each frame's locals, printed
//   console     evaluates in the selected frame (:eval-in-frame)
//   Restart Frame   calls the frame's function again, in its newest
//               definition (:restart-frame)
//   Continue    picks one of the condition's restarts (:restart)
//   Stop        aborts the parked evaluations (:abort)
//
// Skein's debugger stops on conditions, not on lines: no breakpoints, no
// stepping.

'use strict';

const vscode = require('vscode');

const FRAMES = 10000; // a frame's DAP id: fiber × FRAMES + its number

class SkeinDebugAdapter {
  constructor(repl) {
    this.repl = repl;
    this.seq = 1;
    this.emitter = new vscode.EventEmitter();
    this.onDidSendMessage = this.emitter.event;
    /** fiber → its frames as :backtrace gave them (numbered as Skein does). */
    this.frames = new Map();
    /** Ended (terminated sent): a later stop needs a session of its own. */
    this.done = false;
    this.sub = repl.onEvent((e) => this.event(e));
  }

  end() {
    if (this.done) return;
    this.done = true;
    this.sub.dispose();
    this.notify('terminated', {});
  }

  send(m) {
    this.emitter.fire({ seq: this.seq++, ...m });
  }
  respond(req, body, error) {
    this.send({ type: 'response', request_seq: req.seq, command: req.command, success: !error, message: error, body });
  }
  notify(event, body) {
    this.send({ type: 'event', event, body });
  }

  stoppedOn(fiber, e) {
    this.frames.delete(fiber);
    this.notify('stopped', { reason: 'exception', description: e.condition, text: e.condition, threadId: fiber, allThreadsStopped: false });
  }

  event(e) {
    if (e.event === 'debug') this.stoppedOn(e.fiber, e);
    else if (e.event === 'resumed') {
      this.frames.delete(e.fiber);
      this.notify('continued', { threadId: e.fiber, allThreadsContinued: false });
      this.notify('thread', { reason: 'exited', threadId: e.fiber });
      if (this.repl.stopped.size === 0) this.end();
    } else if (e.event === 'closed') this.end();
  }

  async backtrace(fiber) {
    if (!this.frames.has(fiber)) {
      const r = await this.repl.request({ op: 'backtrace', fiber });
      this.frames.set(fiber, r.frames || []);
    }
    return this.frames.get(fiber);
  }

  /** Where function name (ns/f, or ns/f/fn for a fn inside it) is defined. */
  async sourceOf(name) {
    const sym = name.split('/').slice(0, 2).join('/');
    const r = await this.repl.request({ op: 'definition', sym, ns: 'user' });
    return r && r.file && r.line ? { source: { name: r.file.split('/').pop(), path: r.file }, line: r.line } : null;
  }

  async handleMessage(req) {
    try {
      await this.handle(req);
    } catch (e) {
      this.respond(req, undefined, e.message || String(e));
    }
  }

  async handle(req) {
    const a = req.arguments || {};
    const fiberOf = (frameId) => Math.floor(frameId / FRAMES);
    switch (req.command) {
      case 'initialize':
        this.respond(req, {
          supportsConfigurationDoneRequest: true,
          supportsRestartFrame: true,
          supportsTerminateRequest: true,
          supportsExceptionInfoRequest: true,
          supportsEvaluateForHovers: true,
        });
        this.notify('initialized', {});
        return;
      case 'attach':
      case 'launch':
        this.respond(req);
        for (const [fiber, e] of this.repl.stopped) this.stoppedOn(fiber, e);
        return;
      case 'configurationDone':
      case 'setBreakpoints':
      case 'setExceptionBreakpoints':
        return this.respond(req, req.command === 'setBreakpoints' ? { breakpoints: [] } : undefined);
      case 'threads':
        return this.respond(req, { threads: [...this.repl.stopped.keys()].map((id) => ({ id, name: `fiber ${id}` })) });
      case 'exceptionInfo': {
        const e = this.repl.stopped.get(a.threadId) || {};
        return this.respond(req, { exceptionId: e.class || 'condition', description: e.condition, breakMode: 'always' });
      }
      case 'stackTrace': {
        const frames = await this.backtrace(a.threadId);
        const shown = frames.map((f, i) => ({ f, i })).filter(({ f }) => !f.internal);
        const out = await Promise.all(
          shown.map(async ({ f, i }) => {
            const at = await this.sourceOf(f.fn);
            return { id: a.threadId * FRAMES + i, name: f.fn, line: at ? at.line : 0, column: at ? 1 : 0, source: at ? at.source : undefined, canRestart: true };
          }),
        );
        return this.respond(req, { stackFrames: out, totalFrames: out.length });
      }
      case 'scopes':
        return this.respond(req, { scopes: [{ name: 'Locals', variablesReference: a.frameId, expensive: false }] });
      case 'variables': {
        const fiber = fiberOf(a.variablesReference);
        const f = (await this.backtrace(fiber))[a.variablesReference % FRAMES] || {};
        return this.respond(req, { variables: (f.vars || []).map(([name, value]) => ({ name, value, variablesReference: 0 })) });
      }
      case 'evaluate': {
        const r =
          a.frameId !== undefined
            ? await this.repl.request({ op: 'eval-in-frame', fiber: fiberOf(a.frameId), frame: a.frameId % FRAMES, code: a.expression })
            : await this.repl.request({ op: 'eval', code: a.expression });
        if (r.error) return this.respond(req, undefined, r.error);
        return this.respond(req, { result: r.value, variablesReference: 0 });
      }
      case 'restartFrame': {
        const r = await this.repl.request({ op: 'restart-frame', fiber: fiberOf(a.frameId), frame: a.frameId % FRAMES });
        return this.respond(req, undefined, r.error);
      }
      case 'continue': {
        const e = this.repl.stopped.get(a.threadId);
        if (!e) return this.respond(req, { allThreadsContinued: false });
        const picked = await vscode.window.showQuickPick(
          (e.restarts || []).map((r, i) => ({ label: r.name, description: r.report, index: i })),
          { title: `Fiber ${a.threadId}: ${e.condition}`, placeHolder: 'A restart to invoke' },
        );
        if (!picked) return this.respond(req, undefined, 'No restart chosen');
        let args;
        if (picked.label === 'use-value' || picked.label === 'store-value') {
          args = await vscode.window.showInputBox({ title: picked.description, prompt: 'The value (Skein)' });
          if (args === undefined) return this.respond(req, undefined, 'No value given');
        }
        const r = await this.repl.request({ op: 'restart', fiber: a.threadId, restart: picked.index, args });
        return this.respond(req, { allThreadsContinued: false }, r.error);
      }
      case 'next':
      case 'stepIn':
      case 'stepOut':
      case 'pause':
        return this.respond(req, undefined, "Skein's debugger stops on conditions and doesn't step: Continue picks a restart.");
      case 'terminate':
      case 'disconnect':
        if (req.command === 'terminate' || (a.terminateDebuggee !== false && req.command === 'disconnect')) {
          await Promise.all([...this.repl.stopped.keys()].map((fiber) => this.repl.request({ op: 'abort', fiber })));
        }
        this.respond(req);
        if (req.command === 'terminate') this.end();
        else this.done = true;
        return;
      default:
        return this.respond(req, undefined, `Not supported: ${req.command}`);
    }
  }

  dispose() {
    this.done = true;
    this.sub.dispose();
  }
}

/** Debugging for REPLs: replFor(root) gives a project's REPL. A REPL's
 * :debug event starts a session for it unless one is running. */
function registerDebugging(context, replFor) {
  // project root → its session's adapter (or {done: false} while it starts)
  const running = new Map();
  context.subscriptions.push(
    vscode.debug.registerDebugAdapterDescriptorFactory('skein', {
      createDebugAdapterDescriptor(session) {
        const repl = replFor(session.configuration.skeinRoot);
        if (!repl) return undefined;
        const adapter = new SkeinDebugAdapter(repl);
        running.set(repl.root, adapter);
        return new vscode.DebugAdapterInlineImplementation(adapter);
      },
    }),
  );
  /** Watch repl's events: a stop with no session running starts one (one
   * ending doesn't count: it no longer hears stops). */
  return (repl) =>
    repl.onEvent((e) => {
      const r = running.get(repl.root);
      if (e.event !== 'debug' || (r && !r.done)) return;
      running.set(repl.root, { done: false });
      const folder = vscode.workspace.getWorkspaceFolder(vscode.Uri.file(repl.root));
      vscode.debug
        .startDebugging(folder, { type: 'skein', request: 'attach', name: `Skein (${repl.root.split('/').pop()})`, skeinRoot: repl.root })
        .then((ok) => ok || running.delete(repl.root), () => running.delete(repl.root));
    });
}

module.exports = { registerDebugging, SkeinDebugAdapter };
