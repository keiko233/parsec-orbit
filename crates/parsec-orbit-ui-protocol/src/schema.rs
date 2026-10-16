//! Declarative page schema for UI service version 2.
//!
//! The normative description is `docs/spec/ui-service.md`. Every type rejects
//! unknown fields, so an unknown block or control type fails at registration.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only accepted value of a page's `schema` field.
pub const SCHEMA_VERSION: u32 = 2;
/// Largest accepted page JSON, in bytes.
pub const MAX_PAGE_BYTES: usize = 256 * 1024;
/// Most blocks on one page.
pub const MAX_BLOCKS: usize = 64;
/// Most settings on one page, nested ones included.
pub const MAX_SETTINGS: usize = 256;
/// Most options in one `select`.
pub const MAX_OPTIONS: usize = 256;
/// Largest `value` or `log` text, in bytes.
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
/// Most `rows` a multiline `text` or a `log` may ask for.
pub const MAX_ROWS: u8 = 20;
/// Pages that only the UI service itself may register carry this prefix.
pub const RESERVED_PREFIX: &str = "orbit.";

/// Semantic icon names; each renderer maps them to its platform symbol set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiIcon {
    Home,
    Plugin,
    Log,
    Network,
    Video,
    Audio,
    Input,
    Gamepad,
    Display,
    Speed,
    Security,
    Server,
    Settings,
    Info,
    Warning,
    Error,
    Success,
    Refresh,
    Folder,
    Copy,
    Play,
    Stop,
    Restart,
    Download,
    Delete,
    Edit,
    Link,
    Clock,
    Tools,
}

/// The severity of a banner or of a setting's status text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiSeverity {
    Info,
    Success,
    Warning,
    Error,
}

/// The navigation group a page appears in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiNavGroup {
    Orbit,
    Plugins,
}

/// How prominent a command button is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiButtonStyle {
    #[default]
    Standard,
    Accent,
    Destructive,
}

/// When an editable text control reports its value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiCommit {
    /// On Enter or when focus leaves the control.
    #[default]
    Submit,
    /// After every edit.
    Change,
}

/// One selectable option of a `select`; `value` is a string or an integer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiOption {
    pub label: String,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The control of a setting; the variant fixes its value type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiControl {
    Toggle {
        default: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        off_text: Option<String>,
    },
    Number {
        default: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        #[serde(default)]
        integer: bool,
    },
    Slider {
        default: f64,
        min: f64,
        max: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    Select {
        default: Value,
        options: Vec<UiOption>,
    },
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default)]
        multiline: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<u8>,
        #[serde(default)]
        monospace: bool,
        #[serde(default)]
        commit: UiCommit,
    },
    Json {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Value>,
    },
    Button {
        text: String,
        #[serde(default)]
        style: UiButtonStyle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<UiIcon>,
    },
    Value {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
        #[serde(default)]
        monospace: bool,
        #[serde(default)]
        copyable: bool,
    },
    Log {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<u8>,
    },
}

/// One thing the user can see or change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSetting {
    pub key: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<UiIcon>,
    pub control: UiControl,
    /// Shown when the setting is expanded; one level only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<UiSetting>,
}

/// A command button in an `actions` block.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiButton {
    pub key: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<UiIcon>,
    #[serde(default)]
    pub style: UiButtonStyle,
}

/// An inline notice.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiBanner {
    pub key: String,
    pub severity: UiSeverity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub message: String,
}

/// A titled run of settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiGroup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub settings: Vec<UiSetting>,
}

/// A row of command buttons.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiActions {
    pub buttons: Vec<UiButton>,
}

/// One block of a page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiBlock {
    Banner(UiBanner),
    Group(UiGroup),
    Actions(UiActions),
}

/// One navigation destination.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiPage {
    pub schema: u32,
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<UiIcon>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<UiNavGroup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<i32>,
    pub blocks: Vec<UiBlock>,
}

/// An interaction event emitted by the renderer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiAction {
    pub page: String,
    pub key: String,
    #[serde(rename = "type")]
    pub action_type: String,
    #[serde(default)]
    pub value: Option<Value>,
}

