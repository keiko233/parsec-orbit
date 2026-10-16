//! Thread-safe registry of UI pages, their state and action subscribers.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::ThreadId;

use parsec_orbit_plugin_sdk::{MessageCallback, ERR_EXISTS, ERR_INVALID, ERR_NOT_FOUND, OK};
use parsec_orbit_ui_protocol::schema::UiNavGroup;
use parsec_orbit_ui_protocol::{PageState, UiAction, UiPage, UiPatch};

struct SubscriptionState {
    active: bool,
    in_flight: HashMap<ThreadId, usize>,
}

struct ActionSubscription {
    page_id: String,
    cb: MessageCallback,
    user: *mut c_void,
    state: Mutex<SubscriptionState>,
    idle: Condvar,
}

// Safety: The subscription callbacks and user pointers are managed across threads by design.
unsafe impl Send for ActionSubscription {}
unsafe impl Sync for ActionSubscription {}

struct DeliveryGuard<'a>(&'a ActionSubscription);

impl Drop for DeliveryGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        let thread_id = std::thread::current().id();
        let count = state
            .in_flight
            .get_mut(&thread_id)
            .expect("delivery thread is tracked");
        *count -= 1;
        if *count == 0 {
            state.in_flight.remove(&thread_id);
        }
        self.0.idle.notify_all();
    }
}

impl ActionSubscription {
    fn deliver(&self, action_json: &str) {
        {
            let mut state = self.state.lock().unwrap();
            if !state.active {
                return;
            }
            *state
                .in_flight
                .entry(std::thread::current().id())
                .or_default() += 1;
        }

        let _delivery = DeliveryGuard(self);
        unsafe {
            (self.cb)(self.user, action_json.as_ptr(), action_json.len());
        }
    }

    fn cancel_and_wait(&self) {
        let mut state = self.state.lock().unwrap();
        state.active = false;
        let current_thread = std::thread::current().id();
        let current_thread_deliveries = state
            .in_flight
            .get(&current_thread)
            .copied()
            .unwrap_or_default();

        // A callback may cancel itself. Wait for deliveries on other threads,
        // while allowing callbacks already on this thread to return normally.
        while state.in_flight.values().sum::<usize>() > current_thread_deliveries {
            state = self.idle.wait(state).unwrap();
        }
    }
}

/// An action encoded and paired with the subscribers present when it was accepted.
pub(crate) struct PendingAction {
    action_json: String,
    subscriptions: Vec<Arc<ActionSubscription>>,
}

impl PendingAction {
    /// Delivers this action to its snapshotted subscribers that remain active.
    pub(crate) fn deliver(self) {
        for subscription in self.subscriptions {
            subscription.deliver(&self.action_json);
        }
    }
}

/// Most pages that may be registered at once.
pub const MAX_PAGES: usize = 64;
/// Largest accepted `update_page` patch, in bytes.
pub const MAX_PATCH_BYTES: usize = 1024 * 1024;

/// A refused request: the ABI code and the reason the service logs.
#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: i32,
    pub reason: String,
}

impl Failure {
    fn invalid(reason: impl Into<String>) -> Self {
        Self {
            code: ERR_INVALID,
            reason: reason.into(),
        }
    }

    fn with(code: i32, reason: &str) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }
}

