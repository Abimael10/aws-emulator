// SPDX-License-Identifier: CC0-1.0

//! The engine's own LOG, and the CloudWatch Logs group a resource with log exports turned
//! on streams it to.
//!
//! `EnableCloudwatchLogsExports` is "the list of log types that need to be enabled for
//! exporting to CloudWatch Logs", so a resource created with one is a resource whose ENGINE
//! LOG a client reads in CloudWatch Logs — not a list this service merely stores. The two
//! halves of that are here:
//!
//! * WHAT the engine writes. A server message carries a SEVERITY, and the engine writes to
//!   its log every message at or above the severity its own configuration logs at
//!   ([`Severity::written_to_the_server_log`]): a statement it refused (`ERROR`), a
//!   condition it warned about (`WARNING`), and the messages a `RAISE` of a code block
//!   produces ([`super::pg_code_block`]). The line itself is rendered the way that engine
//!   writes one ([`rendered`]).
//! * WHERE it goes. Amazon RDS publishes the group per RESOURCE and log type —
//!   `/aws/rds/cluster/<DBClusterIdentifier>/<log type>` for a DB cluster
//!   ([`cluster_log_group`]) and `/aws/rds/instance/<DBInstanceIdentifier>/<log type>` for a
//!   DB instance ([`instance_log_group`]) — and names the STREAM after the DB instance whose
//!   engine wrote the line, which is why one Aurora cluster's group carries a stream per
//!   member. The log type the engine's error log is exported under is the engine's own
//!   ([`engine_log_type`]: `postgresql` for the PostgreSQL engines, `error` for the MySQL
//!   family).
//!
//! The write goes in through CloudWatch Logs' OWN ingest path, over the in-process callback
//! this listener was opened with (`service_common::cloudwatch_logs::delivered_over`), so a
//! delivered line is an ingestion into the group: its metric filters, its subscription
//! filters and its readers (`FilterLogEvents`, `GetLogEvents`) all see exactly what the
//! engine wrote, and a metric filter counting the group's `ERROR` lines counts the lines
//! this module delivered for refused statements. The group is created by the first line
//! delivered into it, which is when Amazon RDS creates it too: the group of an export
//! appears once log data is published to it.
//!
//! Delivery is BEST EFFORT and never fatal to the session: an emulator with no CloudWatch
//! Logs mounted beside this service, or a write CloudWatch Logs refuses, is reported in this
//! service's own tracing, and the statement's own answer to the client stands either way.
//!
//! A resource that has turned NO log type on is delivered nothing at all, which is what
//! makes an export observably different from none.

use std::time::SystemTime;

use super::wire_session::{SessionContext, SessionFacts, SessionTarget};
use super::{EndpointRole, EngineWire};
use crate::state::{ComAmazonawsRdsAmazonrdsv19State, DbClusterRecord};

/// The log type the PostgreSQL engines' server log is exported under, as
/// `EnableCloudwatchLogsExports` names it.
pub const POSTGRESQL_LOG_TYPE: &str = "postgresql";

/// The log type the MySQL family's error log is exported under, under the same member.
pub const MYSQL_ERROR_LOG_TYPE: &str = "error";

/// How many lines ONE statement delivers. A statement produces a line per message the
/// server wrote about it, which a code block's `RAISE` statements can drive, so the delivery
/// is bounded here rather than by what the client sent.
pub const MAX_LINES_PER_DELIVERY: usize = 32;

/// The longest log line delivered, in characters. A line carries a message the client
/// supplied (a `RAISE` format, a refused statement's own sentence), so it is bounded.
pub const MAX_LINE_CHARS: usize = 4096;

/// The log type the error log of an engine speaking `wire` is exported under.
#[must_use]
pub fn engine_log_type(wire: EngineWire) -> &'static str {
    match wire {
        EngineWire::Postgres => POSTGRESQL_LOG_TYPE,
        EngineWire::MySql => MYSQL_ERROR_LOG_TYPE,
    }
}

/// The CloudWatch Logs group a DB CLUSTER's `log_type` is exported to:
/// `/aws/rds/cluster/<DBClusterIdentifier>/<log type>`, the name Amazon RDS publishes for it.
#[must_use]
pub fn cluster_log_group(cluster_identifier: &str, log_type: &str) -> String {
    format!("/aws/rds/cluster/{cluster_identifier}/{log_type}")
}

