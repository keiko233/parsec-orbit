use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use parsec_orbit_ui_protocol::{
    ChildMessage, ParentMessage, SectionState, UiAction, UiItem, UiOption,
};
use serde_json::Value;
use winio::prelude::*;

use crate::platform::{activate, WakeTarget};
use crate::{Inbound, Shared};

pub(crate) enum OrbitUiMessage {
    InputAvailable,
    UserClose,
    Action { panel: usize, row: usize },
    RestartAndApply,
    Refresh,
}

enum Control {
    Toggle(Child<CheckBox>),
    Slider(Child<Slider>),
    Select(Child<ComboBox>),
    Input(Child<Edit>),
    MultilineInput(Child<TextBox>),
    TextView(Child<TextBox>),
    Json(Child<TextBox>),
    Button(Child<Button>),
    Text(Child<Label>),
}

macro_rules! with_control {
    ($control:expr, $widget:ident, $body:expr) => {
        match $control {
            Control::Toggle($widget) => $body,
            Control::Slider($widget) => $body,
            Control::Select($widget) => $body,
            Control::Input($widget) => $body,
            Control::MultilineInput($widget) => $body,
            Control::TextView($widget) => $body,
            Control::Json($widget) => $body,
            Control::Button($widget) => $body,
            Control::Text($widget) => $body,
        }
    };
}

struct Row {
    item: UiItem,
    label: Child<Label>,
    control: Control,
    state_value: Value,
    value: Value,
}

impl Row {
    async fn new(parent: &Child<ScrollView>, item: UiItem, value: Value) -> winio::Result<Self> {
        let mut label = Child::<Label>::init(parent).await?;
        label.set_text(&item.label)?;
        let control = match item.item_type.as_str() {
            "toggle" => {
                let mut toggle = Child::<CheckBox>::init(parent).await?;
                toggle.set_text("")?;
                Control::Toggle(toggle)
            }
            "slider" => {
                let mut slider = Child::<Slider>::init(parent).await?;
                slider.set_minimum(item.min.unwrap_or(0.0).max(0.0) as usize)?;
                slider.set_maximum(item.max.unwrap_or(100.0).max(0.0) as usize)?;
                slider.set_freq(item.step.unwrap_or(1.0).max(1.0) as usize)?;
                Control::Slider(slider)
            }
            "select" => {
                let mut select = Child::<ComboBox>::init(parent).await?;
                select.set_editable(false)?;
                for option in item.options.as_deref().unwrap_or_default() {
                    select.push(&option.label)?;
                }
                Control::Select(select)
            }
            "text_input" if item.multiline == Some(true) => {
                Control::MultilineInput(Child::init(parent).await?)
            }
            "text_input" | "number_input" => Control::Input(Child::init(parent).await?),
            "text_view" => {
                let mut view = Child::<TextBox>::init(parent).await?;
                view.set_readonly(true)?;
                Control::TextView(view)
            }
            "json_editor" => Control::Json(Child::init(parent).await?),
            "button" => {
                let mut button = Child::<Button>::init(parent).await?;
                button.set_text(&item.label)?;
                Control::Button(button)
            }
            _ => Control::Text(Child::init(parent).await?),
        };
        let mut row = Self {
            item,
            label,
            control,
            state_value: Value::Null,
            value: Value::Null,
        };
        if let Some(description) = row.item.description.clone() {
            with_control!(&mut row.control, widget, widget.set_tooltip(description))?;
        }
        row.set_value(value)?;
        Ok(row)
    }

    /// Applies parent state without sending an action back to the parent.
    fn set_value(&mut self, value: Value) -> winio::Result<()> {
        match &mut self.control {
            Control::Toggle(widget) => widget.set_checked(value.as_bool().unwrap_or(false))?,
            Control::Slider(widget) => {
                widget.set_pos(value.as_f64().unwrap_or(0.0).max(0.0) as usize)?;
                self.label
                    .set_text(format!("{}: {}", self.item.label, widget.pos()?))?;
            }
            Control::Select(widget) => {
                let (selected, options) = select_state(&value);
                if let Some(options) = options {
                    widget.set_items(options.iter().map(|option| option.label.clone()))?;
                    self.item.options = Some(options);
                }
                let options = self.item.options.as_deref().unwrap_or_default();
                if let Some(index) = options.iter().position(|option| option.value == *selected) {
                    widget.set_selection(index)?;
                }
                self.value = selected.clone();
            }
            Control::Input(widget) => {
                let text = display_value(&value);
                if widget.text()? != text {
                    widget.set_text(text)?;
                }
            }
            Control::MultilineInput(widget) => {
                let text = display_value(&value);
                if widget.text()? != text {
                    widget.set_text(text)?;
                }
            }
            Control::TextView(widget) => {
                let text = display_value(&value);
                if widget.text()? != text {
                    widget.set_text(text)?;
                }
            }
            Control::Json(widget) => {
                widget.set_text(serde_json::to_string_pretty(&value).unwrap_or_default())?;
            }
            Control::Text(widget) => widget.set_text(display_value(&value))?,
            Control::Button(_) => {}
        }
        self.state_value = value;
        if !matches!(&self.control, Control::Select(_)) {
            self.value = self.state_value.clone();
        }
        Ok(())
    }

