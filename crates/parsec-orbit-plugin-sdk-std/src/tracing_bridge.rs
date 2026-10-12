//! `tracing` events into the loader log (feature `tracing`).

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU64, Ordering};
use std::string::String;

use tracing_core::field::{Field, Visit};
use tracing_core::span::{Attributes, Id, Record};
use tracing_core::{Dispatch, Event, Metadata, Subscriber};

use parsec_orbit_plugin_sdk::{Context, Level};

/// Formats the fields of an event: `message` first, then `name=value`.
#[derive(Default)]
struct Fields {
    message: String,
    rest: String,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }
}

/// Forwards every event to the host log with the plugin id as target.
struct HostSubscriber {
    cx: Context,
    next_span: AtomicU64,
}

impl Subscriber for HostSubscriber {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed))
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let metadata = event.metadata();
        let level = match *metadata.level() {
            tracing_core::Level::TRACE => Level::Trace,
            tracing_core::Level::DEBUG => Level::Debug,
            tracing_core::Level::INFO => Level::Info,
            tracing_core::Level::WARN => Level::Warn,
            tracing_core::Level::ERROR => Level::Error,
        };
        self.cx.log(
            level,
            format_args!("{}: {}{}", metadata.target(), fields.message, fields.rest),
        );
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

/// Bridges `tracing` into the loader log.
pub trait TracingExt {
    /// Makes `tracing` events (from this plugin and from the crates it uses)
    /// end up in the loader log, tagged with the plugin id. Fails if a global
    /// subscriber is already installed in this module.
    fn install_tracing(&self) -> Result<(), tracing_core::dispatcher::SetGlobalDefaultError>;
}

impl TracingExt for Context {
    fn install_tracing(&self) -> Result<(), tracing_core::dispatcher::SetGlobalDefaultError> {
        tracing_core::dispatcher::set_global_default(Dispatch::new(HostSubscriber {
            cx: *self,
            next_span: AtomicU64::new(1),
        }))
    }
}
