//! `norn` — the REPL and script runner.
//!
//! ```text
//! norn                 start a REPL
//! norn FILE.norn       load a file
//! norn -e EXPR         evaluate EXPR and print the result
//! norn --server [PORT]  REPL plus the editor protocol on 127.0.0.1:PORT (default 7888)
//! ```
//!
//! This thread never touches the Norn heap: it hands source text to a root
//! fiber on the scheduler and waits for the outcome.

use norn_vm::{RootTask, start};
use std::io::{self, BufRead, Write};
use std::process::exit;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

const USAGE: &str = "usage: norn [FILE.norn | -e EXPR | --server [PORT]]";

/// Ticket of the root fiber the REPL is waiting on (0 = none), for Ctrl-C.
static CURRENT: AtomicU64 = AtomicU64::new(0);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let norn = start().unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1)
    });
    let _ = ctrlc::set_handler(move || {
        let t = CURRENT.load(Ordering::Acquire);
        if t != 0 {
            norn.interrupt(t);
        }
    });

    let done = match args.as_slice() {
        [] => {
            repl(norn);
            return;
        }
        [flag] if flag == "-h" || flag == "--help" => {
            println!("{USAGE}");
            return;
        }
        [flag, expr] if flag == "-e" => run(norn, RootTask::Eval(expr.clone()), true),
        [flag, rest @ ..] if flag == "--server" && rest.len() <= 1 => {
            let port = rest.first().map(String::as_str).unwrap_or("7888");
            match norn_vm::server::serve(&format!("127.0.0.1:{port}")) {
                Ok(addr) => println!("Editor protocol listening on {addr}"),
                Err(e) => {
                    eprintln!("Can't start the editor server: {e}");
                    exit(1)
                }
            }
            repl(norn);
            return;
        }
        [file] if !file.starts_with('-') => run(norn, RootTask::Load(file.clone()), false),
        _ => {
            eprintln!("{USAGE}");
            exit(2)
        }
    };
    exit(if done { 0 } else { 1 })
}

fn run(norn: &norn_vm::Norn, task: RootTask, print: bool) -> bool {
    let t = norn.submit(task, "user");
    CURRENT.store(t, Ordering::Release);
    let done = norn.wait(t);
    let _ = io::stdout().flush();
    match done.result {
        Ok(v) => {
            if print {
                println!("{v}");
            }
            true
        }
        Err(e) => {
            eprintln!("Error: {e}");
            false
        }
    }
}

