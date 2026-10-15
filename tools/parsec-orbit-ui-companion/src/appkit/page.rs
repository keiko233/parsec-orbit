//! A page as AppKit views: building it from a [`PageState`], reading the user's
//! edits out of its controls, and applying later values and properties to it.
//!
//! Every plugin string reaches the views as a plain string value, never as markup
//! or attributed text.

use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSBox, NSButton, NSColor, NSFont, NSImageView, NSLayoutAttribute, NSStackView,
    NSStackViewDistribution, NSTextField, NSView,
};
use parsec_orbit_ui_protocol::schema::{Element, UiSetting, UiSeverity};
use parsec_orbit_ui_protocol::{PageState, UiBlock, UiProps};
use serde_json::Value;

use super::control::{self, Control};
use super::kit;
use super::target::{ControlTarget, Env};
use crate::model;

/// A banner as built: a tinted strip with an icon, a title and a message.
struct BannerView {
    key: String,
    surface: Retained<NSBox>,
    icon: Retained<NSImageView>,
    title: Retained<NSTextField>,
    message: Retained<NSTextField>,
}

/// An action button as built.
struct ButtonView {
    key: String,
    button: Retained<NSButton>,
}

/// One setting as built.
struct SettingView {
    key: String,
    /// The row, shown or hidden as a whole.
    row: Retained<NSView>,
    label: Retained<NSTextField>,
    description: Retained<NSTextField>,
    status: Status,
    control: Control,
    /// The last value the parent set; an unrelated update never replaces a draft.
    parent: Option<Value>,
    local_error: Option<String>,
    props_status: Option<(UiSeverity, String)>,
    enabled: bool,
    busy: bool,
}

/// The line under a setting for the plugin's status or a local complaint.
struct Status {
    stack: Retained<NSStackView>,
    icon: Retained<NSImageView>,
    text: Retained<NSTextField>,
}

/// A built page.
pub(super) struct PageView {
    pub(super) root: Retained<NSStackView>,
    settings: Vec<SettingView>,
    banners: Vec<BannerView>,
    buttons: Vec<ButtonView>,
    /// Targets of the buttons and disclosures; AppKit holds targets weakly.
    targets: Vec<Retained<ControlTarget>>,
}

fn bold(size: f64) -> Retained<NSFont> {
    NSFont::boldSystemFontOfSize(size)
}

fn symbol_for(severity: UiSeverity) -> parsec_orbit_ui_protocol::schema::UiIcon {
    use parsec_orbit_ui_protocol::schema::UiIcon;
    match severity {
        UiSeverity::Info => UiIcon::Info,
        UiSeverity::Success => UiIcon::Success,
        UiSeverity::Warning => UiIcon::Warning,
        UiSeverity::Error => UiIcon::Error,
    }
}

impl PageView {
    /// Builds the page. Values and properties follow with [`PageView::apply`].
    pub(super) fn build(mtm: MainThreadMarker, env: &Env, state: &PageState) -> PageView {
        let page = &state.page;
        let root = kit::vstack_in(mtm, &[], 18.0, kit::insets(20.0, 24.0, 28.0, 24.0));
        if let Some(description) = &page.description {
            kit::add_filled(&root, &kit::label(mtm, description));
        }
        let mut view = PageView {
            root: root.clone(),
            settings: Vec::new(),
            banners: Vec::new(),
            buttons: Vec::new(),
            targets: Vec::new(),
        };
        for block in &page.blocks {
            match block {
                UiBlock::Banner(banner) => {
                    let built = banner_view(mtm, banner);
                    kit::add_filled(&root, &built.surface);
                    view.banners.push(built);
                }
                UiBlock::Group(group) => {
                    let section = kit::vstack(mtm, &[], 6.0);
                    if let Some(title) = &group.title {
                        let heading = kit::label(mtm, title);
                        heading.setFont(Some(&bold(13.0)));
                        kit::add_filled(&section, &heading);
                    }
                    if let Some(description) = &group.description {
                        kit::add_filled(&section, &kit::caption(mtm, description));
                    }
                    let rows = kit::vstack(mtm, &[], 0.0);
                    let mut first = true;
                    for setting in &group.settings {
                        view.add_setting(mtm, env, &page.id, setting, &rows, &mut first, false);
                    }
                    let card = kit::surface(&rows, mtm, 12.0, kit::insets(0.0, 0.0, 0.0, 0.0));
                    card.setFillColor(&NSColor::controlBackgroundColor());
                    card.setBorderWidth(1.0);
                    card.setBorderColor(&NSColor::separatorColor());
                    kit::add_filled(&section, &card);
                    kit::add_filled(&root, &section);
                }
                UiBlock::Actions(actions) => {
                    let row = kit::hstack(mtm, &[], 10.0);
                    for button in &actions.buttons {
                        let (built, target) = control::action_button(mtm, env, &page.id, button);
                        row.addArrangedSubview(&built);
                        view.targets.push(target);
                        view.buttons.push(ButtonView {
                            key: button.key.clone(),
                            button: built,
                        });
                    }
                    kit::add_filled(&root, &row);
                }
            }
        }
        view
    }

