//! Paging as the compatible API exposes it: 1-based page, page size, a sort
//! key validated against a per-endpoint list, and a direction.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paging {
    pub page: i64,
    pub page_size: i64,
    pub sort_key: String,
    pub descending: bool,
}

impl Paging {
    /// Resolve a request's paging parameters the way the originals do: an
    /// unknown sort key falls back to `default_key`, and a missing direction
    /// is descending whatever the endpoint's nominal default (the originals
    /// fill the direction in before applying that default).
    pub fn resolve(
        page: Option<i64>,
        page_size: Option<i64>,
        sort_key: Option<&str>,
        descending: Option<bool>,
        allowed_keys: &[&str],
        default_key: &str,
    ) -> Paging {
        let sort_key = sort_key
            .and_then(|k| allowed_keys.iter().find(|a| a.eq_ignore_ascii_case(k)))
            .map(|a| a.to_string())
            .unwrap_or_else(|| default_key.to_string());
        Paging {
            page: page.unwrap_or(1).max(1),
            page_size: page_size.unwrap_or(10).max(1),
            sort_key,
            descending: descending.unwrap_or(true),
        }
    }

    pub fn offset(&self) -> i64 {
        (self.page - 1) * self.page_size
    }

    pub fn direction_sql(&self) -> &'static str {
        if self.descending {
            "DESC"
        } else {
            "ASC"
        }
    }

    /// Slice an in-memory list to this page.
    pub fn slice<T: Clone>(&self, items: &[T]) -> Vec<T> {
        let start = (self.offset() as usize).min(items.len());
        let end = (start + self.page_size as usize).min(items.len());
        items[start..end].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_like_the_originals() {
        let p = Paging::resolve(
            None,
            Some(20),
            Some("airDateUtc"),
            Some(true),
            &["episodes.airDateUtc", "series.sortTitle"],
            "episodes.airDateUtc",
        );
        assert_eq!(p.page, 1);
        assert_eq!(p.sort_key, "episodes.airDateUtc");
        assert!(p.descending);
        let p = Paging::resolve(
            Some(2),
            Some(10),
            Some("SERIES.SORTTITLE"),
            None,
            &["series.sortTitle"],
            "date",
        );
        assert_eq!(p.sort_key, "series.sortTitle");
        assert_eq!(p.offset(), 10);
        assert!(p.descending);
        assert_eq!(p.slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]), vec![11, 12]);
    }
}
