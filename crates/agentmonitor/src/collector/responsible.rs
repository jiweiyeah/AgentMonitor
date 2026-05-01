//! macOS "responsible process" lookup.
//!
//! Every userspace process on macOS carries a kernel-tracked **responsible
//! PID** that points back to the GUI app or terminal that originally launched
//! it (Activity Monitor's "Process Group" column). Unlike PPID, this pointer
//! survives parent-exit / re-parenting to launchd, so it correctly attributes
//! orphaned daemons to the terminal session that spawned them — exactly the
//! case where PPID has degenerated to 1 and tells you nothing.
//!
//! The underlying symbol `responsibility_get_pid_responsible_for_pid` lives in
//! `libSystem.dylib` and is technically private (no header in the SDK), but
//! Apple's own Activity Monitor, `launchctl procinfo`, and `taskinfo` all
//! depend on it. It's been stable since 10.10 and isn't going anywhere.
//!
//! On non-macOS targets every entry point is a no-op returning `None` so call
//! sites stay platform-agnostic.

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::CStr;
    use std::os::raw::{c_char, c_int};

    // libSystem exports both proc_* (documented in `libproc.h`) and the
    // private responsibility_* family. Linking by `name = "System"` pulls in
    // libSystem.B.dylib which re-exports both — same dylib Activity Monitor
    // and `taskinfo` use.
    #[link(name = "System", kind = "dylib")]
    extern "C" {
        fn responsibility_get_pid_responsible_for_pid(pid: c_int) -> c_int;
        fn proc_name(pid: c_int, buf: *mut c_char, size: u32) -> c_int;
        fn proc_pidpath(pid: c_int, buf: *mut c_char, size: u32) -> c_int;
    }

    /// Result of resolving a process's responsible/originator process.
    #[derive(Debug, Clone)]
    pub struct Responsible {
        pub pid: u32,
        pub name: String,
        pub path: String,
    }

    /// Look up the responsible (originating) process for `pid`.
    ///
    /// Returns `None` when:
    /// - `pid` has already exited,
    /// - the kernel refuses (rare; happens for some SIP-protected procs), or
    /// - we're not on macOS.
    ///
    /// Self-attribution (a top-level GUI app where `responsible == pid`) is
    /// passed through as-is — callers who want to skip the noise should
    /// compare `r.pid == queried_pid` themselves.
    pub fn for_pid(pid: u32) -> Option<Responsible> {
        let pid_i = pid as c_int;
        // SAFETY: FFI to libSystem. The function is a pure read of kernel
        // state (no allocation, no callbacks), and a negative return value
        // signals failure rather than an out-param error. We treat anything
        // < 0 as "no answer available" and bail.
        let rpid = unsafe { responsibility_get_pid_responsible_for_pid(pid_i) };
        if rpid < 0 {
            return None;
        }

        let mut name_buf = [0u8; 256];
        let mut path_buf = [0u8; 4096];

        // SAFETY: proc_name / proc_pidpath populate up to `size` bytes in the
        // provided buffer and return the number of bytes written (or <= 0 on
        // error). We pass our own stack buffers, so lifetimes are trivially
        // valid for the duration of the call.
        let nlen = unsafe {
            proc_name(
                rpid,
                name_buf.as_mut_ptr() as *mut c_char,
                name_buf.len() as u32,
            )
        };
        let plen = unsafe {
            proc_pidpath(
                rpid,
                path_buf.as_mut_ptr() as *mut c_char,
                path_buf.len() as u32,
            )
        };

        let name = if nlen > 0 {
            // SAFETY: proc_name writes a NUL-terminated C string when nlen>0.
            unsafe { CStr::from_ptr(name_buf.as_ptr() as *const c_char) }
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        };
        let path = if plen > 0 {
            // SAFETY: proc_pidpath writes a NUL-terminated C string when plen>0.
            unsafe { CStr::from_ptr(path_buf.as_ptr() as *const c_char) }
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        };

        Some(Responsible {
            pid: rpid as u32,
            name,
            path,
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    /// Stub mirror of the macOS `Responsible` so the public API stays
    /// portable; constructors always return `None`.
    #[derive(Debug, Clone)]
    pub struct Responsible {
        pub pid: u32,
        pub name: String,
        pub path: String,
    }

    pub fn for_pid(_pid: u32) -> Option<Responsible> {
        None
    }
}

pub use imp::{for_pid, Responsible};

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// The current process always has *some* responsible PID resolvable —
    /// either itself (when launched as a top-level app) or a parent. Either
    /// way, the call must succeed and return a non-empty name. This is the
    /// cheapest smoke test that verifies the FFI links and runs end-to-end
    /// without needing to fixture an external PID.
    #[test]
    fn resolves_self() {
        let me = std::process::id();
        let r = for_pid(me).expect("self should resolve");
        assert!(r.pid > 0, "responsible pid should be positive");
        assert!(!r.name.is_empty(), "responsible name should be non-empty");
    }

    /// PIDs that don't exist (intentionally use a high u32 unlikely to be
    /// allocated mid-test) must degrade to `None` rather than panicking.
    #[test]
    fn handles_dead_pid() {
        // `0` is reserved by the kernel and never assigned to userland — the
        // call should fail cleanly. Picking a real-but-dead PID would race.
        assert!(for_pid(0).is_none());
    }
}
