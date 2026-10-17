//! The window's chrome: a sidebar of pages beside the page being shown, and the
//! floating restart notice.
//!
//! The window's content is a system split view, so the sidebar gets the system's
//! own material (Liquid Glass on macOS 26) without this code drawing any.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, DeclaredClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSColor, NSControlTextEditingDelegate, NSImageView, NSLayoutYAxisAnchor, NSScrollView,
    NSSplitViewController, NSSplitViewItem, NSTableCellView, NSTableColumn, NSTableView,
    NSTableViewDataSource, NSTableViewDelegate, NSTableViewStyle, NSTextField, NSView,
    NSViewController, NSVisualEffectMaterial, NSVisualEffectView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSIndexSet, NSNotification, NSSize};
use parsec_orbit_ui_protocol::schema::UiIcon;

use super::kit;
use super::target::{ControlTarget, Env, Event};
use super::OrbitUiMessage;
use crate::model::{NavEntry, NavRow};

/// What a sidebar line stands for.
#[derive(Clone, Debug, PartialEq)]
enum Line {
    Page {
        id: String,
        title: String,
        icon: Option<UiIcon>,
    },
    Separator,
    Heading(&'static str),
}

define_class!(
    /// A top-down container: a scroll view's document starts at its top.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "OrbitFlippedView"]
    struct FlippedView;

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl FlippedView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(());
        // SAFETY: `NSView` initializes with `init`.
        unsafe { msg_send![super(this), init] }
    }
}

struct SourceIvars {
    env: Env,
    lines: RefCell<Vec<Line>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OrbitSidebarSource"]
    #[ivars = SourceIvars]
    struct SidebarSource;

    impl SidebarSource {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> isize {
            self.ivars().lines.borrow().len() as isize
        }

        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            _table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: isize,
        ) -> Option<Retained<NSView>> {
            self.cell(row)
        }

        #[unsafe(method(tableView:isGroupRow:))]
        fn is_group_row(&self, _table: &NSTableView, row: isize) -> bool {
            matches!(self.line(row), Some(Line::Heading(_)))
        }

        #[unsafe(method(tableView:shouldSelectRow:))]
        fn should_select_row(&self, _table: &NSTableView, row: isize) -> bool {
            matches!(self.line(row), Some(Line::Page { .. }))
        }

        #[unsafe(method(tableView:heightOfRow:))]
        fn height_of_row(&self, _table: &NSTableView, row: isize) -> f64 {
            match self.line(row) {
                Some(Line::Separator) => 12.0,
                Some(Line::Heading(_)) => 28.0,
                _ => 30.0,
            }
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_did_change(&self, notification: &NSNotification) {
            let ivars = self.ivars();
            if ivars.env.is_updating() {
                return;
            }
            let Some(object) = notification.object() else {
                return;
            };
            let Some(table) = object.downcast_ref::<NSTableView>() else {
                return;
            };
            if let Some(Line::Page { id, .. }) = self.line(table.selectedRow()) {
                ivars
                    .env
                    .sender()
                    .post(OrbitUiMessage::Selected(id));
            }
        }
    }

    unsafe impl NSObjectProtocol for SidebarSource {}
    unsafe impl NSControlTextEditingDelegate for SidebarSource {}
    unsafe impl NSTableViewDataSource for SidebarSource {}
    unsafe impl NSTableViewDelegate for SidebarSource {}
);

impl SidebarSource {
    fn new(mtm: MainThreadMarker, env: Env) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(SourceIvars {
            env,
            lines: RefCell::new(Vec::new()),
        });
        // SAFETY: `NSObject` initializes with `init`.
        unsafe { msg_send![super(this), init] }
    }

    fn cell(&self, row: isize) -> Option<Retained<NSView>> {
        let mtm = self.mtm();
        Some(match self.line(row)? {
            Line::Page { title, icon, .. } => page_cell(mtm, &title, icon),
            Line::Heading(text) => heading_cell(mtm, text),
            Line::Separator => {
                let line = kit::separator(mtm);
                line.setTranslatesAutoresizingMaskIntoConstraints(true);
                kit::view(&*line)
            }
        })
    }

    fn line(&self, row: isize) -> Option<Line> {
        self.ivars()
            .lines
            .borrow()
            .get(usize::try_from(row).ok()?)
            .cloned()
    }
}

