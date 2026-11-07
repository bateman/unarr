//! Field schemas of pluggable implementations (indexers, download clients,
//! notifications, import lists, metadata, custom format and auto-tagging
//! specifications), recorded from the real Sonarr's `*/schema` endpoints by
//! `make golden` and embedded here. They supply the labels, help texts and
//! select options the API returns next to every stored `settings` value.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::{Map, Value};

use unarr_core::model::ProviderKind;

const INDEXER: &str = include_str!("../schemas/sonarr/indexer-schema.json");
const DOWNLOAD_CLIENT: &str = include_str!("../schemas/sonarr/downloadclient-schema.json");
const NOTIFICATION: &str = include_str!("../schemas/sonarr/notification-schema.json");
const IMPORT_LIST: &str = include_str!("../schemas/sonarr/importlist-schema.json");
const METADATA: &str = include_str!("../schemas/sonarr/metadata-schema.json");
const CUSTOM_FORMAT: &str = include_str!("../schemas/sonarr/customformat-schema.json");
const AUTO_TAGGING: &str = include_str!("../schemas/sonarr/autotagging-schema.json");

/// Which family of schemas to look in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SchemaKind {
    Provider(ProviderKind),
    CustomFormatSpecification,
    AutoTaggingSpecification,
}

struct Catalogue {
    by_kind: HashMap<SchemaKind, Vec<Value>>,
}

fn catalogue() -> &'static Catalogue {
    static CATALOGUE: OnceLock<Catalogue> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        let parse =
            |s: &str| -> Vec<Value> { serde_json::from_str(s).expect("embedded schema is valid JSON") };
        let mut by_kind = HashMap::new();
        by_kind.insert(SchemaKind::Provider(ProviderKind::Indexer), parse(INDEXER));
        by_kind.insert(
            SchemaKind::Provider(ProviderKind::DownloadClient),
            parse(DOWNLOAD_CLIENT),
        );
        by_kind.insert(
            SchemaKind::Provider(ProviderKind::Notification),
            parse(NOTIFICATION),
        );
        by_kind.insert(SchemaKind::Provider(ProviderKind::ImportList), parse(IMPORT_LIST));
        by_kind.insert(SchemaKind::Provider(ProviderKind::Metadata), parse(METADATA));
        by_kind.insert(SchemaKind::CustomFormatSpecification, parse(CUSTOM_FORMAT));
        by_kind.insert(SchemaKind::AutoTaggingSpecification, parse(AUTO_TAGGING));
        Catalogue { by_kind }
    })
}

/// All schema entries of a kind, as the `*/schema` endpoints return them.
pub fn all(kind: SchemaKind) -> &'static [Value] {
    catalogue().by_kind.get(&kind).map(Vec::as_slice).unwrap_or(&[])
}

/// The schema entry for one implementation.
pub fn lookup(kind: SchemaKind, implementation: &str) -> Option<&'static Value> {
    all(kind)
        .iter()
        .find(|s| s["implementation"].as_str() == Some(implementation))
}

/// Build the `fields` list for an implementation from its stored settings:
/// the schema's field metadata, each with `value` taken from `settings` by
/// field name. Secrets (`privacy` apiKey/password) are masked as the
/// originals do on read.
pub fn render_fields(schema: &Value, settings: &Value) -> Vec<Value> {
    let empty = Map::new();
    let settings = settings.as_object().unwrap_or(&empty);
    schema["fields"]
        .as_array()
        .map(|fields| {
            fields
                .iter()
                .map(|f| {
                    let mut field = f.clone();
                    if let Some(obj) = field.as_object_mut() {
                        let name = obj.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                        let privacy = obj.get("privacy").and_then(Value::as_str).unwrap_or("normal");
                        match lookup_path(settings, &name) {
                            Some(Value::Null) | None => {
                                obj.remove("value");
                            }
                            Some(v)
                                if matches!(privacy, "apiKey" | "password")
                                    && v.as_str().is_some_and(|s| !s.is_empty()) =>
                            {
                                obj.insert("value".into(), Value::String("********".into()));
                            }
                            Some(v) => {
                                obj.insert("value".into(), v.clone());
                            }
                        }
                    }
                    field
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A field named `seedCriteria.seedRatio` reads `settings.seedCriteria.seedRatio`.
fn lookup_path<'a>(settings: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    let mut parts = name.split('.');
    let mut current = settings.get(parts.next()?)?;
    for key in parts {
        current = current.get(key)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_field_names_read_nested_settings() {
        let torznab = lookup(SchemaKind::Provider(ProviderKind::Indexer), "Torznab").unwrap();
        let fields = render_fields(
            torznab,
            &serde_json::json!({"seedCriteria": {"seedRatio": 2.5, "seedTime": 60}, "minimumSeeders": 1}),
        );
        let by_name = |n: &str| fields.iter().find(|f| f["name"] == n).unwrap().clone();
        assert_eq!(by_name("seedCriteria.seedRatio")["value"], 2.5);
        assert_eq!(by_name("seedCriteria.seedTime")["value"], 60);
        assert!(by_name("seedCriteria.seasonPackSeedTime").get("value").is_none());
        assert_eq!(by_name("minimumSeeders")["value"], 1);
    }

    #[test]
    fn schemas_load_and_render_values() {
        let newznab = lookup(SchemaKind::Provider(ProviderKind::Indexer), "Newznab").unwrap();
        assert_eq!(newznab["configContract"], "NewznabSettings");
        let fields = render_fields(
            newznab,
            &serde_json::json!({"baseUrl": "http://indexer.invalid", "apiKey": "secret", "categories": [5030]}),
        );
        let by_name = |n: &str| fields.iter().find(|f| f["name"] == n).unwrap().clone();
        assert_eq!(by_name("baseUrl")["value"], "http://indexer.invalid");
        assert_eq!(by_name("apiKey")["value"], "********");
        assert_eq!(by_name("categories")["value"], serde_json::json!([5030]));
        assert!(by_name("additionalParameters").get("value").is_none());
        assert!(lookup(SchemaKind::CustomFormatSpecification, "ReleaseTitleSpecification").is_some());
        assert!(lookup(SchemaKind::Provider(ProviderKind::Notification), "Nope").is_none());
    }
}
