//! `norn --server`, started by another program (Norn Studio starts one per
//! open folder): when that program goes, its pipes close, and the server
//! must end rather than live on as an orphan.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn a_server_ends_when_its_parent_goes() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_norn"))
        .args(["--server", "0"])
        .env_remove("NORN_PATH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start norn --server");
    // wait until it's serving
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while !line.contains("listening") {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0, "it ended before serving");
    }
    // the parent goes: every pipe closes
    drop(child.stdin.take());
    drop(out);
    drop(child.stderr.take());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("norn --server outlived the pipes of the program that started it");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
