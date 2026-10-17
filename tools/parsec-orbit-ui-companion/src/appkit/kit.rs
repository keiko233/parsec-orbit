//! Small AppKit builders shared by the shell and the pages.

use objc2::rc::Retained;
use objc2::{MainThreadMarker, Message};
use objc2_app_kit::{
    NSAccessibility, NSBox, NSBoxType, NSColor, NSFont, NSImage, NSImageView, NSLayoutAttribute,
    NSStackView, NSTextField, NSUserInterfaceItemIdentification, NSUserInterfaceLayoutOrientation,
    NSView,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSSize, NSString};
use parsec_orbit_ui_protocol::schema::UiSeverity;

pub(super) fn string(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

/// A selectable, wrapping, read-only line of text.
pub(super) fn label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    NSTextField::wrappingLabelWithString(&string(text), mtm)
}

/// Small secondary text, such as a setting's description.
pub(super) fn caption(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let field = label(mtm, text);
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    field.setFont(Some(&NSFont::systemFontOfSize(
        NSFont::smallSystemFontSize(),
    )));
    // A wrapping label beside a wide control would otherwise shrink to one letter.
    field.setPreferredMaxLayoutWidth(420.0);
    field
}

/// A single line that never wraps, such as a value readout.
pub(super) fn line(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    NSTextField::labelWithString(&string(text), mtm)
}

pub(super) fn monospaced(size: f64) -> Retained<NSFont> {
    NSFont::monospacedSystemFontOfSize_weight(size, unsafe { objc2_app_kit::NSFontWeightRegular })
}

/// A retained handle to any view, whatever its class.
pub(super) fn view<T: AsRef<NSView> + Message>(any: &T) -> Retained<NSView> {
    let view: &NSView = any.as_ref();
    view.retain()
}

pub(super) fn views(items: &[&NSView]) -> Retained<NSArray<NSView>> {
    NSArray::from_slice(items)
}

fn stack(
    mtm: MainThreadMarker,
    orientation: NSUserInterfaceLayoutOrientation,
    items: &[&NSView],
    spacing: f64,
    alignment: NSLayoutAttribute,
) -> Retained<NSStackView> {
    let stack = NSStackView::stackViewWithViews(&views(items), mtm);
    stack.setOrientation(orientation);
    stack.setSpacing(spacing);
    stack.setAlignment(alignment);
    stack.setTranslatesAutoresizingMaskIntoConstraints(false);
    stack
}

/// Views side by side, centered on the same line.
pub(super) fn hstack(
    mtm: MainThreadMarker,
    items: &[&NSView],
    spacing: f64,
) -> Retained<NSStackView> {
    stack(
        mtm,
        NSUserInterfaceLayoutOrientation::Horizontal,
        items,
        spacing,
        NSLayoutAttribute::CenterY,
    )
}

/// Views one under the other, each as wide as the stack.
///
/// A vertical `NSStackView` has no "fill" alignment, so each view gets a width
/// constraint instead.
pub(super) fn vstack(
    mtm: MainThreadMarker,
    items: &[&NSView],
    spacing: f64,
) -> Retained<NSStackView> {
    vstack_in(mtm, items, spacing, insets(0.0, 0.0, 0.0, 0.0))
}

/// [`vstack`] with a margin around the views.
pub(super) fn vstack_in(
    mtm: MainThreadMarker,
    items: &[&NSView],
    spacing: f64,
    margin: NSEdgeInsets,
) -> Retained<NSStackView> {
    let stack = stack(
        mtm,
        NSUserInterfaceLayoutOrientation::Vertical,
        &[],
        spacing,
        NSLayoutAttribute::Leading,
    );
    stack.setEdgeInsets(margin);
    for item in items {
        add_filled(&stack, item);
    }
    stack
}

/// Appends `view` to a [`vstack`], as wide as the stack inside its margin.
pub(super) fn add_filled(stack: &NSStackView, view: &NSView) {
    stack.addArrangedSubview(view);
    let margin = stack.edgeInsets();
    view.widthAnchor()
        .constraintEqualToAnchor_constant(&stack.widthAnchor(), -(margin.left + margin.right))
        .setActive(true);
}

pub(super) fn insets(top: f64, left: f64, bottom: f64, right: f64) -> NSEdgeInsets {
    NSEdgeInsets {
        top,
        left,
        bottom,
        right,
    }
}