    fn edited_value(&self) -> winio::Result<Option<Value>> {
        Ok(match &self.control {
            Control::Toggle(widget) => Some(Value::Bool(widget.is_checked()?)),
            Control::Slider(widget) => Some(Value::from(widget.pos()?)),
            Control::Select(widget) => widget.selection()?.and_then(|index| {
                self.item
                    .options
                    .as_ref()?
                    .get(index)
                    .map(|option| option.value.clone())
            }),
            Control::Input(widget) if self.item.item_type == "number_input" => widget
                .text()?
                .parse::<f64>()
                .ok()
                .filter(|number| {
                    number.is_finite()
                        && self.item.min.is_none_or(|min| *number >= min)
                        && self.item.max.is_none_or(|max| *number <= max)
                })
                .map(Value::from),
            Control::Input(widget) => Some(Value::String(widget.text()?)),
            Control::MultilineInput(widget) => Some(Value::String(widget.text()?)),
            Control::TextView(_) => None,
            Control::Json(widget) => serde_json::from_str::<Value>(&widget.text()?)
                .ok()
                .filter(|value| value.is_object() || value.is_array()),
            Control::Button(_) => Some(Value::Null),
            Control::Text(_) => None,
        })
    }

    async fn listen(&mut self, sender: &ComponentSender<OrbitUi>, panel: usize, row: usize) {
        fn touched<E>(panel: usize, row: usize) -> impl FnMut(E) -> Option<OrbitUiMessage> {
            move |_| Some(OrbitUiMessage::Action { panel, row })
        }

        match &mut self.control {
            Control::Toggle(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitUiMessage::Refresh)
                    .await
            }
            Control::Slider(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitUiMessage::Refresh)
                    .await
            }
            Control::Select(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitUiMessage::Refresh)
                    .await
            }
            Control::Input(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitUiMessage::Refresh)
                    .await
            }
            Control::MultilineInput(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitUiMessage::Refresh)
                    .await
            }
            Control::TextView(_) => std::future::pending().await,
            Control::Json(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitUiMessage::Refresh)
                    .await
            }
            Control::Button(widget) => {
                widget
                    .start(sender, touched(panel, row), || OrbitUiMessage::Refresh)
                    .await
            }
            Control::Text(_) => std::future::pending().await,
        }
    }
}

fn select_state(value: &Value) -> (&Value, Option<Vec<UiOption>>) {
    if let (Some(selected), Some(options)) = (
        value.get("value"),
        value.get("options").and_then(Value::as_array),
    ) {
        if let Ok(options) = serde_json::from_value::<Vec<UiOption>>(Value::Array(options.clone()))
        {
            return (selected, Some(options));
        }
    }
    (value, None)
}

fn display_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

struct Panel {
    id: String,
    tab: Child<TabViewItem>,
    scroll: Child<ScrollView>,
    rows: Vec<Row>,
    #[cfg(target_os = "macos")]
    reset_scroll: bool,
}

pub(crate) struct OrbitUi {
    context: Arc<Shared>,
    window: Child<Window>,
    tabs: Child<TabView>,
    restart_button: Child<Button>,
    overview: Child<TabViewItem>,
    overview_text: Child<Label>,
    sections: Vec<SectionState>,
    panels: Vec<Panel>,
    visible: bool,
    restart_visible: bool,
    revision: u64,
}

