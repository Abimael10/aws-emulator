// SPDX-License-Identifier: CC0-1.0

//! The consumer-group and ordering contracts a NON-JAVA client observes, driven at the
//! protocol VERSIONS such a client negotiates.
//!
//! RULE under test: what the broker reports about a request is carried in the FIELD the
//! NEGOTIATED version of that API defines — so a client that negotiated an older version
//! of a group API learns the same thing a client on the newest one learns. Apache Kafka's
//! `OffsetFetchResponse` gained its top-level error code in version 2, and before that a
//! refusal of the whole request is reported on each partition the request named; a client
//! that reads neither field is answered a group with nothing committed, which is
//! indistinguishable from a group it may read.
//!
//! On that wire the three contracts a consuming client depends on hold at every
//! negotiated version:
//!
//! * the coordinator's COMMITTED OFFSETS outlive the member that wrote them, so a second
//!   member of the same group in a fresh session resumes at the committed position —
//!   nothing before it is delivered again, nothing after it is lost — and a committed read
//!   leaves the log where it was;
//! * Amazon MSK's IAM access control decides the `kafka-cluster` GROUP resource PER
//!   ACTION — `AlterGroup` governs `JoinGroup`/`SyncGroup`/`Heartbeat` and `DescribeGroup`
//!   governs `FindCoordinator`/`OffsetFetch` — answering `GROUP_AUTHORIZATION_FAILED` on
//!   refusal and appending and consuming nothing;
//! * records one producer appends to a SINGLE partition are read back from the earliest
//!   offset in exactly that order, at contiguous offsets, with no gap and no duplicate.
//!
//! The rows differ along what the rule generalises over: the NEGOTIATED version (the
//! oldest and the newest the broker's own `ApiVersions` advertises for each API), the
//! ACCESS PATH the client reached the cluster over (plaintext, SASL/IAM, and the public
//! SASL/SCRAM path a cluster publishes once public access is on), how many records and how
//! many batches one producer appends, and which `kafka-cluster` actions the signing
//! identity's policies cover for a group id.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at the
//! address `GetBootstrapBrokers` published.

use std::collections::BTreeMap;
use std::time::Duration;

use ::bytes::{Bytes, BytesMut};
use ::kafka_protocol::messages::{
    create_topics_request::CreatableTopic,
    join_group_request::JoinGroupRequestProtocol,
    leave_group_request::MemberIdentity,
    list_offsets_request::{ListOffsetsPartition, ListOffsetsTopic},
    offset_commit_request::{OffsetCommitRequestPartition, OffsetCommitRequestTopic},
    offset_fetch_request::OffsetFetchRequestTopic,
    produce_request::{PartitionProduceData, TopicProduceData},
    fetch_request::{FetchPartition, FetchTopic},
    sync_group_request::SyncGroupRequestAssignment,
    ApiKey, BrokerId, CreateTopicsRequest, CreateTopicsResponse, FetchRequest, FetchResponse,
    FindCoordinatorRequest, FindCoordinatorResponse, GroupId, HeartbeatRequest, HeartbeatResponse,
    JoinGroupRequest, JoinGroupResponse, LeaveGroupRequest, LeaveGroupResponse, ListOffsetsRequest,
    ListOffsetsResponse, OffsetCommitRequest, OffsetCommitResponse, OffsetFetchRequest,
    OffsetFetchResponse, ProduceRequest, ProduceResponse, SaslAuthenticateRequest,
    SaslAuthenticateResponse, SaslHandshakeRequest, SaslHandshakeResponse, SyncGroupRequest,
    SyncGroupResponse, TopicName,
};
use ::kafka_protocol::protocol::{Message, StrBytes};
use ::kafka_protocol::records::{
    Compression, Record, RecordBatchDecoder, RecordBatchEncoder, RecordEncodeOptions,
    TimestampType,
};
use ::kafka_protocol::ResponseError;
use serde_json::{json, Value};

use crate::broker_data_plane::{
    epoch_millis, given_secret_associated_under_key, Client, AWS_MSK_IAM, CLIENT_ID, LOOPBACK,
    SCRAM_SHA_512,
};
use crate::kafka_harness::{
    given_assumed_role_session, given_role_inline_policy, pct, ClusterRequest, Emulator, ACCOUNT,
    REGION,
};

/// The protocol type a consuming client joins a group with, and the assignor it names.
const CONSUMER_PROTOCOL: &str = "consumer";
const RANGE_ASSIGNOR: &str = "range";

/// The session and rebalance windows every join below states, in milliseconds: well above
/// the time this suite takes, so no row depends on how fast the host runs.
const SESSION_TIMEOUT_MS: i32 = 300_000;
const REBALANCE_TIMEOUT_MS: i32 = 300_000;

/// The topic every row reads and writes, and the one partition it holds: the per-partition
/// guarantee is about ONE partition, so the rows that assert it use a topic with one.
const TOPIC: &str = "orders";
const PARTITION: i32 = 0;

/// The `Timestamp` a `ListOffsets` names to ask where the log ENDS (Apache Kafka protocol,
/// `ListOffsets`: `-1` is the latest offset).
const OFFSET_LATEST: i64 = -1;

/// The committed offset a partition no member of the group has committed carries (Apache
/// Kafka's `OffsetFetch`: a partition with no committed offset answers `-1`).
const NO_COMMITTED_OFFSET: i64 = -1;

/// The version of `OffsetFetchResponse` that first carries a TOP-LEVEL error code (Apache
/// Kafka's `OffsetFetchResponse`: the `ErrorCode` field is `2+`). Below it a refusal of the
/// whole request is reported on each partition the request named.
const OFFSET_FETCH_TOP_LEVEL_ERROR_FROM: i16 = 2;

/// The `kafka-cluster` actions a policy below grants.
pub(crate) const CONNECT: &str = "kafka-cluster:Connect";
pub(crate) const WRITE_DATA: &str = "kafka-cluster:WriteData";
pub(crate) const READ_DATA: &str = "kafka-cluster:ReadData";
pub(crate) const DESCRIBE_TOPIC: &str = "kafka-cluster:DescribeTopic";
const ALTER_GROUP: &str = "kafka-cluster:AlterGroup";
const DESCRIBE_GROUP: &str = "kafka-cluster:DescribeGroup";

/// How long a wait for a condition the service's own clock owes may take, and how often to
/// look: nothing here sleeps for a span, and the deadline is generous so a loaded host
/// cannot decide the outcome.
const SETTLED_DEADLINE: Duration = Duration::from_secs(60);
const LOOK: Duration = Duration::from_millis(50);

