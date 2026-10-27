// Stamps FINANCE_VERSION (finance_core::VERSION): see version_string.rs for
// the rules. Reads git at build time; a build without git (a tarball, a CI
// checkout without history) falls back to the crate's version, silently.
// "-dirty" reflects the tree when this script last ran (it is not re-run for
// every edit, only for a new commit, tag or FINANCE_VERSION). Worktrees that
// share one CARGO_TARGET_DIR share this script's last run too: `touch
// core/build.rs` before a build whose version matters.

include!("version_string.rs");

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

/// A path git prints, made absolute against the crate's directory.
fn git_path(args: &[&str]) -> Option<PathBuf> {
    let p = PathBuf::from(git(args)?);
    Some(if p.is_absolute() { p } else { Path::new(&std::env::var("CARGO_MANIFEST_DIR").ok()?).join(p) })
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=version_string.rs");
    println!("cargo:rerun-if-env-changed=FINANCE_VERSION");
    let env = std::env::var("FINANCE_VERSION").ok();
    let describe = git(&["describe", "--tags", "--always", "--dirty"]);
    if describe.is_some() {
        // HEAD moves on a checkout or commit (its branch ref too, loose or
        // packed); a tag lands in refs/tags or packed-refs. A worktree keeps
        // its HEAD apart from the shared refs.
        if let Some(dir) = git_path(&["rev-parse", "--git-dir"]) {
            println!("cargo:rerun-if-changed={}", dir.join("HEAD").display());
        }
        if let Some(common) = git_path(&["rev-parse", "--git-common-dir"]) {
            if let Some(r) = git(&["symbolic-ref", "-q", "HEAD"]) {
                println!("cargo:rerun-if-changed={}", common.join(r).display());
            }
            println!("cargo:rerun-if-changed={}", common.join("packed-refs").display());
            println!("cargo:rerun-if-changed={}", common.join("refs/tags").display());
        }
    }
    let v = resolve(env.as_deref(), describe.as_deref(), &std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
    println!("cargo:rustc-env=FINANCE_VERSION={v}");
}
