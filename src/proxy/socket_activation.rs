//! Platform-specific socket activation support.
//!
//! On macOS: uses `launch_activate_socket` to receive fds from launchd.
//! On Linux: reads `LISTEN_FDS`/`LISTEN_PID` env vars per sd_listen_fds(3).
//! Returns `None` when no activated sockets are available (fallback to bind).

use anyhow::{Context, Result, bail};
use std::os::unix::io::{FromRawFd, RawFd};
use tokio::net::TcpListener;

/// Attempt to acquire a pre-bound TCP listener from the OS socket activation
/// mechanism. Returns `Ok(None)` if socket activation is not active (caller
/// should fall back to `TcpListener::bind()`).
///
/// Respects `DEVPROXY_NO_SOCKET_ACTIVATION`: when set, skips all fd
/// acquisition and returns `Ok(None)` immediately. This prevents tests
/// from accidentally picking up unrelated file descriptors (e.g. stale
/// `LISTEN_FDS` in the environment, or launchd fds meant for another service).
pub async fn acquire_listener() -> Result<Option<TcpListener>> {
    if std::env::var("DEVPROXY_NO_SOCKET_ACTIVATION").is_ok() {
        return Ok(None);
    }

    match acquire_activated_fds()? {
        Some(fds) if !fds.is_empty() => {
            let fd = fds[0];
            // Safety: the fd is passed to us by launchd/systemd and is a valid
            // bound TCP socket. We take ownership (no dup needed — the OS
            // expects us to consume it).
            let std_listener = unsafe { std::net::TcpListener::from_raw_fd(fd) };
            std_listener
                .set_nonblocking(true)
                .context("failed to set activated socket to non-blocking")?;
            let listener = TcpListener::from_std(std_listener)
                .context("failed to convert activated socket to tokio TcpListener")?;
            Ok(Some(listener))
        }
        _ => Ok(None),
    }
}

/// Low-level: get raw fds from the platform's socket activation mechanism.
/// Returns Ok(None) if socket activation is not in use.
fn acquire_activated_fds() -> Result<Option<Vec<RawFd>>> {
    #[cfg(target_os = "macos")]
    {
        acquire_launchd_fds()
    }
    #[cfg(target_os = "linux")]
    {
        acquire_systemd_fds()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
fn acquire_launchd_fds() -> Result<Option<Vec<RawFd>>> {
    use std::ffi::CString;
    use std::os::raw::c_int;

    // launch_activate_socket is declared in <launch.h>:
    //   int launch_activate_socket(const char *name, int **fds, size_t *cnt);
    // Returns 0 on success, non-zero errno on failure.
    // ESRCH (3) means "not managed by launchd" — this is the normal fallback case.
    unsafe extern "C" {
        fn launch_activate_socket(
            name: *const std::os::raw::c_char,
            fds: *mut *mut c_int,
            cnt: *mut usize,
        ) -> c_int;
    }

    let name = CString::new("Listeners").context("CString::new failed for socket name")?;
    let mut fds_ptr: *mut c_int = std::ptr::null_mut();
    let mut count: usize = 0;

    let ret = unsafe { launch_activate_socket(name.as_ptr(), &mut fds_ptr, &mut count) };

    if ret != 0 {
        // ESRCH means not launched by launchd — normal fallback
        if ret == libc::ESRCH {
            return Ok(None);
        }
        let err = std::io::Error::from_raw_os_error(ret);
        bail!("launch_activate_socket failed: {err}");
    }

    if fds_ptr.is_null() || count == 0 {
        return Ok(None);
    }

    let fds: Vec<RawFd> = unsafe {
        let slice = std::slice::from_raw_parts(fds_ptr, count);
        let v = slice.to_vec();
        // The fds array was malloc'd by launch_activate_socket; we must free it.
        libc::free(fds_ptr as *mut libc::c_void);
        v
    };

    Ok(Some(fds))
}

#[cfg(target_os = "linux")]
fn acquire_systemd_fds() -> Result<Option<Vec<RawFd>>> {
    // sd_listen_fds(3) protocol:
    // - LISTEN_PID must match our PID
    // - LISTEN_FDS is the count of fds starting at fd 3

    let listen_pid = match std::env::var("LISTEN_PID") {
        Ok(val) => val,
        Err(_) => return Ok(None),
    };

    let our_pid = std::process::id().to_string();
    if listen_pid != our_pid {
        return Ok(None);
    }

    let listen_fds: usize = std::env::var("LISTEN_FDS")
        .context("LISTEN_PID set but LISTEN_FDS missing")?
        .parse()
        .context("LISTEN_FDS is not a valid number")?;

    if listen_fds == 0 {
        return Ok(None);
    }

    // Note: we intentionally do NOT unset LISTEN_PID/LISTEN_FDS here.
    // Calling std::env::remove_var is unsafe in a multi-threaded process
    // (Rust 2024 edition), and the tokio runtime has already spawned worker
    // threads by this point. The env var leak is harmless — the daemon runs
    // for its entire lifetime and CLOEXEC on the fds prevents child processes
    // from inheriting them.

    const SD_LISTEN_FDS_START: RawFd = 3;

    // Guard against overflow: listen_fds could be absurdly large
    if listen_fds > (RawFd::MAX - SD_LISTEN_FDS_START) as usize {
        bail!("LISTEN_FDS value {listen_fds} is too large");
    }

    let fds: Vec<RawFd> =
        (SD_LISTEN_FDS_START..SD_LISTEN_FDS_START + listen_fds as RawFd).collect();

    // Set CLOEXEC on all fds so they don't leak to child processes
    for &fd in &fds {
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }

    Ok(Some(fds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acquire_activated_fds_returns_none_when_not_activated() {
        let result = acquire_activated_fds();
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_acquire_listener_returns_none_when_not_activated() {
        let result = acquire_listener().await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_acquire_listener_skips_when_socket_activation_disabled() {
        // Even if LISTEN_FDS were somehow set, the env var gate should
        // make acquire_listener() return None immediately.
        unsafe { std::env::set_var("DEVPROXY_NO_SOCKET_ACTIVATION", "1") };
        let result = acquire_listener().await;
        unsafe { std::env::remove_var("DEVPROXY_NO_SOCKET_ACTIVATION") };
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }
}