/// The CloudWatch Logs group a DB INSTANCE's `log_type` is exported to:
/// `/aws/rds/instance/<DBInstanceIdentifier>/<log type>`, the sibling name for an instance.
///
/// Which group a session's lines go to is [`destination`]: a standalone DB instance exports
/// its engine log to this group under its own `EnabledCloudwatchLogsExports`, while a MEMBER
/// instance of a DB cluster exports to its CLUSTER's group ([`cluster_log_group`]), which is
/// where Amazon RDS puts an Aurora member's engine log.
#[must_use]
pub fn instance_log_group(instance_identifier: &str, log_type: &str) -> String {
    format!("/aws/rds/instance/{instance_identifier}/{log_type}")
}

/// The severity a server message carries, which decides whether the engine writes it to its
/// log and whether the client is told about it.
///
/// The values are PostgreSQL's own message levels; the MySQL error log's three labels are
/// rendered from the same set ([`rendered`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Debug,
    Log,
    Info,
    Notice,
    Warning,
    Error,
    Fatal,
    Panic,
}

impl Severity {
    /// The label a log line and a protocol message carry for this severity.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Log => "LOG",
            Self::Info => "INFO",
            Self::Notice => "NOTICE",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
            Self::Fatal => "FATAL",
            Self::Panic => "PANIC",
        }
    }

    /// The severity `value` names, or `None` for a value that names none.
    ///
    /// The `DEBUG1`..`DEBUG5` levels are one severity here: they differ only in how much a
    /// debugging build says, and this server writes no debugging messages at all.
    #[must_use]
    pub fn named(value: &str) -> Option<Self> {
        let lowered = value.trim().to_ascii_lowercase();
        Some(match lowered.as_str() {
            "debug" | "debug1" | "debug2" | "debug3" | "debug4" | "debug5" => Self::Debug,
            "log" => Self::Log,
            "info" => Self::Info,
            "notice" => Self::Notice,
            "warning" => Self::Warning,
            "error" => Self::Error,
            "fatal" => Self::Fatal,
            "panic" => Self::Panic,
            _ => return None,
        })
    }

    /// This severity's rank in the order `log_min_messages` is read in, where `LOG` sorts
    /// ABOVE `WARNING` — the order PostgreSQL documents for that parameter, which is why a
    /// `RAISE LOG` is written to the log of a server whose `log_min_messages` is the default
    /// `WARNING` while a `RAISE NOTICE` is not.
    fn log_rank(self) -> u8 {
        match self {
            Self::Debug => 1,
            Self::Info => 2,
            Self::Notice => 3,
            Self::Warning => 4,
            Self::Error => 5,
            Self::Log => 6,
            Self::Fatal => 7,
            Self::Panic => 8,
        }
    }

    /// This severity's rank in the order `client_min_messages` is read in, where `LOG` sorts
    /// BELOW `NOTICE` — the other of the two orders PostgreSQL documents, which is why the
    /// same `RAISE LOG` the log carries is not sent to a client at the default `NOTICE`.
    fn client_rank(self) -> u8 {
        match self {
            Self::Debug => 1,
            Self::Log => 2,
            Self::Notice => 3,
            Self::Warning => 4,
            Self::Error => 5,
            Self::Fatal => 6,
            Self::Panic => 7,
            // "INFO level messages are always sent to the client", whatever the setting is.
            Self::Info => u8::MAX,
        }
    }

    /// Whether the engine WRITES a message of this severity to its log.
    ///
    /// PostgreSQL logs every message at or above `log_min_messages`, whose default is
    /// `WARNING`; the value in force for this session is the one the resource's parameter
    /// group sets, so a client that raises the floor is logged accordingly. The MySQL family
    /// has no such parameter: its error log carries the errors and the warnings the server
    /// produced.
    #[must_use]
    pub fn written_to_the_server_log(self, facts: &SessionFacts) -> bool {
        match facts.wire {
            EngineWire::Postgres => {
                let floor = facts
                    .setting("log_min_messages")
                    .as_deref()
                    .and_then(Self::named)
                    .unwrap_or(Self::Warning);
                self.log_rank() >= floor.log_rank()
            }
            EngineWire::MySql => {
                matches!(self, Self::Warning | Self::Error | Self::Fatal | Self::Panic)
            }
        }
    }

    /// Whether the message is also SENT TO THE CLIENT, as a `NoticeResponse` of its own
    /// severity: PostgreSQL sends every message at or above `client_min_messages`, whose
    /// default is `NOTICE`.
    #[must_use]
    pub fn reported_to_the_client(self, facts: &SessionFacts) -> bool {
        let floor = facts
            .setting("client_min_messages")
            .as_deref()
            .and_then(Self::named)
            .unwrap_or(Self::Notice);
        self.client_rank() >= floor.client_rank()
    }
}

