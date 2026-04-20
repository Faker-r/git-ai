use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::metrics::MetricEvent;

pub mod wrapper_performance_targets;

/// Maximum events per metrics envelope
pub const MAX_METRICS_PER_ENVELOPE: usize = 250;

/// Initialize a `tracing_subscriber` for non-daemon processes that appends logs to
/// `~/.git-ai/internal/git-ai.log` (best-effort).
pub fn init_process_file_logging() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // The daemon sets up its own subscriber + log file; don't fight it.
        if crate::daemon::daemon_process_active() {
            return;
        }

        // Avoid writing files in test harnesses (keeps tests hermetic).
        if std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
            || std::env::var_os("GITAI_TEST_DB_PATH").is_some()
        {
            return;
        }

        let Some(internal_dir) = crate::config::internal_dir_path() else {
            return;
        };
        let log_path: PathBuf = internal_dir.join("git-ai.log");
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Ensure the file exists even if no tracing events are emitted.
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path);

        use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

        // Logging levels:
        // - If `RUST_LOG` is set, respect it.
        // - Else if `GIT_AI_DEBUG=1`, enable debug globally.
        // - Else default to `git_ai=debug,info` (capture our crate's debug logs).
        let env_filter = if std::env::var_os("RUST_LOG").is_some() {
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
        } else if std::env::var("GIT_AI_DEBUG").as_deref() == Ok("1") {
            EnvFilter::new("debug")
        } else {
            EnvFilter::new("git_ai=debug,info")
        };

        // Reopen the file per event; if open fails, fall back to a sink.
        let make_writer = {
            use tracing_subscriber::fmt::writer::BoxMakeWriter;
            let log_path = log_path.clone();
            BoxMakeWriter::new(move || {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                    .map(|f| Box::new(f) as Box<dyn std::io::Write + Send>)
                    .unwrap_or_else(|_| Box::new(std::io::sink()))
            })
        };

        let init_result = tracing_subscriber::registry()
            .with(env_filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_thread_ids(false)
                    .with_ansi(false)
                    .with_writer(make_writer),
            )
            .try_init();

        // If another subscriber was already installed, do nothing.
        let _ = init_result;
    });
}

/// Submit telemetry envelopes via the best available path:
/// 1. External daemon control socket (wrapper processes)
/// 2. In-process daemon telemetry worker (daemon process itself)
/// 3. Silently drop if neither is available
fn submit_telemetry_envelope(envelopes: Vec<crate::daemon::TelemetryEnvelope>) {
    if crate::daemon::telemetry_handle::daemon_telemetry_available() {
        crate::daemon::telemetry_handle::submit_telemetry(envelopes);
    } else if crate::daemon::daemon_process_active() {
        crate::daemon::telemetry_worker::submit_daemon_internal_telemetry(envelopes);
    }
}

/// Log an error to Sentry (via daemon telemetry worker)
pub fn log_error(error: &dyn std::error::Error, context: Option<serde_json::Value>) {
    let envelope = crate::daemon::TelemetryEnvelope::Error {
        timestamp: chrono::Utc::now().to_rfc3339(),
        message: error.to_string(),
        context,
    };
    submit_telemetry_envelope(vec![envelope]);
}

/// Log a performance metric to Sentry (via daemon telemetry worker)
pub fn log_performance(
    operation: &str,
    duration: Duration,
    context: Option<serde_json::Value>,
    tags: Option<HashMap<String, String>>,
) {
    let envelope = crate::daemon::TelemetryEnvelope::Performance {
        timestamp: chrono::Utc::now().to_rfc3339(),
        operation: operation.to_string(),
        duration_ms: duration.as_millis(),
        context,
        tags,
    };
    submit_telemetry_envelope(vec![envelope]);
}

/// Log a message to Sentry (info, warning, etc.) (via daemon telemetry worker)
#[allow(dead_code)]
pub fn log_message(message: &str, level: &str, context: Option<serde_json::Value>) {
    let envelope = crate::daemon::TelemetryEnvelope::Message {
        timestamp: chrono::Utc::now().to_rfc3339(),
        message: message.to_string(),
        level: level.to_string(),
        context,
    };
    submit_telemetry_envelope(vec![envelope]);
}

/// Log a batch of metric events (via daemon telemetry worker).
///
/// Events are batched into envelopes of up to 250 events each.
pub fn log_metrics(
    #[cfg_attr(any(test, feature = "test-support"), allow(unused))] events: Vec<MetricEvent>,
) {
    #[cfg(any(test, feature = "test-support"))]
    return;

    #[cfg(not(any(test, feature = "test-support")))]
    {
        if events.is_empty() {
            return;
        }

        // Split into chunks of MAX_METRICS_PER_ENVELOPE
        for chunk in events.chunks(MAX_METRICS_PER_ENVELOPE) {
            let envelope = crate::daemon::TelemetryEnvelope::Metrics {
                events: chunk.to_vec(),
            };
            submit_telemetry_envelope(vec![envelope]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    // Test error logging
    #[test]
    fn test_log_error_no_panic() {
        use std::io;
        let error = io::Error::new(io::ErrorKind::NotFound, "test error");
        log_error(&error, None);
    }

    #[test]
    fn test_log_error_with_context() {
        use serde_json::json;
        use std::io;
        let error = io::Error::new(io::ErrorKind::PermissionDenied, "access denied");
        let context = json!({"file": "test.txt", "operation": "read"});
        log_error(&error, Some(context));
    }

    // Test performance logging
    #[test]
    fn test_log_performance_basic() {
        log_performance("test_operation", Duration::from_millis(100), None, None);
    }

    #[test]
    fn test_log_performance_with_context() {
        use serde_json::json;
        let context = json!({"files": 5, "lines": 100});
        log_performance("test_op", Duration::from_secs(1), Some(context), None);
    }

    #[test]
    fn test_log_performance_with_tags() {
        let mut tags = HashMap::new();
        tags.insert("command".to_string(), "commit".to_string());
        tags.insert("repo".to_string(), "test".to_string());
        log_performance("commit_op", Duration::from_millis(500), None, Some(tags));
    }

    // Test message logging
    #[test]
    fn test_log_message_basic() {
        log_message("test message", "info", None);
    }

    #[test]
    fn test_log_message_with_context() {
        use serde_json::json;
        let context = json!({"user": "test", "action": "login"});
        log_message("user logged in", "info", Some(context));
    }

    #[test]
    fn test_log_message_warning() {
        log_message("warning message", "warning", None);
    }

    // Test metrics logging
    #[test]
    fn test_log_metrics_empty() {
        log_metrics(vec![]);
    }

    // Test constants
    #[test]
    fn test_max_metrics_per_envelope() {
        assert_eq!(MAX_METRICS_PER_ENVELOPE, 250);
    }
}
