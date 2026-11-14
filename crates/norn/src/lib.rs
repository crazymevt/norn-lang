//! `norn` — the REPL and script runner.
//!
//! ```text
//! norn                 start a REPL
//! norn FILE.norn       load a file
//! norn -e EXPR         evaluate EXPR and print the result
//! norn --server [PORT]  REPL plus the editor protocol on 127.0.0.1:PORT (default 7888)
//! norn --image IMG …   start from a saved image instead of booting from source;
//!                      an image with an entry function runs it with the
//!                      remaining arguments (unless they are -e EXPR)
//! norn build OUT.nimg [FILE.norn…] [--entry NS/NAME] [--image BASE.nimg [--layered]]
//!                      load the files into a fresh runtime and save an image
//! ```
//!
//! This thread never touches the Norn heap: it hands source text to a root
//! fiber on the scheduler and waits for the outcome.
//!
//! As a library, it lets a program that embeds native extensions (norn-ui)
//! offer the same command line: `run()` is the whole CLI, and
//! `run_with_main_thread(f)` runs it on a thread of its own and gives the
//! process's first thread to `f` (macOS wants the UI event loop there).

use norn_vm::{RootTask, start_with};
use std::io::{self, BufRead, Write};
use std::process::exit;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

const USAGE: &str = "usage: norn [--image IMG.nimg] [FILE.norn | -e EXPR | --server [PORT] | ARGS…]
       norn build OUT.nimg [FILE.norn…] [--entry NS/NAME] [--image BASE.nimg [--layered]]
       norn test [--only REGEX] [--serial] [--tap] [--junit FILE] [PATH…]
       norn fmt [--check] [PATH…]
       norn lint [PATH…]
       norn check [PATH…]
       norn doc NAMESPACE | NAME | NS/NAME …
       norn image info IMG.nimg | norn image diff A.nimg B.nimg";

/// Ticket of the root fiber the REPL is waiting on (0 = none), for Ctrl-C.
static CURRENT: AtomicU64 = AtomicU64::new(0);

/// Run `f` on the process's first thread once the runtime has started, and
/// the command line (`run`) on another thread. The process ends when either
/// finishes: the command line's outcome, or `f`'s return value as the exit
/// status. If no runtime starts (`--help`, a bad image), `f` never runs.
pub fn run_with_main_thread(f: impl FnOnce(&'static norn_vm::Norn) -> i32) -> ! {
    std::thread::Builder::new()
        .name("norn-cli".into())
        .spawn(|| {
            run();
            let _ = io::stdout().flush();
            exit(0)
        })
        .expect("failed to start the command-line thread");
    match norn_vm::wait_started() {
        Some(norn) => {
            let code = f(norn);
            let _ = io::stdout().flush();
            exit(code)
        }
        // The command-line thread reports the failure and exits.
        None => loop {
            std::thread::park();
        },
    }
}

/// The `norn` command line.
pub fn run() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("build") {
        exit(build(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("image") {
        exit(image_tool(&args[1..]));
    }
    let image = match args.as_slice() {
        [flag, path, ..] if flag == "--image" => {
            let p = std::path::PathBuf::from(path);
            args.drain(..2);
            Some(p)
        }
        _ => None,
    };
    let norn = start_with(image.as_deref()).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1)
    });
    let _ = ctrlc::set_handler(move || {
        let t = CURRENT.load(Ordering::Acquire);
        if t != 0 {
            norn.interrupt(t);
        }
    });

    if image.is_some()
        && args.first().map(String::as_str) != Some("-e")
        && eval_raw(norn, "(if norn.core/%image-entry true false)", "user") == "true"
    {
        let call = format!("(norn.core/%run-entry {})", args.iter().map(|a| norn_string(a)).collect::<Vec<_>>().join(" "));
        let t = norn.submit(RootTask::Eval(call), "user");
        CURRENT.store(t, Ordering::Release);
        let done = norn.wait(t);
        let _ = io::stdout().flush();
        exit(match done.result {
            // An integer result is the exit status.
            Ok(v) => v.parse::<i64>().map(|n| n.clamp(0, 255) as i32).unwrap_or(0),
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        });
    }

    if let Some(tool @ ("test" | "fmt" | "lint" | "doc" | "check")) = args.first().map(String::as_str) {
        // `norn test …`, `norn fmt …`: the library's main returns the exit status.
        let call = format!(
            "(do (require 'norn.{tool}) (norn.{tool}/main {}))",
            args[1..].iter().map(|a| norn_string(a)).collect::<Vec<_>>().join(" ")
        );
        let t = norn.submit(RootTask::Eval(call), "user");
        CURRENT.store(t, Ordering::Release);
        let done = norn.wait(t);
        let _ = io::stdout().flush();
        exit(match done.result {
            Ok(v) => v.parse::<i64>().map(|n| n.clamp(0, 255) as i32).unwrap_or(1),
            Err(e) => {
                eprintln!("Error: {e}");
                1
            }
        });
    }

    let done = match args.as_slice() {
        [] => {
            repl(norn);
            return;
        }
        [flag] if flag == "-h" || flag == "--help" => {
            println!("{USAGE}");
            return;
        }
        [flag, expr] if flag == "-e" => run_task(norn, RootTask::Eval(expr.clone()), true),
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
        // A script's own arguments follow it; (norn.sys/args) has them.
        [file, ..] if !file.starts_with('-') => run_task(norn, RootTask::Load(file.clone()), false),
        _ => {
            eprintln!("{USAGE}");
            exit(2)
        }
    };
    exit(if done { 0 } else { 1 })
}

