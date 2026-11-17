// SPDX-License-Identifier: CC0-1.0

//! BUS LOGGING: the records an event bus whose `LogConfig` asks for logging writes about
//! the events it handles, and the CloudWatch Logs log groups they travel to.
//!
//! # The rule this module implements
//!
//! A bus's `LogConfig` states "the level of logging detail to include[, which] applies to
//! all log destinations for the event bus" (`LogConfig$Level`) and "whether EventBridge
//! include detailed event information in the records it generates" ([`LogConfig`]
//! `IncludeDetail`). So a bus configured at a level that covers a record EMITS that record,
//! and the record carries the event's own detail only under [`INCLUDE_DETAIL_FULL`].
//!
//! WHERE those records go is not configured in EventBridge at all: the bus is the SENDER of
//! a vended-log pipeline the account builds in CloudWatch Logs — `PutDeliverySource` over
//! the bus's ARN with one of the log types that service publishes for an event bus ("For
//! Amazon EventBridge Event Bus (Custom), the valid values are `INFO_LOGS` and
//! `ERROR_LOGS`", `smithy_query(query="doc",shape="PutDeliverySourceRequest",member="logType")`),
//! a delivery destination naming a log group, and a `CreateDelivery` pairing the two. Which
//! deliveries carry a resource's records, and into which group, are CloudWatch Logs' own
//! records, so they are ASKED of that service over the store both share
//! (`log_group_deliveries`) rather than tracked a second time here: a bus no delivery names
//! writes nothing, which is what an account that wired no pipeline sees, and a bus whose
//! pipeline exists starts writing as soon as it does.
//!
//! The two halves are independent, which is what a client configuring one of them observes:
//! a bus with no `LogConfig` emits no record however many deliveries name it, and a bus at
//! `INFO` with no delivery emits records nothing carries.
//!
//! # What one record says
//!
//! One record is one JSON object: which resource it is about and what the bus did, then the
//! `details` of the thing it did. The envelope members are the bus's own
//! ([`BusLogRecord::message`]) — the bus's ARN and name, the instant the message was
//! handled, the message type, the level it was written at and the id of the event it is
//! about — and `details` is the member set of THAT message type, so a consumer reads one
//! message type's detail without knowing about any other.
//!
//! The record is the SENDER's document, not a projection of the delivery's own
//! `recordFields`: this sender's record carries members its published template does not list
//! (the bus's name beside its ARN), and its `details` is a nested object rather than a flat
//! value, so the field list could not describe it. Nor is there a second encoding to pick:
//! the only `outputFormat` CloudWatch Logs publishes for a log-group destination is the
//! structured one, so a delivery into a log group writes this JSON document whatever else its
//! destination could have been configured with. That is why [`deliver`] renders the record
//! here instead of through the field-list renderer a delimited destination needs.
//!
//! Delivery is BEST EFFORT, as it is for every vended-logs destination: the publisher has
//! already been answered, so a group the account has since deleted, or a write CloudWatch
//! Logs refuses, loses the record and is reported in tracing rather than failing the call.

use super::*;

/// `Level` `OFF` — "No logging": a bus configured at it emits no record at all, which is
/// also what a bus with no `LogConfig` does.
pub const LOG_LEVEL_OFF: &str = "OFF";

/// `Level` `ERROR`: the records reporting that something did not succeed.
pub const LOG_LEVEL_ERROR: &str = "ERROR";

/// `Level` `INFO`: the records reporting the ordinary progress of an event through the bus,
/// beside the error ones.
pub const LOG_LEVEL_INFO: &str = "INFO";

/// `Level` `TRACE`: every record the bus writes.
pub const LOG_LEVEL_TRACE: &str = "TRACE";

/// `IncludeDetail` `FULL`: the record carries the detail of the event it is about.
pub const INCLUDE_DETAIL_FULL: &str = "FULL";

