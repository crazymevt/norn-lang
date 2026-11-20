# skein.pe's test programs

Real Windows programs, linked by a real linker, for `pe_test.skn` and
`crates/skein/tests/package.rs` (`skein package --windows`):

- `hello-arm64.exe`, `hello-x64.exe`: `hello.rs`, a program of a few
  kilobytes for Windows on ARM64 and x64 (a console program: it writes a
  line to its standard output and exits with 7). Its sections are those a
  linker usually writes, `.reloc` last.
- `hello-manifest-arm64.exe`: the same with a resource already, from
  `manifest.res` (an `RT_MANIFEST` that asks for the UTF-8 code page). Its
  `.rsrc` comes before `.reloc`, as a Microsoft linker puts it.

`build.sh` makes them again, on any OS with rustup's two Windows targets:
`rust-lld`, which comes with Rust, links them, with no Windows SDK or
runtime library. `manifest.res` is a compiled resource file (the format
`rc.exe` writes), written by hand.

They run on Windows too: copied there, `hello-arm64.exe` prints its line.
A copy that `skein.pe` has edited should still run, show its icon in
Explorer, and show its version under Properties > Details.
