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
        let id = self.next_id;
        self.next_id += 1;
        writeln!(self.out, "{{{body} :id {id}}}").unwrap();
        self.out.flush().unwrap();
        // Maps print in hash order, so :id can be anywhere in the reply.
        let (end, mid) = (format!(":id {id}}}"), format!(":id {id},"));
        self.wait_for(&format!("reply {id}"), |l| !l.contains(":event") && (l.contains(&end) || l.contains(&mid)))
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
