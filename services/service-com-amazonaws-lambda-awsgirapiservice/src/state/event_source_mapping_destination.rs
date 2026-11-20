// SPDX-License-Identifier: CC0-1.0

//! WHICH destinations an event source mapping may hand a record of a failed batch to, and
//! whether the invoked function's EXECUTION ROLE may write the one a request names.
//!
//! `DestinationConfig` is "a configuration object that specifies the destination of an event
//! after Lambda processes it", and the destination inside it is a resource Lambda WRITES to:
//! `OnFailure$Destination` is "the Amazon Resource Name (ARN) of the destination resource",
//! and the resources it may name are enumerated — "you can configure an Amazon SNS topic,
//! Amazon SQS queue, Amazon S3 bucket, or Kafka topic as the destination". A value of any
//! other service is a resource Lambda has no delivery to make to, so a mapping stored with
//! one would report a dead-letter path that nothing could ever write a dropped batch to.
//!
//! So the two calls that state a mapping's destinations ([`crate::ops::create_event_source_mapping`]
//! and [`crate::ops::update_event_source_mapping`]) hold every destination a request names
//! to that enumeration, and refuse one it does not name with the modelled
//! `InvalidParameterValueException` before anything is stored or changed. They hold it to one
//! thing more: Lambda writes the record acting as the function's EXECUTION ROLE, so a
//! destination whose write the policies in force on that role withhold is refused with that
//! same modelled error ([`unwritable_mapping_destination`]) rather than stored as a
//! dead-letter path no record could reach.
//!
//! The one kind of that enumeration that is no ARN carries rules of its own, for the reason
//! it is not one: a `kafka://<topic>` destination is a topic of the mapping's own cluster,
//! produced to by the mapping's own pollers, which only PROVISIONED MODE provides, and it may
//! not be a topic that mapping consumes — so the same two calls hold a Kafka topic to both
//! ([`unsupported_kafka_topic_destination`]).
//!
//! A READ of the request and of the stores the decision is made from: it decides, and changes
//! nothing.

use crate::ops::types::{DestinationConfig, ProvisionedPollerConfig};

/// The ARN vendor fields of the destination services an event source mapping delivers a
/// record of a failed batch to, as [`service_common::arn::vendor`] reads one.
///
/// One entry per kind `OnFailure$Destination` enumerates for the sources a mapping's
/// destination configuration applies to: "you can configure an Amazon SNS topic, Amazon SQS
/// queue, Amazon S3 bucket, or Kafka topic as the destination" — the Kafka topic being the
/// one of the four that is not an ARN at all (see [`KAFKA_TOPIC_SCHEME`]).
const MAPPING_DESTINATION_VENDORS: &[&str] = &["sns", "sqs", "s3"];

/// How a Kafka topic destination is addressed: the one non-ARN form `DestinationArn`'s own
/// pattern admits (`^$|kafka://([^.]([a-zA-Z0-9\-_.]{0,248}))|arn:…`).
const KAFKA_TOPIC_SCHEME: &str = "kafka://";

/// Whether `destination` names one of the resources a mapping's destination may be (see
/// [`MAPPING_DESTINATION_VENDORS`]).
///
/// A value that is no ARN and no Kafka topic URI names no resource of any service, so it is
/// not one of them either.
#[must_use]
pub fn is_supported_mapping_destination(destination: &str) -> bool {
    if destination.starts_with(KAFKA_TOPIC_SCHEME) {
        return true;
    }
    service_common::arn::vendor(destination)
        .is_some_and(|vendor| MAPPING_DESTINATION_VENDORS.contains(&vendor))
}

/// The sentence a request whose `DestinationConfig` names a destination of a service a
/// mapping delivers nothing to is refused with.
///
/// Entirely FIXED text — it names neither the destination nor the kinds that ARE delivered
/// to, so a caller compares it exactly — and it states the one thing the value got wrong:
/// the SERVICE the destination belongs to is not one a mapping delivers to.
pub const UNSUPPORTED_MAPPING_DESTINATION_MESSAGE: &str =
    "The service specified by the TargetArn isn't supported as a destination.";