/// Run `norn --image img -e code` as a child process; its stdout's first
/// line (the code prints what it wants and the value line follows).
fn in_image(img: &str, code: &str) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = std::process::Command::new(exe).args(["--image", img, "-e", code]).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("{img}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `norn image info IMG` and `norn image diff A B`.
fn image_tool(args: &[String]) -> i32 {
    let result = match args {
        [op, img] if op == "info" => in_image(img, "(do (require 'norn.image) (print (norn.image/info-text)) nil)")
            .map(|out| out.strip_suffix("nil\n").unwrap_or(&out).to_owned()),
        [op, a, b] if op == "diff" => {
            let summary = "(do (require 'norn.image) (prn (norn.image/summary)) nil)";
            match (in_image(a, summary), in_image(b, summary)) {
                (Ok(sa), Ok(sb)) => {
                    let first = |s: &str| s.lines().next().unwrap_or("{}").to_owned();
                    let norn = start_with(None).unwrap_or_else(|e| {
                        eprintln!("{e}");
                        exit(1)
                    });
                    let code = format!(
                        "(do (require 'norn.image) (norn.image/diff-text (read-string {}) (read-string {})))",
                        norn_string(&first(&sa)),
                        norn_string(&first(&sb))
                    );
                    match norn.run(RootTask::Eval(code), "user").result {
                        Ok(v) => Ok(v.trim_matches('"').replace("\\n", "\n")),
                        Err(e) => Err(e.to_string()),
                    }
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            }
        }
        _ => Err("usage: norn image info IMG.nimg | norn image diff A.nimg B.nimg".into()),
    };
    match result {
        Ok(text) => {
            print!("{text}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `s` as a Norn string literal.
fn norn_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

/// `norn build OUT.nimg [FILE…] [--entry NS/NAME] [--image BASE.nimg [--layered]]`.
/// Saves without fibers, so the same sources give the same image.
fn build(args: &[String]) -> i32 {
    let (mut out, mut files, mut entry, mut base, mut layered) = (None, Vec::new(), None, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--entry" => entry = it.next().cloned(),
            "--image" => base = it.next().map(std::path::PathBuf::from),
            "--layered" => layered = true,
            _ if out.is_none() => out = Some(a.clone()),
            _ => files.push(a.clone()),
        }
    }
    let Some(out) = out else {
        eprintln!("{USAGE}");
        return 2;
    };
    let norn = start_with(base.as_deref()).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1)
    });
    for f in files {
        if !run_task(norn, RootTask::Load(f), false) {
            return 1;
        }
    }
    let entry = match entry {
        Some(e) => format!("(var {e})"),
        None => "nil".into(),
    };
    let save = format!(
        "(let [r (save-image {} :entry {entry} :fibers :none :layered {layered})] (str (:objects r) \" objects, \" (:bytes r) \" bytes\"))",
        norn_string(&out)
    );
    match norn.run(RootTask::EvalRaw(save), "user").result {
        Ok(s) => {
            println!("{out}: {s}");
            0
        }
        Err(e) => {
            eprintln!("Error: {e}");
            1
        }
    }
}

fn run_task(norn: &norn_vm::Norn, task: RootTask, print: bool) -> bool {
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
        "Norn {} (fibers on {} carriers)\nCtrl-C interrupts a running evaluation; Ctrl-D exits.",
        env!("CARGO_PKG_VERSION"),
        norn.carriers()
    );
    // Started from an image: say which, and who is still in the debugger.
    let info = eval_raw(norn, "(when-let [i (image-info)] (str \"Image: \" (:image i)))", "user");
    if info != "nil" {
        println!("{info}");
        let waiting = eval_raw(norn, "(apply str (map (fn [d] (%debugger-banner (:fiber d))) (debugging)))", "user");
        print!("{waiting}");
    }
    let events = norn_vm::subscribe();
    let mut ns = "user".to_owned();
    let mut line_editor = LineEditor::new(norn);
    loop {
        let src = match line_editor.as_mut() {
            Some(ed) => ed.read(&ns),
            None => read_form(&format!("{ns}=> "), ns.len()),
        };
        let Some(src) = src else { return };
        let t = norn.submit(RootTask::Repl(src), &ns);
        let done = wait_root(norn, &events, t, &ns);
        if let Err(e) = done.result {
            eprintln!("Error: {e}");
        }
        ns = done.ns;
    }
}

/// The terminal line editor (rustyline): history, multi-line input and
/// completion of names. Only when stdin is a terminal.
struct LineEditor {
    ed: rustyline::Editor<NornHelper, rustyline::history::DefaultHistory>,
    history: Option<std::path::PathBuf>,
}

struct NornHelper {
    norn: &'static norn_vm::Norn,
    ns: std::cell::RefCell<String>,
}

impl rustyline::completion::Completer for NornHelper {
    type Candidate = String;
    fn complete(&self, line: &str, pos: usize, _: &rustyline::Context<'_>) -> rustyline::Result<(usize, Vec<String>)> {
        let start = line[..pos].rfind(|c: char| c.is_whitespace() || "()[]{}'`~@\"".contains(c)).map_or(0, |i| i + 1);
        let prefix = &line[start..pos];
        if prefix.is_empty() {
            return Ok((start, Vec::new()));
        }
        let ns = self.ns.borrow().clone();
        let names = eval_raw(
            self.norn,
            &format!("(apply str (interpose \" \" (norn.core/%complete {} {})))", norn_string(prefix), norn_string(&ns)),
            "user",
        );
        let names = names.trim_matches('"');
        Ok((start, names.split_whitespace().map(str::to_owned).collect()))
    }
}

impl rustyline::validate::Validator for NornHelper {
    fn validate(&self, ctx: &mut rustyline::validate::ValidationContext) -> rustyline::Result<rustyline::validate::ValidationResult> {
        Ok(if needs_more(ctx.input()) {
            rustyline::validate::ValidationResult::Incomplete
        } else {
            rustyline::validate::ValidationResult::Valid(None)
        })
    }
}

impl rustyline::hint::Hinter for NornHelper {
    type Hint = String;
}
impl rustyline::highlight::Highlighter for NornHelper {}
impl rustyline::Helper for NornHelper {}

impl LineEditor {
    fn new(norn: &'static norn_vm::Norn) -> Option<LineEditor> {
        use std::io::IsTerminal;
        if !io::stdin().is_terminal() {
            return None;
        }
        let mut ed = rustyline::Editor::new().ok()?;
        ed.set_helper(Some(NornHelper { norn, ns: std::cell::RefCell::new("user".into()) }));
        let history = std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".norn_history"));
        if let Some(h) = &history {
            let _ = ed.load_history(h);
        }
        Some(LineEditor { ed, history })
    }

    /// One complete input, or None at end of input (Ctrl-D).
    fn read(&mut self, ns: &str) -> Option<String> {
        if let Some(h) = self.ed.helper() {
            *h.ns.borrow_mut() = ns.to_owned();
        }
        loop {
            match self.ed.readline(&format!("{ns}=> ")) {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => {
                    let _ = self.ed.add_history_entry(line.as_str());
                    if let Some(h) = &self.history {
                        let _ = self.ed.save_history(h);
                    }
                    return Some(line);
                }
                Err(rustyline::error::ReadlineError::Interrupted) => continue,
                Err(_) => {
                    println!();
                    return None;
                }
            }
        }
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
