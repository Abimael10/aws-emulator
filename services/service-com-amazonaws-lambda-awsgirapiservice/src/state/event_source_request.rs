// SPDX-License-Identifier: CC0-1.0

//! What a request about an event source mapping has to STATE, read in one place for every
//! operation that states it.
//!
//! Three questions are asked by more than one of `CreateEventSourceMapping`,
//! `UpdateEventSourceMapping` and `ListEventSourceMappings`, so each is answered here rather
//! than once per operation — two operations reading the same member two ways would accept a
//! value in one call and refuse it in the next:
//!
//! 1. **the event source the value names** ([`event_source_arn_refusal`]) — `EventSourceArn`
//!    is "the Amazon Resource Name (ARN) of the event source", and that member enumerates
//!    the source services and the resource of each one a mapping is read through, so a value
//!    that is no ARN at all, or names another vendor or another resource type of one of those
//!    vendors, names nothing a mapping could ever poll. Both the call that WIRES a mapping to
//!    a source and the call that looks mappings UP by their source hold the value to that
//!    enumeration, because a value naming no source is not a filter that happens to match
//!    nothing.
//! 2. **what a batch of a QUEUE may be** ([`queue_batch_window_refusal`]) — `BatchSize` is
//!    "the maximum number of records in each batch that Lambda pulls from your stream or
//!    queue", whose own "Related setting" states what a batch past ten takes: "When you set
//!    BatchSize to a value greater than 10, you must set MaximumBatchingWindowInSeconds to at
//!    least 1." A queue's default batching window is "0 seconds", so a mapping on a queue
//!    that asks for a larger batch without a window asks for a batch Lambda has no window to
//!    gather in; the stream sources default to a batch of 100 and are not held to it. A FIFO
//!    queue has no batching window at all — its messages are handed to one consumer in order
//!    — so asking to gather one is its own refusal, and the two together hold a FIFO queue's
//!    batch at ten.
//! 3. **whether the criteria can decide anything** ([`filter_criteria_refusal`]) — a
//!    `Filter$Pattern` is "a filter pattern", written in the event-pattern language this tree
//!    reads in exactly one place ([`::service_common::event_pattern`]), so text that is not a
//!    pattern at all would be stored as criteria no record could ever be matched against: the
//!    mapping's source would then be consumed by nothing. Both calls that STATE criteria
//!    refuse it instead.
//!
//! And one question is about the source RESOURCE rather than the value that names it
//! ([`absent_event_source_message`]): an Amazon MQ broker is consumed by connecting to the
//! endpoints of the named broker, which Lambda reads off the broker itself, so an
//! `EventSourceArn` naming a broker that does not exist is a NOT-FOUND condition rather than
//! a conflict or a bad parameter. The broker is looked for in the store of the service that
//! OWNS brokers, over the shared engine, exactly as a queue's own configuration is read out
//! of the queue service's store ([`super::queue_visibility_timeout_seconds`]): a broker
//! exists here exactly when that service reports it, so a mapping is never wired to a source
//! nothing could connect to.
//!
//! A READ of the request and of those stores: every answer here decides, and changes nothing.

use crate::ops::types::FilterCriteria;

/// The account field of an ARN FORMAT: the twelve-digit example id an ARN written as a
/// SHAPE carries, beside the placeholder names of its resource half, in every format AWS
/// spells out. It is the account half of an example rather than an account anything lives
/// in — which is why it is stated here instead of the account whose resources this emulator
/// holds ([`::engine::emulator_account_id`], the id every ARN this service MINTS carries).
pub const EXAMPLE_ACCOUNT_ID: &str = "123456789012";

/// The largest batch a mapping on a QUEUE gathers without a batching window, from
/// `BatchSize`'s own related setting: "When you set BatchSize to a value greater than 10, you
/// must set MaximumBatchingWindowInSeconds to at least 1."
pub const MAX_QUEUE_BATCH_SIZE_WITHOUT_WINDOW: i32 = 10;

/// The suffix the NAME of a FIFO queue ends with — the one thing that tells a FIFO queue's
/// ARN from a standard queue's, because Amazon SQS requires it of every FIFO queue's name.
const FIFO_QUEUE_SUFFIX: &str = ".fifo";