/// A reference to one addressable element of a page.
#[derive(Clone, Copy, Debug)]
pub enum Element<'a> {
    Banner(&'a UiBanner),
    Setting(&'a UiSetting),
    Button(&'a UiButton),
}

impl UiPage {
    /// The navigation group, `plugins` unless the page asks for `orbit`.
    pub fn nav_group(&self) -> UiNavGroup {
        self.group.unwrap_or(UiNavGroup::Plugins)
    }

    /// Parses and validates a page registered by a plugin or by the service.
    ///
    /// `reserved` allows the `orbit.` identifier prefix and the `orbit` group,
    /// which only the UI service's own pages may use.
    pub fn parse(json: &str, reserved: bool) -> Result<Self, String> {
        if json.len() > MAX_PAGE_BYTES {
            return Err(format!("page exceeds {MAX_PAGE_BYTES} bytes"));
        }
        let page: UiPage = serde_json::from_str(json).map_err(|error| error.to_string())?;
        page.validate(reserved)?;
        Ok(page)
    }

    /// Checks identity, limits, key uniqueness and that every default fits its control.
    pub fn validate(&self, reserved: bool) -> Result<(), String> {
        if self.schema != SCHEMA_VERSION {
            return Err(format!("schema must be {SCHEMA_VERSION}"));
        }
        if !valid_name(&self.id, 128) {
            return Err("page id must be 1-128 characters of A-Z a-z 0-9 _ . -".into());
        }
        if !reserved
            && (self.id.starts_with(RESERVED_PREFIX) || self.group == Some(UiNavGroup::Orbit))
        {
            return Err("the orbit. prefix and the orbit group are reserved".into());
        }
        check_text("title", &self.title, 64, true)?;
        check_optional("description", self.description.as_deref(), 1024)?;
        if self.blocks.len() > MAX_BLOCKS {
            return Err(format!("a page holds at most {MAX_BLOCKS} blocks"));
        }
        let mut keys = HashSet::new();
        let mut settings = 0;
        for block in &self.blocks {
            match block {
                UiBlock::Banner(banner) => {
                    claim_key(&mut keys, &banner.key)?;
                    check_optional("banner title", banner.title.as_deref(), 256)?;
                    check_text("banner message", &banner.message, 1024, false)?;
                }
                UiBlock::Group(group) => {
                    check_optional("group title", group.title.as_deref(), 256)?;
                    check_optional("group description", group.description.as_deref(), 1024)?;
                    for setting in &group.settings {
                        validate_setting(setting, 0, &mut keys, &mut settings)?;
                    }
                }
                UiBlock::Actions(actions) => {
                    for button in &actions.buttons {
                        claim_key(&mut keys, &button.key)?;
                        check_text("button text", &button.text, 256, true)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Visits every addressable element in document order.
    pub fn elements(&self) -> Vec<Element<'_>> {
        fn walk<'a>(setting: &'a UiSetting, out: &mut Vec<Element<'a>>) {
            out.push(Element::Setting(setting));
            for nested in &setting.settings {
                walk(nested, out);
            }
        }
        let mut out = Vec::new();
        for block in &self.blocks {
            match block {
                UiBlock::Banner(banner) => out.push(Element::Banner(banner)),
                UiBlock::Group(group) => group.settings.iter().for_each(|s| walk(s, &mut out)),
                UiBlock::Actions(actions) => {
                    out.extend(actions.buttons.iter().map(Element::Button))
                }
            }
        }
        out
    }

    /// Finds one element by key.
    pub fn element(&self, key: &str) -> Option<Element<'_>> {
        self.elements().into_iter().find(|element| match element {
            Element::Banner(banner) => banner.key == key,
            Element::Setting(setting) => setting.key == key,
            Element::Button(button) => button.key == key,
        })
    }

    /// The values declared by controls that carry one.
    pub fn initial_values(&self) -> HashMap<String, Value> {
        let mut values = HashMap::new();
        for element in self.elements() {
            if let Element::Setting(setting) = element {
                if let Some(value) = setting.control.default_value() {
                    values.insert(setting.key.clone(), value);
                }
            }
        }
        values
    }
}

impl UiControl {
    /// The declared default as a JSON value, or `None` when there is none.
    pub fn default_value(&self) -> Option<Value> {
        match self {
            UiControl::Toggle { default, .. } => Some(Value::Bool(*default)),
            UiControl::Number { default, .. } => Some(number_value(*default)),
            UiControl::Slider { default, .. } => Some(number_value(*default)),
            UiControl::Select { default, .. } => Some(default.clone()),
            UiControl::Text { default, .. }
            | UiControl::Value { default, .. }
            | UiControl::Log { default, .. } => default.clone().map(Value::String),
            UiControl::Json { default } => default.clone(),
            UiControl::Button { .. } => None,
        }
    }

    /// Whether the control holds a value that updates and actions carry.
    pub fn has_value(&self) -> bool {
        !matches!(self, UiControl::Button { .. })
    }

    /// Whether the user can change the control's value.
    pub fn is_editable(&self) -> bool {
        !matches!(
            self,
            UiControl::Button { .. } | UiControl::Value { .. } | UiControl::Log { .. }
        )
    }
}

/// An integral number as a JSON integer, anything else as a float.
pub fn number_value(number: f64) -> Value {
    if number.fract() == 0.0 && number.abs() < 9.0e15 {
        Value::from(number as i64)
    } else {
        Value::from(number)
    }
}

/// Checks one value against a control; `options` overrides a select's options.
pub fn check_value(
    control: &UiControl,
    options: Option<&[UiOption]>,
    value: &Value,
) -> Result<(), String> {
    match control {
        UiControl::Toggle { .. } => value
            .is_boolean()
            .then_some(())
            .ok_or_else(|| "expected a boolean".into()),
        UiControl::Number {
            min, max, integer, ..
        } => check_number(value, *min, *max, *integer),
        UiControl::Slider { min, max, .. } => check_number(value, Some(*min), Some(*max), false),
        UiControl::Select {
            options: declared, ..
        } => {
            let options = options.unwrap_or(declared);
            if options.iter().any(|option| option.value == *value) {
                Ok(())
            } else {
                Err("value is not one of the options".into())
            }
        }
        UiControl::Text { .. } | UiControl::Value { .. } | UiControl::Log { .. } => {
            match value.as_str() {
                Some(text) if text.len() <= MAX_TEXT_BYTES => Ok(()),
                Some(_) => Err(format!("text exceeds {MAX_TEXT_BYTES} bytes")),
                None => Err("expected a string".into()),
            }
        }
        UiControl::Json { .. } => (value.is_object() || value.is_array())
            .then_some(())
            .ok_or_else(|| "expected an object or an array".into()),
        UiControl::Button { .. } => Err("a button holds no value".into()),
    }
}

fn check_number(
    value: &Value,
    min: Option<f64>,
    max: Option<f64>,
    integer: bool,
) -> Result<(), String> {
    let number = value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or("expected a finite number")?;
    if min.is_some_and(|min| number < min) || max.is_some_and(|max| number > max) {
        return Err("number is outside the allowed range".into());
    }
    if integer && number.fract() != 0.0 {
        return Err("expected an integer".into());
    }
    Ok(())
}

pub(crate) fn valid_name(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

pub(crate) fn check_text(what: &str, text: &str, max: usize, required: bool) -> Result<(), String> {
    if required && text.is_empty() {
        return Err(format!("{what} must not be empty"));
    }
    if text.chars().count() > max {
        return Err(format!("{what} exceeds {max} characters"));
    }
    Ok(())
}

pub(crate) fn check_optional(what: &str, text: Option<&str>, max: usize) -> Result<(), String> {
    text.map_or(Ok(()), |text| check_text(what, text, max, false))
}

/// Checks the options of a select, whether declared or supplied by a patch.
pub(crate) fn check_options(options: &[UiOption]) -> Result<(), String> {
    if options.is_empty() || options.len() > MAX_OPTIONS {
        return Err("a select needs 1 to 256 options".into());
    }
    for (index, option) in options.iter().enumerate() {
        check_text("option label", &option.label, 256, true)?;
        check_optional("option description", option.description.as_deref(), 1024)?;
        if !(option.value.is_string() || option.value.is_i64() || option.value.is_u64()) {
            return Err("option values are strings or integers".into());
        }
        if options[..index].iter().any(|o| o.value == option.value) {
            return Err("option values must be unique".into());
        }
    }
    Ok(())
}

fn claim_key(keys: &mut HashSet<String>, key: &str) -> Result<(), String> {
    if !valid_name(key, 64) {
        return Err(format!(
            "key {key:?} must be 1-64 characters of A-Z a-z 0-9 _ . -"
        ));
    }
    if !keys.insert(key.to_owned()) {
        return Err(format!("key {key:?} is used twice"));
    }
    Ok(())
}

fn validate_setting(
    setting: &UiSetting,
    depth: usize,
    keys: &mut HashSet<String>,
    count: &mut usize,
) -> Result<(), String> {
    *count += 1;
    if *count > MAX_SETTINGS {
        return Err(format!("a page holds at most {MAX_SETTINGS} settings"));
    }
    claim_key(keys, &setting.key)?;
    check_text("label", &setting.label, 256, true)?;
    check_optional("description", setting.description.as_deref(), 1024)?;
    validate_control(&setting.key, &setting.control)?;
    if depth > 0 && !setting.settings.is_empty() {
        return Err(format!("{:?}: settings nest one level only", setting.key));
    }
    for nested in &setting.settings {
        validate_setting(nested, depth + 1, keys, count)?;
    }
    Ok(())
}

fn validate_control(key: &str, control: &UiControl) -> Result<(), String> {
    let fail = |message: &str| Err(format!("{key:?}: {message}"));
    match control {
        UiControl::Toggle {
            on_text, off_text, ..
        } => {
            check_optional("on_text", on_text.as_deref(), 64)?;
            check_optional("off_text", off_text.as_deref(), 64)?;
        }
        UiControl::Number {
            default,
            min,
            max,
            step,
            unit,
            ..
        } => {
            if !default.is_finite()
                || [min, max, step]
                    .iter()
                    .any(|n| n.is_some_and(|n| !n.is_finite()))
            {
                return fail("numbers must be finite");
            }
            if min.zip(*max).is_some_and(|(min, max)| min > max) {
                return fail("min exceeds max");
            }
            if step.is_some_and(|step| step <= 0.0) {
                return fail("step must be positive");
            }
            check_optional("unit", unit.as_deref(), 32)?;
        }
        UiControl::Slider {
            default,
            min,
            max,
            step,
            unit,
        } => {
            if ![*default, *min, *max].iter().all(|n| n.is_finite()) || min >= max {
                return fail("a slider needs finite numbers with min below max");
            }
            if step.is_some_and(|step| !step.is_finite() || step <= 0.0) {
                return fail("step must be positive");
            }
            check_optional("unit", unit.as_deref(), 32)?;
        }
        UiControl::Select { options, .. } => {
            check_options(options).map_err(|error| format!("{key:?}: {error}"))?;
        }
        UiControl::Text {
            rows, placeholder, ..
        } => {
            check_optional("placeholder", placeholder.as_deref(), 256)?;
            if rows.is_some_and(|rows| rows == 0 || rows > MAX_ROWS) {
                return fail("rows is 1 to 20");
            }
        }
        UiControl::Log { rows, .. } => {
            if rows.is_some_and(|rows| rows == 0 || rows > MAX_ROWS) {
                return fail("rows is 1 to 20");
            }
        }
        UiControl::Button { text, .. } => check_text("button text", text, 256, true)?,
        UiControl::Json { .. } | UiControl::Value { .. } => {}
    }
    if let Some(default) = control.default_value() {
        check_value(control, None, &default)
            .map_err(|error| format!("{key:?}: default: {error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example from `docs/spec/ui-service.md`.
    const SPEC_PAGE: &str = r#"{
      "schema": 2,
      "id": "com.example.bandwidth",
      "title": "Bandwidth",
      "description": "Bitrate choices offered by the client.",
      "icon": "speed",
      "group": "plugins",
      "order": 100,
      "blocks": [
        {
          "type": "banner", "key": "patch_state", "severity": "warning",
          "title": "Not applied yet",
          "message": "Restart the client to use the new bitrates."
        },
        {
          "type": "group", "title": "High bitrate",
          "settings": [
            {
              "key": "enabled", "label": "Allow bitrates above 50 Mbps",
              "description": "Adds the extra choices to the bandwidth menu.",
              "control": { "type": "toggle", "default": true }
            },
            {
              "key": "ceiling", "label": "Maximum bitrate",
              "control": { "type": "number", "min": 10, "max": 300, "step": 5,
                           "unit": "Mbps", "integer": true, "default": 100 }
            },
            {
              "key": "codec", "label": "Preferred codec",
              "control": { "type": "select", "default": "hevc", "options": [
                { "label": "H.264", "value": "h264" },
                { "label": "HEVC (H.265)", "value": "hevc" },
                { "label": "AV1", "value": "av1" } ] }
            }
          ]
        },
        {
          "type": "actions",
          "buttons": [ { "key": "reset", "text": "Reset to defaults" } ]
        }
      ]
    }"#;

    fn page_with(settings: &str) -> String {
        format!(
            r#"{{"schema":2,"id":"t.page","title":"T","blocks":[{{"type":"group","settings":[{settings}]}}]}}"#
        )
    }

    fn rejected(json: &str) -> String {
        UiPage::parse(json, false).expect_err("must be rejected")
    }

    #[test]
    fn the_specification_example_is_accepted_and_round_trips() {
        let page = UiPage::parse(SPEC_PAGE, false).unwrap();
        assert_eq!(page.nav_group(), UiNavGroup::Plugins);
        assert_eq!(page.icon, Some(UiIcon::Speed));
        let again: UiPage = serde_json::from_str(&serde_json::to_string(&page).unwrap()).unwrap();
        assert_eq!(page, again);
        let values = page.initial_values();
        assert_eq!(values["enabled"], Value::Bool(true));
        assert_eq!(values["ceiling"], Value::from(100));
        assert_eq!(values["codec"], Value::from("hevc"));
        assert!(!values.contains_key("reset"));
        assert_eq!(page.elements().len(), 5);
        assert!(matches!(page.element("reset"), Some(Element::Button(_))));
    }

    #[test]
    fn unknown_fields_blocks_controls_icons_and_schema_versions_are_rejected() {
        assert!(
            rejected(&SPEC_PAGE.replace(r#""schema": 2"#, r#""schema": 1"#)).contains("schema")
        );
        assert!(UiPage::parse(
            &SPEC_PAGE.replace(r#""icon": "speed""#, r#""icon": "rocket""#),
            false
        )
        .is_err());
        assert!(UiPage::parse(
            &SPEC_PAGE.replace(r#""order": 100"#, r#""order": 100, "x": 1"#),
            false
        )
        .is_err());
        assert!(UiPage::parse(
            &SPEC_PAGE.replace(r#""type": "actions""#, r#""type": "carousel""#),
            false
        )
        .is_err());
        assert!(UiPage::parse(
            &page_with(r#"{"key":"a","label":"A","control":{"type":"text_input"}}"#),
            false
        )
        .is_err());
        assert!(UiPage::parse(
            &page_with(
                r#"{"key":"a","label":"A","control":{"type":"toggle","default":true,"extra":1}}"#
            ),
            false
        )
        .is_err());
    }

    #[test]
    fn identities_keys_and_reserved_names_are_checked() {
        assert!(
            rejected(&SPEC_PAGE.replace("com.example.bandwidth", "bad id")).contains("page id")
        );
        assert!(
            rejected(&SPEC_PAGE.replace("com.example.bandwidth", "orbit.fake"))
                .contains("reserved")
        );
        assert!(
            rejected(&SPEC_PAGE.replace(r#""group": "plugins""#, r#""group": "orbit""#))
                .contains("reserved")
        );
        assert!(UiPage::parse(
            &SPEC_PAGE.replace("com.example.bandwidth", "orbit.fake"),
            true
        )
        .is_ok());
        // A banner, a setting and a button share one key space.
        assert!(
            rejected(&SPEC_PAGE.replace(r#""key": "reset""#, r#""key": "enabled""#))
                .contains("twice")
        );
        assert!(
            rejected(&SPEC_PAGE.replace(r#""key": "reset""#, r#""key": "no good""#))
                .contains("key")
        );
    }

    #[test]
    fn defaults_must_fit_their_controls() {
        let toggle = |extra: &str| {
            page_with(&format!(
                r#"{{"key":"a","label":"A","control":{{{extra}}}}}"#
            ))
        };
        assert!(
            rejected(&toggle(r#""type":"number","default":500,"max":300"#)).contains("default")
        );
        assert!(
            rejected(&toggle(r#""type":"number","default":1.5,"integer":true"#))
                .contains("integer")
        );
        assert!(
            rejected(&toggle(r#""type":"number","default":5,"min":9,"max":3"#))
                .contains("min exceeds max")
        );
        assert!(
            rejected(&toggle(r#""type":"slider","default":5,"min":9,"max":9"#)).contains("slider")
        );
        assert!(rejected(&toggle(
            r#""type":"select","default":"z","options":[{"label":"A","value":"a"}]"#
        ))
        .contains("default"));
        assert!(rejected(&toggle(r#""type":"select","default":"a","options":[{"label":"A","value":"a"},{"label":"B","value":"a"}]"#)).contains("unique"));
        assert!(rejected(&toggle(
            r#""type":"select","default":1,"options":[{"label":"A","value":1.5}]"#
        ))
        .contains("strings or integers"));
        assert!(
            rejected(&toggle(r#""type":"select","default":"a","options":[]"#)).contains("options")
        );
        assert!(rejected(&toggle(r#""type":"json","default":3"#)).contains("object or an array"));
        assert!(rejected(&toggle(r#""type":"text","rows":0"#)).contains("rows"));
        assert!(UiPage::parse(
            &toggle(r#""type":"text","multiline":true,"rows":20,"commit":"change""#),
            false
        )
        .is_ok());
    }

    #[test]
    fn nesting_is_one_level_and_page_size_is_bounded() {
        let nested = |inner: &str| {
            page_with(&format!(
                r#"{{"key":"a","label":"A","control":{{"type":"button","text":"Go"}},"settings":[{inner}]}}"#
            ))
        };
        let leaf = r#"{"key":"b","label":"B","control":{"type":"toggle","default":false}}"#;
        assert!(UiPage::parse(&nested(leaf), false).is_ok());
        let too_deep = nested(&format!(
            r#"{{"key":"b","label":"B","control":{{"type":"toggle","default":false}},"settings":[{}]}}"#,
            leaf.replace(r#""key":"b""#, r#""key":"c""#)
        ));
        assert!(rejected(&too_deep).contains("one level"));

        let many = (0..=MAX_SETTINGS)
            .map(|n| {
                format!(
                    r#"{{"key":"k{n}","label":"L","control":{{"type":"toggle","default":false}}}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        assert!(rejected(&page_with(&many)).contains("at most 256 settings"));
        assert!(rejected(&" ".repeat(MAX_PAGE_BYTES + 1)).contains("exceeds"));
    }

    #[test]
    fn value_checks_follow_the_control() {
        let page = UiPage::parse(SPEC_PAGE, false).unwrap();
        let control = |key| match page.element(key) {
            Some(Element::Setting(setting)) => setting.control.clone(),
            _ => panic!("no setting {key}"),
        };
        assert!(check_value(&control("enabled"), None, &Value::Bool(false)).is_ok());
        assert!(check_value(&control("enabled"), None, &Value::from(1)).is_err());
        assert!(check_value(&control("ceiling"), None, &Value::from(150)).is_ok());
        assert!(check_value(&control("ceiling"), None, &Value::from(301)).is_err());
        assert!(check_value(&control("ceiling"), None, &Value::from(10.5)).is_err());
        assert!(check_value(&control("codec"), None, &Value::from("av1")).is_ok());
        assert!(check_value(&control("codec"), None, &Value::from("vp9")).is_err());
    }
}
