//! Random bytes from the operating system, for what must not be guessed:
//! API keys, the key session cookies are signed with, password salts.

use ring::rand::{SecureRandom, SystemRandom};

/// `N` random bytes, or an error: there is no fallback worth having.
pub fn bytes<const N: usize>() -> anyhow::Result<[u8; N]> {
    let mut out = [0u8; N];
    SystemRandom::new()
        .fill(&mut out)
        .map_err(|_| anyhow::anyhow!("the operating system gave no random bytes"))?;
    Ok(out)
}

/// `N` random bytes as lower-case hexadecimal, twice as many characters.
pub fn hex<const N: usize>() -> anyhow::Result<String> {
    Ok(bytes::<N>()?.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_long_and_differ() {
        let (a, b) = (hex::<16>().unwrap(), hex::<16>().unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }
}
