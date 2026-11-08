//! Filesystem facts the API reports: free space and the mount table.

use std::path::Path;

/// Free and total bytes of the filesystem holding `path`.
pub fn disk_space(path: &Path) -> Option<(i64, i64)> {
    unarr_core::fs::disk_space(path)
}

/// Mount points that look like real disks (or network shares), as the
/// originals list them on the System > Status page: filesystem types that
/// `/proc/filesystems` marks as device-backed, plus common network types,
/// minus the special paths the originals exclude.
pub fn disk_mounts() -> Vec<String> {
    let filesystems = std::fs::read_to_string("/proc/filesystems").unwrap_or_default();
    let device_backed: Vec<&str> = filesystems
        .lines()
        .filter(|l| !l.starts_with("nodev"))
        .map(|l| l.trim())
        .collect();
    let network = [
        "nfs",
        "nfs4",
        "cifs",
        "smb3",
        "smbfs",
        "9p",
        "fuse.rclone",
        "fuse.mergerfs",
        "zfs",
    ];
    let special = [
        "/var/lib/docker",
        "/var/lib/rancher",
        "/var/lib/kubelet",
        "/boot",
        "/etc",
    ];

    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for line in mounts.lines() {
        let mut parts = line.split(' ');
        let (Some(_dev), Some(mount), Some(fstype)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if !(device_backed.contains(&fstype) || network.contains(&fstype)) {
            continue;
        }
        let mount = unescape_mount(mount);
        if special
            .iter()
            .any(|s| mount == *s || mount.starts_with(&format!("{s}/")))
            || mount.contains("/docker/aufs")
            || mount.contains("/.timemachine")
        {
            continue;
        }
        if !out.contains(&mount) {
            out.push(mount);
        }
    }
    out
}

/// `/proc/mounts` escapes spaces and friends as octal (`\040`).
fn unescape_mount(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let digits: String = chars.clone().take(3).collect();
            if digits.len() == 3 {
                if let Ok(v) = u8::from_str_radix(&digits, 8) {
                    out.push(v as char);
                    for _ in 0..3 {
                        chars.next();
                    }
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_has_space_and_is_mounted() {
        let (free, total) = disk_space(Path::new("/")).unwrap();
        assert!(total > 0 && free <= total);
        assert!(disk_space(Path::new("/definitely/not/here")).is_none());
        assert_eq!(unescape_mount("/mnt/my\\040disk"), "/mnt/my disk");
        if cfg!(target_os = "linux") {
            assert!(disk_mounts().iter().any(|m| m == "/"));
        }
    }
}
