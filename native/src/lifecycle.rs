//! Shared launchd lifecycle operations; installation owns artifact staging separately.
use crate::service::LABEL;
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeSet, path::Path, process::Command, time::Duration};

/// Serialize app installation and manual launchd changes across CLI processes.
/// Failing promptly avoids a menu command queue waiting behind a long install.
pub(crate) struct OperationLock(std::fs::File);
impl OperationLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        ensure!(
            file.metadata()?.is_file(),
            "lifecycle lock is not a regular file"
        );
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            ensure!(
                error.kind() != std::io::ErrorKind::WouldBlock,
                "another Jaso NFC installation or lifecycle command is in progress; retry when it finishes"
            );
            return Err(error.into());
        }
        Ok(Self(file))
    }
}
impl Drop for OperationLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

pub(crate) fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

pub(crate) fn launch(args: &[&str]) -> Result<std::process::Output> {
    Ok(Command::new("/bin/launchctl").args(args).output()?)
}
pub(crate) fn stop_label(label: &str) -> Result<()> {
    stop_label_with(&mut NativeLaunchHost, label)
}
pub(crate) fn stop_label_with(host: &mut impl LaunchHost, label: &str) -> Result<()> {
    host_stop(host, label, &mut BTreeSet::new())
}

pub(crate) fn start_with(host: &mut impl LaunchHost, plist: &Path) -> Result<()> {
    ensure!(plist.exists(), "install the application first");
    let out = host.launch(&["print-disabled", &domain()])?;
    ensure!(out.status.success(), "cannot inspect login startup state");
    let disabled = disabled_from_output(&String::from_utf8_lossy(&out.stdout), LABEL);
    if disabled {
        host_checked(host, &["enable", &format!("{}/{LABEL}", domain())])?;
    }
    let started = (|| {
        if host_loaded(host, LABEL)? {
            host_checked(host, &["kickstart", &format!("{}/{LABEL}", domain())])
        } else {
            host_checked(
                host,
                &[
                    "bootstrap",
                    &domain(),
                    plist.to_str().context("invalid job path")?,
                ],
            )
        }
    })();
    let restored = if disabled {
        host_checked(host, &["disable", &format!("{}/{LABEL}", domain())])
    } else {
        Ok(())
    };
    match (started, restored) {
        (Err(start), Err(restore)) => {
            anyhow::bail!("{start:#}; restoring login setting also failed: {restore:#}")
        }
        (Err(error), _) | (_, Err(error)) => Err(error),
        _ => Ok(()),
    }
}
pub(crate) fn restart_with(host: &mut impl LaunchHost, plist: &Path) -> Result<()> {
    ensure!(plist.exists(), "install the application first");
    stop_label_with(host, LABEL)?;
    start_with(host, plist)
}

pub(crate) fn startup_with(
    host: &mut impl LaunchHost,
    jobs: &[(&str, &Path)],
    mode: &str,
) -> Result<serde_json::Value> {
    ensure!(
        jobs.len() == 1 && jobs[0].0 == LABEL,
        "expected the single worker login job"
    );
    ensure!(
        matches!(mode, "on" | "off" | "status"),
        "invalid startup mode"
    );
    let before = startup_disabled(host, jobs)?;
    let disabled = if mode == "status" {
        before
    } else {
        ensure!(
            jobs.iter().all(|(_, path)| path.is_file()),
            "install the application first"
        );
        let applied = (|| -> Result<Vec<bool>> {
            for (label, _) in jobs {
                set_disabled(host, label, mode == "off")?;
            }
            let after = startup_disabled(host, jobs)?;
            ensure!(
                after.iter().all(|disabled| *disabled == (mode == "off")),
                "login startup update was not confirmed"
            );
            Ok(after)
        })();
        match applied {
            Ok(after) => after,
            Err(error) => {
                let mut failures = Vec::new();
                for ((label, _), disabled) in jobs.iter().zip(before) {
                    if let Err(restore) = set_disabled(host, label, disabled) {
                        failures.push(format!("{label}: {restore:#}"));
                    }
                }
                ensure!(
                    failures.is_empty(),
                    "{error:#}; restoring login settings also failed: {}",
                    failures.join("; ")
                );
                return Err(error);
            }
        }
    };
    let worker_enabled = jobs[0].1.is_file() && !disabled[0];
    // These two legacy fields remain aliases of the one shared login setting.
    let menu_enabled = worker_enabled;
    Ok(serde_json::json!({
        "enabled":worker_enabled && menu_enabled,
        "installed":jobs.iter().all(|(_, path)| path.is_file()),
        "worker_enabled":worker_enabled,
        "menu_enabled":menu_enabled,
        "consistent":worker_enabled == menu_enabled
    }))
}

