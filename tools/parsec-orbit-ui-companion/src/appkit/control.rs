//! The AppKit control behind a setting or an action button: how it is built, how
//! its events become messages, and how parent values and properties reach it.

use std::cell::RefCell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{sel, MainThreadMarker, Message};
use objc2_app_kit::{
    NSAccessibility, NSBezelStyle, NSBorderType, NSButton, NSButtonType, NSCellImagePosition,
    NSColor, NSControlSize, NSControlStateValueOff, NSControlStateValueOn,
    NSControlTextDidChangeNotification, NSControlTextDidEndEditingNotification, NSPasteboard,
    NSPopUpButton, NSProgressIndicator, NSProgressIndicatorStyle, NSScrollView, NSSlider,
    NSStackView, NSStepper, NSSwitch, NSTextDidChangeNotification, NSTextDidEndEditingNotification,
    NSTextField, NSTextView, NSView,
};
use objc2_foundation::{NSRange, NSString};
use parsec_orbit_ui_protocol::schema::{
    number_value, UiButton, UiButtonStyle, UiCommit, UiControl, UiSetting,
};
use parsec_orbit_ui_protocol::{UiOption, UiProps};
use serde_json::Value;

use super::icons;
use super::kit;
use super::target::{ControlTarget, Env, Event};
use crate::model;

/// Text with a trailing unit, as a number reads in a field or a readout.
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

/// A text editor: a one-line field or a text view in a scroll view.
#[derive(Clone)]
enum Editor {
    Line(Retained<NSTextField>),
    Area(Retained<NSTextView>),
}

impl Editor {
    fn object(&self) -> &AnyObject {
        match self {
            Editor::Line(field) => field,
            Editor::Area(view) => view,
        }
    }

    /// The text with plain line breaks, as plugins get it.
    fn text(&self) -> String {
        let text = match self {
            Editor::Line(field) => field.stringValue().to_string(),
            Editor::Area(view) => view.string().to_string(),
        };
        model::normalize_newlines(&text)
    }

    fn set_text(&self, text: &str) {
        if self.text() == text {
            return;
        }
        match self {
            Editor::Line(field) => field.setStringValue(&kit::string(text)),
            Editor::Area(view) => view.setString(&kit::string(text)),
        }
    }

    fn set_enabled(&self, enabled: bool) {
        match self {
            Editor::Line(field) => field.setEnabled(enabled),
            Editor::Area(view) => view.setEditable(enabled),
        }
    }

    /// Calls `target` for the editor's change and end-editing notifications.
    fn observe(&self, target: &ControlTarget, changes: bool) {
        // SAFETY: these are AppKit's own notification names.
        let (changed, ended) = unsafe {
            match self {
                Editor::Line(_) => (
                    NSControlTextDidChangeNotification,
                    NSControlTextDidEndEditingNotification,
                ),
                Editor::Area(_) => (NSTextDidChangeNotification, NSTextDidEndEditingNotification),
            }
        };
        if changes {
            target.observe(changed, self.object(), false);
        }
        target.observe(ended, self.object(), true);
    }
}

enum Kind {
    Toggle {
        switch: Retained<NSSwitch>,
        caption: Option<(Retained<NSTextField>, Option<String>, Option<String>)>,
    },
    Number {
        field: Retained<NSTextField>,
        stepper: Retained<NSStepper>,
        last: Rc<RefCell<Value>>,
    },
    Slider {
        slider: Retained<NSSlider>,
        readout: Retained<NSTextField>,
        unit: Option<String>,
    },
    Select {
        popup: Retained<NSPopUpButton>,
        options: Rc<RefCell<Vec<UiOption>>>,
    },
    Text {
        editor: Editor,
        committed: Rc<RefCell<String>>,
    },
    Json {
        editor: Editor,
        committed: Rc<RefCell<String>>,
    },
    Button(Retained<NSButton>),
    Value(Retained<NSTextField>),
    Log {
        scroll: Retained<NSScrollView>,
        view: Retained<NSTextView>,
    },
}

