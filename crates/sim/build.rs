//! Derives two identities from the checkout.
//!
//! The protocol identity: two clients can only play together if they step the exact same
//! simulation, so instead of a version number somebody has to remember to bump, the build hashes
//! every `src/*.rs` file of this crate (FNV-1a over the bytes, in path order). The result is
//! identical for the desktop and web builds of the same sources and changes whenever the
//! simulation changes. `endif_sim::SIM_HASH` exposes it; the signaling server refuses clients
//! whose hash differs from its own.
//!
//! The build identity: the git commit (`ENDIF_BUILD_ID` overrides it; `dev` without git). It
//! changes with every push, simulation or not, and `endif_sim::BUILD_ID` exposes it. The server
//! reports it on `GET /build`; a client that sees a different one knows a newer build exists even
//! when it could still play on this one.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    println!("cargo:rerun-if-changed={}", src.display());
    let mut files: Vec<_> = walk(&src);
    files.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = fs::read(path).expect("read sim source");
        // Normalise line endings so checkouts on Windows and Linux agree.
        for b in bytes.into_iter().filter(|b| *b != b'\r') {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    println!("cargo:rustc-env=ENDIF_SIM_HASH={hash:016x}");
    println!("cargo:rustc-env=ENDIF_BUILD_ID={}", build_id());
}

fn build_id() -> String {
    println!("cargo:rerun-if-env-changed=ENDIF_BUILD_ID");
    if let Ok(id) = std::env::var("ENDIF_BUILD_ID")
        && !id.trim().is_empty()
    {
        return id.trim().to_string();
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    // Build again when HEAD moves: the HEAD file, and the branch ref it points at.
    if let Some(git) = git_dir(manifest) {
        let head = git.join("HEAD");
        println!("cargo:rerun-if-changed={}", head.display());
        if let Ok(text) = fs::read_to_string(&head)
            && let Some(r) = text.trim().strip_prefix("ref: ")
        {
            println!("cargo:rerun-if-changed={}", git.join(r.trim()).display());
            println!("cargo:rerun-if-changed={}", git.join("packed-refs").display());
        }
    }
    let out = Command::new("git").arg("-C").arg(manifest).args(["rev-parse", "--short=10", "HEAD"]).output();
    match out {
        Ok(o) if o.status.success() => {
            let id = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if id.is_empty() { "dev".to_string() } else { id }
        }
        _ => "dev".to_string(),
    }
}

/// The repository's `.git` directory (following a worktree's `gitdir:` file), if any.
fn git_dir(from: &Path) -> Option<PathBuf> {
    for dir in from.ancestors() {
        let git = dir.join(".git");
        if git.is_dir() {
            return Some(git);
        }
        if git.is_file() {
            let text = fs::read_to_string(&git).ok()?;
            let target = text.trim().strip_prefix("gitdir:")?.trim();
            let path = Path::new(target);
            return Some(if path.is_absolute() { path.to_path_buf() } else { dir.join(path) });
        }
    }
    None
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out
}
