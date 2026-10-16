//! The row model of the interim widget renderer.
//!
//! The renderer predates schema version 2 and draws every setting as a label above one
//! widget. It is what macOS uses until its native shell is finished; Windows renders
//! the schema with the Fluent shell. The reduction from a page to rows is plain data,
//! so it is tested on every platform.

use parsec_orbit_ui_protocol::schema::{Element, UiControl};
use parsec_orbit_ui_protocol::{PageState, UiOption};
use serde_json::Value;

/// One row of the interim renderer: a page element reduced to a widget and a label.
///
/// This renderer predates schema version 2 and draws every row the same way.
/// It is kept for platforms whose native shell is not finished yet.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UiItem {
    pub(crate) key: String,
    pub(crate) item_type: String,
    pub(crate) label: String,
    pub(crate) description: Option<String>,
    pub(crate) default: Option<Value>,
    pub(crate) min: Option<f64>,
    pub(crate) max: Option<f64>,
    pub(crate) step: Option<f64>,
    pub(crate) options: Option<Vec<UiOption>>,
    pub(crate) multiline: Option<bool>,
}

impl UiItem {
    pub(crate) fn new(key: &str, item_type: &str, label: &str) -> Self {
        Self {
            key: key.to_owned(),
            item_type: item_type.to_owned(),
            label: label.to_owned(),
            description: None,
            default: None,
            min: None,
            max: None,
            step: None,
            options: None,
            multiline: None,
        }
    }
}

/// Reduces a page to rows, with select options taken from the current state.
pub(crate) fn flatten(state: &PageState) -> Vec<UiItem> {
    let mut items = Vec::new();
    for element in state.page.elements() {
        match element {
            Element::Banner(banner) => {
                let mut item = UiItem::new(
                    &banner.key,
                    "readonly_text",
                    banner.title.as_deref().unwrap_or("Notice"),
                );
                item.default = Some(Value::String(banner.message.clone()));
                items.push(item);
            }
            Element::Button(button) => {
                items.push(UiItem::new(&button.key, "button", &button.text));
            }
            Element::Setting(setting) => {
                let mut item = UiItem::new(&setting.key, "readonly_text", &setting.label);
                item.description = setting.description.clone();
                item.default = setting.control.default_value();
                match &setting.control {
                    UiControl::Toggle { .. } => item.item_type = "toggle".into(),
                    UiControl::Number { min, max, step, .. } => {
                        item.item_type = "number_input".into();
                        (item.min, item.max, item.step) = (*min, *max, *step);
                    }
                    UiControl::Slider { min, max, step, .. } => {
                        item.item_type = "slider".into();
                        (item.min, item.max, item.step) = (Some(*min), Some(*max), *step);
                    }
                    UiControl::Select { .. } => {
                        item.item_type = "select".into();
                        item.options = state.effective_options(&setting.key).map(<[_]>::to_vec);
                    }
                    UiControl::Text { multiline, .. } => {
                        item.item_type = "text_input".into();
                        item.multiline = Some(*multiline);
                    }
                    UiControl::Json { .. } => item.item_type = "json_editor".into(),
                    UiControl::Button { text, .. } => {
                        item.item_type = "button".into();
                        item.label = text.clone();
                    }
                    UiControl::Value { .. } => {}
                    UiControl::Log { .. } => item.item_type = "text_view".into(),
                }
                items.push(item);
            }
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_ui_protocol::{UiPage, UiPatch};

    fn state() -> PageState {
        PageState::new(
            UiPage::parse(
                &serde_json::json!({
                    "schema": 2, "id": "t.page", "title": "T",
                    "blocks": [
                        {"type": "banner", "key": "note", "severity": "info", "title": "Heads up", "message": "m"},
                        {"type": "group", "settings": [
                            {"key": "on", "label": "On", "control": {"type": "toggle", "default": true}},
                            {"key": "pick", "label": "Pick", "control": {"type": "select", "default": "a",
                                "options": [{"label": "A", "value": "a"}]}},
                            {"key": "go", "label": "Go", "control": {"type": "button", "text": "Run"}}
                        ]},
                        {"type": "actions", "buttons": [{"key": "reset", "text": "Reset"}]}
                    ]
                })
                .to_string(),
                false,
            )
            .unwrap(),
        )
    }

    #[test]
    fn a_page_flattens_into_rows_in_document_order() {
        let items = flatten(&state());
        let rows: Vec<_> = items
            .iter()
            .map(|item| {
                (
                    item.key.as_str(),
                    item.item_type.as_str(),
                    item.label.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("note", "readonly_text", "Heads up"),
                ("on", "toggle", "On"),
                ("pick", "select", "Pick"),
                ("go", "button", "Run"),
                ("reset", "button", "Reset"),
            ]
        );
        assert_eq!(items[1].default, Some(Value::Bool(true)));
    }

    #[test]
    fn replaced_select_options_change_the_layout() {
        let mut state = state();
        let before = flatten(&state);
        let patch: UiPatch = serde_json::from_value(serde_json::json!({
            "props": {"pick": {"options": [{"label": "B", "value": "b"}]}}
        }))
        .unwrap();
        state.apply_patch(patch).unwrap();
        let after = flatten(&state);
        assert_ne!(before, after);
        assert_eq!(after[2].options.as_ref().unwrap()[0].label, "B");
    }
}
