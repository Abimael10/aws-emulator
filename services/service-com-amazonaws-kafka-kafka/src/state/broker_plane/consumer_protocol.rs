// SPDX-License-Identifier: CC0-1.0

//! The CONSUMER EMBEDDED PROTOCOL a member of a consumer group speaks: the protocol type and
//! the assignor a consuming client joins under, and the bytes its SUBSCRIPTION and its
//! ASSIGNMENT travel as.
//!
//! A group's members are opaque to the coordinator — it carries each member's subscription and
//! assignment as bytes and hands them to the member that computes the assignment
//! ([`super::groups`]) — but a READER of the group decodes them: `DescribeGroups` on a group
//! whose `ProtocolType` is [`CONSUMER_PROTOCOL_TYPE`] reports what each member subscribed to
//! and the partitions it holds, which every Apache Kafka admin client reads with the consumer
//! protocol's own schema. So a consumer that runs IN this process and joins a group here
//! states those two in that same encoding, rather than in bytes of its own that no reader
//! could decode.
//!
//! The encoding is Apache Kafka's `ConsumerProtocol`: a two-byte VERSION, then the
//! subscription (the topics, and the member's own opaque data) or the assignment (the
//! partitions of each topic the member holds, and that same opaque data) as the protocol's own
//! message. Both are written by the protocol implementation this crate serves its whole wire
//! with, so what an admin client reads back is what that implementation encodes.

use ::bytes::{Bytes, BytesMut};
use ::kafka_protocol::messages::{
    consumer_protocol_assignment::{ConsumerProtocolAssignment, TopicPartition},
    consumer_protocol_subscription::ConsumerProtocolSubscription,
    TopicName,
};
use ::kafka_protocol::protocol::{Encodable, StrBytes};

/// The `ProtocolType` a CONSUMING client joins a group under, which is what tells a reader of
/// the group that its members' subscriptions and assignments are the consumer protocol's
/// (Apache Kafka's `ConsumerProtocol.PROTOCOL_TYPE`).
pub const CONSUMER_PROTOCOL_TYPE: &str = "consumer";

/// The assignor a consumer names when it joins: Apache Kafka's RANGE assignor, the
/// `partition.assignment.strategy` default of a consuming client
/// (`org.apache.kafka.clients.consumer.RangeAssignor`, whose protocol name is `range`) — the
/// name a group reports as the protocol its members agreed on.
pub const RANGE_ASSIGNOR: &str = "range";

/// The version these two are written at: the first version of the consumer embedded protocol,
/// whose two documents every consumer client reads, and the only one that carries exactly the
/// subscription's topics and the assignment's partitions.
pub const CONSUMER_PROTOCOL_VERSION: i16 = 0;

/// The bytes a member's SUBSCRIPTION travels as: the topics it subscribed to, in the consumer
/// protocol's own encoding.
#[must_use]
pub fn subscription_bytes(topics: &[String]) -> Bytes {
    let subscription = ConsumerProtocolSubscription::default()
        .with_topics(
            topics
                .iter()
                .map(|topic| StrBytes::from_string(topic.clone()))
                .collect(),
        )
        .with_user_data(None);
    encoded(&subscription)
}

/// The bytes a member's ASSIGNMENT travels as: the partitions of each topic it holds, in that
/// same encoding.
#[must_use]
pub fn assignment_bytes(assigned: &[(String, Vec<i32>)]) -> Bytes {
    let assignment = ConsumerProtocolAssignment::default()
        .with_assigned_partitions(
            assigned
                .iter()
                .map(|(topic, partitions)| {
                    TopicPartition::default()
                        .with_topic(TopicName(StrBytes::from_string(topic.clone())))
                        .with_partitions(partitions.clone())
                })
                .collect(),
        )
        .with_user_data(None);
    encoded(&assignment)
}

/// One of those two documents as the member's bytes: the protocol version, then the document
/// itself at that version.
///
/// A document the implementation cannot encode leaves the member carrying nothing, which is
/// what a client that stated none carries — never half a document a reader would fail to
/// decode.
fn encoded<Document: Encodable>(document: &Document) -> Bytes {
    let mut written = BytesMut::new();
    written.extend_from_slice(&CONSUMER_PROTOCOL_VERSION.to_be_bytes());
    match document.encode(&mut written, CONSUMER_PROTOCOL_VERSION) {
        Ok(()) => written.freeze(),
        Err(_unencodable) => Bytes::new(),
    }
}

