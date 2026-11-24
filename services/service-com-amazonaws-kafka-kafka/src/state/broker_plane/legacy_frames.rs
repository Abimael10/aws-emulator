// SPDX-License-Identifier: CC0-1.0

//! The OLDEST versions of the data-plane APIs, framed here.
//!
//! What a broker serves is decided by the Apache Kafka release it runs, and every 3.x
//! release — the versions Amazon MSK offers for a new cluster, and the one a cluster here
//! reports as its `KafkaVersion` — answers `Produce`, `Fetch`, `CreateTopics` and
//! `DeleteTopics` from version 0 ("validVersions" of each request in the Apache Kafka
//! message definitions of those releases). The protocol implementation this endpoint reads
//! every other frame with has dropped them: it tracks the definitions of the 4.x line,
//! where the oldest versions of every API were removed, so the versions it can carry start
//! at 3 for `Produce`, 4 for `Fetch`, 2 for `CreateTopics` and 1 for `DeleteTopics`.
//!
//! A client picks the version it sends, and one that frames the protocol ITSELF — the
//! raw-protocol probe a VPC-confined runner carries, a tool of an older distribution, a
//! connector pinned to an older client library — picks the OLDEST form of every request,
//! because that is the one with no tagged fields, no compact strings and no record batch
//! to write. A broker that does not serve the version it sends answers by closing the
//! connection, which is what Apache Kafka itself does with a frame it will not parse: such
//! a client would be dropped with no error code to branch on where a real broker of the
//! same release answers it, and the whole produce/fetch round trip would be unreachable
//! for it.
//!
//! So those frames are read and written HERE, and the endpoint advertises them. The
//! formats are the pre-flexible ones (an `INT32` count before every array, an `INT16`
//! length before every string, no tagged fields), which is why no part of the protocol
//! implementation can be reused for them. Everything BEHIND the frame is shared: the
//! decoded request is the implementation's own request type, so one produce, one fetch and
//! one topic administration serve every version a client can send, and the answer is the
//! same response value the newer versions are encoded from — narrowed to the members the
//! negotiated version declares. The records themselves are converted to the message format
//! that version carries by the record layer ([`super::records::fetch_magic`]).

use ::bytes::{BufMut, Bytes, BytesMut};
use ::kafka_protocol::messages::{
    fetch_request::{FetchPartition, FetchTopic},
    produce_request::{PartitionProduceData, TopicProduceData},
    ApiKey, BrokerId, CreateTopicsRequest, CreateTopicsResponse, DeleteTopicsRequest,
    DeleteTopicsResponse, FetchRequest, FetchResponse, ProduceRequest, ProduceResponse, TopicName,
};
use ::kafka_protocol::protocol::{Message, StrBytes};

/// The request-header version every version framed here carries: the header with a
/// `ClientId` and no tagged fields, which is the one every pre-flexible request carries
/// (Apache Kafka protocol, request header v1).
const PRE_FLEXIBLE_REQUEST_HEADER: i16 = 1;

/// The response-header version they carry: the correlation id alone (response header v0).
const PRE_FLEXIBLE_RESPONSE_HEADER: i16 = 0;

/// The oldest version of each of these APIs an Apache Kafka 3.x broker answers, as that
/// release's message definitions declare it.
const OLDEST_VERSION: i16 = 0;

/// The most entries one frame framed here may carry, per array it names: a process-safety
/// bound, not a protocol limit — the counts come from the request, so without a ceiling one
/// frame would size the work of reading it. What the request then ASKS for is bounded by
/// the produce, the fetch and the administration behind it, exactly as it is for every
/// other version.
const MAX_ENTRIES: usize = 4096;

/// One version of one API this module frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// `Produce` at a version below the protocol implementation's floor.
    Produce(i16),
    /// `Fetch` at a version below that floor.
    Fetch(i16),
    /// `CreateTopics` at a version below it.
    CreateTopics(i16),
    /// `DeleteTopics` at a version below it.
    DeleteTopics(i16),
}

impl Framing {
    /// Whether `version` of `key` is a version framed here: one an Apache Kafka 3.x broker
    /// answers that the protocol implementation cannot carry.
    ///
    /// Decided against that implementation's OWN declared range, so a version it gains
    /// support for stops being framed here rather than being framed twice.
    #[must_use]
    pub fn of(key: ApiKey, version: i16) -> Option<Self> {
        if !(OLDEST_VERSION..floor(key)?).contains(&version) {
            return None;
        }
        match key {
            ApiKey::Produce => Some(Self::Produce(version)),
            ApiKey::Fetch => Some(Self::Fetch(version)),
            ApiKey::CreateTopics => Some(Self::CreateTopics(version)),
            ApiKey::DeleteTopics => Some(Self::DeleteTopics(version)),
            _ => None,
        }
    }
}

