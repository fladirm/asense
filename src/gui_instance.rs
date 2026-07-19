use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SOCKET_PREFIX: &str = "com.github.fladirm.asense.gui.v1.";
const TERM_TIMEOUT: Duration = Duration::from_secs(4);
const KILL_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Open,
    Toggle,
}

/// The bound socket is the GUI lease.  It must stay alive until the desktop
/// event loop returns; only its owner is allowed to enter `app::launch`.
pub struct GuiLease {
    _listener: UnixListener,
    stop_drain: Arc<AtomicBool>,
    drain_thread: Option<JoinHandle<()>>,
}

impl GuiLease {
    fn bind(address: &SocketAddr) -> io::Result<Self> {
        let listener = UnixListener::bind_addr(address)?;
        listener.set_nonblocking(true)?;
        let drain_listener = listener.try_clone()?;
        let stop_drain = Arc::new(AtomicBool::new(false));
        let drain_flag = Arc::clone(&stop_drain);
        let drain_thread = thread::Builder::new()
            .name("asense-gui-lease".to_string())
            .spawn(move || drain_connections(drain_listener, drain_flag))?;

        Ok(Self {
            _listener: listener,
            stop_drain,
            drain_thread: Some(drain_thread),
        })
    }
}

impl Drop for GuiLease {
    fn drop(&mut self) {
        self.stop_drain.store(true, Ordering::Release);
        if let Some(thread) = self.drain_thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }

        // Field drop now releases the primary descriptor.  The clone owned by
        // the joined drain thread has already closed.
    }
}

pub fn claim(mode: Mode) -> Result<Option<GuiLease>, String> {
    let address = socket_address(effective_uid())
        .map_err(|error| format!("cannot create the GUI lease address: {error}"))?;
    claim_with(&address, mode, terminate_owner)
        .map_err(|error| format!("cannot coordinate the ASense window: {error}"))
}

fn socket_address(uid: u32) -> io::Result<SocketAddr> {
    SocketAddr::from_abstract_name(format!("{SOCKET_PREFIX}{uid}").as_bytes())
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

fn claim_with<F>(address: &SocketAddr, mode: Mode, terminate: F) -> io::Result<Option<GuiLease>>
where
    F: FnOnce(&UnixStream) -> io::Result<()>,
{
    if let Some(lease) = try_bind(address)? {
        return Ok(Some(lease));
    }

    let owner = match UnixStream::connect_addr(address) {
        Ok(owner) => owner,
        Err(error) if owner_disappeared(&error) => {
            return match mode {
                // The toggle observed an owner, so a concurrent natural exit
                // still counts as the close half of the toggle operation.
                Mode::Toggle => Ok(None),
                Mode::Open => try_bind(address),
            };
        }
        Err(error) => return Err(error),
    };

    terminate(&owner)?;
    if mode == Mode::Toggle {
        return Ok(None);
    }

    // Another simultaneous opener may win this bind.  In that case its new
    // GUI satisfies all of the coalesced open requests, so do not terminate it
    // and queue another replacement.
    try_bind(address)
}

fn try_bind(address: &SocketAddr) -> io::Result<Option<GuiLease>> {
    match GuiLease::bind(address) {
        Ok(lease) => Ok(Some(lease)),
        Err(error) if error.raw_os_error() == Some(libc::EADDRINUSE) => Ok(None),
        Err(error) => Err(error),
    }
}

fn owner_disappeared(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ECONNREFUSED | libc::ENOENT)
    )
}

fn drain_connections(listener: UnixListener, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Acquire) {
        loop {
            match listener.accept() {
                // Peer credentials are fixed when connect(2) succeeds, so the
                // claimant can still inspect them after this accepted end is
                // closed.  Accepting prevents abandoned requests from filling
                // the listener backlog over the lifetime of the GUI.
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            }
        }
        thread::park_timeout(DRAIN_INTERVAL);
    }
}

fn terminate_owner(stream: &UnixStream) -> io::Result<()> {
    let peer = peer_credentials(stream)?;
    if peer.uid != effective_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the GUI lease belongs to a different user",
        ));
    }
    if peer.pid <= 0 || peer.pid == std::process::id() as i32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the GUI lease reported an invalid owner",
        ));
    }

    let pidfd = peer_pidfd(stream)?;

    send_pidfd_signal(&pidfd, libc::SIGTERM)?;
    if wait_for_exit(&pidfd, TERM_TIMEOUT)? {
        return Ok(());
    }

    // SIGKILL is deliberately only an escalation for an owner that ignored or
    // could not complete SIGTERM within the grace period.
    send_pidfd_signal(&pidfd, libc::SIGKILL)?;
    if wait_for_exit(&pidfd, KILL_TIMEOUT)? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the previous GUI did not exit after SIGTERM and SIGKILL",
        ))
    }
}

#[derive(Debug, Eq, PartialEq)]
struct PeerCredentials {
    pid: i32,
    uid: u32,
}