/// The sentence a mapping on a FIFO queue asking to gather its batch over a WINDOW is
/// refused with.
///
/// Entirely FIXED text: a FIFO queue hands its messages to one consumer in order, so there
/// is nothing to gather a batch over time for, and the setting is not one such a mapping has.
pub const FIFO_BATCHING_WINDOW_MESSAGE: &str = "Batching window is not supported for FIFO queues";

/// The sentence a mapping on a queue asking for a larger batch than that without a window is
/// refused with.
///
/// Entirely FIXED text: it states the requirement rather than the values sent, so a caller
/// compares it exactly.
pub const QUEUE_BATCH_WINDOW_MESSAGE: &str =
    "Maximum batch window in seconds must be greater than 0 if maximum batch size is greater \
     than 10";

/// The sentence a request whose `FilterCriteria` carry text that is not a filter pattern is
/// refused with.
///
/// Entirely FIXED text — it names neither the filter nor what the pattern got wrong — and it
/// states the one thing the value got wrong: the definition is not a pattern.
pub const INVALID_FILTER_PATTERN_MESSAGE: &str = "Invalid filter pattern definition.";

/// Why the `EventSourceArn` `event_source` names no event source a mapping could poll, or
/// `None` when it names one.
///
/// Two faults, each with its own sentence, because they tell a caller different things: a
/// value that is no ARN at all broke the MEMBER's own requirement, so it is stated as the
/// request-validation failure it is, and a well-formed ARN that names no pollable resource is
/// an event source Lambda does not recognize ([`unrecognized_event_source`]).
///
/// `region` is the Region the caller signed for, which the example ARN a refusal states is
/// written for: the resources of a mapping's event source live in the caller's own Region.
#[must_use]
pub fn event_source_arn_refusal(event_source: &str, region: &str) -> Option<String> {
    let Some(parsed) = ::service_common::arn::parse(event_source) else {
        return Some(format!(
            "Value '{event_source}' at 'eventSourceArn' failed to satisfy constraint: Member \
             must be the Amazon Resource Name of an event source"
        ));
    };
    (!super::names_event_source(&parsed)).then(|| {
        unrecognized_event_source(parsed.vendor, event_source, region)
    })
}

/// How a request whose `EventSourceArn` names nothing an event source mapping can poll is
/// refused.
///
/// Both refusals open with the same sentence — the source was not recognized — and then
/// state the ONE thing that distinguishes the two cases, which is what tooling matches on.
///
/// An ARN whose VENDOR is one of the source services that member enumerates DOES name a
/// service a mapping polls, so what the value got wrong is the SHAPE of the resource: the
/// refusal states the ARN format that service's pollable resource is addressed by
/// ([`expected_event_source_resource`]), because the missing or extra segment is the only
/// thing that tells the caller what to send instead. An ARN of any other vendor names no
/// source service at all, so there is no format to expect and nothing about the resource
/// half to state: the refusal says the source ARN itself is unsupported and quotes it, which
/// is the only variable part of that sentence.
///
/// What that format states is the SHAPE of the ARN rather than any one resource: the
/// resource half is written with the placeholder names the format uses
/// ([`expected_event_source_resource`]) and the account half with the example account id
/// ([`EXAMPLE_ACCOUNT_ID`]), because neither names a resource of anybody's. The ONE field of
/// it that is the caller's own is the Region, which the resources of a mapping's event source
/// live in, so the example is written for the Region this request was made in.
#[must_use]
pub fn unrecognized_event_source(vendor: &str, event_source: &str, region: &str) -> String {
    match expected_event_source_resource(vendor) {
        Some(resource) => format!(
            "Unrecognized event source. Expected arn of format \
             arn:aws:{vendor}:{region}:{EXAMPLE_ACCOUNT_ID}:{resource}"
        ),
        None => format!("Unrecognized event source. Unsupported source arn : {event_source}"),
    }
}