fn page_cell(mtm: MainThreadMarker, title: &str, icon: Option<UiIcon>) -> Retained<NSView> {
    let cell = NSTableCellView::new(mtm);
    let image: Retained<NSImageView> = kit::symbol_view(
        mtm,
        icon.and_then(super::icons::image),
        Some(&NSColor::controlAccentColor()),
    );
    let text: Retained<NSTextField> = kit::line(mtm, title);
    text.setTranslatesAutoresizingMaskIntoConstraints(false);
    cell.addSubview(&image);
    cell.addSubview(&text);
    // SAFETY: both views are subviews of the cell.
    unsafe {
        cell.setImageView(Some(&image));
        cell.setTextField(Some(&text));
    }
    for constraint in [
        image
            .leadingAnchor()
            .constraintEqualToAnchor_constant(&cell.leadingAnchor(), 2.0),
        image
            .centerYAnchor()
            .constraintEqualToAnchor(&cell.centerYAnchor()),
        image.widthAnchor().constraintEqualToConstant(20.0),
        text.leadingAnchor()
            .constraintEqualToAnchor_constant(&image.trailingAnchor(), 8.0),
        text.trailingAnchor()
            .constraintLessThanOrEqualToAnchor(&cell.trailingAnchor()),
        text.centerYAnchor()
            .constraintEqualToAnchor(&cell.centerYAnchor()),
    ] {
        constraint.setActive(true);
    }
    kit::view(&*cell)
}

fn heading_cell(mtm: MainThreadMarker, title: &str) -> Retained<NSView> {
    let text = kit::line(mtm, title);
    text.setTextColor(Some(&NSColor::secondaryLabelColor()));
    kit::view(&*text)
}

/// A bar over the page for the restart notice: Liquid Glass where the system has
/// it, else a vibrancy material.
fn notice_surface(mtm: MainThreadMarker, content: &NSView) -> Retained<NSView> {
    let wrap = |surface: Retained<NSView>| {
        surface.setTranslatesAutoresizingMaskIntoConstraints(false);
        surface
    };
    if AnyClass::get(c"NSGlassEffectView").is_some() {
        let glass = objc2_app_kit::NSGlassEffectView::new(mtm);
        glass.setContentView(Some(content));
        glass.setCornerRadius(18.0);
        return wrap(kit::view(&*glass));
    }
    let effect = NSVisualEffectView::new(mtm);
    effect.setMaterial(NSVisualEffectMaterial::HUDWindow);
    effect.setBlendingMode(objc2_app_kit::NSVisualEffectBlendingMode::WithinWindow);
    effect.setState(objc2_app_kit::NSVisualEffectState::Active);
    effect.setWantsLayer(true);
    if let Some(layer) = effect.layer() {
        // SAFETY: `setCornerRadius:` takes a CGFloat.
        let _: () = unsafe { msg_send![&*layer, setCornerRadius: 12.0_f64] };
    }
    effect.addSubview(content);
    kit::pin(content, &effect);
    wrap(kit::view(&*effect))
}

pub(super) struct Shell {
    table: Retained<NSTableView>,
    source: Retained<SidebarSource>,
    scroll: Retained<NSScrollView>,
    document: Retained<FlippedView>,
    empty: Retained<NSTextField>,
    notice: Retained<NSView>,
    _restart: Retained<ControlTarget>,
}

