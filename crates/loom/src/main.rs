//! `loom` — the REPL and script runner.
//!
//! ```text
//! loom                 start a REPL
//! loom FILE.loom       load a file
//! loom -e EXPR         evaluate EXPR and print the result
//! ```

use loom_rt::print::pr_str;
use loom_vm::{INTERRUPT, ReadError, Reader, Vm};
use std::io::{self, BufRead, Write};
use std::process::exit;
use std::sync::atomic::Ordering;

const USAGE: &str = "usage: loom [FILE.loom | -e EXPR]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut vm = Vm::new().unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1)
    });
    // Ctrl-C sets a flag that every Loom function entry and loop back-edge
    // polls, so a runaway loop can be interrupted without killing the process.
    let _ = ctrlc::set_handler(|| INTERRUPT.store(true, Ordering::Relaxed));

    match args.as_slice() {
        [] => repl(&mut vm),
        [flag] if flag == "-h" || flag == "--help" => println!("{USAGE}"),
        [flag, expr] if flag == "-e" => match vm.eval_str(expr) {
            Ok(v) => println!("{}", pr_str(v, true)),
            Err(e) => {
                eprintln!("Error: {e}");
                exit(1)
            }
        },
        [file] if !file.starts_with('-') => {
            if let Err(e) = vm.load_file(file) {
                eprintln!("Error: {e}");
                exit(1)
            }
        }
        _ => {
            eprintln!("{USAGE}");
            exit(2)
        }
    }
}

/// Does `src` contain only complete forms? `Err` carries a syntax error.
fn complete(vm: &mut Vm, src: &str) -> Result<bool, ReadError> {
    let mut r = Reader::new(src);
    loop {
        match r.read(vm) {
            Ok(_) => {}
            Err(ReadError::Eof) => return Ok(true),
            Err(ReadError::Incomplete(_)) => return Ok(false),
            Err(e) => return Err(e),
        }
    }
}

fn repl(vm: &mut Vm) {
    println!(
        "Loom {} (M0: bytecode interpreter, no GC yet)\nCtrl-C interrupts a running evaluation; Ctrl-D exits.",
        env!("CARGO_PKG_VERSION")
    );
    let stdin = io::stdin();
    let mut buf = String::new();
    loop {
        let ns = vm.ns.name.sym_name().to_owned();
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
        match complete(vm, &buf) {
            Ok(false) => continue,
            Err(e) => {
                eprintln!("Syntax error: {e}");
                buf.clear();
                continue;
            }
            Ok(true) => {}
        }

        // Read and evaluate one form at a time, so `(in-ns 'x)` affects the
        // forms after it on the same line.
        let src = std::mem::take(&mut buf);
        let mut r = Reader::new(&src);
        INTERRUPT.store(false, Ordering::Relaxed);
        while let Ok(form) = r.read(vm) {
            match vm.eval(form) {
                Ok(v) => println!("{}", pr_str(v, true)),
                Err(e) => {
                    eprintln!("Error: {e}");
                    break;
                }
            }
        }
    }
}
