//! endif-updater: swaps a desktop install for the current build from the site and relaunches it.
//!
//! The game starts it (`crates/client/src/update.rs`) and quits:
//!
//! ```text
//! endif-updater --dir <install dir> --url <archive url> --launch <game exe> [--wait-pid <pid>] [--expect <build id>]
//! ```
//!
//! Steps, in order: move itself out of the install directory (a running exe cannot be replaced on
//! Windows, and it ships inside the archive); download the archive next to the install; wait for
//! the game to exit; unpack into a staging directory; with `--expect`, ask the staged game for its
//! build id and refuse a download that is still the previous build (the site publishes the
//! desktop packages a few minutes after the server; the game normally waits for them, this is
//! the backstop); move the staged files over the install;
//! relaunch the game. Nothing in the install is touched before the staged build has been checked,
//! so a failed download or unpack leaves the old version runnable, and the game is relaunched
//! either way.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// How long the game may take to exit after starting the updater.
const EXIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Temporary copies of the updater are named `endif-updater-<pid>`; leftovers are swept at start.
const RELOCATED_PREFIX: &str = "endif-updater-";

struct Args {
    dir: PathBuf,
    url: String,
    launch: PathBuf,
    wait_pid: Option<u32>,
    expect: Option<String>,
    relocated: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut dir = None;
    let mut url = None;
    let mut launch = None;
    let mut wait_pid = None;
    let mut expect = None;
    let mut relocated = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        let a = a.to_string_lossy().into_owned();
        let mut value = |name: &str| args.next().map(PathBuf::from).ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--dir" => dir = Some(value("--dir")?),
            "--url" => url = Some(value("--url")?.to_string_lossy().into_owned()),
            "--launch" => launch = Some(value("--launch")?),
            "--wait-pid" => wait_pid = Some(value("--wait-pid")?.to_string_lossy().parse::<u32>().map_err(|e| format!("--wait-pid: {e}"))?),
            "--expect" => expect = Some(value("--expect")?.to_string_lossy().trim().to_string()),
            "--relocated" => relocated = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        dir: dir.ok_or("--dir is required")?,
        url: url.ok_or("--url is required")?,
        launch: launch.ok_or("--launch is required")?,
        wait_pid,
        expect,
        relocated,
    })
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("endif-updater: {e}");
            eprintln!("usage: endif-updater --dir <install dir> --url <archive url> --launch <game exe> [--wait-pid <pid>] [--expect <build id>]");
            pause();
            std::process::exit(2);
        }
    };
    match relocate(&args) {
        Ok(true) => return,
        Ok(false) => {}
        Err(e) => println!("note: could not run from a temporary copy ({e}); the updater itself will not be updated"),
    }
    match run(&args) {
        Ok(()) => {
            println!("update complete; starting the game");
            if let Err(e) = relaunch(&args) {
                println!("could not start the game: {e}");
                pause();
                std::process::exit(1);
            }
        }
        Err(e) => {
            println!();
            println!("UPDATE FAILED: {e}");
            println!("the installed version was left as it was.");
            match relaunch(&args) {
                Ok(()) => println!("the previous version is starting again."),
                Err(e) => println!("could not start the previous version either: {e}"),
            }
            pause();
            std::process::exit(1);
        }
    }
}

/// Copies the updater out of the install directory and runs the copy instead, so the file in the
/// install can be replaced like everything else. Returns whether a copy took over.
fn relocate(args: &Args) -> Result<bool, String> {
    let me = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    if args.relocated {
        return Ok(false);
    }
    sweep_old_copies();
    let inside = match (fs::canonicalize(&me), fs::canonicalize(&args.dir)) {
        (Ok(me), Ok(dir)) => me.starts_with(dir),
        _ => me.starts_with(&args.dir),
    };
    if !inside {
        return Ok(false);
    }
    let tmp = std::env::temp_dir().join(format!("{RELOCATED_PREFIX}{}{}", std::process::id(), std::env::consts::EXE_SUFFIX));
    fs::copy(&me, &tmp).map_err(|e| format!("copy to {}: {e}", tmp.display()))?;
    Command::new(&tmp)
        .args(std::env::args_os().skip(1))
        .arg("--relocated")
        .spawn()
        .map_err(|e| format!("start {}: {e}", tmp.display()))?;
    Ok(true)
}

