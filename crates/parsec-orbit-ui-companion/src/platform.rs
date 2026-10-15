use winio::handle::AsWindow;
use winio::prelude::{Child, Window};

#[cfg(target_os = "macos")]
use winio::handle::AsWidget;
#[cfg(target_os = "macos")]
use winio::prelude::{ScrollView, Size, TabView};

pub enum WakeTarget {
    #[cfg(windows)]
    Windows(windows_sys::Win32::Foundation::HWND),
    #[cfg(target_os = "macos")]
    MacOs {
        run_loop: objc2_core_foundation::CFRetained<objc2_core_foundation::CFRunLoop>,
        source: objc2_core_foundation::CFRetained<objc2_core_foundation::CFRunLoopSource>,
    },
}

// SAFETY: worker threads only use these handles for documented native wake
// operations (PostMessageW or signaling/waking the CFRunLoop); the window
// itself stays owned and accessed by the main-thread component.
unsafe impl Send for WakeTarget {}

#[cfg(target_os = "macos")]
// Handling this source makes CFRunLoopRunInMode return so winio polls again.
unsafe extern "C-unwind" fn wake_source_performed(_: *mut std::ffi::c_void) {}

#[cfg(target_os = "macos")]
pub fn finish_launching() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
    use objc2_foundation::NSString;

    let main_thread = MainThreadMarker::new().expect("winio app is not on the main thread");
    let application = NSApplication::sharedApplication(main_thread);
    application.finishLaunching();

    let main_menu = application.mainMenu().unwrap_or_else(|| {
        let menu = NSMenu::new(main_thread);
        application.setMainMenu(Some(&menu));
        menu
    });
    let edit_title = NSString::from_str("Edit");
    if main_menu.itemWithTitle(&edit_title).is_some() {
        return;
    }

    let edit_menu = NSMenu::new(main_thread);
    edit_menu.setTitle(&edit_title);
    for (title, key, action) in [
        ("Cut", "x", objc2::sel!(cut:)),
        ("Copy", "c", objc2::sel!(copy:)),
        ("Paste", "v", objc2::sel!(paste:)),
    ] {
        // SAFETY: These are standard AppKit responder-chain actions.
        unsafe {
            edit_menu.addItemWithTitle_action_keyEquivalent(
                &NSString::from_str(title),
                Some(action),
                &NSString::from_str(key),
            );
        }
    }
    edit_menu.addItem(&NSMenuItem::separatorItem(main_thread));
    // SAFETY: `selectAll:` is a standard AppKit responder-chain action.
    unsafe {
        edit_menu.addItemWithTitle_action_keyEquivalent(
            &NSString::from_str("Select All"),
            Some(objc2::sel!(selectAll:)),
            &NSString::from_str("a"),
        );
    }

    let edit_item = NSMenuItem::new(main_thread);
    edit_item.setTitle(&edit_title);
    edit_item.setSubmenu(Some(&edit_menu));
    main_menu.addItem(&edit_item);
}

#[cfg(target_os = "macos")]
pub fn tab_content_size(tabs: &Child<TabView>) -> winio::Result<Size> {
    use objc2_app_kit::NSTabView;

    let view = tabs
        .as_widget()
        .as_app_kit()
        .downcast_ref::<NSTabView>()
        .expect("winio TabView has the expected AppKit class");
    let content = view.contentRect().size;
    Ok(Size::new(content.width, content.height))
}

#[cfg(target_os = "macos")]
pub fn scroll_to_top(scroll: &Child<ScrollView>) {
    use objc2_app_kit::NSScrollView;

    let view = scroll
        .as_widget()
        .as_app_kit()
        .downcast_ref::<NSScrollView>()
        .expect("winio ScrollView has the expected AppKit class");
    let clip = view.contentView();
    if let Some(document) = view.documentView() {
        let bounds = clip.bounds();
        let document_frame = document.frame();
        let mut origin = bounds.origin;
        origin.y = (document_frame.origin.y + document_frame.size.height - bounds.size.height)
            .max(document_frame.origin.y);
        clip.scrollToPoint(origin);
        view.reflectScrolledClipView(&clip);
    }
}

#[cfg(target_os = "macos")]
impl Drop for WakeTarget {
    fn drop(&mut self) {
        let Self::MacOs { run_loop, source } = self;
        run_loop.remove_source(Some(&**source), unsafe {
            objc2_core_foundation::kCFRunLoopDefaultMode
        });
    }
}

impl WakeTarget {
    pub fn for_window(window: &Child<Window>) -> Self {
        #[cfg(windows)]
        {
            Self::Windows(window.as_window().as_win32())
        }

        #[cfg(target_os = "macos")]
        {
            let _ = window;
            use objc2_core_foundation::{
                kCFAllocatorDefault, kCFRunLoopDefaultMode, CFRunLoop, CFRunLoopSource,
                CFRunLoopSourceContext,
            };

            let run_loop =
                CFRunLoop::current().expect("winio initialized without a main CFRunLoop");
            let mut source_context = CFRunLoopSourceContext {
                version: 0,
                info: std::ptr::null_mut(),
                retain: None,
                release: None,
                copyDescription: None,
                equal: None,
                hash: None,
                schedule: None,
                cancel: None,
                perform: Some(wake_source_performed),
            };
            // SAFETY: Core Foundation copies this initialized context. Its
            // callback carries no state and is valid for the source lifetime.
            let source =
                unsafe { CFRunLoopSource::new(kCFAllocatorDefault, 0, &mut source_context) }
                    .expect("could not create the companion run loop wake source");
            run_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
            Self::MacOs { run_loop, source }
        }
    }

    pub fn wake(&self) {
        match self {
            #[cfg(windows)]
            Self::Windows(hwnd) => unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_NULL};
                // SAFETY: the UI context retains this HWND only while the
                // window is alive, and PostMessageW is the cross-thread wake.
                PostMessageW(*hwnd, WM_NULL, 0, 0);
            },
            #[cfg(target_os = "macos")]
            Self::MacOs { run_loop, source } => {
                source.signal();
                run_loop.wake_up();
            }
        }
    }
}

pub fn activate(window: &Child<Window>) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SetForegroundWindow, ShowWindow, SW_RESTORE,
        };
        // SAFETY: the component calls this on the UI thread while it owns the
        // live HWND. Both calls operate on that window only.
        let hwnd = window.as_window().as_win32();
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
    }

    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::NSApplication;

        let main_thread = MainThreadMarker::new().expect("winio window is not on the main thread");
        #[allow(deprecated)]
        NSApplication::sharedApplication(main_thread).activateIgnoringOtherApps(true);
        let window = window.as_window().as_app_kit();
        if window.isMiniaturized() {
            window.deminiaturize(None);
        }
        window.makeKeyAndOrderFront(None);
    }
}