/// The oldest version of `key` the protocol implementation can carry, request and response
/// both, or `None` for an API this module frames nothing of.
fn floor(key: ApiKey) -> Option<i16> {
    match key {
        ApiKey::Produce => Some(ProduceRequest::VERSIONS.min.max(ProduceResponse::VERSIONS.min)),
        ApiKey::Fetch => Some(FetchRequest::VERSIONS.min.max(FetchResponse::VERSIONS.min)),
        ApiKey::CreateTopics => Some(
            CreateTopicsRequest::VERSIONS
                .min
                .max(CreateTopicsResponse::VERSIONS.min),
        ),
        ApiKey::DeleteTopics => Some(
            DeleteTopicsRequest::VERSIONS
                .min
                .max(DeleteTopicsResponse::VERSIONS.min),
        ),
        _ => None,
    }
}

/// The version range of `key` this endpoint serves, once the versions framed here are
/// added to the ones the protocol implementation carries (`carried`): the same range with
/// its floor lowered to the oldest version an Apache Kafka 3.x broker answers.
///
/// `None` for an API whose implementation-carried range is empty, which is an API this
/// endpoint then advertises nothing for — a version this module frames is still answered
/// out of the implementation's own request and response types, so a key it cannot carry at
/// all is not one this module can serve either.
#[must_use]
pub fn lowered(key: ApiKey, carried: Option<(i16, i16)>) -> Option<(i16, i16)> {
    let (min, max) = carried?;
    match floor(key) {
        Some(_) => Some((min.min(OLDEST_VERSION), max)),
        None => Some((min, max)),
    }
}

/// The request-header version a frame framed here carries, or `None` for a pair this
/// module does not frame.
#[must_use]
pub fn request_header_version(key: ApiKey, version: i16) -> Option<i16> {
    Framing::of(key, version).map(|_| PRE_FLEXIBLE_REQUEST_HEADER)
}

/// The response header one of these answers opens with.
#[must_use]
pub fn response_header_version() -> i16 {
    PRE_FLEXIBLE_RESPONSE_HEADER
}

/// Read a `Produce` request of version 0, 1 or 2 out of `body`, as the implementation's
/// own request type — so everything behind the frame is the shared produce.
///
/// `None` for a body that is not that version's request, which is a frame the broker
/// cannot answer at all.
#[must_use]
pub fn produce_request(body: &mut Bytes, _version: i16) -> Option<ProduceRequest> {
    let acks = int16(body)?;
    let timeout_ms = int32(body)?;
    let mut topic_data = Vec::new();
    for _ in 0..count(body)? {
        let name = string(body)?;
        let mut partition_data = Vec::new();
        for _ in 0..count(body)? {
            let index = int32(body)?;
            let records = nullable_bytes(body)?;
            partition_data.push(
                PartitionProduceData::default()
                    .with_index(index)
                    .with_records(records),
            );
        }
        topic_data.push(
            TopicProduceData::default()
                .with_name(TopicName(StrBytes::from_string(name)))
                .with_partition_data(partition_data),
        );
    }
    Some(
        ProduceRequest::default()
            .with_acks(acks)
            .with_timeout_ms(timeout_ms)
            .with_topic_data(topic_data),
    )
}

/// Write `response` as version 0, 1 or 2 of the `Produce` response: the per-partition
/// offset and error code, the instant the broker stamped the batch at from version 2 on,
/// and the throttle time — which this response carries LAST, from version 1 on.
#[must_use]
pub fn produce_response(response: &ProduceResponse, version: i16) -> Vec<u8> {
    let mut encoded = BytesMut::new();
    put_count(&mut encoded, response.responses.len());
    for topic in response.responses.iter() {
        put_string(&mut encoded, topic.name.0.as_str());
        put_count(&mut encoded, topic.partition_responses.len());
        for partition in topic.partition_responses.iter() {
            encoded.put_i32(partition.index);
            encoded.put_i16(partition.error_code);
            encoded.put_i64(partition.base_offset);
            if version >= 2 {
                encoded.put_i64(partition.log_append_time_ms);
            }
        }
    }
    if version >= 1 {
        encoded.put_i32(response.throttle_time_ms);
    }
    encoded.to_vec()
}

