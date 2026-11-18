// SPDX-License-Identifier: CC0-1.0

//! The CONSUMER GROUPS a cluster's brokers coordinate, as a consuming client drives
//! them over the published bootstrap address.
//!
//! RULE under test: the coordinator runs ONE rebalance per generation. A `JoinGroup`
//! from a member the group does not hold, or from one it holds while the group is
//! stable, starts a rebalance; the generation advances once, every member is admitted at
//! that one generation and the group's LEADER is told the whole membership. The
//! assignment each member then holds is the one the LEADER stated for it in its
//! `SyncGroup` — a member that sends no assignment of its own is handed the leader's, not
//! an empty one. While a rebalance is in flight a `Heartbeat` answers
//! `REBALANCE_IN_PROGRESS`, a member quoting a generation the group has left answers
//! `ILLEGAL_GENERATION`, and one the group does not hold answers `UNKNOWN_MEMBER_ID`.
//!
//! The rows span what the rule generalises over: how many members the group holds (one,
//! which leads itself, and two, where one member's assignment comes from the other), and
//! which of the protocol's answers a member is given for the generation it quotes.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at
//! the address `GetBootstrapBrokers` published.

use std::time::Duration;

use ::bytes::Bytes;
use ::kafka_protocol::messages::{
    join_group_request::JoinGroupRequestProtocol, sync_group_request::SyncGroupRequestAssignment,
    ApiKey, GroupId, HeartbeatRequest, HeartbeatResponse, JoinGroupRequest, JoinGroupResponse,
    LeaveGroupRequest, LeaveGroupResponse, SyncGroupRequest, SyncGroupResponse,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::ResponseError;

use crate::broker_data_plane::Client;
use crate::kafka_harness::{ClusterRequest, Emulator};

/// The protocol type a consuming client joins with, and the assignor it names.
const CONSUMER_PROTOCOL: &str = "consumer";
const RANGE_ASSIGNOR: &str = "range";

/// The session and rebalance windows every join below states, in milliseconds: well
/// above the time a rebalance of this suite takes, so no row depends on how fast the
/// host runs.
const SESSION_TIMEOUT_MS: i32 = 30_000;
const REBALANCE_TIMEOUT_MS: i32 = 30_000;

/// How long this suite waits for a condition the service owes: well above the time a
/// rebalance of it takes, so no row depends on how fast the host runs.
const SETUP_DEADLINE: Duration = Duration::from_secs(60);

/// RULE: the one member of a group leads it, is told its own subscription back, and is
/// handed the assignment it stated.
#[tokio::test]
async fn the_only_member_of_a_group_leads_it_and_holds_its_own_assignment() {
    let case = "a group of one member";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "group-single").await;
    let authority = plaintext_address(&emulator, &cluster, case).await;
    let mut member = Client::plaintext(case, &authority).await;
    member.api_versions(case).await;

    let joined = join(&mut member, case, "solo", "").await;
    assert_eq!(
        joined.error_code, 0,
        "case {case:?}: the join must be admitted, got error {}",
        joined.error_code
    );
    assert_eq!(
        joined.leader, joined.member_id,
        "case {case:?}: the only member of a group is its leader"
    );
    assert_eq!(
        joined.members.len(),
        1,
        "case {case:?}: the leader must be told every member, got {:?}",
        joined.members
    );

    let assignment = Bytes::from_static(b"solo-0");
    let synced = sync(
        &mut member,
        case,
        "solo",
        joined.generation_id,
        &joined.member_id,
        &[(joined.member_id.to_string(), assignment.clone())],
    )
    .await;
    assert_eq!(
        (synced.error_code, synced.assignment.clone()),
        (0, assignment),
        "case {case:?}: the member must be handed the assignment the leader stated"
    );
}

