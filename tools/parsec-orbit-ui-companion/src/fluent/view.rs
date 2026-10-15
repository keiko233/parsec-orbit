//! A page as XAML: building it from a [`PageState`], reading the user's edits
//! out of its controls, and applying later values and properties to it.
//!
//! Every plugin string reaches the controls through a setter after the fixed
//! template has loaded; see [`super::xaml`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use parsec_orbit_ui_protocol::schema::{
    number_value, UiButtonStyle, UiCommit, UiControl, UiSetting, UiSeverity,
};
use parsec_orbit_ui_protocol::{PageState, UiBlock, UiOption, UiProps};
use parsec_orbit_winui_bindings::{
    AutomationProperties, Button, ComboBox, ComboBoxItem, ContentControl, Control,
    DependencyObject, Expander, FrameworkElement, InfoBar, InfoBarSeverity, KeyEventHandler,
    NumberBox, NumberBoxValueChangedEventArgs, Panel, ProgressRing, RangeBaseValueChangedEventArgs,
    RangeBaseValueChangedEventHandler, RoutedEventHandler, ScrollViewer,
    SelectionChangedEventHandler, Slider, StackPanel, TextBlock, TextBox, TextChangedEventHandler,
    ToggleSwitch, UIElement, UIElementCollection, Visibility,
};
use serde_json::Value;
use windows::Foundation::{PropertyValue, TypedEventHandler};
use windows::System::VirtualKey;
use windows_core::{Interface, Ref, HSTRING};
use winio::prelude::ComponentSender;

use super::xaml::{self, ButtonLook, ControlTemplate};
use super::OrbitUiMessage;

/// What the controls share: where edits go, and whether a change is the
/// renderer's own (applying parent state) rather than the user's.
#[derive(Clone)]
pub(super) struct Env {
    sender: ComponentSender<super::OrbitUi>,
    updating: Arc<AtomicBool>,
}

