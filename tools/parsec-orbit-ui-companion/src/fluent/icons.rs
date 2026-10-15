//! Segoe Fluent Icons for the semantic icon names of the page schema.
//!
//! The names are a closed list ([UI service](../../../../docs/spec/ui-service.md#icons)),
//! so the glyphs below are the only ones that ever reach the XAML templates.

use parsec_orbit_ui_protocol::schema::UiIcon;

/// The code point of the glyph in Segoe Fluent Icons.
pub(super) fn glyph(icon: UiIcon) -> char {
    let code = match icon {
        UiIcon::Home => 0xE80F,
        UiIcon::Plugin => 0xEA86,
        UiIcon::Log => 0xE8A5,
        UiIcon::Network => 0xE774,
        UiIcon::Video => 0xE714,
        UiIcon::Audio => 0xE767,
        UiIcon::Input => 0xE765,
        UiIcon::Gamepad => 0xE7FC,
        UiIcon::Display => 0xE7F4,
        UiIcon::Speed => 0xEC4A,
        UiIcon::Security => 0xEA18,
        UiIcon::Server => 0xE753,
        UiIcon::Settings => 0xE713,
        UiIcon::Info => 0xE946,
        UiIcon::Warning => 0xE7BA,
        UiIcon::Error => 0xEA39,
        UiIcon::Success => 0xE73E,
        UiIcon::Refresh => 0xE72C,
        UiIcon::Folder => 0xE8B7,
        UiIcon::Copy => 0xE8C8,
        UiIcon::Play => 0xE768,
        UiIcon::Stop => 0xE71A,
        UiIcon::Restart => 0xE777,
        UiIcon::Download => 0xE896,
        UiIcon::Delete => 0xE74D,
        UiIcon::Edit => 0xE70F,
        UiIcon::Link => 0xE71B,
        UiIcon::Clock => 0xE121,
        UiIcon::Tools => 0xE90F,
    };
    char::from_u32(code).expect("the glyph table holds valid code points")
}

/// A `FontIcon` element for the glyph, as XAML text. The glyph comes from the
/// table above, never from a plugin.
pub(super) fn font_icon(icon: UiIcon, attributes: &str) -> String {
    format!(
        "<FontIcon Glyph=\"&#x{:X};\" {attributes}/>",
        glyph(icon) as u32
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_has_a_distinct_private_use_glyph() {
        use UiIcon::*;
        let all = [
            Home, Plugin, Log, Network, Video, Audio, Input, Gamepad, Display, Speed, Security,
            Server, Settings, Info, Warning, Error, Success, Refresh, Folder, Copy, Play, Stop,
            Restart, Download, Delete, Edit, Link, Clock, Tools,
        ];
        let mut seen = std::collections::HashSet::new();
        for icon in all {
            let code = glyph(icon) as u32;
            assert!(
                (0xE000..=0xF8FF).contains(&code),
                "{icon:?} is outside the private use area"
            );
            assert!(seen.insert(code), "{icon:?} repeats a glyph");
        }
        assert_eq!(all.len(), 29, "the schema's icon list changed");
    }

    #[test]
    fn a_font_icon_is_a_numeric_character_reference() {
        assert_eq!(
            font_icon(UiIcon::Home, "FontSize=\"16\""),
            "<FontIcon Glyph=\"&#xE80F;\" FontSize=\"16\"/>"
        );
    }
}