/// The refusal for a request whose `DestinationConfig` names a destination of a service a
/// mapping delivers nothing to — and `None` when every destination it names is one of them,
/// which includes a configuration naming none at all.
#[must_use]
pub fn unsupported_mapping_destination(
    destination_config: Option<&DestinationConfig>,
) -> Option<String> {
    // The destinations a request STATES, in the one reading of a `DestinationConfig` this
    // crate has — which holds the empty string the member's pattern admits as no destination
    // at all, so taking a destination away is not refused as a destination of nothing.
    super::requested_destinations(destination_config)
        .iter()
        .any(|destination| !is_supported_mapping_destination(destination))
        .then(|| UNSUPPORTED_MAPPING_DESTINATION_MESSAGE.to_string())
}

/// The topic a `kafka://<topic>` destination names, and `None` for a destination of any
/// other form.
///
/// The scheme is all the form carries beside the topic: no cluster, no account and no
/// Region ([`KAFKA_TOPIC_SCHEME`]), so the whole remainder of the value is the topic's
/// name.
#[must_use]
pub fn kafka_destination_topic(destination: &str) -> Option<&str> {
    destination.strip_prefix(KAFKA_TOPIC_SCHEME)
}

/// The sentence a request stating a Kafka-topic destination for a mapping that is not in
/// provisioned mode is refused with.
///
/// Entirely FIXED text — it names neither the topic nor the mapping, so a caller compares it
/// exactly — and it states both the rule the request broke and the member that satisfies it,
/// which is the only way a caller learns what to send instead. RECORDED from AWS's own answer
/// to a create whose on-failure destination was a Kafka topic and whose request stated no
/// `ProvisionedPollerConfig` (the corpus unit
/// `lambda__secretsmanager.kafka-esm-onfailure-destination-kafka-topic-rules-rejected`).
pub const KAFKA_DESTINATION_WITHOUT_PROVISIONED_MODE_MESSAGE: &str =
    "Kafka as a DestinationConfig is only available for provisioned mode. To use Kafka as a \
     DestinationConfig, enable provisioned mode by specifying MinimumPollers in \
     ProvisionedPollerConfig, and try again.";

/// The sentence a request stating a Kafka-topic destination that names a topic the mapping
/// itself CONSUMES is refused with, for the topic `topic` both halves name.
///
/// The fixed text states the one thing the value got wrong — the topic a record of a failed
/// batch is produced to has to be another topic than the one the batch was read from — and
/// the variable part is the topic NAME the request stated on both sides, which is what an
/// operator changes. RECORDED from AWS's own answer to a create whose on-failure destination
/// named the topic its `Topics` named (the corpus unit
/// `lambda__secretsmanager.kafka-esm-onfailure-destination-kafka-topic-rules-rejected`).
#[must_use]
pub fn kafka_destination_is_a_consumed_topic_message(topic: &str) -> String {
    format!("The destination topic name cannot be the same as the source topic name: {topic}.")
}

/// The refusal for a request whose `DestinationConfig` states a Kafka-topic destination the
/// mapping it configures could not deliver to — and `None` when it states none, which
/// includes a configuration naming no destination at all.
///
/// A Kafka topic is the one destination kind `OnFailure$Destination` enumerates that is not a
/// resource of another service but a topic of the mapping's own cluster, produced to by the
/// mapping's own pollers. Two things follow, and both are decided by the calls that STATE a
/// mapping's destinations ([`crate::ops::create_event_source_mapping`],
/// [`crate::ops::update_event_source_mapping`]) before anything is stored or changed, rather
/// than kept as a dead-letter path no record would reach:
///
/// * that delivery is only made by the pollers PROVISIONED MODE provides — "Use Provisioned
///   Mode to customize the minimum and maximum number of event pollers for your event source"
///   (`ProvisionedPollerConfig`), whose `MinimumPollers` is what a request turns that mode on
///   with — so a Kafka topic is admitted only for a mapping in that mode;
/// * the topic it names may not be one the mapping CONSUMES (`Topics`: "The name of the Kafka
///   topic"), since a record of a batch produced back into the topic that batch was read from
///   is handed straight back to the mapping that gave up on it.
///
/// Both halves are judged against the mapping AFTER this call: `provisioned_poller_config` is
/// what the request states, or what the mapping already holds for a request that states none,
/// and `topics` are the mapping's own, which no update changes — exactly as the pair
/// `BatchSize`'s own related setting binds is judged after the call
/// ([`super::queue_batch_window_refusal`]).
///
/// Both slots of the configuration are judged, exactly as the kind of a destination is
/// ([`unsupported_mapping_destination`]): a Kafka topic is the same value in either.
///
/// A READ of the request and of the stored mapping: it decides, and changes nothing.
#[must_use]
pub fn unsupported_kafka_topic_destination(
    destination_config: Option<&DestinationConfig>,
    provisioned_poller_config: Option<&ProvisionedPollerConfig>,
    topics: Option<&[String]>,
) -> Option<String> {
    // The destinations a request STATES, in the one reading of a `DestinationConfig` this
    // crate has — which holds the empty string the member's pattern admits as no destination
    // at all, so taking a destination away states no Kafka topic.
    let destinations = super::requested_destinations(destination_config);
    let stated: Vec<&str> = destinations
        .iter()
        .filter_map(|destination| kafka_destination_topic(destination))
        .collect();
    if stated.is_empty() {
        return None;
    }
    let provisioned =
        provisioned_poller_config.is_some_and(|config| config.minimum_pollers.is_some());
    if !provisioned {
        return Some(KAFKA_DESTINATION_WITHOUT_PROVISIONED_MODE_MESSAGE.to_string());
    }
    let consumed = topics.unwrap_or_default();
    stated.iter().find_map(|topic| {
        consumed
            .iter()
            .find(|consumed| consumed == topic)
            .map(|consumed| kafka_destination_is_a_consumed_topic_message(consumed))
    })
}

