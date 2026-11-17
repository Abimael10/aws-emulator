// SPDX-License-Identifier: CC0-1.0

//! The DB instance log files `DescribeDBLogFiles` lists and `DownloadDBLogFilePortion`
//! serves.
//!
//! What a file holds is what was written to it: the lifecycle lines THIS SERVICE keeps for a
//! DB instance — one per event it actually performed on that instance (the create it
//! accepted, each status the clock then made due) — and the lines the instance's own ENGINE
//! wrote while serving sessions on its endpoint, each rendered the way that engine writes one
//! ([`super::data_plane::log_export`]). That is real, stored state — a line appears only
//! because the service or the engine did the thing it records — and it is what the two log
//! operations report: the file's size is the size of those lines, and its last-written time is
//! the instant of the last one.
//!
//! An instance has the server log its engine writes ([`log_file_name`]), and the MySQL family
//! also has the GENERAL QUERY LOG, a file of its own that carries the statements the server
//! ran while `general_log` was on ([`MYSQL_GENERAL_LOG_FILE`]); a file nothing has written to
//! is not one the instance has.
//!
//! Each log lives ON the DB instance record, so it is removed with the instance and is
//! bounded by [`MAX_LOG_ENTRIES`] lines of at most [`MAX_LOG_LINE_LEN`] characters each.

/// How many lines one instance's log keeps. A DB instance's lifecycle produces a line per
/// accepted change, and a caller can drive those without limit, so the log is a ring: past
/// this many lines the oldest goes, exactly as a real engine log rotates.
pub const MAX_LOG_ENTRIES: usize = 64;

/// The longest log line this service writes. Every line is built from a status or an
/// identifier this service already bounds, so this is a backstop rather than a filter.
pub const MAX_LOG_LINE_LEN: usize = 256;

/// The file the MySQL family's GENERAL QUERY LOG is written to, as Amazon RDS names it in
/// `DescribeDBLogFiles`.
///
/// Source: the per-engine log files Amazon RDS publishes in "Monitoring Amazon RDS log
/// files" — the MySQL general query log is `general/mysql-general.log`, beside the
/// `error/mysql-error.log` of the same engine ([`log_file_name`]). A separate file because
/// that is what it is on the server: the statements the engine ran are not entries of its
/// error log.
pub const MYSQL_GENERAL_LOG_FILE: &str = "general/mysql-general.log";

/// One line of a DB instance's log: when this service recorded it, and the line itself.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// When the event this line records happened.
    pub at: ::aws_smithy_types::DateTime,
    /// The line as a client downloading the file reads it.
    pub line: String,
}

/// A DB instance's log: the lines this service and its engine have recorded for it, oldest
/// first.
#[derive(Debug, Clone, Default)]
pub struct InstanceLog {
    /// The lines, oldest first, at most [`MAX_LOG_ENTRIES`] of them.
    pub entries: Vec<LogEntry>,
}

impl InstanceLog {
    /// Record one line of this SERVICE's own: the instant it happened, then what happened,
    /// which is how a line of a lifecycle event reads.
    pub fn record(&mut self, text: impl Into<String>) {
        let at = super::now_as_wire_timestamp();
        let instant = at
            .fmt(::aws_smithy_types::date_time::Format::DateTime)
            .unwrap_or_else(|_| at.as_secs_f64().to_string());
        let text: String = text.into();
        self.push(at, format!("{instant} {text}"));
    }

    /// Record one line the ENGINE wrote, which already carries the prefix that engine writes
    /// in front of a line (its instant, the session it was written for, the severity) — so
    /// the file holds the line as the engine wrote it rather than one this service re-stamped
    /// ([`super::data_plane::log_export::rendered`]).
    pub fn record_engine_line(&mut self, line: impl Into<String>) {
        self.push(super::now_as_wire_timestamp(), line.into());
    }

    /// Append one line, dropping the oldest when the log is full.
    fn push(&mut self, at: ::aws_smithy_types::DateTime, line: String) {
        let mut line = line;
        line.truncate(
            line.char_indices()
                .nth(MAX_LOG_LINE_LEN)
                .map_or(line.len(), |(offset, _)| offset),
        );
        self.entries.push(LogEntry { at, line });
        if self.entries.len() > MAX_LOG_ENTRIES {
            self.entries.remove(0);
        }
    }

    /// The file's lines as a client downloading it reads them.
    pub fn lines(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.line.clone())
            .collect()
    }

    /// `DescribeDBLogFilesDetails$Size`: "The size, in bytes, of the log file" — the size
    /// of the lines above, each with its newline, so it cannot disagree with what a
    /// download returns.
    pub fn size(&self) -> i64 {
        let bytes: usize = self
            .lines()
            .iter()
            .map(|line| line.len() + "\n".len())
            .sum();
        i64::try_from(bytes).unwrap_or(i64::MAX)
    }

    /// `DescribeDBLogFilesDetails$LastWritten`: "A POSIX timestamp when the last log entry
    /// was written", which Amazon RDS reports in MILLISECONDS, or `None` for a log with no
    /// lines yet.
    pub fn last_written(&self) -> Option<i64> {
        self.entries
            .last()
            .map(|entry| (entry.at.as_secs_f64() * 1_000.0) as i64)
    }
}

/// The name of the log file a DB instance running `engine` writes.
///
/// Source: the per-engine log files Amazon RDS publishes in "Monitoring Amazon RDS log
/// files" (Amazon RDS User Guide) and lists through `DescribeDBLogFiles` — the MySQL and
/// MariaDB error log `error/mysql-error.log`, the PostgreSQL one under `error/`, the SQL
/// Server `log/ERRORLOG`, the Oracle alert log under `trace/` and the Db2 `db2diag.log`.
/// An engine this table does not carry gets the neutral name below rather than another
/// engine's: the file is this service's own log either way (see the module docs).
pub fn log_file_name(engine: &str) -> &'static str {
    /// The published names, matched on the prefix of the `Engine` value, longest-matching
    /// family first.
    const PUBLISHED: &[(&str, &str)] = &[
        ("aurora-mysql", "error/mysql-error.log"),
        ("aurora-postgresql", "error/postgresql.log"),
        ("mariadb", "error/mysql-error.log"),
        ("mysql", "error/mysql-error.log"),
        ("postgres", "error/postgresql.log"),
        ("custom-sqlserver", "log/ERRORLOG"),
        ("sqlserver", "log/ERRORLOG"),
        ("custom-oracle", "trace/alert.log"),
        ("oracle", "trace/alert.log"),
        ("db2", "db2diag.log"),
    ];

    PUBLISHED
        .iter()
        .find(|(family, _)| engine.starts_with(family))
        .map_or("error/engine.log", |(_, name)| *name)
}
