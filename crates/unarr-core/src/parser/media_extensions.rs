//! The originals' `MediaFileExtensions`: every extension they treat as a
//! media file, with the quality assumed when the name says nothing else.

/// `(extension with dot, quality id)`, as the originals list them.
pub const MEDIA_EXTENSIONS: &[(&str, i64)] = &[
    // Unknown
    (".webm", 0),
    // SDTV
    (".m4v", 1),
    (".3gp", 1),
    (".nsv", 1),
    (".ty", 1),
    (".strm", 1),
    (".rm", 1),
    (".rmvb", 1),
    (".m3u", 1),
    (".ifo", 1),
    (".mov", 1),
    (".qt", 1),
    (".divx", 1),
    (".xvid", 1),
    (".bivx", 1),
    (".nrg", 1),
    (".pva", 1),
    (".wmv", 1),
    (".asf", 1),
    (".asx", 1),
    (".ogm", 1),
    (".ogv", 1),
    (".m2v", 1),
    (".avi", 1),
    (".bin", 1),
    (".dat", 1),
    (".dvr-ms", 1),
    (".mpg", 1),
    (".mpeg", 1),
    (".mp4", 1),
    (".avc", 1),
    (".vp3", 1),
    (".svq3", 1),
    (".nuv", 1),
    (".viv", 1),
    (".dv", 1),
    (".fli", 1),
    (".flv", 1),
    (".wpl", 1),
    // DVD
    (".img", 2),
    (".iso", 2),
    (".vob", 2),
    // HD
    (".mkv", 4),
    (".ts", 4),
    (".wtv", 4),
    // Bluray
    (".m2ts", 6),
];

/// Whether `ext` (with its dot, any case) is a media extension.
pub fn is_media_extension(ext: &str) -> bool {
    MEDIA_EXTENSIONS.iter().any(|(e, _)| e.eq_ignore_ascii_case(ext))
}

/// `GetQualityForExtension`: the quality id an extension implies, 0 if none.
pub fn quality_for_extension(ext: &str) -> i64 {
    MEDIA_EXTENSIONS
        .iter()
        .find(|(e, _)| e.eq_ignore_ascii_case(ext))
        .map(|(_, q)| *q)
        .unwrap_or(0)
}

/// `Path.GetExtension`: the last `.ext` of the final path component, or empty.
pub fn path_extension(name: &str) -> &str {
    let last = name.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
    let component = &name[last..];
    match component.rfind('.') {
        Some(i) => &component[i..],
        None => "",
    }
}

/// The originals' `DiskExtensions` and `StreamingExtensions`: disk images
/// and stream files, which ffprobe is not asked about.
pub fn is_disk_or_stream_extension(ext: &str) -> bool {
    [".img", ".iso", ".vob", ".m3u", ".strm"]
        .iter()
        .any(|e| e.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_lookups() {
        assert!(is_media_extension(".MKV"));
        assert!(!is_media_extension(".nzb"));
        assert_eq!(quality_for_extension(".mkv"), 4);
        assert_eq!(quality_for_extension(".avi"), 1);
        assert_eq!(quality_for_extension(".txt"), 0);
        assert_eq!(path_extension("a/b/Show.S01E01.mkv"), ".mkv");
        assert_eq!(path_extension("Show.S01E01"), ".S01E01");
        assert_eq!(path_extension("dir.name/file"), "");
    }
}
