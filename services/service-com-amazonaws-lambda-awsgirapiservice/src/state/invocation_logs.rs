// SPDX-License-Identifier: CC0-1.0

//! WHERE a function's invocations are logged and IN WHICH FORM: the one reading of
//! `LoggingConfig$LogGroup` and of the default group a function that names none logs to, and
//! of `LoggingConfig$LogFormat` and the format a function that names none is rendered in.
//!
//! `LoggingConfig$LogGroup` is "the name of the Amazon CloudWatch log group the function
//! sends logs to", and "by default, Lambda functions send logs to a default log group named
//! `/aws/lambda/<function name>`" — so a function's log destination is that member when it
//! carries one and [`super::default_log_group`] otherwise.
//!
//! `LoggingConfig$LogFormat` is "the format in which Lambda sends your function's application
//! and system logs to CloudWatch. Select between plain text and structured JSON" — so a
//! function's records are rendered in that member's format when it carries one and in
//! [`super::DEFAULT_LOG_FORMAT`] otherwise.
//!
//! Two places answer with both and must not disagree: what a READ of the function reports as
//! its `LoggingConfig` ([`super::FunctionRecord`]), and where and how a RUN's log is
//! DELIVERED ([`super::execution::log_delivery::log_target`], which also reads what the
//! execution role may do to that group). Both take them from here, so the group, the form and
//! the level filter a client is told about are the group, the form and the filter a client
//! finds the records under.

use super::{LoggingConfig, DEFAULT_LOG_FORMAT, DEFAULT_LOG_LEVEL, LOG_FORMAT_JSON};

/// The CloudWatch log group the invocations of the function `name` are logged to: the one its
/// own `logging` names, and [`super::default_log_group`] when it names none.
///
/// An empty `LogGroup` names no group, so it is the default one too.
#[must_use]
pub fn configured_log_group(logging: Option<&LoggingConfig>, name: &str) -> String {
    logging
        .and_then(|logging| logging.log_group.clone())
        .filter(|group| !group.is_empty())
        .unwrap_or_else(|| super::default_log_group(name))
}

/// The FORMAT the records of the function's invocations are rendered in: the one its own
/// `logging` names, and [`super::DEFAULT_LOG_FORMAT`] when it names none.
///
/// One of the two values the model's `LogFormat` enum declares, which is what a READ of the
/// function reports for the member too.
#[must_use]
pub fn configured_log_format(logging: Option<&LoggingConfig>) -> &'static str {
    let named = logging.and_then(|logging| logging.log_format.as_deref());
    if named == Some(LOG_FORMAT_JSON) {
        LOG_FORMAT_JSON
    } else {
        DEFAULT_LOG_FORMAT
    }
}

/// Whether the function's records are delivered as structured JSON objects rather than in the
/// runtime's plain-text framing — the one question the rendering of a record asks of the
/// format.
#[must_use]
pub fn delivers_json_records(logging: Option<&LoggingConfig>) -> bool {
    configured_log_format(logging) == LOG_FORMAT_JSON
}

/// The LEVEL the application records of the function's invocations are filtered at before
/// Lambda sends them to CloudWatch Logs — `LoggingConfig$ApplicationLogLevel`, which is
/// "set this property to filter the application logs for your function that Lambda sends to
/// CloudWatch. Lambda only sends application logs at the selected level of detail and lower,
/// where `TRACE` is the highest level and `FATAL` is the lowest" — or `None` where the
/// function's records are filtered at no level at all.
///
/// It is the SAME reading a read of the function reports: the level is a filter of the
/// structured format, so a function on that format is filtered at the level it names and at
/// [`super::DEFAULT_LOG_LEVEL`] until it names one, while a function on the plain-text format
/// — which can carry no level ([`super::log_level_format_refusal`]) — is filtered at none.
#[must_use]
pub fn configured_application_log_level(logging: Option<&LoggingConfig>) -> Option<String> {
    configured_level(
        logging,
        logging.and_then(|logging| logging.application_log_level.as_deref()),
    )
}

/// The LEVEL the system records of the function's invocations are filtered at —
/// `LoggingConfig$SystemLogLevel`, "set this property to filter the system logs for your
/// function that Lambda sends to CloudWatch" — read the same way, so a read of the function
/// reports both levels under one rule.
#[must_use]
pub fn configured_system_log_level(logging: Option<&LoggingConfig>) -> Option<String> {
    configured_level(
        logging,
        logging.and_then(|logging| logging.system_log_level.as_deref()),
    )
}