/// The sentence a request whose `DestinationConfig` names a failure destination the invoked
/// function's EXECUTION ROLE may not write is refused with — and `None` when that role may
/// write it, or when nothing of the kind can be shown.
///
/// Lambda writes the record of a batch a mapping gave up on ACTING AS that role — "Lambda
/// reads items from the event source and invokes the function"
/// ([`CreateEventSourceMapping`](crate::ops::create_event_source_mapping)) under the
/// function's own execution role — so a destination whose write the policies in force on it
/// withhold is a dead-letter path no record could ever reach: the question is settled by the
/// call that CONFIGURES the pair, before anything is persisted, exactly as it is settled for
/// the event SOURCE the mapping reads ([`super::event_source_authorization`]) and for the
/// destination an asynchronous invocation's record is delivered to
/// ([`super::event_invoke_config::unwritable_destination`]).
///
/// The slot decided is the ON-FAILURE one, and only it: that is the one a mapping's record is
/// written to ([`super::event_source_poller::failure_destination`]), so it is the only write
/// a role has to be able to make for the mapping to work. An `OnSuccess` destination is not
/// judged here — this emulator writes no record to it, so withholding a mapping for it would
/// refuse a configuration on a write nothing makes.
///
/// On the same evidence as the gates above, so nothing is refused on evidence this emulator
/// does not have: an engine carrying no IAM store, a role ARN that store holds no role for, a
/// role carrying a grant this emulator cannot read, and a destination kind no write is stated
/// for each refuse nothing.
///
/// A READ of both stores: it decides, and changes nothing.
#[must_use]
pub fn unwritable_mapping_destination<Eng: Send + Sync + 'static>(
    engine: &Eng,
    role_arn: &str,
    destination_config: Option<&DestinationConfig>,
) -> Option<String> {
    let destination = super::event_source_poller::failure_destination::on_failure_destination(
        destination_config,
    )?;
    let iam = super::event_source_authorization::iam_state(engine)?;
    let (action, service) = super::event_invoke_config::withheld_destination_write(
        &iam.lock(),
        role_arn,
        &destination,
    )?;
    Some(super::event_source_authorization::function_role_refusal_message(action, service))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::{OnFailure, OnSuccess};

    /// A destination is judged on the SERVICE it names: each kind the member enumerates is
    /// accepted whatever resource of that service it addresses, and a resource of any other
    /// service — or a value that is no resource at all — is not.
    #[test]
    fn only_the_enumerated_destination_kinds_are_supported() {
        // (case, the destination, whether a mapping may deliver to it)
        let rows: [(&str, &str, bool); 12] = [
            (
                "an Amazon SNS topic",
                "arn:aws:sns:us-east-1:123456789012:failed-batches",
                true,
            ),
            (
                "an Amazon SQS queue",
                "arn:aws:sqs:eu-west-1:123456789012:order-dlq",
                true,
            ),
            ("an Amazon S3 bucket", "arn:aws:s3:::failed-batches", true),
            ("a Kafka topic", "kafka://failed-batches", true),
            (
                "an Amazon DynamoDB table",
                "arn:aws:dynamodb:us-east-1:123456789012:table/orders",
                false,
            ),
            (
                "an Amazon Kinesis data stream",
                "arn:aws:kinesis:us-east-1:123456789012:stream/orders",
                false,
            ),
            (
                "a Lambda function",
                "arn:aws:lambda:us-east-1:123456789012:function:on-failure",
                false,
            ),
            (
                "an Amazon EventBridge event bus",
                "arn:aws:events:us-east-1:123456789012:event-bus/failures",
                false,
            ),
            (
                "an Amazon CloudWatch Logs log group",
                "arn:aws:logs:us-east-1:123456789012:log-group:/aws/lambda/failures",
                false,
            ),
            (
                "an ARN of a partition of its own",
                "arn:aws-us-gov:sqs:us-gov-west-1:123456789012:order-dlq",
                true,
            ),
            ("a name that is no ARN at all", "order-dlq", false),
            ("a URI of another scheme", "https://example.org/", false),
        ];

        for (case, destination, supported) in rows {
            let decided = is_supported_mapping_destination(destination);
            assert_eq!(
                decided, supported,
                "case {case:?}: expected is_supported_mapping_destination({destination:?}) to be \
                 {supported:?}, got {decided:?}"
            );
        }
    }

    /// The refusal is over the configuration as a whole: a configuration naming a
    /// destination no mapping delivers to is refused with the one fixed sentence, and one
    /// naming only supported destinations — or none — is refused with nothing.
    #[test]
    fn a_configuration_is_refused_for_the_destination_no_mapping_delivers_to() {
        let topic = "arn:aws:sns:us-east-1:123456789012:failed-batches";
        let table = "arn:aws:dynamodb:us-east-1:123456789012:table/orders";

        // (case, the configuration, whether it is refused)
        let rows: [(&str, Option<DestinationConfig>, bool); 6] = [
            ("no configuration at all", None, false),
            (
                "a configuration naming no slot",
                Some(DestinationConfig {
                    on_success: None,
                    on_failure: None,
                }),
                false,
            ),
            (
                "an on-failure slot naming a supported destination",
                Some(destination_config(None, Some(topic))),
                false,
            ),
            (
                "an on-failure slot cleared with the empty destination the pattern admits",
                Some(destination_config(None, Some(""))),
                false,
            ),
            (
                "an on-failure slot naming a destination of another service",
                Some(destination_config(None, Some(table))),
                true,
            ),
            (
                "an on-success slot naming a destination of another service",
                Some(destination_config(Some(table), Some(topic))),
                true,
            ),
        ];

        for (case, config, refused) in rows {
            let message = unsupported_mapping_destination(config.as_ref());
            let expected =
                refused.then(|| UNSUPPORTED_MAPPING_DESTINATION_MESSAGE.to_string());
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }

    /// A `DestinationConfig` stating the two slots, each with the destination given.
    fn destination_config(
        on_success: Option<&str>,
        on_failure: Option<&str>,
    ) -> DestinationConfig {
        DestinationConfig {
            on_success: on_success.map(|destination| OnSuccess {
                destination: Some(destination.to_string()),
            }),
            on_failure: on_failure.map(|destination| OnFailure {
                destination: Some(destination.to_string()),
            }),
        }
    }

    /// A Kafka-topic destination is judged on the MODE the mapping would deliver in and on
    /// the topics it consumes: it stands for a mapping whose provisioned-poller configuration
    /// states `MinimumPollers` and names a topic that mapping does not consume, and is refused
    /// for one that is not in provisioned mode or that consumes the topic named. A destination
    /// of any other form is not judged by this rule at all.
    #[test]
    fn a_kafka_topic_destination_takes_provisioned_mode_and_another_topic() {
        let consumed = ["order-events".to_string()];
        let dotted = ["order-events.retry".to_string()];

        /// One row: the destination stated, the provisioned-poller configuration and the
        /// consumed topics in force after the call, and the refusal the three earn.
        struct Row<'row> {
            case: &'row str,
            destination: &'row str,
            provisioned: Option<ProvisionedPollerConfig>,
            topics: &'row [String],
            refusal: Option<String>,
        }

        let rows = [
            Row {
                case: "a Kafka topic for a mapping in provisioned mode",
                destination: "kafka://failed-batches",
                provisioned: Some(pollers(Some(2), None)),
                topics: &consumed,
                refusal: None,
            },
            Row {
                case: "a Kafka topic for a mapping whose mode states both poller bounds",
                destination: "kafka://order-events.failed",
                provisioned: Some(pollers(Some(1), Some(10))),
                topics: &consumed,
                refusal: None,
            },
            Row {
                case: "a Kafka topic for a mapping that consumes no topic",
                destination: "kafka://failed-batches",
                provisioned: Some(pollers(Some(1), None)),
                topics: &[],
                refusal: None,
            },
            Row {
                case: "a Kafka topic for a mapping with no poller configuration at all",
                destination: "kafka://failed-batches",
                provisioned: None,
                topics: &consumed,
                refusal: Some(KAFKA_DESTINATION_WITHOUT_PROVISIONED_MODE_MESSAGE.to_string()),
            },
            Row {
                case: "a Kafka topic for a configuration that states no minimum",
                destination: "kafka://failed-batches",
                provisioned: Some(pollers(None, Some(10))),
                topics: &consumed,
                refusal: Some(KAFKA_DESTINATION_WITHOUT_PROVISIONED_MODE_MESSAGE.to_string()),
            },
            Row {
                case: "the topic a provisioned mapping consumes",
                destination: "kafka://order-events",
                provisioned: Some(pollers(Some(1), None)),
                topics: &consumed,
                refusal: Some(kafka_destination_is_a_consumed_topic_message("order-events")),
            },
            Row {
                case: "the dotted topic a provisioned mapping consumes",
                destination: "kafka://order-events.retry",
                provisioned: Some(pollers(Some(1), Some(4))),
                topics: &dotted,
                refusal: Some(kafka_destination_is_a_consumed_topic_message(
                    "order-events.retry",
                )),
            },
            Row {
                case: "the consumed topic for a mapping not in provisioned mode, the mode \
                       first",
                destination: "kafka://order-events",
                provisioned: None,
                topics: &consumed,
                refusal: Some(KAFKA_DESTINATION_WITHOUT_PROVISIONED_MODE_MESSAGE.to_string()),
            },
            Row {
                case: "a queue, which this rule does not judge",
                destination: "arn:aws:sqs:us-east-1:000000000000:order-dlq",
                provisioned: None,
                topics: &consumed,
                refusal: None,
            },
            Row {
                case: "a bucket, which this rule does not judge",
                destination: "arn:aws:s3:::failed-batches",
                provisioned: None,
                topics: &consumed,
                refusal: None,
            },
            Row {
                case: "no destination at all, the empty string the pattern admits",
                destination: "",
                provisioned: None,
                topics: &consumed,
                refusal: None,
            },
        ];

        for row in rows {
            let case = row.case;
            let config = destination_config(None, Some(row.destination));
            let message = unsupported_kafka_topic_destination(
                Some(&config),
                row.provisioned.as_ref(),
                Some(row.topics),
            );
            assert_eq!(
                message, row.refusal,
                "case {case:?}: expected {:?}, got {message:?}",
                row.refusal
            );
        }
    }

    /// A `ProvisionedPollerConfig` stating the poller bounds given.
    fn pollers(minimum: Option<i32>, maximum: Option<i32>) -> ProvisionedPollerConfig {
        ProvisionedPollerConfig {
            minimum_pollers: minimum,
            maximum_pollers: maximum,
            poller_group_name: None,
        }
    }

    /// The rule is over the configuration as a whole, so a Kafka topic stated in the
    /// ON-SUCCESS slot is judged exactly as one in the on-failure slot.
    #[test]
    fn a_kafka_topic_in_either_slot_is_judged() {
        let config = destination_config(Some("kafka://failed-batches"), None);
        let message = unsupported_kafka_topic_destination(Some(&config), None, None);
        assert_eq!(
            message.as_deref(),
            Some(KAFKA_DESTINATION_WITHOUT_PROVISIONED_MODE_MESSAGE),
            "a Kafka topic in the on-success slot must be judged alike, got {message:?}"
        );
    }
}