impl OrbitUi {
    async fn rebuild(&mut self, sections: Vec<SectionState>) -> winio::Result<()> {
        self.tabs.clear()?;
        self.panels.clear();
        let has_overview = sections
            .iter()
            .any(|section| section.schema.id == "orbit.overview");
        self.restart_visible = has_overview;
        self.restart_button.set_visible(has_overview)?;
        self.overview_text.set_visible(!has_overview)?;
        if !has_overview {
            self.tabs.push(&self.overview)?;
            self.overview.set_text("Overview")?;
            self.overview_text.set_text(format!(
                "Parsec Orbit {}\n\n\
                 {} configuration section(s) registered by plugins.\n\n\
                 Closing this window hides it; plugins keep running.",
                env!("CARGO_PKG_VERSION"),
                sections.len()
            ))?;
        }
        for section in &sections {
            let mut tab = Child::<TabViewItem>::init(()).await?;
            tab.set_text(&section.schema.title)?;
            self.tabs.push(&tab)?;
            let mut scroll = Child::<ScrollView>::init(&tab).await?;
            scroll.set_vscroll(true)?;
            let mut rows = Vec::new();
            for item in &section.schema.items {
                if section.schema.id == "orbit.overview"
                    && item.item_type == "button"
                    && item.key == "restart"
                {
                    continue;
                }
                let value = section
                    .values
                    .get(&item.key)
                    .cloned()
                    .or_else(|| item.default.clone())
                    .unwrap_or(Value::Null);
                rows.push(Row::new(&scroll, item.clone(), value).await?);
            }
            self.panels.push(Panel {
                id: section.schema.id.clone(),
                tab,
                scroll,
                rows,
                #[cfg(target_os = "macos")]
                reset_scroll: true,
            });
        }
        self.tabs.set_selection(0)?;
        self.sections = sections;
        Ok(())
    }

    fn set_visible(&mut self, visible: bool) -> winio::Result<bool> {
        if self.visible == visible {
            return Ok(false);
        }
        self.window.set_visible(visible)?;
        self.visible = visible;
        if visible {
            activate(&self.window)?;
        }
        Ok(true)
    }

    fn request_visibility(&mut self, visible: bool, revision: u64) -> winio::Result<bool> {
        if revision < self.revision {
            return Ok(false);
        }
        self.revision = revision;
        if visible && self.visible {
            activate(&self.window)?;
            Ok(false)
        } else {
            self.set_visible(visible)
        }
    }

    async fn apply_snapshot(
        &mut self,
        sections: Vec<SectionState>,
        visible: bool,
        revision: u64,
    ) -> winio::Result<bool> {
        if revision < self.revision {
            return Ok(false);
        }
        self.revision = revision;
        let same_schema = self
            .sections
            .iter()
            .map(|section| &section.schema)
            .eq(sections.iter().map(|section| &section.schema));
        let mut changed = false;
        if same_schema {
            self.sections = sections;
            for (panel, section) in self.panels.iter_mut().zip(&self.sections) {
                for row in &mut panel.rows {
                    let value = section
                        .values
                        .get(&row.item.key)
                        .cloned()
                        .or_else(|| row.item.default.clone())
                        .unwrap_or(Value::Null);
                    if row.state_value != value {
                        row.set_value(value)?;
                        changed = true;
                    }
                }
            }
        } else {
            self.rebuild(sections).await?;
            changed = true;
        }
        changed |= self.set_visible(visible)?;
        Ok(changed)
    }

    fn apply_action(&mut self, panel_index: usize, row_index: usize) -> winio::Result<bool> {
        let Some(panel) = self.panels.get_mut(panel_index) else {
            return Ok(false);
        };
        let section_id = panel.id.clone();
        let Some(row) = panel.rows.get_mut(row_index) else {
            return Ok(false);
        };
        let clicked = row.item.item_type == "button";
        let Some(value) = row.edited_value()? else {
            return Ok(false);
        };
        if !clicked && row.value == value {
            // Native controls may report a change notification after a
            // programmatic setter. The cached value filters that event.
            return Ok(false);
        }

        let key = row.item.key.clone();
        // The native widget already contains the edit. Writing it back on
        // every keystroke reformats numbers ("7" -> "7.0") and moves the text
        // caret; only programmatic snapshots should replace editable text.
        if let Control::Slider(widget) = &row.control {
            row.label
                .set_text(format!("{}: {}", row.item.label, widget.pos()?))?;
        }
        row.value = value.clone();
        if let Some(section) = self.sections.get_mut(panel_index) {
            section.values.insert(key.clone(), value.clone());
        }
        self.context.write(ChildMessage::Action(UiAction {
            section: section_id,
            key,
            action_type: if clicked { "click" } else { "change" }.to_string(),
            value: Some(value),
        }));
        Ok(true)
    }
}

