//! What a torrent is known by: the SHA-1 of its bencoded `info` dictionary,
//! read from a `.torrent` file or a magnet link, after the originals'
//! `TorrentFileInfoReader` (MonoTorrent) and `MagnetLink.Parse`.

use ring::digest::{digest, SHA1_FOR_LEGACY_USE_ONLY};

/// The info hash of a `.torrent` file, upper-case hex, or `None` when the
/// bytes are not a bencoded dictionary with an `info` entry.
pub fn info_hash(torrent: &[u8]) -> Option<String> {
    let (start, end) = info_span(torrent)?;
    Some(hex_upper(
        digest(&SHA1_FOR_LEGACY_USE_ONLY, &torrent[start..end]).as_ref(),
    ))
}

/// The torrent's name and total length, for mocks and messages.
pub fn summary(torrent: &[u8]) -> Option<(String, i64)> {
    let (start, end) = info_span(torrent)?;
    let (info, _) = parse(torrent, start)?;
    let Value::Dict(entries) = info else {
        return None;
    };
    let _ = end;
    let get = |key: &[u8]| entries.iter().find(|(k, _)| k.as_slice() == key).map(|(_, v)| v);
    let name = match get(b"name") {
        Some(Value::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
        _ => return None,
    };
    let length = match get(b"length") {
        Some(Value::Int(n)) => *n,
        _ => match get(b"files") {
            Some(Value::List(files)) => files
                .iter()
                .filter_map(|f| match f {
                    Value::Dict(d) => d
                        .iter()
                        .find(|(k, _)| k.as_slice() == b"length")
                        .and_then(|(_, v)| match v {
                            Value::Int(n) => Some(*n),
                            _ => None,
                        }),
                    _ => None,
                })
                .sum(),
            _ => 0,
        },
    };
    Some((name, length))
}

/// The info hash a magnet link names (`xt=urn:btih:`), upper-case hex; a
/// base32 hash is converted. `None` when the link carries none.
pub fn magnet_info_hash(magnet: &str) -> Option<String> {
    let query = magnet.strip_prefix("magnet:?")?;
    for pair in query.split('&') {
        let Some(xt) = pair.strip_prefix("xt=") else {
            continue;
        };
        let Some(hash) = xt.strip_prefix("urn:btih:") else {
            continue;
        };
        if hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hash.to_ascii_uppercase());
        }
        if hash.len() == 32 {
            if let Some(bytes) = base32_decode(hash) {
                return Some(hex_upper(&bytes));
            }
        }
    }
    None
}

/// The `dn` (display name) of a magnet link, when it carries one.
pub fn magnet_display_name(magnet: &str) -> Option<String> {
    let query = magnet.strip_prefix("magnet:?")?;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("dn="))
        .map(|dn| percent_decode(dn).replace('+', " "))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn base32_decode(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut bits: u64 = 0;
    let mut count = 0;
    let mut out = Vec::new();
    for c in s.bytes() {
        let v = ALPHABET.iter().position(|a| *a == c.to_ascii_uppercase())? as u64;
        bits = (bits << 5) | v;
        count += 5;
        if count >= 8 {
            out.push((bits >> (count - 8)) as u8);
            count -= 8;
            bits &= (1 << count) - 1;
        }
    }
    Some(out)
}

enum Value {
    Int(i64),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Dict(Vec<(Vec<u8>, Value)>),
}

/// Where the top-level `info` value starts and ends.
fn info_span(data: &[u8]) -> Option<(usize, usize)> {
    if data.first() != Some(&b'd') {
        return None;
    }
    let mut i = 1;
    while i < data.len() && data[i] != b'e' {
        let (key, next) = parse(data, i)?;
        let Value::Bytes(key) = key else {
            return None;
        };
        let start = next;
        let (_, end) = parse(data, start)?;
        if key == b"info" {
            return Some((start, end));
        }
        i = end;
    }
    None
}

