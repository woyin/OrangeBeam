//! Single running instance. Two copies (for example /Applications and a
//! development build) would both read the remote, draw different effects at
//! once and interleave HID++ requests; the second could even adopt the first's
//! live diversion as a "leftover". An exclusive flock on a file in the
//! support directory prevents that. The kernel drops the lock when the process
//! exits for any reason, so a crash never leaves the app unable to start.
use std::fs::File;
use std::path::Path;

/// Holds the lock for as long as it lives.
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// Some(lock) when this is the only instance, None when another holds it.
    #[cfg(unix)]
    pub fn acquire(path: &Path) -> std::io::Result<Option<Self>> {
        use std::os::fd::AsRawFd;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        // SAFETY: flock on a descriptor owned by `file` for the call's duration.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Some(Self { _file: file }));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error)
        }
    }

    /// The app is macOS-only; other platforms never run two GUI instances.
    #[cfg(not(unix))]
    pub fn acquire(path: &Path) -> std::io::Result<Option<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Some(Self {
            _file: File::create(path)?,
        }))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn second_holder_is_refused_until_the_first_releases() {
        let path = std::env::temp_dir()
            .join(format!(
                "orange-beam-lock-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("instance.lock");
        let first = InstanceLock::acquire(&path).unwrap();
        assert!(first.is_some());
        // A separate open file description conflicts, as another process would.
        assert!(InstanceLock::acquire(&path).unwrap().is_none());
        drop(first);
        assert!(InstanceLock::acquire(&path).unwrap().is_some());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
