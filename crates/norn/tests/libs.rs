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

#[test]
fn a_script_gets_its_arguments() {
    let d = scratch("args");
    std::fs::write(d.join("args.norn"), "(require '[norn.string :as str] '[norn.sys :as sys])\n(prn (vec (rest (drop-while (fn [a] (not (str/ends-with? a \"args.norn\"))) (sys/args)))))\n").unwrap();
    assert_eq!(norn(&d, &["args.norn", "one", "two words"]), r#"["one" "two words"]"#);
}

#[test]
fn norn_expand_shows_each_macro_step() {
    // (M8 part 8.9) each form is run after it's shown, so my.app's require
    // and my.util's macro are there for the forms after it
    let d = scratch("expand");
    let out = norn(&d, &["expand", "--steps", "my/app.norn"]);
    assert!(out.contains("my.util/twice (defmacro)"), "{out}");
    assert!(out.contains("=> (norn.core/str (my.util/shout \"c\") (my.util/shout \"c\"))"), "{out}");
    assert!(out.contains("Fully expanded:"), "{out}");
    let plain = norn(&d, &["expand", "-e", "(when a b)"]);
    assert!(plain.ends_with("(if a (do b))"), "{plain}");
}

#[test]
fn a_project_puts_its_packages_on_the_path_and_builds_from_main() {
    // (M8 part 8.10) app uses lib-a by path; the script runs from another
    // folder, and the project is found from the script's
    let d = std::env::temp_dir().join(format!("norn-libs-{}-project", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    for (path, text) in [
        ("app/norn.edn", "{:name app :version \"0.1.0\" :deps {lib-a {:path \"../lib-a\"}} :main app.core/main}\n"),
        ("app/src/app/core.norn", "(ns app.core (:require [la.core :as la]))\n(defn main [& args] (println (la/hello) (vec args)) 0)\n"),
        ("app/run.norn", "(require 'app.core)\n(app.core/main)\n"),
        ("lib-a/norn.edn", "{:name lib-a :version \"0.2.0\"}\n"),
        ("lib-a/src/la/core.norn", "(ns la.core)\n(defn hello [] \"hello from lib-a\")\n"),
    ] {
        std::fs::create_dir_all(d.join(path).parent().unwrap()).unwrap();
        std::fs::write(d.join(path), text).unwrap();
    }
    let elsewhere = std::env::temp_dir();
    let out = norn(&elsewhere, &[d.join("app/run.norn").to_str().unwrap()]);
    assert_eq!(out, "hello from lib-a []");
    let listed = norn(&d.join("app"), &["deps"]);
    assert!(listed.contains("lib-a 0.2.0: path "), "{listed}");
    let img = d.join("app.nimg");
    norn(&d.join("app"), &["build", img.to_str().unwrap()]);
    assert_eq!(norn(&elsewhere, &["--image", img.to_str().unwrap(), "x"]), "hello from lib-a [x]");
}

