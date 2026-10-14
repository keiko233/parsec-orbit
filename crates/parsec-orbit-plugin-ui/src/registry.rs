//! Thread-safe state registry for registered UI sections, values, and action subscribers.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::RwLock;

use parsec_orbit_plugin_sdk_std::{
    MessageCallback, ERR_EXISTS, ERR_INVALID, ERR_NOT_FOUND, OK,
};
use serde_json::Value;

use crate::schema::{UiAction, UiSection};

struct ActionSubscription {
    section_id: String,
    cb: MessageCallback,
    user: *mut c_void,
}

// Safety: The subscription callbacks and user pointers are managed across threads by design.
unsafe impl Send for ActionSubscription {}
unsafe impl Sync for ActionSubscription {}

/// Manages UI section schemas, state properties, and action subscribers.
pub struct Registry {
    sections: RwLock<HashMap<String, UiSection>>,
    states: RwLock<HashMap<String, HashMap<String, Value>>>,
    subscriptions: RwLock<HashMap<i64, ActionSubscription>>,
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
        let mut sections = self.sections.write().unwrap();
        if sections.remove(section_id).is_none() {
            return ERR_NOT_FOUND;
        }
        drop(sections);

        let mut states = self.states.write().unwrap();
        states.remove(section_id);
        drop(states);

        let mut subs = self.subscriptions.write().unwrap();
        subs.retain(|_, sub| sub.section_id != section_id);

        OK
    }

    /// Updates state values for a registered section.
    pub fn update_state(&self, section_id: &str, state_json: &str) -> i32 {
        let updates: HashMap<String, Value> = match serde_json::from_str(state_json) {
            Ok(map) => map,
            Err(_) => return ERR_INVALID,
        };

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
            ActionSubscription {
                section_id: section_id.to_string(),
                cb,
                user,
            },
        );
        id
    }

    /// Unsubscribes an action subscriber.
    pub fn off_action(&self, subscription: i64) -> i32 {
        let mut subs = self.subscriptions.write().unwrap();
        if subs.remove(&subscription).is_some() {
            OK
        } else {
            ERR_NOT_FOUND
        }
    }

    /// Dispatches an action event to all registered subscribers of the action's section.
    pub fn dispatch_action(&self, action: &UiAction) {
        let action_json = match serde_json::to_string(action) {
            Ok(json) => json,
            Err(_) => return,
        };

        let subs = self.subscriptions.read().unwrap();
        for sub in subs.values() {
            if sub.section_id == action.section {
                unsafe {
                    (sub.cb)(sub.user, action_json.as_ptr(), action_json.len());
                }
            }
        }
    }

    /// Returns a snapshot of all registered sections, sorted by order.
    pub fn list_sections(&self) -> Vec<UiSection> {
        let sections = self.sections.read().unwrap();
        let mut list: Vec<UiSection> = sections.values().cloned().collect();
        list.sort_by_key(|s| s.order.unwrap_or(100));
        list
    }

    /// Returns a copy of the state values for a section.
    pub fn get_state(&self, section_id: &str) -> Option<HashMap<String, Value>> {
        let states = self.states.read().unwrap();
        states.get(section_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static CALLED: Mutex<Option<String>> = Mutex::new(None);

    unsafe extern "system" fn test_cb(user: *mut c_void, msg: *const u8, len: usize) {
        let slice = unsafe { core::slice::from_raw_parts(msg, len) };
        let s = String::from_utf8_lossy(slice).into_owned();
        *CALLED.lock().unwrap() = Some(format!("{}:{:?}", s, user));
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

        let sub = reg.on_action("test.sec", test_cb, 0x1234 as *mut c_void);
        assert!(sub > 0);

        reg.dispatch_action(&UiAction {
            section: "test.sec".to_string(),
            key: "bitrate".to_string(),
            action_type: "change".to_string(),
            value: Some(Value::from(80)),
        });

        let received = CALLED.lock().unwrap().take().expect("callback received");
        assert!(received.contains("\"key\":\"bitrate\""));
        assert!(received.contains("\"value\":80"));

        assert_eq!(reg.off_action(sub), OK);
        assert_eq!(reg.off_action(sub), ERR_NOT_FOUND);

        assert_eq!(reg.unregister_section("test.sec"), OK);
        assert_eq!(reg.unregister_section("test.sec"), ERR_NOT_FOUND);
    }
}
