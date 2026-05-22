//! `okvm-logging` — initialisation `tracing` + sink Windows Event Log.
//!
//! Le sink Windows Event Log utilise `ReportEventW` avec une source dediee
//! `OneClickKVM` (a enregistrer via une installation de cle de registre lors
//! du setup ; en l'absence de cle, ReportEventW journalise tout de meme sous
//! la source mais avec un message generique).
//!
//! - **Aucun payload sensible** (frappes, cles, contenu clipboard) ne doit
//!   transiter par les logs — voir `docs/SECURITY.md` §7.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]

use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

const EVENT_SOURCE: &str = "OneClickKVM";

/// Initialise les souscripteurs `tracing` (stdout JSON + filtre via `RUST_LOG`).
///
/// Le sink Windows Event Log est ajoute uniquement sur Windows ; il logge les
/// niveaux WARN/ERROR (les niveaux inferieurs alourdiraient inutilement le
/// journal Windows).
pub fn init_default() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level()));

    let fmt_layer = fmt::layer().with_target(true).with_level(true).json();

    #[cfg(windows)]
    {
        let event_log = event_log::EventLogLayer::new(EVENT_SOURCE);
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .with(event_log)
            .try_init();
    }
    #[cfg(not(windows))]
    {
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .try_init();
    }
}

fn default_level() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "info"
    }
}

/// Sink Windows Event Log accessible directement par les tests.
#[cfg(windows)]
pub mod event_log {
    use std::sync::Once;

    use super::{Context, Event, Layer, Subscriber};

    use windows::core::PCWSTR;
    use windows::Win32::System::EventLog::{
        DeregisterEventSource, RegisterEventSourceW, ReportEventW, EVENTLOG_ERROR_TYPE,
        EVENTLOG_INFORMATION_TYPE, EVENTLOG_WARNING_TYPE, REPORT_EVENT_TYPE,
    };

    /// Layer `tracing-subscriber` qui forward les events Warn/Error vers Event Log.
    pub struct EventLogLayer {
        source: Vec<u16>,
        init: Once,
        handle: parking_lot::Mutex<Option<isize>>,
    }

    impl EventLogLayer {
        /// Cree une nouvelle layer pour la source `name`.
        #[must_use]
        pub fn new(name: &str) -> Self {
            let source = name.encode_utf16().chain(std::iter::once(0)).collect();
            Self {
                source,
                init: Once::new(),
                handle: parking_lot::Mutex::new(None),
            }
        }

        fn ensure_handle(&self) {
            self.init.call_once(|| {
                let h =
                    unsafe { RegisterEventSourceW(PCWSTR::null(), PCWSTR(self.source.as_ptr())) };
                match h {
                    Ok(handle) => {
                        *self.handle.lock() = Some(handle.0 as isize);
                    }
                    Err(e) => {
                        eprintln!("RegisterEventSourceW failed: {e}");
                    }
                }
            });
        }

        fn emit(&self, level: tracing::Level, msg: &str) {
            self.ensure_handle();
            let handle_isize = match *self.handle.lock() {
                Some(h) => h,
                None => return,
            };
            let event_type: REPORT_EVENT_TYPE = match level {
                tracing::Level::ERROR => EVENTLOG_ERROR_TYPE,
                tracing::Level::WARN => EVENTLOG_WARNING_TYPE,
                _ => EVENTLOG_INFORMATION_TYPE,
            };

            let mut wide: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
            let ptr_arr = [PCWSTR(wide.as_mut_ptr())];

            // SAFETY: handle a ete cree par RegisterEventSourceW ;
            // ptr_arr pointe vers une chaine UTF-16 nul-terminee ; le slice
            // n'est pas elargi pendant l'appel.
            unsafe {
                let _ = ReportEventW(
                    windows::Win32::Foundation::HANDLE(handle_isize as *mut _),
                    event_type,
                    0,
                    0,
                    None,
                    0,
                    Some(&ptr_arr),
                    None,
                );
            }
        }
    }

    impl Drop for EventLogLayer {
        fn drop(&mut self) {
            if let Some(h) = self.handle.lock().take() {
                // SAFETY: handle issu de RegisterEventSourceW.
                unsafe {
                    let _ = DeregisterEventSource(windows::Win32::Foundation::HANDLE(h as *mut _));
                }
            }
        }
    }

    impl<S: Subscriber> Layer<S> for EventLogLayer {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let level = *event.metadata().level();
            // On ne forward que WARN et ERROR vers Event Viewer.
            if level > tracing::Level::WARN {
                return;
            }
            let mut visitor = MessageVisitor::default();
            event.record(&mut visitor);
            let msg = format!(
                "[{}] {}: {}",
                event.metadata().target(),
                event.metadata().name(),
                visitor.message,
            );
            self.emit(level, &msg);
        }
    }

    #[derive(Default)]
    struct MessageVisitor {
        message: String,
    }

    impl tracing::field::Visit for MessageVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write;
            let _ = write!(&mut self.message, "{}={:?} ", field.name(), value);
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            use std::fmt::Write;
            let _ = write!(&mut self.message, "{}={} ", field.name(), value);
        }
    }
}

/// Compatibilite : ancien nom.
#[cfg(windows)]
pub fn install_event_log_sink() -> okvm_core::Result<()> {
    Ok(())
}