    #[allow(clippy::too_many_arguments)]
    fn add_setting(
        &mut self,
        mtm: MainThreadMarker,
        env: &Env,
        page: &str,
        setting: &UiSetting,
        host: &NSStackView,
        first: &mut bool,
        nested: bool,
    ) {
        let label = kit::line(mtm, &setting.label);
        let description = kit::caption(mtm, setting.description.as_deref().unwrap_or(""));
        description.setHidden(setting.description.is_none());
        let status = status_line(mtm);
        let control = control::build(mtm, env, page, setting);

        let icon = kit::symbol_view(mtm, setting.icon.and_then(super::icons::image), None);
        kit::width(&icon, 22.0);
        let text = kit::vstack(mtm, &[&label, &description, &status.stack], 2.0);
        let head = kit::hstack(mtm, &[&icon, &text], 10.0);
        let busy = control.busy_indicator().clone();
        let nested_rows = (!setting.settings.is_empty()).then(|| {
            let rows = kit::vstack(mtm, &[], 0.0);
            rows.setHidden(true);
            rows
        });
        let pad = kit::insets(10.0, if nested { 38.0 } else { 14.0 }, 10.0, 14.0);
        let row = if control.is_wide() || nested_rows.is_some() {
            let top = kit::hstack(mtm, &[&head, &busy], 8.0);
            if let Some(rows) = &nested_rows {
                let (toggle, target) = control::disclosure(mtm, rows);
                top.insertArrangedSubview_atIndex(&toggle, 0);
                self.targets.push(target);
            }
            let body = kit::vstack_in(mtm, &[&top], 8.0, pad);
            if let Some(view) = control.view() {
                kit::add_filled(&body, &view);
            }
            body
        } else {
            let trailing = kit::hstack(mtm, &[&busy], 8.0);
            if let Some(view) = control.view() {
                trailing.addArrangedSubview(&view);
            }
            // The label takes the leading edge and the control the trailing one;
            // with `Fill` the spare width went to either, depending on the text.
            let line = kit::hstack(mtm, &[&head], 12.0);
            line.setDistribution(NSStackViewDistribution::GravityAreas);
            line.addView_inGravity(&trailing, objc2_app_kit::NSStackViewGravity::Trailing);
            kit::vstack_in(mtm, &[&line], 0.0, pad)
        };
        kit::name(&row, &format!("{}.row", setting.key), None);

        let container = kit::vstack(mtm, &[], 0.0);
        if !*first {
            kit::add_filled(&container, &kit::separator(mtm));
        }
        *first = false;
        kit::add_filled(&container, &row);

        if let Some(rows) = &nested_rows {
            kit::add_filled(&container, rows);
        }
        kit::add_filled(host, &container);
        self.settings.push(SettingView {
            key: setting.key.clone(),
            row: kit::view(&*container),
            label,
            description,
            status,
            control,
            parent: None,
            local_error: None,
            props_status: None,
            enabled: true,
            busy: false,
        });
        if let Some(rows) = nested_rows {
            let mut first_child = true;
            for child in &setting.settings {
                self.add_setting(mtm, env, page, child, &rows, &mut first_child, true);
            }
        }
    }