/// The RESOURCE field of the ARN that addresses the resource one source service is polled
/// through, for each vendor namespace `EventSourceArn` enumerates — and `None` for a vendor
/// that enumeration does not name.
///
/// One row per entry of that member's list, each written as that service writes the ARN of
/// the resource the entry names: "Amazon Kinesis – The ARN of the data stream or a stream
/// consumer", "Amazon DynamoDB Streams – The ARN of the stream" (the stream OF a table,
/// hence the table's own resource field plus the stream segment), "Amazon Simple Queue
/// Service – The ARN of the queue" (whose resource field is the queue's name), "Amazon
/// Managed Streaming for Apache Kafka – The ARN of the cluster" (the VPC connection the
/// same entry documents is its cross-account form rather than the shape a same-account
/// mapping names), "Amazon MQ – The ARN of the broker" and "Amazon DocumentDB – The ARN of
/// the DocumentDB change stream", which is an Amazon RDS cluster.
///
/// The placeholder names are the ones the format states for a shape rather than values of
/// any resource, so the sentence reads the same whatever the caller sent.
#[must_use]
pub fn expected_event_source_resource(vendor: &str) -> Option<&'static str> {
    Some(match vendor {
        super::EVENT_SOURCE_VENDOR_KINESIS => "stream/StreamName",
        super::EVENT_SOURCE_VENDOR_DYNAMODB => "table/TableName/stream/StreamId",
        super::EVENT_SOURCE_VENDOR_QUEUE => "QueueName",
        super::EVENT_SOURCE_VENDOR_KAFKA => "cluster/ClusterName/ClusterId",
        super::EVENT_SOURCE_VENDOR_MQ => "broker:brokerName:brokerId",
        super::EVENT_SOURCE_VENDOR_DOCUMENT_DB => "cluster:ClusterName",
        _ => return None,
    })
}

/// Lambda's refusal of what a mapping on a QUEUE would poll in — `None` for every pair a
/// queue may be polled in, and for every other kind of event source.
///
/// Two requirements, each with its own sentence, because they tell a caller different things:
///
/// * a FIFO queue has no batching WINDOW at all ([`FIFO_BATCHING_WINDOW_MESSAGE`]) — its
///   messages are handed to one consumer in order, so there is nothing to gather a batch over
///   time for, and a request asking to gather one is refused whatever its batch size;
/// * and a batch past [`MAX_QUEUE_BATCH_SIZE_WITHOUT_WINDOW`] takes a window past zero
///   ([`QUEUE_BATCH_WINDOW_MESSAGE`]), which is `BatchSize`'s own related setting. Together
///   they are what holds a FIFO queue's batch at that ceiling: a larger batch has no window to
///   gather it in, and stating one is the other refusal.
///
/// `event_source_arn` is the source the mapping polls — `None` for the self-managed cluster,
/// which is no queue — `batch_size` the batch the mapping would read in and `window_seconds`
/// its `MaximumBatchingWindowInSeconds`, each the value in force AFTER the call, so an update
/// that states one of the two is decided against the stored value of the other.
///
/// A queue's window defaults to "0 seconds", so a mapping on a queue that states no window
/// has none: the absent member is read as that zero rather than as an unknown.
#[must_use]
pub fn queue_batch_window_refusal(
    event_source_arn: Option<&str>,
    batch_size: i32,
    window_seconds: Option<i32>,
) -> Option<&'static str> {
    let queue = event_source_arn.filter(|arn| {
        super::event_source_vendor(arn) == Some(super::EVENT_SOURCE_VENDOR_QUEUE)
    })?;
    if names_fifo_queue(queue) && window_seconds.is_some_and(|window| window > 0) {
        return Some(FIFO_BATCHING_WINDOW_MESSAGE);
    }
    let gathers_without_a_window = window_seconds.unwrap_or(0) < 1;
    (batch_size > MAX_QUEUE_BATCH_SIZE_WITHOUT_WINDOW && gathers_without_a_window)
        .then_some(QUEUE_BATCH_WINDOW_MESSAGE)
}

/// Whether `event_source_arn` names a FIFO queue, which its NAME — the whole resource field
/// of a queue's ARN — ending in [`FIFO_QUEUE_SUFFIX`] is what says.
#[must_use]
pub fn names_fifo_queue(event_source_arn: &str) -> bool {
    ::service_common::arn::parse(event_source_arn)
        .is_some_and(|arn| arn.resource.ends_with(FIFO_QUEUE_SUFFIX))
}

