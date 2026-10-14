//! Window controller using winio.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use winio::elm::{init, start, update_children};
use winio::handle::AsWindow;
use winio::layout::layout;
use winio::prelude::*;

#[derive(Clone, Copy, Debug)]
pub enum OrbitMsg {
    Show,
    Hide,
    Toggle,
    Close,
    Resize,
}

pub struct OrbitUi {
    window: Child<Window>,
    tabs: Child<TabView>,
}

static UI_SENDER: Mutex<Option<ComponentSender<OrbitUi>>> = Mutex::new(None);
static IS_VISIBLE: AtomicBool = AtomicBool::new(false);

impl Component for OrbitUi {
    type Init<'a> = ();
    type Message = OrbitMsg;
    type Event = ();
    type Error = winio::Error;

    async fn init(_init: Self::Init<'_>, sender: &ComponentSender<Self>) -> winio::Result<Self> {
        init! {
            window: Window = (()) => {
                text: "Parsec Orbit",
                size: Size::new(880.0, 580.0),
            },
            tabs: TabView = (&window),
        }
        window.set_visible(false)?;

        *UI_SENDER.lock().unwrap() = Some(sender.clone());

        Ok(Self { window, tabs })
    }

    async fn start(&mut self, sender: &ComponentSender<Self>) -> ! {
        start! {
            sender, default: OrbitMsg::Resize,
            self.window => {
                WindowEvent::Close => OrbitMsg::Close,
                WindowEvent::Resize => OrbitMsg::Resize,
            },
        }
    }

    async fn update_children(&mut self) -> winio::Result<bool> {
        update_children!(self.window, self.tabs)
    }

    async fn update(
        &mut self,
        message: Self::Message,
        _: &ComponentSender<Self>,
    ) -> winio::Result<bool> {
        match message {
            OrbitMsg::Show => {
                self.window.set_visible(true)?;
                #[cfg(windows)]
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::SetForegroundWindow(
                        self.window.as_window().as_win32(),
                    );
                }
                IS_VISIBLE.store(true, Ordering::SeqCst);
                Ok(true)
            }
            OrbitMsg::Hide => {
                self.window.set_visible(false)?;
                IS_VISIBLE.store(false, Ordering::SeqCst);
                Ok(true)
            }
            OrbitMsg::Toggle => {
                let cur = IS_VISIBLE.load(Ordering::SeqCst);
                self.window.set_visible(!cur)?;
                if !cur {
                    #[cfg(windows)]
                    unsafe {
                        windows_sys::Win32::UI::WindowsAndMessaging::SetForegroundWindow(
                            self.window.as_window().as_win32(),
                        );
                    }
                }
                IS_VISIBLE.store(!cur, Ordering::SeqCst);
                Ok(true)
            }
            OrbitMsg::Close => {
                self.window.set_visible(false)?;
                IS_VISIBLE.store(false, Ordering::SeqCst);
                Ok(true)
            }
            OrbitMsg::Resize => Ok(true),
        }
    }

    fn render(&mut self, _: &ComponentSender<Self>) -> winio::Result<()> {
        let size = self.window.client_size()?;
        let mut grid = layout! {
            Grid::from_str("1*", "1*").unwrap(),
            self.tabs => { column: 0, row: 0 },
        };
        let _ = grid.set_size(size);
        Ok(())
    }

    fn render_children(&mut self) -> winio::Result<()> {
        self.window.render()?;
        self.tabs.render()?;
        Ok(())
    }
}

pub fn show() {
    ensure_started();
    if let Some(sender) = UI_SENDER.lock().unwrap().as_ref() {
        sender.post(OrbitMsg::Show);
    } else {
        IS_VISIBLE.store(true, Ordering::SeqCst);
    }
}

pub fn hide() {
    if let Some(sender) = UI_SENDER.lock().unwrap().as_ref() {
        sender.post(OrbitMsg::Hide);
    }
    IS_VISIBLE.store(false, Ordering::SeqCst);
}

pub fn toggle() {
    ensure_started();
    if let Some(sender) = UI_SENDER.lock().unwrap().as_ref() {
        sender.post(OrbitMsg::Toggle);
    } else {
        let cur = IS_VISIBLE.load(Ordering::SeqCst);
        IS_VISIBLE.store(!cur, Ordering::SeqCst);
    }
}

pub fn is_visible() -> bool {
    IS_VISIBLE.load(Ordering::SeqCst)
}

static STARTED: OnceLock<()> = OnceLock::new();

pub fn ensure_started() {
    STARTED.get_or_init(|| {
        std::thread::Builder::new()
            .name("parsec-orbit-ui".to_string())
            .spawn(|| {
                let app = match App::builder().name("Parsec Orbit").build() {
                    Ok(a) => a,
                    Err(_) => return,
                };
                let _ = app.block_on(OrbitUi::run_until_event(()));
            })
            .expect("spawn ui thread");
    });
}