/// One message the server wrote about a statement: its severity and its sentence.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub severity: Severity,
    pub message: String,
}

impl LogLine {
    /// A message of `severity`, reading `message`.
    #[must_use]
    pub fn new(severity: Severity, message: impl Into<String>) -> Self {
        Self {
            severity,
            message: message.into(),
        }
    }

    /// The line the engine writes for a statement it REFUSED, which carries the refusal's
    /// own sentence — the very sentence the client was told.
    #[must_use]
    pub fn of_refusal(error: &super::sql_engine::EngineError) -> Self {
        Self::new(Severity::Error, error.message.clone())
    }
}

/// One log line as the engine WRITES it, at the instant `at`.
///
/// PostgreSQL prefixes each line with its `log_line_prefix`, which Amazon RDS ships as
/// `%t:%r:%u@%d:[%p]:` — the instant, the client the session is from, the user and database
/// it is on and the backend's pid — followed by the severity and the message. The MySQL
/// family writes the instant, the connection id and the severity's own label in brackets.
/// Either way the line is built from this session's own facts, so nothing in it is invented.
#[must_use]
pub fn rendered(facts: &SessionFacts, at: SystemTime, line: &LogLine) -> String {
    let message = truncated(&line.message);
    match facts.wire {
        EngineWire::Postgres => format!(
            "{}:{}:{}@{}:[{}]:{}:  {message}",
            postgres_instant(at),
            facts.endpoint_host,
            facts.user,
            facts.database,
            facts.connection_id,
            line.severity.label(),
        ),
        EngineWire::MySql => format!(
            "{} {} [{}] {message}",
            mysql_instant(at),
            facts.connection_id,
            mysql_label(line.severity),
        ),
    }
}

/// `message`, cut to [`MAX_LINE_CHARS`] characters on a character boundary.
fn truncated(message: &str) -> String {
    match message.char_indices().nth(MAX_LINE_CHARS) {
        Some((at, _)) => message[..at].to_string(),
        None => message.to_string(),
    }
}

/// The instant as PostgreSQL's `%t` prints it: the date and time to the second, with the
/// time zone the server logs in, which is UTC here.
fn postgres_instant(at: SystemTime) -> String {
    let rendered = super::pg_server::timestamptz_text(epoch_seconds(at));
    let to_the_second = rendered
        .split_once('.')
        .map_or(rendered.as_str(), |(whole, _)| whole);
    format!("{to_the_second} UTC")
}

/// The instant as the MySQL error log prints it: the ISO 8601 UTC form its
/// `log_timestamps=UTC` default writes — the same instant form its general query log writes
/// ([`super::statement_log`]).
pub(super) fn mysql_instant(at: SystemTime) -> String {
    let instant = ::aws_smithy_types::DateTime::from_secs_f64(epoch_seconds(at));
    instant
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|_| epoch_seconds(at).to_string())
}

/// `at` as seconds since the Unix epoch; an instant before it, which no clock here produces,
/// reads as the epoch.
fn epoch_seconds(at: SystemTime) -> f64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default()
}

/// The label the MySQL error log writes for a severity: it carries errors, warnings and
/// notes rather than PostgreSQL's eight levels.
fn mysql_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Error | Severity::Fatal | Severity::Panic => "ERROR",
        Severity::Warning => "Warning",
        _ => "Note",
    }
}

/// WHERE one resource's engine log is exported to, read off this service's own store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportDestination {
    /// The Region the group lives in: the Region of the resource whose log it is.
    pub region: String,
    /// The log group Amazon RDS publishes that resource's log type under.
    pub log_group: String,
    /// The stream inside it: the DB instance whose engine wrote the lines.
    pub log_stream: String,
}

