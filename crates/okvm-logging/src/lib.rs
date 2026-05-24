//! `okvm-logging` — initialisation `tracing` + sinks (stdout, Windows Event Log,
//! fichier rotatif quotidien).
//!
//! Sinks disponibles :
//! - **stdout JSON** : utile en dev (`pnpm tauri dev`), invisible en release
//!   (windows_subsystem = "windows" supprime la console).
//! - **Windows Event Log** (`OneClickKVM` source) : WARN/ERROR uniquement.
//! - **Fichier rotatif** : tous les events au niveau filter par défaut,
//!   écrits dans `%LocalAppData%\OneClick\OneClickKVM\logs\app.log`
//!   avec rotation quotidienne (`app.log.YYYY-MM-DD`, max 7 fichiers).
//!
//! - **Aucun payload sensible** (frappes, cles, contenu clipboard) ne doit
//!   transiter par les logs — voir `docs/SECURITY.md` §7.

#![deny(unsafe_op_in_unsafe_fn, unreachable_pub)]
#![warn(missing_docs, clippy::pedantic)]

use std::path::PathBuf;

use tracing::{Event, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

const EVENT_SOURCE: &str = "OneClickKVM";

/// Guard à garder vivant tant que l'app tourne — sans ça les logs vers
/// fichier ne sont pas flushés à la sortie.
pub struct LoggingGuard {
    _file_guard: Option<WorkerGuard>,
}

/// Initialise les souscripteurs `tracing` SANS appender fichier (legacy +
/// utilisable par les tests qui ne veulent pas créer de log dir).
///
/// Sinks : stdout JSON + Windows Event Log (WARN/ERROR).
///
/// **Pour une app desktop**, préférer [`init_with_file`] qui ajoute en plus
/// un fichier rotatif lisible par l'utilisateur en cas de problème.
pub fn init_default() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level()));

    let fmt_layer = fmt::layer().with_target(true).with_level(true).json();
    let event_log = event_log::EventLogLayer::new(EVENT_SOURCE);
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(event_log)
        .try_init();
}

/// Initialise tous les sinks : stdout + Event Log + **fichier rotatif**.
///
/// Le fichier est écrit dans `%LocalAppData%\OneClick\OneClickKVM\logs\`
/// (cf. [`log_dir`]). Rotation quotidienne via `tracing_appender::rolling::daily`,
/// non-bloquant via worker thread interne.
///
/// **IMPORTANT** : garder le [`LoggingGuard`] retourné vivant pendant toute
/// la durée de vie du process. À sa destruction, le worker flushe les logs
/// pending puis termine. Si le guard est drop trop tôt, les dernières lignes
/// ne sont pas écrites.
///
/// # Erreurs
/// Aucune — best-effort. Si le log dir ne peut pas être créé, on init quand
/// même les sinks stdout + Event Log et on logue un `warn`.
#[must_use]
pub fn init_with_file() -> LoggingGuard {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level()));

    let fmt_layer = fmt::layer().with_target(true).with_level(true).json();
    let event_log = event_log::EventLogLayer::new(EVENT_SOURCE);

    // Setup file appender. Si la création du dir échoue, on continue sans
    // file sink mais on logge un warn via les autres sinks.
    let (file_layer, file_guard) = match build_file_appender() {
        Ok((writer, guard)) => {
            let layer = fmt::layer()
                .with_target(true)
                .with_level(true)
                .with_ansi(false)
                .json()
                .with_writer(writer)
                .boxed();
            (Some(layer), Some(guard))
        }
        Err(e) => {
            // On utilise eprintln! ici car tracing n'est pas encore init.
            // Sera silencieux en release (pas de console) — c'est OK,
            // c'est juste un fallback de fallback.
            eprintln!("okvm-logging: file appender disabled: {e}");
            (None, None)
        }
    };

    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .with(event_log);

    let _ = if let Some(file) = file_layer {
        registry.with(file).try_init()
    } else {
        registry.try_init()
    };

    LoggingGuard {
        _file_guard: file_guard,
    }
}

