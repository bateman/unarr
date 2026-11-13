//! Validation failures as the originals report them: a 400 whose body is a
//! list of failures. Two shapes appear in the oracle: rule failures from
//! FluentValidation (with `errorCode`, `attemptedValue` and placeholder
//! values) and failures raised by code, such as a connection test (with
//! `isWarning` and optional details). Both share `propertyName`,
//! `errorMessage` and `severity`.

use serde_json::{json, Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationFailure {
    pub property_name: String,
    pub error_message: String,
    /// Rule failures carry the value that failed; code failures do not.
    pub attempted_value: Option<Value>,
    /// The FluentValidation validator name (`NotEmptyValidator`,
    /// `PredicateValidator`...). `None` for failures raised by code.
    pub error_code: Option<String>,
    pub is_warning: bool,
    pub detailed_description: Option<String>,
    pub info_link: Option<String>,
    /// Extra `formattedMessagePlaceholderValues` some validators add
    /// (`comparisonValue` for `GreaterThanValidator`).
    pub placeholders: Vec<(String, Value)>,
    /// `context.AddFailure(message)` in a custom rule: serialised bare,
    /// with no `isWarning`.
    pub bare: bool,
}

impl ValidationFailure {
    /// A rule failure: `property` as the resource names it (`Name`,
    /// `BaseUrl`), `code` the validator, `message` the text with the display
    /// name already filled in.
    pub fn rule(property: &str, code: &str, message: &str, attempted: Value) -> ValidationFailure {
        ValidationFailure {
            property_name: property.to_string(),
            error_message: message.to_string(),
            attempted_value: Some(attempted),
            error_code: Some(code.to_string()),
            is_warning: false,
            detailed_description: None,
            info_link: None,
            placeholders: Vec::new(),
            bare: false,
        }
    }

    /// A settings rule failure (`*SettingsValidator`): the originals hand
    /// these back as `NzbDroneValidationFailure`s, with the attempted value
    /// but without the validator's code and placeholders.
    pub fn setting(property: &str, message: &str, attempted: Value) -> ValidationFailure {
        ValidationFailure {
            property_name: property.to_string(),
            error_message: message.to_string(),
            attempted_value: Some(attempted),
            error_code: None,
            is_warning: false,
            detailed_description: None,
            info_link: None,
            placeholders: Vec::new(),
            bare: false,
        }
    }

    /// `'Base Url' must not be empty.` as a settings rule.
    pub fn setting_not_empty(property: &str, attempted: Value) -> ValidationFailure {
        Self::setting(
            property,
            &format!("'{}' must not be empty.", display_name(property)),
            attempted,
        )
    }

    /// `'Indexer Id' must be greater than '0'.` for a `GreaterThanValidator`.
    pub fn greater_than(property: &str, attempted: Value, comparison: i64) -> ValidationFailure {
        let mut f = Self::rule(
            property,
            "GreaterThanValidator",
            &format!(
                "'{}' must be greater than '{comparison}'.",
                display_name(property)
            ),
            attempted,
        );
        f.placeholders = vec![
            ("comparisonProperty".into(), json!("")),
            ("comparisonValue".into(), json!(comparison)),
        ];
        f
    }

    /// `'Name' must not be empty.` for a `NotEmptyValidator`.
    pub fn not_empty(property: &str, attempted: Value) -> ValidationFailure {
        Self::rule(
            property,
            "NotEmptyValidator",
            &format!("'{}' must not be empty.", display_name(property)),
            attempted,
        )
    }

    /// `'Fields' must not be empty.` for a `NotNullValidator`.
    pub fn not_null(property: &str) -> ValidationFailure {
        Self::rule(
            property,
            "NotNullValidator",
            &format!("'{}' must not be empty.", display_name(property)),
            Value::Null,
        )
    }

    /// A failure raised by code: a test that could not connect, a setting
    /// that does not fit.
    pub fn error(property: &str, message: &str) -> ValidationFailure {
        ValidationFailure {
            property_name: property.to_string(),
            error_message: message.to_string(),
            attempted_value: None,
            error_code: None,
            is_warning: false,
            detailed_description: None,
            info_link: None,
            placeholders: Vec::new(),
            bare: false,
        }
    }

    /// A failure a custom rule adds by message only.
    pub fn bare(property: &str, message: &str) -> ValidationFailure {
        ValidationFailure {
            bare: true,
            ..Self::error(property, message)
        }
    }

    pub fn warning(property: &str, message: &str) -> ValidationFailure {
        ValidationFailure {
            is_warning: true,
            ..Self::error(property, message)
        }
    }

    pub fn with_detail(mut self, detail: &str) -> ValidationFailure {
        self.detailed_description = Some(detail.to_string());
        self
    }

    /// The JSON object the API body lists.
    pub fn to_json(&self) -> Value {
        if self.bare {
            return self.to_plain_json();
        }
        let mut m = Map::new();
        if let Some(code) = &self.error_code {
            // FluentValidation shape; a null attempted value is left out of
            // the top level (the serializer skips nulls) but kept among the
            // placeholders (a dictionary keeps them).
            if let Some(attempted) = self.attempted_value.clone().filter(|v| !v.is_null()) {
                m.insert("attemptedValue".into(), attempted);
            }
            m.insert("errorCode".into(), json!(code));
            m.insert("errorMessage".into(), json!(self.error_message));
            m.insert("formattedMessageArguments".into(), json!([]));
            let mut placeholders = Map::new();
            for (k, v) in &self.placeholders {
                placeholders.insert(k.clone(), v.clone());
            }
            placeholders.insert("propertyName".into(), json!(display_name(&self.property_name)));
            placeholders.insert(
                "propertyValue".into(),
                self.attempted_value.clone().unwrap_or(Value::Null),
            );
            m.insert(
                "formattedMessagePlaceholderValues".into(),
                Value::Object(placeholders),
            );
            m.insert("propertyName".into(), json!(self.property_name));
            m.insert("severity".into(), json!("error"));
        } else {
            // A settings rule keeps its attempted value, and a warning its
            // custom state; a failure raised by code has neither.
            if let Some(attempted) = self.attempted_value.clone().filter(|v| !v.is_null()) {
                m.insert("attemptedValue".into(), attempted);
            }
            if self.is_warning && self.attempted_value.is_some() {
                m.insert("customState".into(), json!({"isWarning": true}));
            }
            if let Some(detail) = &self.detailed_description {
                m.insert("detailedDescription".into(), json!(detail));
            }
            m.insert("errorMessage".into(), json!(self.error_message));
            if let Some(link) = &self.info_link {
                m.insert("infoLink".into(), json!(link));
            }
            m.insert("isWarning".into(), json!(self.is_warning));
            m.insert("propertyName".into(), json!(self.property_name));
            m.insert("severity".into(), json!("error"));
        }
        Value::Object(m)
    }
}

/// FluentValidation's display name: `BaseUrl` becomes `Base Url`.
impl ValidationFailure {
    /// The JSON `testall` lists: the originals serialise the bare
    /// `ValidationFailure` there, so only what is set shows (no
    /// `isWarning`, no detail).
    pub fn to_plain_json(&self) -> Value {
        let mut m = Map::new();
        if let Some(attempted) = self.attempted_value.clone().filter(|v| !v.is_null()) {
            m.insert("attemptedValue".into(), attempted);
        }
        if let Some(code) = &self.error_code {
            m.insert("errorCode".into(), json!(code));
        }
        m.insert("errorMessage".into(), json!(self.error_message));
        if self.error_code.is_some() {
            m.insert("formattedMessageArguments".into(), json!([]));
            let mut placeholders = Map::new();
            for (k, v) in &self.placeholders {
                placeholders.insert(k.clone(), v.clone());
            }
            placeholders.insert("propertyName".into(), json!(display_name(&self.property_name)));
            placeholders.insert(
                "propertyValue".into(),
                self.attempted_value.clone().unwrap_or(Value::Null),
            );
            m.insert(
                "formattedMessagePlaceholderValues".into(),
                Value::Object(placeholders),
            );
        }
        m.insert("propertyName".into(), json!(self.property_name));
        m.insert("severity".into(), json!("error"));
        Value::Object(m)
    }
}

/// FluentValidation's `SplitPascalCase`: a space before a capital that
/// follows a small letter, and before the last capital of a run that a
/// small letter follows (`UILanguage` is `UI Language`).
pub fn display_name(property: &str) -> String {
    let chars: Vec<char> = property.chars().collect();
    let mut out = String::with_capacity(property.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            let prev = chars[i - 1];
            let next_small = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if !prev.is_ascii_uppercase() || next_small {
                out.push(' ');
            }
        }
        out.push(c);
    }
    out
}

