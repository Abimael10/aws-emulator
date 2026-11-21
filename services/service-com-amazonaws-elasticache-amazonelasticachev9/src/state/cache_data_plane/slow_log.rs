// SPDX-License-Identifier: CC0-1.0

//! The SLOW LOG of one cache node: the commands the engine found slow, what `SLOWLOG`
//! answers about them, and the records a log-delivery configuration delivers into
//! CloudWatch Logs.
//!
//! `LogDeliveryConfiguration$LogType` is documented as "Refers to
//! [slow-log](https://redis.io/commands/slowlog) or engine-log", so the slow log a
//! configuration delivers is the ENGINE's own: the command the engine recorded because its
//! execution took at least as long as the `slowlog-log-slower-than` parameter states, kept
//! at most `slowlog-max-len` entries deep, which a client reads back with `SLOWLOG GET`.
//! Both parameters come from the cache's own parameter group, read where
//! `DescribeCacheParameters` reports them ([`super::cache_facts`]), so a change the control
//! plane makes to either is in force on the next command.
//!
//! The DURATION compared against the threshold is the one this emulator really took to
//! execute the command: nothing is recorded for work that was not slow, and no duration is
//! invented for work nothing ran. A threshold of zero therefore records every command, which
//! is what a zero threshold does on the engine ("A value of zero forces the logging of every
//! command").
//!
//! # Where a record goes
//!
//! A cache whose `LogDeliveryConfigurations` carry an active `slow-log` configuration with
//! `DestinationType` `cloudwatch-logs` also delivers each entry into the CloudWatch Logs log
//! group that configuration's `CloudWatchLogsDestinationDetails$LogGroup` names — written
//! into that service's OWN store in the SAME shared engine, through its own record types, so
//! what was delivered is exactly what `logs:FilterLogEvents`, `logs:GetLogEvents` and
//! `logs:DescribeLogStreams` report. The entry is rendered in the configuration's
//! `LogFormat`: the documented slow-log fields as a JSON object, or the same fields as one
//! line of text.
//!
//! Delivery is BEST EFFORT, as publishing to a customer-owned destination is: a log group
//! CloudWatch Logs does not hold receives nothing (nothing here creates one), and a group
//! with no room for another stream takes no further record. The command that produced the
//! entry still answers its client either way; the loss is traced.
//!
//! # Bounds
//!
//! One entry keeps at most [`MAX_ENTRY_ARGUMENTS`] of the command's arguments and at most
//! [`MAX_ARGUMENT_BYTES`] of each, as the engine's own slow log does, and one log keeps at
//! most [`MAX_KEPT_ENTRIES`] of them however many the parameter asks for — so the slow log of
//! a cache a client floods is bounded whatever the parameter group says.

use ::std::collections::VecDeque;
use ::std::time::{Duration, SystemTime};

use ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state as logs;

use super::resp_frame::Reply;

/// The parameter whose value is how long a command must take to be recorded, in
/// microseconds — the cache's own `slowlog-log-slower-than`.
pub const THRESHOLD_PARAMETER: &str = "slowlog-log-slower-than";

/// The parameter whose value is how many entries the slow log keeps — `slowlog-max-len`.
pub const MAX_LENGTH_PARAMETER: &str = "slowlog-max-len";

/// The `LogDeliveryConfiguration$LogType` this module is the log of.
pub const LOG_TYPE: &str = "slow-log";

/// The `LogDeliveryConfiguration$DestinationType` whose destination is a CloudWatch Logs log
/// group.
pub const CLOUDWATCH_LOGS_DESTINATION: &str = "cloudwatch-logs";

/// The `LogFormat` that delivers each entry as a JSON object; the other value that member
/// takes (`text`) delivers it as one line of text.
pub const JSON_LOG_FORMAT: &str = "json";

/// How many of a command's arguments ONE entry keeps: the engine's own
/// `SLOWLOG_ENTRY_MAX_ARGC`. A command with more is kept with that many, the last of them
/// stating how many were left out.
const MAX_ENTRY_ARGUMENTS: usize = 32;

/// How many bytes of ONE argument an entry keeps: the engine's own
/// `SLOWLOG_ENTRY_MAX_STRING`. A longer argument is kept cut to it, with how many bytes were
/// left out stated after it.
const MAX_ARGUMENT_BYTES: usize = 128;

