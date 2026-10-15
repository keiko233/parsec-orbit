//! Thread-safe state registry for registered UI sections, values, and action subscribers.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::ThreadId;

use parsec_orbit_plugin_sdk::{MessageCallback, ERR_EXISTS, ERR_INVALID, ERR_NOT_FOUND, OK};
use serde_json::Value;

use crate::schema::{UiAction, UiSection};

struct SubscriptionState {
    active: bool,
    in_flight: HashMap<ThreadId, usize>,
}

struct ActionSubscription {
    section_id: String,
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

/// Manages UI section schemas, state properties, and action subscribers.
pub struct Registry {
    lifecycle: Mutex<()>,
    sections: RwLock<HashMap<String, UiSection>>,
    states: RwLock<HashMap<String, HashMap<String, Value>>>,
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
            sections: RwLock::new(HashMap::new()),
            states: RwLock::new(HashMap::new()),
            subscriptions: RwLock::new(HashMap::new()),
            next_subscription_id: AtomicI64::new(1),
        }
    }

    /// Registers a declarative section schema.
    pub fn register_section(&self, schema_json: &str) -> i32 {
        let section: UiSection = match serde_json::from_str(schema_json) {
            Ok(sec) => sec,
            Err(_) => return ERR_INVALID,
        };

        if section.id.trim().is_empty() {
            return ERR_INVALID;
        }

        let _lifecycle = self.lifecycle.lock().unwrap();
        let mut sections = self.sections.write().unwrap();
        if sections.contains_key(&section.id) {
            return ERR_EXISTS;
        }

        let mut initial_state = HashMap::new();
        for item in &section.items {
            if let Some(def) = &item.default {
                initial_state.insert(item.key.clone(), def.clone());
            }
        }

        let id = section.id.clone();
        sections.insert(id.clone(), section);
        drop(sections);

        let mut states = self.states.write().unwrap();
        states.insert(id, initial_state);

        OK
    }

    /// Unregisters a section by its ID.
    pub fn unregister_section(&self, section_id: &str) -> i32 {
        let removed = {
            let _lifecycle = self.lifecycle.lock().unwrap();
            let mut sections = self.sections.write().unwrap();
            if sections.remove(section_id).is_none() {
                return ERR_NOT_FOUND;
            }
            drop(sections);

            self.states.write().unwrap().remove(section_id);
            let mut subs = self.subscriptions.write().unwrap();
            let ids: Vec<_> = subs
                .iter()
                .filter_map(|(id, sub)| (sub.section_id == section_id).then_some(*id))
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

    /// Updates state values for a registered section.
    pub fn update_state(&self, section_id: &str, state_json: &str) -> i32 {
        let updates: HashMap<String, Value> = match serde_json::from_str(state_json) {
            Ok(map) => map,
            Err(_) => return ERR_INVALID,
        };

        let _lifecycle = self.lifecycle.lock().unwrap();
        let sections = self.sections.read().unwrap();
        if !sections.contains_key(section_id) {
            return ERR_NOT_FOUND;
        }
        drop(sections);

        let mut states = self.states.write().unwrap();
        let entry = states.entry(section_id.to_string()).or_default();
        for (k, v) in updates {
            entry.insert(k, v);
        }

        OK
    }

    /// Subscribes to action events emitted by a section.
    pub fn on_action(&self, section_id: &str, cb: MessageCallback, user: *mut c_void) -> i64 {
        let id = self.next_subscription_id.fetch_add(1, Ordering::SeqCst);
        let mut subs = self.subscriptions.write().unwrap();
        subs.insert(
            id,
            Arc::new(ActionSubscription {
                section_id: section_id.to_string(),
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

    /// Dispatches an action event to all registered subscribers of the action's section.
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
            .filter(|(_, sub)| sub.section_id == action.section)
            .map(|(id, sub)| (*id, Arc::clone(sub)))
            .collect();
        subs.sort_by_key(|(id, _)| *id);
        Some(PendingAction {
            action_json,
            subscriptions: subs.into_iter().map(|(_, sub)| sub).collect(),
        })
    }

    /// Returns a snapshot of all registered sections, sorted by order.
    pub fn list_sections(&self) -> Vec<UiSection> {
        let _lifecycle = self.lifecycle.lock().unwrap();
        let sections = self.sections.read().unwrap();
        let mut list: Vec<UiSection> = sections.values().cloned().collect();
        list.sort_by_key(|s| s.order.unwrap_or(100));
        list
    }

    /// Returns a copy of the state values for a section.
    pub fn get_state(&self, section_id: &str) -> Option<HashMap<String, Value>> {
        let _lifecycle = self.lifecycle.lock().unwrap();
        let states = self.states.read().unwrap();
        states.get(section_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    unsafe extern "system" fn update_state_cb(user: *mut c_void, _: *const u8, _: usize) {
        let registry = unsafe { &*(user as *const Registry) };
        assert_eq!(registry.update_state("test.sec", r#"{"updated":true}"#), OK);
    }

    unsafe extern "system" fn blocking_cb(user: *mut c_void, _: *const u8, _: usize) {
        let barriers = unsafe { &*(user as *const (Barrier, Barrier)) };
        barriers.0.wait();
        barriers.1.wait();
    }

    #[test]
    fn registry_crud_and_events() {
        let reg = Registry::new();
        let schema = r#"{
            "id": "test.sec",
            "title": "Title",
            "items": [{"key": "bitrate", "type": "number_input", "label": "Bitrate", "default": 50}]
        }"#;

        assert_eq!(reg.register_section(schema), OK);
        assert_eq!(reg.register_section(schema), ERR_EXISTS);

        let state = reg.get_state("test.sec").expect("state exists");
        assert_eq!(state.get("bitrate").unwrap(), &Value::from(50));

        let received: Mutex<Option<String>> = Mutex::new(None);
        let sub = reg.on_action("test.sec", test_cb, &received as *const _ as *mut c_void);
        assert!(sub > 0);

        reg.dispatch_action(&UiAction {
            section: "test.sec".to_string(),
            key: "bitrate".to_string(),
            action_type: "change".to_string(),
            value: Some(Value::from(80)),
        });

        let received = received.lock().unwrap().take().expect("callback received");
        assert!(received.contains("\"key\":\"bitrate\""));
        assert!(received.contains("\"value\":80"));

        assert_eq!(reg.off_action(sub), OK);
        assert_eq!(reg.off_action(sub), ERR_NOT_FOUND);

        assert_eq!(reg.unregister_section("test.sec"), OK);
        assert_eq!(reg.unregister_section("test.sec"), ERR_NOT_FOUND);
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
                section: "cancel.sec".to_string(),
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
    fn callback_can_reenter_update_state() {
        let reg = Registry::new();
        assert_eq!(
            reg.register_section(r#"{"id":"test.sec","title":"Test","items":[]}"#),
            OK
        );
        reg.on_action("test.sec", update_state_cb, &reg as *const _ as *mut c_void);

        reg.dispatch_action(&test_action());

        assert_eq!(
            reg.get_state("test.sec").unwrap().get("updated"),
            Some(&Value::Bool(true))
        );
    }

    fn test_action() -> UiAction {
        UiAction {
            section: "test.sec".to_string(),
            key: "test".to_string(),
            action_type: "change".to_string(),
            value: None,
        }
    }
}