/// The `logType` of a delivery source that carries an event bus's INFORMATIONAL records, as
/// CloudWatch Logs' own catalogue spells it for this sender ("For Amazon EventBridge Event
/// Bus (Custom), the valid values are `INFO_LOGS` and `ERROR_LOGS`",
/// `smithy_query(query="doc",shape="PutDeliverySourceRequest",member="logType")`).
pub const INFO_LOGS_LOG_TYPE: &str = "INFO_LOGS";

/// The `logType` of a delivery source that carries an event bus's ERROR records; see
/// [`INFO_LOGS_LOG_TYPE`].
pub const ERROR_LOGS_LOG_TYPE: &str = "ERROR_LOGS";

/// The `message_type` of the record a bus writes for an event it INGESTED: the one signal a
/// client has that the bus received an event, as opposed to never seeing it or dropping it.
pub const EVENT_INGEST_SUCCESS: &str = "EVENT_INGEST_SUCCESS";

/// The `message_type` of the record a bus writes for a target invocation that did NOT
/// succeed: the one signal a client has that an event the bus routed never reached the
/// target the rule pointed it at.
pub const INVOCATION_FAILURE: &str = "INVOCATION_FAILURE";

/// The record member naming the resource the record is about: the bus's own ARN.
const RESOURCE_ARN_FIELD: &str = "resource_arn";

/// The record member naming the instant the message was handled, in epoch milliseconds.
const MESSAGE_TIMESTAMP_MS_FIELD: &str = "message_timestamp_ms";

/// The record member naming the bus the record is about.
const EVENT_BUS_NAME_FIELD: &str = "event_bus_name";

/// The record member naming WHAT the bus did ([`EVENT_INGEST_SUCCESS`]).
const MESSAGE_TYPE_FIELD: &str = "message_type";

/// The record member naming the level the record was written at, which is the level a bus's
/// `LogConfig` has to cover for it to be written at all ([`emitted_at_level`]).
const LOG_LEVEL_FIELD: &str = "log_level";

/// The record member naming the event the record is about, by the `EventId` the publishing
/// call was answered with.
const EVENT_ID_FIELD: &str = "event_id";

/// The record member holding the detail of this message type.
const DETAILS_FIELD: &str = "details";

/// The `details` member naming the account the publishing call was made by.
const CALLER_ACCOUNT_ID_DETAIL: &str = "caller_account_id";

/// The `details` member naming the published event's `Source`.
const SOURCE_DETAIL: &str = "source";

/// The `details` member naming the published event's `DetailType`.
const DETAIL_TYPE_DETAIL: &str = "detail_type";

/// The `details` member carrying the published event's own `Detail`, written only under
/// [`INCLUDE_DETAIL_FULL`].
const EVENT_DETAIL_DETAIL: &str = "event_detail";

/// The `details` member naming the rule whose `EventPattern` matched the event the failed
/// invocation was carrying, by its ARN.
const RULE_ARN_DETAIL: &str = "rule_arn";

/// The `details` member naming the target the failed invocation was made on, by its `Arn`.
const TARGET_ARN_DETAIL: &str = "target_arn";

/// The `details` member naming the status the invocation ENDED on — the condition the dead
/// letter of that delivery carries beside it ([`super::NO_PERMISSIONS`] and its
/// neighbours), which for a failed invocation is what withheld it.
const FINAL_INVOCATION_STATUS_DETAIL: &str = "final_invocation_status";

/// The `details` member carrying what that condition reported: the sentence the target's own
/// service, or the authorization that withheld the call, gave for it.
const ERROR_MESSAGE_DETAIL: &str = "error_message";

/// The `details` member carrying what the target was to be HANDED — the event envelope, or
/// the text the target asked for instead — written only under [`INCLUDE_DETAIL_FULL`].
const TARGET_INPUT_DETAIL: &str = "target_input";