/// Lambda's refusal of `criteria` carrying text that is not a filter pattern — `None` when
/// every pattern they carry is one, including criteria that carry no pattern at all.
///
/// The patterns are read by the ONE reader of the event-pattern language in this tree
/// ([`::service_common::event_pattern::parse`]), so a filter of an event source mapping and
/// an EventBridge rule's pattern admit the same documents and refuse the same text. Which
/// fault the text has is not in the sentence ([`INVALID_FILTER_PATTERN_MESSAGE`]), so the
/// reason the reader gives is not carried into it.
#[must_use]
pub fn filter_criteria_refusal(criteria: Option<&FilterCriteria>) -> Option<&'static str> {
    super::event_source_filter::patterns_of(criteria)
        .iter()
        .any(|pattern| ::service_common::event_pattern::parse(pattern).is_err())
        .then_some(INVALID_FILTER_PATTERN_MESSAGE)
}

/// The sentence a request whose `EventSourceArn` names a BROKER that does not exist is
/// refused with — `None` when the value names no broker at all, or when the broker it names
/// is one this emulator holds.
///
/// Lambda consumes an Amazon MQ source by connecting to the endpoints of the named broker,
/// which it reads off the broker itself, so a mapping onto a broker that does not exist is a
/// source that could never be reached: the condition is the ABSENCE of the resource the
/// request named, which is what the modelled `ResourceNotFoundException` states, rather than
/// a bad parameter or a conflict with another mapping.
///
/// The broker is looked for in the store of the service that OWNS brokers, reached through
/// the shared engine (the same way a queue's own configuration is read out of the queue
/// service's store), so a broker exists here exactly when that service reports it: the ARN
/// is resolved to the broker its resource field NAMES (`broker:<name>:<id>`), because a
/// broker name identifies one broker of an account — the id beside it is the one Amazon MQ
/// minted for that broker, which a caller holding an ARN of another account's spelling of
/// the same broker does not carry.
///
/// The sentence is FIXED text, naming the kind of resource that is missing and the request
/// member that named it rather than the value sent.
#[must_use]
pub fn absent_event_source_message<Eng: Send + Sync + 'static>(
    engine: &Eng,
    event_source_arn: &str,
) -> Option<&'static str> {
    use ::service_com_amazonaws_mq_mq as mq;

    let name = named_broker(event_source_arn)?;
    let brokers = mq::state::service_state(engine)?;
    let brokers = brokers.lock();
    brokers.broker_by_name(name).is_none().then_some(ABSENT_BROKER_MESSAGE)
}

/// The NAME of the Amazon MQ broker `event_source_arn` names — the first segment of the
/// resource field of a broker ARN, `broker:<name>:<id>` — and `None` for a value that names
/// no broker at all.
#[must_use]
pub fn named_broker(event_source_arn: &str) -> Option<&str> {
    let parsed = ::service_common::arn::parse(event_source_arn)?;
    if parsed.vendor != super::EVENT_SOURCE_VENDOR_MQ
        || parsed.resource_type() != super::EVENT_SOURCE_RESOURCE_BROKER
    {
        return None;
    }
    parsed
        .resource
        .strip_prefix(super::EVENT_SOURCE_RESOURCE_BROKER)?
        .strip_prefix(':')?
        .split(':')
        .next()
        .filter(|name| !name.is_empty())
}

/// See [`absent_event_source_message`]: how Lambda states that the broker the request named
/// does not exist.
pub const ABSENT_BROKER_MESSAGE: &str =
    "The broker resource: eventSourceArn you requested does not exist.";

/// The Amazon SQS call Lambda reads a QUEUE event source with: a mapping on a queue consumes
/// it by receiving its messages, so that is the call whose failure refuses the mapping.
pub const QUEUE_SOURCE_READ_CALL: &str = "ReceiveMessage";