impl Env {
    pub(super) fn new(sender: ComponentSender<super::OrbitUi>) -> Self {
        Self {
            sender,
            updating: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether the renderer itself is changing controls right now.
    pub(super) fn is_updating(&self) -> bool {
        self.updating.load(Ordering::SeqCst)
    }

    /// Runs `apply` with user-edit notifications suppressed.
    pub(super) fn updating<T>(&self, apply: impl FnOnce() -> T) -> T {
        self.updating.store(true, Ordering::SeqCst);
        let result = apply();
        self.updating.store(false, Ordering::SeqCst);
        result
    }

    fn edited(&self, page: &str, key: &str, value: Option<Value>) {
        if !self.updating.load(Ordering::SeqCst) {
            self.sender.post(OrbitUiMessage::Edited {
                page: page.to_owned(),
                key: key.to_owned(),
                value,
            });
        }
    }

    fn chosen(&self, page: &str, key: &str, index: usize) {
        if !self.updating.load(Ordering::SeqCst) {
            self.sender.post(OrbitUiMessage::Chosen {
                page: page.to_owned(),
                key: key.to_owned(),
                index,
            });
        }
    }

    fn invalid(&self, page: &str, key: &str, reason: &str) {
        if !self.updating.load(Ordering::SeqCst) {
            self.sender.post(OrbitUiMessage::Invalid {
                page: page.to_owned(),
                key: key.to_owned(),
                reason: reason.to_owned(),
            });
        }
    }
}

fn hstring(text: &str) -> HSTRING {
    HSTRING::from(text)
}

fn boxed(text: &str) -> windows_core::Result<windows_core::IInspectable> {
    PropertyValue::CreateString(&hstring(text))
}

fn set_text(block: &TextBlock, text: &str) {
    let _ = block.SetText(&hstring(text));
}

fn show<T: Interface>(element: &T, visible: bool) {
    if let Ok(element) = element.cast::<UIElement>() {
        let _ = element.SetVisibility(if visible {
            Visibility::Visible
        } else {
            Visibility::Collapsed
        });
    }
}

/// The accessibility identity: the key as the automation id, and the label as the
/// name unless the element shows text of its own, which then stays its name.
fn name<T: Interface>(element: &T, id: &str, label: Option<&str>) {
    if let Ok(object) = element.cast::<DependencyObject>() {
        let _ = AutomationProperties::SetAutomationId(&object, &hstring(id));
        if let Some(label) = label {
            let _ = AutomationProperties::SetName(&object, &hstring(label));
        }
    }
}

/// A text box reports a line break as `\r`; plugins get `\n`.
fn plain_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The text of an edit box with plain line breaks.
fn edit_text(edit: &TextBox) -> windows_core::Result<String> {
    Ok(plain_newlines(&edit.Text()?.to_string()))
}

fn number_text(number: f64, unit: Option<&str>) -> String {
    let number = if number.fract() == 0.0 {
        format!("{number:.0}")
    } else {
        format!("{number}")
    };
    match unit {
        Some(unit) => format!("{number} {unit}"),
        None => number,
    }
}

/// The widget behind a setting's control, with what its updates need.
enum ControlView {
    Toggle(ToggleSwitch),
    Number(NumberBox),
    Slider {
        slider: Slider,
        readout: TextBlock,
        unit: Option<String>,
    },
    Select {
        host: ContentControl,
        combo: Option<ComboBox>,
        options: Vec<UiOption>,
        env: Env,
        page: String,
        label: String,
    },
    Text {
        text: TextBox,
        committed: Arc<Mutex<String>>,
    },
    Json {
        text: TextBox,
        committed: Arc<Mutex<String>>,
    },
    Button(TextBlock),
    Value(TextBlock),
    Log {
        scroll: ScrollViewer,
        text: TextBlock,
    },
}

/// One setting as built.
struct SettingView {
    key: String,
    card: FrameworkElement,
    label: TextBlock,
    description: TextBlock,
    /// Info, success, warning and error lines, in that order.
    statuses: [TextBlock; 4],
    busy: ProgressRing,
    editable: Option<Control>,
    control: ControlView,
    /// The last value the parent set; an unrelated update never replaces a draft.
    parent: Option<Value>,
    local_error: Option<String>,
    props_status: Option<(UiSeverity, String)>,
    enabled: bool,
    is_busy: bool,
}

struct BannerView {
    key: String,
    bar: InfoBar,
}

struct ButtonView {
    key: String,
    button: Button,
    text: TextBlock,
    busy: ProgressRing,
    enabled: bool,
    is_busy: bool,
}

/// A built page.
pub(super) struct PageView {
    pub(super) root: FrameworkElement,
    settings: Vec<SettingView>,
    banners: Vec<BannerView>,
    buttons: Vec<ButtonView>,
}

fn look(style: UiButtonStyle) -> ButtonLook {
    match style {
        UiButtonStyle::Standard => ButtonLook::Standard,
        UiButtonStyle::Accent => ButtonLook::Accent,
        UiButtonStyle::Destructive => ButtonLook::Destructive,
    }
}

fn severity_index(severity: UiSeverity) -> usize {
    match severity {
        UiSeverity::Info => 0,
        UiSeverity::Success => 1,
        UiSeverity::Warning => 2,
        UiSeverity::Error => 3,
    }
}

fn bar_severity(severity: UiSeverity) -> InfoBarSeverity {
    match severity {
        UiSeverity::Info => InfoBarSeverity::Informational,
        UiSeverity::Success => InfoBarSeverity::Success,
        UiSeverity::Warning => InfoBarSeverity::Warning,
        UiSeverity::Error => InfoBarSeverity::Error,
    }
}

fn template(control: &UiControl) -> ControlTemplate {
    match control {
        UiControl::Toggle { .. } => ControlTemplate::Toggle,
        UiControl::Number { .. } => ControlTemplate::Number,
        UiControl::Slider { .. } => ControlTemplate::Slider,
        UiControl::Select { .. } => ControlTemplate::Select,
        UiControl::Text {
            multiline: false, ..
        } => ControlTemplate::TextLine,
        UiControl::Text {
            rows, monospace, ..
        } => ControlTemplate::TextBlock {
            rows: rows.unwrap_or(4),
            monospace: *monospace,
        },
        UiControl::Json { .. } => ControlTemplate::Json { rows: 8 },
        UiControl::Button { style, icon, .. } => ControlTemplate::Button {
            look: look(*style),
            icon: *icon,
        },
        UiControl::Value { monospace, .. } => ControlTemplate::Value {
            monospace: *monospace,
        },
        UiControl::Log { rows, .. } => ControlTemplate::Log {
            rows: rows.unwrap_or(10),
        },
    }
}

fn click(env: &Env, page: &str, key: &str, button: &Button) -> windows_core::Result<()> {
    let (env, page, key) = (env.clone(), page.to_owned(), key.to_owned());
    button.Click(&RoutedEventHandler::new(move |_, _| {
        env.edited(&page, &key, None);
        Ok(())
    }))?;
    Ok(())
}

impl PageView {
    /// Builds the page. Values and properties follow with [`PageView::apply`].
    pub(super) fn build(env: &Env, state: &PageState) -> windows_core::Result<PageView> {
        let page = &state.page;
        let root = xaml::load(&xaml::page())?;
        set_text(&xaml::find(&root, "Title")?, &page.title);
        if let Some(description) = &page.description {
            let block: TextBlock = xaml::find(&root, "Description")?;
            set_text(&block, description);
            show(&block, true);
        }
        let blocks: StackPanel = xaml::find(&root, "Blocks")?;
        let children = blocks.cast::<Panel>()?.Children()?;

        let mut view = PageView {
            root: root.clone(),
            settings: Vec::new(),
            banners: Vec::new(),
            buttons: Vec::new(),
        };
        for block in &page.blocks {
            match block {
                UiBlock::Banner(banner) => {
                    let element = xaml::load(&xaml::banner())?;
                    let bar: InfoBar = xaml::find(&element, "Bar")?;
                    bar.SetSeverity(bar_severity(banner.severity))?;
                    bar.SetTitle(&hstring(banner.title.as_deref().unwrap_or("")))?;
                    bar.SetMessage(&hstring(&banner.message))?;
                    name(&bar, &banner.key, None);
                    children.Append(&element.cast::<UIElement>()?)?;
                    view.banners.push(BannerView {
                        key: banner.key.clone(),
                        bar,
                    });
                }
                UiBlock::Group(group) => {
                    let element = xaml::load(&xaml::group())?;
                    if let Some(title) = &group.title {
                        let block: TextBlock = xaml::find(&element, "Title")?;
                        set_text(&block, title);
                        show(&block, true);
                    }
                    if let Some(description) = &group.description {
                        let block: TextBlock = xaml::find(&element, "Description")?;
                        set_text(&block, description);
                        show(&block, true);
                    }
                    let items: StackPanel = xaml::find(&element, "Items")?;
                    let host = items.cast::<Panel>()?.Children()?;
                    for setting in &group.settings {
                        view.add_setting(env, &page.id, setting, &host)?;
                    }
                    children.Append(&element.cast::<UIElement>()?)?;
                }
                UiBlock::Actions(actions) => {
                    let row = xaml::load(&xaml::actions())?;
                    let host = row.cast::<Panel>()?.Children()?;
                    for button in &actions.buttons {
                        let element =
                            xaml::load(&xaml::button(look(button.style), button.icon, false))?;
                        let control: Button = xaml::find(&element, "Control")?;
                        let text: TextBlock = xaml::find(&element, "Text")?;
                        set_text(&text, &button.text);
                        name(&control, &button.key, Some(&button.text));
                        click(env, &page.id, &button.key, &control)?;
                        host.Append(&element.cast::<UIElement>()?)?;
                        view.buttons.push(ButtonView {
                            key: button.key.clone(),
                            button: control,
                            text,
                            busy: xaml::find(&element, "Busy")?,
                            enabled: true,
                            is_busy: false,
                        });
                    }
                    children.Append(&row.cast::<UIElement>()?)?;
                }
            }
        }
        Ok(view)
    }

    fn add_setting(
        &mut self,
        env: &Env,
        page: &str,
        setting: &UiSetting,
        host: &UIElementCollection,
    ) -> windows_core::Result<()> {
        let nested = !setting.settings.is_empty();
        let template = template(&setting.control);
        let card = xaml::load(&xaml::card(setting.icon, &template, nested))?;
        let label: TextBlock = xaml::find(&card, "Label")?;
        set_text(&label, &setting.label);
        let description: TextBlock = xaml::find(&card, "Description")?;
        if let Some(text) = &setting.description {
            set_text(&description, text);
            show(&description, true);
        }
        let statuses = [
            xaml::find(&card, "StatusInfo")?,
            xaml::find(&card, "StatusSuccess")?,
            xaml::find(&card, "StatusWarning")?,
            xaml::find(&card, "StatusError")?,
        ];

        let (control, editable) = build_control(env, page, setting, &card)?;
        let view = SettingView {
            key: setting.key.clone(),
            card: card.clone(),
            label,
            description,
            statuses,
            busy: xaml::find(&card, "Busy")?,
            editable,
            control,
            parent: None,
            local_error: None,
            props_status: None,
            enabled: true,
            is_busy: false,
        };
        host.Append(&card.cast::<UIElement>()?)?;
        self.settings.push(view);

        if nested {
            let expander: Expander = xaml::find(&card, "Card")?;
            let _ = expander.SetIsExpanded(false);
            let nested_host = xaml::find::<StackPanel>(&card, "Nested")?
                .cast::<Panel>()?
                .Children()?;
            for child in &setting.settings {
                self.add_setting(env, page, child, &nested_host)?;
            }
        }
        Ok(())
    }

    /// Applies the page's current values and properties. Values the parent has
    /// not changed since the last call leave user drafts alone.
    pub(super) fn apply(&mut self, state: &PageState) {
        for setting in &mut self.settings {
            let definition = match state.page.element(&setting.key) {
                Some(parsec_orbit_ui_protocol::schema::Element::Setting(definition)) => definition,
                _ => continue,
            };
            let props = state.props.get(&setting.key);
            if let UiControl::Select { options, .. } = &definition.control {
                let options = state.effective_options(&setting.key).unwrap_or(options);
                setting.apply_options(options);
            }
            if let Some(value) = state.values.get(&setting.key) {
                if setting.parent.as_ref() != Some(value) {
                    setting.set_value(value);
                    setting.parent = Some(value.clone());
                }
            } else if setting.parent.take().is_some() {
                setting.clear_value();
            }
            setting.apply_props(props);
        }
        for banner in &self.banners {
            let props = state.props.get(&banner.key);
            if let Some(props) = props {
                if let Some(severity) = props.severity {
                    let _ = banner.bar.SetSeverity(bar_severity(severity));
                }
                if let Some(title) = &props.title {
                    let _ = banner.bar.SetTitle(&hstring(title));
                }
                if let Some(message) = &props.message {
                    let _ = banner.bar.SetMessage(&hstring(message));
                }
                if let Some(visible) = props.visible {
                    show(&banner.bar, visible);
                }
            }
        }
        for button in &mut self.buttons {
            let Some(props) = state.props.get(&button.key) else {
                continue;
            };
            if let Some(text) = &props.text {
                set_text(&button.text, text);
            }
            if let Some(visible) = props.visible {
                show(&button.button, visible);
            }
            if let Some(enabled) = props.enabled {
                button.enabled = enabled;
            }
            if let Some(busy) = props.busy {
                button.is_busy = busy;
                let _ = button.busy.SetIsActive(busy);
                show(&button.busy, busy);
            }
            if props.enabled.is_some() || props.busy.is_some() {
                if let Ok(control) = button.button.cast::<Control>() {
                    let _ = control.SetIsEnabled(button.enabled && !button.is_busy);
                }
            }
        }
    }

    /// Shows a local problem with a setting's input, such as a value out of range,
    /// or clears it when `reason` is `None`.
    pub(super) fn set_local_error(&mut self, key: &str, reason: Option<&str>) {
        if let Some(setting) = self.settings.iter_mut().find(|setting| setting.key == key) {
            setting.local_error = reason.map(str::to_owned);
            setting.show_status();
        }
    }
}

/// Creates the control of a setting from its loaded card, wires its events and
/// applies the control's fixed configuration.
fn build_control(
    env: &Env,
    page: &str,
    setting: &UiSetting,
    card: &FrameworkElement,
) -> windows_core::Result<(ControlView, Option<Control>)> {
    let (page, key) = (page.to_owned(), setting.key.clone());
    let named = |part: &str| -> windows_core::Result<FrameworkElement> { xaml::find(card, part) };
    // A select's combo box is made when its choices arrive, so the host stands in
    // for it: it takes the enabled state, and the combo carries the identity.
    let select = matches!(setting.control, UiControl::Select { .. });
    let control_element = named(if select { "ComboHost" } else { "Control" })?;
    if !select {
        let shows_text = matches!(
            setting.control,
            UiControl::Value { .. } | UiControl::Log { .. }
        );
        name(
            &control_element,
            &key,
            (!shows_text).then_some(setting.label.as_str()),
        );
    }
    let editable = control_element.cast::<Control>().ok();
    let view = match &setting.control {
        UiControl::Toggle {
            on_text, off_text, ..
        } => {
            let toggle: ToggleSwitch = control_element.cast()?;
            if let Some(text) = on_text {
                toggle.SetOnContent(&boxed(text)?)?;
            }
            if let Some(text) = off_text {
                toggle.SetOffContent(&boxed(text)?)?;
            }
            let (env, source) = (env.clone(), toggle.clone());
            toggle.Toggled(&RoutedEventHandler::new(move |_, _| {
                env.edited(&page, &key, Some(Value::Bool(source.IsOn()?)));
                Ok(())
            }))?;
            ControlView::Toggle(toggle)
        }
        UiControl::Number {
            min,
            max,
            step,
            unit,
            ..
        } => {
            let number: NumberBox = control_element.cast()?;
            if let Some(min) = min {
                number.SetMinimum(*min)?;
            }
            if let Some(max) = max {
                number.SetMaximum(*max)?;
            }
            if let Some(step) = step {
                number.SetSmallChange(*step)?;
                number.SetLargeChange(*step * 10.0)?;
            }
            if let Some(unit) = unit {
                let block: TextBlock = xaml::find(card, "Unit")?;
                set_text(&block, unit);
                show(&block, true);
            }
            let env = env.clone();
            number.ValueChanged(&TypedEventHandler::new(
                move |_, args: Ref<NumberBoxValueChangedEventArgs>| {
                    let value = args.ok()?.NewValue()?;
                    if value.is_nan() {
                        env.invalid(&page, &key, "Enter a number.");
                    } else {
                        env.edited(&page, &key, Some(number_value(value)));
                    }
                    Ok(())
                },
            ))?;
            ControlView::Number(number)
        }
        UiControl::Slider {
            min,
            max,
            step,
            unit,
            ..
        } => {
            let slider: Slider = control_element.cast()?;
            slider.SetMinimum(*min)?;
            slider.SetMaximum(*max)?;
            let step = step.unwrap_or(1.0);
            slider.SetStepFrequency(step)?;
            slider.SetSmallChange(step)?;
            let readout: TextBlock = xaml::find(card, "Readout")?;
            let unit = unit.clone();
            let (env, readout_text, unit_text) = (env.clone(), readout.clone(), unit.clone());
            slider.ValueChanged(&RangeBaseValueChangedEventHandler::new(
                move |_, args: Ref<RangeBaseValueChangedEventArgs>| {
                    let value = args.ok()?.NewValue()?;
                    set_text(&readout_text, &number_text(value, unit_text.as_deref()));
                    env.edited(&page, &key, Some(number_value(value)));
                    Ok(())
                },
            ))?;
            ControlView::Slider {
                slider,
                readout,
                unit,
            }
        }
        UiControl::Select { .. } => {
            // The combo is made when the choices arrive, with the first apply.
            ControlView::Select {
                host: xaml::find(card, "ComboHost")?,
                combo: None,
                options: Vec::new(),
                env: env.clone(),
                page,
                label: setting.label.clone(),
            }
        }
        UiControl::Text {
            multiline, commit, ..
        } => {
            let text: TextBox = control_element.cast()?;
            let committed = Arc::new(Mutex::new(String::new()));
            let send = {
                let (env, source, committed) = (env.clone(), text.clone(), committed.clone());
                move |page: &str, key: &str| -> windows_core::Result<()> {
                    let current = edit_text(&source)?;
                    let mut last = committed.lock().unwrap();
                    if *last != current {
                        last.clone_from(&current);
                        env.edited(page, key, Some(Value::String(current)));
                    }
                    Ok(())
                }
            };
            match commit {
                UiCommit::Change => {
                    let (page, key, send) = (page.clone(), key.clone(), send.clone());
                    text.TextChanged(&TextChangedEventHandler::new(move |_, _| send(&page, &key)))?;
                }
                UiCommit::Submit => {
                    {
                        let (page, key, send) = (page.clone(), key.clone(), send.clone());
                        text.LostFocus(&RoutedEventHandler::new(move |_, _| send(&page, &key)))?;
                    }
                    if !multiline {
                        let (page, key, send) = (page.clone(), key.clone(), send.clone());
                        text.KeyDown(&KeyEventHandler::new(move |_, args| {
                            if args.ok()?.Key()? == VirtualKey::Enter {
                                send(&page, &key)?;
                            }
                            Ok(())
                        }))?;
                    }
                }
            }
            ControlView::Text { text, committed }
        }
        UiControl::Json { .. } => {
            let text: TextBox = control_element.cast()?;
            let committed = Arc::new(Mutex::new(String::new()));
            let (env, source, last) = (env.clone(), text.clone(), committed.clone());
            text.LostFocus(&RoutedEventHandler::new(move |_, _| {
                let current = edit_text(&source)?;
                let mut last = last.lock().unwrap();
                if *last == current {
                    return Ok(());
                }
                match serde_json::from_str::<Value>(&current) {
                    Ok(value) if value.is_object() || value.is_array() => {
                        last.clone_from(&current);
                        env.edited(&page, &key, Some(value));
                    }
                    Ok(_) => env.invalid(&page, &key, "Expected a JSON object or array."),
                    Err(error) => env.invalid(&page, &key, &format!("Not valid JSON: {error}.")),
                }
                Ok(())
            }))?;
            ControlView::Json { text, committed }
        }
        UiControl::Button { text, .. } => {
            let button: Button = control_element.cast()?;
            let label: TextBlock = xaml::find(card, "ButtonText")?;
            set_text(&label, text);
            click(env, &page, &key, &button)?;
            ControlView::Button(label)
        }
        UiControl::Value { .. } => ControlView::Value(control_element.cast()?),
        UiControl::Log { .. } => ControlView::Log {
            scroll: xaml::find(card, "LogScroll")?,
            text: control_element.cast()?,
        },
    };
    Ok((view, editable))
}

impl SettingView {
    /// Shows a value the parent set, without telling the parent about it.
    fn set_value(&mut self, value: &Value) {
        let text = |value: &Value| match value {
            Value::String(text) => text.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        match &self.control {
            ControlView::Toggle(toggle) => {
                let _ = toggle.SetIsOn(value.as_bool().unwrap_or(false));
            }
            ControlView::Number(number) => {
                let _ = number.SetValue(value.as_f64().unwrap_or(0.0));
            }
            ControlView::Slider {
                slider,
                readout,
                unit,
            } => {
                let number = value.as_f64().unwrap_or(0.0);
                let _ = slider.SetValue(number);
                set_text(readout, &number_text(number, unit.as_deref()));
            }
            ControlView::Select { combo, options, .. } => {
                let index = options.iter().position(|option| option.value == *value);
                if let Some(combo) = combo {
                    let _ = combo.SetSelectedIndex(index.map_or(-1, |index| index as i32));
                }
            }
            ControlView::Text {
                text: edit,
                committed,
            } => {
                let wanted = plain_newlines(&text(value));
                if edit_text(edit).ok().as_deref() != Some(wanted.as_str()) {
                    let _ = edit.SetText(&hstring(&wanted));
                }
                *committed.lock().unwrap() = wanted;
            }
            ControlView::Json {
                text: edit,
                committed,
            } => {
                let wanted = serde_json::to_string_pretty(value).unwrap_or_default();
                if edit_text(edit).ok().as_deref() != Some(wanted.as_str()) {
                    let _ = edit.SetText(&hstring(&wanted));
                }
                *committed.lock().unwrap() = wanted;
            }
            ControlView::Button(_) => {}
            ControlView::Value(block) => set_text(block, &text(value)),
            ControlView::Log {
                scroll,
                text: block,
            } => {
                let wanted = text(value);
                let at_end = scroll
                    .VerticalOffset()
                    .and_then(|offset| {
                        scroll
                            .ScrollableHeight()
                            .map(|height| offset >= height - 4.0)
                    })
                    .unwrap_or(true);
                if block
                    .Text()
                    .map(|current| current.to_string())
                    .ok()
                    .as_deref()
                    != Some(wanted.as_str())
                {
                    set_text(block, &wanted);
                    if at_end {
                        // The new height is known only after a layout pass.
                        if let Ok(element) = scroll.cast::<UIElement>() {
                            let _ = element.UpdateLayout();
                        }
                        if let Ok(height) = scroll.ScrollableHeight() {
                            let _ = scroll.ScrollToVerticalOffset(height);
                        }
                    }
                }
            }
        }
        // A parent value supersedes a local complaint about an earlier edit.
        if self.local_error.take().is_some() {
            self.show_status();
        }
    }

    /// The parent no longer holds a value for the control.
    fn clear_value(&mut self) {
        if let ControlView::Select {
            combo: Some(combo), ..
        } = &self.control
        {
            let _ = combo.SetSelectedIndex(-1);
        }
    }

    /// Replaces the choices of a select when they differ from the ones shown.
    fn apply_options(&mut self, wanted: &[UiOption]) {
        let ControlView::Select {
            host,
            combo,
            options,
            env,
            page,
            label,
        } = &mut self.control
        else {
            return;
        };
        if combo.is_some() && options.as_slice() == wanted {
            return;
        }
        *options = wanted.to_vec();
        match make_combo(env, page, &self.key, label, wanted) {
            Ok(made) => {
                let _ = host.SetContent(&made);
                *combo = Some(made);
            }
            Err(error) => {
                eprintln!("parsec-orbit-ui-companion: could not build a choice list: {error}")
            }
        }
        // The new combo starts without a selection; the next value applies it again.
        self.parent = None;
    }

    fn apply_props(&mut self, props: Option<&UiProps>) {
        let Some(props) = props else { return };
        if let Some(label) = &props.label {
            set_text(&self.label, label);
        }
        if let Some(text) = &props.description {
            set_text(&self.description, text);
            show(&self.description, !text.is_empty());
        }
        if let Some(text) = &props.text {
            if let ControlView::Button(block) = &self.control {
                set_text(block, text);
            }
        }
        if let Some(visible) = props.visible {
            show(&self.card, visible);
        }
        if let Some(enabled) = props.enabled {
            self.enabled = enabled;
        }
        if let Some(busy) = props.busy {
            self.is_busy = busy;
            let _ = self.busy.SetIsActive(busy);
            show(&self.busy, busy);
        }
        if props.enabled.is_some() || props.busy.is_some() {
            if let Some(control) = &self.editable {
                let _ = control.SetIsEnabled(self.enabled && !self.is_busy);
            }
        }
        if props.status.is_some() {
            self.props_status = props
                .status()
                .map(|status| (status.severity, status.text.clone()));
            self.show_status();
        }
    }

    /// Shows the local complaint if there is one, else the plugin's status.
    fn show_status(&self) {
        for line in &self.statuses {
            show(line, false);
        }
        let status = match (&self.local_error, &self.props_status) {
            (Some(reason), _) => Some((UiSeverity::Error, reason.as_str())),
            (None, Some((severity, text))) => Some((*severity, text.as_str())),
            (None, None) => None,
        };
        if let Some((severity, text)) = status {
            let line = &self.statuses[severity_index(severity)];
            set_text(line, text);
            show(line, true);
        }
    }
}

/// A combo box offering `options`, reporting the index the user chooses.
fn make_combo(
    env: &Env,
    page: &str,
    key: &str,
    label: &str,
    options: &[UiOption],
) -> windows_core::Result<ComboBox> {
    let element = xaml::load(&xaml::combo(options.len()))?;
    for (index, option) in options.iter().enumerate() {
        let item: ComboBoxItem = xaml::find(&element, &format!("Item{index}"))?;
        item.SetContent(&boxed(&option.label)?)?;
    }
    let combo: ComboBox = element.cast()?;
    name(&combo, key, Some(label));
    let (env, page, key, source) = (env.clone(), page.to_owned(), key.to_owned(), combo.clone());
    combo.SelectionChanged(&SelectionChangedEventHandler::new(move |_, _| {
        let index = source.SelectedIndex()?;
        if index >= 0 {
            env.chosen(&page, &key, index as usize);
        }
        Ok(())
    }))?;
    Ok(combo)
}

impl PageView {
    /// The text of a select's choice, for a message from its selection event.
    pub(super) fn choice(&self, key: &str, index: usize) -> Option<Value> {
        self.settings
            .iter()
            .find_map(|setting| match &setting.control {
                ControlView::Select { options, .. } if setting.key == key => {
                    options.get(index).map(|option| option.value.clone())
                }
                _ => None,
            })
    }
}
