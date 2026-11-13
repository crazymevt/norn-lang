//! Loading libraries with `require` (before modules, M8): NORN_PATH lookup,
//! `:as` aliases (also inside syntax-quote), `:refer`, concurrent requires,
//! circular requires, and aliases surviving an image save and load.

use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("norn-libs-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("my")).unwrap();
    std::fs::write(
        d.join("my/util.norn"),
        "(ns my.util)\n(def loads (atom 0))\n(swap! loads inc)\n(defn shout [s] (str s \"!\"))\n(defmacro twice [x] `(str (shout ~x) (shout ~x)))\n",
    )
    .unwrap();
    std::fs::write(
        d.join("my/app.norn"),
        "(ns my.app (:require [my.util :as u :refer [shout]]))\n(defn run [] [(u/shout \"a\") (shout \"b\") (u/twice \"c\")])\n",
    )
    .unwrap();
    std::fs::write(d.join("my/loop-a.norn"), "(ns my.loop-a (:require my.loop-b))\n").unwrap();
    std::fs::write(d.join("my/loop-b.norn"), "(ns my.loop-b (:require my.loop-a))\n").unwrap();
    d
}

fn norn(dir: &PathBuf, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_norn")).args(args).env("NORN_PATH", dir).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn require_loads_once_with_aliases_and_refers() {
    let d = scratch("basic");
    let r = norn(
        &d,
        &[
            "-e",
            r#"(do (require 'my.app)
                   (mapv await (mapv (fn [_] (spawn-detached (require 'my.util))) (range 8)))
                   [(my.app/run) (ns-aliases 'my.app) @my.util/loads (current-ns)
                    (try (require 'nope.lib) (catch Error e :missing))])"#,
        ],
    );
    assert_eq!(r, r#"[["a!" "b!" "c!c!"] {u my.util} 1 user :missing]"#);
}

#[test]
fn circular_requires_are_an_error() {
    let d = scratch("circular");
    let r = norn(&d, &["-e", "(try (require 'my.loop-a) (catch Error e (condition-message e)))"]);
    assert!(r.contains("Circular require"), "{r}");
}

#[test]
fn aliases_survive_an_image() {
    let d = scratch("image");
    let img = d.join("libs.nimg");
    norn(&d, &["-e", &format!("(do (require 'my.app) (:objects (save-image \"{}\")))", img.display())]);
    let r = norn(&d, &["--image", img.to_str().unwrap(), "-e", "(do (in-ns 'my.app) [(u/shout \"x\") (run) @my.util/loads])"]);
    assert_eq!(r, r#"["x!" ["a!" "b!" "c!c!"] 1]"#);
}
