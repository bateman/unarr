//! Newznab `t=caps`: what an indexer says it supports.

use roxmltree::Document;

use crate::indexers::rss::check_error;
use crate::indexers::IndexerError;

#[derive(Debug, Clone, PartialEq)]
pub struct Category {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub subcategories: Vec<Category>,
}

/// `NewznabCapabilities`, with the defaults older indexers are assumed to have.
#[derive(Debug, Clone, PartialEq)]
pub struct Capabilities {
    pub default_page_size: i64,
    pub max_page_size: i64,
    /// `None` when `search` is unavailable.
    pub supported_search_parameters: Option<Vec<String>>,
    /// `None` when `tv-search` is unavailable.
    pub supported_tv_search_parameters: Option<Vec<String>>,
    pub supports_aggregate_id_search: bool,
    pub text_search_engine: String,
    pub tv_text_search_engine: String,
    pub categories: Vec<Category>,
}

impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            default_page_size: 100,
            max_page_size: 100,
            supported_search_parameters: Some(vec!["q".into()]),
            // `rid` stays for older newznab installs, as the originals note.
            supported_tv_search_parameters: Some(vec![
                "q".into(),
                "rid".into(),
                "season".into(),
                "ep".into(),
            ]),
            supports_aggregate_id_search: false,
            text_search_engine: "sphinx".into(),
            tv_text_search_engine: "sphinx".into(),
            categories: Vec::new(),
        }
    }
}

impl Capabilities {
    pub fn supports_search_param(&self, p: &str) -> bool {
        self.supported_search_parameters
            .as_ref()
            .is_some_and(|v| v.iter().any(|x| x == p))
    }

    pub fn supports_tv_param(&self, p: &str) -> bool {
        self.supported_tv_search_parameters
            .as_ref()
            .is_some_and(|v| v.iter().any(|x| x == p))
    }

    /// `Newznab.GetProviderPageSize`.
    pub fn page_size(&self) -> i64 {
        100.min(self.default_page_size.max(self.max_page_size))
    }
}

/// Parse a `caps` document. An `<error>` becomes the matching
/// [`IndexerError`]; anything unexpected keeps the defaults, as the
/// originals do ("using the defaults instead").
pub fn parse_caps(xml: &str, request_url: &str) -> Result<Capabilities, IndexerError> {
    let doc = Document::parse(xml).map_err(|e| IndexerError::Xml(e.to_string()))?;
    check_error(&doc, request_url)?;
    let root = doc.root_element();
    if root.tag_name().name() != "caps" {
        return Err(IndexerError::Xml("Unexpected XML".into()));
    }
    let mut caps = Capabilities::default();
    if let Some(limits) = root.children().find(|n| n.has_tag_name("limits")) {
        if let Some(v) = limits.attribute("default").and_then(|v| v.parse().ok()) {
            caps.default_page_size = v;
        }
        if let Some(v) = limits.attribute("max").and_then(|v| v.parse().ok()) {
            caps.max_page_size = v;
        }
    }
    if let Some(searching) = root.children().find(|n| n.has_tag_name("searching")) {
        let split = |v: &str| v.split(',').map(str::to_string).collect::<Vec<_>>();
        match searching.children().find(|n| n.has_tag_name("search")) {
            Some(s) if s.attribute("available") == Some("yes") => {
                if let Some(p) = s.attribute("supportedParams") {
                    caps.supported_search_parameters = Some(split(p));
                }
                if let Some(e) = s.attribute("searchEngine") {
                    caps.text_search_engine = e.to_string();
                }
            }
            _ => caps.supported_search_parameters = None,
        }
        match searching.children().find(|n| n.has_tag_name("tv-search")) {
            Some(s) if s.attribute("available") == Some("yes") => {
                if let Some(p) = s.attribute("supportedParams") {
                    caps.supported_tv_search_parameters = Some(split(p));
                    caps.supports_aggregate_id_search = true;
                }
                if let Some(e) = s.attribute("searchEngine") {
                    caps.tv_text_search_engine = e.to_string();
                }
            }
            _ => caps.supported_tv_search_parameters = None,
        }
    }
    if let Some(categories) = root.children().find(|n| n.has_tag_name("categories")) {
        for cat in categories.children().filter(|n| n.has_tag_name("category")) {
            let Some(id) = cat.attribute("id").and_then(|v| v.parse().ok()) else {
                continue;
            };
            let mut category = Category {
                id,
                name: cat.attribute("name").unwrap_or("").to_string(),
                description: cat.attribute("description").unwrap_or("").to_string(),
                subcategories: Vec::new(),
            };
            for sub in cat.children().filter(|n| n.has_tag_name("subcat")) {
                if let Some(id) = sub.attribute("id").and_then(|v| v.parse().ok()) {
                    category.subcategories.push(Category {
                        id,
                        name: sub.attribute("name").unwrap_or("").to_string(),
                        description: sub.attribute("description").unwrap_or("").to_string(),
                        subcategories: Vec::new(),
                    });
                }
            }
            caps.categories.push(category);
        }
    }
    Ok(caps)
}