/// One record a bus's log configuration owes about one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusLogRecord {
    /// The bus the record is about, by name.
    pub bus: String,
    /// That bus's ARN — the resource the record is about, and the resource a delivery
    /// source has to represent for the record to be carried anywhere.
    pub bus_arn: String,
    /// The level this record is written at: the record is emitted only by a bus whose
    /// configured `Level` covers it ([`emitted_at_level`]).
    pub log_level: String,
    /// What the bus did ([`EVENT_INGEST_SUCCESS`]).
    pub message_type: String,
    /// The `EventId` of the event this record is about.
    pub event_id: String,
    /// The instant the bus handled the message.
    pub at: SystemTime,
    /// The detail members of this message type, already narrowed by the bus's
    /// `IncludeDetail`.
    pub details: ::serde_json::Map<String, ::serde_json::Value>,
}

impl BusLogRecord {
    /// This record as the one JSON object a consumer reads out of the log group.
    #[must_use]
    pub fn message(&self) -> String {
        let mut record = ::serde_json::Map::new();
        record.insert(
            RESOURCE_ARN_FIELD.to_string(),
            self.bus_arn.clone().into(),
        );
        record.insert(
            MESSAGE_TIMESTAMP_MS_FIELD.to_string(),
            epoch_millis(self.at).into(),
        );
        record.insert(EVENT_BUS_NAME_FIELD.to_string(), self.bus.clone().into());
        record.insert(
            MESSAGE_TYPE_FIELD.to_string(),
            self.message_type.clone().into(),
        );
        record.insert(LOG_LEVEL_FIELD.to_string(), self.log_level.clone().into());
        record.insert(EVENT_ID_FIELD.to_string(), self.event_id.clone().into());
        record.insert(
            DETAILS_FIELD.to_string(),
            ::serde_json::Value::Object(self.details.clone()),
        );
        ::serde_json::Value::Object(record).to_string()
    }
}

/// `at` in milliseconds since the Unix epoch, which is what the record's own instant member
/// carries. An instant before the epoch, which no record can be written at, reads as the
/// epoch rather than wrapping.
fn epoch_millis(at: SystemTime) -> i64 {
    at.duration_since(::std::time::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// How verbose a level is, so the level a bus is CONFIGURED at can be compared with the
/// level a record is written at: `OFF` covers nothing, and each further level covers
/// everything the one before it covers ("The level of logging detail to include").
///
/// A value that is no modelled `Level` covers nothing, which is the same thing a bus with no
/// configuration at all does.
fn verbosity(level: &str) -> u8 {
    match level {
        LOG_LEVEL_ERROR => 1,
        LOG_LEVEL_INFO => 2,
        LOG_LEVEL_TRACE => 3,
        // `OFF`, and any value the `Level` enum does not admit.
        _ => 0,
    }
}

/// Whether a bus configured with `config` emits the records written at `record_level`.
///
/// A bus with no `LogConfig`, one at [`LOG_LEVEL_OFF`] and one whose `Level` the enum does
/// not admit emit nothing; otherwise a level covers every record at or below its own
/// verbosity, so `INFO` emits the informational and the error records while `ERROR` emits
/// the error ones alone.
#[must_use]
pub fn emitted_at_level(config: Option<&LogConfig>, record_level: &str) -> bool {
    let configured = config.and_then(|config| config.level.as_deref()).unwrap_or(LOG_LEVEL_OFF);
    let wanted = verbosity(record_level);
    wanted > 0 && verbosity(configured) >= wanted
}

/// Whether a bus configured with `config` includes the event's own detail in the records it
/// writes: "whether EventBridge include detailed event information in the records it
/// generates" (`LogConfig$IncludeDetail`), which only [`INCLUDE_DETAIL_FULL`] asks for.
#[must_use]
pub fn includes_detail(config: Option<&LogConfig>) -> bool {
    config
        .and_then(|config| config.include_detail.as_deref())
        .is_some_and(|include| include == INCLUDE_DETAIL_FULL)
}

/// The `logType`s a delivery source may represent a bus under to carry a record written at
/// `record_level`.
///
/// CloudWatch Logs publishes two log types for an event bus ([`INFO_LOGS_LOG_TYPE`],
/// [`ERROR_LOGS_LOG_TYPE`]), and each names the LOWEST verbosity it carries the way the
/// bus's own `Level` does: a pipeline for the informational records carries the error ones
/// too, while one for the error records carries those alone.
#[must_use]
pub fn carrying_log_types(record_level: &str) -> &'static [&'static str] {
    if record_level == LOG_LEVEL_ERROR {
        &[ERROR_LOGS_LOG_TYPE, INFO_LOGS_LOG_TYPE]
    } else {
        &[INFO_LOGS_LOG_TYPE]
    }
}

/// One event a bus has INGESTED, as the record about it reports it: which bus took it in,
/// the receipt the publishing call was answered with, who made that call, and the members of
/// the published entry.
#[derive(Debug, Clone, Copy)]
pub struct IngestedEvent<'a> {
    /// The bus that ingested the event.
    pub bus: &'a str,
    /// The `EventId` the publishing call was answered with for this entry.
    pub event_id: &'a str,
    /// The account the publishing call was made by.
    pub caller_account_id: &'a str,
    /// The entry's `Source`.
    pub source: Option<&'a str>,
    /// The entry's `DetailType`.
    pub detail_type: Option<&'a str>,
    /// The entry's `Detail`, carried only where the configuration asks for it
    /// ([`includes_detail`]).
    pub detail: Option<&'a str>,
}

