//! M3 done-check: fix a crashed fiber from the editor and resume it.
//!
//! This test plays the editor: it talks to the editor protocol over TCP
//! (Norn data, one map per line), exactly as an editor plugin would.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

struct Editor {
    out: TcpStream,
    lines: std::sync::mpsc::Receiver<String>,
    /// Lines received but not yet matched. Events can arrive before the reply
    /// to the request that caused them, so nothing may be dropped.
    backlog: std::cell::RefCell<Vec<String>>,
    next_id: u32,
}

impl Editor {
    fn connect() -> Editor {
        let addr = norn_vm::server::serve("127.0.0.1:0").expect("server");
        let stream = TcpStream::connect(addr).expect("connect");
        let reader = stream.try_clone().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Editor { out: stream, lines: rx, backlog: Default::default(), next_id: 1 }
    }

    /// Wait for a line satisfying `pred` (responses and events interleave).
    fn wait_for(&self, what: &str, pred: impl Fn(&str) -> bool) -> String {
        {
            let mut b = self.backlog.borrow_mut();
            if let Some(i) = b.iter().position(|l| pred(l)) {
                return b.remove(i);
            }
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) if pred(&line) => return line,
                Ok(line) => self.backlog.borrow_mut().push(line),
                Err(_) => panic!(
                    "timed out waiting for {what}; unmatched lines:\n{}",
                    self.backlog.borrow().join("\n")
                ),
            }
        }
    }

    /// Send a request (the inside of a map, without :id) and wait for its reply.
    fn request(&mut self, body: &str) -> String {
        let id = self.send(body);
        self.reply(id)
    }

    /// Send a request without waiting: its id.
    fn send(&mut self, body: &str) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(self.out, "{{{body} :id {id}}}").unwrap();
        self.out.flush().unwrap();
        id
    }

    /// The reply to request `id`.
    fn reply(&self, id: u32) -> String {
        // Maps print in hash order, so :id can be anywhere in the reply.
        let (end, mid) = (format!(":id {id}}}"), format!(":id {id},"));
        self.wait_for(&format!("reply {id}"), |l| !l.contains(":event :") && (l.contains(&end) || l.contains(&mid)))
    }
}

