//! Generates `crates/parsec-orbit-winui-bindings/src/bindings.rs`.
//!
//! winio's WinUI backend publishes the generic widgets and `winio-winui3` binds
//! only the controls those widgets need. The Fluent shell of the UI companion
//! ([ADR 0027](../../docs/design/decisions/0027-windows-fluent-xaml-shell.md))
//! also needs `NavigationView`, `ToggleSwitch`, `NumberBox`, `InfoBar` and their
//! events. They come from the Windows App SDK metadata that ships with the
//! Windows App Runtime. The result is committed; the build never reads the
//! runtime, and this tool runs only when [`TYPES`] or the SDK pin changes.
//!
//! ```text
//! parsec-orbit-winui-bindgen <Microsoft.WindowsAppRuntime package directory> [output]
//! ```
//!
//! The package directory is the install location of
//! `Microsoft.WindowsAppRuntime.<major.minor>` (`Get-AppxPackage` prints it); it
//! must hold `Microsoft.UI.Xaml.winmd` next to the other `Microsoft.*.winmd`
//! files. The output defaults to the committed file.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The metadata files the requested types and their dependencies live in.
const METADATA: &[&str] = &[
    "Microsoft.UI",
    "Microsoft.UI.Text",
    "Microsoft.UI.Xaml",
    "Microsoft.Foundation",
    "Microsoft.Graphics",
];

/// The types the companion uses, with the base classes their wrappers need.
///
/// Dependencies come along automatically; a type is listed only because the
/// renderer names it, or because a class wrapper needs its base class.
const TYPES: &[&str] = &[
    // The shell.
    "Microsoft.UI.Xaml.Controls.NavigationView",
    "Microsoft.UI.Xaml.Controls.NavigationViewItem",
    "Microsoft.UI.Xaml.Controls.NavigationViewItemBase",
    "Microsoft.UI.Xaml.Controls.NavigationViewItemHeader",
    "Microsoft.UI.Xaml.Controls.NavigationViewItemSeparator",
    "Microsoft.UI.Xaml.Controls.NavigationViewSelectionChangedEventArgs",
    // Controls of a setting.
    "Microsoft.UI.Xaml.Controls.ToggleSwitch",
    "Microsoft.UI.Xaml.Controls.NumberBox",
    "Microsoft.UI.Xaml.Controls.NumberBoxValueChangedEventArgs",
    "Microsoft.UI.Xaml.Controls.Slider",
    "Microsoft.UI.Xaml.Controls.Primitives.RangeBase",
    "Microsoft.UI.Xaml.Controls.Primitives.RangeBaseValueChangedEventHandler",
    "Microsoft.UI.Xaml.Controls.Primitives.RangeBaseValueChangedEventArgs",
    "Microsoft.UI.Xaml.Controls.TextBox",
    "Microsoft.UI.Xaml.Controls.TextChangedEventHandler",
    "Microsoft.UI.Xaml.Controls.TextChangedEventArgs",
    "Microsoft.UI.Xaml.Controls.ComboBox",
    "Microsoft.UI.Xaml.Controls.ComboBoxItem",
    "Microsoft.UI.Xaml.Controls.SelectionChangedEventHandler",
    "Microsoft.UI.Xaml.Controls.SelectionChangedEventArgs",
    "Microsoft.UI.Xaml.Controls.Button",
    "Microsoft.UI.Xaml.Controls.Primitives.ButtonBase",
    // Notices, progress and structure. Grids, borders and icons are only ever
    // created from the XAML templates, so they need no wrapper.
    "Microsoft.UI.Xaml.Controls.InfoBar",
    "Microsoft.UI.Xaml.Controls.ProgressRing",
    "Microsoft.UI.Xaml.Controls.Expander",
    "Microsoft.UI.Xaml.Controls.TextBlock",
    "Microsoft.UI.Xaml.Controls.StackPanel",
    "Microsoft.UI.Xaml.Controls.ScrollViewer",
    "Microsoft.UI.Xaml.Controls.InfoBarSeverity",
    "Microsoft.UI.Xaml.Controls.UIElementCollection",
    "Microsoft.UI.Xaml.Visibility",
    "Microsoft.UI.Xaml.Automation.AutomationProperties",
    // Events and loading.
    "Microsoft.UI.Xaml.RoutedEventHandler",
    "Microsoft.UI.Xaml.RoutedEventArgs",
    "Microsoft.UI.Xaml.Input.KeyEventHandler",
    "Microsoft.UI.Xaml.Input.KeyRoutedEventArgs",
    "Microsoft.UI.Xaml.Markup.XamlReader",
    // Bases of the class wrappers above.
    "Microsoft.UI.Xaml.DependencyObject",
    "Microsoft.UI.Xaml.UIElement",
    "Microsoft.UI.Xaml.FrameworkElement",
    "Microsoft.UI.Xaml.Controls.Control",
    "Microsoft.UI.Xaml.Controls.ContentControl",
    "Microsoft.UI.Xaml.Controls.ItemsControl",
    "Microsoft.UI.Xaml.Controls.IconElement",
    "Microsoft.UI.Xaml.Controls.Primitives.Selector",
    "Microsoft.UI.Xaml.Controls.Primitives.SelectorItem",
    "Microsoft.UI.Xaml.Controls.Panel",
];