    /// Applies the page's current values and properties. Values the parent has
    /// not changed since the last call leave user drafts alone.
    pub(super) fn apply(&mut self, state: &PageState) {
        for setting in &mut self.settings {
            let Some(Element::Setting(definition)) = state.page.element(&setting.key) else {
                continue;
            };
            if let Some(options) = state.effective_options(&setting.key) {
                if setting.control.set_options(options) {
                    // The new choices start without a selection; the value applies again.
                    setting.parent = None;
                }
            }
            match state.values.get(&setting.key) {
                Some(value)
                    if setting
                        .parent
                        .as_ref()
                        .is_none_or(|last| model::replaces_draft(last, value)) =>
                {
                    setting.control.set_value(value);
                    setting.parent = Some(value.clone());
                    // A parent value supersedes a complaint about an earlier edit.
                    if setting.local_error.take().is_some() {
                        setting.show_status();
                    }
                }
                Some(_) => {}
                None => {
                    if setting.parent.take().is_some() {
                        setting.control.clear_value();
                    } else if setting.parent.is_none() && !setting.control.has_options() {
                        // Nothing set yet: show the declared default.
                        let default = model::parent_value(state, &setting.key, &definition.control);
                        if !default.is_null() {
                            setting.control.set_value(&default);
                        }
                    }
                }
            }
            setting.apply_props(state.props.get(&setting.key));
        }
        for banner in &self.banners {
            if let Some(props) = state.props.get(&banner.key) {
                banner.apply(props);
            }
        }
        for button in &self.buttons {
            if let Some(props) = state.props.get(&button.key) {
                control::apply_button_props(&button.button, props);
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

fn status_line(mtm: MainThreadMarker) -> Status {
    let icon = kit::symbol_view(mtm, None, None);
    kit::width(&icon, 14.0);
    let text = kit::caption(mtm, "");
    let stack = kit::hstack(mtm, &[&icon, &text], 5.0);
    stack.setHidden(true);
    Status { stack, icon, text }
}

fn banner_view(
    mtm: MainThreadMarker,
    banner: &parsec_orbit_ui_protocol::schema::UiBanner,
) -> BannerView {
    let icon = kit::symbol_view(mtm, None, None);
    kit::width(&icon, 22.0);
    let title = kit::label(mtm, banner.title.as_deref().unwrap_or(""));
    title.setFont(Some(&bold(13.0)));
    title.setHidden(banner.title.as_deref().is_none_or(str::is_empty));
    let message = kit::label(mtm, &banner.message);
    // Without a width to wrap at, a label reports a height that cuts its last line.
    title.setPreferredMaxLayoutWidth(640.0);
    message.setPreferredMaxLayoutWidth(640.0);
    let text = kit::vstack(mtm, &[&title, &message], 2.0);
    let content = kit::hstack(mtm, &[&icon, &text], 10.0);
    content.setAlignment(NSLayoutAttribute::Top);
    let surface = kit::surface(&content, mtm, 12.0, kit::insets(12.0, 14.0, 12.0, 14.0));
    kit::name(&surface, &banner.key, None);
    let view = BannerView {
        key: banner.key.clone(),
        surface,
        icon,
        title,
        message,
    };
    view.set_severity(banner.severity);
    view
}

impl BannerView {
    fn set_severity(&self, severity: UiSeverity) {
        let color = kit::severity_color(severity);
        self.surface
            .setFillColor(&color.colorWithAlphaComponent(0.14));
        self.icon
            .setImage(super::icons::image(symbol_for(severity)).as_deref());
        self.icon.setContentTintColor(Some(&color));
        self.icon.setHidden(false);
    }

    fn apply(&self, props: &UiProps) {
        if let Some(severity) = props.severity {
            self.set_severity(severity);
        }
        if let Some(title) = &props.title {
            self.title.setStringValue(&kit::string(title));
            self.title.setHidden(title.is_empty());
        }
        if let Some(message) = &props.message {
            self.message.setStringValue(&kit::string(message));
        }
        if let Some(visible) = props.visible {
            self.surface.setHidden(!visible);
        }
    }
}

impl SettingView {
    fn apply_props(&mut self, props: Option<&UiProps>) {
        let Some(props) = props else { return };
        if let Some(label) = &props.label {
            self.label.setStringValue(&kit::string(label));
            self.control.relabel(label);
        }
        if let Some(text) = &props.description {
            self.description.setStringValue(&kit::string(text));
            self.description.setHidden(text.is_empty());
        }
        if let Some(text) = &props.text {
            self.control.set_button_text(text);
        }
        if let Some(visible) = props.visible {
            self.row.setHidden(!visible);
        }
        if let Some(enabled) = props.enabled {
            self.enabled = enabled;
        }
        if let Some(busy) = props.busy {
            self.busy = busy;
            self.control.set_busy(busy);
        }
        if props.enabled.is_some() || props.busy.is_some() {
            self.control.set_enabled(self.enabled && !self.busy);
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
        let status = match (&self.local_error, &self.props_status) {
            (Some(reason), _) => Some((UiSeverity::Error, reason.as_str())),
            (None, Some((severity, text))) => Some((*severity, text.as_str())),
            (None, None) => None,
        };
        let line = &self.status;
        match status {
            Some((severity, text)) => {
                let color = kit::severity_color(severity);
                line.text.setStringValue(&kit::string(text));
                line.text.setTextColor(Some(&color));
                line.icon
                    .setImage(super::icons::image(symbol_for(severity)).as_deref());
                line.icon.setContentTintColor(Some(&color));
                line.icon.setHidden(false);
                line.stack.setHidden(false);
            }
            None => line.stack.setHidden(true),
        }
    }
}
