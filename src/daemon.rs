//! Headless engine that keeps downloads going after the TUI quits.
//!
//! The TUI hands off to it by stopping its own embedded engine (which frees
//! the API port and flushes session state) and spawning `torflix --daemon`
//! detached. The next `torflix` finds the engine already listening and simply
//! connects to it. The daemon exits by itself once nothing is left to download,
//! or when asked to via a stop file (`torflix --stop`, or `Q` in the TUI).

use crate::rqbit::{self, Client};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn state_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("torflix")
}

fn pid_file() -> PathBuf {
    state_dir().join("daemon.pid")
}

fn stop_file() -> PathBuf {
    state_dir().join("daemon.stop")
}

/// True when a background engine started by torflix is recorded as running.
pub fn is_running() -> bool {
    pid_file().is_file()
}

/// Remove leftovers from a daemon that died without cleaning up. Call only once
/// it's established that no engine is actually listening.
pub fn clear_stale() {
    std::fs::remove_file(pid_file()).ok();
    std::fs::remove_file(stop_file()).ok();
}

/// Launch `torflix --daemon` fully detached from this terminal.
pub fn spawn_detached() -> Result<()> {
    let exe = std::env::current_exe().context("locating the torflix binary")?;
    let mut cmd = Command::new(exe);
    // TORFLIX_DOWNLOAD_DIR / TORFLIX_RQBIT_URL are inherited from our environment.
    cmd.arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group, so closing the terminal (SIGHUP to its group) doesn't kill it.
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn().context("starting the background engine")?;
    Ok(())
}

/// Entry point for `torflix --daemon`.
pub fn run(download_dir: &Path, api_url: &str) -> Result<()> {
    std::fs::create_dir_all(state_dir()).ok();
    std::fs::remove_file(stop_file()).ok();
    let client = Client::new(api_url);

    // The TUI that launched us may still be releasing the port, so retry briefly.
    let mut engine = None;
    let mut last_err = None;
    for _ in 0..40 {
        if !client.is_up() {
            match rqbit::start_embedded_engine(download_dir, api_url) {
                Ok(e) => {
                    engine = Some(e);
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let Some(mut engine) = engine else {
        return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("another engine is already on {}", api_url)));
    };
    std::fs::write(pid_file(), std::process::id().to_string()).ok();
    rqbit::forget_temp_torrents(&client);

    let started = Instant::now();
    loop {
        std::thread::sleep(Duration::from_secs(2));
        if stop_file().exists() {
            break;
        }
        let Ok(list) = client.list() else { break };
        // Restored torrents report "initializing" for a moment; don't judge them yet.
        if started.elapsed() < Duration::from_secs(15) {
            continue;
        }
        // Exit once nothing is actively downloading, rather than seeding forever.
        // Paused and errored torrents can't make progress on their own, so they don't keep us alive.
        let busy = list.iter().any(|t| match client.stats(t.id) {
            Ok(s) => !s.finished && s.state != "paused" && s.state != "error",
            Err(_) => false,
        });
        if !busy {
            break;
        }
    }

    engine.stop();
    clear_stale();
    Ok(())
}

/// Ask the background engine to shut down, waiting briefly for it to go.
/// Returns false if there was no torflix daemon to stop.
pub fn request_stop() -> bool {
    if !is_running() {
        return false;
    }
    std::fs::write(stop_file(), b"").ok();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !pid_file().exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // It never acknowledged — most likely it had already died. Don't leave state behind.
    clear_stale();
    true
}