/// The [`EVENT_INGEST_SUCCESS`] record a bus owes for one event it has just ingested, or
/// `None` when that bus's configuration writes none.
///
/// The record is about the INGESTION, so it is owed by every event the bus took in, whatever
/// becomes of it afterwards: an event no rule matched was ingested just as much as one that
/// reached a target.
#[must_use]
pub fn ingest_record(
    config: Option<&LogConfig>,
    region: &str,
    event: &IngestedEvent<'_>,
    at: SystemTime,
) -> Option<BusLogRecord> {
    if !emitted_at_level(config, LOG_LEVEL_INFO) {
        return None;
    }
    let mut details = ::serde_json::Map::new();
    details.insert(
        CALLER_ACCOUNT_ID_DETAIL.to_string(),
        event.caller_account_id.to_string().into(),
    );
    if let Some(source) = event.source {
        details.insert(SOURCE_DETAIL.to_string(), source.to_string().into());
    }
    if let Some(detail_type) = event.detail_type {
        details.insert(
            DETAIL_TYPE_DETAIL.to_string(),
            detail_type.to_string().into(),
        );
    }
    if includes_detail(config) {
        if let Some(detail) = event.detail {
            details.insert(EVENT_DETAIL_DETAIL.to_string(), detail.to_string().into());
        }
    }
    Some(BusLogRecord {
        bus: event.bus.to_string(),
        bus_arn: event_bus_arn(region, event.bus),
        log_level: LOG_LEVEL_INFO.to_string(),
        message_type: EVENT_INGEST_SUCCESS.to_string(),
        event_id: event.event_id.to_string(),
        at,
        details,
    })
}

/// One invocation of a rule's target that FAILED, as the record about it reports it: which
/// bus routed the event, which event it was, the rule and the target the handover was made
/// under, and the condition it failed with.
#[derive(Debug, Clone, Copy)]
pub struct FailedInvocation<'a> {
    /// The bus whose rule matched the event — the resource the record is about.
    pub bus: &'a str,
    /// The `EventId` of the event that was not delivered.
    pub event_id: &'a str,
    /// The ARN of the rule whose match made this handover.
    pub rule_arn: &'a str,
    /// The `Arn` of the target the handover was made on.
    pub target_arn: &'a str,
    /// The condition EventBridge reports for the failure, which is the status the
    /// invocation ended on.
    pub error_code: &'a str,
    /// What that condition reported.
    pub error_message: &'a str,
    /// What the target was to be handed — the event envelope, or the text the target asked
    /// for instead — carried only where the configuration asks for it
    /// ([`includes_detail`]).
    pub envelope: &'a str,
}