impl Component for OrbitUi {
    type Init<'a> = Arc<Shared>;
    type Message = OrbitUiMessage;
    type Event = ();
    type Error = winio::Error;

    async fn init(context: Self::Init<'_>, sender: &ComponentSender<Self>) -> winio::Result<Self> {
        let mut window = Child::<Window>::init(()).await?;
        window.set_text("Parsec Orbit")?;
        window.set_size(Size::new(880.0, 580.0))?;
        window.set_visible(false)?;
        let tabs = Child::<TabView>::init(&window).await?;
        let mut restart_button = Child::<Button>::init(&window).await?;
        restart_button.set_text("Restart and apply")?;
        restart_button.set_visible(false)?;
        let mut overview = Child::<TabViewItem>::init(()).await?;
        overview.set_text("Overview")?;
        let overview_text = Child::<Label>::init(&overview).await?;
        let mut ui = Self {
            context: Arc::clone(&context),
            window,
            tabs,
            restart_button,
            overview,
            overview_text,
            sections: Vec::new(),
            panels: Vec::new(),
            visible: false,
            restart_visible: false,
            revision: 0,
        };
        ui.rebuild(Vec::new()).await?;
        let wake = WakeTarget::for_window(&ui.window)?;
        context.connect(sender, wake);
        context.write(ChildMessage::Ready {
            version: parsec_orbit_ui_protocol::VERSION,
        });
        Ok(ui)
    }

    async fn start(&mut self, sender: &ComponentSender<Self>) -> ! {
        let Self {
            window,
            tabs,
            restart_button,
            panels,
            ..
        } = self;
        let mut listeners: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = Vec::new();
        listeners.push(Box::pin(async {
            window
                .start(
                    sender,
                    |event| {
                        Some(match event {
                            WindowEvent::Close => OrbitUiMessage::UserClose,
                            _ => OrbitUiMessage::Refresh,
                        })
                    },
                    || OrbitUiMessage::Refresh,
                )
                .await;
        }));
        listeners.push(Box::pin(async {
            tabs.start(
                sender,
                |_| Some(OrbitUiMessage::Refresh),
                || OrbitUiMessage::Refresh,
            )
            .await;
        }));
        listeners.push(Box::pin(async {
            restart_button
                .start(
                    sender,
                    |_| Some(OrbitUiMessage::RestartAndApply),
                    || OrbitUiMessage::Refresh,
                )
                .await;
        }));
        for (index, panel) in panels.iter_mut().enumerate() {
            let scroll = &mut panel.scroll;
            listeners.push(Box::pin(async {
                scroll
                    .start(sender, |_| None, || OrbitUiMessage::Refresh)
                    .await;
            }));
            for (row_index, row) in panel.rows.iter_mut().enumerate() {
                listeners.push(Box::pin(row.listen(sender, index, row_index)));
            }
        }
        poll_fn(|context| {
            for listener in &mut listeners {
                let _ = listener.as_mut().poll(context);
            }
            Poll::<()>::Pending
        })
        .await;
        unreachable!("control listeners do not complete")
    }

    async fn update(
        &mut self,
        message: OrbitUiMessage,
        sender: &ComponentSender<Self>,
    ) -> winio::Result<bool> {
        match message {
            OrbitUiMessage::InputAvailable => {
                let mut changed = false;
                for message in self.context.take_input() {
                    match message {
                        Inbound::Eof | Inbound::Frame(ParentMessage::Shutdown) => {
                            sender.output(());
                            return Ok(false);
                        }
                        Inbound::Frame(ParentMessage::Snapshot {
                            sections,
                            visible,
                            revision,
                        }) => {
                            changed |= self.apply_snapshot(sections, visible, revision).await?;
                        }
                        Inbound::Frame(ParentMessage::Visibility { visible, revision }) => {
                            changed |= self.request_visibility(visible, revision)?;
                        }
                    }
                }
                Ok(changed)
            }
            OrbitUiMessage::UserClose => {
                self.set_visible(false)?;
                self.context.write(ChildMessage::Visibility {
                    visible: false,
                    revision: self.revision,
                });
                Ok(true)
            }
            OrbitUiMessage::RestartAndApply => {
                self.context.write(ChildMessage::Action(UiAction {
                    section: "orbit.overview".to_string(),
                    key: "restart".to_string(),
                    action_type: "click".to_string(),
                    value: Some(Value::Null),
                }));
                Ok(true)
            }
            OrbitUiMessage::Action { panel, row } => self.apply_action(panel, row),
            OrbitUiMessage::Refresh => Ok(true),
        }
    }

