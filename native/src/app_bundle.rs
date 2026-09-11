//! The Rust executable is the app's main executable as well as its CLI worker.
//! Only an argument-free bundled launch replaces it with the native menu.

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{path::Path, process::Command};

pub const IDENTIFIER: &str = "io.github.garlicvread.jaso-nfc";
pub const MAIN_EXECUTABLE: &str = "jaso-nfc";
pub const GUI_EXECUTABLE: &str = "Jaso NFC";

/// Check the shared installation and app-launch metadata contract.
pub fn validate_metadata(app: &Path) -> Result<()> {
    let output = Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-", "--"])
        .arg(app.join("Contents/Info.plist"))
        .output()
        .context("cannot read application metadata")?;
    ensure!(
        output.status.success(),
        "invalid application metadata: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let metadata: Value =
        serde_json::from_slice(&output.stdout).context("invalid application metadata")?;
    for (key, expected) in [
        ("CFBundleIdentifier", IDENTIFIER),
        ("CFBundleExecutable", MAIN_EXECUTABLE),
        ("CFBundlePackageType", "APPL"),
    ] {
        ensure!(
            metadata.get(key).and_then(Value::as_str) == Some(expected),
            "application {key} must be {expected}"
        );
    }
    Ok(())
}

/// Returns only for an unbundled executable or an error; a valid app uses exec.
#[cfg(target_os = "macos")]
pub fn dispatch_gui_if_bundled() -> Result<()> {
    use std::os::unix::process::CommandExt;

    let executable = std::env::current_exe().context("cannot locate app executable")?;
    let Some(macos) = executable.parent() else {
        return Ok(());
    };
    let Some(contents) = macos.parent() else {
        return Ok(());
    };
    let Some(app) = contents.parent() else {
        return Ok(());
    };
    if executable
        .file_name()
        .is_none_or(|name| name != MAIN_EXECUTABLE)
        || macos.file_name().is_none_or(|name| name != "MacOS")
        || contents.file_name().is_none_or(|name| name != "Contents")
        || app.extension().is_none_or(|extension| extension != "app")
    {
        return Ok(());
    }
    validate_metadata(app)?;
    let gui = macos.join(GUI_EXECUTABLE);
    validate_native_gui(&gui)?;
    // Preserve the PID and application bundle identity without a resident wrapper.
    // CLI arguments never reach this path and never initialize AppKit.
    Err(Command::new(&gui).exec()).context("cannot execute native GUI")
}

#[cfg(target_os = "macos")]
pub(crate) fn validate_native_gui(gui: &Path) -> Result<()> {
    use std::{
        fs::OpenOptions,
        io::Read,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };

    // O_NOFOLLOW rejects a redirected sibling and O_NONBLOCK prevents a malformed
    // bundle containing a FIFO from hanging before the regular-file check.
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(gui)
        .context("cannot open native GUI")?;
    let metadata = file.metadata().context("cannot inspect native GUI")?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        "native GUI must be an executable regular file"
    );
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .context("cannot read native GUI executable header")?;
    ensure!(
        matches!(
            u32::from_be_bytes(magic),
            0xfeedface
                | 0xcefaedfe
                | 0xfeedfacf
                | 0xcffaedfe
                | 0xcafebabe
                | 0xbebafeca
                | 0xcafebabf
                | 0xbfbafeca
        ),
        "native GUI must be a Mach-O executable, not a script"
    );
    Ok(())
}
