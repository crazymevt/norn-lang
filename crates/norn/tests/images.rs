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
    let d = scratch("suites");
    let base = d.join("base.nimg");
    ok(norn().arg("build").arg(&base).output().unwrap());
    for suite in ["core_test", "conc_test", "clos_test", "cond_test", "compiler_test", "lib_test"] {
        let out = ok(norn().arg("--image").arg(&base).arg(format!("tests/norn/{suite}.norn")).output().unwrap());
        let summary = out.lines().find(|l| l.contains(" checks, ")).unwrap_or("").to_owned();
        assert!(summary.ends_with(" 0 failures"), "{suite} from an image: {out}");
    }
}

#[test]
fn layered_images_store_only_what_changed() {
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
    let d = scratch("build");
    let src = d.join("app.norn");
    std::fs::write(&src, "(ns app)\n(defn main [& args] (println \"app got\" (vec args)) (count args))\n").unwrap();
    let (a, b) = (d.join("a.nimg"), d.join("b.nimg"));
    for img in [&a, &b] {
        ok(norn().arg("build").arg(img).arg(&src).args(["--entry", "app/main"]).output().unwrap());
    }
    assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap(), "same sources, same image");
    let o = norn().arg("--image").arg(&a).args(["x", "y"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "app got [x y]");
    assert_eq!(o.status.code(), Some(2), "an integer result is the exit status");
}

#[test]
fn resources_are_dead_after_a_restore_until_reconnected() {
    let d = scratch("resources");
    let img = d.join("res.nimg");
    let setup = format!(
        r#"(do
  (defclass Conn () ((name :initform "db")))
  (def eager (register-resource! (Conn :name "eager")))
  (def lazy (register-resource! (Conn :name "lazy")))
  (def reconnects (atom 0))
  (defmethod on-image-restore ((c Conn)) (= (:name c) "eager"))
  (defmethod reconnect ((c Conn)) (swap! reconnects inc))
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
fn damaged_images_are_refused() {
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