/// WHICH end of each API's advertised version range a row negotiates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    /// The oldest version the broker advertises — what a client that infers an older
    /// broker, or one whose library stops at an older version, sends.
    Oldest,
    /// The newest it advertises, which a current Java client negotiates.
    Newest,
}

/// The version table the BROKER itself advertised, and the end of each range this row
/// negotiates — so a row spans the versions the endpoint really offers rather than a list
/// written out here.
struct Negotiated {
    advertised: BTreeMap<i16, (i16, i16)>,
    pick: Pick,
}

impl Negotiated {
    /// The table as the broker stated it in `ApiVersions`.
    async fn of(client: &mut Client, case: &str, pick: Pick) -> Self {
        let advertised = client.api_versions(case).await;
        assert_eq!(
            advertised.error_code, 0,
            "case {case:?}: ApiVersions must be answered, got error {}",
            advertised.error_code
        );
        Self {
            advertised: advertised
                .api_keys
                .iter()
                .map(|api| (api.api_key, (api.min_version, api.max_version)))
                .collect(),
            pick,
        }
    }

    /// The version of `key` this row sends: the end of the advertised range it picked,
    /// narrowed to the versions this test's own protocol library can frame.
    fn version<Request: Message, Response: Message>(&self, case: &str, key: ApiKey) -> i16 {
        let (advertised_min, advertised_max) = *self
            .advertised
            .get(&(key as i16))
            .unwrap_or_else(|| panic!("case {case:?}: the broker must advertise {key:?}"));
        let min = advertised_min
            .max(Request::VERSIONS.min)
            .max(Response::VERSIONS.min);
        let max = advertised_max
            .min(Request::VERSIONS.max)
            .min(Response::VERSIONS.max);
        assert!(
            min <= max,
            "case {case:?}: {key:?} is advertised over {advertised_min}..={advertised_max}, which \
             this suite cannot frame"
        );
        match self.pick {
            Pick::Oldest => min,
            Pick::Newest => max,
        }
    }
}

