//! Application logging (§44).
//!
//! Logging must help debugging without leaking credentials or database content, so this module is
//! the single place that decides what may be written:
//!
//! * Passwords never reach a log. [`SecretString`](crate::config::SecretString) redacts itself in
//!   both `Debug` and `Display`, and nothing here formats a connection URL.
//! * Result rows are never logged — call sites log counts and durations instead.
//! * SQL statement text is logged only when explicitly enabled, because the SQL itself may carry
//!   sensitive values.

use std::env;

const LEVEL_VARIABLE: &str = "RUSTY_SQL_LOG";
const SQL_VARIABLE: &str = "RUSTY_SQL_LOG_SQL";
const DEFAULT_DIRECTIVE: &str = "info";

/// Stands in for a statement whenever SQL logging has not been enabled.
const WITHHELD: &str = "[SQL withheld]";

/// Installs the stderr subscriber. Calling it more than once is harmless — the second call finds a
/// subscriber already set and leaves it alone, which keeps tests that install their own working.
pub fn init() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(env_filter(env::var(LEVEL_VARIABLE).ok().as_deref()))
        .with_writer(std::io::stderr)
        .try_init();
    // Confirms at a glance that logging is wired and at what verbosity, which is otherwise only
    // discoverable by provoking a database operation.
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        sql_logging = sql_logging_enabled(),
        "starting"
    );
}

/// The configured filter. An unparseable directive is a typo in an environment variable; building
/// the filter leniently would silently drop it and leave an error-only filter, quietly suppressing
/// the logging the operator was trying to turn on, so an unusable directive falls back to the
/// default instead.
fn env_filter(raw: Option<&str>) -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_new(filter_directive(raw))
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_DIRECTIVE))
}

/// The filter to apply, defaulting to `info` when the variable is unset or blank.
fn filter_directive(raw: Option<&str>) -> String {
    raw.map(str::trim)
        .filter(|directive| !directive.is_empty())
        .unwrap_or(DEFAULT_DIRECTIVE)
        .to_owned()
}

/// Whether the operator has opted in to logging SQL statement text.
fn sql_logging_from(raw: Option<&str>) -> bool {
    raw.map(|value| value.trim().to_ascii_lowercase())
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
}

/// Whether SQL statement text may be logged, as configured for this process.
pub fn sql_logging_enabled() -> bool {
    sql_logging_from(env::var(SQL_VARIABLE).ok().as_deref())
}

/// The text to log for a statement — the SQL itself only where that has been enabled (§44).
pub fn statement(sql: &str) -> &str {
    statement_text(sql, sql_logging_enabled())
}

/// The text to log in place of a statement.
fn statement_text(sql: &str, enabled: bool) -> &str {
    if enabled { sql } else { WITHHELD }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_level_defaults_to_info() {
        assert_eq!(filter_directive(None), "info");
    }

    #[test]
    fn a_blank_level_defaults_to_info() {
        assert_eq!(filter_directive(Some("   ")), "info");
    }

    #[test]
    fn an_explicit_level_is_honoured() {
        assert_eq!(filter_directive(Some("debug")), "debug");
    }

    #[test]
    fn a_per_target_directive_is_passed_through() {
        assert_eq!(
            filter_directive(Some(" rusty_sql_tool=trace,tokio_postgres=warn ")),
            "rusty_sql_tool=trace,tokio_postgres=warn"
        );
    }

    /// Built leniently, an unusable directive is dropped and the filter degrades to error-only,
    /// silently suppressing the logging the operator was asking for.
    #[test]
    fn an_unparseable_directive_falls_back_to_the_default() {
        assert_eq!(
            env_filter(Some("rusty_sql_tool=verbose")).to_string(),
            "info"
        );
    }

    #[test]
    fn a_usable_directive_reaches_the_filter() {
        assert_eq!(env_filter(Some("warn")).to_string(), "warn");
    }

    #[test]
    fn sql_logging_is_off_unless_it_is_asked_for() {
        assert!(!sql_logging_from(None));
        assert!(!sql_logging_from(Some("")));
        assert!(!sql_logging_from(Some("0")));
        assert!(!sql_logging_from(Some("false")));
    }

    #[test]
    fn sql_logging_accepts_the_usual_affirmatives() {
        for value in ["1", "true", "TRUE", "yes", "on", " true "] {
            assert!(sql_logging_from(Some(value)), "{value:?} should enable it");
        }
    }

    #[test]
    fn a_statement_is_withheld_unless_sql_logging_is_enabled() {
        assert_eq!(
            statement_text("SELECT * FROM patient WHERE nhs_number = '123'", false),
            WITHHELD
        );
    }

    #[test]
    fn a_statement_is_logged_in_full_once_enabled() {
        assert_eq!(statement_text("SELECT 1", true), "SELECT 1");
    }
}
