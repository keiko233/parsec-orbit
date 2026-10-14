//! Native AppKit controls for the declarative UI items.
//!
//! One [`Control`] per item type of `docs/spec/ui-service.md`. Creation, value
//! round-tripping and event wiring live here; the window that arranges them
//! (tabs, layout, show/hide) is the parent module.
//!
//! Every function runs on the main thread, inside a main-queue block.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSButton, NSButtonType, NSControl, NSControlStateValueOff, NSControlStateValueOn, NSFont,
    NSPopUpButton, NSScrollView, NSSlider, NSTextField, NSTextView, NSView,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use serde_json::Value;

use crate::schema::UiItem;

/// The height a row reserves for its control.
pub const CONTROL_HEIGHT: f64 = 24.0;
/// The JSON editor needs room for several lines.
pub const JSON_HEIGHT: f64 = 120.0;

/// One native control per declarative item type.
pub enum Control {
    Toggle(Retained<NSButton>),
    Slider(Retained<NSSlider>),
    Select(Retained<NSPopUpButton>),
    Input(Retained<NSTextField>),
    Json {
        scroll: Retained<NSScrollView>,
        text: Retained<NSTextView>,
    },
    Button(Retained<NSButton>),
    Text(Retained<NSTextField>),
}

impl Control {
    /// Builds the control for `item`. Controls that report user interaction
    /// send `action` to `target`; the caller adds [`Control::view`] to its
    /// parent and gives it a frame.
    pub fn new(item: &UiItem, mtm: MainThreadMarker, target: &AnyObject, action: Sel) -> Self {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        match item.item_type.as_str() {
            "toggle" => {
                let button = NSButton::initWithFrame(NSButton::alloc(mtm), frame);
                button.setButtonType(NSButtonType::Switch);
                // The checkbox carries the label; the row adds no second one.
                button.setTitle(&NSString::from_str(&item.label));
                unsafe {
                    button.setTarget(Some(target));
                    button.setAction(Some(action));
                }
                Control::Toggle(button)
            }
            "slider" => {
                let slider = unsafe {
                    NSSlider::sliderWithValue_minValue_maxValue_target_action(
                        item.default.as_ref().and_then(Value::as_f64).unwrap_or(0.0),
                        item.min.unwrap_or(0.0),
                        item.max.unwrap_or(100.0),
                        Some(target),
                        Some(action),
                        mtm,
                    )
                };
                Control::Slider(slider)
            }
            "select" => {
                let select =
                    NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), frame, false);
                for option in item.options.as_deref().unwrap_or_default() {
                    select.addItemWithTitle(&NSString::from_str(&option.label));
                }
                unsafe {
                    select.setTarget(Some(target));
                    select.setAction(Some(action));
                }
                Control::Select(select)
            }
            "text_input" | "number_input" => {
                let input = NSTextField::initWithFrame(NSTextField::alloc(mtm), frame);
                if let Some(placeholder) = item.placeholder.as_deref() {
                    input.setPlaceholderString(Some(&NSString::from_str(placeholder)));
                }
                unsafe {
                    input.setTarget(Some(target));
                    input.setAction(Some(action));
                }
                Control::Input(input)
            }
            "json_editor" => {
                let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
                scroll.setHasVerticalScroller(true);
                let text = NSTextView::initWithFrame(NSTextView::alloc(mtm), frame);
                text.setRichText(false);
                text.setAutomaticQuoteSubstitutionEnabled(false);
                text.setFont(NSFont::userFixedPitchFontOfSize(11.0).as_deref());
                // The editor reports through its delegate (`textDidEndEditing:`)
                // rather than a control action, so no target is set here.
                scroll.setDocumentView(Some(&text));
                Control::Json { scroll, text }
            }
            "button" => {
                let button = NSButton::initWithFrame(NSButton::alloc(mtm), frame);
                button.setButtonType(NSButtonType::MomentaryPushIn);
                button.setTitle(&NSString::from_str(&item.label));
                unsafe {
                    button.setTarget(Some(target));
                    button.setAction(Some(action));
                }
                Control::Button(button)
            }
            // `readonly_text`, the schemas' `text`, and anything unknown.
            _ => {
                let text = NSTextField::initWithFrame(NSTextField::alloc(mtm), frame);
                text.setEditable(false);
                text.setBezeled(false);
                text.setDrawsBackground(false);
                text.setSelectable(true);
                Control::Text(text)
            }
        }
    }

    /// The view that owns the control's frame.
    pub fn view(&self) -> &NSView {
        match self {
            Control::Toggle(button) => button,
            Control::Slider(slider) => slider,
            Control::Select(select) => select,
            Control::Input(input) => input,
            Control::Json { scroll, .. } => scroll,
            Control::Button(button) => button,
            Control::Text(text) => text,
        }
    }

    /// The height the row reserves for this control.
    pub fn height(&self) -> f64 {
        match self {
            Control::Json { .. } => JSON_HEIGHT,
            _ => CONTROL_HEIGHT,
        }
    }

    /// Tags the control so an action can name its row. The JSON editor has no
    /// tag: it reports the text view through its delegate instead.
    pub fn set_tag(&self, tag: isize) {
        if let Some(control) = self.control() {
            control.setTag(tag);
        }
    }

    /// The tag AppKit reported for this control; `0` for the JSON editor,
    /// which has none.
    pub fn tag(&self) -> isize {
        self.control().map(|control| control.tag()).unwrap_or(0)
    }

    /// The control behind this row, when it is one AppKit can tag.
    fn control(&self) -> Option<&NSControl> {
        match self {
            Control::Toggle(button) => Some(button),
            Control::Slider(slider) => Some(slider),
            Control::Select(select) => Some(select),
            Control::Input(input) => Some(input),
            Control::Json { .. } => None,
            Control::Button(button) => Some(button),
            Control::Text(text) => Some(text),
        }
    }

    /// The JSON editor's text view, so its delegate notification can name its
    /// row.
    pub fn text_view(&self) -> Option<&NSTextView> {
        match self {
            Control::Json { text, .. } => Some(text),
            _ => None,
        }
    }

    /// Writes `value` into the control without emitting an action.
    pub fn set_value(&self, item: &UiItem, value: &Value) {
        match self {
            Control::Toggle(button) => button.setState(if value.as_bool().unwrap_or(false) {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            }),
            Control::Slider(slider) => {
                slider.setDoubleValue(value.as_f64().unwrap_or(0.0));
            }
            Control::Select(select) => {
                let options = item.options.as_deref().unwrap_or_default();
                if let Some(index) = options.iter().position(|option| option.value == *value) {
                    select.selectItemAtIndex(index as isize);
                }
            }
            Control::Input(input) => {
                input.setStringValue(&NSString::from_str(&display_value(value)))
            }
            Control::Json { text, .. } => text.setString(&NSString::from_str(
                &serde_json::to_string_pretty(value).unwrap_or_default(),
            )),
            Control::Text(text) => text.setStringValue(&NSString::from_str(&display_value(value))),
            Control::Button(_) => {}
        }
    }

    /// Reads what the user has in the control, or `None` when it is not a
    /// usable value (an unparsable number, broken JSON, a read-only row).
    pub fn read_value(&self, item: &UiItem) -> Option<Value> {
        match self {
            Control::Toggle(button) => Some(Value::Bool(button.state() == NSControlStateValueOn)),
            Control::Slider(slider) => {
                let raw = slider.doubleValue();
                let step = item.step.unwrap_or(0.0);
                let snapped = if step > 0.0 {
                    let base = item.min.unwrap_or(0.0);
                    base + ((raw - base) / step).round() * step
                } else {
                    raw
                };
                Some(Value::from(snapped))
            }
            Control::Select(select) => {
                let index = select.indexOfSelectedItem();
                let options = item.options.as_ref()?;
                options
                    .get(usize::try_from(index).ok()?)
                    .map(|option| option.value.clone())
            }
            Control::Input(input) if item.item_type == "number_input" => input
                .stringValue()
                .to_string()
                .parse::<f64>()
                .ok()
                .filter(|number| {
                    number.is_finite()
                        && item.min.is_none_or(|min| *number >= min)
                        && item.max.is_none_or(|max| *number <= max)
                })
                .map(Value::from),
            Control::Input(input) => Some(Value::String(input.stringValue().to_string())),
            Control::Json { text, .. } => serde_json::from_str::<Value>(&text.string().to_string())
                .ok()
                .filter(|value| value.is_object() || value.is_array()),
            Control::Button(_) => Some(Value::Null),
            Control::Text(_) => None,
        }
    }

    /// Shows `text` when the pointer rests on the control.
    pub fn set_tooltip(&self, text: &str) {
        let view = match self {
            Control::Json { scroll, .. } => &**scroll,
            other => other.view(),
        };
        view.setToolTip(Some(&NSString::from_str(text)));
    }
}

fn display_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