/// Nesting a torrent file never needs; deeper input is hostile and would
/// overflow the stack of a recursive parser.
const MAX_DEPTH: usize = 128;

fn parse(data: &[u8], i: usize) -> Option<(Value, usize)> {
    parse_at(data, i, 0)
}

fn parse_at(data: &[u8], i: usize, depth: usize) -> Option<(Value, usize)> {
    if depth > MAX_DEPTH {
        return None;
    }
    match *data.get(i)? {
        b'i' => {
            let end = i + data[i..].iter().position(|b| *b == b'e')?;
            let n: i64 = std::str::from_utf8(&data[i + 1..end]).ok()?.parse().ok()?;
            Some((Value::Int(n), end + 1))
        }
        b'l' => {
            let mut items = Vec::new();
            let mut j = i + 1;
            while *data.get(j)? != b'e' {
                let (v, next) = parse_at(data, j, depth + 1)?;
                items.push(v);
                j = next;
            }
            Some((Value::List(items), j + 1))
        }
        b'd' => {
            let mut entries = Vec::new();
            let mut j = i + 1;
            while *data.get(j)? != b'e' {
                let (k, next) = parse_at(data, j, depth + 1)?;
                let Value::Bytes(k) = k else {
                    return None;
                };
                let (v, next) = parse_at(data, next, depth + 1)?;
                entries.push((k, v));
                j = next;
            }
            Some((Value::Dict(entries), j + 1))
        }
        b'0'..=b'9' => {
            let colon = i + data[i..].iter().position(|b| *b == b':')?;
            let len: usize = std::str::from_utf8(&data[i..colon]).ok()?.parse().ok()?;
            let start = colon + 1;
            let end = start.checked_add(len)?;
            if end > data.len() {
                return None;
            }
            Some((Value::Bytes(data[start..end].to_vec()), end))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_the_info_dictionary() {
        let torrent = b"d8:announce3:url4:infod6:lengthi7e4:name5:a.mkv12:piece lengthi1e6:pieces0:ee";
        let (start, end) = info_span(torrent).unwrap();
        assert_eq!(
            &torrent[start..end],
            b"d6:lengthi7e4:name5:a.mkv12:piece lengthi1e6:pieces0:e".as_slice()
        );
        let hash = info_hash(torrent).unwrap();
        assert_eq!(hash.len(), 40);
        assert_eq!(hash, hash.to_ascii_uppercase());
        assert_eq!(summary(torrent), Some(("a.mkv".into(), 7)));
        assert_eq!(info_hash(b"not a torrent"), None);
        assert_eq!(info_hash(b"d8:announce3:urle"), None);
    }

    /// A list nested twenty thousand deep is not a torrent; a recursive
    /// parser would overflow the stack on it, and nothing catches that.
    #[test]
    fn refuses_hostile_nesting() {
        let mut data = b"d4:info".to_vec();
        data.extend(std::iter::repeat_n(b'l', 20_000));
        assert_eq!(info_hash(&data), None);
        assert!(parse(&data, 0).is_none());
        // Reasonable nesting still parses.
        let shallow = b"llllllllllllllllllllllllllllllllleeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        assert!(parse(shallow, 0).is_some());
    }

    #[test]
    fn reads_magnet_hashes() {
        assert_eq!(
            magnet_info_hash("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=X"),
            Some("0123456789ABCDEF0123456789ABCDEF01234567".into())
        );
        // Base32 of the same twenty bytes.
        assert_eq!(
            magnet_info_hash("magnet:?xt=urn:btih:AERUKZ4JVPG66AJDIVTYTK6N54ASGRLH"),
            Some("0123456789ABCDEF0123456789ABCDEF01234567".into())
        );
        assert_eq!(magnet_info_hash("magnet:?dn=X"), None);
        assert_eq!(
            magnet_display_name("magnet:?xt=urn:btih:x&dn=The%20Pitt+S01E01"),
            Some("The Pitt S01E01".into())
        );
    }
}
