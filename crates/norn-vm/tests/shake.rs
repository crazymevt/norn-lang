//! A shaken image (save-image :shake, norn.shake) keeps the norn.core names
//! the runtime looks up by name: norn.shake lists them. This checks the
//! list against the Rust side, so a new lookup can't be left off it.

use std::collections::BTreeSet;
use std::path::Path;

fn read_dir_rs(dir: &Path) -> String {
    let mut all = String::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "rs") {
            all.push_str(&std::fs::read_to_string(&p).unwrap());
        }
    }
    all
}

/// The names a `'[…]` list after `(def ^:private NAME` holds.
fn listed(src: &str, def: &str) -> BTreeSet<String> {
    let at = src.find(&format!("(def ^:private {def}")).unwrap_or_else(|| panic!("norn.shake has no {def}"));
    let rest = &src[at..];
    let open = rest.find("'[").unwrap() + 2;
    let close = open + rest[open..].find(']').unwrap();
    rest[open..close].split_whitespace().map(str::to_owned).collect()
}

#[test]
fn the_shake_keeps_every_name_the_runtime_looks_up() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let rust = read_dir_rs(&root.join("crates/norn-vm/src")) + &read_dir_rs(&root.join("crates/norn/src"));
    let core = std::fs::read_to_string(root.join("lib/core/core.norn")).unwrap();
    let shake = std::fs::read_to_string(root.join("lib/norn/shake.norn")).unwrap();
    let kept: BTreeSet<String> = listed(&shake, "runtime-names").union(&listed(&shake, "evaluator-names")).cloned().collect();

    let patterns = [
        r#"core_fn\("([^"]+)""#,
        r#"typed_call\(vm, "([^"]+)""#,
        r#"RootTask::Call\("([^"]+)""#,
        r#"RootTask::\w+\([a-z_]+\) => \("([^"]+)""#,
        r#"symbol\(&mut [a-z.]+, None, "([^"]+)"\)"#,
        r#"Some\("norn\.core"\), "([^"]+)""#,
    ];
    let mut looked_up = BTreeSet::new();
    for p in patterns {
        for c in regex::Regex::new(p).unwrap().captures_iter(&rust) {
            looked_up.insert(c[1].to_owned());
        }
    }
    // (only names norn.core defines: a symbol made for another use isn't one)
    let defined = |n: &str| {
        let q = regex::escape(n);
        regex::Regex::new(&format!(r"\(def[a-z-]*\s+(\^\S+\s+)*{q}[\s)]")).unwrap().is_match(&core)
            || rust.contains(&format!("\"{n}\" "))
    };
    let missing: Vec<&String> = looked_up.iter().filter(|n| defined(n) && !kept.contains(*n)).collect();
    assert!(looked_up.len() > 15, "the patterns found too little: {looked_up:?}");
    assert!(missing.is_empty(), "norn.shake's runtime-names should list {missing:?} (the runtime looks them up by name)");
}
