//! Field schemas of pluggable implementations (indexers, download clients,
//! notifications, import lists, metadata, custom format and auto-tagging
//! specifications), recorded from the real Sonarr's `*/schema` endpoints by
//! `make golden` and embedded here. They supply the labels, help texts and
//! select options the API returns next to every stored `settings` value.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::{json, Map, Value};

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

/// `SchemaBuilder.ReadFromSchema`: settings from a `fields` list. Every
/// schema field starts at its default (the value the schema endpoint shows),
/// each field sent overrides it, converted to the default's JSON type as the
/// originals convert by property type; a masked secret (`********`) keeps
/// the existing value. Fields the schema does not know are ignored.
pub fn read_fields(schema: &Value, fields: &Value, existing: Option<&Value>) -> Value {
    let mut settings = Map::new();
    let schema_fields = schema["fields"].as_array().cloned().unwrap_or_default();
    for f in &schema_fields {
        if let Some(name) = f["name"].as_str() {
            set_path(
                &mut settings,
                name,
                f.get("value").cloned().unwrap_or(Value::Null),
            );
        }
    }
    let empty = Map::new();
    let existing = existing.and_then(Value::as_object).unwrap_or(&empty);
    for sent in fields.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let Some(name) = sent["name"].as_str() else {
            continue;
        };
        let Some(schema_field) = schema_fields.iter().find(|f| f["name"].as_str() == Some(name)) else {
            continue;
        };
        let privacy = schema_field["privacy"].as_str().unwrap_or("normal");
        let value = sent.get("value").cloned().unwrap_or(Value::Null);
        let value = if matches!(privacy, "apiKey" | "password") && value.as_str() == Some("********") {
            lookup_path(existing, name).cloned().unwrap_or(Value::Null)
        } else {
            convert_like(schema_field, value)
        };
        set_path(&mut settings, name, value);
    }
    Value::Object(settings)
}

/// Convert `value` to the type the schema field declares (`type`, `isFloat`
/// and the default's JSON type), as the schema reader converts a field to
/// its property type.
fn convert_like(field: &Value, value: Value) -> Value {
    let default = field.get("value").filter(|v| !v.is_null());
    let kind = field["type"].as_str().unwrap_or("textbox");
    let is_float = field["isFloat"].as_bool().unwrap_or(false)
        || matches!(default, Some(Value::Number(n)) if n.is_f64());
    let numeric = kind == "number" || matches!(default, Some(Value::Number(_)));
    let boolean = kind == "checkbox" || matches!(default, Some(Value::Bool(_)));
    let list = matches!(default, Some(Value::Array(_)));
    match value {
        Value::String(s) if numeric => {
            let s = s.trim();
            if s.is_empty() {
                Value::Null
            } else if is_float || s.contains('.') {
                s.parse::<f64>().map(|f| json!(f)).unwrap_or(Value::Null)
            } else {
                s.parse::<i64>().map(|i| json!(i)).unwrap_or(Value::Null)
            }
        }
        Value::Null if numeric && default.is_some() => json!(0),
        Value::String(s) if boolean => json!(s.trim().eq_ignore_ascii_case("true")),
        Value::Null if boolean => json!(false),
        Value::String(s) if list => Value::Array(
            s.split(',')
                .filter_map(|p| p.trim().parse::<i64>().ok().map(|i| json!(i)))
                .collect(),
        ),
        Value::Null if list => json!([]),
        v => v,
    }
}

/// `seedCriteria.seedRatio` writes `settings.seedCriteria.seedRatio`.
fn set_path(settings: &mut Map<String, Value>, name: &str, value: Value) {
    let mut parts: Vec<&str> = name.split('.').collect();
    let last = parts.pop().unwrap_or(name);
    let mut current = settings;
    for key in parts {
        let entry = current
            .entry(key.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        current = entry.as_object_mut().unwrap();
    }
    current.insert(last.to_string(), value);
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
    fn fields_are_read_back_into_settings() {
        let torznab = lookup(SchemaKind::Provider(ProviderKind::Indexer), "Torznab").unwrap();
        let settings = read_fields(
            torznab,
            &json!([
                {"name": "baseUrl", "value": "http://127.0.0.1:18989/torznab"},
                {"name": "apiKey", "value": "********"},
                {"name": "minimumSeeders", "value": "3"},
                {"name": "categories", "value": [5030, 5040]},
                {"name": "seedCriteria.seedRatio", "value": "1.5"},
                {"name": "unknownField", "value": 1},
            ]),
            Some(&json!({"apiKey": "kept"})),
        );
        assert_eq!(settings["baseUrl"], "http://127.0.0.1:18989/torznab");
        assert_eq!(settings["apiKey"], "kept");
        assert_eq!(settings["apiPath"], "/api");
        assert_eq!(settings["minimumSeeders"], 3);
        assert_eq!(settings["seedCriteria"]["seedRatio"], 1.5);
        assert_eq!(settings["seedCriteria"]["seedTime"], Value::Null);
        assert_eq!(settings["animeStandardFormatSearch"], false);
        assert!(settings.get("unknownField").is_none());
        let newznab = lookup(SchemaKind::Provider(ProviderKind::Indexer), "Newznab").unwrap();
        let fresh = read_fields(newznab, &json!([{"name": "apiKey", "value": "********"}]), None);
        assert_eq!(fresh["apiKey"], Value::Null);
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
