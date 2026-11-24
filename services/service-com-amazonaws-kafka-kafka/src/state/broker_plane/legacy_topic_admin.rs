// SPDX-License-Identifier: CC0-1.0

//! The OLDEST versions of the TOPIC-ADMINISTRATION APIs, framed here.
//!
//! `CreateTopics` from version 0 and `DeleteTopics` from version 0 are versions every
//! Apache Kafka 3.x broker answers and the protocol implementation behind this endpoint
//! cannot carry — the rule, and why those frames are written in this crate at all, is
//! [`super::legacy_frames`]'s, which holds the same framing for the produce and fetch
//! halves of the same client's round trip and the pre-flexible readers and writers both
//! files frame with.
//!
//! Here: the two frames an admin client administers topics with, and nothing else, so the
//! administration's own frames sit beside each other rather than inside the data plane's.

use ::bytes::{BufMut, Bytes, BytesMut};
use ::kafka_protocol::messages::{
    create_topics_request::{CreatableReplicaAssignment, CreatableTopic, CreatableTopicConfig},
    BrokerId, CreateTopicsRequest, CreateTopicsResponse, DeleteTopicsRequest, DeleteTopicsResponse,
    TopicName,
};
use ::kafka_protocol::protocol::StrBytes;

use super::legacy_frames::{
    boolean, count, int16, int32, nullable_string, put_count, put_nullable_string, put_string,
    string,
};

/// Read a `CreateTopics` request of version 0 or 1 out of `body`, as the implementation's
/// own request type — so everything behind the frame is the shared administration.
///
/// `None` for a body that is not that version's request, which is a frame the broker
/// cannot answer at all.
#[must_use]
pub fn create_topics_request(body: &mut Bytes, version: i16) -> Option<CreateTopicsRequest> {
    let mut topics = Vec::new();
    for _ in 0..count(body)? {
        let name = string(body)?;
        let num_partitions = int32(body)?;
        let replication_factor = int16(body)?;
        let mut assignments = Vec::new();
        for _ in 0..count(body)? {
            let partition_index = int32(body)?;
            let mut broker_ids = Vec::new();
            for _ in 0..count(body)? {
                broker_ids.push(BrokerId(int32(body)?));
            }
            assignments.push(
                CreatableReplicaAssignment::default()
                    .with_partition_index(partition_index)
                    .with_broker_ids(broker_ids),
            );
        }
        let mut configs = Vec::new();
        for _ in 0..count(body)? {
            let name = string(body)?;
            let value = nullable_string(body)?;
            configs.push(
                CreatableTopicConfig::default()
                    .with_name(StrBytes::from_string(name))
                    .with_value(value.map(StrBytes::from_string)),
            );
        }
        topics.push(
            CreatableTopic::default()
                .with_name(TopicName(StrBytes::from_string(name)))
                .with_num_partitions(num_partitions)
                .with_replication_factor(replication_factor)
                .with_assignments(assignments)
                .with_configs(configs),
        );
    }
    let timeout_ms = int32(body)?;
    // `ValidateOnly` arrives from version 1 on; version 0 carries no such member, and a
    // request that does not state it asks for the topics to be created.
    let validate_only = match version >= 1 {
        true => boolean(body)?,
        false => false,
    };
    Some(
        CreateTopicsRequest::default()
            .with_topics(topics)
            .with_timeout_ms(timeout_ms)
            .with_validate_only(validate_only),
    )
}

/// Write `response` as version 0 or 1 of the `CreateTopics` response: the per-topic name
/// and error code, plus the error message from version 1 on. Neither version carries a
/// throttle time, which is why the newer encoding cannot be reused.
#[must_use]
pub fn create_topics_response(response: &CreateTopicsResponse, version: i16) -> Vec<u8> {
    let mut encoded = BytesMut::new();
    put_count(&mut encoded, response.topics.len());
    for topic in response.topics.iter() {
        put_string(&mut encoded, topic.name.0.as_str());
        encoded.put_i16(topic.error_code);
        if version >= 1 {
            put_nullable_string(
                &mut encoded,
                topic.error_message.as_ref().map(|message| message.as_str()),
            );
        }
    }
    encoded.to_vec()
}

/// Read a `DeleteTopics` request of version 0 out of `body`, as the implementation's own
/// request type.
#[must_use]
pub fn delete_topics_request(body: &mut Bytes, _version: i16) -> Option<DeleteTopicsRequest> {
    let mut names = Vec::new();
    for _ in 0..count(body)? {
        names.push(TopicName(StrBytes::from_string(string(body)?)));
    }
    let timeout_ms = int32(body)?;
    Some(
        DeleteTopicsRequest::default()
            .with_topic_names(names)
            .with_timeout_ms(timeout_ms),
    )
}

