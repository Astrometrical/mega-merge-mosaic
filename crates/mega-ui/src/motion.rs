//! OS "reduce motion" preference probe.

/// Whether the OS asks for reduced motion. Blocks (may spawn a process);
/// call once, off the UI thread. Any failure gives `false`.
pub fn reduced_motion() -> bool {
    probe()
}

#[cfg(windows)]
fn probe() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
    };
    let mut on: i32 = 1;
    // SAFETY: the out pointer is a valid, writable BOOL for the call.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut on as *mut i32).cast(),
            0,
        )
    };
    ok != 0 && on == 0
}

#[cfg(target_os = "macos")]
fn probe() -> bool {
    command_says(
        "defaults",
        &["read", "com.apple.universalaccess", "reduceMotion"],
        "1",
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn probe() -> bool {
    command_says(
        "gsettings",
        &["get", "org.gnome.desktop.interface", "enable-animations"],
        "false",
    )
}

#[cfg(not(any(windows, unix)))]
fn probe() -> bool {
    false
}

/// Run `cmd` and report whether it succeeded printing exactly `expect`.
#[cfg(unix)]
fn command_says(cmd: &str, args: &[&str], expect: &str) -> bool {
    std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == expect)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_motion_probe_returns() {
        let _ = reduced_motion();
    }
}
