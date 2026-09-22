//! Signal handlers only set an atomic; restoration runs in the normal HID loop.
use std::sync::atomic::{AtomicI32, Ordering};

static STOP: AtomicI32 = AtomicI32::new(0);

extern "C" fn request_stop(signal: libc::c_int) {
    STOP.store(signal, Ordering::Relaxed);
}

pub struct StopSignals(Vec<(libc::c_int, libc::sigaction)>);

impl StopSignals {
    pub fn install() -> std::io::Result<Self> {
        STOP.store(0, Ordering::Relaxed);
        let mut guard = Self(Vec::new());
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: sigaction is a C POD. The handler only touches a lock-free
            // atomic, and old dispositions remain owned by this CLI-scoped guard.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                let mut previous: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = request_stop as *const () as usize;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, &mut previous) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                guard.0.push((signal, previous));
            }
        }
        Ok(guard)
    }

    pub fn received(&self) -> i32 {
        STOP.load(Ordering::Relaxed)
    }
}

impl Drop for StopSignals {
    fn drop(&mut self) {
        for (signal, previous) in self.0.iter().rev() {
            // SAFETY: restore the exact dispositions saved during installation.
            unsafe {
                libc::sigaction(*signal, previous, std::ptr::null_mut());
            }
        }
    }
}