/// How many entries ONE node's slow log keeps at most, whatever `slowlog-max-len` asks for.
///
/// The ceiling is this emulator's own, as the keyspace's is
/// ([`super::keyspace::MAX_KEYSPACE_BYTES`]): the parameter's published range has no upper
/// bound, and the slow log of a cache a client floods must not take the host.
pub const MAX_KEPT_ENTRIES: usize = 1_024;

/// What the engine's default `slowlog-log-slower-than` is, in microseconds — the value the
/// published parameter catalog carries for it ([`crate::state::parameters`]). Used only for a
/// cache that runs no parameter group at all (a serverless cache), which has no caller-set
/// value to read.
const DEFAULT_THRESHOLD_MICROS: u64 = 10_000;

/// The same for `slowlog-max-len`.
const DEFAULT_MAX_LENGTH: usize = 128;

/// The address reported for a client whose peer this server cannot name.
///
/// The core owns the socket and hands this server a byte STREAM rather than a peer, so there
/// is no address to report; `?:0` is what the engine itself reports for a client whose peer
/// it cannot format, rather than an address made up here.
pub const UNKNOWN_CLIENT_ADDRESS: &str = "?:0";

/// How the slow log of one cache is kept: the threshold a command's own duration has to
/// reach to be recorded, and how many entries are kept.
#[derive(Debug, Clone, Copy)]
pub struct SlowLogPolicy {
    /// What `slowlog-log-slower-than` states, as a duration — `None` for a value that
    /// records nothing at all, which is what a negative one means on the engine.
    threshold: Option<Duration>,
    /// What `slowlog-max-len` states, held to [`MAX_KEPT_ENTRIES`].
    kept: usize,
}

impl SlowLogPolicy {
    /// The policy a cache whose parameters state `threshold_micros` and `max_length` runs
    /// under.
    ///
    /// A value the parameter group does not state, or one that is not a number, leaves that
    /// half on the engine's own default. A NEGATIVE threshold records nothing, which is how
    /// the engine's own parameter turns the slow log off.
    #[must_use]
    pub fn of(threshold_micros: Option<&str>, max_length: Option<&str>) -> Self {
        let threshold = match threshold_micros.map(str::trim) {
            Some(stated) => match stated.parse::<i64>() {
                Ok(micros) if micros < 0 => None,
                Ok(micros) => Some(Duration::from_micros(micros.unsigned_abs())),
                Err(_unparsed) => Some(Duration::from_micros(DEFAULT_THRESHOLD_MICROS)),
            },
            None => Some(Duration::from_micros(DEFAULT_THRESHOLD_MICROS)),
        };
        let kept = max_length
            .map(str::trim)
            .and_then(|stated| stated.parse::<i64>().ok())
            .map_or(DEFAULT_MAX_LENGTH, |stated| {
                usize::try_from(stated).unwrap_or(0)
            })
            .min(MAX_KEPT_ENTRIES);
        Self { threshold, kept }
    }

    /// Whether a command that took `took` is one the slow log records.
    ///
    /// "`slowlog-log-slower-than` … how long, in microseconds, a command must take to be
    /// recorded in the slow log", so a command whose own duration REACHES the threshold is
    /// recorded — which makes a threshold of zero record every command.
    #[must_use]
    pub fn records(&self, took: Duration) -> bool {
        self.kept > 0 && self.threshold.is_some_and(|threshold| took >= threshold)
    }

    /// How many entries the log keeps.
    #[must_use]
    pub fn kept(&self) -> usize {
        self.kept
    }
}

impl Default for SlowLogPolicy {
    /// What a cache whose parameters say nothing runs under: the engine's own defaults.
    fn default() -> Self {
        Self {
            threshold: Some(Duration::from_micros(DEFAULT_THRESHOLD_MICROS)),
            kept: DEFAULT_MAX_LENGTH,
        }
    }
}

/// ONE entry of the slow log: a command the engine found slow, as `SLOWLOG GET` reports it.
#[derive(Debug, Clone)]
pub struct SlowLogEntry {
    /// "A unique progressive identifier for every slow log entry", counted per node.
    pub id: i64,
    /// When the command was processed.
    pub at: SystemTime,
    /// How long executing it took.
    pub took: Duration,
    /// The command and its arguments, cut to what one entry keeps.
    pub arguments: Vec<Vec<u8>>,
    /// The address of the client that sent it ([`UNKNOWN_CLIENT_ADDRESS`]).
    pub client_address: String,
    /// The name its `CLIENT SETNAME` gave it, empty for a client that set none.
    pub client_name: String,
}

