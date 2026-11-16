//! M5 done-check: images. A saved fiber resumes after load (parked on a
//! channel, sleeping, preempted mid-loop, and crashed into the debugger), the
//! test suites pass in a runtime started from an image, layers and `norn
//! build` work, damaged images are refused, and start-up is under 50 ms
//! (checked in release builds; debug builds are several times slower).
//!
//! Every test drives the real `norn` binary, since an image is loaded at
//! process start.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Instant;

/// The image tests run in parallel threads; the start-up timing runs alone
/// (others hold this shared, it holds it exclusively), so it measures
/// start-up rather than how busy the machine is.
static ALONE: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn norn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_norn"));
    c.current_dir(format!("{}/../..", env!("CARGO_MANIFEST_DIR")));
    c
}

/// A fresh directory for one test's images.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("norn-images-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn ok(o: Output) -> String {
    assert!(o.status.success(), "norn failed:\n{}", text(&o));
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

fn eval(image: Option<&PathBuf>, expr: &str) -> String {
    let mut c = norn();
    if let Some(i) = image {
        c.arg("--image").arg(i);
    }
    ok(c.arg("-e").arg(expr).output().unwrap())
}

#[test]
fn saved_fibers_resume_after_load() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("fibers");
    let img = d.join("fibers.nimg");
    // A worker that crashes into the debugger, a channel reader, a sleeper, a
    // timeout channel, and busy fibers preempted mid-loop.
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
  (def busy (mapv (fn [i] (spawn-detached (dotimes [_ 100000] (swap! counter inc)) i)) (range 4)))
  (sleep 30)
  (save-image "{}")
  [(count (debugging)) (< @counter 400000)])"#,
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
    assert_eq!(eval(Some(&img), after), r#"[:done [20 0 25] "got 42" :slept nil [0 1 2 3] 400000 []]"#);
}

#[test]
fn the_saving_fiber_and_unsaved_fibers_end_with_an_error() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("dead");
    let (parked, none) = (d.join("parked.nimg"), d.join("none.nimg"));
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
    let base = d.join("base.nimg");
    ok(norn().arg("build").arg(&base).output().unwrap());
    for suite in ["core_test", "conc_test", "clos_test", "cond_test", "compiler_test", "lib_test", "test_test"] {
        let out = ok(norn().arg("--image").arg(&base).arg("test").arg(format!("tests/norn/{suite}.norn")).output().unwrap());
        let summary = out.lines().rev().find(|l| l.contains(" checks: ")).unwrap_or("").to_owned();
        assert!(summary.ends_with(" 0 failed, 0 errors"), "{suite} from an image: {out}");
    }
}

#[test]
fn layered_images_store_only_what_changed() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("layers");
    let (l0, l1, l2) = (d.join("l0.nimg"), d.join("l1.nimg"), d.join("l2.nimg"));
    eval(None, &format!("(:objects (save-image \"{}\"))", l0.display()));
    eval(Some(&l0), &format!(r#"(do (defn hello [n] (str "hello " n)) (def answer 42) (save-image "{}" :layered true))"#, l1.display()));
    eval(Some(&l1), &format!(r#"(do (defn more [] (hello answer)) (save-image "{}" :layered true))"#, l2.display()));
    assert_eq!(eval(Some(&l2), "[(more) (count (:chain (image-info)))]"), r#"["hello 42" 3]"#);
    let size = |p: &PathBuf| std::fs::metadata(p).unwrap().len();
    assert!(size(&l1) * 5 < size(&l0), "a delta should be much smaller than its base");
    // A delta refuses a base that has changed since.
    eval(None, &format!("(do (def changed true) (:objects (save-image \"{}\")))", l0.display()));
    let o = norn().arg("--image").arg(&l1).arg("-e").arg("1").output().unwrap();
    assert!(!o.status.success() && text(&o).contains("different version"), "{}", text(&o));
}

#[test]
fn build_is_reproducible_and_runs_the_entry_function() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("build");
    let src = d.join("app.norn");
    std::fs::write(&src, "(ns app)\n(defn main [& args] (println \"app got\" (vec args)) (count args))\n").unwrap();
    let (a, b) = (d.join("a.nimg"), d.join("b.nimg"));
    // (one cache folder for both: a build mustn't write to it, or the second
    // would load from it)
    let cache = d.join("cache");
    for img in [&a, &b] {
        ok(norn().arg("build").arg(img).arg(&src).args(["--entry", "app/main"]).env("NORN_CACHE", &cache).output().unwrap());
    }
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap(), "same sources, same image");
    assert!(std::fs::read_dir(&cache).map_or(true, |mut d| d.next().is_none()), "a build doesn't use the module cache");
    let o = norn().arg("--image").arg(&a).args(["x", "y"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "app got [x y]");
    assert_eq!(o.status.code(), Some(2), "an integer result is the exit status");
    // Every argument goes to the entry, a leading -e too.
    let o = norn().arg("--image").arg(&a).args(["-e", "(+ 1 2)"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), r#"app got [-e (+ 1 2)]"#);
    // -e before --image evaluates in the image, entry or not (as norn image does).
    let o = norn().args(["-e", "(+ 1 2)", "--image"]).arg(&a).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "3", "{}", text(&o));
    let o = ok(norn().args(["image", "info"]).arg(&a).output().unwrap());
    assert!(o.contains("app/main"), "{o}");
}

#[test]
fn resources_are_dead_after_a_restore_until_reconnected() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("resources");
    let img = d.join("res.nimg");
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
    let img = d.join("print.nimg");
    let setup = format!(
        r#"(do (require '[norn.decimal :as dec])
  (defclass Pt () ((x)))
  (defmethod print-object ((p Pt)) "<a point>")
  (def things [(Pt :x 1) (dec/parse "12.30")])
  (:objects (save-image "{}")))"#,
        img.display()
    );
    eval(None, &setup);
    assert_eq!(eval(Some(&img), "(do (prn things) (println things))"), "[<a point> #norn/decimal \"12.30\"]\n[<a point> 12.30]\nnil");
}

#[test]
fn damaged_images_are_refused() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("damaged");
    let good = d.join("good.nimg");
    eval(None, &format!("(:objects (save-image \"{}\"))", good.display()));
    let bytes = std::fs::read(&good).unwrap();
    let mut cases: Vec<(&str, Vec<u8>)> = vec![
        ("truncated", bytes[..bytes.len() / 2].to_vec()),
        ("text", b"not an image\n".to_vec()),
        ("empty", Vec::new()),
    ];
    for at in [16, 100, bytes.len() / 3, bytes.len() - 9] {
        let mut b = bytes.clone();
        b[at] ^= 0x41;
        cases.push(("flipped", b));
    }
    for (what, b) in cases {
        let p = d.join(format!("{what}.nimg"));
        std::fs::write(&p, b).unwrap();
        let o = norn().arg("--image").arg(&p).arg("-e").arg("1").output().unwrap();
        assert_eq!(o.status.code(), Some(1), "{what}: {}", text(&o));
        assert!(text(&o).contains(&*p.to_string_lossy()), "{what}: the error names the file: {}", text(&o));
    }
}

