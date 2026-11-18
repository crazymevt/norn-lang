//! M5 done-check: images. A saved fiber resumes after load (parked on a
//! channel, sleeping, preempted mid-loop, and crashed into the debugger), the
//! test suites pass in a runtime started from an image, layers and `skein
//! build` work, damaged images are refused, and start-up is under 50 ms
//! (checked in release builds; debug builds are several times slower).
//!
//! Every test drives the real `skein` binary, since an image is loaded at
//! process start.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Instant;

/// The image tests run in parallel threads; the start-up timing runs alone
/// (others hold this shared, it holds it exclusively), so it measures
/// start-up rather than how busy the machine is.
static ALONE: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn skein() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_skein"));
    c.current_dir(format!("{}/../..", env!("CARGO_MANIFEST_DIR")));
    c
}

/// A fresh directory for one test's images.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skein-images-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn ok(o: Output) -> String {
    assert!(o.status.success(), "skein failed:\n{}", text(&o));
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

fn eval(image: Option<&PathBuf>, expr: &str) -> String {
    let mut c = skein();
    if let Some(i) = image {
        c.arg("--image").arg(i);
    }
    ok(c.arg("-e").arg(expr).output().unwrap())
}

#[test]
fn saved_fibers_resume_after_load() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("fibers");
    let img = d.join("fibers.skimg");
    // A worker that crashes into the debugger, a channel reader, a sleeper, a
    // timeout channel, and busy fibers preempted mid-loop. (Busy for most
    // of a second: at 100,000 turns each they were done in 30-60 ms, and
    // on a fast machine before the image was saved.)
    let setup = format!(
        r#"(do
  (defn process [x] (/ 100 x))
  (def results (atom []))
  (def worker (binding [*debugger* :park]
                (spawn-detached (doseq [x [5 0 4]] (swap! results conj (process x))) :done)))
  (def ch (chan))
  (def reader (spawn-detached (str "got " (<! ch))))
  (def sleeper (spawn-detached (sleep 300) :slept))
  (def t (timeout 200))
  (def counter (atom 0))
  (def busy (mapv (fn [i] (spawn-detached (dotimes [_ 2000000] (swap! counter inc)) i)) (range 4)))
  (sleep 30)
  (save-image "{}")
  [(count (debugging)) (< @counter 8000000)])"#,
        img.display()
    );
    assert_eq!(eval(None, &setup), "[1 true]");

    let after = r#"(do
  (defn process [x] (if (zero? x) 0 (/ 100 x)))
  (let [id (:fiber (first (debugging)))
        fs (backtrace id)
        i (some (fn [i] (when (= 'user/process (:fn (nth fs i))) i)) (range (count fs)))]
    (debug-restart-frame id i))
  (>! ch 42)
  [(await worker) @results (await reader) (await sleeper) (<! t) (mapv await busy) @counter (debugging)])"#;
    assert_eq!(eval(Some(&img), after), r#"[:done [20 0 25] "got 42" :slept nil [0 1 2 3] 8000000 []]"#);
}

#[test]
fn the_saving_fiber_and_unsaved_fibers_end_with_an_error() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("dead");
    let (parked, none) = (d.join("parked.skimg"), d.join("none.skimg"));
    let setup = |path: &PathBuf, opts: &str| {
        format!(
            r#"(do (def ch (chan))
  (def w (spawn-detached (<! ch)))
  (def me (current-fiber))
  (def waits-on-me (spawn-detached (try (await me) (catch Error e (condition-message e)))))
  (sleep 20)
  (:objects (save-image "{}" {opts})))"#,
            path.display()
        )
    };
    eval(None, &setup(&parked, ""));
    eval(None, &setup(&none, ":fibers :none"));
    let msg = "\"Fiber was not saved in the image\"";
    let check = "[(try (await me) (catch Error e (condition-message e))) (await waits-on-me)]";
    assert_eq!(eval(Some(&parked), check), format!("[{msg} {msg}]"));
    assert_eq!(eval(Some(&parked), "(do (>! ch 7) (await w))"), "7");
    // Without fibers: the reader is gone, and a put doesn't vanish into it.
    assert_eq!(eval(Some(&none), "(try (await w) (catch Error e (condition-message e)))"), msg);
    assert_eq!(eval(Some(&none), "(second (alts! [[ch 1] (timeout 50)]))"), "#<channel>");
}

#[test]
fn suites_pass_in_a_runtime_started_from_an_image() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("suites");
    let base = d.join("base.skimg");
    ok(skein().arg("build").arg(&base).output().unwrap());
    for suite in ["core_test", "conc_test", "clos_test", "cond_test", "compiler_test", "lib_test", "test_test"] {
        let out = ok(skein().arg("--image").arg(&base).arg("test").arg(format!("tests/skein/{suite}.skn")).output().unwrap());
        let summary = out.lines().rev().find(|l| l.contains(" checks: ")).unwrap_or("").to_owned();
        assert!(summary.ends_with(" 0 failed, 0 errors"), "{suite} from an image: {out}");
    }
}

#[test]
fn layered_images_store_only_what_changed() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("layers");
    let (l0, l1, l2) = (d.join("l0.skimg"), d.join("l1.skimg"), d.join("l2.skimg"));
    eval(None, &format!("(:objects (save-image \"{}\"))", l0.display()));
    eval(Some(&l0), &format!(r#"(do (defn hello [n] (str "hello " n)) (def answer 42) (save-image "{}" :layered true))"#, l1.display()));
    eval(Some(&l1), &format!(r#"(do (defn more [] (hello answer)) (save-image "{}" :layered true))"#, l2.display()));
    assert_eq!(eval(Some(&l2), "[(more) (count (:chain (image-info)))]"), r#"["hello 42" 3]"#);
    let size = |p: &PathBuf| std::fs::metadata(p).unwrap().len();
    assert!(size(&l1) * 5 < size(&l0), "a delta should be much smaller than its base");
    // A delta refuses a base that has changed since.
    eval(None, &format!("(do (def changed true) (:objects (save-image \"{}\")))", l0.display()));
    let o = skein().arg("--image").arg(&l1).arg("-e").arg("1").output().unwrap();
    assert!(!o.status.success() && text(&o).contains("different version"), "{}", text(&o));
}

#[test]
fn build_is_reproducible_and_runs_the_entry_function() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("build");
    let src = d.join("app.skn");
    std::fs::write(&src, "(ns app)\n(defn main [& args] (println \"app got\" (vec args)) (count args))\n").unwrap();
    let (a, b) = (d.join("a.skimg"), d.join("b.skimg"));
    // (one cache folder for both: a build mustn't write to it, or the second
    // would load from it)
    let cache = d.join("cache");
    for img in [&a, &b] {
        ok(skein().arg("build").arg(img).arg(&src).args(["--entry", "app/main"]).env("SKEIN_CACHE", &cache).output().unwrap());
    }
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap(), "same sources, same image");
    assert!(std::fs::read_dir(&cache).map_or(true, |mut d| d.next().is_none()), "a build doesn't use the module cache");
    let o = skein().arg("--image").arg(&a).args(["x", "y"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "app got [x y]");
    assert_eq!(o.status.code(), Some(2), "an integer result is the exit status");
    // Every argument goes to the entry, a leading -e too.
    let o = skein().arg("--image").arg(&a).args(["-e", "(+ 1 2)"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), r#"app got [-e (+ 1 2)]"#);
    // -e before --image evaluates in the image, entry or not (as skein image does).
    let o = skein().args(["-e", "(+ 1 2)", "--image"]).arg(&a).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "3", "{}", text(&o));
    let o = ok(skein().args(["image", "info"]).arg(&a).output().unwrap());
    assert!(o.contains("app/main"), "{o}");
}

#[test]
fn resources_are_dead_after_a_restore_until_reconnected() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("resources");
    let img = d.join("res.skimg");
    let setup = format!(
        r#"(do
  (defclass Conn () ((name :initform "db")))
  (def eager (register-resource! (Conn :name "eager")))
  (def lazy (register-resource! (Conn :name "lazy")))
  (def reconnects (atom 0))
  (defmethod on-image-restore ((c Conn)) (= (:name c) "eager"))
  (defmethod reconnect ((c Conn)) (swap! reconnects inc) c)
  (:objects (save-image "{}")))"#,
        img.display()
    );
    eval(None, &setup);
    let check = r#"[(resource-live? eager) (resource-live? lazy)
  (handler-case (ensure-live lazy) (ResourceLost [c] :lost))
  (handler-bind [ResourceLost (fn [c] (invoke-restart 'reconnect))] (:name (ensure-live lazy)))
  @reconnects (resource-live? lazy)]"#;
    assert_eq!(eval(Some(&img), check), r#"[true false :lost "lazy" 1 true]"#);
}

#[test]
fn print_object_methods_still_print_after_a_restore() {
    // (whether any class has one is this process's to know: an image's
    // classes used to print as records)
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("print");
    let img = d.join("print.skimg");
    let setup = format!(
        r#"(do (require '[skein.decimal :as dec])
  (defclass Pt () ((x)))
  (defmethod print-object ((p Pt)) "<a point>")
  (def things [(Pt :x 1) (dec/parse "12.30")])
  (:objects (save-image "{}")))"#,
        img.display()
    );
    eval(None, &setup);
    assert_eq!(eval(Some(&img), "(do (prn things) (println things))"), "[<a point> #skein/decimal \"12.30\"]\n[<a point> 12.30]\nnil");
}

#[test]
fn damaged_images_are_refused() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("damaged");
    let good = d.join("good.skimg");
    eval(None, &format!("(:objects (save-image \"{}\"))", good.display()));
    let bytes = std::fs::read(&good).unwrap();
    let mut cases: Vec<(&str, Vec<u8>)> =
        vec![("truncated", bytes[..bytes.len() / 2].to_vec()), ("text", b"not an image\n".to_vec()), ("empty", Vec::new())];
    for at in [16, 100, bytes.len() / 3, bytes.len() - 9] {
        let mut b = bytes.clone();
        b[at] ^= 0x41;
        cases.push(("flipped", b));
    }
    for (what, b) in cases {
        let p = d.join(format!("{what}.skimg"));
        std::fs::write(&p, b).unwrap();
        let o = skein().arg("--image").arg(&p).arg("-e").arg("1").output().unwrap();
        assert_eq!(o.status.code(), Some(1), "{what}: {}", text(&o));
        assert!(text(&o).contains(&*p.to_string_lossy()), "{what}: the error names the file: {}", text(&o));
    }
}

#[test]
fn start_up_from_an_image_is_fast() {
    let _alone = ALONE.write().unwrap_or_else(|e| e.into_inner());
    let d = scratch("startup");
    let img = d.join("app.skimg");
    ok(skein().arg("build").arg(&img).output().unwrap());
    let mut best = u128::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        assert_eq!(eval(Some(&img), "(+ 1 2)"), "3");
        best = best.min(t.elapsed().as_millis());
    }
    // (The 50 ms target is a release build's on a developer's machine. A CI
    // runner, a shared virtual machine, gets a looser bound: a regression
    // by several times still fails there.)
    let limit = if std::env::var_os("CI").is_some() { 250 } else { 50 };
    if cfg!(debug_assertions) {
        eprintln!("start-up from an image: {best} ms (debug build; the {limit} ms target is checked in release)");
    } else {
        assert!(best < limit, "start-up took {best} ms (the target: {limit} ms)");
    }
}

#[test]
fn weak_references_keep_only_what_else_is_saved() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("weak");
    let img = d.join("weak.skimg");
    eval(
        None,
        &format!(
            r#"(do (def strong (vec (range 5)))
                   (def w1 (weak-ref strong))
                   (def w2 (weak-ref (vec (range 7))))
                   (def c (add-cleanup! strong (fn [] :never)))
                   (:objects (save-image "{}")))"#,
            img.display()
        ),
    );
    // The cleanup releases a per-process resource, so it isn't saved.
    assert_eq!(eval(Some(&img), "[(weak-get w1) (weak-get w2) (cancel-cleanup! c)]"), "[[0 1 2 3 4] nil false]");
}

#[test]
fn a_file_handle_from_an_image_is_lost_until_reconnected() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("file-handle");
    let (img, file) = (d.join("fh.skimg"), d.join("log.txt"));
    eval(
        None,
        &format!(
            r#"(do (require 'skein.io)
                   (def h (skein.io/open "{}" :write))
                   (skein.io/write h "before\n")
                   (:objects (save-image "{}")))"#,
            file.display(),
            img.display()
        ),
    );
    let check = format!(
        r#"[(try (skein.io/write h "x") (catch ResourceLost c :lost))
            (handler-bind [ResourceLost (fn [c] (invoke-restart 'reconnect))] (skein.io/write h "after\n") :ok)
            (skein.io/read-text "{}")]"#,
        file.display()
    );
    assert_eq!(eval(Some(&img), &check), r#"[:lost :ok "before\nafter\n"]"#);
}

#[test]
fn foreign_functions_work_again_after_a_restore() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("ffi");
    let img = d.join("ffi.skimg");
    eval(
        None,
        &format!(
            r#"(do (require '[skein.ffi :as ffi])
                   (ffi/defcfun c-strlen (ffi/library :c) "strlen" [:string] :usize)
                   [(c-strlen "abc") (:objects (save-image "{}"))])"#,
            img.display()
        ),
    );
    // The library reloads and the symbol re-resolves without a ResourceLost.
    assert_eq!(eval(Some(&img), r#"[(c-strlen "hello") (c-strlen "")]"#), "[5 0]");
}

#[test]
fn mutable_objects_owned_by_the_saver_are_free_after_a_restore() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("owned");
    let img = d.join("owned.skimg");
    eval(
        None,
        &format!(
            r#"(do (defclass counter () ((n :mutable true :initform 0)) (:mutable))
                   (def c (counter))
                   (set-slot! c :n 5)
                   (:objects (save-image "{}")))"#,
            img.display()
        ),
    );
    // The saving fiber didn't survive, so the object is free to claim.
    assert_eq!(eval(Some(&img), "(do (update-slot! c :n inc) [(:n c) (identical? (owner c) (current-fiber))])"), "[6 true]");
}

#[test]
fn sorted_collections_and_queues_survive_an_image() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("colls");
    let img = d.join("colls.skimg");
    eval(
        None,
        &format!(r#"(do (def sm (sorted-map 3 :c 1 :a)) (def q (queue 1 2)) (def ss (sorted-set 5 4)) (save-image "{}"))"#, img.display()),
    );
    assert_eq!(
        eval(Some(&img), "[(assoc sm 2 :b) (conj q 3) (conj ss 1) (= sm {1 :a 3 :c}) (pop q)]"),
        "[{1 :a, 2 :b, 3 :c} #queue [1 2 3] #{1 4 5} true #queue [2]]"
    );
}

/// An image's checksum over its words after the header (as the loader
/// computes it).
fn reseal(words: &mut [u64]) {
    const HEADER_WORDS: usize = 11;
    const H_SUM: usize = 10;
    let mut h: u64 = 0x243f_6a88_85a3_08d3;
    for &w in &words[HEADER_WORDS..] {
        h = (h ^ w).wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(29);
    }
    words[H_SUM] = h ^ (words.len() - HEADER_WORDS) as u64;
}

#[test]
fn code_in_an_image_is_verified_before_it_runs() {
    // (M8 part 8.11) a proto of a seven-parameter function, damaged two ways
    // a crafted image could be, the checksum made right again
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("verify");
    let good = d.join("good.skimg");
    eval(None, &format!("(do (defn seven [a b c d e f g] [a g]) (:objects (save-image \"{}\")))", good.display()));
    assert_eq!(eval(Some(&good), "(seven 1 2 3 4 5 6 7)"), "[1 7]");
    let bytes = std::fs::read(&good).unwrap();
    let words: Vec<u64> = bytes.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect();
    // A proto in the image: [kind][size][name nfixed variadic nregs consts code locals native]
    let kind = skein_rt::object::Kind::Proto as u64;
    let at = (0..words.len() - 10)
        .find(|&i| words[i] == kind && words[i + 3] == 7 && words[i + 4] == 0 && (7..32).contains(&words[i + 5]))
        .expect("seven's proto");
    let damaged = |what: &str, word: usize, value: u64| {
        let mut w = words.clone();
        w[at + 2 + word] = value;
        reseal(&mut w);
        let p = d.join(format!("{what}.skimg"));
        std::fs::write(&p, w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
        let o = skein().arg("--image").arg(&p).args(["-e", "1"]).output().unwrap();
        assert!(!o.status.success(), "{what}: loaded");
        text(&o)
    };
    let native = damaged("native", 7, 0x1234_5678);
    assert!(native.contains("is not a valid Skein image: its code:") && native.contains("a tier-1 entry from another process"), "{native}");
    let regs = damaged("regs", 3, 2);
    assert!(regs.contains("7 parameters in 2 registers"), "{regs}");
}

/// M9b part 1: `skein build --shake`.
fn shake_sources(d: &std::path::Path) -> PathBuf {
    std::fs::create_dir_all(d.join("shk")).unwrap();
    std::fs::write(
        d.join("shk/util.skn"),
        r#"(ns shk.util)
(defclass Used () ((n)))
(defclass Unused () ((n)))
(defmethod print-object ((u Used)) (str "<used " (:n u) ">"))
(defmethod print-object ((u Unused)) (str "<unused " (:n u) ">"))
(defgeneric size [x])
(defmethod size ((u Used)) (:n u))
(defmethod size :before ((u Used)) nil)
(defn used [n] (Used :n n))
(defn unused [n] (Unused :n n))
"#,
    )
    .unwrap();
    std::fs::write(
        d.join("shk/app.skn"),
        r#"(ns shk.app (:require [shk.util :as u] [skein.json :as json] [skein.edn :as edn]))
(defn plugin [] "plugged in")
(defn main [& args]
  (println (u/used 3) (u/size (u/used 4)) (json/write-string {:a [1 2]}))
  (println (edn/read-string "{:at #inst \"2026-01-01T00:00:00Z\"}"))
  (when (seq args) (println ((var-get (resolve (symbol "shk.app" (first args)))))))
  0)
"#,
    )
    .unwrap();
    std::fs::write(d.join("shk/evals.skn"), "(ns shk.evals)\n(defn main [& args] (println (eval (read-string (first args)))) 0)\n")
        .unwrap();
    d.to_path_buf()
}

#[test]
fn a_shaken_image_leaves_out_what_its_entry_doesnt_reach() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = shake_sources(&scratch("shake"));
    let build = |img: &PathBuf, extra: &[&str]| {
        let o = skein()
            .arg("build")
            .arg(img)
            .arg(d.join("shk/util.skn"))
            .arg(d.join("shk/app.skn"))
            .args(["--entry", "shk.app/main"])
            .args(extra)
            .output()
            .unwrap();
        assert!(o.status.success(), "skein build failed:\n{}", text(&o));
        text(&o)
    };
    let (full, shaken, kept) = (d.join("full.skimg"), d.join("shaken.skimg"), d.join("kept.skimg"));
    build(&full, &[]);
    let report = build(&shaken, &["--shake"]);
    assert!(report.contains("% smaller)") && report.contains("definitions left out"), "{report}");
    assert!(!report.contains(" 0 methods"), "print-object's method for Unused, which nothing makes, goes: {report}");
    assert!(report.contains("warning: shk.app/main calls skein.core/resolve"), "a lookup by name is pointed out: {report}");
    let size = |p: &PathBuf| std::fs::metadata(p).unwrap().len();
    assert!(size(&shaken) * 2 < size(&full), "{} against {}", size(&shaken), size(&full));
    // It does what the whole image does: dispatch (a :before method too),
    // printing its own classes, JSON, and #inst read from EDN.
    let run = |img: &PathBuf, args: &[&str]| skein().arg("--image").arg(img).args(args).output().unwrap();
    let want = "<used 3> 4 {\"a\":[1,2]}\n{:at 2026-01-01T00:00:00Z}";
    assert_eq!(ok(run(&full, &[])), want);
    assert_eq!(ok(run(&shaken, &[])), want);
    // What's looked up by name is gone unless kept.
    assert!(text(&run(&shaken, &["plugin"])).contains("var-get expects Var, got nil"));
    build(&kept, &["--shake", "--keep", "shk.app/plugin"]);
    assert_eq!(ok(run(&kept, &["plugin"])), format!("{want}\nplugged in"));
    // No compiler: skein image info still works (it compiles nothing in the
    // image), and evaluating in it says why it can't.
    let info = ok(skein().args(["image", "info"]).arg(&shaken).output().unwrap());
    assert!(info.contains("shk.app/main") && !info.contains("skein.compiler"), "{info}");
    let o = skein().args(["-e", "(+ 1 2)", "--image"]).arg(&shaken).output().unwrap();
    assert!(text(&o).contains("this image has no skein.core/%eval-root"), "{}", text(&o));
}

#[test]
fn a_shaken_image_that_evaluates_keeps_the_language() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = shake_sources(&scratch("shake-eval"));
    let img = d.join("evals.skimg");
    let o = skein().arg("build").arg(&img).arg(d.join("shk/evals.skn")).args(["--entry", "shk.evals/main", "--shake"]).output().unwrap();
    assert!(text(&o).contains("warning: shk.evals/main calls skein.core/eval: the language is kept whole"), "{}", text(&o));
    let o = skein().arg("--image").arg(&img).arg("(when true (str (+ 1 2) (skein.core/inc 4)))").output().unwrap();
    assert_eq!(ok(o), "35");
    // A REPL's image (no entry) keeps modules whole or not at all, and one
    // left out loads again when required.
    let repl = d.join("repl.skimg");
    eval(None, &format!("(do (require 'skein.json) (:objects (save-image {:?} :shake true)))", repl.display().to_string()));
    let got = eval(Some(&repl), "(do (require '[skein.json :as j]) [(j/write-string [1]) (some #{'skein.compiler} (all-ns))])");
    assert_eq!(got, r#"["[1]" skein.compiler]"#);
}