/// Renvoie le chemin du répertoire des logs :
/// `%LocalAppData%\OneClick\OneClickKVM\logs\` sur Windows.
///
/// Respecte `OKVM_INSTANCE` pour permettre l'isolation des logs en mode
/// multi-instance (alice/bob → `OneClickKVM-alice/logs/`).
///
/// # Erreurs
/// Si `ProjectDirs` ne peut pas être déterminé (PC sans `%LocalAppData%`,
/// très improbable).
pub fn log_dir() -> std::io::Result<PathBuf> {
    let suffix = std::env::var("OKVM_INSTANCE")
        .ok()
        .map(|s| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .take(32)
                .collect::<String>()
        })
        .filter(|s| !s.is_empty());
    let app_name = match &suffix {
        Some(s) => format!("OneClickKVM-{s}"),
        None => "OneClickKVM".to_string(),
    };
    let pd = directories::ProjectDirs::from("io", "OneClick", &app_name)
        .ok_or_else(|| std::io::Error::other("ProjectDirs indisponible — pas de %LocalAppData%"))?;
    Ok(pd.data_local_dir().join("logs"))
}

fn build_file_appender(
) -> std::io::Result<(tracing_appender::non_blocking::NonBlocking, WorkerGuard)> {
    let dir = log_dir()?;
    std::fs::create_dir_all(&dir)?;
    let file_appender = tracing_appender::rolling::daily(&dir, "app.log");
    Ok(tracing_appender::non_blocking(file_appender))
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
    }

    #[allow(unsafe_code)]
    impl EventLogLayer {
        fn ensure_handle(&self) {
            self.init.call_once(|| {
                let pcw = PCWSTR(self.source.as_ptr());
                // SAFETY: `pcw` pointe vers un buffer UTF-16 valide owned par self.source.
                let h = unsafe { RegisterEventSourceW(None, pcw) };
                if let Ok(h) = h {
                    *self.handle.lock() = Some(h.0 as isize);
                }
            });
        }

        fn report(&self, level: REPORT_EVENT_TYPE, msg: &str) {
            self.ensure_handle();
            let Some(handle) = *self.handle.lock() else {
                return;
            };
            let mut wide: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
            let strings: [PCWSTR; 1] = [PCWSTR(wide.as_mut_ptr())];
            // SAFETY: handle valide, strings pointe vers buffer UTF-16 valide.
            unsafe {
                let _ = ReportEventW(
                    windows::Win32::Foundation::HANDLE(handle as *mut _),
                    level,
                    0,
                    0,
                    None,
                    0,
                    Some(&strings),
                    None,
                );
            }
        }
    }

    impl Drop for EventLogLayer {
        fn drop(&mut self) {
            if let Some(handle) = self.handle.lock().take() {
                // SAFETY: handle obtenu via RegisterEventSourceW.
                unsafe {
                    let _ =
                        DeregisterEventSource(windows::Win32::Foundation::HANDLE(handle as *mut _));
                }
            }
        }
    }

    impl<S: Subscriber> Layer<S> for EventLogLayer {
        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let lvl = *event.metadata().level();
            let evt_type = match lvl {
                tracing::Level::ERROR => EVENTLOG_ERROR_TYPE,
                tracing::Level::WARN => EVENTLOG_WARNING_TYPE,
                tracing::Level::INFO => EVENTLOG_INFORMATION_TYPE,
                _ => return, // skip Debug/Trace
            };
            // INFO est skip pour ne pas inonder Event Log (le file appender
            // les capture pour le diagnostic détaillé).
            if lvl == tracing::Level::INFO {
                return;
            }
            // Format minimaliste : "{target}: {message-fields}"
            let mut visitor = StringVisitor::default();
            event.record(&mut visitor);
            let msg = format!("{}: {}", event.metadata().target(), visitor.0);
            self.report(evt_type, &msg);
        }
    }

    #[derive(Default)]
    struct StringVisitor(String);

    impl tracing::field::Visit for StringVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if !self.0.is_empty() {
                self.0.push(' ');
            }
            use std::fmt::Write as _;
            let _ = write!(self.0, "{}={:?}", field.name(), value);
        }
    }
}

/// Stub vide pour les plateformes non-Windows.
#[cfg(not(windows))]
pub mod event_log {
    use super::{Context, Event, Layer, Subscriber};

    /// Stub no-op.
    pub struct EventLogLayer;

    impl EventLogLayer {
        /// Stub no-op.
        #[must_use]
        pub fn new(_name: &str) -> Self {
            Self
        }
    }

    impl<S: Subscriber> Layer<S> for EventLogLayer {
        fn on_event(&self, _event: &Event<'_>, _ctx: Context<'_, S>) {}
    }
}