#[test]
fn start_up_from_an_image_is_fast() {
    let _alone = ALONE.write().unwrap_or_else(|e| e.into_inner());
    let d = scratch("startup");
    let img = d.join("app.nimg");
    ok(norn().arg("build").arg(&img).output().unwrap());
    let mut best = u128::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        assert_eq!(eval(Some(&img), "(+ 1 2)"), "3");
        best = best.min(t.elapsed().as_millis());
    }
    if cfg!(debug_assertions) {
        eprintln!("start-up from an image: {best} ms (debug build; the 50 ms target is checked in release)");
    } else {
        assert!(best < 50, "start-up took {best} ms");
    }
}

#[test]
fn weak_references_keep_only_what_else_is_saved() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("weak");
    let img = d.join("weak.nimg");
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
    let (img, file) = (d.join("fh.nimg"), d.join("log.txt"));
    eval(
        None,
        &format!(
            r#"(do (require 'norn.io)
                   (def h (norn.io/open "{}" :write))
                   (norn.io/write h "before\n")
                   (:objects (save-image "{}")))"#,
            file.display(),
            img.display()
        ),
    );
    let check = format!(
        r#"[(try (norn.io/write h "x") (catch ResourceLost c :lost))
            (handler-bind [ResourceLost (fn [c] (invoke-restart 'reconnect))] (norn.io/write h "after\n") :ok)
            (norn.io/read-text "{}")]"#,
        file.display()
    );
    assert_eq!(eval(Some(&img), &check), r#"[:lost :ok "before\nafter\n"]"#);
}

#[test]
fn foreign_functions_work_again_after_a_restore() {
    let _shared = ALONE.read().unwrap_or_else(|e| e.into_inner());
    let d = scratch("ffi");
    let img = d.join("ffi.nimg");
    eval(
        None,
        &format!(
            r#"(do (require '[norn.ffi :as ffi])
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
    let img = d.join("owned.nimg");
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
    let img = d.join("colls.nimg");
    eval(None, &format!(r#"(do (def sm (sorted-map 3 :c 1 :a)) (def q (queue 1 2)) (def ss (sorted-set 5 4)) (save-image "{}"))"#, img.display()));
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
    let good = d.join("good.nimg");
    eval(None, &format!("(do (defn seven [a b c d e f g] [a g]) (:objects (save-image \"{}\")))", good.display()));
    assert_eq!(eval(Some(&good), "(seven 1 2 3 4 5 6 7)"), "[1 7]");
    let bytes = std::fs::read(&good).unwrap();
    let words: Vec<u64> = bytes.chunks(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect();
    // A proto in the image: [kind][size][name nfixed variadic nregs consts code locals native]
    let kind = norn_rt::object::Kind::Proto as u64;
    let at = (0..words.len() - 10)
        .find(|&i| words[i] == kind && words[i + 3] == 7 && words[i + 4] == 0 && (7..32).contains(&words[i + 5]))
        .expect("seven's proto");
    let damaged = |what: &str, word: usize, value: u64| {
        let mut w = words.clone();
        w[at + 2 + word] = value;
        reseal(&mut w);
        let p = d.join(format!("{what}.nimg"));
        std::fs::write(&p, w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
        let o = norn().arg("--image").arg(&p).args(["-e", "1"]).output().unwrap();
        assert!(!o.status.success(), "{what}: loaded");
        text(&o)
    };
    let native = damaged("native", 7, 0x1234_5678);
    assert!(native.contains("is not a valid Norn image: its code:") && native.contains("a tier-1 entry from another process"), "{native}");
    let regs = damaged("regs", 3, 2);
    assert!(regs.contains("7 parameters in 2 registers"), "{regs}");
}