fn peer_credentials(stream: &UnixStream) -> io::Result<PeerCredentials> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: credentials points to writable storage of exactly `length`
    // bytes, and stream owns a valid Unix socket descriptor.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SO_PEERCRED returned a malformed value",
        ));
    }

    Ok(PeerCredentials {
        pid: credentials.pid,
        uid: credentials.uid,
    })
}

fn peer_pidfd(stream: &UnixStream) -> io::Result<OwnedFd> {
    let mut descriptor: libc::c_int = -1;
    let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SO_PEERPIDFD obtains the descriptor directly from the connected socket,
    // eliminating the PID lookup race on kernels that provide it.
    // SAFETY: descriptor is writable for `length` bytes and stream is valid.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERPIDFD,
            (&mut descriptor as *mut libc::c_int).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if descriptor < 0 || length as usize != std::mem::size_of::<libc::c_int>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SO_PEERPIDFD returned a malformed descriptor",
        ));
    }
    // SAFETY: a successful SO_PEERPIDFD call returned a new descriptor owned
    // by this process.  There is deliberately no PID-based fallback because
    // looking the peer PID up again would introduce a PID-reuse race.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn send_pidfd_signal(pidfd: &OwnedFd, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: pidfd is valid, signal is SIGTERM or SIGKILL, siginfo is null as
    // required for a normal process-directed signal, and flags must be zero.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0_u32,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

fn wait_for_exit(pidfd: &OwnedFd, timeout: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_ms = if remaining.is_zero() {
            0
        } else {
            remaining.as_millis().clamp(1, i32::MAX as u128) as i32
        };
        let mut descriptor = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor points to one initialized pollfd for the duration
        // of the call.
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result > 0 {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "pidfd became invalid while waiting for the GUI",
                ));
            }
            return Ok(true);
        }
        if result == 0 {
            return Ok(false);
        }

        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Barrier, Mutex};

    static NEXT_ADDRESS: AtomicU64 = AtomicU64::new(0);

    fn unique_address() -> SocketAddr {
        let sequence = NEXT_ADDRESS.fetch_add(1, Ordering::Relaxed);
        SocketAddr::from_abstract_name(
            format!("asense-gui-test.{}.{sequence}", std::process::id()).as_bytes(),
        )
        .expect("test address should fit in sockaddr_un")
    }

    #[test]
    fn lease_is_exclusive_and_released_on_drop() {
        let address = unique_address();
        let first = try_bind(&address).unwrap().unwrap();
        assert!(try_bind(&address).unwrap().is_none());
        drop(first);
        assert!(try_bind(&address).unwrap().is_some());
    }

    #[test]
    fn free_toggle_claims_the_lease() {
        let address = unique_address();
        let lease = claim_with(&address, Mode::Toggle, |_| {
            panic!("a free toggle must not contact an owner")
        })
        .unwrap();
        assert!(lease.is_some());
    }

    #[test]
    fn occupied_toggle_stops_without_reacquiring() {
        let address = unique_address();
        let old = Mutex::new(try_bind(&address).unwrap());

        let result = claim_with(&address, Mode::Toggle, |_| {
            drop(old.lock().unwrap().take());
            Ok(())
        })
        .unwrap();

        assert!(result.is_none());
        assert!(try_bind(&address).unwrap().is_some());
    }

    #[test]
    fn occupied_open_replaces_and_holds_the_lease() {
        let address = unique_address();
        let old = Mutex::new(try_bind(&address).unwrap());

        let replacement = claim_with(&address, Mode::Open, |_| {
            drop(old.lock().unwrap().take());
            Ok(())
        })
        .unwrap()
        .unwrap();

        assert!(try_bind(&address).unwrap().is_none());
        drop(replacement);
        assert!(try_bind(&address).unwrap().is_some());
    }

    #[test]
    fn simultaneous_openers_coalesce_to_one_replacement() {
        let address = unique_address();
        let old = Arc::new(Mutex::new(try_bind(&address).unwrap()));
        let connected = Arc::new(Barrier::new(2));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let address = address.clone();
                let old = Arc::clone(&old);
                let connected = Arc::clone(&connected);
                thread::spawn(move || {
                    claim_with(&address, Mode::Open, |_| {
                        connected.wait();
                        let lease = old.lock().unwrap().take();
                        drop(lease);
                        Ok(())
                    })
                })
            })
            .collect();

        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|lease| lease.is_some()).count(), 1);
    }

    #[test]
    fn kernel_reports_the_lease_owner_and_pidfd() {
        let address = unique_address();
        let _lease = try_bind(&address).unwrap().unwrap();
        let stream = UnixStream::connect_addr(&address).unwrap();
        let peer = peer_credentials(&stream).unwrap();

        assert_eq!(peer.uid, effective_uid());
        assert_eq!(peer.pid, std::process::id() as i32);
        let pidfd = peer_pidfd(&stream).unwrap();
        assert!(!wait_for_exit(&pidfd, Duration::ZERO).unwrap());
    }
}
