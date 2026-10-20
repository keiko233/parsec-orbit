//! Renderer-neutral decisions of the companion: what the navigation lists, when the
//! restart notice shows, and how a user's input becomes a value or an inline error.
//!
//! Everything here is plain data, so it is tested on every platform. A renderer
//! only has to read its controls into these functions and show their answers
//! (`docs/spec/ui-service.md`, "State and editing").

use std::time::Duration;

use parsec_orbit_ui_protocol::schema::{
    check_value, number_value, Element, UiControl, UiIcon, UiNavGroup,
};
use parsec_orbit_ui_protocol::{PageState, UiAction};
use serde_json::Value;

/// The page whose `restart` button the restart notice presses.
pub(crate) const OVERVIEW: &str = "orbit.overview";

/// A slider reports once it has been still this long, not on every step of a drag.
pub(crate) const SLIDER_SETTLE: Duration = Duration::from_millis(250);

/// One page in the navigation, with what the sidebar shows of it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NavEntry {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) icon: Option<UiIcon>,
    pub(crate) group: UiNavGroup,
}

/// One line of the sidebar, in display order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NavRow<'a> {
    Page(&'a NavEntry),
    Separator,
    /// The heading over the plugin pages.
    Heading(&'static str),
}

pub(crate) fn nav_entries(pages: &[PageState]) -> Vec<NavEntry> {
    pages
        .iter()
        .map(|state| NavEntry {
            id: state.page.id.clone(),
            title: state.page.title.clone(),
            icon: state.page.icon,
            group: state.page.nav_group(),
        })
        .collect()
}

/// Lists the `orbit` pages, then a "Plugins" heading over the rest. The two
/// groups are divided by a separator only when both are present.
pub(crate) fn nav_rows(entries: &[NavEntry]) -> Vec<NavRow<'_>> {
    let (orbit, plugins): (Vec<_>, Vec<_>) = entries
        .iter()
        .partition(|entry| entry.group == UiNavGroup::Orbit);
    let mut rows: Vec<NavRow<'_>> = orbit.iter().map(|entry| NavRow::Page(entry)).collect();
    if !plugins.is_empty() {
        if !orbit.is_empty() {
            rows.push(NavRow::Separator);
        }
        rows.push(NavRow::Heading("Plugins"));
        rows.extend(plugins.iter().map(|entry| NavRow::Page(entry)));
    }
    rows
}

/// Whether the restart notice has something to offer: a page needs a restart and
/// the Overview has the button that performs it.
pub(crate) fn restart_offered(pages: &[PageState]) -> bool {
    pages.iter().any(|state| state.restart_required)
        && pages.iter().any(|state| {
            state.page.id == OVERVIEW
                && state.page.element("restart").is_some_and(|element| {
                    matches!(element, Element::Setting(_) | Element::Button(_))
                })
        })
}

/// The value the parent holds for a setting, or its declared default.
pub(crate) fn parent_value(state: &PageState, key: &str, control: &UiControl) -> Value {
    state
        .value(key)
        .cloned()
        .or_else(|| control.default_value())
        .unwrap_or(Value::Null)
}

/// Whether a parent value should replace what the user is editing: only when
/// it differs from the last value the parent set, so an unrelated update never
/// overwrites a draft.
pub(crate) fn replaces_draft(last_parent: &Value, parent: &Value) -> bool {
    last_parent != parent
}

/// A reason as a sentence: capitalized and ended by a period.
pub(crate) fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => {
            let sentence = format!("{}{}", first.to_uppercase(), chars.as_str());
            if sentence.ends_with('.') {
                sentence
            } else {
                format!("{sentence}.")
            }
        }
        None => String::new(),
    }
}

/// Plugins get `\n`; a text control may report `\r` or `\r\n`.
pub(crate) fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Reads typed text as the number of a `number` or `slider` control.
///
/// The text is refused, with a sentence for the inline error, when it is not a
/// finite number or does not fit the control.
pub(crate) fn number_edit(control: &UiControl, text: &str) -> Result<Value, String> {
    let number: f64 = text
        .trim()
        .parse()
        .ok()
        .filter(|number: &f64| number.is_finite())
        .ok_or_else(|| "Enter a number.".to_owned())?;
    let value = number_value(number);
    check_value(control, None, &value).map_err(|reason| capitalized(&reason))?;
    Ok(value)
}

/// Reads text as the object or array of a `json` control.
pub(crate) fn json_edit(text: &str) -> Result<Value, String> {
    match serde_json::from_str::<Value>(text) {
        Ok(value) if value.is_object() || value.is_array() => Ok(value),
        Ok(_) => Err("Expected a JSON object or array.".to_owned()),
        Err(error) => Err(format!("Not valid JSON: {error}.")),
    }
}

