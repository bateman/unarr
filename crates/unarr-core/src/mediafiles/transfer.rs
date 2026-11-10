//! Files on disk: the originals' `DiskTransferService`, `RecycleBinProvider`
//! and `MediaFileAttributeService`, for what the import and the renames
//! need: move, hardlink-or-copy, copy, send to the recycling bin, set
//! permissions, create and prune folders.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::store::config::MediaManagementConfig;

/// `TransferMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
    Move,
    Copy,
    HardLinkOrCopy,
}

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("Episode file path does not exist: {0}")]
    SourceMissing(String),
    #[error("File not moved, source and destination are the same: {0}")]
    SameFile(String),
    #[error("Destination already exists: {0}")]
    DestinationExists(String),
    #[error("Root folder '{0}' was not found.")]
    RootFolderMissing(String),
    #[error("Unable to move '{path}' to the recycling bin: '{destination}'")]
    RecycleBin { path: String, destination: String },
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// `TransferFile(source, target, mode)`: the target's folder exists when
/// this is called. A move across filesystems copies then deletes; a
/// hardlink that fails falls back to a copy; the result says what happened.
pub fn transfer_file(
    source: &Path,
    target: &Path,
    mode: TransferMode,
) -> Result<TransferMode, TransferError> {
    if !source.is_file() {
        return Err(TransferError::SourceMissing(source.display().to_string()));
    }
    if same_path(source, target) {
        return Err(TransferError::SameFile(source.display().to_string()));
    }
    if target.exists() {
        return Err(TransferError::DestinationExists(target.display().to_string()));
    }
    match mode {
        TransferMode::Move => {
            match fs::rename(source, target) {
                Ok(()) => {}
                Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                    // Another filesystem: copy, verify the size, then delete.
                    copy_verified(source, target)?;
                    fs::remove_file(source)?;
                }
                Err(e) => return Err(e.into()),
            }
            Ok(TransferMode::Move)
        }
        TransferMode::Copy => {
            copy_verified(source, target)?;
            Ok(TransferMode::Copy)
        }
        TransferMode::HardLinkOrCopy => match fs::hard_link(source, target) {
            Ok(()) => Ok(TransferMode::HardLinkOrCopy),
            Err(e) => {
                tracing::debug!(error = %e, "hardlink failed, copying instead");
                copy_verified(source, target)?;
                Ok(TransferMode::Copy)
            }
        },
    }
}

