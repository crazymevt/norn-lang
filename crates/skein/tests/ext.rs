//! M7.5 done-check (this repository's half): native extensions and the
//! main-thread handoff, through `skein-ext-demo` (src/bin/ext_demo.rs).

use std::path::PathBuf;
use std::process::Command;

fn demo(args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_skein-ext-demo")).args(args).envs(env.iter().copied()).output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_owned(), String::from_utf8_lossy(&out.stderr).trim().to_owned())
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skein-ext-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const RECEIVE: &str = "(let [ch (chan 64)] (skein.demo/subscribe ch 20000) \
    (loop [n 0 main 0 last -1 ordered true] (let [e (take! ch)] \
      (if (nil? e) [n main ordered (skein.demo/posted)] \
        (recur (inc n) (if (:main e) (inc main) main) (:n e) (and ordered (= (:n e) (inc last))))))))";

#[test]
fn the_main_thread_feeds_a_channel_in_order_under_gc_stress() {
    for env in [
        &[("SKEIN_GC_STRESS", "2"), ("SKEIN_TIER", "0")][..],
        &[("SKEIN_GC_STRESS", "2"), ("SKEIN_THREADS", "2"), ("SKEIN_TIER", "0")][..],
        &[][..],
    ] {
        let (ok, out, err) = demo(&["-e", RECEIVE], env);
        assert!(ok, "{env:?}: {err}");
        assert_eq!(out, "[20000 20000 true 20000]", "{env:?}");
    }
}

#[test]
fn extension_primitives_and_native_handles() {
    let (ok, out, err) = demo(
        &[
            "-e",
            "(let [c (skein.demo/counter)] [(skein.demo/bump! c) (skein.demo/bump! c) (handle-kind c) \
                 (do (%io-close c) (try (skein.demo/bump! c) (catch Error e (condition-message e))))])",
        ],
        &[],
    );
    assert!(ok, "{err}");
    assert_eq!(out, r#"[1 2 :counter "The handle is closed"]"#);
    // An extension's embedded library loads with require.
    let (ok, out, err) = demo(&["-e", "(do (require '[skein.demo.lib :as d]) (d/greet \"ext\"))"], &[]);
    assert!(ok, "{err}");
    assert_eq!(out, r#""hello ext""#);
    // The plain binary has no such namespace.
    let plain = Command::new(env!("CARGO_BIN_EXE_skein")).args(["-e", "(resolve 'skein.demo/counter)"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&plain.stdout).trim(), "nil");
}

#[test]
fn images_remember_their_extensions() {
    let d = scratch("images");
    let img = d.join("demo.skimg");
    let save = format!("(def c (skein.demo/counter)) (skein.demo/bump! c) (save-image \"{}\")", img.display());
    let (ok, _, err) = demo(&["-e", &save], &[]);
    assert!(ok, "{err}");
    // Reloaded in the same binary, the native object is gone, like a socket.
    let (ok, out, err) =
        demo(&["--image", img.to_str().unwrap(), "-e", "(try (skein.demo/bump! c) (catch Error e (condition-message e)))"], &[]);
    assert!(ok, "{err}");
    assert!(out.contains("Resource lost"), "{out}");
    // A binary without the extension refuses the image.
    let plain = Command::new(env!("CARGO_BIN_EXE_skein")).args(["--image", img.to_str().unwrap(), "-e", "1"]).output().unwrap();
    assert!(!plain.status.success());
    assert!(String::from_utf8_lossy(&plain.stderr).contains("different build"), "{}", String::from_utf8_lossy(&plain.stderr));
}
