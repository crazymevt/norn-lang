//! Bootstrap step 6 (runtime.md §9): `skein` starts from its base image,
//! saved in the cache folder the first time it boots from source. Every
//! test has a cache folder of its own (SKEIN_CACHE).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

fn cache(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skein-base-image-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `exe -e expr`, with cache folder c: its output, trimmed.
fn eval(exe: &Path, c: &Path, expr: &str, env: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(exe);
    cmd.args(["-e", expr]).env("SKEIN_CACHE", c).env_remove("SKEIN_BASE_IMAGE");
    for (k, v) in env {
        cmd.env(k, v);
    }
    // (a copy of skein just written can't be run for a moment on Linux:
    // another test's child, forked meanwhile, holds the copy's descriptor
    // open for writing until it execs, and exec refuses a file open for
    // writing, ETXTBSY. Tried again until that child has gone on.)
    let mut tries = 0;
    let o = loop {
        match cmd.output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            r => break r.unwrap(),
        }
    };
    assert!(o.status.success(), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

/// The base images in cache folder c.
fn images(c: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(c)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("base-") && p.extension().is_some_and(|x| x == "skimg"))
        .collect();
    v.sort();
    v
}

fn skein() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_skein"))
}

#[test]
fn the_first_start_saves_it_and_the_next_start_from_it() {
    let c = cache("first");
    assert_eq!(eval(&skein(), &c, "(+ 1 2)", &[]), "3");
    let saved = images(&c);
    assert_eq!(saved.len(), 1, "one image: {saved:?}");
    let when = std::fs::metadata(&saved[0]).unwrap().modified().unwrap();
    // the same answers from it, the library and the compiler included
    assert_eq!(eval(&skein(), &c, "(+ 1 2)", &[]), "3");
    assert_eq!(eval(&skein(), &c, "(do (require '[skein.string :as s]) (s/upper-case \"ok\"))", &[]), "\"OK\"");
    assert_eq!(eval(&skein(), &c, "((fn f [n] (if (< n 2) n (+ (f (- n 1)) (f (- n 2))))) 20)", &[]), "6765");
    assert_eq!(images(&c), saved, "started from, not saved again");
    // and it looks booted from source: no image to tell of, to layer on, or
    // whose fiber numbers go on
    assert_eq!(eval(&skein(), &c, "(image-info)", &[]), "nil");
    assert_eq!(eval(&skein(), &c, "(str (current-fiber))", &[]), "\"#<fiber 1 running>\"");
    let layered =
        format!("(try (save-image {:?} :layered true) (catch Error e (condition-message e)))", c.join("l.skimg").display().to_string());
    assert!(eval(&skein(), &c, &layered, &[]).contains("needs a base"), "a layered save has no base");
    assert_eq!(std::fs::metadata(&saved[0]).unwrap().modified().unwrap(), when);
    std::fs::remove_dir_all(&c).unwrap();
}

#[test]
fn a_damaged_image_is_booted_past_and_made_again() {
    let c = cache("damaged");
    eval(&skein(), &c, "nil", &[]);
    let [img] = images(&c).try_into().unwrap();
    std::fs::write(&img, b"not an image").unwrap();
    assert_eq!(eval(&skein(), &c, "(+ 1 2)", &[]), "3", "it boots from source");
    assert!(std::fs::metadata(&img).unwrap().len() > 100_000, "and saves the image again");
    assert_eq!(eval(&skein(), &c, "(+ 1 2)", &[]), "3");
    std::fs::remove_dir_all(&c).unwrap();
}

#[test]
fn none_is_made_when_its_off_or_for_another_boot() {
    let c = cache("off");
    eval(&skein(), &c, "nil", &[("SKEIN_BASE_IMAGE", "off")]);
    eval(&skein(), &c, "nil", &[("SKEIN_EXPANDER", "off")]);
    eval(&skein(), &c, "nil", &[("SKEIN_COMPILER", "seed")]);
    assert_eq!(images(&c), Vec::<PathBuf>::new());
    // skein build makes its image from source, and leaves the base image alone
    let out = c.join("built.skimg");
    let o = Command::new(skein()).arg("build").arg(&out).env("SKEIN_CACHE", &c).current_dir(&c).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(images(&c), Vec::<PathBuf>::new());
    std::fs::remove_dir_all(&c).unwrap();
}

#[test]
fn a_rebuilt_skein_replaces_only_its_own_image() {
    let c = cache("rebuilt");
    eval(&skein(), &c, "nil", &[]);
    let mine = images(&c);
    // another skein (a copy elsewhere) has an image of its own
    let other = c.join(format!("other-skein{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(skein(), &other).unwrap();
    eval(&other, &c, "nil", &[]);
    let both = images(&c);
    assert_eq!(both.len(), 2, "{both:?}");
    // rebuilt (here: a newer file time), it saves a new one in place of its old
    let f = std::fs::File::options().write(true).open(&other).unwrap();
    f.set_modified(SystemTime::now() + Duration::from_secs(60)).unwrap();
    drop(f);
    eval(&other, &c, "nil", &[]);
    let after = images(&c);
    assert_eq!(after.len(), 2, "{after:?}");
    assert!(after.contains(&mine[0]), "the first skein's image stays");
    assert!(both.iter().any(|p| !after.contains(p)), "the copy's old image went");
    std::fs::remove_dir_all(&c).unwrap();
}
