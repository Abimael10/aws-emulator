// SPDX-License-Identifier: CC0-1.0

//! The OFFSETS a consumer group commits, and WHO the coordinator reports as holding it.
//!
//! RULE under test: the coordinator records the `client_id` each member presents with its
//! join and reports every member under it, beside the assignment the group's leader
//! stored for that member — a reader of the group tells one member's partitions from
//! another's by it. A commit is FENCED by that membership and by the generation: a commit
//! quoting a member the group no longer holds is `UNKNOWN_MEMBER_ID`, one quoting another
//! generation is `ILLEGAL_GENERATION`, one that quotes no generation and no member at all
//! is admitted only while no member holds the group, and a refused commit stores nothing
//! — so the position a group holds is the one the member that owns the partition
//! committed. The offsets themselves outlive the member that wrote them: a member that
//! takes the partitions over resumes at the committed offset, so no record it has already
//! read is delivered again.
//!
//! The rows span what the rule generalises over: which member and which generation a
//! commit quotes (the one in force, one the group has left, a member it never held, and
//! none at all), and whether the group holds a member when an unjoined commit arrives.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at
//! the address `GetBootstrapBrokers` published.

use std::time::Duration;

use ::bytes::Bytes;
use ::kafka_protocol::messages::{
    describe_groups_response::DescribedGroup,
    join_group_request::JoinGroupRequestProtocol,
    offset_commit_request::{OffsetCommitRequestPartition, OffsetCommitRequestTopic},
    offset_fetch_request::OffsetFetchRequestTopic,
    sync_group_request::SyncGroupRequestAssignment,
    ApiKey, DescribeGroupsRequest, DescribeGroupsResponse, GroupId, HeartbeatRequest,
    HeartbeatResponse, JoinGroupRequest, JoinGroupResponse, LeaveGroupRequest, LeaveGroupResponse,
    OffsetCommitRequest, OffsetCommitResponse, OffsetFetchRequest, OffsetFetchResponse,
    SyncGroupRequest, SyncGroupResponse, TopicName,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::ResponseError;
use serde_json::json;

use crate::broker_data_plane::Client;
use crate::kafka_harness::{ClusterRequest, Emulator};

/// The protocol type a consuming client joins with, and the assignor it names.
const CONSUMER_PROTOCOL: &str = "consumer";
const RANGE_ASSIGNOR: &str = "range";

/// The session and rebalance windows every join below states, in milliseconds: well
/// above the time this suite takes, so no row depends on how fast the host runs.
const SESSION_TIMEOUT_MS: i32 = 300_000;
const REBALANCE_TIMEOUT_MS: i32 = 300_000;

/// The group every row below drives, and the topic its members read.
const GROUP: &str = "readers";
const TOPIC: &str = "orders";

/// How long this suite waits for a condition the service owes: well above the time a
/// rebalance of it takes, so no row depends on how fast the host runs.
const SETUP_DEADLINE: Duration = Duration::from_secs(60);

/// RULE: each member is reported under the client id it presented, with the assignment the
/// leader stored for it; the offsets it commits survive it, so the member that takes its
/// partitions over resumes where it stopped and reads no record again.
#[tokio::test]
async fn a_group_reports_its_members_by_client_id_and_its_offsets_outlive_them() {
    let case = "a group whose member is replaced";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "group-offsets").await;
    let authority = plaintext_address(&emulator, &cluster, case).await;
    given_topic(&emulator, &cluster, TOPIC, 1).await;

    // Four records, so a resumed member has some to read and some to skip.
    let mut producer = Client::plaintext(case, &authority).await;
    producer.api_versions(case).await;
    let timestamp = epoch_millis();
    let produced: Vec<(i64, Option<&str>, &str)> = (0..4)
        .map(|index| (timestamp, None, ["r0", "r1", "r2", "r3"][index]))
        .collect();
    producer.produce(case, TOPIC, 0, &produced).await;

    // The first member joins under its own name and is handed the whole topic.
    let mut first = Client::plaintext(case, &authority).await.identified_as("reader-a");
    first.api_versions(case).await;
    let joined = join(&mut first, case, "").await;
    assert_eq!(
        joined.error_code, 0,
        "case {case:?}: the join must be admitted, got error {}",
        joined.error_code
    );
    let first_id = joined.member_id.to_string();
    let assignment = Bytes::from_static(b"partition-0");
    let synced = sync(
        &mut first,
        case,
        joined.generation_id,
        &first_id,
        &[(first_id.clone(), assignment.clone())],
    )
    .await;
    assert_eq!(
        synced.error_code, 0,
        "case {case:?}: the sync must be answered, got error {}",
        synced.error_code
    );

    // The coordinator reports it under the name its own client gave itself, with the
    // assignment the leader stored for it.
    let described = describe(&mut first, case).await;
    let members: Vec<(String, Bytes)> = described
        .members
        .iter()
        .map(|member| (member.client_id.to_string(), member.member_assignment.clone()))
        .collect();
    assert_eq!(
        members,
        vec![("reader-a".to_string(), assignment.clone())],
        "case {case:?}: the group must report its member by client id with its own \
         assignment, got {members:?}"
    );

    // It reads the first two records and commits where it stopped.
    let read = first.fetch(case, TOPIC, 0, 0).await;
    assert_eq!(
        read.len(),
        4,
        "case {case:?}: the member must read the records produced, got {read:?}"
    );
    let committed = commit(&mut first, case, joined.generation_id, &first_id, 2).await;
    assert_eq!(
        partition_errors(&committed),
        vec![0],
        "case {case:?}: the commit must be stored, got {:?}",
        partition_errors(&committed)
    );

    // What a commit quoting something else is told, while that member still holds the
    // group — none of which may move the position the group holds.
    let rows: &[(&str, i32, &str, i16)] = &[
        (
            "a generation the group has left",
            joined.generation_id - 1,
            &first_id,
            ResponseError::IllegalGeneration.code(),
        ),
        (
            "a member the group never held",
            joined.generation_id,
            "reader-ghost",
            ResponseError::UnknownMemberId.code(),
        ),
        (
            "no generation and no member, while a member holds the group",
            -1,
            "",
            ResponseError::UnknownMemberId.code(),
        ),
    ];
    for (row, generation, member_id, expected) in rows {
        let refused = commit(&mut first, row, *generation, member_id, 99).await;
        assert_eq!(
            partition_errors(&refused),
            vec![*expected],
            "case {row:?}: expected error {expected}, got {:?}",
            partition_errors(&refused)
        );
        assert_eq!(
            fetched_offset(&mut first, row).await,
            2,
            "case {row:?}: a refused commit must store nothing"
        );
    }

    // The member leaves, and the one that takes its place is reported under ITS name and
    // resumes at the committed offset: no record the group had read is delivered again.
    let left = leave(&mut first, case, &first_id).await;
    assert_eq!(
        left.error_code, 0,
        "case {case:?}: the member must be let out, got error {}",
        left.error_code
    );
    let mut second = Client::plaintext(case, &authority).await.identified_as("reader-b");
    second.api_versions(case).await;
    let rejoined = join(&mut second, case, "").await;
    assert_eq!(
        rejoined.error_code, 0,
        "case {case:?}: the second member's join must be admitted, got error {}",
        rejoined.error_code
    );
    let second_id = rejoined.member_id.to_string();
    sync(
        &mut second,
        case,
        rejoined.generation_id,
        &second_id,
        &[(second_id.clone(), assignment.clone())],
    )
    .await;
    let described = describe(&mut second, case).await;
    let members: Vec<String> = described
        .members
        .iter()
        .map(|member| member.client_id.to_string())
        .collect();
    assert_eq!(
        members,
        vec!["reader-b".to_string()],
        "case {case:?}: the group must now report only the member that holds it, got \
         {members:?}"
    );
    let resumed = fetched_offset(&mut second, case).await;
    assert_eq!(
        resumed, 2,
        "case {case:?}: the committed offset must outlive the member that wrote it, got \
         {resumed}"
    );
    let delivered = second.fetch(case, TOPIC, 0, resumed).await;
    let values: Vec<String> = delivered.iter().map(|(_, _, value)| value.clone()).collect();
    assert_eq!(
        values,
        vec!["r2".to_string(), "r3".to_string()],
        "case {case:?}: the resumed member must read only what the group had not, got \
         {values:?}"
    );

    // And once no member holds the group, a commit that joined no group is admitted —
    // which is a client storing its offsets in the group without taking part in it.
    let alone = leave(&mut second, case, &second_id).await;
    assert_eq!(
        alone.error_code, 0,
        "case {case:?}: the second member must be let out, got error {}",
        alone.error_code
    );
    let unjoined = commit(&mut second, "no generation, no member, empty group", -1, "", 3).await;
    assert_eq!(
        partition_errors(&unjoined),
        vec![0],
        "case {case:?}: a client storing offsets in an empty group must be admitted, got \
         {:?}",
        partition_errors(&unjoined)
    );
    assert_eq!(
        fetched_offset(&mut second, case).await,
        3,
        "case {case:?}: that commit must be the position the group now holds"
    );
}