/// The control of one setting with what its updates need.
pub(super) struct Control {
    kind: Kind,
    view: Option<Retained<NSView>>,
    busy: Retained<NSProgressIndicator>,
    /// Where the accessible name is set, for a later label.
    named: Option<Retained<NSView>>,
    enablers: Vec<Box<dyn Fn(bool)>>,
    /// Targets live as long as the control; AppKit holds them weakly.
    _targets: Vec<Retained<ControlTarget>>,
}

impl Control {
    pub(super) fn busy_indicator(&self) -> &Retained<NSProgressIndicator> {
        &self.busy
    }

    /// The control as a view for the row, or `None` when the row is the control.
    pub(super) fn view(&self) -> Option<Retained<NSView>> {
        self.view.clone()
    }

    /// Whether the control needs the row's whole width, below the label.
    pub(super) fn is_wide(&self) -> bool {
        matches!(
            self.kind,
            Kind::Text {
                editor: Editor::Area(_),
                ..
            } | Kind::Json { .. }
                | Kind::Log { .. }
        )
    }

    pub(super) fn has_options(&self) -> bool {
        matches!(self.kind, Kind::Select { .. })
    }

    /// Replaces the choices of a select when they differ from the ones shown.
    /// Returns whether it did.
    pub(super) fn set_options(&mut self, wanted: &[UiOption]) -> bool {
        let Kind::Select { popup, options } = &self.kind else {
            return false;
        };
        if options.borrow().as_slice() == wanted {
            return false;
        }
        *options.borrow_mut() = wanted.to_vec();
        let menu = popup.menu();
        match menu {
            Some(menu) => {
                menu.removeAllItems();
                for option in wanted {
                    // SAFETY: an item without an action is valid; the popup selects it.
                    unsafe {
                        menu.addItemWithTitle_action_keyEquivalent(
                            &kit::string(&option.label),
                            None,
                            &kit::string(""),
                        );
                    }
                }
            }
            None => return false,
        }
        popup.selectItemAtIndex(-1);
        true
    }