impl Shell {
    /// Makes the shell the content of `window`.
    pub(super) fn install(mtm: MainThreadMarker, window: &NSWindow, env: &Env) -> Shell {
        let source = SidebarSource::new(mtm, env.clone());
        let table = NSTableView::new(mtm);
        table.setStyle(NSTableViewStyle::SourceList);
        table.setHeaderView(None);
        table.setAllowsEmptySelection(false);
        table.setFloatsGroupRows(false);
        let column = NSTableColumn::initWithIdentifier(mtm.alloc(), &kit::string("page"));
        table.addTableColumn(&column);
        // SAFETY: `SidebarSource` implements both protocols and outlives the
        // table: the shell owns it.
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(&*source)));
            table.setDelegate(Some(ProtocolObject::from_ref(&*source)));
        }
        let sidebar_scroll = NSScrollView::new(mtm);
        sidebar_scroll.setDocumentView(Some(&table));
        sidebar_scroll.setHasVerticalScroller(true);
        sidebar_scroll.setDrawsBackground(false);
        let sidebar_controller = NSViewController::new(mtm);
        sidebar_controller.setView(&sidebar_scroll);

        // The detail: the page scrolls under the toolbar, the notice floats over it.
        let scroll = NSScrollView::new(mtm);
        scroll.setHasVerticalScroller(true);
        scroll.setDrawsBackground(false);
        scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
        let document = FlippedView::new(mtm);
        document.setTranslatesAutoresizingMaskIntoConstraints(false);
        scroll.setDocumentView(Some(&document));
        let clip = scroll.contentView();
        document
            .leadingAnchor()
            .constraintEqualToAnchor(&clip.leadingAnchor())
            .setActive(true);
        document
            .trailingAnchor()
            .constraintEqualToAnchor(&clip.trailingAnchor())
            .setActive(true);
        let empty = kit::label(mtm, "Parsec Orbit has no pages to show yet.");
        empty.setTextColor(Some(&NSColor::secondaryLabelColor()));
        empty.setTranslatesAutoresizingMaskIntoConstraints(false);
        let detail = NSView::new(mtm);
        detail.addSubview(&scroll);
        detail.addSubview(&empty);
        kit::pin(&scroll, &detail);
        empty
            .centerXAnchor()
            .constraintEqualToAnchor(&detail.centerXAnchor())
            .setActive(true);
        empty
            .centerYAnchor()
            .constraintEqualToAnchor(&detail.centerYAnchor())
            .setActive(true);

        let restart = ControlTarget::new(mtm, {
            let env = env.clone();
            move |event| {
                if event == Event::Action {
                    env.sender().post(OrbitUiMessage::RestartAndApply);
                }
            }
        });
        let message = kit::line(mtm, "Restart Parsec to apply your changes.");
        // SAFETY: `fire:` is implemented by `ControlTarget`.
        let button = unsafe {
            objc2_app_kit::NSButton::buttonWithTitle_target_action(
                &kit::string("Restart and apply"),
                Some(&restart),
                Some(objc2::sel!(fire:)),
                mtm,
            )
        };
        kit::name(&button, "restart-and-apply", Some("Restart and apply"));
        let bar = kit::hstack(mtm, &[&message, &button], 12.0);
        bar.setEdgeInsets(kit::insets(10.0, 16.0, 10.0, 12.0));
        let notice = notice_surface(mtm, &bar);
        notice.setHidden(true);
        detail.addSubview(&notice);
        notice
            .centerXAnchor()
            .constraintEqualToAnchor(&detail.centerXAnchor())
            .setActive(true);
        notice
            .bottomAnchor()
            .constraintEqualToAnchor_constant(&detail.bottomAnchor(), -16.0)
            .setActive(true);
        let detail_controller = NSViewController::new(mtm);
        detail_controller.setView(&detail);

        let controller = NSSplitViewController::new(mtm);
        let sidebar_item = NSSplitViewItem::sidebarWithViewController(&sidebar_controller);
        sidebar_item.setMinimumThickness(200.0);
        sidebar_item.setMaximumThickness(280.0);
        controller.addSplitViewItem(&sidebar_item);
        controller.addSplitViewItem(&NSSplitViewItem::contentListWithViewController(
            &detail_controller,
        ));

        window.setStyleMask(window.styleMask() | NSWindowStyleMask::FullSizeContentView);
        window.setContentViewController(Some(&controller));
        // The page starts below the title bar and scrolls under it.
        if let Some(guide) = window.contentLayoutGuide() {
            // SAFETY: the window's content layout guide is a `NSLayoutGuide`.
            let top: Retained<NSLayoutYAxisAnchor> = unsafe { msg_send![&*guide, topAnchor] };
            document
                .topAnchor()
                .constraintEqualToAnchor(&top)
                .setActive(true);
        }
        window.setContentSize(NSSize::new(920.0, 640.0));
        window.setContentMinSize(NSSize::new(640.0, 420.0));

        Shell {
            table,
            source,
            scroll,
            document,
            empty,
            notice,
            _restart: restart,
        }
    }

    /// Lists the pages in the sidebar, keeping the selection if it is still there.
    pub(super) fn set_nav(&self, rows: &[NavRow<'_>], selected: Option<&str>) {
        let lines = rows
            .iter()
            .map(|row| match row {
                NavRow::Page(NavEntry {
                    id, title, icon, ..
                }) => Line::Page {
                    id: id.clone(),
                    title: title.clone(),
                    icon: *icon,
                },
                NavRow::Separator => Line::Separator,
                NavRow::Heading(text) => Line::Heading(text),
            })
            .collect();
        *self.source.ivars().lines.borrow_mut() = lines;
        self.table.reloadData();
        if let Some(selected) = selected {
            self.select(selected);
        }
    }

    /// Selects the sidebar line of a page without telling the component.
    pub(super) fn select(&self, id: &str) {
        let row = self
            .source
            .ivars()
            .lines
            .borrow()
            .iter()
            .position(|line| matches!(line, Line::Page { id: page, .. } if page == id));
        if let Some(row) = row {
            self.source.ivars().env.updating(|| {
                self.table.selectRowIndexes_byExtendingSelection(
                    &NSIndexSet::indexSetWithIndex(row),
                    false,
                );
            });
        }
    }

    /// Shows a page, or the empty state for `None`.
    pub(super) fn show(&self, page: Option<&NSView>) {
        self.empty.setHidden(page.is_some());
        self.scroll.setHidden(page.is_none());
        for old in self.document.subviews().iter() {
            old.removeFromSuperview();
        }
        if let Some(page) = page {
            self.document.addSubview(page);
            kit::pin_column(page, &self.document);
        }
    }

    pub(super) fn scroll_to_top(&self) {
        let clip = self.scroll.contentView();
        let mut origin = clip.bounds().origin;
        // The top of the page sits below the title bar: the scroll view's inset.
        origin.y = -self.scroll.contentInsets().top;
        clip.scrollToPoint(origin);
        self.scroll.reflectScrolledClipView(&clip);
    }

    pub(super) fn set_notice(&self, shown: bool) {
        self.notice.setHidden(!shown);
        // Leave room under the last row so the floating notice never covers it.
        let mut insets = self.scroll.contentInsets();
        insets.bottom = if shown { 76.0 } else { 0.0 };
        self.scroll.setContentInsets(insets);
    }
}
