//! Declarative schema types for the Orbit UI service.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One selectable option in a select/dropdown control.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiOption {
    pub label: String,
    pub value: Value,
}

/// A control or form element within a UI section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiItem {
    pub key: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(default)]
    pub options: Option<Vec<UiOption>>,
    #[serde(default)]
    pub multiline: Option<bool>,
}

/// A top-level panel, tab, or card registered by a plugin.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiSection {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub order: Option<i32>,
    #[serde(default)]
    pub items: Vec<UiItem>,
}

/// An interaction event emitted by the UI window when the user modifies a control.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiAction {
    pub section: String,
    pub key: String,
    #[serde(rename = "type")]
    pub action_type: String,
    #[serde(default)]
    pub value: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_round_trips_json() {
        let json = r#"{
            "id": "com.example.test",
            "title": "Test Section",
            "description": "Testing declarative schema",
            "order": 10,
            "items": [
                {
                    "key": "enabled",
                    "type": "toggle",
                    "label": "Enable",
                    "default": true
                },
                {
                    "key": "codec",
                    "type": "select",
                    "label": "Codec",
                    "options": [
                        {"label": "HEVC", "value": "hevc"},
                        {"label": "AV1", "value": "av1"}
                    ],
                    "default": "hevc"
                },
                {
                    "key": "notes",
                    "type": "text_input",
                    "label": "Notes",
                    "multiline": true
                },
                {
                    "key": "log",
                    "type": "text_view",
                    "label": "Log"
                }
            ]
        }"#;

        let section: UiSection = serde_json::from_str(json).expect("valid schema");
        assert_eq!(section.id, "com.example.test");
        assert_eq!(section.items.len(), 4);
        assert_eq!(section.items[0].key, "enabled");
        assert_eq!(section.items[0].item_type, "toggle");
        assert_eq!(section.items[1].options.as_ref().unwrap().len(), 2);
        assert_eq!(section.items[2].multiline, Some(true));
        assert_eq!(section.items[3].item_type, "text_view");
    }
}
