//! The XAML templates of the Fluent shell.
//!
//! Every template is fixed text compiled into the companion. Strings a plugin
//! supplies (labels, descriptions, option names, values) are never put into
//! this markup: they are applied afterwards through property setters, so a
//! plugin cannot inject elements, bindings or resource references
//! ([ADR 0027](../../../../docs/design/decisions/0027-windows-fluent-xaml-shell.md)).
//! The only values interpolated are numbers (`u8` rows, `u32` heights) and
//! glyphs from the closed icon table.

use parsec_orbit_ui_protocol::schema::UiIcon;
use parsec_orbit_winui_bindings::{FrameworkElement, XamlReader};
use windows_core::{Interface, HSTRING};

use super::icons;

const NS: &str = r#"xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml""#;

const SECONDARY: &str = r#"Foreground="{ThemeResource TextFillColorSecondaryBrush}""#;

/// Parses a template into an element.
pub(super) fn load(xaml: &str) -> windows_core::Result<FrameworkElement> {
    XamlReader::Load(&HSTRING::from(xaml))?.cast()
}

/// The named part of a loaded template.
pub(super) fn find<T: Interface>(root: &FrameworkElement, name: &str) -> windows_core::Result<T> {
    root.FindName(&HSTRING::from(name))?.cast()
}

/// The window's content: the navigation pane, the restart notice and the page area.
pub(super) fn shell() -> String {
    format!(
        r#"<NavigationView {NS} x:Name="Nav" PaneDisplayMode="Left" IsSettingsVisible="False"
    IsBackButtonVisible="Collapsed" IsPaneToggleButtonVisible="False" OpenPaneLength="260"
    AlwaysShowHeader="False">
  <Grid x:Name="ContentGrid" RowDefinitions="Auto,*">
    <InfoBar x:Name="RestartNotice" Grid.Row="0" Severity="Warning" IsOpen="False"
        IsClosable="False" Margin="36,12,36,0" Title="Restart required"
        Message="Some changes apply after the Parsec client restarts.">
      <InfoBar.ActionButton>
        <Button x:Name="RestartButton" Content="Restart and apply"/>
      </InfoBar.ActionButton>
    </InfoBar>
    <ScrollViewer x:Name="PageScroll" Grid.Row="1" VerticalScrollBarVisibility="Auto"/>
  </Grid>
</NavigationView>"#
    )
}

/// What the page area shows before any page is registered.
pub(super) fn empty_state() -> String {
    format!(
        r#"<StackPanel {NS} Padding="36,48" Spacing="8" HorizontalAlignment="Center">
  <TextBlock Style="{{StaticResource SubtitleTextBlockStyle}}" Text="Nothing to configure yet"/>
  <TextBlock {SECONDARY} TextWrapping="Wrap" MaxWidth="420"
      Text="Settings appear here as Parsec Orbit and its plugins register them."/>
</StackPanel>"#
    )
}

pub(super) fn nav_item(icon: Option<UiIcon>) -> String {
    let icon = icon
        .map(|icon| {
            format!(
                "<NavigationViewItem.Icon>{}</NavigationViewItem.Icon>",
                icons::font_icon(icon, "")
            )
        })
        .unwrap_or_default();
    format!("<NavigationViewItem {NS}>{icon}</NavigationViewItem>")
}