/// The `bindgen` arguments for a metadata directory and an output file.
fn arguments(runtime: &Path, output: &Path) -> Vec<String> {
    let text = |path: &Path| path.to_string_lossy().into_owned();
    let mut args = vec!["--in".to_string(), "default".to_string()];
    args.extend(
        METADATA
            .iter()
            .map(|name| text(&runtime.join(format!("{name}.winmd")))),
    );
    args.extend([
        "--out".to_string(),
        text(output),
        // One file instead of a module per namespace: the crate is small.
        "--flat".to_string(),
        "--no-comment".to_string(),
        // Windows.* types come from the `windows` crate.
        "--reference".to_string(),
        "windows,skip-root,Windows".to_string(),
        "--filter".to_string(),
    ]);
    args.extend(TYPES.iter().map(|name| name.to_string()));
    args
}

/// Records where the bindings came from at the top of the file.
fn stamp(output: &Path, runtime: &Path) -> std::io::Result<()> {
    let package = runtime
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let body = std::fs::read_to_string(output)?;
    std::fs::write(
        output,
        format!("// @generated by tools/parsec-orbit-winui-bindgen from {package}; do not edit.\n{body}"),
    )
}

fn default_output() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/parsec-orbit-winui-bindings/src/bindings.rs")
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let Some(runtime) = args.next().map(PathBuf::from) else {
        eprintln!(
            "usage: parsec-orbit-winui-bindgen <Microsoft.WindowsAppRuntime directory> [output]"
        );
        return ExitCode::from(2);
    };
    let output = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(default_output);
    for name in METADATA {
        let file = runtime.join(format!("{name}.winmd"));
        if !file.is_file() {
            eprintln!("missing metadata file {}", file.display());
            return ExitCode::FAILURE;
        }
    }
    // `bindgen` reports the members it had to skip because their dependencies
    // were not requested. They are expected; only a missing type matters, and
    // the crate's own tests catch that.
    let warnings = windows_bindgen::bindgen(arguments(&runtime, &output));
    if let Err(error) = stamp(&output, &runtime) {
        eprintln!(
            "could not record the source in {}: {error}",
            output.display()
        );
        return ExitCode::FAILURE;
    }
    eprintln!("wrote {}\n{warnings:?}", output.display());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arguments_name_every_metadata_file_and_type_once() {
        let args = arguments(Path::new("R"), Path::new("out.rs"));
        for name in METADATA {
            assert!(
                args.iter().any(|a| a.ends_with(&format!("{name}.winmd"))),
                "{name}"
            );
        }
        let filter = args.iter().position(|a| a == "--filter").unwrap();
        assert_eq!(
            args[filter + 1..],
            TYPES.iter().map(|t| t.to_string()).collect::<Vec<_>>()
        );
        let mut sorted = TYPES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), TYPES.len(), "a type is listed twice");
    }

    /// The committed output holds a wrapper for everything the list asks for, so
    /// a type added to the list without regenerating is caught here.
    #[test]
    fn the_committed_bindings_hold_every_listed_type() {
        let bindings = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../crates/parsec-orbit-winui-bindings/src/bindings.rs"),
        )
        .expect("the generated bindings are committed");
        for name in TYPES {
            let short = name.rsplit('.').next().unwrap();
            // Classes and enums are structs; delegates are declared with a vtable.
            let declared = [
                format!("pub struct {short}("),
                format!("pub struct {short};"),
                format!("pub struct {short}_Vtbl {{"),
            ]
            .iter()
            .any(|pattern| bindings.contains(pattern));
            assert!(
                declared,
                "{name} is not in the committed bindings; regenerate"
            );
        }
    }
}
