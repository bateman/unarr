//! String ordering as the originals produce it.
//!
//! .NET's default string comparison is culture-aware; with ICU present (the
//! Docker images) it follows the CLDR root collation: letters compare
//! case-insensitively first, whitespace sorts before punctuation before digits
//! before letters, and only when everything else is equal does case decide,
//! lower before upper. Ordinal order would put `LQ` before `Language`, and
//! `NTb` before `cakes`; the originals do neither.
//!
//! This is an approximation that covers names made of ASCII letters, digits
//! and punctuation, which is what release groups, custom formats and
//! providers are called in practice.

use std::cmp::Ordering;

/// Weight of one character at the primary level.
fn primary(c: char) -> (u8, u32) {
    if c.is_whitespace() {
        (0, 0)
    } else if c.is_numeric() {
        (2, c as u32)
    } else if c.is_alphabetic() {
        (3, c.to_lowercase().next().unwrap_or(c) as u32)
    } else {
        // Punctuation and symbols, in CLDR root order for the common ones.
        let rank = match c {
            '_' => 1,
            '-' => 2,
            ',' => 3,
            ';' => 4,
            ':' => 5,
            '!' => 6,
            '?' => 7,
            '.' => 8,
            '\'' => 9,
            '"' => 10,
            '(' => 11,
            ')' => 12,
            '[' => 13,
            ']' => 14,
            '{' => 15,
            '}' => 16,
            '@' => 17,
            '*' => 18,
            '/' => 19,
            '\\' => 20,
            '&' => 21,
            '#' => 22,
            '%' => 23,
            '+' => 24,
            '<' => 25,
            '=' => 26,
            '>' => 27,
            '|' => 28,
            '~' => 29,
            '$' => 30,
            other => 100 + other as u32,
        };
        (1, rank)
    }
}

/// Compare two strings the way the originals sort names.
pub fn compare(a: &str, b: &str) -> Ordering {
    let pa = a.chars().map(primary);
    let pb = b.chars().map(primary);
    match pa.cmp(pb) {
        Ordering::Equal => {}
        other => return other,
    }
    // Tertiary level: lower case before upper case, position by position.
    let ta = a.chars().map(|c| c.is_uppercase());
    let tb = b.chars().map(|c| c.is_uppercase());
    ta.cmp(tb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted<'a>(items: &[&'a str]) -> Vec<&'a str> {
        let mut v = items.to_vec();
        v.sort_by(|a, b| compare(a, b));
        v
    }

    #[test]
    fn letters_ignore_case_first_then_lower_before_upper() {
        assert_eq!(
            sorted(&["NTb", "CAKES", "PECULATE", "cakes"]),
            ["cakes", "CAKES", "NTb", "PECULATE"]
        );
        assert_eq!(
            sorted(&["SuccessfulCrab", "NTb", "successfulcrab"]),
            ["NTb", "successfulcrab", "SuccessfulCrab"]
        );
        assert_eq!(sorted(&["NTb", "monkee"]), ["monkee", "NTb"]);
    }

    #[test]
    fn punctuation_and_digits_before_letters() {
        assert_eq!(
            sorted(&["LQ", "Language: Not Original", "Language not Italian"]),
            ["Language not Italian", "Language: Not Original", "LQ"]
        );
        assert_eq!(
            sorted(&["x265", "x264", "WEB Tier 01", "AMZN"]),
            ["AMZN", "WEB Tier 01", "x264", "x265"]
        );
        assert_eq!(
            sorted(&["Newznab 2", "Newznab 1", "Torznab 7"]),
            ["Newznab 1", "Newznab 2", "Torznab 7"]
        );
    }
}
