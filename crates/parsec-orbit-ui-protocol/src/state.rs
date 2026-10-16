//! A page's authoritative state: values, presentation properties and patches.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::schema::{
    check_optional, check_options, check_value, number_value, Element, UiAction, UiControl,
    UiOption, UiPage, UiSeverity,
};

/// Status text shown with a setting, such as a validation error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiStatus {
    pub severity: UiSeverity,
    pub text: String,
}

/// Presentation changes for one element. Absent members change nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<UiOption>>,
    /// `Some(None)` is an explicit `null` that clears the status.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<Option<UiStatus>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<UiSeverity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

impl UiProps {
    /// The current status, if one is set.
    pub fn status(&self) -> Option<&UiStatus> {
        self.status.as_ref().and_then(Option::as_ref)
    }

    fn merge(&mut self, other: UiProps) {
        fn take<T>(slot: &mut Option<T>, value: Option<T>) {
            if value.is_some() {
                *slot = value;
            }
        }
        take(&mut self.enabled, other.enabled);
        take(&mut self.visible, other.visible);
        take(&mut self.busy, other.busy);
        take(&mut self.label, other.label);
        take(&mut self.description, other.description);
        take(&mut self.text, other.text);
        take(&mut self.options, other.options);
        take(&mut self.status, other.status);
        take(&mut self.severity, other.severity);
        take(&mut self.title, other.title);
        take(&mut self.message, other.message);
        // A cleared status carries no information; keep the stored form small.
        if self.status == Some(None) {
            self.status = None;
        }
    }
}

/// The argument of `update_page`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiPatch {
    #[serde(default)]
    pub values: HashMap<String, Value>,
    #[serde(default)]
    pub props: HashMap<String, UiProps>,
    #[serde(default)]
    pub restart_required: Option<bool>,
}

/// A registered page with its values and presentation overrides.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageState {
    pub page: UiPage,
    pub values: HashMap<String, Value>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub props: HashMap<String, UiProps>,
    #[serde(default)]
    pub restart_required: bool,
}

impl PageState {
    /// A fresh page showing its declared defaults.
    pub fn new(page: UiPage) -> Self {
        Self {
            values: page.initial_values(),
            page,
            props: HashMap::new(),
            restart_required: false,
        }
    }

    /// The options a select currently offers.
    pub fn effective_options(&self, key: &str) -> Option<&[UiOption]> {
        match self.page.element(key)? {
            Element::Setting(setting) => match &setting.control {
                UiControl::Select { options, .. } => Some(
                    self.props
                        .get(key)
                        .and_then(|props| props.options.as_deref())
                        .unwrap_or(options),
                ),
                _ => None,
            },
            _ => None,
        }
    }

    /// Applies a patch entirely or not at all.
    pub fn apply_patch(&mut self, patch: UiPatch) -> Result<(), String> {
        let mut next = self.clone();
        next.apply(patch)?;
        *self = next;
        Ok(())
    }

    fn apply(&mut self, patch: UiPatch) -> Result<(), String> {
        let mut reoptioned = Vec::new();
        for (key, props) in patch.props {
            let element = self
                .page
                .element(&key)
                .ok_or_else(|| format!("props: no element {key:?}"))?;
            check_props(&key, element, &props)?;
            if props.options.is_some() {
                reoptioned.push(key.clone());
            }
            self.props.entry(key).or_default().merge(props);
        }
        for (key, value) in &patch.values {
            let Some(Element::Setting(setting)) = self.page.element(key) else {
                return Err(format!("values: {key:?} is not a setting"));
            };
            check_value(&setting.control, self.effective_options(key), value)
                .map_err(|error| format!("values: {key:?}: {error}"))?;
        }
        self.values.extend(patch.values);
        // A replaced option list drops a selection it no longer offers.
        for key in reoptioned {
            let stale = self.values.get(&key).is_some_and(|value| {
                self.effective_options(&key)
                    .is_some_and(|options| options.iter().all(|option| option.value != *value))
            });
            if stale {
                self.values.remove(&key);
            }
        }
        if let Some(required) = patch.restart_required {
            self.restart_required = required;
        }
        Ok(())
    }