/// RULE: a second member's join is one REBALANCE: both members are admitted at the same
/// generation, the leader is told both subscriptions, and the member that sends no
/// assignment of its own is handed the one the leader stated for it.
#[tokio::test]
async fn a_second_member_rebalances_the_group_and_is_assigned_by_its_leader() {
    let case = "a group of two members";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "group-pair").await;
    let authority = plaintext_address(&emulator, &cluster, case).await;
    let group = "readers";

    // The first member joins an empty group and takes the whole topic.
    let mut first = Client::plaintext(case, &authority).await;
    first.api_versions(case).await;
    let alone = join(&mut first, case, group, "").await;
    assert_eq!(
        alone.error_code, 0,
        "case {case:?}: the first join must be admitted, got error {}",
        alone.error_code
    );
    let first_id = alone.member_id.to_string();
    sync(
        &mut first,
        case,
        group,
        alone.generation_id,
        &alone.member_id,
        &[(first_id.clone(), Bytes::from_static(b"both-partitions"))],
    )
    .await;

    // The second member's join starts a rebalance the first must rejoin, which is what
    // its heartbeat is told. Both joins are in flight at once, as they are on a real
    // coordinator: the second member's call is answered only once the first has rejoined
    // the generation it started.
    let mut second = Client::plaintext(case, &authority).await;
    second.api_versions(case).await;
    let joining_second = join(&mut second, case, group, "");
    let rejoining_first = async {
        // LOOKS AGAIN until the coordinator reports the rebalance the second member
        // started, rather than assuming the two calls arrive in an order.
        let deadline = std::time::Instant::now() + SETUP_DEADLINE;
        loop {
            let beat = heartbeat(&mut first, case, group, alone.generation_id, &first_id).await;
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
        join(&mut first, case, group, &first_id).await
    };
    let (second_joined, first_rejoined) = ::tokio::join!(joining_second, rejoining_first);

    for (who, joined) in [("the second member", &second_joined), ("the first member", &first_rejoined)] {
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
    assert!(
        second_joined.generation_id > alone.generation_id,
        "case {case:?}: a rebalance must advance the generation past {}, got {}",
        alone.generation_id,
        second_joined.generation_id
    );
    assert_eq!(
        second_joined.leader, first_rejoined.leader,
        "case {case:?}: both members must be told the same leader, got {:?} and {:?}",
        second_joined.leader, first_rejoined.leader
    );

    // Whichever of them leads is the one told the whole membership; the other is told
    // none of it, because computing the assignment is the leader's half of the protocol.
    let leader = second_joined.leader.to_string();
    let (leader_joined, follower_joined, mut leader_client, mut follower_client) =
        match leader == second_joined.member_id.to_string() {
            true => (&second_joined, &first_rejoined, second, first),
            false => (&first_rejoined, &second_joined, first, second),
        };
    assert_eq!(
        leader_joined.members.len(),
        2,
        "case {case:?}: the leader must be told both members, got {:?}",
        leader_joined.members
    );
    assert!(
        follower_joined.members.is_empty(),
        "case {case:?}: a follower must be told no membership, got {:?}",
        follower_joined.members
    );

    // The leader states one assignment per member; the follower sends none and must be
    // handed the one the leader stated for it.
    let leader_id = leader_joined.member_id.to_string();
    let follower_id = follower_joined.member_id.to_string();
    let leader_part = Bytes::from_static(b"partition-0");
    let follower_part = Bytes::from_static(b"partition-1");
    let generation = leader_joined.generation_id;
    let leader_synced = sync(
        &mut leader_client,
        case,
        group,
        generation,
        &leader_joined.member_id,
        &[
            (leader_id.clone(), leader_part.clone()),
            (follower_id.clone(), follower_part.clone()),
        ],
    )
    .await;
    assert_eq!(
        (leader_synced.error_code, leader_synced.assignment.clone()),
        (0, leader_part),
        "case {case:?}: the leader must hold the part of its own assignment"
    );
    let follower_synced = sync(
        &mut follower_client,
        case,
        group,
        generation,
        &follower_joined.member_id,
        &[],
    )
    .await;
    assert_eq!(
        (follower_synced.error_code, follower_synced.assignment.clone()),
        (0, follower_part),
        "case {case:?}: the follower must be handed the assignment its leader stated, \
         not an empty one"
    );

    // What each member is told for the generation it quotes, once the group is in force.
    let rows: &[(&str, i32, &str, i16)] = &[
        ("the generation in force", generation, &follower_id, 0),
        (
            "a generation the group has left",
            generation - 1,
            &follower_id,
            ResponseError::IllegalGeneration.code(),
        ),
        (
            "a member the group does not hold",
            generation,
            "no-such-member",
            ResponseError::UnknownMemberId.code(),
        ),
    ];
    for (row, quoted, member_id, expected) in rows {
        let beat = heartbeat(&mut follower_client, case, group, *quoted, member_id).await;
        assert_eq!(
            beat.error_code, *expected,
            "case {case:?}: a heartbeat quoting {row} must answer {expected}, got {}",
            beat.error_code
        );
    }

    // And a member that leaves rebalances the group again, so the one that remains is
    // asked to rejoin rather than keeping an assignment computed for two.
    let left = leave(&mut follower_client, case, group, &follower_id).await;
    assert_eq!(
        left.error_code, 0,
        "case {case:?}: the member must be let out, got error {}",
        left.error_code
    );
    let beat = heartbeat(&mut leader_client, case, group, generation, &leader_id).await;
    assert_eq!(
        beat.error_code,
        ResponseError::RebalanceInProgress.code(),
        "case {case:?}: the departure of a member must rebalance the group, got {}",
        beat.error_code
    );
}

/// `JoinGroup` for `group`, as a consuming client sends it: `member_id` empty for a
/// member the coordinator has not named yet.
async fn join(
    client: &mut Client,
    case: &str,
    group: &str,
    member_id: &str,
) -> JoinGroupResponse {
    client
        .call(
            case,
            ApiKey::JoinGroup,
            5,
            &JoinGroupRequest::default()
                .with_group_id(GroupId(StrBytes::from_string(group.to_string())))
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

/// `SyncGroup` for one member, stating `assignments` — empty for a member that is not the
/// group's leader.
async fn sync(
    client: &mut Client,
    case: &str,
    group: &str,
    generation: i32,
    member_id: &StrBytes,
    assignments: &[(String, Bytes)],
) -> SyncGroupResponse {
    client
        .call(
            case,
            ApiKey::SyncGroup,
            3,
            &SyncGroupRequest::default()
                .with_group_id(GroupId(StrBytes::from_string(group.to_string())))
                .with_generation_id(generation)
                .with_member_id(member_id.clone())
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

/// `Heartbeat` for one member at the generation it believes it holds.
async fn heartbeat(
    client: &mut Client,
    case: &str,
    group: &str,
    generation: i32,
    member_id: &str,
) -> HeartbeatResponse {
    client
        .call(
            case,
            ApiKey::Heartbeat,
            3,
            &HeartbeatRequest::default()
                .with_group_id(GroupId(StrBytes::from_string(group.to_string())))
                .with_generation_id(generation)
                .with_member_id(StrBytes::from_string(member_id.to_string())),
        )
        .await
}

/// `LeaveGroup` for one member.
async fn leave(
    client: &mut Client,
    case: &str,
    group: &str,
    member_id: &str,
) -> LeaveGroupResponse {
    client
        .call(
            case,
            ApiKey::LeaveGroup,
            2,
            &LeaveGroupRequest::default()
                .with_group_id(GroupId(StrBytes::from_string(group.to_string())))
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
