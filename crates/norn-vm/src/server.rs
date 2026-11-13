//! The editor protocol (design.md §5.1): Norn data over TCP, one map per line.
//!
//! ```text
//! → {:op :eval :id 1 :code "(+ 1 2)" :ns "user"}
//! ← {:value "3" :ns "user" :id 1}
//! ← {:event :debug :fiber 12 :condition "Divide by zero" :restarts […]}   (pushed)
//! ```
//!
//! Ops (implemented in Norn, `%serve-request` in core.norn): `:eval`,
//! `:load-file`, `:interrupt`, `:debugging`, `:backtrace`, `:restart`,
//! `:restart-frame`, `:abort`, `:eval-in-frame`, `:complete`, `:doc`,
//! `:definition`, `:macroexpand`, `:inspect`, `:ns-vars`, `:test`,
//! `:format` and `:lint`. What an evaluation prints streams as
//! `{:event :out :id … :text …}` events. Each request runs in its own root fiber,
//! so a request that waits in the debugger doesn't block the connection:
//! the editor can inspect it and pick a restart on the same socket.
//!
//! This module only moves lines: Rust threads never touch the Norn heap.

use crate::sched::{RootTask, start, subscribe};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

/// Start serving on `addr` (e.g. "127.0.0.1:7888"; port 0 picks one).
/// Returns the bound address.
pub fn serve(addr: &str) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr)?;
    let local = listener.local_addr()?;
    thread::Builder::new().name("norn-server".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = thread::Builder::new().name("norn-conn".into()).spawn(move || connection(stream));
        }
    })?;
    Ok(local)
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn connection(stream: TcpStream) {
    let Ok(loom) = start() else { return };
    let Ok(mut out) = stream.try_clone() else { return };
    let (tx, rx) = mpsc::channel::<String>();
    // One writer, so responses and events never interleave mid-line.
    thread::spawn(move || {
        for line in rx {
            if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                break;
            }
        }
    });
    let events = subscribe();
    let etx = tx.clone();
    thread::spawn(move || {
        for e in events {
            if etx.send(e).is_err() {
                break;
            }
        }
    });
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let tx = tx.clone();
        thread::spawn(move || {
            let id = request_id(&line);
            let reply = match loom.run(RootTask::Request(line), "user").result {
                Ok(s) => s,
                // An interrupted or failed request: still answer its id.
                Err(e) => match id {
                    Some(id) => format!("{{:error \"{}\" :id {id}}}", escape(&e.to_string())),
                    None => format!("{{:error \"{}\"}}", escape(&e.to_string())),
                },
            };
            let _ = tx.send(reply);
        });
    }
}

/// The `:id` of a request line, if it's a plain integer.
fn request_id(line: &str) -> Option<i64> {
    let at = line.find(":id ")? + 4;
    let digits: String = line[at..].trim_start().chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
    digits.parse().ok()
}