impl SlowLogEntry {
    /// The entry as `SLOWLOG GET` answers it: the id, the instant, the duration in
    /// microseconds, the command's arguments, the client's address and its name.
    #[must_use]
    pub fn reply(&self) -> Reply {
        Reply::Array(vec![
            Reply::Integer(self.id),
            Reply::Integer(unix_seconds(self.at)),
            Reply::Integer(i64::try_from(self.took.as_micros()).unwrap_or(i64::MAX)),
            Reply::Array(
                self.arguments
                    .iter()
                    .map(|argument| Reply::Bulk(argument.clone()))
                    .collect(),
            ),
            Reply::text(self.client_address.clone()),
            Reply::text(self.client_name.clone()),
        ])
    }

    /// The command this entry is of, as the delivered record states it: its arguments,
    /// separated by spaces, exactly as the entry kept them.
    #[must_use]
    pub fn command(&self) -> String {
        self.arguments
            .iter()
            .map(|argument| String::from_utf8_lossy(argument).to_string())
            .collect::<Vec<String>>()
            .join(" ")
    }
}

/// The slow log of ONE cache node, shared by every session on its socket.
#[derive(Debug, Default)]
pub struct SlowLog {
    /// The entries kept, oldest first.
    entries: VecDeque<SlowLogEntry>,
    /// The id the next entry takes.
    next_id: i64,
}

impl SlowLog {
    /// Record the command `arguments`, which took `took`, and answer the entry recorded.
    ///
    /// The caller decides WHETHER a command is slow enough ([`SlowLogPolicy::records`]);
    /// `kept` is how many entries the log may hold, so the oldest go once it is full.
    pub fn record(
        &mut self,
        arguments: &[Vec<u8>],
        took: Duration,
        kept: usize,
        client_address: &str,
        client_name: &str,
        at: SystemTime,
    ) -> SlowLogEntry {
        let entry = SlowLogEntry {
            id: self.next_id,
            at,
            took,
            arguments: kept_arguments(arguments),
            client_address: client_address.to_string(),
            client_name: client_name.to_string(),
        };
        self.next_id = self.next_id.saturating_add(1);
        self.entries.push_back(entry.clone());
        let kept = kept.min(MAX_KEPT_ENTRIES);
        while self.entries.len() > kept {
            self.entries.pop_front();
        }
        entry
    }

    /// How many entries the log holds, which is what `SLOWLOG LEN` answers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log holds no entry at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The `count` most recent entries, newest first — which is the order `SLOWLOG GET`
    /// answers in. `None` asks for every entry there is.
    #[must_use]
    pub fn recent(&self, count: Option<usize>) -> Vec<&SlowLogEntry> {
        self.entries
            .iter()
            .rev()
            .take(count.unwrap_or(usize::MAX))
            .collect()
    }

    /// Discard every entry, which is what `SLOWLOG RESET` does. The ids go on counting: an
    /// entry's id is unique for the node, not for what the log currently holds.
    pub fn reset(&mut self) {
        self.entries.clear();
    }
}

/// The arguments one entry keeps of `arguments`: at most [`MAX_ENTRY_ARGUMENTS`] of them,
/// each at most [`MAX_ARGUMENT_BYTES`] long, with what was left out stated the way the
/// engine's own slow log states it.
fn kept_arguments(arguments: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let mut kept: Vec<Vec<u8>> = Vec::with_capacity(arguments.len().min(MAX_ENTRY_ARGUMENTS));
    // The engine keeps `MAX_ENTRY_ARGUMENTS` slots, the last of which states how many
    // arguments it stands for when there were more.
    let fits = arguments.len() <= MAX_ENTRY_ARGUMENTS;
    let taken = if fits {
        arguments.len()
    } else {
        MAX_ENTRY_ARGUMENTS - 1
    };
    for argument in arguments.iter().take(taken) {
        if argument.len() > MAX_ARGUMENT_BYTES {
            let mut cut = argument[..MAX_ARGUMENT_BYTES].to_vec();
            cut.extend_from_slice(
                format!(
                    "... ({} more bytes)",
                    argument.len() - MAX_ARGUMENT_BYTES
                )
                .as_bytes(),
            );
            kept.push(cut);
        } else {
            kept.push(argument.clone());
        }
    }
    if !fits {
        kept.push(
            format!("... ({} more arguments)", arguments.len() - taken).into_bytes(),
        );
    }
    kept
}

