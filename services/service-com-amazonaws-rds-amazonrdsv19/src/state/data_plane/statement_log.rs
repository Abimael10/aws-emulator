// SPDX-License-Identifier: CC0-1.0

//! The STATEMENT the engine ran, written to the engine's own log.
//!
//! Each engine logs the statements it executes only when its own statement logging is
//! configured to, and writes them in the log that engine keeps them in:
//!
//! * PostgreSQL logs a statement when `log_statement` covers its class — the parameter's
//!   published values are `none`, `ddl`, `mod` and `all` — or when its execution time
//!   reaches `log_min_duration_statement`, which a value of zero makes every statement do
//!   ("Setting this to zero prints all statement durations"). The line goes to the server
//!   log, the file this instance's `DescribeDBLogFiles` already lists, at the `LOG` level
//!   PostgreSQL writes `statement:` lines at.
//! * The MySQL family logs every statement while `general_log` is on ("enables the general
//!   query log"), and the general query log is a FILE OF ITS OWN rather than part of the
//!   error log, which is why the line goes to the instance's general-log file
//!   ([`crate::state::engine_log::MYSQL_GENERAL_LOG_FILE`]) and is rendered the way that log
//!   writes a line: the
//!   instant, the connection id, the command and the statement.
//!
//! An engine whose statement logging is off writes no such line, so a client that has not
//! turned it on reads a log of the server's own messages and nothing else — which is what
//! the default parameter values ([`crate::ops::describe_engine_default_parameters`]: `none`,
//! `-1`, `0`) mean.

use super::log_export::{LogLine, Severity};
use super::wire_session::{SessionContext, SessionFacts, SessionTarget};
use super::EngineWire;

/// The line a PostgreSQL server writes for a statement it ran, or `None` when
/// `log_statement` and `log_min_duration_statement` do not log it.
///
/// For the MySQL family this is always `None`: its statements go to the general query log
/// instead ([`general_log_line`]).
#[must_use]
pub fn server_log_line(facts: &SessionFacts, verb: &str, sql: &str) -> Option<LogLine> {
    if facts.wire != EngineWire::Postgres || !postgres_logs_statements(facts, verb) {
        return None;
    }
    Some(LogLine::new(Severity::Log, format!("statement: {sql}")))
}

/// Whether this PostgreSQL session's configuration logs a statement whose command is `verb`.
fn postgres_logs_statements(facts: &SessionFacts, verb: &str) -> bool {
    // "Setting this to zero prints all statement durations": every statement's execution
    // time reaches a minimum of none, so every statement is logged.
    if facts
        .setting("log_min_duration_statement")
        .and_then(|value| value.trim().parse::<i64>().ok())
        == Some(0)
    {
        return true;
    }
    let setting = facts
        .setting("log_statement")
        .unwrap_or_else(|| "none".to_string());
    match setting.trim().to_ascii_lowercase().as_str() {
        "all" => true,
        // "mod" logs the data-definition statements and the ones that change data.
        "mod" => is_data_definition(verb) || super::read_only::verb_writes(verb),
        "ddl" => is_data_definition(verb),
        _ => false,
    }
}

/// Whether `verb` names a data-definition statement, the class `log_statement` `ddl` logs.
///
/// Read off the command NAME the statement's answer reports, as the writing-command
/// catalogue beside it is ([`super::read_only::verb_writes`]).
fn is_data_definition(verb: &str) -> bool {
    let command = verb
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(
        command.as_str(),
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "COMMENT" | "GRANT" | "REVOKE"
    )
}

/// The line the MySQL general query log carries for a statement this session ran, or `None`
/// while `general_log` is off.
///
/// The format is the general log's own: the instant, the connection id, the command and the
/// statement, which is how `mysqld` writes an entry.
#[must_use]
pub fn general_log_line(facts: &SessionFacts, sql: &str) -> Option<String> {
    if facts.wire != EngineWire::MySql || !mysql_general_log_is_on(facts) {
        return None;
    }
    Some(format!(
        "{}\t{} Query\t{sql}",
        super::log_export::mysql_instant(::std::time::SystemTime::now()),
        facts.connection_id,
    ))
}

/// Whether `general_log` is on for this session's instance: the parameter is a boolean whose
/// published values are `0` and `1`, and a client may also write it as `ON`.
fn mysql_general_log_is_on(facts: &SessionFacts) -> bool {
    facts
        .setting("general_log")
        .is_some_and(|value| super::read_only::setting_is_on(value.trim()))
}

/// Write `line` into the general query log of the DB instance this session is on.
///
/// Does nothing for an endpoint of any other resource: the log files this service serves are
/// a DB INSTANCE's ([`crate::ops::describe_db_log_files`]).
pub fn record_general_log(context: &SessionContext, line: String) {
    let SessionTarget::DbInstance {
        region,
        resource_id,
    } = &context.target
    else {
        return;
    };
    let mut store = context.state.lock();
    if let Some(record) = store.instance_by_resource_id_mut(region, resource_id) {
        record.general_query_log.record_engine_line(line);
    }
}
