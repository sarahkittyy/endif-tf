//! Desktop only: the "update now" button. The game cannot replace its own executable, so it hands
//! off to `endif-updater` (`crates/updater`), which ships next to it in the desktop packages, and
//! quits. The updater downloads the current package, waits for this process to exit, swaps the
//! files and starts the game again.

use crate::config::ClientConfig;
use std::process::Command;

const UPDATER: &str = if cfg!(windows) { "endif-updater.exe" } else { "endif-updater" };

/// Starts the updater. The caller exits the app on `Ok`.
pub fn launch_updater(cfg: &ClientConfig, server_version: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find the game executable: {e}"))?;
    let dir = exe.parent().ok_or("the game executable has no directory")?.to_path_buf();
    let updater = dir.join(UPDATER);
    if !updater.is_file() {
        return Err(format!("the updater is missing next to the game; download the current build from {}", cfg.download_url()));
    }
    let mut cmd = Command::new(&updater);
    cmd.current_dir(&dir)
        .arg("--dir")
        .arg(&dir)
        .arg("--url")
        .arg(cfg.download_url())
        .arg("--launch")
        .arg(&exe)
        .arg("--wait-pid")
        .arg(std::process::id().to_string());
    if !server_version.is_empty() {
        cmd.arg("--expect").arg(server_version);
    }
    cmd.spawn().map_err(|e| format!("could not start the updater: {e}"))?;
    Ok(())
}