/// Read a `Fetch` request of version 0 to 3 out of `body`, as the implementation's own
/// request type.
#[must_use]
pub fn fetch_request(body: &mut Bytes, version: i16) -> Option<FetchRequest> {
    let replica_id = int32(body)?;
    let max_wait_ms = int32(body)?;
    let min_bytes = int32(body)?;
    // `MaxBytes` — the ceiling over the WHOLE answer — arrived with version 3; a request
    // below it bounds only each partition.
    let max_bytes = match version >= 3 {
        true => int32(body)?,
        false => i32::MAX,
    };
    let mut topics = Vec::new();
    for _ in 0..count(body)? {
        let name = string(body)?;
        let mut partitions = Vec::new();
        for _ in 0..count(body)? {
            let partition = int32(body)?;
            let fetch_offset = int64(body)?;
            let partition_max_bytes = int32(body)?;
            partitions.push(
                FetchPartition::default()
                    .with_partition(partition)
                    .with_fetch_offset(fetch_offset)
                    .with_partition_max_bytes(partition_max_bytes),
            );
        }
        topics.push(
            FetchTopic::default()
                .with_topic(TopicName(StrBytes::from_string(name)))
                .with_partitions(partitions),
        );
    }
    Some(
        FetchRequest::default()
            .with_replica_id(BrokerId(replica_id))
            .with_max_wait_ms(max_wait_ms)
            .with_min_bytes(min_bytes)
            .with_max_bytes(max_bytes)
            .with_topics(topics),
    )
}

/// Write `response` as version 0 to 3 of the `Fetch` response: the throttle time — which
/// this response carries FIRST, from version 1 on — and then each partition's error code,
/// high watermark and records.
///
/// The RECORDS themselves are already in the message format this version carries: the
/// record layer converted them to it ([`super::records::fetch_magic`]).
#[must_use]
pub fn fetch_response(response: &FetchResponse, version: i16) -> Vec<u8> {
    let mut encoded = BytesMut::new();
    if version >= 1 {
        encoded.put_i32(response.throttle_time_ms);
    }
    put_count(&mut encoded, response.responses.len());
    for topic in response.responses.iter() {
        put_string(&mut encoded, topic.topic.0.as_str());
        put_count(&mut encoded, topic.partitions.len());
        for partition in topic.partitions.iter() {
            encoded.put_i32(partition.partition_index);
            encoded.put_i16(partition.error_code);
            encoded.put_i64(partition.high_watermark);
            put_nullable_bytes(&mut encoded, partition.records.as_ref());
        }
    }
    encoded.to_vec()
}

/// Read a `CreateTopics` request of version 0 or 1 out of `body`, as the implementation's
/// own request type.
#[must_use]
pub fn create_topics_request(body: &mut Bytes, version: i16) -> Option<CreateTopicsRequest> {
    super::legacy_topic_admin::create_topics_request(body, version)
}

/// Write `response` as version 0 or 1 of the `CreateTopics` response.
#[must_use]
pub fn create_topics_response(response: &CreateTopicsResponse, version: i16) -> Vec<u8> {
    super::legacy_topic_admin::create_topics_response(response, version)
}

/// Read a `DeleteTopics` request of version 0 out of `body`, as the implementation's own
/// request type.
#[must_use]
pub fn delete_topics_request(body: &mut Bytes, version: i16) -> Option<DeleteTopicsRequest> {
    super::legacy_topic_admin::delete_topics_request(body, version)
}

/// Write `response` as version 0 of the `DeleteTopics` response.
#[must_use]
pub fn delete_topics_response(response: &DeleteTopicsResponse, version: i16) -> Vec<u8> {
    super::legacy_topic_admin::delete_topics_response(response, version)
}

/// The length of one array, as a pre-flexible frame states it: an `INT32` count. A
/// negative count is a null array, which is read as an empty one — the member it stands
/// for carries nothing either way — and a count past [`MAX_ENTRIES`] is a frame this
/// endpoint will not read.
pub(super) fn count(body: &mut Bytes) -> Option<usize> {
    let stated = int32(body)?;
    let count = usize::try_from(stated).unwrap_or(0);
    (count <= MAX_ENTRIES).then_some(count)
}