fn quote(code: &str) -> String {
    // One request per line: newlines inside the code must be escaped.
    format!("\"{}\"", code.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

#[test]
fn fix_a_crashed_fiber_from_the_editor_and_resume_it() {
    let mut ed = Editor::connect();

    // 1. Evaluate buggy code that starts a worker. The editor's evaluations run
    //    with the debugger enabled, and the worker inherits that.
    let r = ed.request(&format!(
        ":op :eval :ns \"user\" :code {}",
        quote(
            "(defn process [x] (/ 100 x))
             (def results (atom []))
             (def worker (spawn-detached (doseq [x [5 0 4]] (swap! results conj (process x))) :done))
             (fiber-id worker)"
        )
    ));
    let fiber: u64 = r.split(":value \"").nth(1).and_then(|s| s.split('"').next()).and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no fiber id in {r}"));

    // 2. The worker crashes on x = 0 and the editor is told it is waiting.
    let ev = ed.wait_for("debug event", |l| l.contains(":event :debug") && l.contains(&format!(":fiber {fiber}")));
    assert!(ev.contains("Divide by zero"), "{ev}");
    assert!(ev.contains("use-value") && ev.contains("retry"), "{ev}");

    // 3. Inspect: the stack is intact, with the failing call and its argument.
    let bt = ed.request(&format!(":op :backtrace :fiber {fiber}"));
    assert!(bt.contains("user/process"), "{bt}");
    assert!(bt.contains("{x 0}"), "{bt}");
    let idx = ed.request(&format!(
        ":op :eval :code {}",
        quote(&format!(
            "(let [fs (backtrace {fiber})] (some (fn [i] (when (= 'user/process (:fn (nth fs i))) i)) (range (count fs))))"
        ))
    ));
    let frame: usize = idx.split(":value \"").nth(1).and_then(|s| s.split('"').next()).and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no frame index in {idx}"));
    let local = ed.request(&format!(":op :eval-in-frame :fiber {fiber} :frame {frame} :code {}", quote("(+ x 1)")));
    assert!(local.contains(":value \"1\""), "{local}");

    // 4. Fix the code in the running image.
    let fix = ed.request(&format!(
        ":op :eval :code {}",
        quote("(defn process [x] (if (zero? x) 0 (/ 100 x)))")
    ));
    assert!(fix.contains("#'user/process"), "{fix}");

    // 5. Resume the crashed fiber by restarting the failed frame: it calls the
    //    new definition with the same argument and carries on.
    let rf = ed.request(&format!(":op :restart-frame :fiber {fiber} :frame {frame}"));
    assert!(rf.contains(":value \":ok\""), "{rf}");
    ed.wait_for("resumed event", |l| l.contains(":event :resumed") && l.contains(&format!(":fiber {fiber}")));

    // 6. The worker finished its whole job, including the items after the crash.
    let done = ed.request(&format!(":op :eval :code {}", quote("[(await worker) @results]")));
    assert!(done.contains("[:done [20 0 25]]"), "{done}");
    let none = ed.request(":op :debugging");
    assert!(none.contains(":value []"), "{none}");
}

#[test]
fn the_editor_formats_and_lints_code() {
    let mut ed = Editor::connect();
    let f = ed.request(&format!(":op :format :code {}", quote("(defn f [x]\n      (let [y   1]\n  (+ x y)))")));
    assert!(f.contains(r#":value "(defn f [x]\n  (let [y 1]\n    (+ x y)))\n""#), "{f}");
    let l = ed.request(&format!(":op :lint :file \"f.norn\" :code {}", quote("(ns lint-from-editor)\n(defn g [x] (let [unused 1] (h x)))")));
    assert!(l.contains(":kind :unused") && l.contains(":kind :unresolved") && l.contains(":line 2"), "{l}");
}

#[test]
fn the_editor_finds_definitions_docs_and_vars() {
    let mut ed = Editor::connect();
    // Evaluated as if at line 40 of a file, as the editor does with ⌘⏎.
    let r = ed.request(&format!(
        ":op :eval :ns \"user\" :file \"/tmp/where.norn\" :line 40 :code {}",
        quote("(defn area\n  \"The area of a w by h rectangle.\"\n  [w h]\n  (* w h))")
    ));
    assert!(r.contains("#'user/area"), "{r}");
    let d = ed.request(":op :definition :ns \"user\" :sym \"area\"");
    assert!(d.contains(":file \"/tmp/where.norn\"") && d.contains(":line 40"), "{d}");
    let doc = ed.request(":op :doc :ns \"user\" :sym \"area\"");
    assert!(doc.contains("The area of a w by h rectangle.") && doc.contains("[w h]"), "{doc}");
    let core = ed.request(":op :doc :ns \"user\" :sym \"map\"");
    assert!(core.contains(":arglists"), "{core}");
    let vars = ed.request(":op :ns-vars :ns \"user\"");
    assert!(vars.contains(":name \"area\""), "{vars}");
    let m = ed.request(&format!(":op :macroexpand :ns \"user\" :code {}", quote("(when ok (go))")));
    assert!(m.contains("(if ok"), "{m}");
    let steps = ed.request(&format!(":op :expand-steps :ns \"user\" :code {}", quote("(when ok (go))")));
    assert!(steps.contains(":macro \"norn.core/when\"") && steps.contains(":expanded \"(if ok (do (go)))\""), "{steps}");
    // Who calls area, and what does it call?
    ed.request(&format!(
        ":op :eval :ns \"user\" :file \"/tmp/where.norn\" :line 50 :code {}",
        quote("(defn floor-area [rooms] (reduce + (map (fn [[w h]] (area w h)) rooms)))")
    ));
    let who = ed.request(":op :who-calls :ns \"user\" :sym \"area\"");
    assert!(who.contains(":name \"floor-area\"") && who.contains(":line 50"), "{who}");
    let calls = ed.request(":op :calls :ns \"user\" :sym \"floor-area\"");
    assert!(calls.contains(":name \"area\"") && calls.contains(":name \"reduce\""), "{calls}");
    let none = ed.request(":op :who-calls :ns \"user\" :sym \"nowhere\"");
    assert!(none.contains(":error \"No definition of nowhere\""), "{none}");
    // Profile an evaluation: its value, the report, and folded stacks.
    ed.request(&format!(
        ":op :eval :ns \"user\" :code {}",
        quote("(defn busy [] (let [end (+ (nano-time) 100000000)] (loop [n 0] (if (< (nano-time) end) (recur (inc n)) n))))")
    ));
    let p = ed.request(&format!(":op :profile :ns \"user\" :interval-us 500 :code {}", quote("(do (busy) :done)")));
    assert!(p.contains(":value \":done\"") && p.contains("samples, about every 500") && p.contains("user/busy"), "{p}");
    assert!(p.contains(":folded \"") && p.contains("user/busy "), "{p}");
}

#[test]
fn the_editor_lists_definitions_its_files_dont_hold() {
    let mut ed = Editor::connect();
    let dir = std::env::temp_dir().join(format!("norn-unsaved-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("drift.norn");
    std::fs::write(&file, "(ns drift)\n\n(defn kept [x]\n  (inc x))\n").unwrap();
    let path = file.display().to_string();
    let r = ed.request(&format!(":op :load-file :path {}", quote(&path)));
    assert!(r.contains(":value"), "{r}");
    // Evaluated from the editor as if at line 3 of the file, changed there
    // but not saved; and a new one, never saved.
    let eval = |ed: &mut Editor, line: u32, code: &str| {
        ed.request(&format!(":op :eval :ns \"drift\" :file {} :line {line} :code {}", quote(&path), quote(code)))
    };
    eval(&mut ed, 3, "(defn kept [x]\n  (+ x 1))");
    eval(&mut ed, 6, "(defn fresh [] :new)");
    let u = ed.request(":op :unsaved");
    assert!(u.contains(":name \"kept\"") && u.contains(":reason :changed"), "{u}");
    assert!(u.contains(":name \"fresh\"") && u.contains(":reason :not-in-file") && u.contains(":from :editor"), "{u}");
    // Saved (laid out differently): only fresh, still not in the file.
    std::fs::write(&file, "(ns drift)\n\n(defn kept [x] (+ x 1))\n").unwrap();
    let u = ed.request(":op :unsaved");
    assert!(!u.contains(":name \"kept\"") && u.contains(":name \"fresh\""), "{u}");
    // Saved too, and loaded again: nothing.
    std::fs::write(&file, "(ns drift)\n\n(defn kept [x] (+ x 1))\n\n(defn fresh [] :new)\n").unwrap();
    ed.request(&format!(":op :load-file :path {}", quote(&path)));
    let u = ed.request(":op :unsaved");
    assert!(!u.contains("drift"), "{u}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn printed_output_streams_and_evaluations_can_be_interrupted() {
    let mut ed = Editor::connect();
    let id = ed.send(&format!(":op :eval :code {}", quote("(dotimes [i 3] (println \"line\" i)) :done")));
    let first = ed.wait_for("output event", |l| l.contains(":event :out") && l.contains(&format!(":id {id}")));
    assert!(first.contains("line 0"), "{first}");
    let r = ed.reply(id);
    assert!(r.contains(":value \":done\""), "{r}");

    // A runaway evaluation, stopped from the editor.
    let busy = ed.send(&format!(":op :eval :code {}", quote("(loop [n 0] (recur (inc n)))")));
    std::thread::sleep(Duration::from_millis(300));
    let i = ed.request(&format!(":op :interrupt :target {busy}"));
    assert!(i.contains(":value \"true\""), "{i}");
    let stopped = ed.reply(busy);
    assert!(stopped.contains("Interrupted"), "{stopped}");
    let none = ed.request(&format!(":op :interrupt :target {busy}"));
    assert!(none.contains(":value \"false\""), "{none}");
}

#[test]
fn the_editor_inspects_loads_files_and_runs_tests() {
    let mut ed = Editor::connect();
    let v = ed.request(&format!(":op :inspect :ns \"user\" :code {}", quote("{:a [1 2 3] :b \"x\"}")));
    assert!(v.contains("Map\"") && v.contains(":count 2"), "{v}");
    // The :a entry can be opened: its id is the third element of its item.
    let at = v.find("[\":a\" \"[1 2 3]\" ").unwrap_or_else(|| panic!("{v}")) + "[\":a\" \"[1 2 3]\" ".len();
    let child: String = v[at..].chars().take_while(|c| c.is_ascii_digit()).collect();
    let c = ed.request(&format!(":op :inspect :target {child}"));
    assert!(c.contains(":count 3") && c.contains("Vector\""), "{c}");

    let dir = std::env::temp_dir().join(format!("norn-editor-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("loaded.norn");
    std::fs::write(&src, "(ns loaded)\n\n(defn twice [x] (* 2 x))\n(twice 21)\n").unwrap();
    let l = ed.request(&format!(":op :load-file :path {}", quote(&src.to_string_lossy())));
    assert!(l.contains(":value \"42\""), "{l}");
    let d = ed.request(":op :definition :ns \"loaded\" :sym \"twice\"");
    assert!(d.contains(":line 3") && d.contains("loaded.norn"), "{d}");

    let t = dir.join("sample_test.norn");
    std::fs::write(
        &t,
        "(ns sample-test (:require [norn.test :refer [is deftest]]))\n(deftest good (is (= 2 (+ 1 1))))\n(deftest bad (is (= 3 (+ 1 1))))\n",
    )
    .unwrap();
    let r = ed.request(&format!(":op :test :paths [{}]", quote(&t.to_string_lossy())));
    assert!(r.contains(":pass 1") && r.contains(":fail 1"), "{r}");
    assert!(r.contains(":expected \"3\"") && r.contains(":actual \"2\""), "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_editors_numbering_requests_alike_dont_cross() {
    // Each editor numbers its own requests: both send id 1 here. An
    // interrupt stops its own editor's request, and output goes to the
    // editor whose request printed it.
    let (mut a, mut b) = (Editor::connect(), Editor::connect());
    let busy = |ed: &mut Editor, tag: &str| {
        ed.send(&format!(":op :eval :code {}", quote(&format!("(println \"{tag}\") (loop [n 0] (recur (inc n)))"))))
    };
    let (ia, ib) = (busy(&mut a, "from-a"), busy(&mut b, "from-b"));
    assert_eq!((ia, ib), (1, 1));
    let out_a = a.wait_for("a's output", |l| l.contains(":event :out"));
    assert!(out_a.contains("from-a"), "{out_a}");
    std::thread::sleep(Duration::from_millis(200));
    let i = a.request(":op :interrupt :target 1");
    assert!(i.contains(":value \"true\""), "{i}");
    assert!(a.reply(1).contains("Interrupted"));
    // b's request is still running, and b never saw a's output.
    let i = b.request(":op :interrupt :target 1");
    assert!(i.contains(":value \"true\""), "b's own request was still running: {i}");
    assert!(b.reply(1).contains("Interrupted"));
    assert!(!b.backlog.borrow().iter().any(|l| l.contains("from-a")), "{:?}", b.backlog.borrow());
}