    async fn update_children(&mut self) -> winio::Result<bool> {
        let mut changed = self.window.update().await?
            | self.tabs.update().await?
            | self.restart_button.update().await?;
        changed |= self.overview.update().await? | self.overview_text.update().await?;
        for panel in &mut self.panels {
            changed |= panel.tab.update().await? | panel.scroll.update().await?;
            for row in &mut panel.rows {
                changed |= row.label.update().await?;
                changed |= with_control!(&mut row.control, widget, widget.update().await)?;
            }
        }
        Ok(changed)
    }

    fn render(&mut self, _: &ComponentSender<Self>) -> winio::Result<()> {
        let size = self.window.client_size()?;
        self.tabs.set_loc(Point::new(0.0, 0.0))?;
        let footer_height = if self.restart_visible { 64.0 } else { 0.0 };
        self.tabs
            .set_size(Size::new(size.width, size.height - footer_height))?;
        self.restart_button.set_loc(Point::new(
            (size.width - 200.0).max(24.0),
            (size.height - 48.0).max(8.0),
        ))?;
        self.restart_button.set_size(Size::new(176.0, 32.0))?;
        self.overview_text.set_loc(Point::new(24.0, 24.0))?;
        self.overview_text
            .set_size(Size::new((size.width - 64.0).max(100.0), 260.0))?;
        for panel in &mut self.panels {
            #[cfg(target_os = "macos")]
            let size = crate::platform::tab_content_size(&self.tabs)?;
            #[cfg(windows)]
            let size = panel.tab.size()?;
            panel.scroll.set_loc(Point::new(0.0, 0.0))?;
            let width = (size.width - 64.0).max(100.0);
            let mut y = 20.0;
            for row in &mut panel.rows {
                row.label.set_loc(Point::new(24.0, y))?;
                row.label.set_size(Size::new(width, 24.0))?;
                let height = if matches!(
                    row.control,
                    Control::Json(_) | Control::MultilineInput(_) | Control::TextView(_)
                ) {
                    120.0
                } else {
                    32.0
                };
                with_control!(
                    &mut row.control,
                    widget,
                    widget.set_loc(Point::new(24.0, y + 28.0))
                )?;
                with_control!(
                    &mut row.control,
                    widget,
                    widget.set_size(Size::new(width, height))
                )?;
                y += height + 48.0;
            }
            // AppKit sizes the document view from the child frames, so lay out
            // every row before asking the scroll view to recompute its bounds.
            panel.scroll.set_size(size)?;
            #[cfg(target_os = "macos")]
            if panel.reset_scroll {
                crate::platform::scroll_to_top(&panel.scroll);
                panel.reset_scroll = false;
            }
        }
        Ok(())
    }

    fn render_children(&mut self) -> winio::Result<()> {
        self.window.render()?;
        self.tabs.render()?;
        self.restart_button.render()?;
        self.overview.render()?;
        self.overview_text.render()?;
        for panel in &mut self.panels {
            panel.tab.render()?;
            panel.scroll.render()?;
            for row in &mut panel.rows {
                row.label.render()?;
                with_control!(&mut row.control, widget, widget.render())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_state_carries_new_options_and_the_selected_value() {
        let state = serde_json::json!({
            "value": "new",
            "options": [
                {"label": "New", "value": "new"},
                {"label": "Existing", "value": "existing"}
            ]
        });

        let (value, options) = select_state(&state);
        assert_eq!(value, "new");
        assert_eq!(
            options.unwrap(),
            vec![
                UiOption {
                    label: "New".into(),
                    value: Value::String("new".into()),
                },
                UiOption {
                    label: "Existing".into(),
                    value: Value::String("existing".into()),
                },
            ]
        );
    }

    #[test]
    fn ordinary_select_state_keeps_its_scalar_value_and_existing_options() {
        let value = serde_json::json!(42);
        let (selected, options) = select_state(&value);
        assert_eq!(selected, 42);
        assert_eq!(options, None);
    }
}