/// Read one `INT16`.
pub(super) fn int16(body: &mut Bytes) -> Option<i16> {
    let bytes = take(body, 2)?;
    Some(i16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Read one `INT32`.
pub(super) fn int32(body: &mut Bytes) -> Option<i32> {
    let bytes = take(body, 4)?;
    Some(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Read one `INT64`.
fn int64(body: &mut Bytes) -> Option<i64> {
    let bytes = take(body, 8)?;
    Some(i64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}

/// Read one `BOOLEAN`: one byte, false only when it is zero.
pub(super) fn boolean(body: &mut Bytes) -> Option<bool> {
    Some(take(body, 1)?[0] != 0)
}

/// Read one `STRING`: an `INT16` length and that many bytes of UTF-8.
pub(super) fn string(body: &mut Bytes) -> Option<String> {
    nullable_string(body)?.or(Some(String::new()))
}

/// Read one `NULLABLE_STRING`, whose length is `-1` when it carries no value.
pub(super) fn nullable_string(body: &mut Bytes) -> Option<Option<String>> {
    let length = int16(body)?;
    if length < 0 {
        return Some(None);
    }
    let bytes = take(body, usize::try_from(length).ok()?)?;
    Some(Some(String::from_utf8(bytes.to_vec()).ok()?))
}

/// Read one `NULLABLE_BYTES`: an `INT32` length and that many bytes, where `-1` is the
/// ABSENCE of the field — which is how a produce states a partition it sends no records
/// for.
fn nullable_bytes(body: &mut Bytes) -> Option<Option<Bytes>> {
    let length = int32(body)?;
    if length < 0 {
        return Some(None);
    }
    Some(Some(take(body, usize::try_from(length).ok()?)?))
}

/// Split `count` bytes off the front of `body`, or `None` when it holds fewer.
fn take(body: &mut Bytes, count: usize) -> Option<Bytes> {
    (body.len() >= count).then(|| body.split_to(count))
}

/// Write one array's `INT32` count.
pub(super) fn put_count(encoded: &mut BytesMut, count: usize) {
    encoded.put_i32(i32::try_from(count).unwrap_or(i32::MAX));
}

/// Write one `STRING`.
pub(super) fn put_string(encoded: &mut BytesMut, value: &str) {
    encoded.put_i16(i16::try_from(value.len()).unwrap_or(i16::MAX));
    encoded.put_slice(value.as_bytes());
}

/// Write one `NULLABLE_STRING`.
pub(super) fn put_nullable_string(encoded: &mut BytesMut, value: Option<&str>) {
    match value {
        None => encoded.put_i16(-1),
        Some(value) => put_string(encoded, value),
    }
}

/// Write one `NULLABLE_BYTES`.
fn put_nullable_bytes(encoded: &mut BytesMut, value: Option<&Bytes>) {
    match value {
        None => encoded.put_i32(-1),
        Some(bytes) => {
            encoded.put_i32(i32::try_from(bytes.len()).unwrap_or(i32::MAX));
            encoded.put_slice(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a version an Apache Kafka 3.x broker answers and the protocol implementation
    /// cannot carry is framed here, and one the implementation carries is not.
    #[test]
    fn the_versions_framed_here_are_the_ones_the_implementation_dropped() {
        // The rows span the four APIs and the three kinds of version: the oldest, the
        // implementation's own floor, and one above it.
        let rows: &[(&str, ApiKey)] = &[
            ("Produce", ApiKey::Produce),
            ("Fetch", ApiKey::Fetch),
            ("CreateTopics", ApiKey::CreateTopics),
            ("DeleteTopics", ApiKey::DeleteTopics),
        ];
        for (case, key) in rows {
            let floor = floor(*key).unwrap_or_else(|| panic!("case {case:?}: {key:?} has a floor"));
            assert!(
                floor > OLDEST_VERSION,
                "case {case:?}: the implementation carries {key:?} from v{floor}, so there is \
                 nothing for this module to frame"
            );
            // Every version below the floor is framed here, so the advertised range has
            // no hole in it.
            for version in OLDEST_VERSION..floor {
                assert!(
                    Framing::of(*key, version).is_some(),
                    "case {case:?}: v{version} is below the floor and must be framed here"
                );
            }
            assert!(
                Framing::of(*key, floor).is_none(),
                "case {case:?}: v{floor} is carried by the implementation and must not be \
                 framed here"
            );
        }
        // An API this module frames nothing of is framed by the implementation at every
        // version, and so is a version below zero (which no client can send).
        assert!(
            Framing::of(ApiKey::Metadata, 0).is_none(),
            "an API this module frames nothing of must not be framed here"
        );
        assert!(
            Framing::of(ApiKey::Produce, -1).is_none(),
            "a version below the oldest one must not be framed here"
        );
    }

    /// RULE: the advertised range opens at the oldest version a 3.x broker answers, and
    /// keeps the newest version the implementation carries.
    #[test]
    fn the_advertised_range_opens_at_the_oldest_version_a_broker_answers() {
        assert_eq!(
            lowered(ApiKey::Produce, Some((3, 9))),
            Some((OLDEST_VERSION, 9)),
            "Produce must be advertised from its oldest answered version"
        );
        assert_eq!(
            lowered(ApiKey::Fetch, Some((4, 12))),
            Some((OLDEST_VERSION, 12)),
            "Fetch must be advertised from its oldest answered version"
        );
        assert_eq!(
            lowered(ApiKey::CreateTopics, Some((2, 5))),
            Some((OLDEST_VERSION, 5)),
            "CreateTopics must be advertised from its oldest answered version"
        );
        assert_eq!(
            lowered(ApiKey::DeleteTopics, Some((1, 5))),
            Some((OLDEST_VERSION, 5)),
            "DeleteTopics must be advertised from its oldest answered version"
        );
        assert_eq!(
            lowered(ApiKey::Metadata, Some((0, 12))),
            Some((0, 12)),
            "an API this module frames nothing of keeps the range the implementation carries"
        );
        assert_eq!(
            lowered(ApiKey::CreateTopics, None),
            None,
            "an API the implementation carries nothing of is advertised not at all"
        );
    }

    /// RULE: a request written in one of these versions is read back member for member,
    /// and a response written in it carries exactly the members that version declares.
    #[test]
    fn a_pre_flexible_produce_frame_round_trips() {
        // The rows span the three versions `Produce` is framed in here, which differ in
        // what the ANSWER carries: a throttle time from version 1, the broker's own
        // append instant from version 2.
        for (case, version, answer_bytes) in [
            ("Produce v0", 0_i16, 4 + 2 + 6 + 4 + 4 + 2 + 8),
            ("Produce v1", 1_i16, 4 + 2 + 6 + 4 + 4 + 2 + 8 + 4),
            ("Produce v2", 2_i16, 4 + 2 + 6 + 4 + 4 + 2 + 8 + 8 + 4),
        ] {
            let mut frame = BytesMut::new();
            frame.put_i16(1);
            frame.put_i32(1_500);
            put_count(&mut frame, 1);
            put_string(&mut frame, "orders");
            put_count(&mut frame, 1);
            frame.put_i32(0);
            put_nullable_bytes(&mut frame, Some(&Bytes::from_static(b"a batch")));
            let mut body = frame.freeze();
            let request = produce_request(&mut body, version)
                .unwrap_or_else(|| panic!("case {case:?}: the request must be readable"));
            assert_eq!(
                (
                    request.acks,
                    request.timeout_ms,
                    request.topic_data.len(),
                    request.topic_data[0].name.0.as_str().to_string(),
                    request.topic_data[0].partition_data[0].index,
                    request.topic_data[0].partition_data[0]
                        .records
                        .clone()
                        .unwrap_or_default(),
                ),
                (
                    1,
                    1_500,
                    1,
                    "orders".to_string(),
                    0,
                    Bytes::from_static(b"a batch")
                ),
                "case {case:?}: every member of the request must be read back"
            );
            assert!(
                body.is_empty(),
                "case {case:?}: the whole frame must be consumed, {} bytes left",
                body.len()
            );

            let answered = ProduceResponse::default()
                .with_throttle_time_ms(0)
                .with_responses(vec![
                    ::kafka_protocol::messages::produce_response::TopicProduceResponse::default()
                        .with_name(TopicName(StrBytes::from_static_str("orders")))
                        .with_partition_responses(vec![
                            ::kafka_protocol::messages::produce_response::PartitionProduceResponse::default()
                                .with_index(0)
                                .with_error_code(0)
                                .with_base_offset(7)
                                .with_log_append_time_ms(-1),
                        ]),
                ]);
            assert_eq!(
                produce_response(&answered, version).len(),
                answer_bytes,
                "case {case:?}: expected a {answer_bytes}-byte answer"
            );
        }
    }

    /// RULE, for the read half: a fetch written in one of these versions is read back
    /// member for member, and its answer carries what that version declares.
    #[test]
    fn a_pre_flexible_fetch_frame_round_trips() {
        // The rows span the four versions `Fetch` is framed in here: version 0 (no
        // throttle time in the answer), 1 and 2 (a throttle time), and 3, whose request
        // adds the answer-wide `MaxBytes`.
        for (case, version, states_max_bytes) in [
            ("Fetch v0", 0_i16, false),
            ("Fetch v1", 1_i16, false),
            ("Fetch v2", 2_i16, false),
            ("Fetch v3", 3_i16, true),
        ] {
            let mut frame = BytesMut::new();
            frame.put_i32(-1);
            frame.put_i32(500);
            frame.put_i32(1);
            if states_max_bytes {
                frame.put_i32(2_048);
            }
            put_count(&mut frame, 1);
            put_string(&mut frame, "orders");
            put_count(&mut frame, 1);
            frame.put_i32(0);
            frame.put_i64(4);
            frame.put_i32(1_024);
            let mut body = frame.freeze();
            let request = fetch_request(&mut body, version)
                .unwrap_or_else(|| panic!("case {case:?}: the request must be readable"));
            assert_eq!(
                (
                    request.replica_id.0,
                    request.max_wait_ms,
                    request.min_bytes,
                    request.max_bytes,
                    request.topics[0].topic.0.as_str().to_string(),
                    request.topics[0].partitions[0].partition,
                    request.topics[0].partitions[0].fetch_offset,
                    request.topics[0].partitions[0].partition_max_bytes,
                ),
                (
                    -1,
                    500,
                    1,
                    match states_max_bytes {
                        true => 2_048,
                        false => i32::MAX,
                    },
                    "orders".to_string(),
                    0,
                    4,
                    1_024,
                ),
                "case {case:?}: every member of the request must be read back"
            );
            assert!(
                body.is_empty(),
                "case {case:?}: the whole frame must be consumed, {} bytes left",
                body.len()
            );

            let answered = FetchResponse::default().with_throttle_time_ms(0).with_responses(vec![
                ::kafka_protocol::messages::fetch_response::FetchableTopicResponse::default()
                    .with_topic(TopicName(StrBytes::from_static_str("orders")))
                    .with_partitions(vec![
                        ::kafka_protocol::messages::fetch_response::PartitionData::default()
                            .with_partition_index(0)
                            .with_error_code(0)
                            .with_high_watermark(5)
                            .with_records(Some(Bytes::from_static(b"a batch"))),
                    ]),
            ]);
            let expected = 4 + 2 + 6 + 4 + 4 + 2 + 8 + 4 + 7 + match version >= 1 {
                true => 4,
                false => 0,
            };
            assert_eq!(
                fetch_response(&answered, version).len(),
                expected,
                "case {case:?}: expected a {expected}-byte answer"
            );
        }
    }

    /// RULE: a frame that ends early, or states an array longer than this endpoint reads,
    /// is not read at all rather than read as a shorter one.
    #[test]
    fn a_frame_this_endpoint_will_not_read_is_refused() {
        // The rows span where the frame goes wrong: a truncated string, a truncated
        // trailing member, an array count past the ceiling, and an empty body.
        let rows: &[(&str, Vec<u8>)] = &[
            ("a truncated topic name", {
                let mut frame = BytesMut::new();
                put_count(&mut frame, 1);
                frame.put_i16(16);
                frame.put_slice(b"orders");
                frame.to_vec()
            }),
            ("no timeout after the topics", {
                let mut frame = BytesMut::new();
                put_count(&mut frame, 0);
                frame.to_vec()
            }),
            ("an array longer than this endpoint reads", {
                let mut frame = BytesMut::new();
                put_count(&mut frame, MAX_ENTRIES + 1);
                frame.to_vec()
            }),
            ("an empty body", Vec::new()),
        ];
        for (case, frame) in rows {
            let mut body = Bytes::from(frame.clone());
            assert!(
                create_topics_request(&mut body, 0).is_none(),
                "case {case:?}: a frame this endpoint cannot read must be refused"
            );
            let mut body = Bytes::from(frame.clone());
            assert!(
                fetch_request(&mut body, 0).is_none(),
                "case {case:?}: a frame this endpoint cannot read must be refused"
            );
            let mut body = Bytes::from(frame.clone());
            assert!(
                produce_request(&mut body, 0).is_none(),
                "case {case:?}: a frame this endpoint cannot read must be refused"
            );
        }
    }
}
