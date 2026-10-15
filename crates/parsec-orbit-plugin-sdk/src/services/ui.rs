//! Typed calls on `parsec-orbit.ui`, version 2: pages of settings in the
//! companion window. See `docs/spec/ui-service.md`.

use core::ffi::{c_void, CStr};

use super::{code, id};
use crate::abi::{MessageCallback, ParsecOrbitInterfaceHeader, ParsecOrbitUiV2, ERR_NOT_READY};
use crate::context::{Context, Error, Interface, Result};

/// Binding of `parsec-orbit.ui`, version 2.
pub struct Ui;

impl Interface for Ui {
    const NAME: &'static CStr = c"parsec-orbit.ui";
    const MAJOR: u16 = 2;
    type Table = ParsecOrbitUiV2;

    fn header(table: &ParsecOrbitUiV2) -> &ParsecOrbitInterfaceHeader {
        &table.header
    }
}

/// Safe calls on `parsec-orbit.ui`.
#[derive(Clone, Copy)]
pub struct UiClient {
    pub(crate) table: &'static ParsecOrbitUiV2,
}

impl UiClient {
    /// The service, if the UI plugin is loaded and speaks version 2.
    pub fn new(cx: &Context) -> Option<UiClient> {
        cx.service::<Ui>().map(|table| UiClient { table })
    }

    /// Builds a client directly from a table (for testing or mocks).
    pub const fn from_table(table: &'static ParsecOrbitUiV2) -> UiClient {
        UiClient { table }
    }

    /// Shows and focuses the companion window.
    pub fn show(&self) -> Result {
        let show = self.table.show.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { show() })
    }

    /// Hides the companion window without stopping the event loop.
    pub fn hide(&self) -> Result {
        let hide = self.table.hide.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { hide() })
    }

    /// Returns `true` if the window is currently visible.
    pub fn is_visible(&self) -> Result<bool> {
        let is_visible = self.table.is_visible.ok_or(Error::new(ERR_NOT_READY))?;
        let res = unsafe { is_visible() };
        if res < 0 {
            Err(Error::new(res))
        } else {
            Ok(res != 0)
        }
    }

    /// Selects a registered page in the window.
    pub fn navigate(&self, page_id: &str) -> Result {
        let navigate = self.table.navigate.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { navigate(page_id.as_ptr(), page_id.len()) })
    }

    /// Registers a declarative page (JSON text).
    pub fn register_page(&self, page_json: &str) -> Result {
        let register = self.table.register_page.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { register(page_json.as_ptr(), page_json.len()) })
    }

    /// Unregisters a page by its ID.
    pub fn unregister_page(&self, page_id: &str) -> Result {
        let unregister = self
            .table
            .unregister_page
            .ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { unregister(page_id.as_ptr(), page_id.len()) })
    }

    /// Applies a state patch (JSON object with `values`, `props` and
    /// `restart_required`) to a page.
    pub fn update_page(&self, page_id: &str, patch_json: &str) -> Result {
        let update = self.table.update_page.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe {
            update(
                page_id.as_ptr(),
                page_id.len(),
                patch_json.as_ptr(),
                patch_json.len(),
            )
        })
    }

    /// Subscribes to user interactions within the specified page.
    ///
    /// # Safety
    /// `callback` may be called from the UI delivery thread with `user`. Both must
    /// remain valid until [`UiClient::off_action`].
    pub unsafe fn on_action(
        &self,
        page_id: &str,
        callback: MessageCallback,
        user: *mut c_void,
    ) -> Result<i64> {
        let on = self.table.on_action.ok_or(Error::new(ERR_NOT_READY))?;
        id(unsafe { on(page_id.as_ptr(), page_id.len(), Some(callback), user) })
    }

    /// Cancels an action subscription.
    pub fn off_action(&self, subscription: i64) -> Result {
        let off = self.table.off_action.ok_or(Error::new(ERR_NOT_READY))?;
        code(unsafe { off(subscription) })
    }
}
