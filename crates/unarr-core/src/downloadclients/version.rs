//! `System.Version` as the clients' test code compares and prints it: two
//! to four numeric components, shown with as many as were given.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: i64,
    pub minor: i64,
    pub build: i64,
    pub revision: i64,
    /// How many components `Display` shows (2 to 4).
    pub shown: u8,
}

impl Version {
    pub const fn new(major: i64, minor: i64) -> Version {
        Version {
            major,
            minor,
            build: 0,
            revision: 0,
            shown: 2,
        }
    }

    pub const fn with_build(major: i64, minor: i64, build: i64) -> Version {
        Version {
            major,
            minor,
            build,
            revision: 0,
            shown: 3,
        }
    }

    /// `Version.Parse`: `None` where .NET throws (fewer than two or more
    /// than four components, a non-numeric one, "4.2alpha").
    pub fn parse(text: &str) -> Option<Version> {
        let parts: Vec<&str> = text.trim().split('.').collect();
        if !(2..=4).contains(&parts.len()) {
            return None;
        }
        let mut numbers = [0i64; 4];
        for (i, part) in parts.iter().enumerate() {
            numbers[i] = part.parse().ok().filter(|n| *n >= 0)?;
        }
        Some(Version {
            major: numbers[0],
            minor: numbers[1],
            build: numbers[2],
            revision: numbers[3],
            shown: parts.len() as u8,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.shown {
            3 => write!(f, "{}.{}.{}", self.major, self.minor, self.build),
            4 => write!(
                f,
                "{}.{}.{}.{}",
                self.major, self.minor, self.build, self.revision
            ),
            _ => write!(f, "{}.{}", self.major, self.minor),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_compares_like_dotnet() {
        assert_eq!(Version::parse("2.11.4"), Some(Version::with_build(2, 11, 4)));
        assert!(Version::parse("2.11.4").unwrap() >= Version::with_build(2, 11, 0));
        assert!(Version::parse("2.8.1").unwrap() < Version::new(2, 11));
        assert_eq!(Version::parse("5"), None);
        assert_eq!(Version::parse("4.2alpha"), None);
        assert_eq!(Version::parse("1.2.3.4.5"), None);
        assert_eq!(Version::with_build(0, 6, 9).to_string(), "0.6.9");
        assert_eq!(Version::parse("12.0").unwrap().to_string(), "12.0");
    }
}