/// Temporary copies from earlier updates cannot delete themselves while running; the next run does.
fn sweep_old_copies() {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(RELOCATED_PREFIX) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn run(args: &Args) -> Result<(), String> {
    let archive = args.dir.join(".endif-update.download");
    let stage = args.dir.join(".endif-update-stage");
    let _ = fs::remove_file(&archive);
    let _ = fs::remove_dir_all(&stage);
    let result = update(args, &archive, &stage);
    // Leave nothing behind either way; a refused download is not worth keeping.
    let _ = fs::remove_dir_all(&stage);
    let _ = fs::remove_file(&archive);
    result
}

fn update(args: &Args, archive: &Path, stage: &Path) -> Result<(), String> {
    println!("downloading {}", args.url);
    download(&args.url, archive)?;

    if let Some(pid) = args.wait_pid {
        println!("waiting for the game to close");
        wait_for_exit(pid, &args.launch)?;
    }

    println!("unpacking");
    unpack(archive, stage)?;
    let root = staged_root(stage);
    let exe_name = args.launch.file_name().ok_or("--launch has no file name")?;
    let staged_exe = root.join(exe_name);
    if !staged_exe.is_file() {
        return Err(format!("the archive holds no {}", exe_name.to_string_lossy()));
    }
    if let Some(expect) = &args.expect {
        let got = build_of(&staged_exe)?;
        if got != *expect {
            return Err(format!(
                "the download is still the previous build (its build is {got}, the server's is {expect}). \
                 The desktop packages go up a few minutes after the server; try again shortly."
            ));
        }
    }

    println!("installing into {}", args.dir.display());
    install(&root, &args.dir)
}

fn download(url: &str, to: &Path) -> Result<(), String> {
    let res = ureq::get(url).call().map_err(|e| format!("GET {url}: {e}"))?;
    let total = res.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|s| s.parse::<u64>().ok());
    let mut reader = res.into_body().into_reader();
    let mut file = File::create(to).map_err(|e| format!("create {}: {e}", to.display()))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    let mut next_report = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("download: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("write {}: {e}", to.display()))?;
        got += n as u64;
        if got >= next_report {
            match total {
                Some(t) => println!("  {} / {}", mb(got), mb(t)),
                None => println!("  {}", mb(got)),
            }
            next_report = got + 4 * 1024 * 1024;
        }
    }
    file.flush().map_err(|e| format!("write {}: {e}", to.display()))?;
    if let Some(t) = total
        && got != t
    {
        return Err(format!("download ended early: {} of {}", mb(got), mb(t)));
    }
    println!("  downloaded {}", mb(got));
    Ok(())
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

fn wait_for_exit(pid: u32, exe: &Path) -> Result<(), String> {
    let start = Instant::now();
    while process_alive(pid, exe) {
        if start.elapsed() > EXIT_TIMEOUT {
            return Err(format!("the game did not close within {} s; close it and update again", EXIT_TIMEOUT.as_secs()));
        }
        sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// Windows keeps a running executable locked against writing, which is exactly the condition that
/// matters here; Linux exposes the process itself.
#[cfg(windows)]
fn process_alive(_pid: u32, exe: &Path) -> bool {
    fs::OpenOptions::new().write(true).open(exe).is_err()
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32, _exe: &Path) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn process_alive(_pid: u32, _exe: &Path) -> bool {
    sleep(Duration::from_secs(2));
    false
}

/// The archive kind is read off the file, not the URL (`/download/<platform>` has no extension).
fn unpack(archive: &Path, stage: &Path) -> Result<(), String> {
    let mut file = File::open(archive).map_err(|e| format!("open {}: {e}", archive.display()))?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).map_err(|_| "the download is empty".to_string())?;
    io::Seek::seek(&mut file, io::SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    fs::create_dir_all(stage).map_err(|e| format!("create {}: {e}", stage.display()))?;
    if magic.starts_with(b"PK") {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("zip: {e}"))?;
        zip.extract(stage).map_err(|e| format!("unzip: {e}"))?;
    } else if magic.starts_with(&[0x1f, 0x8b]) {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        tar.unpack(stage).map_err(|e| format!("untar: {e}"))?;
    } else {
        return Err("the download is neither a zip nor a tar.gz (is the site serving an error page?)".into());
    }
    Ok(())
}

/// The packages wrap everything in one `endif/` directory; tolerate a flat archive too.
fn staged_root(stage: &Path) -> PathBuf {
    let entries: Vec<PathBuf> = fs::read_dir(stage).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    match entries.as_slice() {
        [only] if only.is_dir() => only.clone(),
        _ => stage.to_path_buf(),
    }
}

/// Asks a game executable which protocol it speaks (`endif --protocol` prints it and exits).
fn build_of(exe: &Path) -> Result<String, String> {
    let out = Command::new(exe)
        .arg("--build-id")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("run {} --build-id: {e}", exe.display()))?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || s.is_empty() {
        return Err(format!("{} --build-id gave no answer (exit {})", exe.display(), out.status));
    }
    Ok(s)
}

fn install(root: &Path, dir: &Path) -> Result<(), String> {
    // The asset directory ships whole: drop the old one so files that no longer exist do not linger.
    if root.join("assets").is_dir() && dir.join("assets").is_dir() {
        fs::remove_dir_all(dir.join("assets")).map_err(|e| format!("remove old assets: {e}"))?;
    }
    move_tree(root, dir)
}

fn move_tree(src: &Path, dst: &Path) -> Result<(), String> {
    for entry in fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))?.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            fs::create_dir_all(&to).map_err(|e| format!("create {}: {e}", to.display()))?;
            move_tree(&from, &to)?;
        } else {
            rename_retry(&from, &to)?;
        }
    }
    Ok(())
}

/// Windows can hold a just-exited program's file for a moment (antivirus, the console host).
fn rename_retry(from: &Path, to: &Path) -> Result<(), String> {
    let mut last = None;
    for _ in 0..25 {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                sleep(Duration::from_millis(200));
            }
        }
    }
    Err(format!("replace {}: {}", to.display(), last.map(|e| e.to_string()).unwrap_or_default()))
}

fn relaunch(args: &Args) -> Result<(), String> {
    Command::new(&args.launch).current_dir(&args.dir).spawn().map_err(|e| format!("{}: {e}", args.launch.display()))?;
    Ok(())
}

/// Keeps the console window up so a failure can be read.
fn pause() {
    print!("press Enter to close this window ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
}
