//! The one Objective-C class the renderer defines for controls: an action and
//! notification target that calls a Rust closure.
//!
//! A control's callback only reads the control and posts an [`OrbitUiMessage`];
//! nothing runs plugin or pipe work inside an AppKit callback.

use std::cell::Cell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, DeclaredClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSString};
use winio::prelude::ComponentSender;

use super::{OrbitUi, OrbitUiMessage};
use serde_json::Value;

/// What the controls share: where their edits go, and whether a change is the
/// renderer's own (applying parent state) rather than the user's.
#[derive(Clone)]
pub(super) struct Env {
    sender: ComponentSender<OrbitUi>,
    updating: Rc<Cell<bool>>,
}

impl Env {
    pub(super) fn new(sender: ComponentSender<OrbitUi>) -> Self {
        Self {
            sender,
            updating: Rc::new(Cell::new(false)),
        }
    }

    /// Whether the renderer itself is changing controls right now.
    pub(super) fn is_updating(&self) -> bool {
        self.updating.get()
    }

    /// Runs `apply` with user-edit notifications suppressed.
    pub(super) fn updating<T>(&self, apply: impl FnOnce() -> T) -> T {
        let before = self.updating.replace(true);
        let result = apply();
        self.updating.set(before);
        result
    }

    pub(super) fn sender(&self) -> &ComponentSender<OrbitUi> {
        &self.sender
    }

    /// A control reported a value (`Some`) or a click (`None`).
    pub(super) fn edited(&self, page: &str, key: &str, value: Option<Value>) {
        if !self.is_updating() {
            self.sender.post(OrbitUiMessage::Edited {
                page: page.to_owned(),
                key: key.to_owned(),
                value,
            });
        }
    }

    /// A control holds input that cannot be a value.
    pub(super) fn invalid(&self, page: &str, key: &str, reason: &str) {
        if !self.is_updating() {
            self.sender.post(OrbitUiMessage::Invalid {
                page: page.to_owned(),
                key: key.to_owned(),
                reason: reason.to_owned(),
            });
        }
    }
}

/// How a control called its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Event {
    /// The control's action: a click, a toggle, a drag, a choice.
    Action,
    /// The text of an editor changed.
    Changed,
    /// An editor ended editing: Return, Tab or focus loss.
    Ended,
}

pub(super) struct TargetIvars {
    handler: Box<dyn Fn(Event)>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OrbitControlTarget"]
    #[ivars = TargetIvars]
    pub(super) struct ControlTarget;

    impl ControlTarget {
        #[unsafe(method(fire:))]
        fn fire(&self, _sender: Option<&AnyObject>) {
            (self.ivars().handler)(Event::Action);
        }

        #[unsafe(method(textChanged:))]
        fn text_changed(&self, _notification: &NSNotification) {
            (self.ivars().handler)(Event::Changed);
        }

        #[unsafe(method(editingEnded:))]
        fn editing_ended(&self, _notification: &NSNotification) {
            (self.ivars().handler)(Event::Ended);
        }
    }

    unsafe impl NSObjectProtocol for ControlTarget {}
);

impl ControlTarget {
    /// A target that calls `handler` on the main thread.
    pub(super) fn new(mtm: MainThreadMarker, handler: impl Fn(Event) + 'static) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(TargetIvars {
            handler: Box::new(handler),
        });
        // SAFETY: `NSObject` initializes with `init`.
        unsafe { msg_send![super(this), init] }
    }

    /// Calls the target when `object` posts the notification `name`
    /// (`Changed` or `Ended` events). A target is held weakly by the center.
    pub(super) fn observe(&self, name: &NSString, object: &AnyObject, ended: bool) {
        let selector = if ended {
            sel!(editingEnded:)
        } else {
            sel!(textChanged:)
        };
        // SAFETY: both selectors take one notification, as the center requires.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                self,
                selector,
                Some(name),
                Some(object),
            );
        }
    }
}

impl Drop for ControlTarget {
    fn drop(&mut self) {
        // The center does not retain observers, but a stale pointer must not
        // outlive the target on systems that still keep one.
        // SAFETY: removing an observer that may not be registered is allowed.
        unsafe {
            NSNotificationCenter::defaultCenter().removeObserver(self);
        }
    }
}
