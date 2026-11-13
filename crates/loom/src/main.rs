//! `loom` — the REPL and script runner.
//!
//! ```text
//! loom                 start a REPL
//! loom FILE.loom       load a file
//! loom -e EXPR         evaluate EXPR and print the result
//! ```
//!
//! This thread never touches the Loom heap: it hands source text to a root
//! fiber on the scheduler and waits for the outcome.

use loom_vm::{RootTask, start};
use std::io::{self, BufRead, Write};
use std::process::exit;
use std::sync::atomic::{AtomicU64, Ordering};

const USAGE: &str = "usage: loom [FILE.loom | -e EXPR]";

/// Ticket of the root fiber the REPL is waiting on (0 = none), for Ctrl-C.
static CURRENT: AtomicU64 = AtomicU64::new(0);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let loom = start().unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1)
    });
    let _ = ctrlc::set_handler(move || {
        let t = CURRENT.load(Ordering::Acquire);
        if t != 0 {
            loom.interrupt(t);
        }
    });

    let done = match args.as_slice() {
        [] => {
            repl(loom);
            return;
        }
        [flag] if flag == "-h" || flag == "--help" => {
            println!("{USAGE}");
            return;
        }
        [flag, expr] if flag == "-e" => run(loom, RootTask::Eval(expr.clone()), true),
        [file] if !file.starts_with('-') => run(loom, RootTask::Load(file.clone()), false),
        _ => {
            eprintln!("{USAGE}");
            exit(2)
        }
    };
    exit(if done { 0 } else { 1 })
}

fn run(loom: &loom_vm::Loom, task: RootTask, print: bool) -> bool {
    let t = loom.submit(task, "user");
    CURRENT.store(t, Ordering::Release);
    let done = loom.wait(t);
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

fn repl(loom: &'static loom_vm::Loom) {
    println!(
        "Loom {} (M1: fibers on {} carriers)\nCtrl-C interrupts a running evaluation; Ctrl-D exits.",
        env!("CARGO_PKG_VERSION"),
        loom.carriers()
    );
    let stdin = io::stdin();
    let mut ns = "user".to_owned();
    let mut buf = String::new();
    loop {
        if buf.is_empty() {
            print!("{ns}=> ");
        } else {
            print!("{}.. ", " ".repeat(ns.len()));
        }
        let _ = io::stdout().flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => {
                println!();
                return;
            }
            Ok(_) => buf.push_str(&line),
        }
        if needs_more(&buf) {
            continue;
        }
        let src = std::mem::take(&mut buf);
        if src.trim().is_empty() {
            continue;
        }
        let t = loom.submit(RootTask::Repl(src), &ns);
        CURRENT.store(t, Ordering::Release);
        let done = loom.wait(t);
        CURRENT.store(0, Ordering::Release);
        let _ = io::stdout().flush();
        if let Err(e) = done.result {
            eprintln!("Error: {e}");
        }
        ns = done.ns;
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