    /// Shows a value the parent set, without telling the parent about it.
    pub(super) fn set_value(&self, value: &Value) {
        let text = |value: &Value| match value {
            Value::String(text) => text.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        match &self.kind {
            Kind::Toggle { switch, caption } => {
                let on = value.as_bool().unwrap_or(false);
                switch.setState(if on {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
                if let Some((field, on_text, off_text)) = caption {
                    field.setStringValue(&kit::string(
                        if on { on_text } else { off_text }.as_deref().unwrap_or(""),
                    ));
                }
            }
            Kind::Number {
                field,
                stepper,
                last,
            } => {
                let number = value.as_f64().unwrap_or(0.0);
                field.setStringValue(&kit::string(&number_text(number, None)));
                stepper.setDoubleValue(number);
                *last.borrow_mut() = value.clone();
            }
            Kind::Slider {
                slider,
                readout,
                unit,
            } => {
                let number = value.as_f64().unwrap_or(0.0);
                slider.setDoubleValue(number);
                readout.setStringValue(&kit::string(&number_text(number, unit.as_deref())));
            }
            Kind::Select { popup, options } => {
                let index = options
                    .borrow()
                    .iter()
                    .position(|option| option.value == *value);
                popup.selectItemAtIndex(index.map_or(-1, |index| index as isize));
            }
            Kind::Text { editor, committed } => {
                let wanted = model::normalize_newlines(&text(value));
                editor.set_text(&wanted);
                *committed.borrow_mut() = wanted;
            }
            Kind::Json { editor, committed } => {
                let wanted = serde_json::to_string_pretty(value).unwrap_or_default();
                editor.set_text(&wanted);
                *committed.borrow_mut() = wanted;
            }
            Kind::Button(_) => {}
            Kind::Value(field) => field.setStringValue(&kit::string(&text(value))),
            Kind::Log { scroll, view } => {
                let wanted = text(value);
                if view.string().to_string() == wanted {
                    return;
                }
                let document = view.frame().size.height;
                let visible = scroll.documentVisibleRect();
                let at_end = visible.origin.y + visible.size.height >= document - 4.0;
                view.setString(&kit::string(&wanted));
                if at_end {
                    let end = wanted.encode_utf16().count();
                    view.scrollRangeToVisible(NSRange::new(end, 0));
                }
            }
        }
    }

    /// The parent no longer holds a value for the control.
    pub(super) fn clear_value(&self) {
        if let Kind::Select { popup, .. } = &self.kind {
            popup.selectItemAtIndex(-1);
        }
    }

    pub(super) fn relabel(&self, label: &str) {
        if let Some(view) = &self.named {
            view.setAccessibilityLabel(Some(&kit::string(label)));
        }
    }

    pub(super) fn set_button_text(&self, text: &str) {
        if let Kind::Button(button) = &self.kind {
            button.setTitle(&kit::string(text));
        }
    }

    pub(super) fn set_busy(&self, busy: bool) {
        self.busy.setHidden(!busy);
        if busy {
            // SAFETY: a nil sender is what AppKit expects from code.
            unsafe { self.busy.startAnimation(None) };
        } else {
            // SAFETY: as above.
            unsafe { self.busy.stopAnimation(None) };
        }
    }

    pub(super) fn set_enabled(&self, enabled: bool) {
        for enable in &self.enablers {
            enable(enabled);
        }
    }
}

fn busy_indicator(mtm: MainThreadMarker) -> Retained<NSProgressIndicator> {
    let spinner = NSProgressIndicator::new(mtm);
    spinner.setStyle(NSProgressIndicatorStyle::Spinning);
    spinner.setControlSize(NSControlSize::Small);
    spinner.setDisplayedWhenStopped(false);
    spinner.setTranslatesAutoresizingMaskIntoConstraints(false);
    spinner.setHidden(true);
    spinner
}

fn action<T: AsRef<objc2_app_kit::NSControl>>(control: &T, target: &Retained<ControlTarget>) {
    let control: &objc2_app_kit::NSControl = control.as_ref();
    // SAFETY: `fire:` takes the sender, as target-action requires; the target is
    // kept alive by the control's owner.
    unsafe {
        control.setTarget(Some(target));
        control.setAction(Some(sel!(fire:)));
    }
}

/// An editor for text: one line, or a scrolling area of `rows` lines.
fn make_editor(
    mtm: MainThreadMarker,
    multiline: bool,
    monospace: bool,
    rows: f64,
    placeholder: Option<&str>,
) -> (Editor, Retained<NSView>) {
    if !multiline {
        let field = NSTextField::textFieldWithString(&kit::string(""), mtm);
        if let Some(placeholder) = placeholder {
            field.setPlaceholderString(Some(&kit::string(placeholder)));
        }
        if monospace {
            field.setFont(Some(&kit::monospaced(13.0)));
        }
        field.setTranslatesAutoresizingMaskIntoConstraints(false);
        kit::width(&field, 260.0);
        let view = kit::view(&*field);
        return (Editor::Line(field), view);
    }
    let scroll = NSTextView::scrollableTextView(mtm);
    let view = scroll
        .documentView()
        .and_then(|view| view.downcast::<NSTextView>().ok())
        .unwrap_or_else(|| NSTextView::new(mtm));
    view.setRichText(false);
    view.setAutomaticQuoteSubstitutionEnabled(false);
    view.setAutomaticDashSubstitutionEnabled(false);
    view.setAutomaticTextReplacementEnabled(false);
    let font = if monospace {
        kit::monospaced(12.0)
    } else {
        objc2_app_kit::NSFont::systemFontOfSize(13.0)
    };
    view.setFont(Some(&font));
    scroll.setBorderType(NSBorderType::BezelBorder);
    scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
    scroll
        .heightAnchor()
        .constraintEqualToConstant(rows * 17.0 + 8.0)
        .setActive(true);
    let outer = kit::view(&*scroll);
    (Editor::Area(view), outer)
}

/// Creates the control of a setting and wires its events.
pub(super) fn build(mtm: MainThreadMarker, env: &Env, page: &str, setting: &UiSetting) -> Control {
    let (page, key) = (page.to_owned(), setting.key.clone());
    let mut targets: Vec<Retained<ControlTarget>> = Vec::new();
    let mut enablers: Vec<Box<dyn Fn(bool)>> = Vec::new();
    let mut named: Option<Retained<NSView>> = None;
    let label = setting.label.as_str();

    let (kind, view): (Kind, Option<Retained<NSView>>) = match &setting.control {
        UiControl::Toggle {
            on_text, off_text, ..
        } => {
            let switch = NSSwitch::new(mtm);
            switch.setTranslatesAutoresizingMaskIntoConstraints(false);
            let target = ControlTarget::new(mtm, {
                let (env, page, key, source) =
                    (env.clone(), page.clone(), key.clone(), switch.clone());
                move |_| {
                    env.edited(
                        &page,
                        &key,
                        Some(Value::Bool(source.state() == NSControlStateValueOn)),
                    );
                }
            });
            action(&switch, &target);
            targets.push(target);
            kit::name(&switch, &key, Some(label));
            named = Some(kit::view(&*switch));
            let source = switch.clone();
            enablers.push(Box::new(move |enabled| source.setEnabled(enabled)));
            let caption = (on_text.is_some() || off_text.is_some())
                .then(|| (kit::line(mtm, ""), on_text.clone(), off_text.clone()));
            let view = match &caption {
                Some((field, ..)) => {
                    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
                    kit::view(&*kit::hstack(mtm, &[field, &switch], 8.0))
                }
                None => kit::view(&*switch),
            };
            (Kind::Toggle { switch, caption }, Some(view))
        }
        UiControl::Number {
            min,
            max,
            step,
            unit,
            ..
        } => {
            let field = NSTextField::textFieldWithString(&kit::string(""), mtm);
            field.setAlignment(objc2_app_kit::NSTextAlignment::Right);
            field.setTranslatesAutoresizingMaskIntoConstraints(false);
            kit::width(&field, 90.0);
            let stepper = NSStepper::new(mtm);
            stepper.setMinValue(min.unwrap_or(-1e15));
            stepper.setMaxValue(max.unwrap_or(1e15));
            stepper.setIncrement(step.unwrap_or(1.0));
            stepper.setValueWraps(false);
            stepper.setTranslatesAutoresizingMaskIntoConstraints(false);
            let last = Rc::new(RefCell::new(Value::Null));
            let control = setting.control.clone();
            let commit = {
                let (env, page, key) = (env.clone(), page.clone(), key.clone());
                let (stepper, last) = (stepper.clone(), last.clone());
                move |text: String| match model::number_edit(&control, &text) {
                    Ok(value) => {
                        stepper.setDoubleValue(value.as_f64().unwrap_or(0.0));
                        if *last.borrow() != value {
                            *last.borrow_mut() = value.clone();
                            env.edited(&page, &key, Some(value));
                        }
                    }
                    Err(reason) => env.invalid(&page, &key, &reason),
                }
            };
            let typed = ControlTarget::new(mtm, {
                let (field, commit) = (field.clone(), commit.clone());
                move |event| {
                    if event == Event::Ended {
                        commit(field.stringValue().to_string());
                    }
                }
            });
            Editor::Line(field.clone()).observe(&typed, false);
            let stepped = ControlTarget::new(mtm, {
                let (field, stepper, commit) = (field.clone(), stepper.clone(), commit);
                move |_| {
                    let text = number_text(stepper.doubleValue(), None);
                    field.setStringValue(&kit::string(&text));
                    commit(text);
                }
            });
            action(&stepper, &stepped);
            targets.extend([typed, stepped]);
            kit::name(&field, &key, Some(label));
            kit::name(&stepper, &format!("{key}.stepper"), Some(label));
            named = Some(kit::view(&*field));
            let (f, s) = (field.clone(), stepper.clone());
            enablers.push(Box::new(move |enabled| {
                f.setEnabled(enabled);
                s.setEnabled(enabled);
            }));
            let mut parts: Vec<Retained<NSView>> = vec![kit::view(&*field)];
            if let Some(unit) = unit {
                let unit = kit::line(mtm, unit);
                unit.setTextColor(Some(&NSColor::secondaryLabelColor()));
                parts.push(kit::view(&*unit));
            }
            parts.push(kit::view(&*stepper));
            let refs: Vec<&NSView> = parts.iter().map(|part| &**part).collect();
            let view = kit::view(&*kit::hstack(mtm, &refs, 6.0));
            (
                Kind::Number {
                    field,
                    stepper,
                    last,
                },
                Some(view),
            )
        }
        UiControl::Slider {
            min,
            max,
            step,
            unit,
            ..
        } => {
            let slider = NSSlider::new(mtm);
            slider.setMinValue(*min);
            slider.setMaxValue(*max);
            slider.setContinuous(true);
            slider.setTranslatesAutoresizingMaskIntoConstraints(false);
            kit::width(&slider, 200.0);
            let readout = kit::line(mtm, "");
            readout.setAlignment(objc2_app_kit::NSTextAlignment::Right);
            readout.setTextColor(Some(&NSColor::secondaryLabelColor()));
            readout.setTranslatesAutoresizingMaskIntoConstraints(false);
            kit::width(&readout, 64.0);
            let (low, high, step) = (*min, *max, *step);
            let target = ControlTarget::new(mtm, {
                let (env, page, key) = (env.clone(), page.clone(), key.clone());
                let (source, readout, unit) = (slider.clone(), readout.clone(), unit.clone());
                move |_| {
                    let mut number = source.doubleValue();
                    if let Some(step) = step.filter(|step| *step > 0.0) {
                        number = (low + ((number - low) / step).round() * step).clamp(low, high);
                    }
                    readout.setStringValue(&kit::string(&number_text(number, unit.as_deref())));
                    env.edited(&page, &key, Some(number_value(number)));
                }
            });
            action(&slider, &target);
            targets.push(target);
            kit::name(&slider, &key, Some(label));
            named = Some(kit::view(&*slider));
            let source = slider.clone();
            enablers.push(Box::new(move |enabled| source.setEnabled(enabled)));
            let view = kit::view(&*kit::hstack(mtm, &[&slider, &readout], 8.0));
            (
                Kind::Slider {
                    slider,
                    readout,
                    unit: unit.clone(),
                },
                Some(view),
            )
        }
        UiControl::Select { .. } => {
            let popup = NSPopUpButton::initWithFrame_pullsDown(
                mtm.alloc(),
                objc2_foundation::NSRect::ZERO,
                false,
            );
            popup.setTranslatesAutoresizingMaskIntoConstraints(false);
            let options = Rc::new(RefCell::new(Vec::<UiOption>::new()));
            let target = ControlTarget::new(mtm, {
                let (env, page, key) = (env.clone(), page.clone(), key.clone());
                let (source, options) = (popup.clone(), options.clone());
                move |_| {
                    let index = source.indexOfSelectedItem();
                    let value = usize::try_from(index)
                        .ok()
                        .and_then(|index| options.borrow().get(index).map(|o| o.value.clone()));
                    if let Some(value) = value {
                        env.edited(&page, &key, Some(value));
                    }
                }
            });
            action(&popup, &target);
            targets.push(target);
            kit::name(&popup, &key, Some(label));
            named = Some(kit::view(&*popup));
            let source = popup.clone();
            enablers.push(Box::new(move |enabled| source.setEnabled(enabled)));
            let view = kit::view(&*popup);
            (Kind::Select { popup, options }, Some(view))
        }
        UiControl::Text {
            multiline,
            rows,
            monospace,
            placeholder,
            commit,
            ..
        } => {
            let (editor, outer) = make_editor(
                mtm,
                *multiline,
                *monospace,
                rows.map_or(4.0, |rows| rows as f64),
                placeholder.as_deref(),
            );
            let committed = Rc::new(RefCell::new(String::new()));
            let target = ControlTarget::new(mtm, {
                let (env, page, key) = (env.clone(), page.clone(), key.clone());
                let (editor, committed) = (editor.clone(), committed.clone());
                let on_change = *commit == UiCommit::Change;
                move |event| {
                    let wanted = match event {
                        Event::Changed => on_change,
                        Event::Ended => true,
                        Event::Action => false,
                    };
                    if !wanted {
                        return;
                    }
                    let current = editor.text();
                    if *committed.borrow() != current {
                        committed.borrow_mut().clone_from(&current);
                        env.edited(&page, &key, Some(Value::String(current)));
                    }
                }
            });
            editor.observe(&target, *commit == UiCommit::Change);
            targets.push(target);
            kit::name(&outer, &key, Some(label));
            if let Editor::Area(view) = &editor {
                kit::name(view, &key, Some(label));
            }
            named = Some(outer.clone());
            let source = editor.clone();
            enablers.push(Box::new(move |enabled| source.set_enabled(enabled)));
            (Kind::Text { editor, committed }, Some(outer))
        }
        UiControl::Json { .. } => {
            let (editor, outer) = make_editor(mtm, true, true, 8.0, None);
            let committed = Rc::new(RefCell::new(String::new()));
            let target = ControlTarget::new(mtm, {
                let (env, page, key) = (env.clone(), page.clone(), key.clone());
                let (editor, committed) = (editor.clone(), committed.clone());
                move |event| {
                    if event != Event::Ended {
                        return;
                    }
                    let current = editor.text();
                    if *committed.borrow() == current {
                        return;
                    }
                    match model::json_edit(&current) {
                        Ok(value) => {
                            committed.borrow_mut().clone_from(&current);
                            env.edited(&page, &key, Some(value));
                        }
                        Err(reason) => env.invalid(&page, &key, &reason),
                    }
                }
            });
            editor.observe(&target, false);
            targets.push(target);
            kit::name(&outer, &key, Some(label));
            if let Editor::Area(view) = &editor {
                kit::name(view, &key, Some(label));
            }
            named = Some(outer.clone());
            let source = editor.clone();
            enablers.push(Box::new(move |enabled| source.set_enabled(enabled)));
            (Kind::Json { editor, committed }, Some(outer))
        }
        UiControl::Button {
            text, style, icon, ..
        } => {
            let button = push_button(mtm, env, &page, &key, text, *style, *icon, &mut targets);
            let source = button.clone();
            enablers.push(Box::new(move |enabled| source.setEnabled(enabled)));
            kit::name(&button, &key, Some(text));
            let view = kit::view(&*button);
            (Kind::Button(button), Some(view))
        }
        UiControl::Value {
            monospace,
            copyable,
            ..
        } => {
            let field = kit::label(mtm, "");
            field.setSelectable(true);
            field.setPreferredMaxLayoutWidth(320.0);
            field.setAlignment(objc2_app_kit::NSTextAlignment::Right);
            if *monospace {
                field.setFont(Some(&kit::monospaced(12.0)));
            }
            // The text is its own accessible name.
            kit::name(&field, &key, None);
            let view = if *copyable {
                let target = ControlTarget::new(mtm, {
                    let source = field.clone();
                    move |_| {
                        let board = NSPasteboard::generalPasteboard();
                        board.clearContents();
                        let text: Retained<NSString> = source.stringValue();
                        // SAFETY: plain text under the standard string type.
                        unsafe {
                            board.setString_forType(&text, objc2_app_kit::NSPasteboardTypeString);
                        }
                    }
                });
                // SAFETY: `fire:` is implemented by `ControlTarget`.
                let copy = unsafe {
                    NSButton::buttonWithTitle_target_action(
                        &kit::string(""),
                        Some(&target),
                        Some(sel!(fire:)),
                        mtm,
                    )
                };
                copy.setImage(
                    icons::image(parsec_orbit_ui_protocol::schema::UiIcon::Copy).as_deref(),
                );
                copy.setBezelStyle(NSBezelStyle::Toolbar);
                copy.setBordered(false);
                kit::name(&copy, &format!("{key}.copy"), Some("Copy"));
                targets.push(target);
                kit::view(&*kit::hstack(mtm, &[&field, &copy], 6.0))
            } else {
                kit::view(&*field)
            };
            (Kind::Value(field), Some(view))
        }
        UiControl::Log { rows, .. } => {
            let (editor, outer) =
                make_editor(mtm, true, true, rows.map_or(10.0, |r| r as f64), None);
            let Editor::Area(view) = editor else {
                unreachable!("a log is multi-line");
            };
            view.setEditable(false);
            view.setSelectable(true);
            let scroll = outer
                .downcast::<NSScrollView>()
                .unwrap_or_else(|_| NSScrollView::new(mtm));
            // The text is its own accessible name.
            kit::name(&view, &key, None);
            kit::name(&scroll, &format!("{key}.log"), None);
            let outer = kit::view(&*scroll);
            (Kind::Log { scroll, view }, Some(outer))
        }
    };

    Control {
        kind,
        view,
        busy: busy_indicator(mtm),
        named,
        enablers,
        _targets: targets,
    }
}

#[allow(clippy::too_many_arguments)]
fn push_button(
    mtm: MainThreadMarker,
    env: &Env,
    page: &str,
    key: &str,
    text: &str,
    style: UiButtonStyle,
    icon: Option<parsec_orbit_ui_protocol::schema::UiIcon>,
    targets: &mut Vec<Retained<ControlTarget>>,
) -> Retained<NSButton> {
    let target = ControlTarget::new(mtm, {
        let (env, page, key) = (env.clone(), page.to_owned(), key.to_owned());
        move |_| env.edited(&page, &key, None)
    });
    // SAFETY: `fire:` is implemented by `ControlTarget`.
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &kit::string(text),
            Some(&target),
            Some(sel!(fire:)),
            mtm,
        )
    };
    button.setBezelStyle(NSBezelStyle::Push);
    button.setTranslatesAutoresizingMaskIntoConstraints(false);
    match style {
        UiButtonStyle::Standard => {}
        UiButtonStyle::Accent => button.setBezelColor(Some(&NSColor::controlAccentColor())),
        UiButtonStyle::Destructive => button.setHasDestructiveAction(true),
    }
    if let Some(image) = icon.and_then(icons::image) {
        button.setImage(Some(&image));
        button.setImagePosition(NSCellImagePosition::ImageLeading);
    }
    targets.push(target);
    button
}

/// An action button of an `actions` block, with the target the page keeps
/// alive for it.
pub(super) fn action_button(
    mtm: MainThreadMarker,
    env: &Env,
    page: &str,
    button: &UiButton,
) -> (Retained<NSButton>, Retained<ControlTarget>) {
    let mut targets = Vec::new();
    let built = push_button(
        mtm,
        env,
        page,
        &button.key,
        &button.text,
        button.style,
        button.icon,
        &mut targets,
    );
    kit::name(&built, &button.key, Some(&button.text));
    (built, targets.remove(0))
}

/// Applies the properties an action button honors.
pub(super) fn apply_button_props(button: &NSButton, props: &UiProps) {
    if let Some(text) = &props.text {
        button.setTitle(&kit::string(text));
    }
    if let Some(visible) = props.visible {
        button.setHidden(!visible);
    }
    if props.enabled.is_some() || props.busy.is_some() {
        let enabled = props.enabled.unwrap_or(true) && !props.busy.unwrap_or(false);
        button.setEnabled(enabled);
    }
}

/// A disclosure triangle that shows or hides the nested rows, with the target
/// the page keeps alive for it.
pub(super) fn disclosure(
    mtm: MainThreadMarker,
    rows: &NSStackView,
) -> (Retained<NSButton>, Retained<ControlTarget>) {
    let button = NSButton::new(mtm);
    button.setButtonType(NSButtonType::OnOff);
    button.setBezelStyle(NSBezelStyle::Disclosure);
    button.setTitle(&kit::string(""));
    button.setTranslatesAutoresizingMaskIntoConstraints(false);
    kit::name(&button, "disclosure", Some("Show details"));
    let target = ControlTarget::new(mtm, {
        let (rows, source) = (rows.retain(), button.clone());
        move |_| rows.setHidden(source.state() != NSControlStateValueOn)
    });
    action(&button, &target);
    (button, target)
}