/// Manages UI pages, their state and action subscribers.
pub struct Registry {
    lifecycle: Mutex<()>,
    pages: RwLock<HashMap<String, PageState>>,
    selected: Mutex<Option<String>>,
    subscriptions: RwLock<HashMap<i64, Arc<ActionSubscription>>>,
    next_subscription_id: AtomicI64,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            lifecycle: Mutex::new(()),
            pages: RwLock::new(HashMap::new()),
            selected: Mutex::new(None),
            subscriptions: RwLock::new(HashMap::new()),
            next_subscription_id: AtomicI64::new(1),
        }
    }

    /// Registers a declarative page. `reserved` admits the `orbit.` prefix and
    /// group, which only the UI service's own pages may use.
    pub fn register_page(&self, page_json: &str, reserved: bool) -> Result<(), Failure> {
        let page = UiPage::parse(page_json, reserved).map_err(Failure::invalid)?;
        let _lifecycle = self.lifecycle.lock().unwrap();
        let mut pages = self.pages.write().unwrap();
        if pages.contains_key(&page.id) {
            return Err(Failure::with(ERR_EXISTS, "page id is already registered"));
        }
        if pages.len() >= MAX_PAGES {
            return Err(Failure::invalid(format!(
                "at most {MAX_PAGES} pages may be registered"
            )));
        }
        pages.insert(page.id.clone(), PageState::new(page));
        Ok(())
    }

    /// Unregisters a page by its ID.
    pub fn unregister_page(&self, page_id: &str) -> i32 {
        let removed = {
            let _lifecycle = self.lifecycle.lock().unwrap();
            if self.pages.write().unwrap().remove(page_id).is_none() {
                return ERR_NOT_FOUND;
            }
            let mut selected = self.selected.lock().unwrap();
            if selected.as_deref() == Some(page_id) {
                *selected = None;
            }
            drop(selected);
            let mut subs = self.subscriptions.write().unwrap();
            let ids: Vec<_> = subs
                .iter()
                .filter_map(|(id, sub)| (sub.page_id == page_id).then_some(*id))
                .collect();
            ids.into_iter()
                .filter_map(|id| subs.remove(&id))
                .collect::<Vec<_>>()
        };
        for sub in removed {
            sub.cancel_and_wait();
        }

        OK
    }

    /// Applies a patch to a registered page, entirely or not at all.
    pub fn update_page(&self, page_id: &str, patch_json: &str) -> Result<(), Failure> {
        if patch_json.len() > MAX_PATCH_BYTES {
            return Err(Failure::invalid(format!(
                "patch exceeds {MAX_PATCH_BYTES} bytes"
            )));
        }
        // serde would also read a struct from a JSON array; a patch is an object.
        if !patch_json.trim_start().starts_with('{') {
            return Err(Failure::invalid("a patch is a JSON object"));
        }
        let patch: UiPatch = serde_json::from_str(patch_json)
            .map_err(|error| Failure::invalid(error.to_string()))?;
        let _lifecycle = self.lifecycle.lock().unwrap();
        let mut pages = self.pages.write().unwrap();
        let page = pages
            .get_mut(page_id)
            .ok_or_else(|| Failure::with(ERR_NOT_FOUND, "page is not registered"))?;
        page.apply_patch(patch).map_err(Failure::invalid)
    }

    /// Selects a registered page for the window to show.
    pub fn navigate(&self, page_id: &str) -> i32 {
        let _lifecycle = self.lifecycle.lock().unwrap();
        if !self.pages.read().unwrap().contains_key(page_id) {
            return ERR_NOT_FOUND;
        }
        *self.selected.lock().unwrap() = Some(page_id.to_owned());
        OK
    }

    /// The page most recently selected with [`Registry::navigate`], if it still exists.
    pub fn selected(&self) -> Option<String> {
        self.selected.lock().unwrap().clone()
    }

    /// Subscribes to action events emitted by a page.
    pub fn on_action(&self, page_id: &str, cb: MessageCallback, user: *mut c_void) -> i64 {
        let id = self.next_subscription_id.fetch_add(1, Ordering::SeqCst);
        let mut subs = self.subscriptions.write().unwrap();
        subs.insert(
            id,
            Arc::new(ActionSubscription {
                page_id: page_id.to_string(),
                cb,
                user,
                state: Mutex::new(SubscriptionState {
                    active: true,
                    in_flight: HashMap::new(),
                }),
                idle: Condvar::new(),
            }),
        );
        id
    }

    /// Unsubscribes an action subscriber.
    pub fn off_action(&self, subscription: i64) -> i32 {
        let sub = self.subscriptions.write().unwrap().remove(&subscription);
        match sub {
            Some(sub) => {
                sub.cancel_and_wait();
                OK
            }
            None => ERR_NOT_FOUND,
        }
    }

    /// Dispatches an action event to all registered subscribers of the action's page.
    pub fn dispatch_action(&self, action: &UiAction) {
        if let Some(pending) = self.prepare_action(action) {
            pending.deliver();
        }
    }

    /// Encodes an action and snapshots the subscribers that existed when it was accepted.
    pub(crate) fn prepare_action(&self, action: &UiAction) -> Option<PendingAction> {
        let action_json = serde_json::to_string(action).ok()?;
        let mut subs: Vec<_> = self
            .subscriptions
            .read()
            .unwrap()
            .iter()
            .filter(|(_, sub)| sub.page_id == action.page)
            .map(|(id, sub)| (*id, Arc::clone(sub)))
            .collect();
        subs.sort_by_key(|(id, _)| *id);
        Some(PendingAction {
            action_json,
            subscriptions: subs.into_iter().map(|(_, sub)| sub).collect(),
        })
    }

    /// Whether an action from the renderer names a live element and carries a fitting value.
    pub fn accepts_action(&self, action: &UiAction) -> bool {
        let _lifecycle = self.lifecycle.lock().unwrap();
        self.pages
            .read()
            .unwrap()
            .get(&action.page)
            .is_some_and(|page| page.accepts_action(action))
    }

    /// A snapshot of all pages in navigation order: the `orbit` group first, then by
    /// `order`, title and id.
    pub fn list_pages(&self) -> Vec<PageState> {
        let _lifecycle = self.lifecycle.lock().unwrap();
        let mut list: Vec<PageState> = self.pages.read().unwrap().values().cloned().collect();
        list.sort_by_key(|state| {
            (
                state.page.nav_group() != UiNavGroup::Orbit,
                state.page.order.unwrap_or(0),
                state.page.title.to_lowercase(),
                state.page.id.clone(),
            )
        });
        list
    }

    /// A copy of one page's state.
    pub fn get_page(&self, page_id: &str) -> Option<PageState> {
        let _lifecycle = self.lifecycle.lock().unwrap();
        self.pages.read().unwrap().get(page_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::mpsc;
    use std::sync::Barrier;
    use std::time::Duration;

    unsafe extern "system" fn test_cb(user: *mut c_void, msg: *const u8, len: usize) {
        let slice = unsafe { core::slice::from_raw_parts(msg, len) };
        let s = String::from_utf8_lossy(slice).into_owned();
        let received = unsafe { &*(user as *const Mutex<Option<String>>) };
        *received.lock().unwrap() = Some(s);
    }

    struct CancelContext {
        registry: *const Registry,
        subscription: AtomicI64,
        calls: AtomicI64,
    }

    unsafe extern "system" fn self_cancel_cb(user: *mut c_void, _: *const u8, _: usize) {
        let context = unsafe { &*(user as *const CancelContext) };
        context.calls.fetch_add(1, Ordering::SeqCst);
        unsafe { &*context.registry }.off_action(context.subscription.load(Ordering::SeqCst));
    }

    unsafe extern "system" fn cancel_other_cb(user: *mut c_void, _: *const u8, _: usize) {
        let context = unsafe { &*(user as *const CancelContext) };
        context.calls.fetch_add(1, Ordering::SeqCst);
        unsafe { &*context.registry }.off_action(context.subscription.load(Ordering::SeqCst));
    }

    unsafe extern "system" fn count_cb(user: *mut c_void, _: *const u8, _: usize) {
        let calls = unsafe { &*(user as *const AtomicI64) };
        calls.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "system" fn update_page_cb(user: *mut c_void, _: *const u8, _: usize) {
        let registry = unsafe { &*(user as *const Registry) };
        registry
            .update_page("test.sec", r#"{"values":{"updated":true}}"#)
            .unwrap();
    }

    unsafe extern "system" fn blocking_cb(user: *mut c_void, _: *const u8, _: usize) {
        let barriers = unsafe { &*(user as *const (Barrier, Barrier)) };
        barriers.0.wait();
        barriers.1.wait();
    }

    fn page(id: &str) -> String {
        json!({
            "schema": 2, "id": id, "title": "Title",
            "blocks": [{"type": "group", "settings": [
                {"key": "bitrate", "label": "Bitrate",
                 "control": {"type": "number", "min": 0, "max": 500, "default": 50}},
                {"key": "updated", "label": "Updated",
                 "control": {"type": "toggle", "default": false}}
            ]}]
        })
        .to_string()
    }

    fn code(result: Result<(), Failure>) -> i32 {
        result.err().map_or(OK, |failure| failure.code)
    }

    #[test]
    fn registry_crud_and_events() {
        let reg = Registry::new();
        assert_eq!(code(reg.register_page(&page("test.sec"), false)), OK);
        assert_eq!(
            code(reg.register_page(&page("test.sec"), false)),
            ERR_EXISTS
        );
        assert_eq!(code(reg.register_page("not json", false)), ERR_INVALID);

        let state = reg.get_page("test.sec").expect("state exists");
        assert_eq!(state.values["bitrate"], Value::from(50));

        let received: Mutex<Option<String>> = Mutex::new(None);
        let sub = reg.on_action("test.sec", test_cb, &received as *const _ as *mut c_void);
        assert!(sub > 0);

        reg.dispatch_action(&UiAction {
            page: "test.sec".to_string(),
            key: "bitrate".to_string(),
            action_type: "change".to_string(),
            value: Some(Value::from(80)),
        });

        let received = received.lock().unwrap().take().expect("callback received");
        assert!(received.contains("\"key\":\"bitrate\""));
        assert!(received.contains("\"value\":80"));
        assert!(received.contains("\"page\":\"test.sec\""));

        assert_eq!(reg.off_action(sub), OK);
        assert_eq!(reg.off_action(sub), ERR_NOT_FOUND);

        assert_eq!(reg.unregister_page("test.sec"), OK);
        assert_eq!(reg.unregister_page("test.sec"), ERR_NOT_FOUND);
    }

    #[test]
    fn updates_navigation_and_actions_follow_the_registered_page() {
        let reg = Registry::new();
        assert_eq!(reg.navigate("test.sec"), ERR_NOT_FOUND);
        assert_eq!(code(reg.update_page("test.sec", "{}")), ERR_NOT_FOUND);
        assert_eq!(code(reg.register_page(&page("test.sec"), false)), OK);

        assert_eq!(
            code(reg.update_page("test.sec", r#"{"values":{"bitrate":90}}"#)),
            OK
        );
        assert_eq!(
            reg.get_page("test.sec").unwrap().values["bitrate"],
            json!(90)
        );
        // Unknown keys, type mismatches and unknown members are refused.
        for bad in [
            r#"{"values":{"nope":1}}"#,
            r#"{"values":{"bitrate":"x"}}"#,
            r#"{"values":{"bitrate":900}}"#,
            r#"{"colour":1}"#,
            "[]",
        ] {
            assert_eq!(code(reg.update_page("test.sec", bad)), ERR_INVALID, "{bad}");
        }
        assert_eq!(
            reg.get_page("test.sec").unwrap().values["bitrate"],
            json!(90)
        );

        assert_eq!(reg.navigate("test.sec"), OK);
        assert_eq!(reg.selected().as_deref(), Some("test.sec"));
        let change = |value: Value| UiAction {
            page: "test.sec".into(),
            key: "bitrate".into(),
            action_type: "change".into(),
            value: Some(value),
        };
        assert!(reg.accepts_action(&change(json!(10))));
        assert!(!reg.accepts_action(&change(json!(501))));
        assert!(!reg.accepts_action(&UiAction {
            page: "other".into(),
            ..change(json!(10))
        }));

        assert_eq!(reg.unregister_page("test.sec"), OK);
        assert_eq!(reg.selected(), None);
        assert!(!reg.accepts_action(&change(json!(10))));
    }

    #[test]
    fn reserved_pages_need_the_service_and_the_page_count_is_bounded() {
        let reg = Registry::new();
        assert_eq!(
            code(reg.register_page(&page("orbit.fake"), false)),
            ERR_INVALID
        );
        assert_eq!(code(reg.register_page(&page("orbit.real"), true)), OK);
        for n in 1..MAX_PAGES {
            assert_eq!(code(reg.register_page(&page(&format!("p{n}")), false)), OK);
        }
        assert_eq!(
            code(reg.register_page(&page("one.too.many"), false)),
            ERR_INVALID
        );
    }

    #[test]
    fn pages_list_in_navigation_order() {
        let reg = Registry::new();
        let titled = |id: &str, title: &str, order: Option<i32>, group: Option<&str>| {
            let mut value = json!({"schema": 2, "id": id, "title": title, "blocks": []});
            if let Some(order) = order {
                value["order"] = json!(order);
            }
            if let Some(group) = group {
                value["group"] = json!(group);
            }
            value.to_string()
        };
        for (id, title, order, group) in [
            ("b", "Beta", None, None),
            ("a", "alpha", None, None),
            ("late", "Aardvark", Some(10), None),
            ("orbit.log", "Log", Some(2), Some("orbit")),
            ("orbit.overview", "Overview", Some(1), Some("orbit")),
        ] {
            assert_eq!(
                code(reg.register_page(&titled(id, title, order, group), true)),
                OK
            );
        }
        let ids: Vec<_> = reg
            .list_pages()
            .into_iter()
            .map(|state| state.page.id)
            .collect();
        assert_eq!(ids, ["orbit.overview", "orbit.log", "a", "b", "late"]);
    }

    #[test]
    fn callback_can_unsubscribe_itself() {
        let reg = Registry::new();
        let context = CancelContext {
            registry: &reg,
            subscription: AtomicI64::new(0),
            calls: AtomicI64::new(0),
        };
        let sub = reg.on_action(
            "test.sec",
            self_cancel_cb,
            &context as *const _ as *mut c_void,
        );
        context.subscription.store(sub, Ordering::SeqCst);

        let action = test_action();
        reg.dispatch_action(&action);
        reg.dispatch_action(&action);

        assert_eq!(context.calls.load(Ordering::SeqCst), 1);
        assert_eq!(reg.off_action(sub), ERR_NOT_FOUND);
    }

    #[test]
    fn callback_can_cancel_another_subscription_in_its_snapshot() {
        let reg = Registry::new();
        let calls = AtomicI64::new(0);
        let context = CancelContext {
            registry: &reg,
            subscription: AtomicI64::new(0),
            calls: AtomicI64::new(0),
        };
        reg.on_action(
            "test.sec",
            cancel_other_cb,
            &context as *const _ as *mut c_void,
        );
        let other = reg.on_action("test.sec", count_cb, &calls as *const _ as *mut c_void);
        context.subscription.store(other, Ordering::SeqCst);

        reg.dispatch_action(&test_action());

        assert_eq!(context.calls.load(Ordering::SeqCst), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(reg.off_action(other), ERR_NOT_FOUND);
    }

    #[test]
    fn pending_action_does_not_reach_subscribers_added_after_preparation() {
        let reg = Registry::new();
        let old_calls = AtomicI64::new(0);
        let new_calls = AtomicI64::new(0);
        let old = reg.on_action("test.sec", count_cb, &old_calls as *const _ as *mut c_void);
        let pending = reg.prepare_action(&test_action()).expect("action encoded");

        assert_eq!(reg.off_action(old), OK);
        let new = reg.on_action("test.sec", count_cb, &new_calls as *const _ as *mut c_void);
        pending.deliver();

        assert_eq!(old_calls.load(Ordering::SeqCst), 0);
        assert_eq!(new_calls.load(Ordering::SeqCst), 0);
        assert_eq!(reg.off_action(new), OK);
    }

    #[test]
    fn cancellation_waits_for_an_in_flight_callback() {
        let reg = Arc::new(Registry::new());
        let barriers = Arc::new((Barrier::new(2), Barrier::new(2)));
        let sub = reg.on_action(
            "test.sec",
            blocking_cb,
            Arc::as_ptr(&barriers) as *mut c_void,
        );
        let dispatch_registry = Arc::clone(&reg);
        let dispatch =
            std::thread::spawn(move || dispatch_registry.dispatch_action(&test_action()));

        barriers.0.wait();
        let cancel_registry = Arc::clone(&reg);
        let (done_tx, done_rx) = mpsc::channel();
        let cancel = std::thread::spawn(move || {
            let result = cancel_registry.off_action(sub);
            done_tx.send(result).unwrap();
        });
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        barriers.1.wait();
        assert_eq!(done_rx.recv().unwrap(), OK);
        dispatch.join().unwrap();
        cancel.join().unwrap();
    }

    #[test]
    fn callback_cancellation_waits_for_target_running_on_another_thread() {
        let reg = Arc::new(Registry::new());
        let barriers = Arc::new((Barrier::new(2), Barrier::new(2)));
        let target = reg.on_action(
            "test.sec",
            blocking_cb,
            Arc::as_ptr(&barriers) as *mut c_void,
        );
        let context = CancelContext {
            registry: Arc::as_ptr(&reg),
            subscription: AtomicI64::new(target),
            calls: AtomicI64::new(0),
        };
        reg.on_action(
            "cancel.sec",
            cancel_other_cb,
            &context as *const _ as *mut c_void,
        );

        let target_registry = Arc::clone(&reg);
        let target_dispatch = std::thread::spawn(move || {
            target_registry.dispatch_action(&test_action());
        });
        barriers.0.wait();

        let cancel_registry = Arc::clone(&reg);
        let (done_tx, done_rx) = mpsc::channel();
        let cancel_dispatch = std::thread::spawn(move || {
            cancel_registry.dispatch_action(&UiAction {
                page: "cancel.sec".to_string(),
                key: "cancel".to_string(),
                action_type: "click".to_string(),
                value: None,
            });
            done_tx.send(()).unwrap();
        });

        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        barriers.1.wait();

        done_rx.recv().unwrap();
        target_dispatch.join().unwrap();
        cancel_dispatch.join().unwrap();
        assert_eq!(context.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn callback_can_reenter_update_page() {
        let reg = Registry::new();
        assert_eq!(code(reg.register_page(&page("test.sec"), false)), OK);
        reg.on_action("test.sec", update_page_cb, &reg as *const _ as *mut c_void);

        reg.dispatch_action(&test_action());

        assert_eq!(
            reg.get_page("test.sec").unwrap().values.get("updated"),
            Some(&Value::Bool(true))
        );
    }

    fn test_action() -> UiAction {
        UiAction {
            page: "test.sec".to_string(),
            key: "test".to_string(),
            action_type: "change".to_string(),
            value: None,
        }
    }
}
