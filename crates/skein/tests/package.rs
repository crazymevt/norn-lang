//! `skein package`: a project packaged with the `skein` that built it, run
//! as the app (every argument to its :main, a leading -e included), from
//! wherever it was moved, in the caller's folder.

use std::path::{Path, PathBuf};
use std::process::Command;

fn skein() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_skein"));
    // (SKEIN_UI_HOST names skein-ui-host's file to skein package too)
    c.env_remove("SKEIN_PATH").env_remove("SKEIN_UI_HOST");
    c
}

fn temp(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("skein-package-{name}-{nanos}"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A project whose :main prints its arguments, its folder, a resource and
/// the renderer it's pointed at, and returns how many arguments it got (the
/// exit status). With helper, it declares a native helper as skein-ui does:
/// a stand-in skein-ui-host (.exe on Windows) where cargo would build it.
fn project(dir: &Path, targets: &str, helper: bool) {
    let helpers = if helper { " :helpers {\"skein-ui-host\" [\"build/skein-ui-host\"]}" } else { "" };
    write(
        &dir.join("skein.edn"),
        &format!(
            "{{:name echo-app :version \"1.2.0\" :main echo-app.main/main{helpers}\n :package {{:id \"dev.skein.test.echo\" :display-name \"Echo App\" :resources [\"data\"] :targets {targets}}}}}\n"
        ),
    );
    if helper {
        write(&dir.join(format!("build/skein-ui-host{}", std::env::consts::EXE_SUFFIX)), "a stand-in\n");
    }
    write(
        &dir.join("src/echo-app/main.skn"),
        "(ns echo-app.main (:require [skein.io :as io] [skein.sys :as sys]))\n\
         (defn main [& args]\n\
           (prn :args (vec args))\n\
           (prn :cwd (sys/cwd))\n\
           (prn :hello (io/read-text (io/path (sys/app-resources) \"data\" \"hello.txt\")))\n\
           (prn :host (sys/getenv \"SKEIN_UI_HOST\"))\n\
           (count args))\n",
    );
    write(&dir.join("data/hello.txt"), "hi from data\n");
}

/// Path p as skein writes it: canonical (on Windows the long names, not
/// `RUNNER~1`, and without `\\?\`).
fn canonical(p: &Path) -> String {
    let c = p.canonicalize().unwrap().display().to_string();
    c.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(c)
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
    project(&proj, "[:cli]", true);
    let made = run_ok(skein().arg("package").current_dir(&proj));
    let os = std::env::consts::OS; // (macos, linux, windows: as sys/os names them)
    let base = format!("echo-app-1.2.0-{os}-{}", std::env::consts::ARCH);
    let archive = proj.join("dist").join(format!("{base}.tar.gz"));
    assert!(made.contains(&canonical(&archive)), "{made}");
    // unpacked somewhere else, and run from yet another folder
    let unpacked = dir.join("unpacked");
    let elsewhere = dir.join("elsewhere");
    std::fs::create_dir_all(&unpacked).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    run_ok(Command::new("tar").arg("-xzf").arg(&archive).arg("-C").arg(&unpacked));
    std::fs::remove_dir_all(proj.join("dist")).unwrap();
    let launcher = unpacked.join(&base).join(format!("bin/echo-app{}", std::env::consts::EXE_SUFFIX));
    let out = Command::new(&launcher)
        .args(["-e", "two words", "--image", "x"])
        .current_dir(&elsewhere)
        .env_remove("SKEIN_UI_HOST")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(r#":args ["-e" "two words" "--image" "x"]"#), "every argument goes to :main:\n{text}");
    // (printed, a \ in it as \\; and as the system has it: on Windows maybe
    // C:\Users\RUNNER~1, so compared canonical)
    let cwd = text.lines().find_map(|l| l.strip_prefix(":cwd \"")?.strip_suffix('"')).unwrap_or_else(|| panic!("no :cwd:\n{text}"));
    assert_eq!(canonical(Path::new(&cwd.replace("\\\\", "\\"))), canonical(&elsewhere), "the caller's folder:\n{text}");
    assert!(text.contains(r#":hello "hi from data\n""#), "its resources:\n{text}");
    // its helper, packaged beside its image and given to its code
    let host = unpacked.join(&base).join(format!("lib/echo-app/skein-ui-host{}", std::env::consts::EXE_SUFFIX));
    assert!(host.is_file(), "the helper is packaged as {}", host.display());
    let given = text.lines().find_map(|l| l.strip_prefix(":host \"")?.strip_suffix('"')).unwrap_or_else(|| panic!("no :host:\n{text}"));
    assert_eq!(canonical(Path::new(&given.replace("\\\\", "\\"))), canonical(&host), "SKEIN_UI_HOST:\n{text}");
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
    project(&proj, "[:app]", false);
    run_ok(skein().arg("package").current_dir(&proj));
    let app = proj.join("dist/Echo App.app");
    let contents = app.join("Contents");
    for f in ["Info.plist", "PkgInfo", "MacOS/echo-app", "Resources/echo-app.skimg", "Resources/data/hello.txt"] {
        assert!(contents.join(f).is_file(), "{f} is in the bundle");
    }
    let plist = std::fs::read_to_string(contents.join("Info.plist")).unwrap();
    for want in ["<string>echo-app</string>", "<string>dev.skein.test.echo</string>", "<string>Echo App</string>", "<string>1.2.0</string>"]
    {
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

/// One of skein.pe's test programs (tests/skein/pe), a few kilobytes of
/// real Windows program: what a Windows app is made from here, in place of
/// a Windows skein.exe.
fn test_program(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/skein/pe").join(name)
}

/// An icon file of one image: a PNG's signature and header, all skein.pe
/// reads of one (the image is copied as it is).
fn icon_file() -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend(32u32.to_be_bytes());
    png.extend(32u32.to_be_bytes());
    png.extend([8, 6, 0, 0, 0]);
    let mut ico = vec![0, 0, 1, 0, 1, 0, 32, 32, 0, 0, 1, 0, 32, 0];
    ico.extend((png.len() as u32).to_le_bytes());
    ico.extend(22u32.to_le_bytes());
    ico.extend(png);
    ico
}

/// What skein (with skein.pe and skein.zip) says of a Windows app's files:
/// the program's subsystem, icons and FileDescription, the helper's CPU
/// and subsystem, and the zip's entries.
fn inspect(exe: &Path, helper: &Path, zip: &Path) -> String {
    let path = |p: &Path| canonical(p).replace('\\', "\\\\");
    let expr = format!(
        "(do (require '[skein.pe :as pe] '[skein.zip :as zip] '[skein.io :as io])\n\
           (let [x (io/read-bytes \"{}\") r (pe/resources x)]\n\
             (prn :subsystem (:subsystem (pe/info x)))\n\
             (prn :icons (mapv (fn [g] (mapv :width (:images g))) (pe/icon-groups r)))\n\
             (prn :description (get-in (pe/read-version-info (get-in r [pe/rt-version 1 1033])) [:strings \"FileDescription\"])))\n\
           (prn :helper ((juxt :machine :subsystem) (pe/info (io/read-bytes \"{}\"))))\n\
           (prn :entries (mapv :name (zip/entries (io/read-bytes \"{}\")))))",
        path(exe),
        path(helper),
        path(zip)
    );
    run_ok(skein().arg("-e").arg(expr))
}

#[test]
fn a_windows_app_is_made_on_any_os() {
    let dir = temp("windows");
    let proj = dir.join("echo");
    // a helper built for both CPUs: the one for the launcher's is bundled
    write(
        &proj.join("skein.edn"),
        "{:name echo-app :version \"1.2.0\" :main echo-app.main/main\n\
          :helpers {\"skein-ui-host\" [\"build/x64/skein-ui-host\" \"build/arm64/skein-ui-host\"]}\n\
          :package {:display-name \"Echo App\" :resources [\"data\"] :windows {:icon \"echo.ico\" :company \"Example\"}}}\n",
    );
    write(&proj.join("src/echo-app/main.skn"), "(ns echo-app.main)\n(defn main [& args] (count args))\n");
    write(&proj.join("data/hello.txt"), "hi from data\n");
    std::fs::write(proj.join("echo.ico"), icon_file()).unwrap();
    for (cpu, program) in [("x64", "hello-x64.exe"), ("arm64", "hello-arm64.exe")] {
        std::fs::create_dir_all(proj.join("build").join(cpu)).unwrap();
        std::fs::copy(test_program(program), proj.join("build").join(cpu).join("skein-ui-host.exe")).unwrap();
    }
    // (made elsewhere than Windows, SKEIN_UI_HOST naming a program for that
    // system is passed over: on a Mac, it's the Mac renderer)
    let mut c = skein();
    c.args(["package", "--windows", "--launcher"]).arg(test_program("hello-arm64.exe")).current_dir(&proj);
    if !cfg!(windows) {
        let mac_host = dir.join("mac/skein-ui-host");
        write(&mac_host, "not a Windows program\n");
        c.env("SKEIN_UI_HOST", &mac_host);
    }
    let made = run_ok(&mut c);
    let zip = proj.join("dist/Echo App-1.2.0-windows-aarch64.zip");
    assert!(made.contains(&canonical(&zip)), "{made}");
    let root = proj.join("dist/windows-aarch64/Echo App");
    for f in ["bin/echo-app.exe", "lib/echo-app/echo-app.skimg", "lib/echo-app/data/hello.txt", "lib/echo-app/skein-ui-host.exe"] {
        assert!(root.join(f).is_file(), "{f} is in the app");
    }
    let said = inspect(&root.join("bin/echo-app.exe"), &root.join("lib/echo-app/skein-ui-host.exe"), &zip);
    // windowed, as an app that bundles skein-ui's renderer is, and so is
    // the renderer (a console program would get a console window)
    assert!(said.contains(":subsystem :gui"), "{said}");
    assert!(said.contains(":helper [:aarch64 :gui]"), "{said}");
    assert!(said.contains(":icons [[32]]"), "{said}");
    assert!(said.contains(":description \"Echo App\""), "{said}");
    assert!(
        said.contains(
            r#":entries ["Echo App/bin/echo-app.exe" "Echo App/lib/echo-app/data/hello.txt" "Echo App/lib/echo-app/echo-app.skimg" "Echo App/lib/echo-app/skein-ui-host.exe"]"#
        ),
        "{said}"
    );

    // :gui false: a console program, its helper as it was
    let edn = std::fs::read_to_string(proj.join("skein.edn")).unwrap().replace(":company", ":gui false :company");
    write(&proj.join("skein.edn"), &edn);
    run_ok(skein().args(["package", "--windows", "--launcher"]).arg(test_program("hello-arm64.exe")).current_dir(&proj));
    let said = inspect(&root.join("bin/echo-app.exe"), &root.join("lib/echo-app/skein-ui-host.exe"), &zip);
    assert!(said.contains(":subsystem :console") && said.contains(":helper [:aarch64 :console]"), "{said}");

    // an x64 launcher with no x64 helper: refused, saying what's missing
    std::fs::remove_dir_all(proj.join("build/x64")).unwrap();
    let out = skein().args(["package", "--windows", "--launcher"]).arg(test_program("hello-x64.exe")).current_dir(&proj).output().unwrap();
    assert_eq!(Some(1), out.status.code());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("needs its helper skein-ui-host for windows-x86_64"), "{text}");
    // …unless it's given
    run_ok(
        skein()
            .args(["package", "--windows", "--helper"])
            .arg(format!("skein-ui-host={}", test_program("hello-x64.exe").display()))
            .arg("--launcher")
            .arg(test_program("hello-x64.exe"))
            .current_dir(&proj),
    );
    assert!(proj.join("dist/Echo App-1.2.0-windows-x86_64.zip").is_file());

    // made elsewhere than Windows, it needs to be told the launcher
    if !cfg!(windows) {
        let out = skein().args(["package", "--windows"]).current_dir(&proj).output().unwrap();
        assert_eq!(Some(1), out.status.code());
        assert!(String::from_utf8_lossy(&out.stdout).contains("--launcher FILE"));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The helper an archive made with these arguments and SKEIN_UI_HOST
/// holds; or what skein package said, when it failed.
fn packaged_helper(proj: &Path, args: &[&str], var: Option<&Path>) -> Result<Vec<u8>, String> {
    let mut c = skein();
    c.arg("package").arg("--cli").args(args).current_dir(proj);
    if let Some(v) = var {
        c.env("SKEIN_UI_HOST", v);
    }
    let out = c.output().unwrap();
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let base = format!("echo-app-1.2.0-{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let unpacked = proj.join("unpacked");
    let _ = std::fs::remove_dir_all(&unpacked);
    std::fs::create_dir_all(&unpacked).unwrap();
    run_ok(Command::new("tar").arg("-xzf").arg(proj.join("dist").join(format!("{base}.tar.gz"))).arg("-C").arg(&unpacked));
    Ok(std::fs::read(unpacked.join(&base).join(format!("lib/echo-app/skein-ui-host{}", std::env::consts::EXE_SUFFIX))).unwrap())
}

#[test]
fn a_helper_can_come_from_its_environment_variable() {
    let dir = temp("helper-var");
    let proj = dir.join("echo");
    project(&proj, "[:cli]", true);
    // (a Windows program for this CPU, so it's a helper there too)
    let program = |name: &str| {
        let f = dir.join(name).join(format!("skein-ui-host{}", std::env::consts::EXE_SUFFIX));
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::copy(test_program(if cfg!(target_arch = "aarch64") { "hello-arm64.exe" } else { "hello-x64.exe" }), &f).unwrap();
        std::fs::write(f.with_extension("note"), name).unwrap();
        f
    };
    let from_var = program("from-var");
    let given = program("given");
    let mut given_bytes = std::fs::read(&given).unwrap();
    given_bytes.extend(b"given");
    std::fs::write(&given, &given_bytes).unwrap();
    let built = std::fs::read(proj.join(format!("build/skein-ui-host{}", std::env::consts::EXE_SUFFIX))).unwrap();

    assert_eq!(Ok(built.clone()), packaged_helper(&proj, &[], None), "the package's own candidate");
    assert_eq!(Ok(std::fs::read(&from_var).unwrap()), packaged_helper(&proj, &[], Some(&from_var)), "SKEIN_UI_HOST before it");
    let flag = format!("skein-ui-host={}", given.display());
    assert_eq!(Ok(given_bytes), packaged_helper(&proj, &["--helper", &flag], Some(&from_var)), "--helper before both");
    // with no candidate there, the variable is enough
    std::fs::remove_dir_all(proj.join("build")).unwrap();
    assert_eq!(Ok(std::fs::read(&from_var).unwrap()), packaged_helper(&proj, &[], Some(&from_var)));
    // one naming nothing is an error that says so; an empty one is unset
    let said = packaged_helper(&proj, &[], Some(&dir.join("nowhere"))).unwrap_err();
    assert!(said.contains("(SKEIN_UI_HOST) isn't there"), "{said}");
    let said = packaged_helper(&proj, &[], Some(Path::new(""))).unwrap_err();
    assert!(said.contains("needs its helper skein-ui-host") && said.contains("SKEIN_UI_HOST=FILE"), "{said}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_project_without_main_is_refused() {
    let dir = temp("nomain");
    write(&dir.join("skein.edn"), "{:name thing}\n");
    let out = skein().arg("package").current_dir(&dir).output().unwrap();
    assert_eq!(Some(1), out.status.code());
    assert!(String::from_utf8_lossy(&out.stdout).contains("needs a :main"));
    std::fs::remove_dir_all(&dir).unwrap();
}