/// The [`INVOCATION_FAILURE`] record a bus owes for one handover that failed, or `None` when
/// that bus's configuration writes none.
///
/// The record is written at [`LOG_LEVEL_ERROR`], so a bus configured at that level writes it
/// while writing no informational record at all — which is the whole point of the error
/// level: a client that turns on logging to find out why a target is not firing is told what
/// withheld the invocation and on which rule and target.
#[must_use]
pub fn invocation_failure_record(
    config: Option<&LogConfig>,
    region: &str,
    failure: &FailedInvocation<'_>,
    at: SystemTime,
) -> Option<BusLogRecord> {
    if !emitted_at_level(config, LOG_LEVEL_ERROR) {
        return None;
    }
    let mut details = ::serde_json::Map::new();
    details.insert(
        RULE_ARN_DETAIL.to_string(),
        failure.rule_arn.to_string().into(),
    );
    details.insert(
        TARGET_ARN_DETAIL.to_string(),
        failure.target_arn.to_string().into(),
    );
    details.insert(
        FINAL_INVOCATION_STATUS_DETAIL.to_string(),
        failure.error_code.to_string().into(),
    );
    details.insert(
        ERROR_MESSAGE_DETAIL.to_string(),
        failure.error_message.to_string().into(),
    );
    // What the target was to be handed is "detailed event information", so it travels under
    // the same configuration member that decides whether an ingest record carries the
    // event's own detail: a bus configured to keep the event out of its logs keeps it out of
    // this record too. It travels as the TEXT the delivery would have sent, exactly as the
    // event's own detail does in an ingest record, so a consumer reads what the target would
    // have received rather than a re-serialization of it.
    if includes_detail(config) {
        details.insert(
            TARGET_INPUT_DETAIL.to_string(),
            failure.envelope.to_string().into(),
        );
    }
    Some(BusLogRecord {
        bus: failure.bus.to_string(),
        bus_arn: event_bus_arn(region, failure.bus),
        log_level: LOG_LEVEL_ERROR.to_string(),
        message_type: INVOCATION_FAILURE.to_string(),
        event_id: failure.event_id.to_string(),
        at,
        details,
    })
}

/// The stream a bus's records are written into, inside a destination log group.
///
/// AWS publishes no name for the stream a vended-log delivery writes into, so the records of
/// one SENDER are kept in one stream of their own, named by the resource that sent them: a
/// group several buses deliver into keeps each one's records apart, and a client reading the
/// group — `FilterLogEvents` reads across every stream of it — sees all of them.
fn log_stream_name(bus: &str) -> String {
    bus.to_string()
}

/// How many vended-log records ONE call may write, across every record it owes and every
/// delivery that carries one.
///
/// This emulator's OWN bound, not a limit AWS publishes. Each factor is bounded on its own —
/// at most ten events per publishing call (`PutEvents$Entries` is modelled `length 1..10`),
/// and the deliveries one account holds are bounded by the CloudWatch Logs store that holds
/// them — but their PRODUCT is not, so one call could otherwise make thousands of in-process
/// writes. It is set far above any realistic pipeline (a bus delivering its logs to one or a
/// few log groups), and a call that exhausts it stops writing and says so in the log rather
/// than writing more.
const MAX_BUS_LOG_WRITES_PER_CALL: usize = 100;