/// The destination the engine log of the resource this session is on is exported to, or
/// `None` when nothing is exported.
///
/// `None` covers every case in which there is no export: a resource this store no longer
/// holds, and one whose `EnabledCloudwatchLogsExports` does not carry this engine's log type
/// — the DB CLUSTER's configuration for a member instance of a cluster, whose engine log
/// belongs to its cluster's group, and the DB INSTANCE's own for a standalone instance.
///
/// Read under the store's guard by the caller, so the delivery itself runs with none held.
#[must_use]
pub fn destination(
    store: &ComAmazonawsRdsAmazonrdsv19State,
    target: &SessionTarget,
    role: &EndpointRole,
    wire: EngineWire,
) -> Option<ExportDestination> {
    let log_type = engine_log_type(wire);
    match target {
        SessionTarget::DbCluster {
            region,
            resource_id,
        } => {
            let cluster = store.cluster_by_resource_id(region, resource_id)?;
            exported(cluster, log_type)?;
            Some(ExportDestination {
                region: cluster.region.clone(),
                log_group: cluster_log_group(&cluster.identifier, log_type),
                log_stream: stream_of(store, cluster, role),
            })
        }
        SessionTarget::DbInstance {
            region,
            resource_id,
        } => {
            let instance = store.instance_by_resource_id(region, resource_id)?;
            // A MEMBER instance of a DB cluster exports to its CLUSTER's group, which is
            // where Amazon RDS puts an Aurora member's engine log, under the configuration
            // the CLUSTER carries; its stream is the member that wrote the line.
            if let Some(cluster) = instance
                .db_cluster_identifier
                .as_deref()
                .and_then(|named| store.resolve_db_cluster(&instance.region, named))
            {
                exported(cluster, log_type)?;
                return Some(ExportDestination {
                    region: cluster.region.clone(),
                    log_group: cluster_log_group(&cluster.identifier, log_type),
                    log_stream: instance.identifier.clone(),
                });
            }
            // A standalone DB instance exports its own engine log to its own group, under
            // its own `EnabledCloudwatchLogsExports`.
            instance
                .enabled_cloudwatch_logs_exports
                .iter()
                .any(|enabled| enabled.eq_ignore_ascii_case(log_type))
                .then(|| ExportDestination {
                    region: instance.region.clone(),
                    log_group: instance_log_group(&instance.identifier, log_type),
                    log_stream: instance.identifier.clone(),
                })
        }
        // A line a session through a DB PROXY makes the engine write is written by the
        // engine of the database behind the proxy, so it is exported where THAT database's
        // own sessions export it: the proxy runs no engine and publishes no log group of its
        // own.
        SessionTarget::DbProxy {
            region,
            endpoint_label,
        } => destination(
            store,
            &super::proxy_endpoint::proxied_session_target(store, region, endpoint_label)?,
            role,
            wire,
        ),
    }
}

/// `Some(())` exactly when `cluster` has `log_type` enabled for export.
///
/// The comparison is case-insensitive on the stored value, as the log types
/// `EnableCloudwatchLogsExports` names are.
fn exported(cluster: &DbClusterRecord, log_type: &str) -> Option<()> {
    cluster
        .enabled_cloudwatch_logs_exports
        .iter()
        .any(|enabled| enabled.eq_ignore_ascii_case(log_type))
        .then_some(())
}

/// The STREAM a line written on one of `cluster`'s endpoints goes to: the DB instance whose
/// engine wrote it.
///
/// A member's own endpoint names that member; the cluster's writer endpoint names the member
/// the cluster writes through and its reader endpoint the first of the readers, which is the
/// instance a session on either address is served by. A cluster with no member instance yet
/// has no DB instance to name, so its own identifier is the stream — the line was still
/// written by the engine behind that cluster's endpoint.
fn stream_of(
    store: &ComAmazonawsRdsAmazonrdsv19State,
    cluster: &DbClusterRecord,
    role: &EndpointRole,
) -> String {
    if let EndpointRole::ClusterMember(identifier) = role {
        return identifier.clone();
    }
    let members = store.cluster_members(cluster);
    let served = match role {
        EndpointRole::Reader => members.get(1).or_else(|| members.first()),
        _ => members.first(),
    };
    served.map_or_else(
        || cluster.identifier.clone(),
        |member| member.identifier.clone(),
    )
}