/// Is `src` an unfinished form? A lexical scan for unbalanced brackets,
/// unterminated strings and block comments. Syntax errors are left to the
/// real reader.
fn needs_more(src: &str) -> bool {
    let mut depth: i64 = 0;
    let mut chars = src.chars().peekable();
    let mut block = 0;
    while let Some(c) = chars.next() {
        if block > 0 {
            match c {
                '|' if chars.peek() == Some(&'#') => {
                    chars.next();
                    block -= 1;
                }
                '#' if chars.peek() == Some(&'|') => {
                    chars.next();
                    block += 1;
                }
                _ => {}
            }
            continue;
        }
        match c {
            ';' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '\\' => {
                chars.next();
            }
            '"' => {
                let mut closed = false;
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                        }
                        '"' => {
                            closed = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !closed {
                    return true;
                }
            }
            '#' if chars.peek() == Some(&'|') => {
                chars.next();
                block += 1;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    block > 0 || depth > 0
}

fn repl(norn: &'static norn_vm::Norn) {
    println!(
        "Norn {} (M3: conditions, restarts and a debugger; fibers on {} carriers)\nCtrl-C interrupts a running evaluation; Ctrl-D exits.",
        env!("CARGO_PKG_VERSION"),
        norn.carriers()
    );
    let events = norn_vm::subscribe();
    let mut ns = "user".to_owned();
    loop {
        let Some(src) = read_form(&format!("{ns}=> "), ns.len()) else { return };
        let t = norn.submit(RootTask::Repl(src), &ns);
        let done = wait_root(norn, &events, t, &ns);
        if let Err(e) = done.result {
            eprintln!("Error: {e}");
        }
        ns = done.ns;
    }
}

/// Read lines until they form complete input. None at end of input.
fn read_form(prompt: &str, indent: usize) -> Option<String> {
    let stdin = io::stdin();
    let mut buf = String::new();
    loop {
        if buf.is_empty() {
            print!("{prompt}");
        } else {
            print!("{}.. ", " ".repeat(indent));
        }
        let _ = io::stdout().flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => {
                println!();
                return None;
            }
            Ok(_) => buf.push_str(&line),
        }
        if !needs_more(&buf) {
            if buf.trim().is_empty() {
                buf.clear();
                continue;
            }
            return Some(buf);
        }
    }
}

/// The fiber id in a `{:event :debug :fiber N …}` line.
fn debug_event_fiber(line: &str) -> Option<u64> {
    if !line.contains(":event :debug") {
        return None;
    }
    let rest = &line[line.find(":fiber ")? + 7..];
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

fn eval_raw(norn: &norn_vm::Norn, src: &str, ns: &str) -> String {
    match norn.run(RootTask::EvalRaw(src.to_owned()), ns).result {
        Ok(s) => s,
        Err(e) => format!("Error: {e}"),
    }
}

/// Wait for root fiber `t`, handling the debugger meanwhile: if `t` (or a
/// fiber it spawned) waits in the debugger, show it and take commands.
fn wait_root(norn: &norn_vm::Norn, events: &mpsc::Receiver<String>, t: u64, ns: &str) -> norn_vm::RootDone {
    CURRENT.store(t, Ordering::Release);
    loop {
        if let Some(done) = norn.wait_timeout(t, Duration::from_millis(30)) {
            CURRENT.store(0, Ordering::Release);
            let _ = io::stdout().flush();
            return done;
        }
        while let Ok(ev) = events.try_recv() {
            let Some(id) = debug_event_fiber(&ev) else { continue };
            print!("{}", eval_raw(norn, &format!("(%debugger-banner {id})"), ns));
            if id == t {
                debugger(norn, events, id, ns);
            } else {
                println!("  (it keeps waiting; use (debug-restart {id} n), (debug-restart-frame {id} i) or (debug-abort {id}))");
            }
        }
    }
}

/// The debugger prompt for fiber `id` (the evaluation the REPL is waiting on).
fn debugger(norn: &norn_vm::Norn, events: &mpsc::Receiver<String>, id: u64, ns: &str) {
    println!("  :r N [args…]  invoke restart N    :f N  restart frame N (newest definition)");
    println!("  :bt  frames again    :a  abort    anything else is evaluated (e.g. redefine a function)");
    loop {
        let prompt = format!("debug[{id}]> ");
        let Some(line) = read_form(&prompt, prompt.len() - 3) else { return };
        let cmd = line.trim();
        let mut words = cmd.split_whitespace();
        let code = match words.next() {
            Some(":r") => format!("(debug-restart {id} {})", words.collect::<Vec<_>>().join(" ")),
            Some(":f") => format!("(debug-restart-frame {id} {})", words.next().unwrap_or("0")),
            Some(":a") => format!("(debug-abort {id})"),
            Some(":bt") => {
                print!("{}", eval_raw(norn, &format!("(%debugger-banner {id})"), ns));
                continue;
            }
            _ => {
                // Ordinary input: evaluate it (it may itself enter the debugger).
                let t = norn.submit(RootTask::Repl(line.clone()), ns);
                if let Err(e) = wait_root(norn, events, t, ns).result {
                    eprintln!("Error: {e}");
                }
                CURRENT.store(id, Ordering::Release);
                continue;
            }
        };
        let reply = eval_raw(norn, &code, ns);
        if reply.starts_with("Error") {
            println!("{reply}");
            continue;
        }
        return; // resumed: go back to waiting for the evaluation
    }
}

#[cfg(test)]
mod tests {
    use super::needs_more;

    #[test]
    fn balance() {
        assert!(!needs_more("(+ 1 2)\n"));
        assert!(needs_more("(defn f [x]\n"));
        assert!(!needs_more("\"(\" ; (\n"));
        assert!(needs_more("\"abc\n"));
        assert!(!needs_more("\\( 1\n"));
        assert!(needs_more("#| (\n"));
    }
}