pub(super) fn nav_header() -> String {
    format!(r#"<NavigationViewItemHeader {NS}/>"#)
}

pub(super) fn nav_separator() -> String {
    format!(r#"<NavigationViewItemSeparator {NS}/>"#)
}

/// A page: title, description and the stack the blocks go into.
pub(super) fn page() -> String {
    format!(
        r#"<StackPanel {NS} Spacing="6" MaxWidth="1000" Padding="36,8,36,36"
    HorizontalAlignment="Stretch">
  <TextBlock x:Name="Title" Style="{{StaticResource TitleTextBlockStyle}}"/>
  <TextBlock x:Name="Description" Style="{{StaticResource BodyTextBlockStyle}}" {SECONDARY}
      TextWrapping="Wrap" Margin="0,0,0,8" Visibility="Collapsed"/>
  <StackPanel x:Name="Blocks" Spacing="16"/>
</StackPanel>"#
    )
}

/// A titled run of settings.
pub(super) fn group() -> String {
    format!(
        r#"<StackPanel {NS} Spacing="4">
  <TextBlock x:Name="Title" Style="{{StaticResource BodyStrongTextBlockStyle}}"
      Margin="1,8,0,4" Visibility="Collapsed"/>
  <TextBlock x:Name="Description" Style="{{StaticResource CaptionTextBlockStyle}}" {SECONDARY}
      TextWrapping="Wrap" Margin="1,0,0,4" Visibility="Collapsed"/>
  <StackPanel x:Name="Items" Spacing="2"/>
</StackPanel>"#
    )
}

/// An inline notice.
pub(super) fn banner() -> String {
    format!(r#"<InfoBar {NS} x:Name="Bar" IsOpen="True" IsClosable="False"/>"#)
}

/// A row of command buttons.
pub(super) fn actions() -> String {
    format!(r#"<StackPanel {NS} Orientation="Horizontal" Spacing="8"/>"#)
}

/// How a command button looks.
#[derive(Clone, Copy)]
pub(super) enum ButtonLook {
    Standard,
    Accent,
    Destructive,
}

/// A command button with room for a busy ring, an icon and its text. In a card
/// the card's own ring serves, and the names stay unique within the template.
pub(super) fn button(look: ButtonLook, icon: Option<UiIcon>, in_card: bool) -> String {
    let (ring, text) = if in_card {
        (String::new(), "ButtonText")
    } else {
        (
            r#"<ProgressRing x:Name="Busy" Width="16" Height="16" IsActive="False" Visibility="Collapsed"/>"#
                .to_string(),
            "Text",
        )
    };
    let look = match look {
        ButtonLook::Standard => "",
        ButtonLook::Accent => r#"Style="{StaticResource AccentButtonStyle}""#,
        ButtonLook::Destructive => r#"Foreground="{ThemeResource SystemFillColorCriticalBrush}""#,
    };
    let icon = icon
        .map(|icon| icons::font_icon(icon, r#"FontSize="14""#))
        .unwrap_or_default();
    format!(
        r#"<Button {NS} x:Name="Control" MinWidth="96" {look}>
  <StackPanel Orientation="Horizontal" Spacing="8">
    {ring}
    {icon}
    <TextBlock x:Name="{text}"/>
  </StackPanel>
</Button>"#
    )
}

/// Where a setting's control sits in its card.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Placement {
    /// At the right of the label.
    Inline,
    /// Below the label, across the card.
    Block,
}

/// The XAML of the control itself; its main element is named `Control`.
pub(super) enum ControlTemplate {
    Toggle,
    Number,
    Slider,
    Select,
    TextLine,
    TextBlock {
        rows: u8,
        monospace: bool,
    },
    Json {
        rows: u8,
    },
    Value {
        monospace: bool,
    },
    Log {
        rows: u8,
    },
    /// A button on the setting's row.
    Button {
        look: ButtonLook,
        icon: Option<UiIcon>,
    },
}

impl ControlTemplate {
    pub(super) fn placement(&self) -> Placement {
        match self {
            ControlTemplate::TextBlock { .. }
            | ControlTemplate::Json { .. }
            | ControlTemplate::Log { .. } => Placement::Block,
            _ => Placement::Inline,
        }
    }

    fn xaml(&self) -> String {
        match self {
            ControlTemplate::Toggle => r#"<ToggleSwitch x:Name="Control" MinWidth="0"/>"#.into(),
            ControlTemplate::Number => format!(
                r#"<StackPanel Orientation="Horizontal" Spacing="8">
  <NumberBox x:Name="Control" Width="140" SpinButtonPlacementMode="Compact"/>
  <TextBlock x:Name="Unit" VerticalAlignment="Center" {SECONDARY} Visibility="Collapsed"/>
</StackPanel>"#
            ),
            ControlTemplate::Slider => format!(
                r#"<StackPanel Orientation="Horizontal" Spacing="12">
  <Slider x:Name="Control" Width="220"/>
  <TextBlock x:Name="Readout" MinWidth="64" VerticalAlignment="Center" TextAlignment="Right" {SECONDARY}/>
</StackPanel>"#
            ),
            ControlTemplate::Select => r#"<ContentControl x:Name="ComboHost"/>"#.into(),
            ControlTemplate::TextLine => r#"<TextBox x:Name="Control" Width="260"/>"#.into(),
            ControlTemplate::TextBlock { rows, monospace } => multiline(*rows, *monospace),
            ControlTemplate::Json { rows } => multiline(*rows, true),
            ControlTemplate::Value { monospace } => {
                let font = if *monospace {
                    r#"FontFamily="Consolas""#
                } else {
                    ""
                };
                format!(
                    r#"<TextBlock x:Name="Control" TextWrapping="Wrap" IsTextSelectionEnabled="True"
    MaxWidth="420" TextAlignment="Right" {SECONDARY} {font}/>"#
                )
            }
            ControlTemplate::Log { rows } => {
                let height = u32::from(*rows) * 18 + 16;
                format!(
                    r#"<ScrollViewer x:Name="LogScroll" Height="{height}" Padding="8"
    HorizontalScrollBarVisibility="Auto" VerticalScrollBarVisibility="Auto" CornerRadius="4"
    Background="{{ThemeResource LayerFillColorDefaultBrush}}">
  <TextBlock x:Name="Control" FontFamily="Consolas" FontSize="12" IsTextSelectionEnabled="True"/>
</ScrollViewer>"#
                )
            }
            ControlTemplate::Button { look, icon } => {
                // The button's own template, without the namespace declarations.
                button(*look, *icon, true).replacen(&format!(" {NS}"), "", 1)
            }
        }
    }
}

/// A combo box with one empty item per choice. Choices cannot be added to a
/// combo box built this way, so a change of choices replaces the whole element;
/// only the item count is part of the markup, the labels are set afterwards.
pub(super) fn combo(choices: usize) -> String {
    let items: String = (0..choices)
        .map(|index| format!(r#"<ComboBoxItem x:Name="Item{index}"/>"#))
        .collect();
    format!(r#"<ComboBox {NS} x:Name="Control" MinWidth="220">{items}</ComboBox>"#)
}

fn multiline(rows: u8, monospace: bool) -> String {
    let height = u32::from(rows) * 24 + 12;
    let font = if monospace {
        r#"FontFamily="Consolas""#
    } else {
        ""
    };
    format!(
        r#"<TextBox x:Name="Control" AcceptsReturn="True" TextWrapping="Wrap" MinHeight="{height}"
    HorizontalAlignment="Stretch" ScrollViewer.VerticalScrollBarVisibility="Auto" {font}/>"#
    )
}

/// One status line per severity, hidden until the setting has a status.
fn statuses() -> String {
    [
        ("Info", "{ThemeResource TextFillColorSecondaryBrush}"),
        ("Success", "{ThemeResource SystemFillColorSuccessBrush}"),
        ("Warning", "{ThemeResource SystemFillColorCautionBrush}"),
        ("Error", "{ThemeResource SystemFillColorCriticalBrush}"),
    ]
    .iter()
    .map(|(name, brush)| {
        format!(
            r#"<TextBlock x:Name="Status{name}" Style="{{StaticResource CaptionTextBlockStyle}}"
        Foreground="{brush}" TextWrapping="Wrap" Visibility="Collapsed"/>"#
        )
    })
    .collect::<Vec<_>>()
    .join("\n      ")
}

/// A setting: a card with the label at the left and the control at the right,
/// or below for the block controls. A setting with nested settings is an
/// expander whose header is that card.
pub(super) fn card(icon: Option<UiIcon>, control: &ControlTemplate, nested: bool) -> String {
    let icon = icon
        .map(|icon| {
            icons::font_icon(
                icon,
                r#"Grid.Column="0" FontSize="20" VerticalAlignment="Center" Margin="0,0,16,0""#,
            )
        })
        .unwrap_or_default();
    let (inline, block) = match control.placement() {
        Placement::Inline => (control.xaml(), String::new()),
        Placement::Block => (String::new(), control.xaml()),
    };
    let below = if block.is_empty() {
        String::new()
    } else {
        format!(
            r#"<ContentControl Grid.Row="1" Grid.ColumnSpan="3" Margin="0,12,0,0"
        HorizontalContentAlignment="Stretch">{block}</ContentControl>"#
        )
    };
    let grid = format!(
        r#"<Grid ColumnDefinitions="Auto,*,Auto" RowDefinitions="Auto,Auto" VerticalAlignment="Center">
    {icon}
    <StackPanel Grid.Column="1" VerticalAlignment="Center" Spacing="2" Margin="0,0,16,0">
      <TextBlock x:Name="Label" TextWrapping="Wrap"/>
      <TextBlock x:Name="Description" Style="{{StaticResource CaptionTextBlockStyle}}" {SECONDARY}
          TextWrapping="Wrap" Visibility="Collapsed"/>
      {statuses}
    </StackPanel>
    <StackPanel Grid.Column="2" Orientation="Horizontal" Spacing="8" VerticalAlignment="Center">
      <ProgressRing x:Name="Busy" Width="20" Height="20" IsActive="False" Visibility="Collapsed"/>
      {inline}
    </StackPanel>
    {below}
  </Grid>"#,
        statuses = statuses()
    );
    if nested {
        format!(
            r#"<Expander {NS} x:Name="Card" HorizontalAlignment="Stretch"
    HorizontalContentAlignment="Stretch" CornerRadius="4">
  <Expander.Header>
  {grid}
  </Expander.Header>
  <StackPanel x:Name="Nested" Spacing="2"/>
</Expander>"#
        )
    } else {
        format!(
            r#"<Border {NS} x:Name="Card" CornerRadius="4" Padding="16,12" MinHeight="56"
    BorderThickness="1" Background="{{ThemeResource CardBackgroundFillColorDefaultBrush}}"
    BorderBrush="{{ThemeResource CardStrokeColorDefaultBrush}}">
  {grid}
</Border>"#
        )
    }
}