/// The ONE reading of a level member: the level `named`, [`super::DEFAULT_LOG_LEVEL`] while
/// the structured format names none, and no level at all on the plain-text format, which
/// filters by none.
fn configured_level(logging: Option<&LoggingConfig>, named: Option<&str>) -> Option<String> {
    if !delivers_json_records(logging) {
        // A function on the plain-text format reports whatever was stored for the member and
        // nothing defaulted, because that format filters by no level.
        return named.map(str::to_string);
    }
    Some(named.unwrap_or(DEFAULT_LOG_LEVEL).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `LoggingConfig` carrying `log_group`.
    fn logging(log_group: Option<&str>) -> LoggingConfig {
        LoggingConfig {
            application_log_level: None,
            system_log_level: None,
            log_group: log_group.map(str::to_string),
            log_format: None,
        }
    }

    /// One `LoggingConfig` carrying `log_format`.
    fn formatted(log_format: Option<&str>) -> LoggingConfig {
        LoggingConfig {
            application_log_level: None,
            system_log_level: None,
            log_group: None,
            log_format: log_format.map(str::to_string),
        }
    }

    /// One `LoggingConfig` carrying a format and an application log level.
    fn levelled(log_format: &str, application_log_level: &str) -> LoggingConfig {
        LoggingConfig {
            application_log_level: Some(application_log_level.to_string()),
            system_log_level: None,
            log_group: None,
            log_format: Some(log_format.to_string()),
        }
    }

    /// Every form the format member takes, and the format each names.
    #[test]
    fn the_format_is_the_configured_one_and_otherwise_the_documented_default() {
        // (the case, the function's `LoggingConfig`, the format its records are rendered in)
        let rows: Vec<(&str, Option<LoggingConfig>, &str)> = vec![
            ("a function with no LoggingConfig at all", None, "Text"),
            ("a LoggingConfig naming no format", Some(formatted(None)), "Text"),
            (
                "a LoggingConfig naming the structured format",
                Some(formatted(Some("JSON"))),
                "JSON",
            ),
            (
                "a LoggingConfig naming the plain-text format",
                Some(formatted(Some("Text"))),
                "Text",
            ),
            (
                "a LoggingConfig naming a group and no format",
                Some(logging(Some("/custom/orders"))),
                "Text",
            ),
        ];
        for (case, configured, expected) in rows {
            let actual = configured_log_format(configured.as_ref());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the log format {expected:?}, got {actual:?}"
            );
            assert_eq!(
                delivers_json_records(configured.as_ref()),
                expected == LOG_FORMAT_JSON,
                "case {case:?}: the records are JSON exactly when the format is {LOG_FORMAT_JSON}"
            );
        }
    }

    /// RULE: the application records are filtered at the level the function names, at the
    /// documented default while its structured format names none, and at no level at all on
    /// the plain-text format — the same reading a read of the function reports.
    #[test]
    fn the_application_level_is_the_configured_one_and_otherwise_the_documented_default() {
        // (the case, the function's `LoggingConfig`, the level its records are filtered at)
        let rows: Vec<(&str, Option<LoggingConfig>, Option<&str>)> = vec![
            ("a function with no LoggingConfig at all", None, None),
            (
                "a LoggingConfig naming the plain-text format, which filters by no level",
                Some(formatted(Some("Text"))),
                None,
            ),
            (
                "a LoggingConfig naming the structured format and no level",
                Some(formatted(Some("JSON"))),
                Some("INFO"),
            ),
            (
                "a LoggingConfig naming the structured format and a level",
                Some(levelled("JSON", "WARN")),
                Some("WARN"),
            ),
            (
                "one naming the highest level of detail",
                Some(levelled("JSON", "TRACE")),
                Some("TRACE"),
            ),
            (
                "one naming the lowest",
                Some(levelled("JSON", "FATAL")),
                Some("FATAL"),
            ),
        ];
        for (case, configured, expected) in rows {
            let actual = configured_application_log_level(configured.as_ref());
            assert_eq!(
                actual.as_deref(),
                expected,
                "case {case:?}: expected the application log level {expected:?}, got {actual:?}"
            );
        }
    }

    /// Every form the member takes, and the group each names.
    #[test]
    fn the_group_is_the_configured_one_and_otherwise_the_functions_default() {
        // (the case, the function's `LoggingConfig`, the group its invocations are logged to)
        let rows: Vec<(&str, Option<LoggingConfig>, &str)> = vec![
            (
                "a function with no LoggingConfig at all",
                None,
                "/aws/lambda/orders",
            ),
            (
                "a LoggingConfig naming no group",
                Some(logging(None)),
                "/aws/lambda/orders",
            ),
            (
                "a LoggingConfig whose group is the empty string, which names none",
                Some(logging(Some(""))),
                "/aws/lambda/orders",
            ),
            (
                "a LoggingConfig naming another group",
                Some(logging(Some("/custom/orders"))),
                "/custom/orders",
            ),
            (
                "a LoggingConfig naming a group outside the /aws/lambda prefix entirely",
                Some(logging(Some("orders-log-group"))),
                "orders-log-group",
            ),
        ];
        for (case, configured, expected) in rows {
            let actual = configured_log_group(configured.as_ref(), "orders");
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the log group {expected:?}, got {actual:?}"
            );
        }
    }
}
