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

/// Keep large buffers out of the allocator's arenas. glibc serves a request
/// above its mmap threshold with a mapping of its own, returned on free,
/// but raises that threshold (up to 32 MiB) each time such a mapping is
/// freed; from then on a decoded cover or a large response comes from the
/// arena of whichever thread asked, and stays there. Setting the threshold
/// fixes it. Call once at start; a no-op on other allocators.
pub fn tune_allocator() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: mallopt only sets a tunable of the allocator.
        unsafe {
            libc::mallopt(libc::M_MMAP_THRESHOLD, 1024 * 1024);
        }
    }
}

/// How long a compiled pattern may go unused before it is let go.
pub const PATTERN_IDLE: std::time::Duration = std::time::Duration::from_secs(300);

/// What the daemon does when it has a quiet moment: the compiled patterns
/// of the title parsers and of the custom formats that nothing used for
/// [`PATTERN_IDLE`] are dropped (they are compiled again on their next
/// use, a second for the lot), then the allocator's free pages go back to
/// the system.
pub fn release_idle_memory() {
    let patterns = crate::parser::netregex::release_idle(PATTERN_IDLE);
    let formats = crate::customformat::release_idle(PATTERN_IDLE);
    if patterns + formats > 0 {
        tracing::debug!(patterns, formats, "released the compiled patterns left idle");
    }
    release_memory();
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