/// WRITE what the engine wrote about one statement to the resource's own log, and deliver it
/// to the log export where one is configured.
///
/// The lines a severity keeps out of the log are left out here, so what the engine's log
/// FILE holds and what reaches CloudWatch Logs are the same lines: the file is the one
/// `DescribeDBLogFiles` lists and `DownloadDBLogFilePortion` serves
/// ([`crate::state::InstanceLog`]), and the delivery is what a resource with
/// `EnableCloudwatchLogsExports` streams. Nothing is delivered for a resource with no export
/// configured, the file is still written either way, and nothing about the outcome changes
/// the answer the client has already been given.
///
/// Call it with NO store guard held — it awaits.
pub async fn export(context: &SessionContext, facts: &SessionFacts, lines: &[LogLine]) {
    let written: Vec<&LogLine> = lines
        .iter()
        .filter(|line| line.severity.written_to_the_server_log(facts))
        .take(MAX_LINES_PER_DELIVERY)
        .collect();
    if written.is_empty() {
        return;
    }
    let at = SystemTime::now();
    let records: Vec<String> = written
        .into_iter()
        .map(|line| rendered(facts, at, line))
        .collect();
    // The guard is BOUND to this block, so it is released before the delivery is awaited.
    // The engine WROTE these lines, so the instance's own log file carries them whether or
    // not anything exports it.
    let exported_to = {
        let mut store = context.state.lock();
        if let SessionTarget::DbInstance {
            region,
            resource_id,
        } = &context.target
        {
            if let Some(record) = store.instance_by_resource_id_mut(region, resource_id) {
                for line in &records {
                    record.engine_log.record_engine_line(line.clone());
                }
            }
        }
        destination(&store, &context.target, &context.role, facts.wire)
    };
    let Some(destination) = exported_to else {
        return;
    };
    if !::service_common::cloudwatch_logs::delivered_over(
        &context.callback,
        &destination.region,
        &destination.log_group,
        &destination.log_stream,
        &records,
    )
    .await
    {
        ::tracing::debug!(
            log_group = %destination.log_group,
            log_stream = %destination.log_stream,
            "the engine log lines of this statement could not be delivered to CloudWatch Logs"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the group a resource's log type is exported to is the name Amazon RDS
    /// publishes for that resource kind. The rows span both kinds and both engines' log
    /// types.
    #[test]
    fn each_resource_kind_exports_to_the_group_amazon_rds_publishes() {
        let rows: [(&str, String, &str); 4] = [
            (
                "an Aurora cluster's PostgreSQL log",
                cluster_log_group("obs-cluster", POSTGRESQL_LOG_TYPE),
                "/aws/rds/cluster/obs-cluster/postgresql",
            ),
            (
                "a cluster's MySQL error log",
                cluster_log_group("shop", MYSQL_ERROR_LOG_TYPE),
                "/aws/rds/cluster/shop/error",
            ),
            (
                "an instance's PostgreSQL log",
                instance_log_group("ledger-1", POSTGRESQL_LOG_TYPE),
                "/aws/rds/instance/ledger-1/postgresql",
            ),
            (
                "an instance's audit log",
                instance_log_group("ledger-1", "audit"),
                "/aws/rds/instance/ledger-1/audit",
            ),
        ];
        for (case, rendered, expected) in rows {
            assert_eq!(
                rendered, expected,
                "case {case:?}: expected {expected:?}, got {rendered:?}"
            );
        }
    }

    /// RULE: the engine's error log is exported under its own log type.
    #[test]
    fn each_engines_error_log_has_its_own_log_type() {
        assert_eq!(
            engine_log_type(EngineWire::Postgres),
            "postgresql",
            "the PostgreSQL engines export their server log as `postgresql`"
        );
        assert_eq!(
            engine_log_type(EngineWire::MySql),
            "error",
            "the MySQL family exports its error log as `error`"
        );
    }

    /// RULE: a severity is written to the log when it is at or above `log_min_messages` and
    /// sent to the client when it is at or above `client_min_messages`, in the two ORDERS
    /// PostgreSQL documents — which is why `LOG` is logged and not sent, and `NOTICE` is
    /// sent and not logged, at the two defaults.
    #[test]
    fn the_two_orders_a_severity_is_read_in_differ_for_log_and_client() {
        let rows: [(&str, Severity, bool, bool); 6] = [
            ("a debug message", Severity::Debug, false, false),
            ("a notice", Severity::Notice, false, true),
            ("a warning", Severity::Warning, true, true),
            ("a log message", Severity::Log, true, false),
            ("an error", Severity::Error, true, true),
            ("a fatal", Severity::Fatal, true, true),
        ];
        for (case, severity, logged, reported) in rows {
            assert_eq!(
                severity.log_rank() >= Severity::Warning.log_rank(),
                logged,
                "case {case:?}: expected logged={logged} at the default log_min_messages"
            );
            assert_eq!(
                severity.client_rank() >= Severity::Notice.client_rank(),
                reported,
                "case {case:?}: expected reported={reported} at the default \
                 client_min_messages"
            );
        }
    }

    /// RULE: a severity is named by the values the two parameters take, the `DEBUG<n>`
    /// levels included, and nothing else names one.
    #[test]
    fn a_severity_is_named_by_the_values_those_parameters_take() {
        for (value, expected) in [
            ("warning", Some(Severity::Warning)),
            ("WARNING", Some(Severity::Warning)),
            ("log", Some(Severity::Log)),
            ("debug3", Some(Severity::Debug)),
            ("panic", Some(Severity::Panic)),
            ("whenever", None),
            ("", None),
        ] {
            let named = Severity::named(value);
            assert_eq!(
                named, expected,
                "case {value:?}: expected {expected:?}, got {named:?}"
            );
        }
    }
}
