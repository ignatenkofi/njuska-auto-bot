//! Build script: bakes the short git SHA into the binary as `NJUSKA_GIT_SHA`.
//!
//! Why a build script and not a runtime `git` call? The deployed binary runs
//! on a VM with no git checkout — the SHA has to be captured at *compile*
//! time, where the source tree (usually) is one. When it isn't (a source
//! tarball, a vendored build), we fall back to `"unknown"` rather than
//! failing the build.

use std::process::Command;

fn main() {
    // Re-run when HEAD moves (new commit, branch switch) so the baked SHA
    // can't go stale. `.git/HEAD` covers branch switches; the refs dir covers
    // new commits on the current branch.
    println!("cargo::rerun-if-changed=.git/HEAD");
    println!("cargo::rerun-if-changed=.git/refs");

    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo::rustc-env=NJUSKA_GIT_SHA={sha}");
}
