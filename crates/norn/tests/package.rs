//! `norn package`: a project packaged with the `norn` that built it, run
//! as the app (every argument to its :main, a leading -e included), from
//! wherever it was moved, in the caller's folder.

use std::path::{Path, PathBuf};
use std::process::Command;

fn norn() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_norn"));
    c.env_remove("NORN_PATH");
    c
}

fn temp(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("norn-package-{name}-{nanos}"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A project whose :main prints its arguments, its folder and a resource,
/// and returns how many arguments it got (the exit status).
fn project(dir: &Path, targets: &str) {
    write(
        &dir.join("norn.edn"),
        &format!(
            "{{:name echo-app :version \"1.2.0\" :main echo-app.main/main\n :package {{:id \"dev.norn.test.echo\" :display-name \"Echo App\" :resources [\"data\"] :targets {targets}}}}}\n"
        ),
    );
    write(
        &dir.join("src/echo-app/main.norn"),
        "(ns echo-app.main (:require [norn.io :as io] [norn.sys :as sys]))\n\
         (defn main [& args]\n\
           (prn :args (vec args))\n\
           (prn :cwd (sys/cwd))\n\
           (prn :hello (io/read-text (io/path (sys/app-resources) \"data\" \"hello.txt\")))\n\
           (count args))\n",
    );
    write(&dir.join("data/hello.txt"), "hi from data\n");
}

fn run_ok(c: &mut Command) -> String {
    let out = c.output().expect("run");
    assert!(out.status.success(), "{c:?} failed:\n{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_command_line_archive_runs_its_main_from_anywhere() {
    let dir = temp("cli");
    let proj = dir.join("echo");
    project(&proj, "[:cli]");
    let made = run_ok(norn().arg("package").current_dir(&proj));
    let os = if cfg!(target_os = "macos") { "macos" } else { "linux" };
    let base = format!("echo-app-1.2.0-{os}-{}", std::env::consts::ARCH);
    let archive = proj.join("dist").join(format!("{base}.tar.gz"));
    assert!(made.contains(archive.to_str().unwrap()), "{made}");
    // unpacked somewhere else, and run from yet another folder
    let unpacked = dir.join("unpacked");
    let elsewhere = dir.join("elsewhere");
    std::fs::create_dir_all(&unpacked).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    run_ok(Command::new("tar").arg("-xzf").arg(&archive).arg("-C").arg(&unpacked));
    std::fs::remove_dir_all(proj.join("dist")).unwrap();
    let launcher = unpacked.join(&base).join("bin/echo-app");
    let out = Command::new(&launcher).args(["-e", "two words", "--image", "x"]).current_dir(&elsewhere).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(r#":args ["-e" "two words" "--image" "x"]"#), "every argument goes to :main:\n{text}");
    let cwd = elsewhere.canonicalize().unwrap();
    assert!(text.contains(&format!(":cwd \"{}\"", cwd.display())), "the caller's folder:\n{text}");
    assert!(text.contains(r#":hello "hi from data\n""#), "its resources:\n{text}");
    assert_eq!(Some(4), out.status.code(), ":main's integer is the exit status");
    // through a symbolic link, as when linked into a bin folder
    #[cfg(unix)]
    {
        let link = dir.join("echo-link");
        std::os::unix::fs::symlink(&launcher, &link).unwrap();
        let out = Command::new(&link).arg("linked").output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).contains(r#":args ["linked"]"#));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn a_macos_app_is_a_signed_bundle_that_runs() {
    let dir = temp("app");
    let proj = dir.join("echo");
    project(&proj, "[:app]");
    run_ok(norn().arg("package").current_dir(&proj));
    let app = proj.join("dist/Echo App.app");
    let contents = app.join("Contents");
    for f in ["Info.plist", "PkgInfo", "MacOS/echo-app", "Resources/echo-app.nimg", "Resources/data/hello.txt"] {
        assert!(contents.join(f).is_file(), "{f} is in the bundle");
    }
    let plist = std::fs::read_to_string(contents.join("Info.plist")).unwrap();
    for want in ["<string>echo-app</string>", "<string>dev.norn.test.echo</string>", "<string>Echo App</string>", "<string>1.2.0</string>"] {
        assert!(plist.contains(want), "Info.plist has {want}:\n{plist}");
    }
    run_ok(Command::new("codesign").args(["--verify", "--strict"]).arg(&app));
    assert!(proj.join("dist/Echo App-1.2.0-macos-aarch64.zip").is_file() || cfg!(not(target_arch = "aarch64")));
    // moved away from the project, it still runs
    let moved = dir.join("Applications");
    std::fs::create_dir_all(&moved).unwrap();
    run_ok(Command::new("ditto").arg(&app).arg(moved.join("Echo App.app")));
    std::fs::remove_dir_all(proj.join("dist")).unwrap();
    let out = Command::new(moved.join("Echo App.app/Contents/MacOS/echo-app")).arg("-e").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains(r#":args ["-e"]"#));
    assert_eq!(Some(1), out.status.code());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_project_without_main_is_refused() {
    let dir = temp("nomain");
    write(&dir.join("norn.edn"), "{:name thing}\n");
    let out = norn().arg("package").current_dir(&dir).output().unwrap();
    assert_eq!(Some(1), out.status.code());
    assert!(String::from_utf8_lossy(&out.stdout).contains("needs a :main"));
    std::fs::remove_dir_all(&dir).unwrap();
}
