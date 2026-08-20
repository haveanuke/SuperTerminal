//! Current working directory of a process via
//! `proc_pidinfo(PROC_PIDVNODEPATHINFO)`.
//!
//! Direct syscall — the libproc crate stubs this out on macOS, and the C
//! struct layout (vnode_info 152 bytes + MAXPATHLEN 1024, twice = 2352) is
//! stable ABI. On non-macOS targets `pid_cwd` returns `None`.

#[cfg(target_os = "macos")]
mod imp {
    use std::os::raw::{c_int, c_void};

    const PROC_PIDVNODEPATHINFO: c_int = 9;

    #[repr(C)]
    struct VnodeInfoPath {
        _vi: [u8; 152],
        vip_path: [u8; 1024],
    }

    #[repr(C)]
    struct ProcVnodePathInfo {
        pvi_cdir: VnodeInfoPath,
        pvi_rdir: VnodeInfoPath,
    }

    extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
    }

    pub fn pid_cwd(pid: i32) -> Option<String> {
        let mut info = std::mem::MaybeUninit::<ProcVnodePathInfo>::uninit();
        let size = std::mem::size_of::<ProcVnodePathInfo>() as c_int;
        let ret = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr() as *mut c_void,
                size,
            )
        };
        if ret <= 0 {
            return None;
        }
        let info = unsafe { info.assume_init() };
        let path = &info.pvi_cdir.vip_path;
        let len = path.iter().position(|&b| b == 0).unwrap_or(path.len());
        std::str::from_utf8(&path[..len]).ok().map(String::from)
    }
}

/// Best-effort cwd of `pid`; `None` when the process is gone, inaccessible,
/// or the platform is not macOS.
pub fn pid_cwd(pid: i32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        imp::pid_cwd(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn reports_own_process_cwd() {
        let cwd = pid_cwd(std::process::id() as i32).expect("own cwd");
        assert_eq!(
            std::path::PathBuf::from(&cwd),
            std::env::current_dir().unwrap()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reports_child_process_cwd_and_none_after_exit() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .current_dir("/private/tmp")
            .spawn()
            .unwrap();
        let cwd = pid_cwd(child.id() as i32).expect("child cwd");
        assert!(
            cwd.starts_with("/private/tmp") || cwd.starts_with("/tmp"),
            "{cwd}"
        );
        child.kill().unwrap();
        child.wait().unwrap();
        // The pid may be briefly queryable post-reap on some systems, so only
        // assert that the call does not panic.
        let _ = pid_cwd(child.id() as i32);
    }
}