fn copy_verified(source: &Path, target: &Path) -> Result<(), TransferError> {
    let expected = fs::metadata(source)?.len();
    let copied = fs::copy(source, target)?;
    if copied != expected {
        let _ = fs::remove_file(target);
        return Err(io::Error::other(format!(
            "copy of {} wrote {copied} bytes of {expected}",
            source.display()
        ))
        .into());
    }
    Ok(())
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || match (fs::canonicalize(a), fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
}

/// `RecycleBinProvider.DeleteFile(path, subfolder)`: to the recycling bin
/// when one is configured (under `subfolder`, numbered when the name is
/// taken), else deleted for good. Returns where the file went.
pub fn recycle_file(
    config: &MediaManagementConfig,
    path: &Path,
    subfolder: &str,
) -> Result<Option<PathBuf>, TransferError> {
    let bin = config.recycle_bin.trim();
    if bin.is_empty() {
        tracing::info!(
            "Recycling Bin has not been configured, deleting permanently. {}",
            path.display()
        );
        fs::remove_file(path)?;
        return Ok(None);
    }
    let folder = Path::new(bin).join(subfolder.trim_start_matches('/'));
    fs::create_dir_all(&folder).map_err(|e| {
        tracing::error!(error = %e, "Unable to create the folder '{}' in the recycling bin", folder.display());
        TransferError::RecycleBin {
            path: path.display().to_string(),
            destination: folder.display().to_string(),
        }
    })?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut destination = folder.join(&name);
    let mut index = 1;
    while destination.exists() {
        index += 1;
        destination = if ext.is_empty() {
            folder.join(format!("{name}_{index}"))
        } else {
            folder.join(format!("{stem}_{index}{ext}"))
        };
    }
    tracing::debug!("Moving '{}' to '{}'", path.display(), destination.display());
    transfer_file(path, &destination, TransferMode::Move).map_err(|e| {
        tracing::error!(error = %e, "Unable to move '{}' to the recycling bin", path.display());
        TransferError::RecycleBin {
            path: path.display().to_string(),
            destination: destination.display().to_string(),
        }
    })?;
    set_last_write_time(&destination, Utc::now());
    Ok(Some(destination))
}

/// `RecycleBinProvider.DeleteFolder`.
pub fn recycle_folder(config: &MediaManagementConfig, path: &Path) -> Result<(), TransferError> {
    let bin = config.recycle_bin.trim();
    if bin.is_empty() {
        tracing::info!(
            "Recycling Bin has not been configured, deleting permanently. {}",
            path.display()
        );
        fs::remove_dir_all(path)?;
        return Ok(());
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let destination = Path::new(bin).join(name);
    fs::create_dir_all(bin)?;
    match fs::rename(path, &destination) {
        Ok(()) => {}
        Err(_) => {
            copy_tree(path, &destination)?;
            fs::remove_dir_all(path)?;
        }
    }
    set_last_write_time(&destination, Utc::now());
    for file in files_under(&destination) {
        set_last_write_time(&file, Utc::now());
    }
    Ok(())
}

/// `RecycleBinProvider.Cleanup` (`CleanUpRecycleBin`, daily): the files
/// that have been in the bin for more than the configured days are
/// deleted, and the folders left empty with them. Returns how many files
/// went; nothing happens without a bin, or with the days at zero.
pub fn clean_up_recycle_bin(config: &MediaManagementConfig, now: DateTime<Utc>) -> usize {
    let bin = config.recycle_bin.trim();
    if bin.is_empty() {
        tracing::info!("Recycle Bin has not been configured, cannot cleanup.");
        return 0;
    }
    let days = config.recycle_bin_cleanup_days;
    if days == 0 {
        tracing::info!("Automatic cleanup of Recycle Bin is disabled");
        return 0;
    }
    tracing::info!("Removing items older than {days} days from the recycling bin");
    let mut removed = 0;
    for file in files_under(Path::new(bin)) {
        // The time it was put in the bin: `recycle_file` stamps it.
        let written: Option<DateTime<Utc>> = fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .map(DateTime::<Utc>::from);
        if written.is_none_or(|at| at + chrono::Duration::days(days) > now) {
            tracing::debug!("File hasn't expired yet, skipping: {}", file.display());
            continue;
        }
        match fs::remove_file(&file) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!(error = %e, "Unable to delete {}", file.display()),
        }
    }
    remove_empty_subfolders(Path::new(bin));
    tracing::debug!("Recycling Bin has been cleaned up.");
    removed
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Every file under `root`, recursively, sorted.
pub fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(t) if t.is_file() || t.is_symlink() => out.push(path),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

/// Every directory directly under `root`, sorted.
pub fn directories_under(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(root)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// `FolderEmpty`.
pub fn folder_empty(path: &Path) -> bool {
    fs::read_dir(path)
        .map(|mut rd| rd.next().is_none())
        .unwrap_or(false)
}

/// `RemoveEmptySubfolders`: every empty folder below `path`, deepest first.
pub fn remove_empty_subfolders(path: &Path) {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for sub in directories_under(&dir) {
            dirs.push(sub.clone());
            stack.push(sub);
        }
    }
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in dirs {
        if folder_empty(&d) {
            let _ = fs::remove_dir(&d);
        }
    }
}

/// `MediaFileAttributeService.SetFilePermissions` / `SetFolderPermissions`
/// on Linux: the configured folder mode, files without the execute bits,
/// the group when one is named.
pub fn set_permissions(config: &MediaManagementConfig, path: &Path) {
    if !config.set_permissions_linux {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(folder_mode) = u32::from_str_radix(config.chmod_folder.trim(), 8) else {
            tracing::warn!(
                "Unable to apply permissions to: {}: bad folder mode {}",
                path.display(),
                config.chmod_folder
            );
            return;
        };
        let is_dir = path.is_dir();
        // The originals derive the file mode from the folder mode by
        // dropping the execute bits.
        let mode = if is_dir { folder_mode } else { folder_mode & !0o111 };
        if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(mode)) {
            tracing::warn!(error = %e, "Unable to apply permissions to: {}", path.display());
        }
        let group = config.chown_group.trim();
        if !group.is_empty() {
            if let Err(e) = chown_group(path, group) {
                tracing::warn!(error = %e, "Unable to apply group {group} to: {}", path.display());
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(unix)]
fn chown_group(path: &Path, group: &str) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let gid: libc::gid_t = match group.parse::<u32>() {
        Ok(n) => n,
        Err(_) => {
            let c = CString::new(group).map_err(|_| io::Error::other("bad group name"))?;
            // SAFETY: getgrnam takes a NUL-terminated name and returns a
            // pointer to static storage or NULL.
            let gr = unsafe { libc::getgrnam(c.as_ptr()) };
            if gr.is_null() {
                return Err(io::Error::other(format!("unknown group {group}")));
            }
            // SAFETY: non-null, points at a valid group record.
            unsafe { (*gr).gr_gid }
        }
    };
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::other("bad path"))?;
    // SAFETY: valid NUL-terminated path; -1 leaves the owner alone.
    let rc = unsafe { libc::chown(c.as_ptr(), u32::MAX, gid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `FileSetLastWriteTime`, errors swallowed as the originals swallow them.
pub fn set_last_write_time(path: &Path, at: DateTime<Utc>) {
    if let Ok(f) = fs::File::options().write(true).open(path) {
        let _ = f.set_modified(std::time::SystemTime::from(at));
    }
}

/// `FileGetLastWrite`.
pub fn last_write_time(path: &Path) -> Option<DateTime<Utc>> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
}

/// `CreateFolder` up to the root, with the configured permissions on each
/// folder created.
pub fn create_folder(config: &MediaManagementConfig, path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut created = Vec::new();
    let mut missing = Vec::new();
    let mut current = Some(path);
    while let Some(p) = current {
        if p.exists() {
            break;
        }
        missing.push(p.to_path_buf());
        current = p.parent();
    }
    for p in missing.iter().rev() {
        fs::create_dir(p)?;
        set_permissions(config, p);
        created.push(p.clone());
    }
    Ok(created)
}

/// `DiskTransferService.TransferFolder(source, destination, Move)`: a
/// rename when both are on one file system, else a copy of every file
/// then the source removed.
pub fn move_folder(source: &Path, destination: &Path) -> io::Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            copy_tree(source, destination)?;
            std::fs::remove_dir_all(source)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("unarr-transfer-{}-{}", std::process::id(), rand_suffix()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[test]
    fn moves_links_and_copies() {
        let dir = temp();
        let src = dir.join("a.mkv");
        fs::write(&src, b"video").unwrap();
        let linked = dir.join("b.mkv");
        assert_eq!(
            transfer_file(&src, &linked, TransferMode::HardLinkOrCopy).unwrap(),
            TransferMode::HardLinkOrCopy
        );
        assert_eq!(fs::read(&linked).unwrap(), b"video");
        let copied = dir.join("c.mkv");
        assert_eq!(
            transfer_file(&src, &copied, TransferMode::Copy).unwrap(),
            TransferMode::Copy
        );
        let moved = dir.join("sub").join("d.mkv");
        fs::create_dir_all(moved.parent().unwrap()).unwrap();
        assert_eq!(
            transfer_file(&src, &moved, TransferMode::Move).unwrap(),
            TransferMode::Move
        );
        assert!(!src.exists());
        assert!(matches!(
            transfer_file(&moved, &moved, TransferMode::Move),
            Err(TransferError::SameFile(_))
        ));
        assert!(matches!(
            transfer_file(&moved, &copied, TransferMode::Move),
            Err(TransferError::DestinationExists(_))
        ));
        assert!(matches!(
            transfer_file(&src, &dir.join("e.mkv"), TransferMode::Move),
            Err(TransferError::SourceMissing(_))
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn recycles_or_deletes() {
        let dir = temp();
        let file = dir.join("x.mkv");
        fs::write(&file, b"v").unwrap();
        let mut config = MediaManagementConfig::default();
        assert_eq!(recycle_file(&config, &file, "Show/Season 01").unwrap(), None);
        assert!(!file.exists());

        config.recycle_bin = dir.join("bin").display().to_string();
        fs::write(&file, b"v").unwrap();
        let first = recycle_file(&config, &file, "Show/Season 01").unwrap().unwrap();
        assert_eq!(first, dir.join("bin/Show/Season 01/x.mkv"));
        fs::write(&file, b"v").unwrap();
        let second = recycle_file(&config, &file, "Show/Season 01").unwrap().unwrap();
        assert_eq!(second, dir.join("bin/Show/Season 01/x_2.mkv"));

        // The clean-up takes what has been in the bin for longer than the
        // days set, and the folders that leaves empty.
        let old = dir.join("bin/Gone/Season 02/old.mkv");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::write(&old, b"v").unwrap();
        let now = Utc::now();
        set_last_write_time(&old, now - chrono::Duration::days(8));
        config.recycle_bin_cleanup_days = 0;
        assert_eq!(clean_up_recycle_bin(&config, now), 0, "disabled");
        config.recycle_bin_cleanup_days = 7;
        assert_eq!(clean_up_recycle_bin(&config, now), 1);
        assert!(!dir.join("bin/Gone").exists(), "emptied folders go");
        assert!(first.exists() && second.exists(), "recent files stay");
        assert_eq!(clean_up_recycle_bin(&config, now + chrono::Duration::days(8)), 2);
        assert!(dir.join("bin").exists() && !dir.join("bin/Show").exists());
        assert_eq!(
            clean_up_recycle_bin(&MediaManagementConfig::default(), now),
            0,
            "no bin"
        );

        fs::create_dir_all(dir.join("show/Season 01/empty")).unwrap();
        fs::write(dir.join("show/Season 01/keep.mkv"), b"v").unwrap();
        remove_empty_subfolders(&dir.join("show"));
        assert!(!dir.join("show/Season 01/empty").exists());
        assert!(dir.join("show/Season 01").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