/// Builds the action for a user's edit (`Some`) or click (`None`) and checks that
/// the page accepts it; the error is the sentence to show next to the control.
pub(crate) fn user_action(
    state: &PageState,
    key: &str,
    value: Option<Value>,
) -> Result<UiAction, String> {
    let action = UiAction {
        page: state.page.id.clone(),
        key: key.to_owned(),
        action_type: if value.is_some() { "change" } else { "click" }.to_owned(),
        value: Some(value.unwrap_or(Value::Null)),
    };
    if state.accepts_action(&action) {
        return Ok(action);
    }
    let reason = match (state.page.element(key), &action.value) {
        (Some(Element::Setting(setting)), Some(value)) => {
            check_value(&setting.control, state.effective_options(key), value).err()
        }
        _ => None,
    };
    Err(reason.map_or_else(
        || "That value is not accepted.".to_owned(),
        |reason| capitalized(&reason),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use parsec_orbit_ui_protocol::{UiPage, UiPatch};

    fn state(id: &str, group: Option<&str>) -> PageState {
        let group = group.map_or(String::new(), |group| format!(r#""group":"{group}","#));
        PageState::new(
            UiPage::parse(
                &format!(
                    r#"{{"schema":2,"id":"{id}","title":"T {id}",{group}"blocks":[
                        {{"type":"group","settings":[
                            {{"key":"limit","label":"Limit","control":{{"type":"number","min":0,"max":100,"integer":true,"default":50}}}},
                            {{"key":"pick","label":"Pick","control":{{"type":"select","default":"a","options":[{{"label":"A","value":"a"}}]}}}},
                            {{"key":"restart","label":"Restart","control":{{"type":"button","text":"Restart"}}}}
                        ]}}]}}"#
                ),
                group.contains("orbit") || id.starts_with("orbit."),
            )
            .unwrap(),
        )
    }

    #[test]
    fn navigation_lists_orbit_then_a_plugins_heading() {
        let pages = [
            state("orbit.overview", Some("orbit")),
            state("orbit.log", Some("orbit")),
            state("a.plugin", None),
        ];
        let entries = nav_entries(&pages);
        let rows: Vec<_> = nav_rows(&entries)
            .into_iter()
            .map(|row| match row {
                NavRow::Page(entry) => entry.id.clone(),
                NavRow::Separator => "-".into(),
                NavRow::Heading(text) => format!("# {text}"),
            })
            .collect();
        assert_eq!(
            rows,
            ["orbit.overview", "orbit.log", "-", "# Plugins", "a.plugin"]
        );
    }

    #[test]
    fn navigation_has_no_separator_or_heading_without_both_groups() {
        let entries = nav_entries(&[state("orbit.overview", Some("orbit"))]);
        assert_eq!(nav_rows(&entries).len(), 1);
        let entries = nav_entries(&[state("a.plugin", None)]);
        assert_eq!(nav_rows(&entries).len(), 2);
        assert!(matches!(nav_rows(&entries)[0], NavRow::Heading("Plugins")));
    }

    #[test]
    fn the_restart_notice_needs_a_page_and_the_overview_button() {
        let mut overview = state("orbit.overview", Some("orbit"));
        let mut plugin = state("a.plugin", None);
        assert!(!restart_offered(&[overview.clone(), plugin.clone()]));
        plugin.restart_required = true;
        assert!(restart_offered(&[overview.clone(), plugin.clone()]));
        assert!(!restart_offered(&[plugin.clone()]));
        overview.restart_required = true;
        assert!(restart_offered(&[overview]));
    }

    #[test]
    fn a_draft_is_replaced_only_by_a_changed_parent_value() {
        assert!(!replaces_draft(&Value::from(5), &Value::from(5)));
        assert!(replaces_draft(&Value::from(5), &Value::from(6)));
    }

    #[test]
    fn numbers_are_refused_locally_with_a_sentence() {
        let page = state("a.plugin", None);
        let Some(Element::Setting(setting)) = page.page.element("limit") else {
            panic!("limit is a setting");
        };
        assert_eq!(number_edit(&setting.control, " 42 "), Ok(Value::from(42)));
        assert_eq!(
            number_edit(&setting.control, "abc"),
            Err("Enter a number.".into())
        );
        assert_eq!(
            number_edit(&setting.control, "101"),
            Err("Number is outside the allowed range.".into())
        );
        assert_eq!(
            number_edit(&setting.control, "1.5"),
            Err("Expected an integer.".into())
        );
        assert!(number_edit(&setting.control, "NaN").is_err());
    }

    #[test]
    fn json_must_be_an_object_or_an_array() {
        assert!(json_edit(r#"{"a":1}"#).is_ok());
        assert!(json_edit("[1]").is_ok());
        assert_eq!(
            json_edit("3"),
            Err("Expected a JSON object or array.".into())
        );
        assert!(json_edit("{").unwrap_err().starts_with("Not valid JSON: "));
    }

    #[test]
    fn newlines_are_normalized_for_plugins() {
        assert_eq!(normalize_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
    }

    #[test]
    fn an_edit_becomes_an_action_only_when_the_page_accepts_it() {
        let mut page = state("a.plugin", None);
        let action = user_action(&page, "limit", Some(Value::from(7))).unwrap();
        assert_eq!(action.action_type, "change");
        assert_eq!(
            user_action(&page, "limit", Some(Value::from(700))),
            Err("Number is outside the allowed range.".into())
        );
        assert_eq!(
            user_action(&page, "restart", None).unwrap().action_type,
            "click"
        );
        assert_eq!(
            user_action(&page, "missing", Some(Value::from(1))),
            Err("That value is not accepted.".into())
        );

        // A replaced option list changes what the page accepts.
        let patch: UiPatch = serde_json::from_value(serde_json::json!({
            "props": {"pick": {"options": [{"label": "B", "value": "b"}]}}
        }))
        .unwrap();
        page.apply_patch(patch).unwrap();
        assert!(user_action(&page, "pick", Some(Value::from("a"))).is_err());
        assert!(user_action(&page, "pick", Some(Value::from("b"))).is_ok());
    }

    #[test]
    fn the_parent_value_falls_back_to_the_default() {
        let page = state("a.plugin", None);
        let Some(Element::Setting(setting)) = page.page.element("limit") else {
            panic!("limit is a setting");
        };
        assert_eq!(
            parent_value(&page, "limit", &setting.control),
            Value::from(50)
        );
        assert_eq!(
            parent_value(&page, "none", &setting.control),
            Value::from(50)
        );
    }
}