/// Where a cache's active slow-log log-delivery configuration delivers, and which node's
/// entries it carries — everything the write into the destination needs, read off the store
/// while it was held ([`super::cache_facts`]).
#[derive(Debug, Clone)]
pub struct SlowLogDelivery {
    /// The log group the configuration's `CloudWatchLogsDestinationDetails$LogGroup` names,
    /// as it named it — never a group of this module's own naming.
    pub log_group: String,
    /// The configuration's `LogFormat`: [`JSON_LOG_FORMAT`] delivers a JSON object, anything
    /// else one line of text.
    pub log_format: String,
    /// `CacheClusterId` of the node the entries come from.
    pub cache_cluster_id: String,
    /// `CacheNodeId` of that node.
    pub cache_node_id: String,
    /// The Region that node's cluster is in, which the destination stream's ARN is stamped
    /// with.
    pub region: String,
}

impl SlowLogDelivery {
    /// The record `entry` is delivered as, in this configuration's own format.
    ///
    /// The fields are the ones a slow-log record carries: the node that logged it, the
    /// entry's own progressive id, the instant, how long the command took in microseconds,
    /// the command itself and the client that sent it.
    #[must_use]
    pub fn message(&self, entry: &SlowLogEntry) -> String {
        let micros = i64::try_from(entry.took.as_micros()).unwrap_or(i64::MAX);
        if self.log_format.eq_ignore_ascii_case(JSON_LOG_FORMAT) {
            return ::serde_json::json!({
                "CacheClusterId": self.cache_cluster_id,
                "CacheNodeId": self.cache_node_id,
                "Id": entry.id,
                "Timestamp": unix_seconds(entry.at),
                "Duration (us)": micros,
                "Command": entry.command(),
                "ClientAddress": entry.client_address,
                "ClientName": entry.client_name,
            })
            .to_string();
        }
        // The same fields, in the same order, as one line: the command is the only one that
        // can carry a space, so it is the one that is quoted.
        format!(
            "{cluster} {node} {id} {at} {micros} \"{command}\" {address} {name}",
            cluster = self.cache_cluster_id,
            node = self.cache_node_id,
            id = entry.id,
            at = unix_seconds(entry.at),
            command = entry.command(),
            address = entry.client_address,
            name = entry.client_name,
        )
    }

    /// The stream a record of this node is written to: the node's own cluster id, so a
    /// reader of the group can tell which node logged what.
    #[must_use]
    pub fn log_stream(&self) -> String {
        self.cache_cluster_id.clone()
    }
}

/// Write the record `entry` produced into the CloudWatch Logs store of the SAME engine.
///
/// Best effort: a destination group CloudWatch Logs does not hold, or one with no room for
/// another stream, loses the record and is traced (see this module's docs). Call it with NO
/// store of this service locked.
pub fn deliver(
    state: &::engine::ServiceState<logs::ComAmazonawsCloudwatchlogsLogs20140328State>,
    delivery: &SlowLogDelivery,
    entry: &SlowLogEntry,
) {
    let message = delivery.message(entry);
    let stream_name = delivery.log_stream();
    let mut store = state.lock();
    let ingested_at = logs::now_ms();
    // The data identifiers the data protection policies in force for the group name: such a
    // policy masks what is ingested into the group, by whichever path the records arrive.
    let data_identifiers = store
        .log_groups
        .get(&delivery.log_group)
        .map(|group| store.data_protection_identifiers_for(group))
        .unwrap_or_default();
    // The customer managed key the destination group is associated with, under which the
    // data it takes in is encrypted — by whichever path the records arrive.
    let ingest_kms_key_id = store
        .log_groups
        .get(&delivery.log_group)
        .and_then(|group| group.kms_key_id.clone());
    // The transformer in force for the destination group transforms what that group takes in
    // — by whichever path the records arrive — so a delivered record is stored with the
    // fields that pipeline produced from it, like any other ingested event.
    let transformer = store
        .log_groups
        .get(&delivery.log_group)
        .map(|group| logs::active_transformer::pipeline(&store, group))
        .unwrap_or_default();
    let stored = logs::StoredEvent {
        timestamp: unix_millis(entry.at),
        masked_message: logs::data_protection::masked(&message, &data_identifiers),
        transformed_message: transformer.transform(&message),
        message,
        ingestion_time: ingested_at,
        ingestion_seq: store.take_ingestion_seq(),
        ingest_kms_key_id,
    };
    let Some(group) = store.log_groups.get_mut(&delivery.log_group) else {
        // The configuration names a group the ACCOUNT owns; nothing here creates one, so a
        // group that is not there is a destination nothing reaches.
        ::tracing::warn!(
            log_group = %delivery.log_group,
            "the log group a cache delivers its slow log to is not held by CloudWatch Logs, \
             so a slow-log record was not delivered"
        );
        return;
    };
    // A stream is opened per NODE, and it is admitted against the ceiling the owning
    // service's own `CreateLogStream` admits one against, so no write path can take a group
    // past it.
    if !group.streams.contains_key(&stream_name) && group.streams.len() >= logs::MAX_STREAMS_PER_GROUP
    {
        ::tracing::warn!(
            log_group = %delivery.log_group,
            log_stream = %stream_name,
            "the log group already holds the maximum number of streams, so a slow-log record \
             was not delivered"
        );
        return;
    }
    let stream = group
        .streams
        .entry(stream_name.clone())
        .or_insert_with(|| {
            logs::LogStreamRecord::new(
                &delivery.region,
                &delivery.log_group,
                &stream_name,
                ingested_at,
            )
        });
    stream.events.push(stored);
    // The stream is kept in the `(timestamp, ingestion_seq)` order its reader pages it in;
    // records arrive in that order already, so only one predating what is there moves.
    stream.events.sort_by_key(logs::StoredEvent::key);
    stream.enforce_storage_ceiling();
    stream.refresh_event_times();
    stream.last_ingestion_time = Some(ingested_at);
}