/// The categories the originals fall back to when `caps` cannot be fetched.
pub fn default_categories() -> Vec<Category> {
    let sub = |id: i64, name: &str| Category {
        id,
        name: name.into(),
        description: String::new(),
        subcategories: Vec::new(),
    };
    vec![Category {
        id: 5000,
        name: "TV".into(),
        description: String::new(),
        subcategories: vec![
            sub(5070, "Anime"),
            sub(5080, "Documentary"),
            sub(5020, "Foreign"),
            sub(5040, "HD"),
            sub(5045, "UHD"),
            sub(5050, "Other"),
            sub(5030, "SD"),
            sub(5060, "Sport"),
            sub(5010, "WEB-DL"),
        ],
    }]
}

/// `NewznabCategoryFieldOptionsConverter.GetFieldSelectOptions`: the select
/// options the `newznabCategories` action answers with.
pub fn category_options(categories: Option<&[Category]>) -> Vec<serde_json::Value> {
    use serde_json::json;
    let ignore = [1000, 3000, 4000, 6000, 7000];
    let unimportant = [0, 2000];
    let fallback = default_categories();
    let mut cats: Vec<&Category> = categories
        .unwrap_or(&fallback)
        .iter()
        .filter(|c| !ignore.contains(&c.id))
        .collect();
    cats.sort_by_key(|c| (unimportant.contains(&c.id), c.id));
    let mut out = Vec::new();
    for c in cats {
        out.push(json!({"value": c.id, "name": c.name, "order": 0, "hint": format!("({})", c.id)}));
        let mut subs: Vec<&Category> = c.subcategories.iter().collect();
        subs.sort_by_key(|s| s.id);
        for s in subs {
            out.push(json!({"value": s.id, "name": s.name, "order": 0, "hint": format!("({})", s.id), "parentValue": c.id}));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_fixture_caps() {
        let xml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/indexers/newznab/caps.xml"
        ))
        .unwrap();
        let caps = parse_caps(&xml, "http://127.0.0.1:18989/newznab/api?t=caps&apikey=k").unwrap();
        assert!(caps.supports_tv_param("tvdbid"));
        assert!(caps.supports_aggregate_id_search);
        assert_eq!(caps.page_size(), 100);
        assert_eq!(caps.categories[1].id, 5000);
        let options = category_options(Some(&caps.categories));
        assert_eq!(options[0]["value"], 5000); // Movies (2000) sorts after TV
        assert_eq!(options.last().unwrap()["parentValue"], 2000);
        assert!(matches!(
            parse_caps(
                "<error code=\"100\" description=\"Incorrect user credentials\"/>",
                "u"
            ),
            Err(IndexerError::ApiKey(_))
        ));
        assert_eq!(category_options(None).len(), 10);
    }
}
