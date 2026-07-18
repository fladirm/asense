//! Cross-process serialization for multi-controller firmware transactions.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

const MUTATION_LOCK: &str = "/run/asense-mutation.lock";

/// Held for the complete firmware mutation/readback transaction.
pub struct MutationGuard {
    _file: File,
}

impl MutationGuard {
    pub fn acquire() -> Result<Self, String> {
        Self::acquire_at(Path::new(MUTATION_LOCK))
    }

    fn acquire_at(path: &Path) -> Result<Self, String> {
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

        loop {
            // SAFETY: flock only consumes the valid descriptor owned by
            // `file`; the guard keeps it open for the critical section.
            let status = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if status == 0 {
                return Ok(Self { _file: file });
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(format!("cannot lock firmware mutation plane: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MutationGuard;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

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
}