    /// Whether an action from the renderer names a live element and carries a value that fits it.
    pub fn accepts_action(&self, action: &UiAction) -> bool {
        let click =
            action.action_type == "click" && action.value.as_ref().is_none_or(Value::is_null);
        match self.page.element(&action.key) {
            None | Some(Element::Banner(_)) => false,
            Some(Element::Button(_)) => click,
            Some(Element::Setting(setting)) => match &setting.control {
                UiControl::Button { .. } => click,
                control if control.is_editable() => {
                    action.action_type == "change"
                        && action.value.as_ref().is_some_and(|value| {
                            check_value(control, self.effective_options(&action.key), value).is_ok()
                        })
                }
                _ => false,
            },
        }
    }

    /// The value a renderer shows for a key, falling back to the declared default.
    pub fn value(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }
}

/// Normalizes a number from a renderer: integral numbers become JSON integers.
pub fn normalized_number(number: f64) -> Value {
    number_value(number)
}

fn check_props(key: &str, element: Element<'_>, props: &UiProps) -> Result<(), String> {
    let fail = |member: &str| Err(format!("props: {key:?} does not accept {member}"));
    let UiProps {
        enabled,
        visible: _,
        busy,
        label,
        description,
        text,
        options,
        status,
        severity,
        title,
        message,
    } = props;
    match element {
        Element::Banner(_) => {
            for (member, present) in [
                ("enabled", enabled.is_some()),
                ("busy", busy.is_some()),
                ("label", label.is_some()),
                ("description", description.is_some()),
                ("text", text.is_some()),
                ("options", options.is_some()),
                ("status", status.is_some()),
            ] {
                if present {
                    return fail(member);
                }
            }
        }
        Element::Button(_) => {
            for (member, present) in [
                ("label", label.is_some()),
                ("description", description.is_some()),
                ("options", options.is_some()),
                ("status", status.is_some()),
                ("severity", severity.is_some()),
                ("title", title.is_some()),
                ("message", message.is_some()),
            ] {
                if present {
                    return fail(member);
                }
            }
        }
        Element::Setting(setting) => {
            for (member, present) in [
                ("severity", severity.is_some()),
                ("title", title.is_some()),
                ("message", message.is_some()),
                (
                    "text",
                    text.is_some() && !matches!(setting.control, UiControl::Button { .. }),
                ),
                (
                    "options",
                    options.is_some() && !matches!(setting.control, UiControl::Select { .. }),
                ),
            ] {
                if present {
                    return fail(member);
                }
            }
        }
    }
    check_optional("label", label.as_deref(), 256)?;
    check_optional("description", description.as_deref(), 1024)?;
    check_optional("text", text.as_deref(), 256)?;
    check_optional("title", title.as_deref(), 256)?;
    check_optional("message", message.as_deref(), 1024)?;
    if let Some(Some(status)) = status {
        check_optional("status text", Some(&status.text), 1024)?;
    }
    if let Some(options) = options {
        check_options(options)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state() -> PageState {
        PageState::new(
            UiPage::parse(
                &json!({
                    "schema": 2, "id": "t.page", "title": "T",
                    "blocks": [
                        {"type": "banner", "key": "note", "severity": "info", "message": "m"},
                        {"type": "group", "settings": [
                            {"key": "on", "label": "On", "control": {"type": "toggle", "default": false}},
                            {"key": "n", "label": "N", "control": {"type": "number", "min": 1, "max": 10, "integer": true, "default": 5}},
                            {"key": "pick", "label": "P", "control": {"type": "select", "default": "a",
                                "options": [{"label": "A", "value": "a"}, {"label": "B", "value": "b"}]}},
                            {"key": "go", "label": "Go", "control": {"type": "button", "text": "Go"}},
                            {"key": "ro", "label": "R", "control": {"type": "value", "default": "x"}}
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

    fn patch(value: Value) -> UiPatch {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_patch_sets_values_properties_and_the_restart_flag() {
        let mut state = state();
        state
            .apply_patch(patch(json!({
                "values": {"on": true, "n": 7},
                "props": {
                    "n": {"enabled": false, "status": {"severity": "error", "text": "bad"}},
                    "note": {"severity": "success", "message": "done"},
                    "reset": {"busy": true, "text": "Resetting"}
                },
                "restart_required": true
            })))
            .unwrap();
        assert_eq!(state.values["on"], json!(true));
        assert_eq!(state.values["n"], json!(7));
        assert_eq!(state.props["n"].enabled, Some(false));
        assert_eq!(state.props["n"].status().unwrap().text, "bad");
        assert_eq!(state.props["reset"].text.as_deref(), Some("Resetting"));
        assert!(state.restart_required);

        // Later patches merge, and `null` clears a status.
        state
            .apply_patch(patch(
                json!({"props": {"n": {"enabled": true, "status": null}}}),
            ))
            .unwrap();
        assert_eq!(state.props["n"].enabled, Some(true));
        assert!(state.props["n"].status().is_none());
        state
            .apply_patch(patch(json!({"restart_required": false})))
            .unwrap();
        assert!(!state.restart_required);
    }

    #[test]
    fn a_rejected_patch_changes_nothing() {
        let mut state = state();
        let before = state.clone();
        for bad in [
            json!({"values": {"on": true, "n": 99}}),
            json!({"values": {"go": null}}),
            json!({"values": {"note": "x"}}),
            json!({"values": {"missing": 1}}),
            json!({"values": {"on": 1}}),
            json!({"props": {"missing": {"enabled": true}}}),
            json!({"props": {"note": {"enabled": false}}}),
            json!({"props": {"on": {"options": []}}}),
            json!({"props": {"on": {"text": "x"}}}),
            json!({"props": {"pick": {"options": []}}}),
            json!({"props": {"reset": {"label": "x"}}}),
            json!({"props": {"note": {"visible": true}}, "values": {"on": 5}}),
        ] {
            assert!(state.apply_patch(patch(bad.clone())).is_err(), "{bad}");
            assert_eq!(state, before, "{bad}");
        }
        assert!(
            serde_json::from_value::<UiPatch>(json!({"props": {"on": {"color": "red"}}})).is_err()
        );
        assert!(serde_json::from_value::<UiPatch>(json!({"vals": {}})).is_err());
    }

    #[test]
    fn replacing_options_keeps_a_present_selection_and_drops_an_absent_one() {
        let mut state = state();
        state
            .apply_patch(patch(json!({"props": {"pick": {"options": [
                {"label": "A", "value": "a"}, {"label": "C", "value": "c"}]}}})))
            .unwrap();
        assert_eq!(state.values["pick"], json!("a"));
        state
            .apply_patch(patch(
                json!({"props": {"pick": {"options": [{"label": "C", "value": "c"}]}}}),
            ))
            .unwrap();
        assert!(!state.values.contains_key("pick"));
        // The new options are what values are checked against, in the same patch too.
        state
            .apply_patch(patch(json!({
                "props": {"pick": {"options": [{"label": "D", "value": "d"}]}},
                "values": {"pick": "d"}
            })))
            .unwrap();
        assert_eq!(state.values["pick"], json!("d"));
        assert!(state
            .apply_patch(patch(json!({"values": {"pick": "a"}})))
            .is_err());
    }

    #[test]
    fn actions_must_name_a_live_element_with_a_fitting_value() {
        let mut state = state();
        let action = |key: &str, kind: &str, value: Option<Value>| UiAction {
            page: "t.page".into(),
            key: key.into(),
            action_type: kind.into(),
            value,
        };
        assert!(state.accepts_action(&action("on", "change", Some(json!(true)))));
        assert!(!state.accepts_action(&action("on", "change", Some(json!("yes")))));
        assert!(!state.accepts_action(&action("on", "click", None)));
        assert!(state.accepts_action(&action("n", "change", Some(json!(3)))));
        assert!(!state.accepts_action(&action("n", "change", Some(json!(3.5)))));
        assert!(!state.accepts_action(&action("n", "change", Some(json!(11)))));
        assert!(state.accepts_action(&action("go", "click", None)));
        assert!(state.accepts_action(&action("go", "click", Some(Value::Null))));
        assert!(!state.accepts_action(&action("go", "click", Some(json!(1)))));
        assert!(state.accepts_action(&action("reset", "click", None)));
        assert!(!state.accepts_action(&action("ro", "change", Some(json!("y")))));
        assert!(!state.accepts_action(&action("note", "click", None)));
        assert!(!state.accepts_action(&action("missing", "click", None)));
        state
            .apply_patch(patch(
                json!({"props": {"pick": {"options": [{"label": "D", "value": "d"}]}}}),
            ))
            .unwrap();
        assert!(state.accepts_action(&action("pick", "change", Some(json!("d")))));
        assert!(!state.accepts_action(&action("pick", "change", Some(json!("a")))));
    }

    #[test]
    fn state_round_trips_through_the_pipe_encoding() {
        let mut state = state();
        state
            .apply_patch(patch(
                json!({"props": {"n": {"status": {"severity": "warning", "text": "w"}}}}),
            ))
            .unwrap();
        let again: PageState =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(state, again);
    }
}
