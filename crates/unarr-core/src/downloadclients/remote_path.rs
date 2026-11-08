//! Remote path mappings, after the originals' `RemotePathMappingService`:
//! a download client on another machine reports its own paths; a mapping
//! (host, remote prefix, local prefix) turns them into paths Unarr can
//! read, and back.

use crate::model::RemotePathMapping;

/// `RemapRemoteToLocal`: the first mapping whose host matches
/// (case-insensitively) and whose remote path contains `remote` rewrites
/// the prefix; otherwise the path is returned as given.
pub fn remap_remote_to_local(mappings: &[RemotePathMapping], host: &str, remote: &str) -> String {
    if remote.is_empty() {
        return remote.to_string();
    }
    for m in mappings {
        if host.eq_ignore_ascii_case(&m.host) && contains(&m.remote_path, remote) {
            return join(&m.local_path, &relative(remote, &m.remote_path));
        }
    }
    remote.to_string()
}

/// `RemapLocalToRemote`: the inverse.
pub fn remap_local_to_remote(mappings: &[RemotePathMapping], host: &str, local: &str) -> String {
    if local.is_empty() {
        return local.to_string();
    }
    for m in mappings {
        if host.eq_ignore_ascii_case(&m.host) && contains(&m.local_path, local) {
            return join(&m.remote_path, &relative(local, &m.local_path));
        }
    }
    local.to_string()
}

/// `OsPath.IsRooted`: an absolute Unix path, a drive path or a UNC path.
pub fn is_rooted(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

/// Windows paths compare case-insensitively.
fn is_windows(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with("\\\\") || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

fn fragments(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).filter(|f| !f.is_empty()).collect()
}

/// `OsPath.Contains`: `parent` is `child` or one of its ancestors; both must
/// be rooted.
pub fn contains(parent: &str, child: &str) -> bool {
    if !is_rooted(parent) || !is_rooted(child) {
        return false;
    }
    let left = fragments(parent);
    let right = fragments(child);
    if right.len() < left.len() {
        return false;
    }
    let ignore_case = is_windows(parent) || is_windows(child);
    left.iter().zip(right.iter()).all(|(a, b)| {
        if ignore_case {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    })
}

/// `path - base`: the fragments of `path` below `base`, joined with
/// `path`'s separator, keeping a trailing separator when `path` had one.
fn relative(path: &str, base: &str) -> String {
    let skip = fragments(base).len();
    let sep = separator(path);
    let mut out = fragments(path)[skip..].join(sep);
    if !out.is_empty() && path.ends_with(['/', '\\']) {
        out.push_str(sep);
    }
    out
}

/// `base + relative`, with `base`'s separator.
pub fn join(base: &str, relative: &str) -> String {
    if relative.is_empty() {
        return base.to_string();
    }
    let sep = separator(base);
    let relative = relative.replace(['/', '\\'], sep);
    format!("{}{}{}", base.trim_end_matches(['/', '\\']), sep, relative)
}

/// The directory a path is in, without its trailing separator.
pub fn parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let idx = trimmed.rfind(['/', '\\'])?;
    let dir = &trimmed[..idx];
    if dir.is_empty() || (is_windows(path) && dir.len() <= 2) {
        return if trimmed.len() > idx + 1 {
            Some(format!("{dir}{}", separator(path)))
        } else {
            None
        };
    }
    Some(dir.to_string())
}

/// The last fragment of a path.
pub fn file_name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
}

fn separator(path: &str) -> &'static str {
    if is_windows(path) || (path.contains('\\') && !path.contains('/')) {
        "\\"
    } else {
        "/"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(host: &str, remote: &str, local: &str) -> RemotePathMapping {
        RemotePathMapping {
            id: 1,
            host: host.into(),
            remote_path: remote.into(),
            local_path: local.into(),
        }
    }

    #[test]
    fn remaps_by_host_and_prefix() {
        let m = vec![mapping("nas", "/downloads/", "/mnt/nas/downloads/")];
        assert_eq!(
            remap_remote_to_local(&m, "NAS", "/downloads/tv/x"),
            "/mnt/nas/downloads/tv/x"
        );
        assert_eq!(
            remap_remote_to_local(&m, "nas", "/downloads/tv/x/"),
            "/mnt/nas/downloads/tv/x/"
        );
        assert_eq!(
            remap_remote_to_local(&m, "other", "/downloads/tv/x"),
            "/downloads/tv/x"
        );
        assert_eq!(
            remap_remote_to_local(&m, "nas", "/downloadsx/tv"),
            "/downloadsx/tv"
        );
        assert_eq!(
            remap_local_to_remote(&m, "nas", "/mnt/nas/downloads/tv"),
            "/downloads/tv"
        );
        assert_eq!(remap_remote_to_local(&m, "nas", ""), "");
    }

    #[test]
    fn windows_paths_compare_case_insensitively() {
        let m = vec![mapping("pc", "C:\\Downloads\\", "/mnt/pc/downloads/")];
        assert_eq!(
            remap_remote_to_local(&m, "pc", "c:\\downloads\\TV\\x"),
            "/mnt/pc/downloads/TV/x"
        );
        assert!(contains("C:\\a", "c:\\A\\b"));
        assert!(!contains("relative", "/abs"));
    }

    #[test]
    fn walks_parents() {
        assert_eq!(
            parent("/downloads/complete/tv/x"),
            Some("/downloads/complete/tv".into())
        );
        assert_eq!(parent("/downloads"), Some("/".into()));
        assert_eq!(parent("/"), None);
        assert_eq!(file_name("/downloads/complete/tv/x/"), "x");
        assert_eq!(join("/a/", "b/c"), "/a/b/c");
    }
}