/// RULE: a refusal the broker must report is carried in the field the NEGOTIATED version
/// defines, and MSK's IAM access control decides the group resource per ACTION — so the
/// `GROUP_AUTHORIZATION_FAILED` a group id no policy covers draws is observable to a client
/// on any version, and the refusals append and consume nothing.
#[tokio::test]
async fn a_group_refusal_is_reported_in_the_field_the_negotiated_version_carries() {
    // The group ids differ along which actions the signing identity's policies cover for
    // them: both group actions, the describe action only, and neither.
    const FULL: &str = "scope-full-readers";
    const DESCRIBE_ONLY: &str = "scope-describe-readers";
    const NEITHER: &str = "scope-none-readers";
    const FRESH: &str = "scope-full-audit";

    for pick in [Pick::Oldest, Pick::Newest] {
        let case = format!("the group scope at the {pick:?} advertised versions");
        let case = case.as_str();
        let emulator = Emulator::with_iam();
        let cluster = emulator
            .create_ok(&ClusterRequest {
                client_broker: Some("TLS"),
                sasl_iam: Some(true),
                ..ClusterRequest::named(&format!("group-scope-{}", suffix(pick)))
            })
            .await;
        given_active_topic(&emulator, &cluster, TOPIC, 1, case).await;
        let access_key_id = format!("ASIAGROUPSCOPE{}", suffix(pick).to_uppercase());
        given_assumed_role_session(&emulator.engine, "group-scope-role", "session-1", &access_key_id);
        given_role_inline_policy(
            &emulator.engine,
            "group-scope-role",
            Some(&json!({
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Action": [CONNECT, WRITE_DATA, READ_DATA, DESCRIBE_TOPIC],
                        "Resource": [
                            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/*"),
                            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:topic/*"),
                        ],
                    },
                    {
                        "Effect": "Allow",
                        "Action": [ALTER_GROUP, DESCRIBE_GROUP],
                        "Resource": [format!("arn:aws:kafka:{REGION}:{ACCOUNT}:group/*/scope-full-*")],
                    },
                    {
                        "Effect": "Allow",
                        "Action": [DESCRIBE_GROUP],
                        "Resource": [
                            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:group/*/scope-describe-*"),
                        ],
                    },
                ],
            })),
        );

        let authority = emulator
            .published_bootstrap(&cluster, "bootstrapBrokerStringSaslIam", case)
            .await;
        let mut client = iam_client(case, &authority, &access_key_id).await;
        let negotiated = Negotiated::of(&mut client, case, pick).await;

        // One record, so a refusal that consumed something would show as a log that moved.
        let produced = produce(
            &mut client,
            &negotiated,
            case,
            &[(None, "the-one-record".to_string())],
        )
        .await;
        assert_eq!(
            produced, 0,
            "case {case:?}: the produce must be accepted, got base offset {produced}"
        );

        let refused = ResponseError::GroupAuthorizationFailed.code();
        let unknown_member = ResponseError::UnknownMemberId.code();
        // (the group id, and what FindCoordinator, JoinGroup, SyncGroup, Heartbeat and
        // OffsetFetch answer for it). A group the policies cover reaches the group's OWN
        // fencing instead of the scope: a heartbeat quoting a member it does not hold is
        // told so, which is what makes the two decisions distinguishable.
        let rows: &[(&str, i16, i16, i16, i16, i16)] = &[
            (FULL, 0, 0, 0, unknown_member, 0),
            (DESCRIBE_ONLY, 0, refused, refused, refused, 0),
            (NEITHER, refused, refused, refused, refused, refused),
        ];
        for (group, coordinator, join, sync, heartbeat, offsets) in rows {
            let probe = format!("{case} / {group}");
            let answered = find_coordinator(&mut client, &negotiated, &probe, group).await;
            assert_eq!(
                answered, *coordinator,
                "case {probe:?}: expected FindCoordinator to answer {coordinator}, got {answered}"
            );
            let joined = join_group(&mut client, &negotiated, &probe, group, "").await;
            assert_eq!(
                joined.error_code, *join,
                "case {probe:?}: expected JoinGroup to answer {join}, got {}",
                joined.error_code
            );
            // The member a group call speaks for: the one the join established where it was
            // admitted, and a member the group cannot hold where it was refused.
            let (generation, member_id) = match joined.error_code {
                0 => (joined.generation_id, joined.member_id.to_string()),
                _ => (99, "nobody".to_string()),
            };
            let synced =
                sync_group(&mut client, &negotiated, &probe, group, generation, &member_id).await;
            assert_eq!(
                synced.error_code, *sync,
                "case {probe:?}: expected SyncGroup to answer {sync}, got {}",
                synced.error_code
            );
            let beat =
                heartbeat_group(&mut client, &negotiated, &probe, group, 99, "nobody").await;
            assert_eq!(
                beat.error_code, *heartbeat,
                "case {probe:?}: expected Heartbeat to answer {heartbeat}, got {}",
                beat.error_code
            );
            let version =
                negotiated.version::<OffsetFetchRequest, OffsetFetchResponse>(&probe, ApiKey::OffsetFetch);
            let fetched = fetch_committed(&mut client, &negotiated, &probe, group).await;
            let partition = fetched
                .topics
                .first()
                .and_then(|topic| topic.partitions.first())
                .unwrap_or_else(|| panic!("case {probe:?}: OffsetFetch reported no partition"));
            assert_eq!(
                partition.error_code, *offsets,
                "case {probe:?}: expected OffsetFetch v{version} to report {offsets} on the \
                 partition it was asked about, got {}",
                partition.error_code
            );
            // And from the version that HAS a top-level error code, there too — while a
            // version that has none reports it on the partition alone, which is the field
            // it does carry.
            let expected_top_level = match version >= OFFSET_FETCH_TOP_LEVEL_ERROR_FROM {
                true => *offsets,
                false => 0,
            };
            assert_eq!(
                fetched.error_code, expected_top_level,
                "case {probe:?}: expected OffsetFetch v{version} to report {expected_top_level} \
                 at the top level, got {}",
                fetched.error_code
            );
        }

        // The refusals consumed nothing: a group the policies DO cover still reads the one
        // record, and the log still ends where the produce left it.
        let joined = join_group(&mut client, &negotiated, case, FRESH, "").await;
        assert_eq!(
            joined.error_code, 0,
            "case {case:?}: a covered group must still join, got {}",
            joined.error_code
        );
        let read = consume(&mut client, &negotiated, case, 0).await;
        let values: Vec<String> = read.iter().map(|(_, value)| value.clone()).collect();
        assert_eq!(
            values,
            vec!["the-one-record".to_string()],
            "case {case:?}: the log must still hold exactly the produced record, got {values:?}"
        );
        let end = log_end(&mut client, &negotiated, case).await;
        assert_eq!(
            end, 1,
            "case {case:?}: the refusals must have consumed and appended nothing, so the log \
             must still end at 1, got {end}"
        );
    }
}

/// RULE: the coordinator's committed offsets outlive the member that wrote them, so a
/// second member of the same group in a fresh session resumes at the committed position —
/// the two members' records are disjoint, together they are the whole log, and a committed
/// read leaves the log's end where it was.
#[tokio::test]
async fn committed_group_offsets_outlive_the_member_that_wrote_them() {
    // The rows differ along the negotiated version and along WHERE in the log the handover
    // happens: after the first record, and part-way through.
    let rows: &[(Pick, i64, i64)] = &[
        (Pick::Oldest, 8, 1),
        (Pick::Oldest, 8, 5),
        (Pick::Newest, 8, 5),
    ];
    for (index, (pick, records, handover)) in rows.iter().enumerate() {
        let case = format!(
            "a handover at offset {handover} of {records} records at the {pick:?} advertised \
             versions"
        );
        let case = case.as_str();
        let group = "readers";
        let emulator = Emulator::new();
        let cluster = emulator
            .create_ok(&ClusterRequest {
                client_broker: Some("PLAINTEXT"),
                unauthenticated: Some(true),
                ..ClusterRequest::named(&format!("group-handover-{index}"))
            })
            .await;
        given_active_topic(&emulator, &cluster, TOPIC, 1, case).await;
        let authority = emulator
            .published_bootstrap(&cluster, "bootstrapBrokerString", case)
            .await;

        let mut producer = Client::plaintext(case, &authority).await;
        let negotiated = Negotiated::of(&mut producer, case, *pick).await;
        let written: Vec<(Option<String>, String)> = (0..*records)
            .map(|ordinal| (None, format!("record-{ordinal}")))
            .collect();
        produce(&mut producer, &negotiated, case, &written).await;

        // The first member joins, reads from the position the coordinator holds for the
        // group — none yet — and commits through the handover point.
        let mut first = Client::plaintext(case, &authority)
            .await
            .identified_as("member-a");
        let negotiated_first = Negotiated::of(&mut first, case, *pick).await;
        let joined = join_group(&mut first, &negotiated_first, case, group, "").await;
        assert_eq!(
            joined.error_code, 0,
            "case {case:?}: the first member must join, got {}",
            joined.error_code
        );
        let member_a = joined.member_id.to_string();
        let synced = sync_group(
            &mut first,
            &negotiated_first,
            case,
            group,
            joined.generation_id,
            &member_a,
        )
        .await;
        assert_eq!(
            synced.error_code, 0,
            "case {case:?}: the first member's SyncGroup must be answered, got {}",
            synced.error_code
        );
        let resumed = committed_offset(&mut first, &negotiated_first, case, group).await;
        assert_eq!(
            resumed, NO_COMMITTED_OFFSET,
            "case {case:?}: a group nobody has committed in must report no committed offset, \
             got {resumed}"
        );
        let read_by_a: Vec<(i64, String)> = consume(&mut first, &negotiated_first, case, 0)
            .await
            .into_iter()
            .take(usize::try_from(*handover).unwrap_or(0))
            .collect();
        let committed = commit_offset(
            &mut first,
            &negotiated_first,
            case,
            group,
            joined.generation_id,
            &member_a,
            *handover,
        )
        .await;
        assert_eq!(
            committed, 0,
            "case {case:?}: the commit must be accepted, got error {committed}"
        );
        leave_group(&mut first, &negotiated_first, case, group, &member_a).await;
        drop(first);

        // The coordinator holds that position for the GROUP, not for the member that wrote
        // it: a client that has joined no group reads it back once the member has left.
        let mut reader = Client::plaintext(case, &authority)
            .await
            .identified_as("offset-reader");
        let negotiated_reader = Negotiated::of(&mut reader, case, *pick).await;
        let held = committed_offset(&mut reader, &negotiated_reader, case, group).await;
        assert_eq!(
            held, *handover,
            "case {case:?}: the coordinator must still report the committed next-offset \
             {handover} once the member that wrote it has left, got {held}"
        );
        drop(reader);

        // A second member of the SAME group, in a fresh session, is told the position the
        // first one committed and resumes there.
        let mut second = Client::plaintext(case, &authority)
            .await
            .identified_as("member-b");
        let negotiated_second = Negotiated::of(&mut second, case, *pick).await;
        let rejoined = join_group(&mut second, &negotiated_second, case, group, "").await;
        assert_eq!(
            rejoined.error_code, 0,
            "case {case:?}: the second member must join, got {}",
            rejoined.error_code
        );
        let member_b = rejoined.member_id.to_string();
        let resynced = sync_group(
            &mut second,
            &negotiated_second,
            case,
            group,
            rejoined.generation_id,
            &member_b,
        )
        .await;
        assert_eq!(
            resynced.error_code, 0,
            "case {case:?}: the second member's SyncGroup must be answered, got {}",
            resynced.error_code
        );
        let position = committed_offset(&mut second, &negotiated_second, case, group).await;
        assert_eq!(
            position, *handover,
            "case {case:?}: expected the coordinator to report the committed next-offset \
             {handover}, got {position}"
        );
        let read_by_b = consume(&mut second, &negotiated_second, case, position).await;

        let offsets_a: Vec<i64> = read_by_a.iter().map(|(offset, _)| *offset).collect();
        let offsets_b: Vec<i64> = read_by_b.iter().map(|(offset, _)| *offset).collect();
        assert!(
            offsets_a.iter().all(|offset| !offsets_b.contains(offset)),
            "case {case:?}: no record may be delivered to both members, got {offsets_a:?} and \
             {offsets_b:?}"
        );
        let mut union: Vec<i64> = offsets_a.clone();
        union.extend(offsets_b.iter().copied());
        let whole: Vec<i64> = (0..*records).collect();
        assert_eq!(
            union, whole,
            "case {case:?}: the two members together must read the whole log exactly once, got \
             {union:?}"
        );
        let values: Vec<String> = read_by_a
            .iter()
            .chain(read_by_b.iter())
            .map(|(_, value)| value.clone())
            .collect();
        let expected: Vec<String> = written.iter().map(|(_, value)| value.clone()).collect();
        assert_eq!(
            values, expected,
            "case {case:?}: the two members together must read the produced records in order, \
             got {values:?}"
        );
        // A committed read does not change the log: it still ends where the produce left
        // it.
        let end = log_end(&mut second, &negotiated_second, case).await;
        assert_eq!(
            end, *records,
            "case {case:?}: a committed read must leave the log's end at {records}, got {end}"
        );
    }
}

/// RULE: records one producer appends to a single partition are read back from the earliest
/// offset in exactly that order, at contiguous offsets, with no gap and no duplicate —
/// whichever access path the client reached the cluster over and whichever version it
/// negotiated.
#[tokio::test]
async fn per_partition_order_is_preserved_at_contiguous_offsets() {
    // The rows differ along the negotiated version and along how one producer's records are
    // batched: one batch, and several batches sent in sequence. The counts are the rows'
    // own; the requirement is the ORDER.
    let rows: &[(Pick, &[usize])] = &[
        (Pick::Oldest, &[5]),
        (Pick::Oldest, &[3, 1, 4]),
        (Pick::Newest, &[3, 1, 4]),
    ];
    for (index, (pick, batches)) in rows.iter().enumerate() {
        let case = format!(
            "{} batch(es) at the {pick:?} advertised versions",
            batches.len()
        );
        let case = case.as_str();
        let emulator = Emulator::new();
        let cluster = emulator
            .create_ok(&ClusterRequest {
                client_broker: Some("PLAINTEXT"),
                unauthenticated: Some(true),
                ..ClusterRequest::named(&format!("partition-order-{index}"))
            })
            .await;
        given_active_topic(&emulator, &cluster, TOPIC, 1, case).await;
        let authority = emulator
            .published_bootstrap(&cluster, "bootstrapBrokerString", case)
            .await;
        let mut client = Client::plaintext(case, &authority).await;
        let negotiated = Negotiated::of(&mut client, case, *pick).await;

        let mut produced: Vec<String> = Vec::new();
        let mut next = 0usize;
        for (batch, count) in batches.iter().enumerate() {
            let records: Vec<(Option<String>, String)> = (0..*count)
                .map(|within| {
                    let value = format!("msg-{:03}", next + within);
                    (Some(format!("key-{batch}")), value)
                })
                .collect();
            let base = produce(&mut client, &negotiated, case, &records).await;
            assert_eq!(
                base,
                i64::try_from(next).unwrap_or_default(),
                "case {case:?}: batch {batch} must be appended after the records before it, got \
                 base offset {base}"
            );
            produced.extend(records.into_iter().map(|(_, value)| value));
            next += count;
        }

        let read = consume(&mut client, &negotiated, case, 0).await;
        let values: Vec<String> = read.iter().map(|(_, value)| value.clone()).collect();
        assert_eq!(
            values, produced,
            "case {case:?}: the consumed sequence must equal the produced sequence, got {values:?}"
        );
        let offsets: Vec<i64> = read.iter().map(|(offset, _)| *offset).collect();
        let contiguous: Vec<i64> = (0..i64::try_from(produced.len()).unwrap_or_default()).collect();
        assert_eq!(
            offsets, contiguous,
            "case {case:?}: the offsets must be contiguous and increasing from the earliest, got \
             {offsets:?}"
        );
    }
}

/// RULE: the same per-partition order holds over the PUBLIC SASL/SCRAM path a cluster
/// publishes once public access is on, and every transition that path waits on — the
/// cluster out of `CREATING`, the connectivity update, the topic out of `CREATING` — is
/// observable to a client that polls, within a poll or two of the call that started it.
#[tokio::test]
async fn the_public_sasl_scram_path_serves_the_same_ordered_log() {
    const USER: &str = "scram-reader";
    const PASSWORD: &str = "scram-Secret-99";

    let case = "the public SASL/SCRAM path";
    let emulator = Emulator::with_secrets_manager_and_kms();
    let request = ClusterRequest {
        client_broker: Some("TLS"),
        sasl_scram: Some(true),
        brokers: 2,
        subnets: vec!["subnet-a", "subnet-b"],
        ..ClusterRequest::named("public-scram-order")
    };
    // The version the cluster runs is one the service's own catalog publishes as offered,
    // which is how a client picks one.
    let listed = emulator
        .call("GET", "/v1/kafka-versions?maxResults=100", None)
        .await;
    assert!(
        listed
            .at("/kafkaVersions")
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| {
                row.get("version").and_then(Value::as_str) == Some(request.kafka_version)
                    && row.get("status").and_then(Value::as_str) == Some("ACTIVE")
            })),
        "case {case:?}: the version the cluster is created on must be published ACTIVE, got {}",
        listed.body
    );
    let cluster = emulator.create_ok(&request).await;

    // Each transition is read by LOOKING AGAIN rather than by waiting out a span: what the
    // contract asks is that a polling client reaches it, and the first look is taken as
    // soon as the call that starts it is answered.
    let version = poll_cluster_active(&emulator, &cluster, case).await;
    // The key a SCRAM secret's value is sealed under is a CUSTOMER MANAGED one, minted
    // through KMS's own `CreateKey`.
    let key = emulator
        .kms(
            REGION,
            "CreateKey",
            json!({ "Description": "the key the SCRAM secret is sealed under" }),
        )
        .await;
    assert_eq!(
        key.status, 200,
        "case {case:?}: KMS must create the key, got {} {}",
        key.status, key.body
    );
    let key_arn = key
        .at("/KeyMetadata/Arn")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateKey reported no key ARN"))
        .to_string();
    given_secret_associated_under_key(
        &emulator,
        &cluster,
        "public-scram-secret",
        USER,
        PASSWORD,
        &key_arn,
        case,
    )
    .await;
    let updated = emulator
        .call(
            "PUT",
            &format!("/v1/clusters/{}/connectivity", pct(&cluster)),
            Some(json!({
                "currentVersion": version,
                "connectivityInfo": {"publicAccess": {"type": "SERVICE_PROVIDED_EIPS"}},
            })),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "case {case:?}: UpdateConnectivity must be accepted, got {} {}",
        updated.status, updated.body
    );
    poll_cluster_active(&emulator, &cluster, case).await;
    given_active_topic(&emulator, &cluster, TOPIC, 1, case).await;

    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerStringPublicSaslScram", case)
        .await;
    let mut client = Client::tls(case, &authority).await;
    let negotiated = Negotiated::of(&mut client, case, Pick::Oldest).await;
    let authenticated = client
        .authenticate_as(case, SCRAM_SHA_512, USER, PASSWORD)
        .await;
    assert_eq!(
        authenticated, 0,
        "case {case:?}: the SASL/SCRAM exchange must authenticate the connection, got error \
         {authenticated}"
    );

    let produced: Vec<(Option<String>, String)> = (0..6)
        .map(|ordinal| (None, format!("msg-{ordinal:03}")))
        .collect();
    produce(&mut client, &negotiated, case, &produced).await;
    let read = consume(&mut client, &negotiated, case, 0).await;
    let values: Vec<String> = read.iter().map(|(_, value)| value.clone()).collect();
    let expected: Vec<String> = produced.iter().map(|(_, value)| value.clone()).collect();
    assert_eq!(
        values, expected,
        "case {case:?}: the consumed sequence must equal the produced sequence, got {values:?}"
    );
    let offsets: Vec<i64> = read.iter().map(|(offset, _)| *offset).collect();
    let contiguous: Vec<i64> = (0..i64::try_from(expected.len()).unwrap_or_default()).collect();
    assert_eq!(
        offsets, contiguous,
        "case {case:?}: the offsets must be contiguous from the earliest, got {offsets:?}"
    );
}

/// RULE: the same contracts hold on an Amazon MSK SERVERLESS cluster, whose clients
/// reach one access path (SASL/IAM over TLS) and create every topic over that wire
/// because the control plane offers them no topic management — so the topic a group
/// consumes, the position it commits and the order it reads are all established on the
/// data plane alone.
#[tokio::test]
async fn a_serverless_clusters_group_contracts_hold_over_its_one_access_path() {
    let case = "a serverless cluster's one access path";
    let group = "readers";
    let emulator = Emulator::with_iam();
    let cluster = emulator
        .create_serverless_ok("serverless-group-contracts", &[vec!["subnet-0a1b2c3d"]], Some(true))
        .await;
    let access_key_id = "ASIASERVERLESSGROUP";
    given_assumed_role_session(&emulator.engine, "serverless-group-role", "session-1", access_key_id);
    // The role's policies cover the cluster, its topics and its groups: what a client of
    // an IAM-authenticated cluster is granted to run this sequence at all.
    given_role_inline_policy(
        &emulator.engine,
        "serverless-group-role",
        Some(&json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": [
                    CONNECT,
                    WRITE_DATA,
                    READ_DATA,
                    DESCRIBE_TOPIC,
                    "kafka-cluster:CreateTopic",
                    ALTER_GROUP,
                    DESCRIBE_GROUP,
                ],
                "Resource": [
                    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/*"),
                    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:topic/*"),
                    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:group/*"),
                ],
            }],
        })),
    );
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerStringSaslIam", case)
        .await;
    let mut client = iam_client(case, &authority, access_key_id).await;
    let negotiated = Negotiated::of(&mut client, case, Pick::Oldest).await;

    // The topic is created over the WIRE, which is the only place a serverless cluster's
    // topics come from, asking the broker for its own replication default.
    let created: CreateTopicsResponse = client
        .call(
            case,
            ApiKey::CreateTopics,
            negotiated.version::<CreateTopicsRequest, CreateTopicsResponse>(case, ApiKey::CreateTopics),
            &CreateTopicsRequest::default()
                .with_timeout_ms(1_000)
                .with_topics(vec![CreatableTopic::default()
                    .with_name(TopicName(StrBytes::from_static_str(TOPIC)))
                    .with_num_partitions(1)
                    .with_replication_factor(-1)]),
        )
        .await;
    let result = created
        .topics
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: CreateTopics reported no topic"))
        .clone();
    assert_eq!(
        result.error_code, 0,
        "case {case:?}: CreateTopics over the wire must be accepted, got error {} {:?}",
        result.error_code,
        result.error_message.as_ref().map(|message| message.to_string())
    );

    // And the broker reports the topic it just created, with a leader for its partition:
    // what a client confirms before it produces.
    let metadata = client.metadata(case).await;
    let partitions: Vec<(i32, i32)> = metadata
        .topics
        .iter()
        .filter(|topic| topic.name.as_ref().is_some_and(|name| name.0.as_str() == TOPIC))
        .flat_map(|topic| {
            topic
                .partitions
                .iter()
                .map(|partition| (partition.partition_index, partition.leader_id.0))
        })
        .collect();
    assert_eq!(
        partitions.len(),
        1,
        "case {case:?}: the broker must report the topic's one partition, got {partitions:?}"
    );
    assert!(
        partitions.iter().all(|(_, leader)| *leader >= 0),
        "case {case:?}: every partition must report a leader, got {partitions:?}"
    );

    let written: Vec<(Option<String>, String)> = (0..4)
        .map(|ordinal| (None, format!("record-{ordinal}")))
        .collect();
    produce(&mut client, &negotiated, case, &written).await;

    // One member reads half the log and commits through it; a second member of the same
    // group in a fresh session resumes exactly there.
    let joined = join_group(&mut client, &negotiated, case, group, "").await;
    assert_eq!(
        joined.error_code, 0,
        "case {case:?}: the first member must join, got {}",
        joined.error_code
    );
    let member_a = joined.member_id.to_string();
    sync_group(&mut client, &negotiated, case, group, joined.generation_id, &member_a).await;
    let read_by_a: Vec<(i64, String)> = consume(&mut client, &negotiated, case, 0)
        .await
        .into_iter()
        .take(2)
        .collect();
    let committed = commit_offset(
        &mut client,
        &negotiated,
        case,
        group,
        joined.generation_id,
        &member_a,
        2,
    )
    .await;
    assert_eq!(
        committed, 0,
        "case {case:?}: the commit must be accepted, got error {committed}"
    );
    leave_group(&mut client, &negotiated, case, group, &member_a).await;

    let mut second = iam_client(case, &authority, access_key_id)
        .await
        .identified_as("member-b");
    let negotiated_second = Negotiated::of(&mut second, case, Pick::Oldest).await;
    let rejoined = join_group(&mut second, &negotiated_second, case, group, "").await;
    assert_eq!(
        rejoined.error_code, 0,
        "case {case:?}: the second member must join, got {}",
        rejoined.error_code
    );
    let member_b = rejoined.member_id.to_string();
    sync_group(
        &mut second,
        &negotiated_second,
        case,
        group,
        rejoined.generation_id,
        &member_b,
    )
    .await;
    let position = committed_offset(&mut second, &negotiated_second, case, group).await;
    assert_eq!(
        position, 2,
        "case {case:?}: the coordinator must report the committed next-offset 2, got {position}"
    );
    let read_by_b = consume(&mut second, &negotiated_second, case, position).await;
    let values: Vec<String> = read_by_a
        .iter()
        .chain(read_by_b.iter())
        .map(|(_, value)| value.clone())
        .collect();
    let expected: Vec<String> = written.iter().map(|(_, value)| value.clone()).collect();
    assert_eq!(
        values, expected,
        "case {case:?}: the two members together must read the produced records once, in order, \
         got {values:?}"
    );
    let end = log_end(&mut second, &negotiated_second, case).await;
    assert_eq!(
        end, 4,
        "case {case:?}: a committed read must leave the log's end at 4, got {end}"
    );
}

/// The suffix a row's resource names carry, so two rows never share a cluster name.
fn suffix(pick: Pick) -> &'static str {
    match pick {
        Pick::Oldest => "oldest",
        Pick::Newest => "newest",
    }
}

/// A client on the SASL/IAM path, authenticated with the credential `access_key_id` names:
/// the exchange Amazon MSK's IAM access control defines, whose payload says which key
/// signed it — so the connection speaks for the principal that key belongs to.
pub(crate) async fn iam_client(case: &str, authority: &str, access_key_id: &str) -> Client {
    let mut client = Client::tls(case, authority).await;
    client.api_versions(case).await;
    let handshake: SaslHandshakeResponse = client
        .call(
            case,
            ApiKey::SaslHandshake,
            1,
            &SaslHandshakeRequest::default().with_mechanism(StrBytes::from_static_str(AWS_MSK_IAM)),
        )
        .await;
    assert_eq!(
        handshake.error_code, 0,
        "case {case:?}: the cluster must offer the IAM mechanism, got error {}",
        handshake.error_code
    );
    let payload = json!({
        "version": "2020_10_22",
        "host": LOOPBACK,
        "user-agent": CLIENT_ID,
        "action": CONNECT,
        "x-amz-algorithm": "AWS4-HMAC-SHA256",
        "x-amz-credential":
            format!("{access_key_id}/20261101/{REGION}/kafka-cluster/aws4_request"),
        "x-amz-date": "20261101T000000Z",
        "x-amz-signedheaders": "host",
        "x-amz-signature": "a-signature",
    });
    let authenticated: SaslAuthenticateResponse = client
        .call(
            case,
            ApiKey::SaslAuthenticate,
            2,
            &SaslAuthenticateRequest::default()
                .with_auth_bytes(Bytes::from(payload.to_string().into_bytes())),
        )
        .await;
    assert_eq!(
        authenticated.error_code, 0,
        "case {case:?}: the IAM exchange must authenticate the connection, got error {}",
        authenticated.error_code
    );
    client
}

/// Create a topic through the control plane and LOOK AGAIN until it reports `ACTIVE`,
/// which is the transition a client of this API waits on.
pub(crate) async fn given_active_topic(
    emulator: &Emulator,
    cluster: &str,
    name: &str,
    partitions: i64,
    case: &str,
) {
    let created = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/topics", pct(cluster)),
            Some(json!({
                "topicName": name,
                "partitionCount": partitions,
                "replicationFactor": 1,
            })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateTopic({name}) must be accepted, got {} {}",
        created.status, created.body
    );
    let deadline = std::time::Instant::now() + SETTLED_DEADLINE;
    loop {
        let reply = emulator
            .call(
                "GET",
                &format!("/v1/clusters/{}/topics/{}", pct(cluster), pct(name)),
                None,
            )
            .await;
        if reply.at("/status").as_str() == Some("ACTIVE") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the topic must reach ACTIVE, last answer {} {}",
            reply.status,
            reply.body
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// LOOK AGAIN until the cluster reports `ACTIVE`, answering the version it then carries.
async fn poll_cluster_active(emulator: &Emulator, cluster: &str, case: &str) -> String {
    let deadline = std::time::Instant::now() + SETTLED_DEADLINE;
    loop {
        let reply = emulator.describe_v2(cluster).await;
        if reply.at("/clusterInfo/state").as_str() == Some("ACTIVE") {
            return reply
                .at("/clusterInfo/currentVersion")
                .as_str()
                .unwrap_or_default()
                .to_string();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the cluster must reach ACTIVE, last answer {} {}",
            reply.status,
            reply.body
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// `Produce` one batch of `records` to the suite's partition, answering the base offset the
/// broker assigned.
async fn produce(
    client: &mut Client,
    negotiated: &Negotiated,
    case: &str,
    records: &[(Option<String>, String)],
) -> i64 {
    let version = negotiated.version::<ProduceRequest, ProduceResponse>(case, ApiKey::Produce);
    let timestamp = epoch_millis();
    let batch: Vec<Record> = records
        .iter()
        .enumerate()
        .map(|(index, (key, value))| Record {
            transactional: false,
            control: false,
            partition_leader_epoch: 0,
            producer_id: -1,
            producer_epoch: -1,
            timestamp_type: TimestampType::Creation,
            offset: index as i64,
            sequence: index as i32,
            timestamp,
            key: key.as_ref().map(|key| Bytes::from(key.as_bytes().to_vec())),
            value: Some(Bytes::from(value.as_bytes().to_vec())),
            headers: Default::default(),
            delete_horizon: false,
        })
        .collect();
    let mut encoded = BytesMut::new();
    RecordBatchEncoder::encode(
        &mut encoded,
        batch.iter(),
        &RecordEncodeOptions {
            version: 2,
            compression: Compression::None,
        },
    )
    .unwrap_or_else(|error| panic!("case {case:?}: the batch must encode: {error}"));
    let answered: ProduceResponse = client
        .call(
            case,
            ApiKey::Produce,
            version,
            &ProduceRequest::default()
                .with_acks(1)
                .with_timeout_ms(1_000)
                .with_topic_data(vec![TopicProduceData::default()
                    .with_name(TopicName(StrBytes::from_static_str(TOPIC)))
                    .with_partition_data(vec![PartitionProduceData::default()
                        .with_index(PARTITION)
                        .with_records(Some(encoded.freeze()))])]),
        )
        .await;
    let partition = answered
        .responses
        .first()
        .and_then(|topic| topic.partition_responses.first())
        .unwrap_or_else(|| panic!("case {case:?}: Produce v{version} reported no partition"))
        .clone();
    assert_eq!(
        partition.error_code, 0,
        "case {case:?}: Produce v{version} must be accepted, got error {}",
        partition.error_code
    );
    partition.base_offset
}

/// `Fetch` the partition from `offset`, answering every record it holds as
/// `(offset, value)`.
async fn consume(
    client: &mut Client,
    negotiated: &Negotiated,
    case: &str,
    offset: i64,
) -> Vec<(i64, String)> {
    let version = negotiated.version::<FetchRequest, FetchResponse>(case, ApiKey::Fetch);
    let mut request = FetchRequest::default()
        .with_max_wait_ms(100)
        .with_min_bytes(0)
        .with_topics(vec![FetchTopic::default()
            .with_topic(TopicName(StrBytes::from_static_str(TOPIC)))
            .with_partitions(vec![FetchPartition::default()
                .with_partition(PARTITION)
                .with_fetch_offset(offset)
                .with_partition_max_bytes(1_048_576)])]);
    if version >= 3 {
        request = request.with_max_bytes(1_048_576);
    }
    let answered: FetchResponse = client.call(case, ApiKey::Fetch, version, &request).await;
    let partition = answered
        .responses
        .first()
        .and_then(|topic| topic.partitions.first())
        .unwrap_or_else(|| panic!("case {case:?}: Fetch v{version} reported no partition"))
        .clone();
    assert_eq!(
        partition.error_code, 0,
        "case {case:?}: Fetch v{version} must be answered, got error {}",
        partition.error_code
    );
    let Some(mut records) = partition.records.clone() else {
        return Vec::new();
    };
    if records.is_empty() {
        return Vec::new();
    }
    RecordBatchDecoder::decode(&mut records)
        .unwrap_or_else(|error| panic!("case {case:?}: the batch must decode: {error}"))
        .records
        .iter()
        .map(|record| {
            (
                record.offset,
                record
                    .value
                    .as_ref()
                    .map(|value| String::from_utf8_lossy(value).to_string())
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// Where the partition's log ENDS, as `ListOffsets` reports it.
async fn log_end(client: &mut Client, negotiated: &Negotiated, case: &str) -> i64 {
    let version =
        negotiated.version::<ListOffsetsRequest, ListOffsetsResponse>(case, ApiKey::ListOffsets);
    let answered: ListOffsetsResponse = client
        .call(
            case,
            ApiKey::ListOffsets,
            version,
            &ListOffsetsRequest::default()
                .with_replica_id(BrokerId(-1))
                .with_topics(vec![ListOffsetsTopic::default()
                    .with_name(TopicName(StrBytes::from_static_str(TOPIC)))
                    .with_partitions(vec![ListOffsetsPartition::default()
                        .with_partition_index(PARTITION)
                        .with_timestamp(OFFSET_LATEST)])]),
        )
        .await;
    let partition = answered
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .unwrap_or_else(|| panic!("case {case:?}: ListOffsets v{version} reported no partition"))
        .clone();
    assert_eq!(
        partition.error_code, 0,
        "case {case:?}: ListOffsets v{version} must be answered, got error {}",
        partition.error_code
    );
    partition.offset
}

/// `FindCoordinator` for `group`, answering the error code it was given.
async fn find_coordinator(
    client: &mut Client,
    negotiated: &Negotiated,
    case: &str,
    group: &str,
) -> i16 {
    let version = negotiated
        .version::<FindCoordinatorRequest, FindCoordinatorResponse>(case, ApiKey::FindCoordinator);
    // The key a request names is carried by `Key` up to version 3 and by the
    // `CoordinatorKeys` list from version 4 on, where the answer is per key too.
    let mut request = FindCoordinatorRequest::default();
    request = match version >= 4 {
        true => request.with_coordinator_keys(vec![StrBytes::from_string(group.to_string())]),
        false => request.with_key(StrBytes::from_string(group.to_string())),
    };
    let answered: FindCoordinatorResponse = client
        .call(case, ApiKey::FindCoordinator, version, &request)
        .await;
    match version >= 4 {
        true => answered
            .coordinators
            .first()
            .map(|coordinator| coordinator.error_code)
            .unwrap_or_else(|| {
                panic!("case {case:?}: FindCoordinator v{version} reported no coordinator")
            }),
        false => answered.error_code,
    }
}

/// `JoinGroup` for `group`, as a consuming client sends it.
async fn join_group(
    client: &mut Client,
    negotiated: &Negotiated,
    case: &str,
    group: &str,
    member_id: &str,
) -> JoinGroupResponse {
    let version = negotiated.version::<JoinGroupRequest, JoinGroupResponse>(case, ApiKey::JoinGroup);
    let mut request = JoinGroupRequest::default()
        .with_group_id(GroupId(StrBytes::from_string(group.to_string())))
        .with_session_timeout_ms(SESSION_TIMEOUT_MS)
        .with_member_id(StrBytes::from_string(member_id.to_string()))
        .with_protocol_type(StrBytes::from_static_str(CONSUMER_PROTOCOL))
        .with_protocols(vec![JoinGroupRequestProtocol::default()
            .with_name(StrBytes::from_static_str(RANGE_ASSIGNOR))
            .with_metadata(Bytes::from_static(b"subscription"))]);
    // `RebalanceTimeoutMs` arrived with version 1, so a join below it states none.
    if version >= 1 {
        request = request.with_rebalance_timeout_ms(REBALANCE_TIMEOUT_MS);
    }
    client.call(case, ApiKey::JoinGroup, version, &request).await
}

/// `SyncGroup` from the member `member_id`, stating the assignment it computed for itself.
async fn sync_group(
    client: &mut Client,
    negotiated: &Negotiated,
    case: &str,
    group: &str,
    generation: i32,
    member_id: &str,
) -> SyncGroupResponse {
    let version = negotiated.version::<SyncGroupRequest, SyncGroupResponse>(case, ApiKey::SyncGroup);
    client
        .call(
            case,
            ApiKey::SyncGroup,
            version,
            &SyncGroupRequest::default()
                .with_group_id(GroupId(StrBytes::from_string(group.to_string())))
                .with_generation_id(generation)
                .with_member_id(StrBytes::from_string(member_id.to_string()))
                .with_assignments(vec![SyncGroupRequestAssignment::default()
                    .with_member_id(StrBytes::from_string(member_id.to_string()))
                    .with_assignment(Bytes::from_static(b"partition-0"))]),
        )
        .await
}

/// `Heartbeat` quoting `generation` and `member_id`.
async fn heartbeat_group(
    client: &mut Client,
    negotiated: &Negotiated,
    case: &str,
    group: &str,
    generation: i32,
    member_id: &str,
) -> HeartbeatResponse {
    let version = negotiated.version::<HeartbeatRequest, HeartbeatResponse>(case, ApiKey::Heartbeat);
    client
        .call(
            case,
            ApiKey::Heartbeat,
            version,
            &HeartbeatRequest::default()
                .with_group_id(GroupId(StrBytes::from_string(group.to_string())))
                .with_generation_id(generation)
                .with_member_id(StrBytes::from_string(member_id.to_string())),
        )
        .await
}

/// `LeaveGroup` for the member `member_id`, which is how a closing consumer leaves.
async fn leave_group(
    client: &mut Client,
    negotiated: &Negotiated,
    case: &str,
    group: &str,
    member_id: &str,
) {
    let version =
        negotiated.version::<LeaveGroupRequest, LeaveGroupResponse>(case, ApiKey::LeaveGroup);
    // The leaving member is named by `MemberId` up to version 2 and by the `Members` list
    // from version 3 on.
    let mut request =
        LeaveGroupRequest::default().with_group_id(GroupId(StrBytes::from_string(group.to_string())));
    request = match version >= 3 {
        true => request.with_members(vec![MemberIdentity::default()
            .with_member_id(StrBytes::from_string(member_id.to_string()))]),
        false => request.with_member_id(StrBytes::from_string(member_id.to_string())),
    };
    let answered: LeaveGroupResponse = client
        .call(case, ApiKey::LeaveGroup, version, &request)
        .await;
    assert_eq!(
        answered.error_code, 0,
        "case {case:?}: LeaveGroup v{version} must be answered, got error {}",
        answered.error_code
    );
}

/// `OffsetCommit` of the next-offset `offset` for the suite's partition, answering the
/// error code the partition was given.
async fn commit_offset(
    client: &mut Client,
    negotiated: &Negotiated,
    case: &str,
    group: &str,
    generation: i32,
    member_id: &str,
    offset: i64,
) -> i16 {
    let version =
        negotiated.version::<OffsetCommitRequest, OffsetCommitResponse>(case, ApiKey::OffsetCommit);
    let answered: OffsetCommitResponse = client
        .call(
            case,
            ApiKey::OffsetCommit,
            version,
            &OffsetCommitRequest::default()
                .with_group_id(GroupId(StrBytes::from_string(group.to_string())))
                .with_generation_id_or_member_epoch(generation)
                .with_member_id(StrBytes::from_string(member_id.to_string()))
                .with_topics(vec![OffsetCommitRequestTopic::default()
                    .with_name(TopicName(StrBytes::from_static_str(TOPIC)))
                    .with_partitions(vec![OffsetCommitRequestPartition::default()
                        .with_partition_index(PARTITION)
                        .with_committed_offset(offset)])]),
        )
        .await;
    answered
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .map(|partition| partition.error_code)
        .unwrap_or_else(|| panic!("case {case:?}: OffsetCommit v{version} reported no partition"))
}

/// `OffsetFetch` for the suite's partition, as the coordinator answers it.
async fn fetch_committed(
    client: &mut Client,
    negotiated: &Negotiated,
    case: &str,
    group: &str,
) -> OffsetFetchResponse {
    let version =
        negotiated.version::<OffsetFetchRequest, OffsetFetchResponse>(case, ApiKey::OffsetFetch);
    client
        .call(
            case,
            ApiKey::OffsetFetch,
            version,
            &OffsetFetchRequest::default()
                .with_group_id(GroupId(StrBytes::from_string(group.to_string())))
                .with_topics(Some(vec![OffsetFetchRequestTopic::default()
                    .with_name(TopicName(StrBytes::from_static_str(TOPIC)))
                    .with_partition_indexes(vec![PARTITION])])),
        )
        .await
}

/// The committed next-offset the coordinator holds for `group` in the suite's partition.
async fn committed_offset(
    client: &mut Client,
    negotiated: &Negotiated,
    case: &str,
    group: &str,
) -> i64 {
    let answered = fetch_committed(client, negotiated, case, group).await;
    assert_eq!(
        answered.error_code, 0,
        "case {case:?}: OffsetFetch must be answered, got error {}",
        answered.error_code
    );
    let partition = answered
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .unwrap_or_else(|| panic!("case {case:?}: OffsetFetch reported no partition"))
        .clone();
    assert_eq!(
        partition.error_code, 0,
        "case {case:?}: OffsetFetch must report the partition, got error {}",
        partition.error_code
    );
    partition.committed_offset
}