/// RULE: a join the coordinator REFUSES is answered — with the protocol error that
/// refusal carries, in a frame the version the client negotiated can carry — not by
/// severing the connection: a client that is disconnected instead of refused reconnects
/// and sends the same join again, for as long as it keeps trying.
#[tokio::test]
async fn a_refused_join_is_answered_rather_than_severing_the_connection() {
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "group-refusals").await;
    let authority = plaintext_address(&emulator, &cluster, "refusals").await;
    given_topic(&emulator, &cluster, TOPIC, 1).await;

    // Every version of the join this endpoint serves, so a refusal is answered in each
    // of them rather than only in the newest.
    for version in 0..=5 {
        let case = format!("a member the group never held, JoinGroup v{version}");
        let mut client = Client::plaintext(&case, &authority).await;
        client.api_versions(&case).await;
        let refused = join_at(&mut client, &case, "reader-ghost", version).await;
        assert_eq!(
            refused.error_code,
            ResponseError::UnknownMemberId.code(),
            "case {case:?}: a join quoting a member the group never held must be answered \
             UNKNOWN_MEMBER_ID, got {}",
            refused.error_code
        );
    }
}

/// RULE: a second member's join is ONE rebalance both members are admitted at, and the
/// coordinator reports each of them under its own client id with the assignment the
/// leader stored for it — which is how a reader of the group tells one member's
/// partitions from the other's. A member the rebalance evicted is fenced out of the
/// group's position.
#[tokio::test]
async fn two_members_gather_in_one_rebalance_and_are_reported_by_their_own_client_ids() {
    let case = "two members in one group";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "group-pair").await;
    let authority = plaintext_address(&emulator, &cluster, case).await;
    given_topic(&emulator, &cluster, TOPIC, 4).await;

    // The first member joins an empty group and takes the whole topic.
    let mut first = Client::plaintext(case, &authority)
        .await
        .identified_as("member-a");
    first.api_versions(case).await;
    let alone = join(&mut first, case, "").await;
    let first_id = alone.member_id.to_string();
    sync(
        &mut first,
        case,
        alone.generation_id,
        &first_id,
        &[(first_id.clone(), Bytes::from_static(b"0,1,2,3"))],
    )
    .await;

    // The second member's join starts a rebalance the first must rejoin, which is what
    // its heartbeat is told. Both joins are in flight at once, as they are on a real
    // coordinator: the second member's call is answered only once the first has rejoined.
    let mut second = Client::plaintext(case, &authority)
        .await
        .identified_as("member-b");
    second.api_versions(case).await;
    let joining_second = join(&mut second, case, "");
    let rejoining_first = async {
        // LOOKS AGAIN until the coordinator reports the rebalance the second member
        // started, rather than assuming the two calls arrive in an order.
        let deadline = std::time::Instant::now() + SETUP_DEADLINE;
        loop {
            let beat = heartbeat(&mut first, case, alone.generation_id, &first_id).await;
            if beat.error_code == ResponseError::RebalanceInProgress.code() {
                break;
            }
            assert_eq!(
                beat.error_code, 0,
                "case {case:?}: a heartbeat of the member in force must be answered or told \
                 to rejoin, got {}",
                beat.error_code
            );
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: a second member's join must rebalance the group, but the \
                 first member was never told to rejoin"
            );
        }
        join(&mut first, case, &first_id).await
    };
    let (second_joined, first_rejoined) = ::tokio::join!(joining_second, rejoining_first);
    for (who, joined) in [
        ("the second member", &second_joined),
        ("the first member", &first_rejoined),
    ] {
        assert_eq!(
            joined.error_code, 0,
            "case {case:?}: {who} must be admitted to the rebalance, got error {}",
            joined.error_code
        );
    }
    assert_eq!(
        second_joined.generation_id, first_rejoined.generation_id,
        "case {case:?}: every member of one rebalance must be admitted at one generation, \
         got {} and {}",
        second_joined.generation_id, first_rejoined.generation_id
    );

    // The leader states one assignment per member — the contiguous halves of the four
    // partitions — and every member holds its own part of it.
    let leader = second_joined.leader.to_string();
    let generation = second_joined.generation_id;
    let (leader_client, follower_client, leader_id, follower_id) =
        match leader == second_joined.member_id.to_string() {
            true => (
                &mut second,
                &mut first,
                second_joined.member_id.to_string(),
                first_rejoined.member_id.to_string(),
            ),
            false => (
                &mut first,
                &mut second,
                first_rejoined.member_id.to_string(),
                second_joined.member_id.to_string(),
            ),
        };
    let leader_part = Bytes::from_static(b"0,1");
    let follower_part = Bytes::from_static(b"2,3");
    let synced = sync(
        leader_client,
        case,
        generation,
        &leader_id,
        &[
            (leader_id.clone(), leader_part.clone()),
            (follower_id.clone(), follower_part.clone()),
        ],
    )
    .await;
    assert_eq!(
        (synced.error_code, synced.assignment.clone()),
        (0, leader_part.clone()),
        "case {case:?}: the leader must hold its own part of the assignment"
    );
    let synced = sync(follower_client, case, generation, &follower_id, &[]).await;
    assert_eq!(
        (synced.error_code, synced.assignment.clone()),
        (0, follower_part.clone()),
        "case {case:?}: the follower must be handed the assignment its leader stated"
    );

    // Both members are reported, each under the client id its own client gave itself and
    // each with the part of the assignment the leader stored for it.
    let described = describe(leader_client, case).await;
    let mut members: Vec<(String, Bytes)> = described
        .members
        .iter()
        .map(|member| (member.client_id.to_string(), member.member_assignment.clone()))
        .collect();
    members.sort();
    let expected = match leader_id == second_joined.member_id.to_string() {
        true => vec![
            ("member-a".to_string(), follower_part.clone()),
            ("member-b".to_string(), leader_part.clone()),
        ],
        false => vec![
            ("member-a".to_string(), leader_part.clone()),
            ("member-b".to_string(), follower_part.clone()),
        ],
    };
    assert_eq!(
        members, expected,
        "case {case:?}: the group must report both members by client id with their own \
         assignments, got {members:?}"
    );

    // A member that LEFT the group is fenced out of its position: the commit it attempts
    // afterwards is refused and the group keeps what the member that holds the partitions
    // committed.
    let committed = commit(leader_client, case, generation, &leader_id, 2).await;
    assert_eq!(
        partition_errors(&committed),
        vec![0],
        "case {case:?}: the member in force must be able to commit, got {:?}",
        partition_errors(&committed)
    );
    leave(follower_client, case, &follower_id).await;
    let refused = commit(follower_client, case, generation, &follower_id, 99).await;
    assert_eq!(
        partition_errors(&refused),
        vec![ResponseError::UnknownMemberId.code()],
        "case {case:?}: the member that left must be fenced out of the group's position, \
         got {:?}",
        partition_errors(&refused)
    );
    assert_eq!(
        fetched_offset(leader_client, case).await,
        2,
        "case {case:?}: the group must keep the position its remaining member committed"
    );
}

