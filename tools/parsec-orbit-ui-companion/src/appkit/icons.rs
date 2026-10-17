//! SF Symbols for the semantic icon names of the page schema.
//!
//! The names are a closed list ([UI service](../../../../docs/spec/ui-service.md#icons)),
//! so the symbol names below are the only ones that ever reach AppKit. A symbol
//! this macOS does not have is drawn as no icon, never as an error.

use objc2::rc::Retained;
use objc2_app_kit::NSImage;
use objc2_foundation::NSString;
use parsec_orbit_ui_protocol::schema::UiIcon;

/// The SF Symbol name of an icon.
pub(super) fn symbol(icon: UiIcon) -> &'static str {
    match icon {
        UiIcon::Home => "house",
        UiIcon::Plugin => "puzzlepiece.extension",
        UiIcon::Log => "doc.text",
        UiIcon::Network => "network",
        UiIcon::Video => "video",
        UiIcon::Audio => "speaker.wave.2",
        UiIcon::Input => "keyboard",
        UiIcon::Gamepad => "gamecontroller",
        UiIcon::Display => "display",
        UiIcon::Speed => "speedometer",
        UiIcon::Security => "lock.shield",
        UiIcon::Server => "server.rack",
        UiIcon::Settings => "gearshape",
        UiIcon::Info => "info.circle",
        UiIcon::Warning => "exclamationmark.triangle",
        UiIcon::Error => "xmark.octagon",
        UiIcon::Success => "checkmark.circle",
        UiIcon::Refresh => "arrow.clockwise",
        UiIcon::Folder => "folder",
        UiIcon::Copy => "doc.on.doc",
        UiIcon::Play => "play.fill",
        UiIcon::Stop => "stop.fill",
        UiIcon::Restart => "arrow.triangle.2.circlepath",
        UiIcon::Download => "arrow.down.circle",
        UiIcon::Delete => "trash",
        UiIcon::Edit => "pencil",
        UiIcon::Link => "link",
        UiIcon::Clock => "clock",
        UiIcon::Tools => "wrench.and.screwdriver",
    }
}

/// The symbol as an image, or `None` when this system has no such symbol.
pub(super) fn image(icon: UiIcon) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol(icon)),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_has_a_distinct_symbol() {
        use UiIcon::*;
        let all = [
            Home, Plugin, Log, Network, Video, Audio, Input, Gamepad, Display, Speed, Security,
            Server, Settings, Info, Warning, Error, Success, Refresh, Folder, Copy, Play, Stop,
            Restart, Download, Delete, Edit, Link, Clock, Tools,
        ];
        let mut seen = std::collections::HashSet::new();
        for icon in all {
            assert!(seen.insert(symbol(icon)), "{icon:?} repeats a symbol");
        }
        assert_eq!(all.len(), 29, "the schema's icon list changed");
    }
}
