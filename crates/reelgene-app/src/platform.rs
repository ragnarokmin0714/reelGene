//! The few things that differ between Windows and Linux. Every Windows call
//! here is a plain Win32 function with no state, each with a SAFETY note.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A program on PATH, searching absolute entries only: never the current
/// directory, which is how a planted binary would get run.
pub fn which(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
}

/// Start a helper program and forget it; a thread reaps it so no zombie stays.
pub fn spawn_detached(mut cmd: Command) {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Ok(mut child) = cmd.spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Show a file selected in its folder, or open a folder, in the file manager.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let Some(root) = std::env::var_os("SystemRoot").map(PathBuf::from) else {
            return;
        };
        if !root.is_absolute() {
            return;
        }
        let mut cmd = Command::new(root.join("explorer.exe"));
        if path.is_file() {
            // Explorer wants exactly /select,"C:\path"; normal quoting confuses it.
            cmd.raw_arg(format!("/select,\"{}\"", path.display()));
        } else {
            cmd.arg(path);
        }
        spawn_detached(cmd);
    }
    #[cfg(not(windows))]
    {
        let dir = if path.is_file() {
            path.parent().unwrap_or(path)
        } else {
            path
        };
        if let Some(open) = which("xdg-open") {
            let mut cmd = Command::new(open);
            cmd.arg(dir);
            spawn_detached(cmd);
        }
    }
}

/// A Windows GUI program has no console. Started from one with arguments
/// (the CLI), attach to it so output shows up; harmless otherwise.
pub fn attach_console() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        // SAFETY: no pointers; fails harmlessly when there is no parent console
        // or the process already has one.
        #[allow(unsafe_code)]
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// `zh-TW`, `en-US`, … from the OS, if it says.
pub fn system_language() -> Option<String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
        const LOCALE_NAME_MAX_LENGTH: usize = 85;
        let mut buf = [0u16; LOCALE_NAME_MAX_LENGTH];
        // SAFETY: the buffer is valid for the length passed; the call writes at
        // most that many UTF-16 units including the terminator.
        #[allow(unsafe_code)]
        let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        let n = usize::try_from(n).ok().filter(|n| *n > 1)?;
        Some(String::from_utf16_lossy(&buf[..n - 1]))
    }
    #[cfg(not(windows))]
    {
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
    }
}

/// Whether the user asked the system for less motion: Windows' "Animation
/// effects" off, or GNOME's `enable-animations` false.
pub fn reduced_motion() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
        };
        let mut enabled: i32 = 1;
        // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL through the pointer,
        // which points at a live i32.
        #[allow(unsafe_code)]
        let ok = unsafe {
            SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0)
        };
        ok != 0 && enabled == 0
    }
    #[cfg(not(windows))]
    {
        let Some(gsettings) = which("gsettings") else {
            return false;
        };
        let mut cmd = Command::new(gsettings);
        cmd.args(["get", "org.gnome.desktop.interface", "enable-animations"]);
        output_within(cmd, Duration::from_secs(1)).is_some_and(|out| out.trim() == "false")
    }
}

/// A helper's stdout, unless it takes longer than `limit`.
#[cfg_attr(windows, allow(dead_code))]
fn output_within(mut cmd: Command, limit: Duration) -> Option<String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_ignores_relative_path_entries() {
        // Whatever the machine has, the result is never relative.
        if let Some(p) = which("sh") {
            assert!(p.is_absolute());
        }
        assert_eq!(which("reelgene-no-such-program"), None);
    }

    #[cfg(unix)]
    #[test]
    fn slow_helpers_are_cut_off() {
        let Some(sleep) = which("sleep") else { return };
        let mut cmd = Command::new(sleep);
        cmd.arg("5");
        let start = Instant::now();
        assert_eq!(output_within(cmd, Duration::from_millis(100)), None);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