#[cfg(test)]
mod tests {
    use ::bytes::Buf as _;
    use ::kafka_protocol::protocol::Decodable;

    use super::*;

    /// One topic and the partitions of it a member holds, as a row below states them.
    type Assigned = Vec<(String, Vec<i32>)>;

    /// What a READER of the group makes of `bytes`: the version they open with, and the rest
    /// read back as the document that version carries — the way an admin client reading the
    /// group decodes a member's subscription or assignment.
    fn decoded<Document: Decodable>(bytes: &Bytes) -> (i16, Document) {
        let mut held = bytes.clone();
        let version = held.get_i16();
        let document = Document::decode(&mut held, version).expect("a document a client decodes");
        assert!(
            !held.has_remaining(),
            "the document must carry nothing past its own fields, got {} byte(s) over",
            held.remaining()
        );
        (version, document)
    }

    /// RULE: a member's subscription carries the protocol version and then the topics it
    /// subscribed to, so a reader of the group decodes exactly what the consumer subscribed
    /// to.
    ///
    /// The rows differ along what the encoding generalises over: how many topics a
    /// subscription names, and the names themselves.
    #[test]
    fn a_subscription_decodes_as_the_topics_it_names() {
        // (case, the topics)
        let rows: Vec<(&str, Vec<String>)> = vec![
            ("one topic", vec!["orders".to_string()]),
            (
                "two topics",
                vec!["orders".to_string(), "shipments".to_string()],
            ),
            (
                "a name carrying the separators a topic name admits",
                vec!["order-events.v1_2".to_string()],
            ),
            ("no topic at all", Vec::new()),
        ];
        for (case, topics) in rows {
            let bytes = subscription_bytes(&topics);
            let (version, subscription) = decoded::<ConsumerProtocolSubscription>(&bytes);
            assert_eq!(
                version, CONSUMER_PROTOCOL_VERSION,
                "case {case:?}: the subscription must open with the protocol version, got \
                 {bytes:?}"
            );
            let named: Vec<String> = subscription
                .topics
                .iter()
                .map(|topic| topic.to_string())
                .collect();
            assert_eq!(
                named, topics,
                "case {case:?}: expected the topics {topics:?}, got {named:?}"
            );
            assert_eq!(
                subscription.user_data, None,
                "case {case:?}: a subscription this consumer states carries no opaque data"
            );
        }
    }

    /// RULE: a member's assignment carries the protocol version and then the partitions of
    /// each topic it holds, so a reader of the group decodes exactly what the consumer was
    /// given.
    ///
    /// The rows differ along what the encoding generalises over: how many topics an assignment
    /// holds, and how many partitions of each.
    #[test]
    fn an_assignment_decodes_as_the_partitions_it_holds() {
        // (case, the assignment)
        let rows: Vec<(&str, Assigned)> = vec![
            (
                "one partition of one topic",
                vec![("orders".to_string(), vec![0])],
            ),
            (
                "several partitions of one topic",
                vec![("orders".to_string(), vec![0, 1, 2])],
            ),
            (
                "partitions of two topics",
                vec![
                    ("orders".to_string(), vec![0]),
                    ("shipments".to_string(), vec![0, 1]),
                ],
            ),
            ("no partition at all", Vec::new()),
        ];
        for (case, assigned) in rows {
            let bytes = assignment_bytes(&assigned);
            let (version, assignment) = decoded::<ConsumerProtocolAssignment>(&bytes);
            assert_eq!(
                version, CONSUMER_PROTOCOL_VERSION,
                "case {case:?}: the assignment must open with the protocol version, got {bytes:?}"
            );
            let held: Assigned = assignment
                .assigned_partitions
                .iter()
                .map(|topic| (topic.topic.to_string(), topic.partitions.clone()))
                .collect();
            assert_eq!(
                held, assigned,
                "case {case:?}: expected the assignment {assigned:?}, got {held:?}"
            );
            assert_eq!(
                assignment.user_data, None,
                "case {case:?}: an assignment this consumer states carries no opaque data"
            );
        }
    }
}