/// `JoinGroup`, as a consuming client sends it: `member_id` empty for a member the
/// coordinator has not named yet.
async fn join(client: &mut Client, case: &str, member_id: &str) -> JoinGroupResponse {
    join_at(client, case, member_id, 5).await
}

/// The same join at one version of the API.
async fn join_at(
    client: &mut Client,
    case: &str,
    member_id: &str,
    version: i16,
) -> JoinGroupResponse {
    client
        .call(
            case,
            ApiKey::JoinGroup,
            version,
            &JoinGroupRequest::default()
                .with_group_id(GroupId(StrBytes::from_static_str(GROUP)))
                .with_session_timeout_ms(SESSION_TIMEOUT_MS)
                .with_rebalance_timeout_ms(REBALANCE_TIMEOUT_MS)
                .with_member_id(StrBytes::from_string(member_id.to_string()))
                .with_protocol_type(StrBytes::from_static_str(CONSUMER_PROTOCOL))
                .with_protocols(vec![JoinGroupRequestProtocol::default()
                    .with_name(StrBytes::from_static_str(RANGE_ASSIGNOR))
                    .with_metadata(Bytes::from_static(b"subscription"))]),
        )
        .await
}

/// `SyncGroup` for one member, stating `assignments`.
async fn sync(
    client: &mut Client,
    case: &str,
    generation: i32,
    member_id: &str,
    assignments: &[(String, Bytes)],
) -> SyncGroupResponse {
    client
        .call(
            case,
            ApiKey::SyncGroup,
            3,
            &SyncGroupRequest::default()
                .with_group_id(GroupId(StrBytes::from_static_str(GROUP)))
                .with_generation_id(generation)
                .with_member_id(StrBytes::from_string(member_id.to_string()))
                .with_assignments(
                    assignments
                        .iter()
                        .map(|(member, assignment)| {
                            SyncGroupRequestAssignment::default()
                                .with_member_id(StrBytes::from_string(member.clone()))
                                .with_assignment(assignment.clone())
                        })
                        .collect(),
                ),
        )
        .await
}

