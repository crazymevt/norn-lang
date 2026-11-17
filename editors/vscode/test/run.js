// Run the extension's test (suite.js) in an editor: Antigravity IDE by
// default, or the one EDITOR names (a VS Code-like Electron executable,
// such as VS Code's). A fresh profile and extensions folder, so the
// user's aren't touched. NORN is the norn to test (default: norn-lang's
// release build). Each line suite.js writes is one feature's answer; this
// checks them, as crates/norn/tests/lsp.rs checks eglot's.
//
//   node test/run.js          (npm test)

'use strict';

const { spawnSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const ext = path.resolve(__dirname, '..');
const editor = process.env.EDITOR_BIN || '/Applications/Antigravity IDE.app/Contents/MacOS/Electron';
const norn = process.env.NORN || path.resolve(ext, '../../target/release/norn');

/** A Norn project in a temporary folder: norn.edn and src/geo/shapes.norn,
 * as lsp.rs writes it. */
function project(name) {
  const d = fs.mkdtempSync(path.join(os.tmpdir(), `norn-vscode-${name}-`));
  fs.mkdirSync(path.join(d, 'src/geo'), { recursive: true });
  fs.writeFileSync(path.join(d, 'norn.edn'), '{:name geo :paths ["src"]}\n');
  fs.writeFileSync(
    path.join(d, 'src/geo/shapes.norn'),
    `(ns geo.shapes)

(defn area
  "The area of a shape: :square or :circle, of side or radius x."
  [kind x]
  (case kind
    :square (* x x)
    :circle (* 3.14159 x x)))

(defn describe [kind x]
  (let [a (area kind x)]
    (if (< a 100) "small" (do (println "large!") "large"))))
`,
  );
  return d;
}

const proj = project('a');
const other = project('b');
const results = path.join(os.tmpdir(), `norn-vscode-results-${process.pid}.txt`);
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'norn-vscode-profile-'));
fs.writeFileSync(results, '');

const r = spawnSync(
  editor,
  [
    proj,
    `--extensionDevelopmentPath=${ext}`,
    `--extensionTestsPath=${path.join(__dirname, 'suite.js')}`,
    `--user-data-dir=${path.join(profile, 'data')}`,
    `--extensions-dir=${path.join(profile, 'extensions')}`,
    '--disable-workspace-trust',
    // (Antigravity's agent starts a server of its own in every window)
    '--disable-extension=google.google-antigravity',
    '--skip-welcome',
    '--skip-release-notes',
    '--new-window',
  ],
  {
    env: { ...process.env, NORN_TEST_RESULTS: results, NORN_TEST_PROJECT: proj, NORN_TEST_OTHER: other, NORN_TEST_NORN: norn, ELECTRON_RUN_AS_NODE: '' },
    stdio: ['ignore', 'pipe', 'pipe'],
    timeout: 180000,
  },
);

const out = fs.readFileSync(results, 'utf8');
process.stdout.write(out);
const wants = [
  'activated',
  'hover: (geo.shapes/area kind x)',
  'definition: 3',
  'references: 11',
  'diagnostics: unresolved symbol nope',
  'completion: reduce',
  'formatted: "(ns geo.shapes)\\n(defn f [x]\\n  (inc x))\\n"',
  'outline: f',
  'eval: 42',
  'other project: (geo.shapes/area kind x)',
  'repl eval: 42',
  "repl define: #'geo.shapes/half",
  'src/geo/shapes.norn:7: odd: 3',
  'debug frame: geo.shapes/half at line 5',
  'debug locals: x=3',
  'debug evaluate: 6',
  "debug redefine: #'geo.shapes/half",
  'debug restarted: 1',
  // (Stop aborts the fiber: the evaluation fails with its condition)
  'src/geo/shapes.norn:5: odd: 5',
  'done',
];
const missing = wants.filter((w) => !out.includes(w));
if (missing.length || r.status !== 0) {
  console.error(`--- missing: ${JSON.stringify(missing)}; editor exit ${r.status}${r.error ? ` (${r.error.message})` : ''}`);
  console.error(`--- editor stdout:\n${(r.stdout || '').toString().slice(-3000)}`);
  console.error(`--- editor stderr:\n${(r.stderr || '').toString().slice(-3000)}`);
  process.exit(1);
}
console.log('ok');
for (const d of [proj, other, profile]) fs.rmSync(d, { recursive: true, force: true });
fs.rmSync(results, { force: true });
