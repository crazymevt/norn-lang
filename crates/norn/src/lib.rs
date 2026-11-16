//! `norn` — the REPL and script runner.
//!
//! ```text
//! norn                 start a REPL
//! norn FILE.norn       load a file
//! norn -e EXPR         evaluate EXPR and print the result
//! norn --server [PORT]  REPL plus the editor protocol on 127.0.0.1:PORT (default 7888)
//! norn --image IMG …   start from a saved image instead of booting from source;
//!                      an image with an entry function runs it with every
//!                      remaining argument (-e too)
//! norn -e EXPR --image IMG
//!                      evaluate EXPR in the image, entry function or not
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
//! The `norn` command passes `norn_vm::main_lane::serve`, so Norn code can
//! make `:main-thread` foreign calls on that thread.

use norn_vm::{RootTask, start_with};
use std::io::{self, BufRead, Write};
use std::process::exit;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

const USAGE: &str = "usage: norn [--image IMG.nimg] [FILE.norn | -e EXPR | --server [PORT] | ARGS…]
       norn -e EXPR --image IMG.nimg   (EXPR in the image, though it has an entry)
       norn build OUT.nimg [FILE.norn…] [--entry NS/NAME] [--image BASE.nimg [--layered]]
                  (in a project, with no files: its :main)
       norn test [--only REGEX] [--serial] [--tap] [--junit FILE] [PATH…]
       norn fmt [--check] [PATH…]
       norn lint [PATH…]
       norn check [PATH…]
       norn expand [--steps] FILE.norn | -e EXPR
       norn deps [--update]
       norn package [--cli] [--app] [--out DIR]
                  (in a project: a release archive of it, with this norn)
       norn doc NAMESPACE | NAME | NS/NAME …
       norn image info IMG.nimg | norn image diff A.nimg B.nimg";

/// Ticket of the root fiber the REPL is waiting on (0 = none), for Ctrl-C.
static CURRENT: AtomicU64 = AtomicU64::new(0);

/// Set when the REPL's input ends at a debugger prompt: the evaluation is
/// aborted and the REPL ends once it has.
static INPUT_ENDED: AtomicBool = AtomicBool::new(false);