/// Write `response` as version 0 of the `DeleteTopics` response: the per-topic name and
/// error code, with no throttle time.
#[must_use]
pub fn delete_topics_response(response: &DeleteTopicsResponse, _version: i16) -> Vec<u8> {
    let mut encoded = BytesMut::new();
    put_count(&mut encoded, response.responses.len());
    for topic in response.responses.iter() {
        put_string(
            &mut encoded,
            topic.name.as_ref().map(|name| name.0.as_str()).unwrap_or(""),
        );
        encoded.put_i16(topic.error_code);
    }
    encoded.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a request written in one of these versions is read back member for member,
    /// and a response written in it carries exactly the members that version declares.
    #[test]
    fn a_pre_flexible_topic_admin_frame_round_trips() {
        // The rows span the two versions `CreateTopics` is framed in here: version 0,
        // which carries no `ValidateOnly` and whose answer carries no error message, and
        // version 1, which carries both.
        for (case, version, validate_only) in [
            ("CreateTopics v0", 0_i16, false),
            ("CreateTopics v1", 1_i16, true),
        ] {
            let mut frame = BytesMut::new();
            put_count(&mut frame, 1);
            put_string(&mut frame, "orders");
            frame.put_i32(-1);
            frame.put_i16(-1);
            put_count(&mut frame, 0);
            put_count(&mut frame, 1);
            put_string(&mut frame, "retention.ms");
            put_nullable_string(&mut frame, Some("5000"));
            frame.put_i32(1_000);
            if version >= 1 {
                frame.put_u8(u8::from(validate_only));
            }
            let mut body = frame.freeze();
            let request = create_topics_request(&mut body, version)
                .unwrap_or_else(|| panic!("case {case:?}: the request must be readable"));
            assert_eq!(
                (
                    request.topics.len(),
                    request.topics[0].name.0.as_str().to_string(),
                    request.topics[0].num_partitions,
                    request.topics[0].replication_factor,
                    request.topics[0].configs.len(),
                    request.timeout_ms,
                    request.validate_only,
                ),
                (1, "orders".to_string(), -1, -1, 1, 1_000, validate_only),
                "case {case:?}: every member of the request must be read back"
            );
            assert!(
                body.is_empty(),
                "case {case:?}: the whole frame must be consumed, {} bytes left",
                body.len()
            );

            let message = "Topic 'orders' already exists.";
            let answered = CreateTopicsResponse::default().with_topics(vec![
                ::kafka_protocol::messages::create_topics_response::CreatableTopicResult::default()
                    .with_name(TopicName(StrBytes::from_static_str("orders")))
                    .with_error_code(36)
                    .with_error_message(Some(StrBytes::from_static_str(message))),
            ]);
            let expected = match version {
                0 => 4 + 2 + 6 + 2,
                _ => 4 + 2 + 6 + 2 + 2 + message.len(),
            };
            assert_eq!(
                create_topics_response(&answered, version).len(),
                expected,
                "case {case:?}: expected a {expected}-byte answer"
            );
        }

        // `DeleteTopics` version 0: the names and the timeout in, the names and the error
        // codes out.
        let case = "DeleteTopics v0";
        let mut frame = BytesMut::new();
        put_count(&mut frame, 2);
        put_string(&mut frame, "orders");
        put_string(&mut frame, "payments");
        frame.put_i32(2_000);
        let mut body = frame.freeze();
        let request = delete_topics_request(&mut body, 0)
            .unwrap_or_else(|| panic!("case {case:?}: the request must be readable"));
        assert_eq!(
            (
                request
                    .topic_names
                    .iter()
                    .map(|name| name.0.as_str().to_string())
                    .collect::<Vec<_>>(),
                request.timeout_ms,
            ),
            (vec!["orders".to_string(), "payments".to_string()], 2_000),
            "case {case:?}: every member of the request must be read back"
        );
        assert!(
            body.is_empty(),
            "case {case:?}: the whole frame must be consumed, {} bytes left",
            body.len()
        );
        let answered = DeleteTopicsResponse::default().with_responses(vec![
            ::kafka_protocol::messages::delete_topics_response::DeletableTopicResult::default()
                .with_name(Some(TopicName(StrBytes::from_static_str("orders"))))
                .with_error_code(3),
        ]);
        assert_eq!(
            delete_topics_response(&answered, 0).len(),
            4 + 2 + 6 + 2,
            "case {case:?}: the answer carries the name and the error code and nothing else"
        );

        // A one-byte frame is no request at all, in either API: it is refused rather than
        // read as a shorter one. (Every other malformed form is
        // `super::legacy_frames`'s own row.)
        let mut body = Bytes::from_static(&[0]);
        assert!(
            create_topics_request(&mut body, 0).is_none()
                && delete_topics_request(&mut Bytes::from_static(&[0]), 0).is_none(),
            "a frame too short to be a request must be refused"
        );
    }
}