/// `OffsetCommit` of one partition at `offset`, quoting `generation` and `member_id`.
async fn commit(
    client: &mut Client,
    case: &str,
    generation: i32,
    member_id: &str,
    offset: i64,
) -> OffsetCommitResponse {
    client
        .call(
            case,
            ApiKey::OffsetCommit,
            6,
            &OffsetCommitRequest::default()
                .with_group_id(GroupId(StrBytes::from_static_str(GROUP)))
                .with_generation_id_or_member_epoch(generation)
                .with_member_id(StrBytes::from_string(member_id.to_string()))
                .with_topics(vec![OffsetCommitRequestTopic::default()
                    .with_name(TopicName(StrBytes::from_static_str(TOPIC)))
                    .with_partitions(vec![OffsetCommitRequestPartition::default()
                        .with_partition_index(0)
                        .with_committed_offset(offset)])]),
        )
        .await
}

/// The error code each partition of a commit was answered with.
fn partition_errors(answered: &OffsetCommitResponse) -> Vec<i16> {
    answered
        .topics
        .iter()
        .flat_map(|topic| topic.partitions.iter().map(|partition| partition.error_code))
        .collect()
}

/// The offset the group has committed for the topic's one partition.
async fn fetched_offset(client: &mut Client, case: &str) -> i64 {
    let answered: OffsetFetchResponse = client
        .call(
            case,
            ApiKey::OffsetFetch,
            5,
            &OffsetFetchRequest::default()
                .with_group_id(GroupId(StrBytes::from_static_str(GROUP)))
                .with_topics(Some(vec![OffsetFetchRequestTopic::default()
                    .with_name(TopicName(StrBytes::from_static_str(TOPIC)))
                    .with_partition_indexes(vec![0])])),
        )
        .await;
    answered
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .map(|partition| partition.committed_offset)
        .unwrap_or_else(|| panic!("case {case:?}: OffsetFetch reported no partition"))
}

