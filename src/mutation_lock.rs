//! Cross-process serialization for multi-controller firmware transactions.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

const MUTATION_LOCK: &str = "/run/asense-mutation.lock";
const MUTATION_LOCK_TIMEOUT: Duration = Duration::from_secs(3);
const MUTATION_LOCK_RETRY: Duration = Duration::from_millis(25);

/// Held for the complete firmware mutation/readback transaction.
pub struct MutationGuard {
    _file: File,
}

impl MutationGuard {
    pub fn acquire() -> Result<Self, String> {
        Self::acquire_at_with_timeout(Path::new(MUTATION_LOCK), MUTATION_LOCK_TIMEOUT)
    }

    #[cfg(test)]
    fn acquire_at(path: &Path) -> Result<Self, String> {
        Self::acquire_at_with_timeout(path, MUTATION_LOCK_TIMEOUT)
    }

    fn acquire_at_with_timeout(path: &Path, timeout: Duration) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| format!("cannot open firmware mutation lock: {error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("cannot inspect firmware mutation lock: {error}"))?;
        if !metadata.file_type().is_file()
            || metadata.uid() != 0
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o600
        {
            return Err("unsafe firmware mutation lock ownership or mode".to_string());
        }

        Self::lock_file_with_timeout(file, timeout)
    }

    fn lock_file_with_timeout(file: File, timeout: Duration) -> Result<Self, String> {
        let deadline = Instant::now() + timeout;
        loop {
            // SAFETY: flock only consumes the valid descriptor owned by
            // `file`; the guard keeps it open for the critical section.
            let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if status == 0 {
                return Ok(Self { _file: file });
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted if Instant::now() < deadline => continue,
                io::ErrorKind::WouldBlock if Instant::now() < deadline => {
                    thread::sleep(
                        MUTATION_LOCK_RETRY.min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => {
                    return Err(format!(
                        "firmware mutation plane stayed busy for {} ms",
                        timeout.as_millis()
                    ));
                }
                _ => return Err(format!("cannot lock firmware mutation plane: {error}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MutationGuard;
    use std::fs::{self, OpenOptions};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn rejects_a_preexisting_permissive_lock_file() {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("asense-mutation-lock-{}-{id}", std::process::id()));
        fs::write(&path, "").unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o666);
        fs::set_permissions(&path, permissions).unwrap();
        assert!(MutationGuard::acquire_at(&path).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn times_out_instead_of_blocking_forever_on_a_held_lock() {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("asense-mutation-lock-{}-{id}", std::process::id()));
        let first = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let held = MutationGuard::lock_file_with_timeout(first, Duration::from_millis(50)).unwrap();
        let started = Instant::now();
        let error = MutationGuard::lock_file_with_timeout(second, Duration::from_millis(50))
            .err()
            .expect("a separately opened descriptor must not bypass the held flock");
        assert!(error.contains("stayed busy"));
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(held);
        let third = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(MutationGuard::lock_file_with_timeout(third, Duration::from_millis(50)).is_ok());
        fs::remove_file(path).unwrap();
    }
}