/// Set by Ctrl-C (the debugger prompt then checks its fiber is still there).
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Run `f` on the process's first thread once the runtime has started, and
/// the command line (`run`) on another thread. The process ends when either
/// finishes: the command line's outcome, or `f`'s return value as the exit
/// status. If no runtime starts (`--help`, a bad image), `f` never runs.
pub fn run_with_main_thread(f: impl FnOnce(&'static norn_vm::Norn) -> i32) -> ! {
    prepare_app();
    std::thread::Builder::new()
        .name("norn-cli".into())
        // (as much stack as a process's first thread usually gets)
        .stack_size(8 << 20)
        .spawn(|| {
            // A panic here (printing to a closed pipe, say, when the
            // process that started this one is gone) ends the process:
            // otherwise the first thread, serving main-thread calls, would
            // keep it alive with nothing left to do.
            let ok = std::panic::catch_unwind(run).is_ok();
            let _ = io::stdout().flush();
            exit(if ok { 0 } else { 101 })
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

// ---- packaged apps ----

/// A packaged app (`norn package`): this executable, copied under the
/// app's name, finds its image beside it, in the macOS bundle's
/// `Contents/Resources/NAME.nimg` or a command-line archive's
/// `lib/NAME/NAME.nimg`, and runs it: every argument goes to the image's
/// entry function, none is read as a `norn` flag.
struct App {
    name: String,
    image: std::path::PathBuf,
    /// The folder holding the image and the app's resources.
    resources: std::path::PathBuf,
    /// Where native helpers (norn-ui-host) are looked for.
    helper_dirs: Vec<std::path::PathBuf>,
}

static APP: std::sync::OnceLock<Option<App>> = std::sync::OnceLock::new();

fn find_app() -> Option<App> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let name = exe.file_name()?.to_str()?.to_owned();
    if name == "norn" {
        return None;
    }
    let dir = exe.parent()?.to_path_buf();
    let image = [dir.join("../Resources").join(format!("{name}.nimg")), dir.join("../lib").join(&name).join(format!("{name}.nimg"))]
        .into_iter()
        .find(|p| p.is_file())?
        .canonicalize()
        .ok()?;
    let resources = image.parent()?.to_path_buf();
    Some(App { name, image, helper_dirs: vec![dir, resources.clone()], resources })
}

/// Find out whether this is a packaged app, before the process starts any
/// other thread: if so, tell its code where its resources are
/// (`NORN_APP_RESOURCES`, `norn.sys/app-resources`) and point `NORN_UI_HOST`
/// at the bundled renderer (unless it's set).
fn prepare_app() {
    let app = APP.get_or_init(find_app);
    if let Some(a) = app {
        let host = a.helper_dirs.iter().map(|d| d.join("norn-ui-host")).find(|p| p.is_file());
        // SAFETY: no other thread exists yet (run_with_main_thread calls
        // this first), so nothing reads the environment meanwhile.
        unsafe {
            std::env::set_var("NORN_APP_RESOURCES", &a.resources);
            if let Some(h) = host
                && std::env::var_os("NORN_UI_HOST").is_none()
            {
                std::env::set_var("NORN_UI_HOST", h);
            }
        }
    }
}

/// Run a packaged app: its image's entry with every argument (macOS's
/// `-psn_…` process serial number aside), in the caller's directory.
fn run_app(app: &App) -> ! {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("-psn_")).collect();
    norn_vm::sys::set_program_args(args.clone());
    let norn = start_with(Some(&app.image)).unwrap_or_else(|e| {
        eprintln!("{}: {e}", app.name);
        exit(1)
    });
    let _ = ctrlc::set_handler(move || {
        let t = CURRENT.load(Ordering::Acquire);
        if t != 0 {
            norn.interrupt(t);
        }
    });
    if eval_raw(norn, "(if norn.core/%image-entry true false)", "user") != "true" {
        eprintln!("{}: its image has no entry function", app.name);
        exit(1)
    }
    run_entry(norn, &args)
}

/// Call the image's entry function with args, and exit with its status.
fn run_entry(norn: &'static norn_vm::Norn, args: &[String]) -> ! {
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

/// The `norn` command line.
pub fn run() {
    if let Some(app) = APP.get_or_init(find_app) {
        run_app(app);
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("build") {
        exit(build(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("image") {
        exit(image_tool(&args[1..]));
    }
    // `norn --image IMG ARGS…`: an image with an entry function gets every
    // argument after it, -e too. `norn -e EXPR --image IMG` evaluates EXPR
    // in the image instead, entry or not.
    let (image, eval_in_image) = match args.as_slice() {
        [flag, path, ..] if flag == "--image" => {
            let p = std::path::PathBuf::from(path);
            args.drain(..2);
            (Some(p), false)
        }
        [e, _, flag, path] if e == "-e" && flag == "--image" => {
            let p = std::path::PathBuf::from(path);
            args.truncate(2);
            (Some(p), true)
        }
        _ => (None, false),
    };
    // (norn.sys/program-args): an image's entry gets everything after the
    // image; a script or tool, everything after its name; -e and the REPL,
    // nothing. (Whether -e after an image goes to an entry is known once the
    // image is loaded: then they're set.)
    match args.first() {
        Some(f) if image.is_some() && f == "-e" => {}
        Some(_) if image.is_some() => norn_vm::sys::set_program_args(args.clone()),
        Some(f) if !f.starts_with('-') => norn_vm::sys::set_program_args(args[1..].to_vec()),
        _ => norn_vm::sys::set_program_args(Vec::new()),
    }
    let norn = start_with(image.as_deref()).unwrap_or_else(|e| {
        eprintln!("{e}");
        exit(1)
    });
    let _ = ctrlc::set_handler(move || {
        INTERRUPTED.store(true, Ordering::Release);
        let t = CURRENT.load(Ordering::Acquire);
        if t != 0 {
            norn.interrupt(t);
        }
    });

    if image.is_some() && !eval_in_image && eval_raw(norn, "(if norn.core/%image-entry true false)", "user") == "true" {
        norn_vm::sys::set_program_args(args.clone());
        run_entry(norn, &args);
    }

    // The project this run belongs to (M8 part 8.10): its paths and its
    // packages' go on the library path. (norn deps resolves it itself.)
    if args.first().map(String::as_str) != Some("deps") {
        let start = match args.first() {
            Some(f) if !f.starts_with('-') && f.ends_with(".norn") => {
                std::path::Path::new(f).parent().map(std::path::Path::to_path_buf).unwrap_or_default()
            }
            _ => std::path::PathBuf::from("."),
        };
        if let Some(dir) = find_project(&start)
            && !use_project(norn, &dir)
            && !args.is_empty()
        {
            exit(1);
        }
    }

    if let Some(tool @ ("test" | "fmt" | "lint" | "doc" | "check" | "expand" | "deps" | "package")) = args.first().map(String::as_str) {
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

/// Run `norn -e code --image img` as a child process (code runs in the
/// image even when it has an entry function); its stdout (the code prints
/// what it wants and the value line follows).
fn in_image(img: &str, code: &str) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = std::process::Command::new(exe).args(["-e", code, "--image", img]).output().map_err(|e| e.to_string())?;
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
    // Same sources, same image: everything is compiled here, not taken
    // from the module cache (the image is the cache for its start-up). No
    // macros in this form: the first macro to expand loads norn.validate,
    // which must not come from (or go to) the cache either.
    norn.run(RootTask::EvalRaw("(let* [v (resolve 'norn.expander/set-cache-dir!)] (if v (v nil)))".into()), "user");
    // In a project, its packages are on the library path, and with no files
    // its :main is the entry, its namespace loaded.
    let start = files.first().and_then(|f| std::path::Path::new(f).parent().map(std::path::Path::to_path_buf)).unwrap_or_default();
    if let Some(dir) = find_project(&start) {
        if !use_project(norn, &dir) {
            return 1;
        }
        if files.is_empty() && entry.is_none() {
            let code = format!("(norn.deps/load-main! {})", norn_string(&dir.to_string_lossy()));
            match norn.run(RootTask::Eval(code), "user").result {
                Ok(v) if v != "nil" => entry = Some(v.trim_matches('"').to_owned()),
                Ok(_) => {}
                Err(e) => {
                    eprintln!("Error: {e}");
                    return 1;
                }
            }
        }
    }
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
        announce_debugging(norn, &events, &ns);
        let src = match line_editor.as_mut() {
            Some(ed) => ed.read(&ns),
            None => read_form(&format!("{ns}=> "), ns.len()),
        };
        let Some(src) = src else { return };
        if src.trim().is_empty() {
            continue;
        }
        let t = norn.submit(RootTask::Repl(src), &ns);
        let done = wait_root(norn, &events, t, &ns);
        if let Err(e) = done.result {
            eprintln!("Error: {e}");
        }
        ns = done.ns;
        if INPUT_ENDED.load(Ordering::Acquire) {
            return;
        }
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

/// Read lines until they form complete input (or a blank line). None at
/// end of input.
fn read_form(prompt: &str, indent: usize) -> Option<String> {
    let stdin = io::stdin();
    let mut buf = String::new();
    // (stdout may be a pipe nobody reads any more: writes that fail are
    // ignored, and the end of stdin ends the REPL)
    let mut out = io::stdout();
    loop {
        if buf.is_empty() {
            let _ = write!(out, "{prompt}");
        } else {
            let _ = write!(out, "{}.. ", " ".repeat(indent));
        }
        let _ = out.flush();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => {
                let _ = writeln!(out);
                return None;
            }
            Ok(_) => buf.push_str(&line),
        }
        if !needs_more(&buf) {
            // (blank input too: the caller may have something to check)
            return Some(buf);
        }
    }
}

/// The project (M8 part 8.10) a run in folder `start` belongs to: the
/// nearest folder with a norn.edn, from `start` up.
fn find_project(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let start = if start.as_os_str().is_empty() { std::path::Path::new(".") } else { start };
    let mut d = std::fs::canonicalize(start).ok()?;
    loop {
        if d.join("norn.edn").is_file() {
            return Some(d);
        }
        if !d.pop() {
            return None;
        }
    }
}

/// Use the project in `dir`: its packages resolved (and fetched), its paths
/// and theirs on the library path. False, having said why, if it can't be.
fn use_project(norn: &norn_vm::Norn, dir: &std::path::Path) -> bool {
    let code = format!("(do (require 'norn.deps) (norn.deps/use-project! {}) nil)", norn_string(&dir.to_string_lossy()));
    match norn.run(RootTask::Eval(code), "user").result {
        Ok(_) => true,
        Err(e) => {
            eprintln!("norn: the project in {}: {e}", dir.display());
            false
        }
    }
}

fn eval_raw(norn: &norn_vm::Norn, src: &str, ns: &str) -> String {
    match norn.run(RootTask::EvalRaw(src.to_owned()), ns).result {
        Ok(s) => s,
        Err(e) => format!("Error: {e}"),
    }
}

/// Wait for root fiber `t`, handling the debugger meanwhile: if `t`, or
/// another fiber (one `t` spawned, often, which it may be waiting for),
/// waits in the debugger, show it and take commands.
fn wait_root(norn: &norn_vm::Norn, events: &mpsc::Receiver<String>, t: u64, ns: &str) -> norn_vm::RootDone {
    CURRENT.store(t, Ordering::Release);
    loop {
        if let Some(done) = norn.wait_timeout(t, Duration::from_millis(30)) {
            CURRENT.store(0, Ordering::Release);
            let _ = io::stdout().flush();
            return done;
        }
        while let Ok(ev) = events.try_recv() {
            let Some(id) = note_event(&ev) else { continue };
            // (None: it isn't waiting any more)
            let Some(banner) = banner(norn, id, ns) else { continue };
            print!("{banner}");
            if id != t {
                println!("  (the evaluation goes on meanwhile)");
            }
            debugger(norn, events, id, ns);
            if INPUT_ENDED.load(Ordering::Acquire) && id != t {
                norn.interrupt(t);
            }
        }
    }
}

/// Before the next input: say which fibers that entered the debugger since
/// the last evaluation are still waiting there (they keep waiting).
fn announce_debugging(norn: &norn_vm::Norn, events: &mpsc::Receiver<String>, ns: &str) {
    while let Ok(ev) = events.try_recv() {
        let Some(id) = note_event(&ev) else { continue };
        if let Some(banner) = banner(norn, id, ns) {
            print!("{banner}");
            println!("  (it keeps waiting; use (debug-restart {id} n), (debug-restart-frame {id} i) or (debug-abort {id}))");
        }
    }
}

/// Fiber `id`'s debugger banner, or None if it isn't in the debugger.
fn banner(norn: &norn_vm::Norn, id: u64, ns: &str) -> Option<String> {
    let b = eval_raw(norn, &format!("(%debugger-banner {id})"), ns);
    if b.starts_with("Error") { None } else { Some(b) }
}

/// The fibers that have left the debugger (`:resumed` events), as far as
/// the REPL has seen.
static RESUMED: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

/// Note an event line: the fiber of a `{:event :debug :fiber N …}`, which
/// has entered the debugger. (A `:resumed` one goes in RESUMED.)
fn note_event(line: &str) -> Option<u64> {
    let fiber = || -> Option<u64> {
        let rest = &line[line.find(":fiber ")? + 7..];
        rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    };
    let mut resumed = RESUMED.lock().unwrap_or_else(|e| e.into_inner());
    if line.contains(":event :resumed") {
        resumed.extend(fiber());
        None
    } else if line.contains(":event :debug") {
        let id = fiber()?;
        resumed.retain(|&r| r != id);
        Some(id)
    } else {
        None
    }
}

/// Has fiber `id` left the debugger, as the events say?
fn resumed(id: u64) -> bool {
    RESUMED.lock().unwrap_or_else(|e| e.into_inner()).contains(&id)
}

/// Is fiber `id` still waiting in the debugger? (Asks the runtime: a fiber
/// cancelled while it waited is noticed only when someone asks.)
fn parked(norn: &norn_vm::Norn, id: u64) -> bool {
    eval_raw(norn, &format!("(norn.core/%in-debugger? {id})"), "user") == "true"
}

/// The debugger prompt for fiber `id`, which waits in the debugger: the
/// evaluation the REPL is waiting on, or another fiber.
fn debugger(norn: &norn_vm::Norn, events: &mpsc::Receiver<String>, id: u64, ns: &str) {
    println!("  :r N [args…]  invoke restart N    :f N  restart frame N (newest definition)");
    println!("  :bt  frames again    :a  abort    anything else is evaluated (e.g. redefine a function)");
    let current = CURRENT.load(Ordering::Acquire);
    // (the end of input leaves the debugger: the fiber is aborted, and the
    // REPL ends once the evaluation has)
    let end = || {
        INPUT_ENDED.store(true, Ordering::Release);
        let _ = eval_raw(norn, &format!("(debug-abort {id})"), ns);
    };
    loop {
        if INPUT_ENDED.load(Ordering::Acquire) {
            return end();
        }
        // (resumed by something evaluated here, as (debug-abort id) does, or
        // cancelled by Ctrl-C, with the evaluation and its scope)
        if resumed(id) || (INTERRUPTED.swap(false, Ordering::AcqRel) && !parked(norn, id)) {
            println!("  (fiber {id} is no longer in the debugger)");
            return;
        }
        let prompt = format!("debug[{id}]> ");
        let Some(line) = read_form(&prompt, prompt.len() - 3) else { return end() };
        let cmd = line.trim();
        if cmd.is_empty() {
            continue;
        }
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
                CURRENT.store(current, Ordering::Release);
                // (what it caused: a :resumed event for this fiber, say)
                announce_debugging(norn, events, ns);
                continue;
            }
        };
        let reply = eval_raw(norn, &code, ns);
        if reply.starts_with("Error") {
            println!("{reply}");
            if parked(norn, id) {
                continue;
            }
            return;
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
