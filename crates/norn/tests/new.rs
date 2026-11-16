//! `norn new`, `norn run` and `norn dev`: a new project runs, tests, lints
//! and packages as it is; run calls its :main; dev reloads what's saved
//! while it runs.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn norn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_norn"));
    c.env_remove("NORN_PATH");
    c
}

fn temp(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("norn-new-{name}-{nanos}"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(c: &mut Command) -> (i32, String) {
    let out = c.output().expect("run");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn a_new_project_runs_tests_and_packages_as_it_is() {
    let dir = temp("project");
    let (code, text) = run(norn().args(["new", "my-app", "--no-git"]).current_dir(&dir));
    assert_eq!(0, code, "{text}");
    let proj = dir.join("my-app");
    for f in ["norn.edn", "src/my-app/main.norn", "tests/main_test.norn", "README.md", ".gitignore"] {
        assert!(proj.join(f).is_file(), "{f} is made");
    }
    assert!(!proj.join(".git").exists(), "--no-git");
    let (code, text) = run(norn().args(["run", "Ada", "Grace"]).current_dir(&proj));
    assert_eq!((0, "Hello, Ada and Grace!\n".to_string()), (code, text));
    let (code, text) = run(norn().arg("test").current_dir(&proj));
    assert_eq!(0, code, "its test passes:\n{text}");
    let (code, text) = run(norn().args(["fmt", "--check", "src", "tests"]).current_dir(&proj));
    assert_eq!(0, code, "formatted:\n{text}");
    let (code, text) = run(norn().args(["lint", "src", "tests"]).current_dir(&proj));
    assert_eq!((0, String::new()), (code, text), "lint-clean");
    let (code, text) = run(norn().args(["package", "--cli"]).current_dir(&proj));
    assert_eq!(0, code, "{text}");
    let os = if cfg!(target_os = "macos") { "macos" } else { "linux" };
    assert!(proj.join(format!("dist/my-app-0.1.0-{os}-{}.tar.gz", std::env::consts::ARCH)).is_file());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_gives_mains_integer_as_the_exit_status() {
    let dir = temp("status");
    run(norn().args(["new", "four", "--no-git"]).current_dir(&dir));
    let proj = dir.join("four");
    std::fs::write(proj.join("src/four/main.norn"), "(ns four.main)\n(defn main [& args] (+ 2 (count args)))\n").unwrap();
    assert_eq!(4, run(norn().args(["run", "a", "b"]).current_dir(&proj)).0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn new_makes_a_git_repository_unless_its_in_one() {
    if Command::new("git").arg("--version").output().is_err() {
        return;
    }
    let dir = temp("git");
    run(norn().args(["new", "repo-app"]).current_dir(&dir));
    assert!(dir.join("repo-app/.git").is_dir());
    // (inside that repository, the next one isn't a repository of its own)
    run(norn().args(["new", "inner"]).current_dir(dir.join("repo-app")));
    assert!(dir.join("repo-app/inner/norn.edn").is_file() && !dir.join("repo-app/inner/.git").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn what_new_and_run_refuse() {
    let dir = temp("refuse");
    let (code, text) = run(norn().args(["new", "My_App"]).current_dir(&dir));
    assert_eq!(1, code);
    assert!(text.contains("lower-case letters and digits"), "{text}");
    std::fs::create_dir_all(dir.join("taken")).unwrap();
    std::fs::write(dir.join("taken/notes.txt"), "mine").unwrap();
    let (code, text) = run(norn().args(["new", "taken", "--no-git"]).current_dir(&dir));
    assert_eq!(1, code);
    assert!(text.contains("isn't an empty folder"), "{text}");
    let (code, text) = run(norn().arg("run").current_dir(&dir));
    assert_eq!(2, code, "no project: {text}");
    assert!(text.contains("no norn.edn"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Read p's output until it holds `want` (or 20 s pass): everything so far.
fn read_until(out: &mut impl Read, seen: &mut String, want: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut buf = [0u8; 4096];
    while !seen.contains(want) {
        if Instant::now() > deadline {
            return false;
        }
        match out.read(&mut buf) {
            Ok(0) | Err(_) => return false,
            Ok(n) => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
        }
    }
    true
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

#[test]
fn dev_reloads_what_is_saved_while_main_runs() {
    let dir = temp("dev");
    run(norn().args(["new", "live", "--no-git"]).current_dir(&dir));
    let proj = dir.join("live");
    let main = proj.join("src/live/main.norn");
    let body = |word: &str| {
        format!("(ns live.main)\n(defn word [] \"{word}\")\n(defn main [& args]\n  (loop [] (println (word)) (sleep 100) (recur)))\n")
    };
    write(&main, &body("first"));
    let mut p = norn().arg("dev").current_dir(&proj).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut out = p.stdout.take().unwrap();
    let mut seen = String::new();
    assert!(read_until(&mut out, &mut seen, "first\n"), "it runs main:\n{seen}");
    write(&main, &body("second"));
    assert!(read_until(&mut out, &mut seen, "reloaded live.main"), "{seen}");
    assert!(read_until(&mut out, &mut seen, "second\n"), "the running loop calls the new word:\n{seen}");
    // a save that doesn't read keeps the old code running
    write(&main, "(ns live.main)\n(defn word [] \"third\"\n");
    assert!(read_until(&mut out, &mut seen, "wasn't reloaded"), "{seen}");
    let after = seen.len();
    assert!(read_until(&mut out, &mut seen, "second\n") && !seen[after..].contains("third"));
    let _ = p.kill();
    let _ = p.wait();
    std::fs::remove_dir_all(&dir).unwrap();
}