/// `DescribeGroups` for the group, answering what the coordinator reports about it.
async fn describe(client: &mut Client, case: &str) -> DescribedGroup {
    let answered: DescribeGroupsResponse = client
        .call(
            case,
            ApiKey::DescribeGroups,
            4,
            &DescribeGroupsRequest::default()
                .with_groups(vec![GroupId(StrBytes::from_static_str(GROUP))]),
        )
        .await;
    answered
        .groups
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: DescribeGroups reported no group"))
}

/// `Heartbeat` for one member at the generation it believes it holds.
async fn heartbeat(
    client: &mut Client,
    case: &str,
    generation: i32,
    member_id: &str,
) -> HeartbeatResponse {
    client
        .call(
            case,
            ApiKey::Heartbeat,
            3,
            &HeartbeatRequest::default()
                .with_group_id(GroupId(StrBytes::from_static_str(GROUP)))
                .with_generation_id(generation)
                .with_member_id(StrBytes::from_string(member_id.to_string())),
        )
        .await
}

/// `LeaveGroup` for one member.
async fn leave(client: &mut Client, case: &str, member_id: &str) -> LeaveGroupResponse {
    client
        .call(
            case,
            ApiKey::LeaveGroup,
            2,
            &LeaveGroupRequest::default()
                .with_group_id(GroupId(StrBytes::from_static_str(GROUP)))
                .with_member_id(StrBytes::from_string(member_id.to_string())),
        )
        .await
}

/// A cluster whose brokers are reached in the clear, so this suite dials one socket
/// without a credential.
async fn given_plaintext_cluster(emulator: &Emulator, name: &str) -> String {
    emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named(name)
        })
        .await
}

/// The address a cluster publishes for its plaintext path, once it publishes one.
async fn plaintext_address(emulator: &Emulator, cluster: &str, case: &str) -> String {
    emulator
        .published_bootstrap(cluster, "bootstrapBrokerString", case)
        .await
}

/// A topic of the cluster that keeps every record for as long as this suite runs, so what
/// a member reads does not depend on when it read.
async fn given_topic(emulator: &Emulator, cluster: &str, name: &str, partitions: i32) {
    let configs = ::aws_smithy_types::base64::encode(json!({"retention.ms": "-1"}).to_string());
    let reply = emulator
        .call(
            "POST",
            &format!(
                "/v1/clusters/{}/topics",
                crate::kafka_harness::pct(cluster)
            ),
            Some(json!({
                "topicName": name,
                "partitionCount": partitions,
                "replicationFactor": 1,
                "configs": configs,
            })),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "the topic {name} must be created, got {} {}",
        reply.status,
        reply.error_message()
    );
}

/// The current instant in epoch milliseconds, the unit a record's timestamp is in.
fn epoch_millis() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is past the epoch")
            .as_millis(),
    )
    .expect("the instant fits in an i64")
}