/// A thin divider line.
pub(super) fn separator(mtm: MainThreadMarker) -> Retained<NSBox> {
    let line = NSBox::new(mtm);
    line.setBoxType(NSBoxType::Separator);
    line.setTranslatesAutoresizingMaskIntoConstraints(false);
    line
}

/// Pins `view` to the four edges of `parent`.
/// A filled, rounded surface that is as large as `content` needs.
pub(super) fn surface(
    content: &NSView,
    mtm: MainThreadMarker,
    radius: f64,
    padding: NSEdgeInsets,
) -> Retained<NSBox> {
    let surface = NSBox::new(mtm);
    surface.setBoxType(NSBoxType::Custom);
    surface.setBorderWidth(0.0);
    surface.setCornerRadius(radius);
    surface.setContentViewMargins(NSSize::new(0.0, 0.0));
    surface.setTranslatesAutoresizingMaskIntoConstraints(false);
    if let Some(inner) = surface.contentView() {
        inner.addSubview(content);
        // Stack edge insets lose their bottom edge under top alignment, so pad here.
        content.setTranslatesAutoresizingMaskIntoConstraints(false);
        for constraint in [
            content
                .topAnchor()
                .constraintEqualToAnchor_constant(&inner.topAnchor(), padding.top),
            content
                .bottomAnchor()
                .constraintEqualToAnchor_constant(&inner.bottomAnchor(), -padding.bottom),
            content
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&inner.leadingAnchor(), padding.left),
            content
                .trailingAnchor()
                .constraintEqualToAnchor_constant(&inner.trailingAnchor(), -padding.right),
        ] {
            constraint.setActive(true);
        }
    }
    surface
}

pub(super) fn pin(view: &NSView, parent: &NSView) {
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    for constraint in [
        view.topAnchor()
            .constraintEqualToAnchor(&parent.topAnchor()),
        view.bottomAnchor()
            .constraintEqualToAnchor(&parent.bottomAnchor()),
        view.leadingAnchor()
            .constraintEqualToAnchor(&parent.leadingAnchor()),
        view.trailingAnchor()
            .constraintEqualToAnchor(&parent.trailingAnchor()),
    ] {
        constraint.setActive(true);
    }
}

pub(super) fn width(view: &NSView, points: f64) {
    view.widthAnchor()
        .constraintEqualToConstant(points)
        .setActive(true);
}

/// A symbol image view in the secondary label color, or `None` without an image.
pub(super) fn symbol_view(
    mtm: MainThreadMarker,
    image: Option<Retained<NSImage>>,
    tint: Option<&NSColor>,
) -> Retained<NSImageView> {
    let view = match &image {
        Some(image) => NSImageView::imageViewWithImage(image, mtm),
        None => NSImageView::new(mtm),
    };
    view.setContentTintColor(Some(tint.unwrap_or(&NSColor::secondaryLabelColor())));
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.setHidden(image.is_none());
    view
}

/// The system color of a severity.
pub(super) fn severity_color(severity: UiSeverity) -> Retained<NSColor> {
    match severity {
        UiSeverity::Info => NSColor::systemBlueColor(),
        UiSeverity::Success => NSColor::systemGreenColor(),
        UiSeverity::Warning => NSColor::systemOrangeColor(),
        UiSeverity::Error => NSColor::systemRedColor(),
    }
}

/// The accessibility identity: the key as the identifier, and the label as the
/// name unless the element shows text of its own, which then stays its name.
pub(super) fn name(element: &NSView, id: &str, label: Option<&str>) {
    element.setIdentifier(Some(&string(id)));
    element.setAccessibilityIdentifier(Some(&string(id)));
    if let Some(label) = label {
        element.setAccessibilityLabel(Some(&string(label)));
    }
}

/// Centers a page's column in the scroll view's clip view, as wide as the clip
/// view up to a comfortable measure.
pub(super) fn pin_column(column: &NSView, clip: &NSView) {
    column.setTranslatesAutoresizingMaskIntoConstraints(false);
    column
        .topAnchor()
        .constraintEqualToAnchor(&clip.topAnchor())
        .setActive(true);
    column
        .bottomAnchor()
        .constraintEqualToAnchor(&clip.bottomAnchor())
        .setActive(true);
    column
        .centerXAnchor()
        .constraintEqualToAnchor(&clip.centerXAnchor())
        .setActive(true);
    column
        .widthAnchor()
        .constraintLessThanOrEqualToConstant(760.0)
        .setActive(true);
    let fill = column
        .widthAnchor()
        .constraintEqualToAnchor(&clip.widthAnchor());
    fill.setPriority(750.0);
    fill.setActive(true);
}