/// Write each record to every log group a delivery carries that bus's records of its level
/// into.
///
/// The pipeline is resolved from the CloudWatch Logs store of this same engine
/// ([`::service_com_amazonaws_cloudwatchlogs_logs_20140328::state::log_group_deliveries`]),
/// so what reaches a group is what the account really created through `PutDeliverySource`,
/// `PutDeliveryDestination` and `CreateDelivery`, and a record several deliveries carry is
/// written once per delivery. The destination group is the ACCOUNT's, so the write goes into
/// the group CloudWatch Logs already holds and creates none.
///
/// Each record is written as it is rendered, so what this holds is ONE record rather than the
/// (records x deliveries) product, and the writes it makes are capped by
/// [`MAX_BUS_LOG_WRITES_PER_CALL`].
///
/// Call it with no state guard held: it awaits the writes.
pub async fn deliver<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    records: &[BusLogRecord],
) {
    if records.is_empty() {
        return;
    }
    // Which deliveries carry one (bus, log type) pair, asked of CloudWatch Logs ONCE for each
    // distinct pair the records name — at most one per bus the publishing call wrote to — and
    // then read for every record of that bus.
    let mut pipelines: BTreeMap<(&str, &str), Vec<LogGroupDelivery>> = BTreeMap::new();
    for record in records {
        for log_type in carrying_log_types(&record.log_level) {
            pipelines
                .entry((record.bus_arn.as_str(), *log_type))
                .or_insert_with(|| {
                    ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state::log_group_deliveries(
                        engine,
                        &record.bus_arn,
                        log_type,
                    )
                });
        }
    }
    let mut made = 0usize;
    for record in records {
        // ONE rendering of this record is resident while it is delivered, whatever the
        // pipeline carries it to.
        let message = record.message();
        // A delivery that carries the record under more than one of the log types its level
        // travels under still carries it ONCE.
        let mut carried: BTreeSet<&str> = BTreeSet::new();
        for log_type in carrying_log_types(&record.log_level) {
            let Some(deliveries) = pipelines.get(&(record.bus_arn.as_str(), *log_type)) else {
                continue;
            };
            for delivery in deliveries {
                if !carried.insert(delivery.delivery_id.as_str()) {
                    continue;
                }
                if made >= MAX_BUS_LOG_WRITES_PER_CALL {
                    ::tracing::warn!(
                        writes = MAX_BUS_LOG_WRITES_PER_CALL,
                        "this call's bus-log writes are spent, so the remaining records are \
                         not delivered"
                    );
                    return;
                }
                made += 1;
                let written = ::service_common::cloudwatch_logs::delivered_into_held_group(
                    ctx,
                    &delivery.region,
                    &delivery.log_group,
                    &log_stream_name(&record.bus),
                    ::std::slice::from_ref(&message),
                )
                .await;
                if !written {
                    ::tracing::debug!(
                        log_group = %delivery.log_group,
                        "the log group a delivery of this bus's logs names did not take the \
                         record"
                    );
                }
            }
        }
    }
}

/// The deliveries of one pipeline, as CloudWatch Logs reports them.
type LogGroupDelivery =
    ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state::LogGroupDelivery;

#[cfg(test)]
mod tests {
    use super::*;

    /// A configuration as `CreateEventBus`/`UpdateEventBus` store one.
    fn config(level: &str, include_detail: &str) -> LogConfig {
        LogConfig {
            level: Some(level.to_string()),
            include_detail: Some(include_detail.to_string()),
        }
    }

    /// RULE: a bus emits the records of every level its own `Level` covers, and a bus that
    /// configures none emits nothing.
    #[test]
    fn a_configured_level_covers_every_record_at_or_below_it() {
        // One row per (configured level, record level) pair, over every modelled `Level`
        // (`LEVEL_VALUES`) and every level a record is written at.
        for (configured, error, info, trace) in [
            (LOG_LEVEL_OFF, false, false, false),
            (LOG_LEVEL_ERROR, true, false, false),
            (LOG_LEVEL_INFO, true, true, false),
            (LOG_LEVEL_TRACE, true, true, true),
        ] {
            for (record_level, expected) in [
                (LOG_LEVEL_ERROR, error),
                (LOG_LEVEL_INFO, info),
                (LOG_LEVEL_TRACE, trace),
            ] {
                let got = emitted_at_level(Some(&config(configured, INCLUDE_DETAIL_FULL)), record_level);
                assert_eq!(
                    got, expected,
                    "case a bus at {configured} and a {record_level} record: expected \
                     emitted={expected}, got {got}"
                );
            }
        }
        for record_level in [LOG_LEVEL_ERROR, LOG_LEVEL_INFO, LOG_LEVEL_TRACE] {
            let got = emitted_at_level(None, record_level);
            assert!(
                !got,
                "case a bus with no configuration and a {record_level} record: expected \
                 emitted=false, got {got}"
            );
        }
    }

