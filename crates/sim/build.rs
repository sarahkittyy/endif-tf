//! Derives the network protocol identity from the simulation sources.
//!
//! Two clients can only play together if they step the exact same simulation, so instead of a
//! version number somebody has to remember to bump, the build hashes every `src/*.rs` file of this
//! crate (FNV-1a over the bytes, in path order). The result is identical for the desktop and web
//! builds of the same sources and changes whenever the simulation changes. `endif_sim::SIM_HASH`
//! exposes it; the signaling server refuses clients whose hash differs from its own.

use std::fs;
use std::path::Path;

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
