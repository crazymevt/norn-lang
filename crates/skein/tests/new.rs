//! `skein new`, `skein run` and `skein dev`: a new project runs, tests, lints
//! and packages as it is; run calls its :main; dev reloads what's saved
//! while it runs.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn skein() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_skein"));
    c.env_remove("SKEIN_PATH");
    c
}

fn temp(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("skein-new-{name}-{nanos}"));
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
    let (code, text) = run(skein().args(["new", "my-app", "--no-git"]).current_dir(&dir));
    assert_eq!(0, code, "{text}");
    let proj = dir.join("my-app");
    for f in ["skein.edn", "src/my-app/main.skn", "tests/main_test.skn", "README.md", ".gitignore"] {
        assert!(proj.join(f).is_file(), "{f} is made");
    }
    assert!(!proj.join(".git").exists(), "--no-git");
    let (code, text) = run(skein().args(["run", "Ada", "Grace"]).current_dir(&proj));
    assert_eq!((0, "Hello, Ada and Grace!\n".to_string()), (code, text));
    let (code, text) = run(skein().arg("test").current_dir(&proj));
    assert_eq!(0, code, "its test passes:\n{text}");
    let (code, text) = run(skein().args(["fmt", "--check", "src", "tests"]).current_dir(&proj));
    assert_eq!(0, code, "formatted:\n{text}");
    let (code, text) = run(skein().args(["lint", "src", "tests"]).current_dir(&proj));
    assert_eq!((0, String::new()), (code, text), "lint-clean");
    let (code, text) = run(skein().args(["package", "--cli"]).current_dir(&proj));
    assert_eq!(0, code, "{text}");
    let os = if cfg!(target_os = "macos") { "macos" } else { "linux" };
    assert!(proj.join(format!("dist/my-app-0.1.0-{os}-{}.tar.gz", std::env::consts::ARCH)).is_file());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_new_library_tests_and_is_used_as_a_package() {
    let dir = temp("lib");
    let (code, text) = run(skein().args(["new", "text-kit", "--lib", "--no-git"]).current_dir(&dir));
    assert_eq!(0, code, "{text}");
    let lib = dir.join("text-kit");
    for f in ["skein.edn", "src/text-kit.skn", "tests/text_kit_test.skn", "README.md", ".gitignore"] {
        assert!(lib.join(f).is_file(), "{f} is made");
    }
    assert!(!std::fs::read_to_string(lib.join("skein.edn")).unwrap().contains(":main"), "no :main");
    let (code, text) = run(skein().arg("test").current_dir(&lib));
    assert_eq!(0, code, "its test passes:\n{text}");
    let (code, text) = run(skein().args(["lint", "src", "tests"]).current_dir(&lib));
    assert_eq!((0, String::new()), (code, text), "lint-clean");
    // an app that uses it
    run(skein().args(["new", "user", "--no-git"]).current_dir(&dir));
    let app = dir.join("user");
    std::fs::write(app.join("skein.edn"), "{:name user :main user.main/main :deps {text-kit {:path \"../text-kit\"}}}\n").unwrap();
    std::fs::write(app.join("src/user/main.skn"), "(ns user.main (:require [text-kit :as k]))\n(defn main [& args] (println (k/greeting \"Ada\")) 0)\n").unwrap();
    assert_eq!((0, "Hello, Ada!\n".to_string()), run(skein().arg("run").current_dir(&app)));
    let (code, text) = run(skein().args(["new", "both", "--lib", "--app", "--no-git"]).current_dir(&dir));
    assert!(code != 0 && text.contains("don't go together"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn run_gives_mains_integer_as_the_exit_status() {
    let dir = temp("status");
    run(skein().args(["new", "four", "--no-git"]).current_dir(&dir));
    let proj = dir.join("four");
    std::fs::write(proj.join("src/four/main.skn"), "(ns four.main)\n(defn main [& args] (+ 2 (count args)))\n").unwrap();
    assert_eq!(4, run(skein().args(["run", "a", "b"]).current_dir(&proj)).0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn new_makes_a_git_repository_unless_its_in_one() {
    if Command::new("git").arg("--version").output().is_err() {
        return;
    }
    let dir = temp("git");
    run(skein().args(["new", "repo-app"]).current_dir(&dir));
    assert!(dir.join("repo-app/.git").is_dir());
    // (inside that repository, the next one isn't a repository of its own)
    run(skein().args(["new", "inner"]).current_dir(dir.join("repo-app")));
    assert!(dir.join("repo-app/inner/skein.edn").is_file() && !dir.join("repo-app/inner/.git").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn what_new_and_run_refuse() {
    let dir = temp("refuse");
    let (code, text) = run(skein().args(["new", "My_App"]).current_dir(&dir));
    assert_eq!(1, code);
    assert!(text.contains("lower-case letters and digits"), "{text}");
    std::fs::create_dir_all(dir.join("taken")).unwrap();
    std::fs::write(dir.join("taken/notes.txt"), "mine").unwrap();
    let (code, text) = run(skein().args(["new", "taken", "--no-git"]).current_dir(&dir));
    assert_eq!(1, code);
    assert!(text.contains("isn't an empty folder"), "{text}");
    let (code, text) = run(skein().arg("run").current_dir(&dir));
    assert_eq!(2, code, "no project: {text}");
    assert!(text.contains("no skein.edn"), "{text}");
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
    run(skein().args(["new", "live", "--no-git"]).current_dir(&dir));
    let proj = dir.join("live");
    let main = proj.join("src/live/main.skn");
    let body = |word: &str| {
        format!("(ns live.main)\n(defn word [] \"{word}\")\n(defn main [& args]\n  (loop [] (println (word)) (sleep 100) (recur)))\n")
    };
    write(&main, &body("first"));
    let mut p = skein().arg("dev").current_dir(&proj).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
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
    // Ctrl-C stops it, cancelling a main that doesn't listen for it
    Command::new("kill").args(["-INT", &p.id().to_string()]).status().unwrap();
    assert!(read_until(&mut out, &mut seen, "skein dev: stopped\n"), "{seen}");
    assert_eq!(Some(0), p.wait().unwrap().code());
    std::fs::remove_dir_all(&dir).unwrap();
}
