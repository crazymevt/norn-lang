//! What more than one of these tests needs.

/// Path p as it's written inside a Skein string: with `\` and `"` escaped,
/// since a Windows path is full of `\` (`C:\Users` would read as a `\U`).
pub fn esc(p: &std::path::Path) -> String {
    p.display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
}