/// Whether a list of failures has errors (warnings do not count).
pub fn has_errors(failures: &[ValidationFailure]) -> bool {
    failures.iter().any(|f| !f.is_warning)
}

/// The failures to report: everything when warnings are included,
/// otherwise only errors; `None` when nothing stops the request.
pub fn blocking(failures: Vec<ValidationFailure>, include_warnings: bool) -> Option<Vec<ValidationFailure>> {
    if include_warnings && !failures.is_empty() {
        return Some(failures);
    }
    let errors: Vec<ValidationFailure> = failures.into_iter().filter(|f| !f.is_warning).collect();
    if errors.is_empty() {
        None
    } else {
        Some(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_match_the_oracle() {
        let f = ValidationFailure::not_empty("Name", json!(""));
        assert_eq!(
            f.to_json(),
            json!({"attemptedValue": "", "errorCode": "NotEmptyValidator", "errorMessage": "'Name' must not be empty.",
                   "formattedMessageArguments": [], "formattedMessagePlaceholderValues": {"propertyName": "Name", "propertyValue": ""},
                   "propertyName": "Name", "severity": "error"})
        );
        let f = ValidationFailure::error("ApiKey", "Invalid API Key");
        assert_eq!(
            f.to_json(),
            json!({"errorMessage": "Invalid API Key", "isWarning": false, "propertyName": "ApiKey", "severity": "error"})
        );
        assert_eq!(display_name("BaseUrl"), "Base Url");
        let w = ValidationFailure::warning("", "warn");
        assert!(!has_errors(std::slice::from_ref(&w)));
        assert!(blocking(vec![w.clone()], false).is_none());
        assert_eq!(blocking(vec![w], true).map(|v| v.len()), Some(1));
    }
}