fn set_disabled(host: &mut impl LaunchHost, label: &str, disabled: bool) -> Result<()> {
    host_checked(
        host,
        &[
            if disabled { "disable" } else { "enable" },
            &format!("{}/{label}", domain()),
        ],
    )
}

fn startup_disabled(host: &mut impl LaunchHost, jobs: &[(&str, &Path)]) -> Result<Vec<bool>> {
    let out = host.launch(&["print-disabled", &domain()])?;
    ensure!(out.status.success(), "cannot read login startup state");
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(jobs
        .iter()
        .map(|(label, _)| disabled_from_output(&text, label))
        .collect())
}

pub(crate) trait LaunchHost {
    fn launch(&mut self, args: &[&str]) -> Result<std::process::Output> {
        launch(args)
    }
    fn wait_for_job_removal(&mut self) {
        std::thread::sleep(Duration::from_millis(100));
    }
}
pub(crate) struct NativeLaunchHost;
impl LaunchHost for NativeLaunchHost {}

pub(crate) fn host_checked(host: &mut impl LaunchHost, args: &[&str]) -> Result<()> {
    let out = host.launch(args)?;
    ensure!(
        out.status.success(),
        "launchctl {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}
pub(crate) fn host_stop(
    host: &mut impl LaunchHost,
    label: &str,
    pending_removals: &mut BTreeSet<String>,
) -> Result<()> {
    let out = host.launch(&["bootout", &format!("{}/{label}", domain())])?;
    ensure!(
        matches!(out.status.code(), Some(0 | 3 | 113)),
        "could not stop {label}: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    pending_removals.insert(label.to_owned());
    wait_for_job_absence(host, label)?;
    pending_removals.remove(label);
    Ok(())
}
pub(crate) fn wait_for_job_absence(host: &mut impl LaunchHost, label: &str) -> Result<()> {
    // A successful bootout can leave a registration visible during teardown.
    // Neither a following CLI start nor installer rollback may reuse that
    // departing registration as a successfully started or restored job.
    for attempt in 0..150 {
        if !host_loaded(host, label)? {
            return Ok(());
        }
        if attempt < 149 {
            host.wait_for_job_removal();
        }
    }
    anyhow::bail!("timed out waiting for launchctl to remove {label}")
}

pub(crate) fn host_loaded(host: &mut impl LaunchHost, label: &str) -> Result<bool> {
    let out = host.launch(&["print", &format!("{}/{label}", domain())])?;
    if out.status.success() {
        return Ok(true);
    }
    ensure!(
        matches!(out.status.code(), Some(3 | 113)),
        "cannot inspect {label}: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(false)
}
pub(crate) fn disabled_from_output(output: &str, label: &str) -> bool {
    output.lines().any(|line| {
        line.split_once("=>").is_some_and(|(name, value)| {
            name.trim() == format!("\"{label}\"")
                && matches!(value.trim().trim_end_matches(';'), "true" | "disabled")
        })
    })
}

#[cfg(test)]
mod operation_lock_tests {
    use super::*;

    #[test]
    fn concurrent_lifecycle_operations_are_excluded_and_release_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lifecycle.lock");
        let first = OperationLock::acquire(&path).unwrap();
        let result = OperationLock::acquire(&path);
        assert!(
            result.is_err(),
            "a second lifecycle command must not interleave"
        );
        drop(first);
        assert!(OperationLock::acquire(&path).is_ok());
    }

    #[test]
    fn lifecycle_lock_does_not_follow_symlinks_or_accept_fifo() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("unrelated");
        std::fs::write(&target, b"unchanged").unwrap();
        let link = dir.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(OperationLock::acquire(&link).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
        let fifo = dir.path().join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(OperationLock::acquire(&fifo).is_err());
    }
}
