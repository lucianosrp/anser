// The version string's choice, shared by build.rs and (for its tests)
// src/version.rs through include!. Pure: no git, no environment.

/// `env`: FINANCE_VERSION (CI/release); `describe`: `git describe --tags
/// --always --dirty`; `pkg`: the crate's own version.
///
/// * env wins when it is not blank;
/// * a describe with a tag in it ("v0.1.0", "v0.1.0-3-gabc1234-dirty") is kept as git prints it;
/// * before any tag, describe is a bare hash ("abc1234", "abc1234-dirty"): "0.1.0+gabc1234[-dirty]";
/// * no git: the crate's version.
pub fn resolve(env: Option<&str>, describe: Option<&str>, pkg: &str) -> String {
    if let Some(e) = env.map(str::trim).filter(|e| !e.is_empty()) {
        return e.to_string();
    }
    let Some(d) = describe.map(str::trim).filter(|d| !d.is_empty()) else { return pkg.to_string() };
    let (hash, dirty) = match d.strip_suffix("-dirty") {
        Some(h) => (h, "-dirty"),
        None => (d, ""),
    };
    if hash.len() >= 7 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!("{pkg}+g{hash}{dirty}")
    } else {
        d.to_string()
    }
}
