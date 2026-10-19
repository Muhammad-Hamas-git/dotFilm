//! Finding and launching the ffmpeg/ffprobe executables.
//!
//! Lookup order: next to cutroom.exe, then the system PATH.
//! On Windows, child processes are started without a console window.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn exe_name(tool: &str) -> String {
    if cfg!(windows) { format!("{tool}.exe") } else { tool.to_string() }
}

fn resolve(tool: &str) -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let local = dir.join(exe_name(tool));
            if local.exists() {
                return local;
            }
        }
    }
    PathBuf::from(exe_name(tool))
}

/// A Command for `ffmpeg` or `ffprobe` with no stdin and no console window.
pub fn command(tool: &str) -> Command {
    let mut cmd = Command::new(resolve(tool));
    cmd.stdin(Stdio::null());
    hide_window(&mut cmd);
    cmd
}

#[cfg(windows)]
fn hide_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_cmd: &mut Command) {}

/// True if both ffmpeg and ffprobe can be started.
pub fn available() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|t| {
        command(t)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}