/// How Lambda reports a failure the queue service answered its read of a queue event source
/// with: the call it attempted, and the error `code` and `message` it got back, verbatim.
///
/// Lambda wires a mapping onto a queue by READING that queue rather than by judging the
/// fields of the ARN that names it, so what refuses such a mapping is the queue service's own
/// answer. The sentence propagates that answer instead of restating it: a caller branches on
/// the queue service's error code, and reads out of its message the address or the queue the
/// read failed on — neither of which a refusal worded by Lambda itself would carry.
#[must_use]
pub fn propagated_queue_read_failure(code: &str, message: &str) -> String {
    format!(
        "Error occurred while {QUEUE_SOURCE_READ_CALL}. SQS Error Code: {code}. \
         SQS Error Message: {message}"
    )
}

/// Why the queue the event source `arn` names cannot be READ from here — `None` when the
/// address it resolves to is one this endpoint serves, and for every source that is no queue.
///
/// The ARN is resolved to the queue's ADDRESS, in the queue service's own url form
/// ([`::service_com_amazonaws_sqs_amazonsqs::state::queue_url_of_account`]) for the account
/// and the queue name the ARN carries, in the Region it carries — a queue belongs to one
/// Region's namespace, and an address of another Region names a different queue. Whether that
/// address is one anything here answers for is the queue service's own decision over the
/// address ([`::service_com_amazonaws_sqs_amazonsqs::state::queue_name_from_url`]), so a
/// mapping is held to what the address resolves to rather than to a comparison of the ARN's
/// account field: an address of another account is served by nothing here, which is the
/// invalid address that service refuses, and Lambda answers with that refusal reported as the
/// read it came from ([`propagated_queue_read_failure`]).
///
/// What this does NOT decide is whether the queue at an address this endpoint does serve
/// EXISTS: that is the queue's own absence, answered by the read of the queue itself.
#[must_use]
pub fn queue_event_source_read_refusal(arn: &::service_common::arn::Arn<'_>) -> Option<String> {
    use ::service_com_amazonaws_sqs_amazonsqs as sqs;

    if arn.vendor != super::EVENT_SOURCE_VENDOR_QUEUE {
        return None;
    }
    let address = sqs::state::queue_url_of_account(arn.region, arn.account, arn.resource);
    sqs::state::queue_name_from_url(&address)
        .is_none()
        .then(|| {
            propagated_queue_read_failure(
                sqs::state::INVALID_ADDRESS_CODE,
                &sqs::state::invalid_address_for_endpoint_message(&address),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{
        absent_event_source_message, event_source_arn_refusal, filter_criteria_refusal,
        named_broker, queue_batch_window_refusal, ABSENT_BROKER_MESSAGE,
        FIFO_BATCHING_WINDOW_MESSAGE, INVALID_FILTER_PATTERN_MESSAGE, QUEUE_BATCH_WINDOW_MESSAGE,
    };
    use crate::ops::types::{Filter, FilterCriteria};

    /// RULE: an `EventSourceArn` names an event source only when it is an ARN of one of the
    /// source services that member enumerates AND of the resource type that service is
    /// polled through; anything else is refused, with the expected ARN format for a source
    /// service and as an unsupported source ARN for any other vendor.
    ///
    /// The rows differ along what the rule generalises over: which vendor the value names,
    /// whether its resource type is the pollable one, and whether it is an ARN at all.
    #[test]
    fn only_the_enumerated_sources_and_resource_types_name_an_event_source() {
        // (case, the value, the refusal expected — `None` for a value that names a source)
        let rows: Vec<(&str, &str, Option<&str>)> = vec![
            (
                "a queue",
                "arn:aws:sqs:eu-west-1:000000000000:orders",
                None,
            ),
            (
                "a data stream",
                "arn:aws:kinesis:eu-west-1:000000000000:stream/records",
                None,
            ),
            (
                "a table's stream",
                "arn:aws:dynamodb:eu-west-1:000000000000:table/orders/stream/2026-01-01T00:00:00.000",
                None,
            ),
            (
                "the table rather than its stream",
                "arn:aws:dynamodb:eu-west-1:000000000000:table/orders",
                Some(
                    "Unrecognized event source. Expected arn of format \
                     arn:aws:dynamodb:eu-west-1:123456789012:table/TableName/stream/StreamId",
                ),
            ),
            (
                "a broker",
                "arn:aws:mq:eu-west-1:000000000000:broker:orders:b-1",
                None,
            ),
            (
                "a vendor that is no source service",
                "arn:aws:sns:eu-west-1:000000000000:orders",
                Some(
                    "Unrecognized event source. Unsupported source arn : \
                     arn:aws:sns:eu-west-1:000000000000:orders",
                ),
            ),
            (
                "no arn at all",
                "orders",
                Some(
                    "Value 'orders' at 'eventSourceArn' failed to satisfy constraint: Member \
                     must be the Amazon Resource Name of an event source",
                ),
            ),
        ];
        for (case, value, expected) in rows {
            let refusal = event_source_arn_refusal(value, "eu-west-1");
            assert_eq!(
                refusal.as_deref(),
                expected,
                "case {case:?}: expected the refusal {expected:?}, got {refusal:?}"
            );
        }
    }

    /// RULE: a mapping on a QUEUE whose batch is past ten takes a batching window past zero,
    /// and one on a FIFO queue takes no batching window at all; no other kind of source is
    /// held to either.
    ///
    /// The rows differ along what the rule generalises over: the batch size either side of
    /// the ceiling, whether a window is stated and what it is, whether the queue is a FIFO
    /// one, and which source service the mapping polls.
    #[test]
    fn a_queue_batch_past_ten_takes_a_batching_window() {
        /// One row: the case, the mapping's event source, the batch size, the batching
        /// window, and the sentence the pair is refused with.
        struct Row {
            case: &'static str,
            event_source: Option<&'static str>,
            batch_size: i32,
            window: Option<i32>,
            refusal: Option<&'static str>,
        }
        let row = |case, event_source, batch_size, window, refusal| Row {
            case,
            event_source,
            batch_size,
            window,
            refusal,
        };
        const QUEUE: &str = "arn:aws:sqs:eu-west-1:000000000000:orders";
        const FIFO_QUEUE: &str = "arn:aws:sqs:eu-west-1:000000000000:orders.fifo";
        const DATA_STREAM: &str = "arn:aws:kinesis:eu-west-1:000000000000:stream/orders";
        const TABLE_STREAM: &str =
            "arn:aws:dynamodb:eu-west-1:000000000000:table/orders/stream/2026-01-01T00:00:00.000";
        const CLUSTER: &str =
            "arn:aws:kafka:eu-west-1:000000000000:cluster/orders/11111111-2222-3333-4444-555555555555-1";
        let past_ten = Some(QUEUE_BATCH_WINDOW_MESSAGE);
        let no_window = Some(FIFO_BATCHING_WINDOW_MESSAGE);
        let rows: Vec<Row> = vec![
            row("a queue at the ceiling without a window", Some(QUEUE), 10, None, None),
            row("a queue past the ceiling without a window", Some(QUEUE), 11, None, past_ten),
            row("a queue past the ceiling with a zero window", Some(QUEUE), 11, Some(0), past_ten),
            row("a queue past the ceiling with a window", Some(QUEUE), 11, Some(1), None),
            row(
                "a queue at the published batch maximum with a window",
                Some(QUEUE),
                10_000,
                Some(300),
                None,
            ),
            row(
                "a queue at the published batch maximum without one",
                Some(QUEUE),
                10_000,
                None,
                past_ten,
            ),
            row("a fifo queue at the ceiling without a window", Some(FIFO_QUEUE), 10, None, None),
            row(
                "a fifo queue past the ceiling without a window",
                Some(FIFO_QUEUE),
                11,
                None,
                past_ten,
            ),
            row(
                "a fifo queue past the ceiling with a window",
                Some(FIFO_QUEUE),
                11,
                Some(1),
                no_window,
            ),
            row(
                "a fifo queue at the ceiling with a window",
                Some(FIFO_QUEUE),
                10,
                Some(5),
                no_window,
            ),
            row(
                "a data stream past the ceiling without a window",
                Some(DATA_STREAM),
                100,
                None,
                None,
            ),
            row(
                "a table's stream at the published maximum",
                Some(TABLE_STREAM),
                10_000,
                Some(0),
                None,
            ),
            row(
                "a cluster past the ceiling without a window",
                Some(CLUSTER),
                100,
                None,
                None,
            ),
            row("a self-managed cluster, which names no source arn", None, 100, None, None),
        ];
        for Row {
            case,
            event_source,
            batch_size,
            window,
            refusal: expected,
        } in rows
        {
            let refusal = queue_batch_window_refusal(event_source, batch_size, window);
            assert_eq!(
                refusal, expected,
                "case {case:?}: expected {expected:?}, got {refusal:?}"
            );
        }
    }

    /// RULE: criteria are refused when any pattern they carry is not a filter pattern, and
    /// accepted when every one of them is — criteria carrying none included.
    ///
    /// The rows differ along what the rule generalises over: how many patterns the criteria
    /// carry, which of them is unreadable, and what makes it unreadable.
    #[test]
    fn criteria_carrying_text_that_is_not_a_pattern_are_refused() {
        let pattern = |text: &str| Filter {
            pattern: Some(text.to_string()),
        };
        let criteria = |patterns: Vec<&str>| {
            Some(FilterCriteria {
                filters: Some(patterns.iter().map(|text| pattern(text)).collect()),
            })
        };
        // (case, the criteria, whether they are refused)
        let rows: Vec<(&str, Option<FilterCriteria>, bool)> = vec![
            ("no criteria at all", None, false),
            ("criteria carrying no filter", Some(FilterCriteria { filters: None }), false),
            ("one pattern", criteria(vec![r#"{"eventName":["INSERT"]}"#]), false),
            (
                "a nested pattern",
                criteria(vec![r#"{"dynamodb":{"Keys":{"pk":{"S":["row-1"]}}}}"#]),
                false,
            ),
            ("text that is not json", criteria(vec!["INSERT"]), true),
            ("json that is not an object", criteria(vec!["[1, 2]"]), true),
            (
                "a member holding a bare value rather than a clause",
                criteria(vec![r#"{"eventName":"INSERT"}"#]),
                true,
            ),
            (
                "an unreadable pattern beside a readable one",
                criteria(vec![r#"{"eventName":["INSERT"]}"#, "{"]),
                true,
            ),
        ];
        for (case, criteria, expected) in rows {
            let refusal = filter_criteria_refusal(criteria.as_ref());
            assert_eq!(
                refusal,
                expected.then_some(INVALID_FILTER_PATTERN_MESSAGE),
                "case {case:?}: expected a refusal: {expected}, got {refusal:?}"
            );
        }
    }

    /// RULE: a source ARN is reported absent only when it names a broker, and only when the
    /// broker store holds no broker of that name.
    ///
    /// The rows differ along what the rule generalises over: the vendor of the source, the
    /// resource type inside that vendor, and whether the value is an ARN at all. (That a
    /// broker the store DOES hold is not reported absent is asserted end to end, through
    /// Amazon MQ's own `CreateBroker`, in
    /// `tests/all/event_source_mapping_request_rules.rs`.)
    #[test]
    fn only_a_broker_no_broker_store_holds_is_reported_absent() {
        let engine = ::engine::InMemoryEngine::new();
        // (case, the source ARN, the message expected over an empty broker store)
        let rows: Vec<(&str, &str, Option<&str>)> = vec![
            (
                "a broker no store holds",
                "arn:aws:mq:eu-west-1:000000000000:broker:orders:b-1",
                Some(ABSENT_BROKER_MESSAGE),
            ),
            (
                "a broker named with another id",
                "arn:aws:mq:eu-west-1:000000000000:broker:orders:b-2222",
                Some(ABSENT_BROKER_MESSAGE),
            ),
            (
                "an mq configuration rather than a broker",
                "arn:aws:mq:eu-west-1:000000000000:configuration:orders:c-1",
                None,
            ),
            (
                "a queue, which is not a broker",
                "arn:aws:sqs:eu-west-1:000000000000:orders",
                None,
            ),
            ("no arn at all", "orders", None),
        ];
        for (case, event_source, expected) in rows {
            let message = absent_event_source_message(&engine, event_source);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
        assert_eq!(
            named_broker("arn:aws:mq:eu-west-1:000000000000:broker:orders:b-1"),
            Some("orders"),
            "the broker an ARN names is the first segment of its resource field"
        );
    }
}
