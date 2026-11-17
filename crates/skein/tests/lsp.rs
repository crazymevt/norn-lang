//! M9b part 5: `skein lsp`. A client's conversation (JSON-RPC on the
//! server's standard input and output, as editors send it), and, when Emacs
//! 29 or later is installed, its LSP client (eglot) using the server
//! (tests/lsp/eglot-check.el).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

fn project(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skein-lsp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("src/geo")).unwrap();
    std::fs::write(d.join("skein.edn"), "{:name geo :paths [\"src\"]}\n").unwrap();
    std::fs::write(
        d.join("src/geo/shapes.skn"),
        r#"(ns geo.shapes)

(defn area
  "The area of a shape: :square or :circle, of side or radius x."
  [kind x]
  (case kind
    :square (* x x)
    :circle (* 3.14159 x x)))

(defn describe [kind x]
  (let [a (area kind x)]
    (if (< a 100) "small" (do (println "large!") "large"))))
"#,
    )
    .unwrap();
    d
}

struct Client {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: u32,
    notes: Vec<String>,
}

impl Client {
    fn start(dir: &Path) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_skein"))
            .arg("lsp")
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Client { child, input, output, next: 0, notes: Vec::new() }
    }

    fn send(&mut self, json: &str) {
        write!(self.input, "Content-Length: {}\r\n\r\n{json}", json.len()).unwrap();
        self.input.flush().unwrap();
    }

    fn receive(&mut self) -> String {
        let mut length = 0;
        loop {
            let mut line = String::new();
            assert!(self.output.read_line(&mut line).unwrap() > 0, "the server closed its output");
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            let (k, v) = line.split_once(':').unwrap();
            assert!(k.eq_ignore_ascii_case("content-length"), "a header: {line}");
            length = v.trim().parse().unwrap();
        }
        let mut body = vec![0; length];
        self.output.read_exact(&mut body).unwrap();
        String::from_utf8(body).unwrap()
    }

    fn request(&mut self, method: &str, params: &str) -> String {
        self.next += 1;
        let id = self.next;
        self.send(&format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{params}}}"#));
        loop {
            let m = self.receive();
            if m.contains(&format!(r#""id":{id},"#)) || m.contains(&format!(r#""id":{id}}}"#)) {
                return m;
            }
            self.notes.push(m);
        }
    }

    fn notify(&mut self, method: &str, params: &str) {
        self.send(&format!(r#"{{"jsonrpc":"2.0","method":"{method}","params":{params}}}"#));
    }
}

fn json_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

#[test]
fn a_client_gets_what_it_asks_for() {
    let d = project("protocol");
    let path = d.join("src/geo/shapes.skn");
    let uri = format!("file://{}", path.display());
    let text = std::fs::read_to_string(&path).unwrap();
    let mut c = Client::start(&d);
    let init = c.request("initialize", r#"{"processId":null,"rootUri":null,"capabilities":{}}"#);
    for cap in ["hoverProvider", "definitionProvider", "referencesProvider", "documentSymbolProvider", "documentFormattingProvider", "skein.eval"] {
        assert!(init.contains(cap), "{cap}: {init}");
    }
    c.notify("initialized", "{}");
    let doc = |t: &str| format!(r#"{{"uri":"{uri}","languageId":"skein","version":1,"text":{}}}"#, json_string(t));
    c.notify("textDocument/didOpen", &format!(r#"{{"textDocument":{}}}"#, doc(&text)));
    let at = |line: u32, ch: u32| format!(r#"{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":{line},"character":{ch}}}}}"#);

    let h = c.request("textDocument/hover", &at(10, 12));
    assert!(h.contains("(geo.shapes/area kind x)") && h.contains("The area of a shape"), "{h}");
    let d1 = c.request("textDocument/definition", &at(10, 12));
    assert!(d1.contains("shapes.skn") && d1.contains(r#""line":2"#), "the defn's line, 0-based: {d1}");
    let r = c.request("textDocument/references", &at(2, 7));
    assert!(r.contains(r#""start":{"character":11,"line":10}"#) || r.contains(r#""start":{"line":10,"character":11}"#), "where describe calls it: {r}");
    let comp = c.request("textDocument/completion", &at(10, 13));
    assert!(comp.contains(r#""label":"area""#), "{comp}");
    let syms = c.request("textDocument/documentSymbol", &format!(r#"{{"textDocument":{{"uri":"{uri}"}}}}"#));
    assert!(syms.contains(r#""name":"area""#) && syms.contains(r#""name":"describe""#), "{syms}");

    // A change with a mistake: diagnostics, pushed.
    let bad = format!("{text}\n(defn oops [x] (nope x))\n");
    c.notify("textDocument/didChange", &format!(r#"{{"textDocument":{{"uri":"{uri}","version":2}},"contentChanges":[{{"text":{}}}]}}"#, json_string(&bad)));
    let _ = c.request("textDocument/documentSymbol", &format!(r#"{{"textDocument":{{"uri":"{uri}"}}}}"#));
    assert!(c.notes.iter().any(|n| n.contains("publishDiagnostics") && n.contains("unresolved symbol nope")), "{:?}", c.notes);

    // Formatting: the whole text, as skein fmt makes it.
    c.notify("textDocument/didChange", &format!(r#"{{"textDocument":{{"uri":"{uri}","version":3}},"contentChanges":[{{"text":{}}}]}}"#, json_string("(ns geo.shapes)\n(defn  f [x]\n      (inc x))\n")));
    let f = c.request("textDocument/formatting", &format!(r#"{{"textDocument":{{"uri":"{uri}"}},"options":{{"tabSize":2,"insertSpaces":true}}}}"#));
    assert!(f.contains(r#"(defn f [x]\n  (inc x))\n"#), "{f}");

    // skein.eval: the top-level form at a position, evaluated; what it prints
    // goes to standard error, not into the protocol.
    c.notify("textDocument/didChange", &format!(r#"{{"textDocument":{{"uri":"{uri}","version":4}},"contentChanges":[{{"text":{}}}]}}"#, json_string(&text)));
    let e = c.request("workspace/executeCommand", &format!(r#"{{"command":"skein.eval","arguments":["{uri}",{{"line":11,"character":4}}]}}"#));
    assert!(e.contains("geo.shapes/describe"), "{e}");
    let e = c.request("workspace/executeCommand", r#"{"command":"skein.nothing","arguments":[]}"#);
    assert!(e.contains("-32601"), "{e}");
    let u = c.request("no/such", "{}");
    assert!(u.contains("-32601") && u.contains("Unknown method"), "{u}");

    let s = c.request("shutdown", "null");
    assert!(s.contains(r#""result":null"#), "{s}");
    c.notify("exit", "null");
    let status = c.child.wait().unwrap();
    assert!(status.success(), "exit after shutdown: 0");
}

#[test]
fn emacs_eglot_uses_it() {
    let emacs = Command::new("emacs").args(["--batch", "-Q", "--eval", "(princ emacs-major-version)"]).output();
    let Ok(o) = emacs else { return eprintln!("skipped: no Emacs") };
    if String::from_utf8_lossy(&o.stdout).trim().parse::<u32>().unwrap_or(0) < 29 {
        return eprintln!("skipped: Emacs before 29 has no eglot");
    }
    let d = project("eglot");
    // (a project, to Emacs, is a version-controlled folder)
    let _ = Command::new("git").args(["init", "-q"]).current_dir(&d).status();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/lsp/eglot-check.el");
    let o = Command::new("emacs")
        .args(["--batch", "-Q", "-l"])
        .arg(&script)
        .env("SKEIN", env!("CARGO_BIN_EXE_skein"))
        .env("PROJ", &d)
        .current_dir(&d)
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    for want in [
        "connected: skein",
        "hover: ```clojure (geo.shapes/area kind x) ```",
        "definition: (3)",
        "references: (11)",
        "diagnostics: (skein lint [unresolved]: unresolved symbol nope)",
        "completion: (reduce reduce-kv)",
        "formatted: \"(ns geo.shapes)\n(defn f [x]\n  (inc x))\n\"",
        "outline: (f)",
        "shut down",
    ] {
        assert!(out.contains(want), "{want}\n--- out:\n{out}\n--- err:\n{}", String::from_utf8_lossy(&o.stderr));
    }
}

/// The VS Code extension (editors/vscode) in an editor built on VS Code:
/// its test (editors/vscode/test/run.js) runs it on a project like the one
/// above, in Antigravity IDE (or the editor EDITOR_BIN names). It opens a
/// window for a few seconds, so it runs only when asked
/// (`SKEIN_VSCODE_TEST=1`), with Node and the extension's packages
/// installed (`npm install` in editors/vscode).
#[test]
fn vscode_extension_uses_it() {
    if std::env::var("SKEIN_VSCODE_TEST").as_deref() != Ok("1") {
        return eprintln!("skipped: SKEIN_VSCODE_TEST=1 runs it (it opens an editor window)");
    }
    let ext = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../editors/vscode");
    assert!(ext.join("node_modules/vscode-languageclient").exists(), "npm install in editors/vscode first");
    let o = Command::new("node")
        .arg("test/run.js")
        .env("SKEIN", env!("CARGO_BIN_EXE_skein"))
        .current_dir(&ext)
        .output()
        .expect("node");
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success() && out.contains("\nok"), "--- out:\n{out}\n--- err:\n{}", String::from_utf8_lossy(&o.stderr));
}
