//! Filesystem facts: free space of the disk holding a path.

use std::path::Path;

/// Free and total bytes of the filesystem holding `path`.
pub fn disk_space(path: &Path) -> Option<(i64, i64)> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is a valid NUL-terminated path and `st` is a properly
        // sized, writable statvfs buffer.
        let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
        if rc != 0 {
            return None;
        }
        let frsize = st.f_frsize as i64;
        Some((st.f_bavail as i64 * frsize, st.f_blocks as i64 * frsize))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Free bytes of the filesystem holding `path`, `None` when the path is not
/// there.
pub fn free_space(path: &Path) -> Option<i64> {
    disk_space(path).map(|(free, _)| free)
}

/// Hand the pages the allocator no longer uses back to the system. glibc
/// keeps freed memory for reuse, so a process that read a whole database
/// for an import, or built a large response once, looks that big for good;
/// this asks it to trim. A no-op on allocators that do not offer it.
pub fn release_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: malloc_trim takes a padding size and touches only the
        // allocator's own bookkeeping.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}