/// The instant `at` as whole seconds since the epoch, which is what a slow-log entry states.
fn unix_seconds(at: SystemTime) -> i64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// The same instant in milliseconds, which is what a log event's `timestamp` is in.
fn unix_millis(at: SystemTime) -> i64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command every row below records.
    fn command(arguments: &[&str]) -> Vec<Vec<u8>> {
        arguments
            .iter()
            .map(|argument| argument.as_bytes().to_vec())
            .collect()
    }

    /// RULE: a command is recorded when its own duration REACHES the threshold the cache's
    /// `slowlog-log-slower-than` states, and nothing is recorded while that parameter states
    /// no threshold at all or the log keeps no entry.
    ///
    /// The rows span what the rule generalises over: the published bottom of the parameter's
    /// range (which records every command), a threshold a command reaches exactly, one it
    /// falls short of, a negative value, a value that is not a number, a parameter the group
    /// does not state, and a depth of zero.
    #[test]
    fn a_command_is_recorded_when_it_reaches_the_threshold_its_parameters_state() {
        // (the case, the `slowlog-log-slower-than` the group states, the `slowlog-max-len` it
        // states, how long the command took in microseconds, whether it is recorded).
        let rows = [
            ("every command", Some("0"), Some("128"), 0_u64, true),
            ("exactly the threshold", Some("500"), Some("128"), 500, true),
            ("past the threshold", Some("500"), Some("128"), 501, true),
            ("short of the threshold", Some("500"), Some("128"), 499, false),
            (
                "a threshold that records nothing",
                Some("-1"),
                Some("128"),
                1_000_000,
                false,
            ),
            (
                "a threshold that is not a number",
                Some("soon"),
                Some("128"),
                10_000,
                true,
            ),
            (
                "a parameter the group does not state",
                None,
                None,
                10_000,
                true,
            ),
            ("a log that keeps no entry", Some("0"), Some("0"), 1_000_000, false),
        ];
        for (case, threshold, depth, micros, expected) in rows {
            let policy = SlowLogPolicy::of(threshold, depth);
            let records = policy.records(Duration::from_micros(micros));
            assert_eq!(
                records, expected,
                "case {case:?}: expected a command of {micros}us to be recorded={expected}, got \
                 {records}"
            );
        }
    }

    /// RULE: the log keeps at most the entries its depth parameter states, the most recent
    /// first, each with an id of its own that goes on counting however many entries are
    /// dropped — and a reset discards them all.
    #[test]
    fn the_log_keeps_its_most_recent_entries_and_counts_every_id() {
        let mut log = SlowLog::default();
        let kept = 2;
        for index in 1..=5 {
            log.record(
                &command(&["SET", &format!("key-{index}"), "value"]),
                Duration::from_micros(index),
                kept,
                UNKNOWN_CLIENT_ADDRESS,
                "",
                SystemTime::UNIX_EPOCH,
            );
        }
        assert_eq!(log.len(), kept, "the log keeps what its depth states");
        let recent: Vec<i64> = log.recent(None).iter().map(|entry| entry.id).collect();
        assert_eq!(
            recent,
            vec![4, 3],
            "the most recent entry comes first, and the ids count every command recorded"
        );
        let newest = log
            .recent(Some(1))
            .first()
            .map(|entry| entry.command())
            .unwrap_or_default();
        assert_eq!(
            newest, "SET key-5 value",
            "the newest entry is the last command recorded, got {newest:?}"
        );
        log.reset();
        assert!(log.is_empty(), "a reset discards every entry the log held");
        let after = log.record(
            &command(&["GET", "key-1"]),
            Duration::ZERO,
            kept,
            UNKNOWN_CLIENT_ADDRESS,
            "",
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(
            after.id, 5,
            "an entry's id is unique for the node, so the count goes on past a reset"
        );
    }

    /// RULE: one entry keeps at most the engine's own number of arguments and at most its own
    /// number of bytes of each, stating what it left out.
    #[test]
    fn an_entry_keeps_a_bounded_reading_of_the_command() {
        let long = "v".repeat(MAX_ARGUMENT_BYTES + 7);
        let kept = kept_arguments(&command(&["SET", "key", &long]));
        assert_eq!(kept.len(), 3, "a command of three arguments keeps all three");
        assert_eq!(
            String::from_utf8_lossy(&kept[2]),
            format!("{}... (7 more bytes)", "v".repeat(MAX_ARGUMENT_BYTES)),
            "a long argument is kept cut, stating how many bytes were left out"
        );
        let many: Vec<String> = (0..MAX_ENTRY_ARGUMENTS + 5)
            .map(|index| format!("key-{index}"))
            .collect();
        let mut arguments = vec!["MGET".to_string()];
        arguments.extend(many);
        let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
        let kept = kept_arguments(&command(&borrowed));
        assert_eq!(
            kept.len(),
            MAX_ENTRY_ARGUMENTS,
            "an entry keeps at most the engine's own number of arguments"
        );
        assert_eq!(
            String::from_utf8_lossy(&kept[MAX_ENTRY_ARGUMENTS - 1]),
            format!(
                "... ({} more arguments)",
                arguments.len() - (MAX_ENTRY_ARGUMENTS - 1)
            ),
            "the last of them states how many arguments were left out"
        );
    }

    /// RULE: a delivered record states the node that executed the command, the entry's own
    /// id, instant and duration, the command itself and the client that sent it — as a JSON
    /// object or as one line of text, as the configuration's `LogFormat` says.
    #[test]
    fn a_delivered_record_states_the_entry_in_the_format_the_configuration_names() {
        let entry = SlowLogEntry {
            id: 7,
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000),
            took: Duration::from_micros(1_234),
            arguments: command(&["GETRANGE", "probe", "0", "-1"]),
            client_address: "10.0.1.7:41234".to_string(),
            client_name: "prober".to_string(),
        };
        let delivery = |format: &str| SlowLogDelivery {
            log_group: "/aws/elasticache/slow".to_string(),
            log_format: format.to_string(),
            cache_cluster_id: "a-cluster-0001-001".to_string(),
            cache_node_id: "0001".to_string(),
            region: "us-west-2".to_string(),
        };
        let json = delivery(JSON_LOG_FORMAT);
        let record: ::serde_json::Value = ::serde_json::from_str(&json.message(&entry))
            .expect("a record delivered as JSON parses as one");
        assert_eq!(
            (
                record["CacheClusterId"].as_str(),
                record["CacheNodeId"].as_str(),
                record["Id"].as_i64(),
                record["Timestamp"].as_i64(),
                record["Duration (us)"].as_i64(),
                record["Command"].as_str(),
                record["ClientAddress"].as_str(),
                record["ClientName"].as_str(),
            ),
            (
                Some("a-cluster-0001-001"),
                Some("0001"),
                Some(7),
                Some(1_600_000_000),
                Some(1_234),
                Some("GETRANGE probe 0 -1"),
                Some("10.0.1.7:41234"),
                Some("prober"),
            ),
            "the JSON record states every field of the entry, got {record}"
        );
        let text = delivery("text").message(&entry);
        assert_eq!(
            text,
            "a-cluster-0001-001 0001 7 1600000000 1234 \"GETRANGE probe 0 -1\" 10.0.1.7:41234 \
             prober",
            "the text record states the same fields on one line, got {text:?}"
        );
        assert_eq!(
            json.log_stream(),
            "a-cluster-0001-001",
            "a record is written to the stream of the node that logged it"
        );
    }
}