    /// RULE: the event's own detail travels only under `IncludeDetail` `FULL`.
    #[test]
    fn the_event_detail_is_carried_only_under_full_detail() {
        for (include_detail, expected) in [("FULL", true), ("NONE", false)] {
            let record = ingest_record(
                Some(&config(LOG_LEVEL_INFO, include_detail)),
                "us-east-1",
                &IngestedEvent {
                    bus: "orders",
                    event_id: "0d1f",
                    caller_account_id: "000000000000",
                    source: Some("custom.orders"),
                    detail_type: Some("OrderPlaced"),
                    detail: Some(r#"{"qty":2}"#),
                },
                SystemTime::UNIX_EPOCH,
            )
            .unwrap_or_else(|| panic!("case {include_detail}: a bus at INFO owes a record"));
            let carried = record.details.contains_key(EVENT_DETAIL_DETAIL);
            assert_eq!(
                carried, expected,
                "case IncludeDetail {include_detail}: expected event_detail carried={expected}, \
                 got {carried}"
            );
            // The members that do not depend on `IncludeDetail` are carried either way.
            for member in [CALLER_ACCOUNT_ID_DETAIL, SOURCE_DETAIL, DETAIL_TYPE_DETAIL] {
                assert!(
                    record.details.contains_key(member),
                    "case IncludeDetail {include_detail}: expected the {member} detail, got \
                     {:?}",
                    record.details
                );
            }
        }
    }

    /// RULE: the record is one JSON object naming the bus, the message type, the level and
    /// the event it is about.
    #[test]
    fn one_record_is_one_json_object_of_the_buss_own_members() {
        let record = ingest_record(
            Some(&config(LOG_LEVEL_INFO, INCLUDE_DETAIL_FULL)),
            "eu-west-2",
            &IngestedEvent {
                bus: "orders",
                event_id: "7f2c",
                caller_account_id: "000000000000",
                source: Some("custom.orders"),
                detail_type: Some("OrderPlaced"),
                detail: Some(r#"{"qty":2}"#),
            },
            SystemTime::UNIX_EPOCH + ::std::time::Duration::from_millis(1_561_515_759_750),
        )
        .expect("a bus at INFO owes a record");
        let message = record.message();
        let parsed: ::serde_json::Value = ::serde_json::from_str(&message)
            .unwrap_or_else(|error| panic!("the record is JSON ({error}): {message}"));
        for (member, expected) in [
            (
                RESOURCE_ARN_FIELD,
                ::serde_json::json!(event_bus_arn("eu-west-2", "orders")),
            ),
            (EVENT_BUS_NAME_FIELD, ::serde_json::json!("orders")),
            (MESSAGE_TYPE_FIELD, ::serde_json::json!(EVENT_INGEST_SUCCESS)),
            (LOG_LEVEL_FIELD, ::serde_json::json!(LOG_LEVEL_INFO)),
            (EVENT_ID_FIELD, ::serde_json::json!("7f2c")),
            (
                MESSAGE_TIMESTAMP_MS_FIELD,
                ::serde_json::json!(1_561_515_759_750_i64),
            ),
        ] {
            assert_eq!(
                parsed[member], expected,
                "case the {member} member: expected {expected}, got {}",
                parsed[member]
            );
        }
        assert_eq!(
            parsed[DETAILS_FIELD][EVENT_DETAIL_DETAIL],
            ::serde_json::json!(r#"{"qty":2}"#),
            "case the carried detail: expected the published Detail, got {}",
            parsed[DETAILS_FIELD]
        );
    }

    /// RULE: the record about a handover that failed is an ERROR one naming the rule, the
    /// target and the status the invocation ended on — and it carries what the target was to
    /// be handed only under `IncludeDetail` `FULL`.
    #[test]
    fn a_failed_handover_is_reported_at_the_error_level() {
        // One row per (configured level, `IncludeDetail`) pair: a bus at `OFF` writes no
        // error record, and every level that covers them writes one.
        let envelope = r#"{"id":"7f2c","detail":{"qty":2}}"#;
        let failure = FailedInvocation {
            bus: "orders",
            event_id: "7f2c",
            rule_arn: "arn:aws:events:eu-west-2:000000000000:rule/orders/to-audit",
            target_arn: "arn:aws:logs:eu-west-2:000000000000:log-group:/aws/events/audit",
            error_code: super::NO_PERMISSIONS,
            error_message: "not authorized to perform: logs:PutLogEvents",
            envelope,
        };
        for (level, include_detail, written, carries_input) in [
            (LOG_LEVEL_OFF, INCLUDE_DETAIL_FULL, false, false),
            (LOG_LEVEL_ERROR, INCLUDE_DETAIL_FULL, true, true),
            (LOG_LEVEL_ERROR, "NONE", true, false),
            (LOG_LEVEL_INFO, INCLUDE_DETAIL_FULL, true, true),
            (LOG_LEVEL_TRACE, "NONE", true, false),
        ] {
            let case = format!("a bus at {level} with {include_detail} detail");
            let record = invocation_failure_record(
                Some(&config(level, include_detail)),
                "eu-west-2",
                &failure,
                SystemTime::UNIX_EPOCH,
            );
            let Some(record) = record else {
                assert!(!written, "case {case}: expected a record, got none");
                continue;
            };
            assert!(written, "case {case}: expected no record, got {record:?}");
            assert_eq!(
                (record.message_type.as_str(), record.log_level.as_str()),
                (INVOCATION_FAILURE, LOG_LEVEL_ERROR),
                "case {case}: expected an {INVOCATION_FAILURE} record at {LOG_LEVEL_ERROR}, \
                 got {record:?}"
            );
            for (member, expected) in [
                (RULE_ARN_DETAIL, ::serde_json::json!(failure.rule_arn)),
                (TARGET_ARN_DETAIL, ::serde_json::json!(failure.target_arn)),
                (
                    FINAL_INVOCATION_STATUS_DETAIL,
                    ::serde_json::json!(failure.error_code),
                ),
                (
                    ERROR_MESSAGE_DETAIL,
                    ::serde_json::json!(failure.error_message),
                ),
            ] {
                assert_eq!(
                    record.details.get(member),
                    Some(&expected),
                    "case {case}: expected the {member} detail to be {expected}, got {:?}",
                    record.details
                );
            }
            let carried = record.details.get(TARGET_INPUT_DETAIL);
            let expected = carries_input.then(|| ::serde_json::json!(envelope));
            assert_eq!(
                carried,
                expected.as_ref(),
                "case {case}: expected the carried input {expected:?}, got {carried:?}"
            );
        }
    }

    /// RULE: a record's own level selects the log types a delivery source carries it under.
    #[test]
    fn a_records_level_selects_the_log_types_that_carry_it() {
        for (record_level, expected) in [
            (
                LOG_LEVEL_ERROR,
                vec![ERROR_LOGS_LOG_TYPE, INFO_LOGS_LOG_TYPE],
            ),
            (LOG_LEVEL_INFO, vec![INFO_LOGS_LOG_TYPE]),
            (LOG_LEVEL_TRACE, vec![INFO_LOGS_LOG_TYPE]),
        ] {
            let got = carrying_log_types(record_level).to_vec();
            assert_eq!(
                got, expected,
                "case a {record_level} record: expected {expected:?}, got {got:?}"
            );
        }
    }
}
