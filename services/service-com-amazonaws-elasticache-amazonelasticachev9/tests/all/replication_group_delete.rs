// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DeleteReplicationGroup`: "Deletes an existing replication group. By
//! default, this operation deletes the entire replication group, including the
//! primary/primaries and all of the read replicas."
//!
//! Every assertion goes over the wire through the shared [`crate::harness`]: an `awsQuery`
//! form-urlencoded request is dispatched through the production router and only the HTTP
//! status and the XML body are inspected, so a response that is not computed from persisted
//! state cannot pass. The one thing reached through the store is the DUE INSTANT of a change
//! the clock already owes ([`every_pending_change_due_in`]) — the status itself is the
//! handlers' own — so a row that reads a resource inside its transient phase does not depend
//! on how fast the host runs.
//!
//! The rule under test: a delete is served only while the group AND every member cluster it
//! is made of are in a state a delete may begin from (`available`); a member in any other
//! state — one being snapshotted, one still `creating`, one `modifying` — is refused
//! `InvalidReplicationGroupState` (HTTP 400) naming THAT member cluster, a group that is
//! itself in another state is refused the same fault naming the group and the status it
//! carries, and either way the group is left exactly as it was. Once the clock has settled
//! the work that status stood for, the same call is served: the group answers `deleting` and
//! it and its member clusters are gone.
//!
//! The tables span what the rule generalises over: WHICH member of the group is held
//! (its only member, the primary of a shard that has a replica, a replica, the primary of
//! a second shard), the group's SHAPE (member count, shard count, cluster mode), and the two
//! members that change what the delete takes (`FinalSnapshotIdentifier`,
//! `RetainPrimaryCluster`).

use std::time::Duration;

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::cache_data_plane::{command, endpoint_of, open, Resp};
use crate::harness::{assert_refusal, params, poll_until, Emulator, Reply, REGION};
use crate::lifecycle_convergence::{every_pending_change_due_in, HELD_OUT_OF_REACH};

/// Create one replication group and answer the create's reply.
async fn create_group(emulator: &Emulator, id: &str, extra: &[(&str, &str)]) -> Reply {
    let mut pairs = params(&[
        ("ReplicationGroupId", id),
        ("ReplicationGroupDescription", "a group this suite deletes"),
    ]);
    pairs.extend(params(extra));
    emulator.call("CreateReplicationGroup", &pairs).await
}

/// Create one replication group and look again until the clock has made it `available`.
async fn available_group(emulator: &Emulator, case: &str, id: &str, extra: &[(&str, &str)]) {
    let created = create_group(emulator, id, extra).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the create must answer 200, got {} {}",
        created.status, created.body
    );
    poll_until(
        emulator,
        REGION,
        case,
        "the group reaching available",
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", id)]),
        |reply| reply.text("ReplicationGroup/Status") == Some(state::STATUS_AVAILABLE),
    )
    .await;
}

/// `DescribeReplicationGroups` for one group id.
async fn describe_group(emulator: &Emulator, id: &str) -> Reply {
    emulator
        .call(
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", id)]),
        )
        .await
}

/// `DescribeCacheClusters` for one cluster id, with its node detail.
async fn describe_cluster(emulator: &Emulator, id: &str) -> Reply {
    emulator
        .call(
            "DescribeCacheClusters",
            &params(&[("CacheClusterId", id), ("ShowCacheNodeInfo", "true")]),
        )
        .await
}

/// `DeleteReplicationGroup` for one group id, plus whatever `extra` names.
async fn delete_group(emulator: &Emulator, id: &str, extra: &[(&str, &str)]) -> Reply {
    let mut pairs = params(&[("ReplicationGroupId", id)]);
    pairs.extend(params(extra));
    emulator.call("DeleteReplicationGroup", &pairs).await
}

/// Take one snapshot of the member cluster `cluster`, asserting it was accepted.
async fn snapshot_member(emulator: &Emulator, case: &str, name: &str, cluster: &str) {
    let created = emulator
        .call(
            "CreateSnapshot",
            &params(&[("SnapshotName", name), ("CacheClusterId", cluster)]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the snapshot of {cluster} must be accepted, got {} {}",
        created.status, created.body
    );
}

/// Assert the group `id` is gone: every reader of one answers the modelled not-found fault,
/// and so does a second delete of the same id.
async fn assert_group_gone(emulator: &Emulator, case: &str, id: &str) {
    let described = describe_group(emulator, id).await;
    assert_eq!(
        (described.status, described.error_code()),
        (404, "ReplicationGroupNotFoundFault"),
        "case {case:?}: the deleted group must be gone, got {} {}",
        described.status,
        described.body
    );
    let again = delete_group(emulator, id, &[]).await;
    assert_eq!(
        (again.status, again.error_code()),
        (404, "ReplicationGroupNotFoundFault"),
        "case {case:?}: deleting it twice must answer the not-found fault, got {} {}",
        again.status,
        again.body
    );
}

/// One held-member row: the case, the group id, the pairs that create it, and the member
/// cluster a snapshot is taken of — the one the refusal has to name.
type HeldMemberCase = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static str,
);

#[tokio::test]
async fn a_delete_is_refused_while_a_member_cluster_is_being_snapshotted() {
    let emulator = Emulator::new();
    // The axis: WHICH member of the group holds the delete up — the group's only member,
    // the primary of a shard that also has a replica, that shard's replica, and the primary
    // of the SECOND shard of a partitioned group. The member cluster ids are the ones
    // ElastiCache derives from the group id: `<group>-00N`, and `<group>-000S-00N` once the
    // keyspace is partitioned.
    let cases: [HeldMemberCase; 4] = [
        (
            "the group's only member",
            "lone-rg",
            &[("ReplicasPerNodeGroup", "0")],
            "lone-rg-001",
        ),
        (
            "the primary of a shard with a replica",
            "pair-rg",
            &[("NumCacheClusters", "2")],
            "pair-rg-001",
        ),
        (
            "the replica of a shard",
            "replica-rg",
            &[("NumCacheClusters", "2")],
            "replica-rg-002",
        ),
        (
            "the primary of the second shard of a partitioned group",
            "shards-rg",
            &[("NumNodeGroups", "2"), ("ReplicasPerNodeGroup", "0")],
            "shards-rg-0002-001",
        ),
    ];

    for (case, id, extra, member) in cases {
        available_group(&emulator, case, id, extra).await;
        snapshot_member(&emulator, case, &format!("{id}-final"), member).await;
        // The snapshot's span is held out of reach, so the member really is being
        // snapshotted when the delete arrives, whatever the host is doing.
        every_pending_change_due_in(&emulator, HELD_OUT_OF_REACH);
        let held = describe_cluster(&emulator, member).await;
        assert_eq!(
            held.text("CacheCluster/CacheClusterStatus"),
            Some(state::STATUS_SNAPSHOTTING),
            "case {case:?}: {member} must be reported as being snapshotted, got {}",
            held.body
        );

        let refused = delete_group(&emulator, id, &[]).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "InvalidReplicationGroupState"),
            "case {case:?}: expected 400 InvalidReplicationGroupState, got {} {}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            format!("Cache cluster {member} is not in a valid state to be deleted."),
            "case {case:?}: the refusal must name the member cluster that holds it up, got {}",
            refused.body
        );
        // "The group must still exist afterwards": a refused delete takes nothing.
        let still = describe_group(&emulator, id).await;
        assert_eq!(
            (still.status, still.text("ReplicationGroup/Status")),
            (200, Some(state::STATUS_AVAILABLE)),
            "case {case:?}: the refused delete must leave the group as it was, got {} {}",
            still.status,
            still.body
        );

        // The member is released by the CLOCK: every outstanding change becomes due and the
        // suite looks again until the member is reported `available`.
        every_pending_change_due_in(&emulator, Duration::ZERO);
        poll_until(
            &emulator,
            REGION,
            case,
            "the snapshotted member reaching available",
            "DescribeCacheClusters",
            &params(&[("CacheClusterId", member)]),
            |reply| {
                reply.text("CacheCluster/CacheClusterStatus") == Some(state::STATUS_AVAILABLE)
            },
        )
        .await;

        let deleted = delete_group(&emulator, id, &[]).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the delete must be served once the snapshot has finished, got {} {}",
            deleted.status, deleted.body
        );
        assert_eq!(
            deleted.text("ReplicationGroup/Status"),
            Some(state::STATUS_DELETING),
            "case {case:?}: the answer must report the group going away, got {}",
            deleted.body
        );
        assert_group_gone(&emulator, case, id).await;
    }
}

#[tokio::test]
async fn a_delete_of_a_group_the_clock_has_not_settled_is_refused_in_the_groups_own_words() {
    let emulator = Emulator::new();
    // The axis: HOW the group came to carry a status other than `available` — a create the
    // clock has not carried through, and a change in flight — and how the request spells the
    // id, because the refusal names the group as the service STORES it ("this value is
    // stored as a lowercase string") rather than as the request spelled it.
    let created = create_group(&emulator, "Fresh-RG", &[("ReplicasPerNodeGroup", "0")]).await;
    assert_eq!(
        created.status, 200,
        "the create must answer 200, got {} {}",
        created.status, created.body
    );
    every_pending_change_due_in(&emulator, HELD_OUT_OF_REACH);
    let refused = delete_group(&emulator, "FRESH-RG", &[]).await;
    assert_eq!(
        (refused.status, refused.error_code()),
        (400, "InvalidReplicationGroupState"),
        "a group that is still creating must be refused, got {} {}",
        refused.status,
        refused.body
    );
    assert_eq!(
        refused.error_message(),
        format!(
            "Replication group fresh-rg has status {} which is not valid for deletion.",
            state::STATUS_CREATING
        ),
        "the refusal must name the stored group id and the status it carries, got {}",
        refused.body
    );
    every_pending_change_due_in(&emulator, Duration::ZERO);

    // A change in flight: the group answers `modifying` until the clock owes it `available`.
    available_group(
        &emulator,
        "scaling-rg",
        "scaling-rg",
        &[("ReplicasPerNodeGroup", "0")],
    )
    .await;
    let scaled = emulator
        .call(
            "IncreaseReplicaCount",
            &params(&[
                ("ReplicationGroupId", "scaling-rg"),
                ("NewReplicaCount", "1"),
                ("ApplyImmediately", "true"),
            ]),
        )
        .await;
    assert_eq!(
        scaled.status, 200,
        "the replica count must be increased, got {} {}",
        scaled.status, scaled.body
    );
    every_pending_change_due_in(&emulator, HELD_OUT_OF_REACH);
    let refused = delete_group(&emulator, "scaling-rg", &[]).await;
    assert_eq!(
        (refused.status, refused.error_code()),
        (400, "InvalidReplicationGroupState"),
        "a group that is still modifying must be refused, got {} {}",
        refused.status,
        refused.body
    );
    assert_eq!(
        refused.error_message(),
        format!(
            "Replication group scaling-rg has status {} which is not valid for deletion.",
            state::STATUS_MODIFYING
        ),
        "the refusal must name the stored group id and the status it carries, got {}",
        refused.body
    );

    // Both groups are deletable once the clock has settled them.
    every_pending_change_due_in(&emulator, Duration::ZERO);
    for id in ["fresh-rg", "scaling-rg"] {
        poll_until(
            &emulator,
            REGION,
            id,
            "the group reaching available",
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", id)]),
            |reply| reply.text("ReplicationGroup/Status") == Some(state::STATUS_AVAILABLE),
        )
        .await;
        let deleted = delete_group(&emulator, id, &[]).await;
        assert_eq!(
            (deleted.status, deleted.text("ReplicationGroup/Status")),
            (200, Some(state::STATUS_DELETING)),
            "case {id:?}: the settled group must be deletable, got {} {}",
            deleted.status,
            deleted.body
        );
        assert_group_gone(&emulator, id, id).await;
    }
}

/// One status-gate row: the case, the group id, the pairs that create it, the ACTION that
/// takes the group out of `available` (the `awsQuery` action name, empty when the row holds
/// the create itself) with the pairs that call it, and the status the group then carries.
type StatusGateCase = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static str,
);

#[tokio::test]
async fn the_status_gate_refusal_reports_the_status_the_group_itself_carries() {
    let emulator = Emulator::new();
    // The axis: WHICH outstanding piece of work the group's status stands for when the
    // delete arrives — a create the clock has not carried through, a data migration in
    // flight, a failover setting being applied, a replica being added. The refusal is the
    // one sentence for all of them, and its status is read off the group's own record, so
    // each row asserts the sentence built from the status `DescribeReplicationGroups`
    // reports rather than from a literal.
    let cases: [StatusGateCase; 4] = [
        (
            "a create the clock has not carried through",
            "gate-fresh-rg",
            &[("ReplicasPerNodeGroup", "0")],
            "",
            &[],
            state::STATUS_CREATING,
        ),
        (
            "a data migration in flight",
            "gate-mig-rg",
            &[("NumCacheClusters", "2"), ("AutomaticFailoverEnabled", "true")],
            "StartMigration",
            &[
                ("CustomerNodeEndpointList.member.1.Address", "10.0.0.7"),
                ("CustomerNodeEndpointList.member.1.Port", "6379"),
            ],
            state::STATUS_MODIFYING,
        ),
        (
            "a failover setting being applied",
            "gate-failover-rg",
            &[("NumCacheClusters", "2")],
            "ModifyReplicationGroup",
            &[
                ("AutomaticFailoverEnabled", "true"),
                ("ApplyImmediately", "true"),
            ],
            state::STATUS_MODIFYING,
        ),
        (
            "a replica being added",
            "gate-grow-rg",
            &[("ReplicasPerNodeGroup", "0")],
            "IncreaseReplicaCount",
            &[("NewReplicaCount", "1"), ("ApplyImmediately", "true")],
            state::STATUS_MODIFYING,
        ),
    ];

    for (case, id, extra, action, action_pairs, expected_status) in cases {
        if action.is_empty() {
            let created = create_group(&emulator, id, extra).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: the create must answer 200, got {} {}",
                created.status, created.body
            );
        } else {
            available_group(&emulator, case, id, extra).await;
            let mut pairs = params(&[("ReplicationGroupId", id)]);
            pairs.extend(params(action_pairs));
            let started = emulator.call(action, &pairs).await;
            assert_eq!(
                started.status, 200,
                "case {case:?}: {action} must be accepted, got {} {}",
                started.status, started.body
            );
        }
        // The outstanding work is held out of reach, so the group really is in its
        // transient status when the delete arrives, whatever the host is doing.
        every_pending_change_due_in(&emulator, HELD_OUT_OF_REACH);

        let held = describe_group(&emulator, id).await;
        let reported = held
            .text("ReplicationGroup/Status")
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            reported, expected_status,
            "case {case:?}: the group must report {expected_status}, got {}",
            held.body
        );

        let refused = delete_group(&emulator, id, &[]).await;
        assert_refusal(
            case,
            &refused,
            400,
            "InvalidReplicationGroupState",
            &format!(
                "Replication group {id} has status {reported} which is not valid for deletion."
            ),
        );
        // "The group must survive the refusal": exactly the one group is still reported,
        // in the status it was in.
        let still = describe_group(&emulator, id).await;
        assert_eq!(
            (
                still.status,
                still.all("ReplicationGroup/ReplicationGroupId"),
                still.text("ReplicationGroup/Status")
            ),
            (200, vec![id], Some(reported.as_str())),
            "case {case:?}: the refused delete must leave the group as it was, got {} {}",
            still.status,
            still.body
        );

        // Once the clock has settled the work the status stood for, the identical delete
        // is served and the group goes away.
        every_pending_change_due_in(&emulator, Duration::ZERO);
        poll_until(
            &emulator,
            REGION,
            case,
            "the group reaching available",
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", id)]),
            |reply| reply.text("ReplicationGroup/Status") == Some(state::STATUS_AVAILABLE),
        )
        .await;
        let deleted = delete_group(&emulator, id, &[]).await;
        assert_eq!(
            (deleted.status, deleted.text("ReplicationGroup/Status")),
            (200, Some(state::STATUS_DELETING)),
            "case {case:?}: the settled group must be deletable, got {} {}",
            deleted.status,
            deleted.body
        );
        assert_group_gone(&emulator, case, id).await;
    }
}

/// One shape row: the case, the group id, the pairs that create it, and the member cluster
/// ids the group is made of — the clusters the delete has to take with it.
type ShapeCase = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static [&'static str],
);

#[tokio::test]
async fn a_delete_takes_every_member_cluster_of_the_group_with_it() {
    let emulator = Emulator::new();
    // The axis: the group's shape — how many clusters it is made of, how many shards they
    // are spread over, and whether the keyspace is partitioned.
    let cases: [ShapeCase; 3] = [
        (
            "one cluster",
            "single-rg",
            &[("ReplicasPerNodeGroup", "0")],
            &["single-rg-001"],
        ),
        (
            "three clusters in one shard",
            "trio-rg",
            &[("NumCacheClusters", "3")],
            &["trio-rg-001", "trio-rg-002", "trio-rg-003"],
        ),
        (
            "two shards with a replica each",
            "split-rg",
            &[("NumNodeGroups", "2"), ("ReplicasPerNodeGroup", "1")],
            &[
                "split-rg-0001-001",
                "split-rg-0001-002",
                "split-rg-0002-001",
                "split-rg-0002-002",
            ],
        ),
    ];

    for (case, id, extra, members) in cases {
        available_group(&emulator, case, id, extra).await;
        // Every member is a cluster of the account's before the delete.
        for member in members {
            let described = describe_cluster(&emulator, member).await;
            assert_eq!(
                described.status, 200,
                "case {case:?}: {member} must be a cluster of this account, got {} {}",
                described.status, described.body
            );
        }

        let deleted = delete_group(&emulator, id, &[]).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the delete must answer 200, got {} {}",
            deleted.status, deleted.body
        );
        assert_eq!(
            deleted.text("ReplicationGroup/Status"),
            Some(state::STATUS_DELETING),
            "case {case:?}: the answer must report the group going away, got {}",
            deleted.body
        );
        assert_eq!(
            deleted.all("MemberClusters/ClusterId"),
            members.to_vec(),
            "case {case:?}: the answer must report the members the group held, got {}",
            deleted.body
        );
        let shard_statuses = deleted.all("NodeGroup/Status");
        assert!(
            !shard_statuses.is_empty()
                && shard_statuses
                    .iter()
                    .all(|status| *status == state::STATUS_DELETING),
            "case {case:?}: every shard must report going away, got {shard_statuses:?}"
        );

        assert_group_gone(&emulator, case, id).await;
        for member in members {
            let described = describe_cluster(&emulator, member).await;
            assert_eq!(
                (described.status, described.error_code()),
                (404, "CacheClusterNotFound"),
                "case {case:?}: {member} must be gone with the group, got {} {}",
                described.status,
                described.body
            );
        }
    }
}

/// One final-snapshot row: the case, the group id, the pairs that create it, and the member
/// cluster ids whose nodes the final snapshot has to capture — one primary per shard.
type FinalSnapshotCase = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    &'static [&'static str],
);

#[tokio::test]
async fn a_final_snapshot_is_taken_from_each_shards_primary_before_the_group_goes() {
    let emulator = Emulator::new();
    // The axis: how many shards the group has, because "ElastiCache creates the snapshot
    // from the primary node in the cluster, rather than one of the replicas" — so the
    // captured nodes are one primary per shard, whatever the replicas are.
    let cases: [FinalSnapshotCase; 3] = [
        (
            "one shard, no replica",
            "final-one-rg",
            &[("ReplicasPerNodeGroup", "0")],
            &["final-one-rg-001"],
        ),
        (
            "one shard with two replicas",
            "final-replicas-rg",
            &[("NumCacheClusters", "3")],
            &["final-replicas-rg-001"],
        ),
        (
            "three shards",
            "final-shards-rg",
            &[("NumNodeGroups", "3"), ("ReplicasPerNodeGroup", "0")],
            &[
                "final-shards-rg-0001-001",
                "final-shards-rg-0002-001",
                "final-shards-rg-0003-001",
            ],
        ),
    ];

    for (case, id, extra, primaries) in cases {
        available_group(&emulator, case, id, extra).await;
        let snapshot_name = format!("{id}-parting-shot");
        let deleted = delete_group(
            &emulator,
            id,
            &[("FinalSnapshotIdentifier", &snapshot_name)],
        )
        .await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the delete must answer 200, got {} {}",
            deleted.status, deleted.body
        );
        assert_group_gone(&emulator, case, id).await;

        // The snapshot outlives the group it was taken of, and reports the group as its
        // source.
        let described = emulator
            .call(
                "DescribeSnapshots",
                &params(&[
                    ("SnapshotName", &snapshot_name),
                    ("ShowNodeGroupConfig", "true"),
                ]),
            )
            .await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: the final snapshot must be reported, got {} {}",
            described.status, described.body
        );
        assert_eq!(
            described.text("Snapshot/ReplicationGroupId"),
            Some(id),
            "case {case:?}: the snapshot must name the group it was taken of, got {}",
            described.body
        );
        assert_eq!(
            described.text("Snapshot/ReplicationGroupDescription"),
            Some("a group this suite deletes"),
            "case {case:?}: the snapshot must carry the source group's description, got {}",
            described.body
        );
        assert_eq!(
            described.text("Snapshot/NumNodeGroups"),
            Some(primaries.len().to_string().as_str()),
            "case {case:?}: the snapshot must report the shard count it captured, got {}",
            described.body
        );
        assert_eq!(
            described.text("Snapshot/SnapshotSource"),
            Some(state::SNAPSHOT_SOURCE_MANUAL),
            "case {case:?}: a snapshot a request named is a manual one, got {}",
            described.body
        );
        assert!(
            described.all("Snapshot/CacheClusterId").is_empty(),
            "case {case:?}: a snapshot of a GROUP names no single source cluster, got {}",
            described.body
        );
        assert_eq!(
            described.all("NodeSnapshot/CacheClusterId"),
            primaries.to_vec(),
            "case {case:?}: the captured nodes must be the shards' primaries, got {}",
            described.body
        );
        assert_eq!(
            described.all("NodeSnapshot/NodeGroupId").len(),
            primaries.len(),
            "case {case:?}: every captured node must name the shard it came from, got {}",
            described.body
        );
    }
}

#[tokio::test]
async fn a_final_snapshot_this_service_cannot_store_is_refused_and_takes_nothing() {
    let emulator = Emulator::new();
    available_group(
        &emulator,
        "held-rg",
        "held-rg",
        &[("ReplicasPerNodeGroup", "0")],
    )
    .await;
    // A name already taken by a stored snapshot.
    let taken = emulator
        .call(
            "CreateSnapshot",
            &params(&[
                ("SnapshotName", "already-here"),
                ("CacheClusterId", "held-rg-001"),
            ]),
        )
        .await;
    assert_eq!(
        taken.status, 200,
        "the snapshot that takes the name must be accepted, got {} {}",
        taken.status, taken.body
    );
    every_pending_change_due_in(&emulator, Duration::ZERO);
    poll_until(
        &emulator,
        REGION,
        "held-rg",
        "the member reaching available again",
        "DescribeCacheClusters",
        &params(&[("CacheClusterId", "held-rg-001")]),
        |reply| reply.text("CacheCluster/CacheClusterStatus") == Some(state::STATUS_AVAILABLE),
    )
    .await;

    // Each row: the final snapshot name the request carries, and the modelled refusal it
    // gets. The axis is WHICH rule about storing a snapshot the name breaks.
    let cases: [(&str, &str, u16, &str); 3] = [
        (
            "a name no snapshot may carry",
            "not a valid name!",
            400,
            "InvalidParameterValue",
        ),
        (
            "a name a stored snapshot already holds",
            "already-here",
            400,
            "SnapshotAlreadyExistsFault",
        ),
        (
            "the same name in another case, which resolves to the same snapshot",
            "ALREADY-HERE",
            400,
            "SnapshotAlreadyExistsFault",
        ),
    ];
    for (case, name, status, code) in cases {
        let refused = delete_group(&emulator, "held-rg", &[("FinalSnapshotIdentifier", name)]).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (status, code),
            "case {case:?}: expected {status} {code}, got {} {}",
            refused.status,
            refused.body
        );
        let still = describe_group(&emulator, "held-rg").await;
        assert_eq!(
            (still.status, still.text("ReplicationGroup/Status")),
            (200, Some(state::STATUS_AVAILABLE)),
            "case {case:?}: a refused delete must leave the group as it was, got {} {}",
            still.status,
            still.body
        );
    }
}

#[tokio::test]
async fn retaining_the_primary_keeps_the_cluster_and_the_data_it_held() {
    let emulator = Emulator::new();
    available_group(
        &emulator,
        "retain-rg",
        "retain-rg",
        &[("NumCacheClusters", "2")],
    )
    .await;

    // What the primary holds before the group goes, written over the endpoint the control
    // plane published for it.
    let described = describe_cluster(&emulator, "retain-rg-001").await;
    let (address, port) = endpoint_of(
        &described,
        "CacheNodes/CacheNode/Endpoint/Address",
        "CacheNodes/CacheNode/Endpoint/Port",
        "the retained primary",
    );
    let mut session = open("retain-rg", &address, &port).await;
    assert_eq!(
        command("retain-rg", &mut session, &["SET", "kept", "value"]).await,
        Resp::Status("OK".to_string()),
        "the primary must take the write"
    );

    let deleted = delete_group(&emulator, "retain-rg", &[("RetainPrimaryCluster", "true")]).await;
    assert_eq!(
        deleted.status, 200,
        "the delete must answer 200, got {} {}",
        deleted.status, deleted.body
    );
    assert_group_gone(&emulator, "retain-rg", "retain-rg").await;

    // "All of the read replicas are deleted, but the primary node is retained": the replica
    // is gone and the primary is a cluster of its own, belonging to no group.
    let replica = describe_cluster(&emulator, "retain-rg-002").await;
    assert_eq!(
        (replica.status, replica.error_code()),
        (404, "CacheClusterNotFound"),
        "the read replica must be deleted with the group, got {} {}",
        replica.status,
        replica.body
    );
    let retained = describe_cluster(&emulator, "retain-rg-001").await;
    assert_eq!(
        (
            retained.status,
            retained.text("CacheCluster/CacheClusterStatus")
        ),
        (200, Some(state::STATUS_AVAILABLE)),
        "the retained primary must still be a cluster of this account, got {} {}",
        retained.status,
        retained.body
    );
    assert!(
        retained.all("CacheCluster/ReplicationGroupId").is_empty(),
        "the retained primary must belong to no replication group, got {}",
        retained.body
    );

    // It is still reachable, at the address it publishes NOW, and it still holds what it
    // held: retaining the primary is what keeps a group's data.
    let (address, port) = endpoint_of(
        &retained,
        "CacheNodes/CacheNode/Endpoint/Address",
        "CacheNodes/CacheNode/Endpoint/Port",
        "the retained primary",
    );
    let mut session = open("retain-rg", &address, &port).await;
    assert_eq!(
        command("retain-rg", &mut session, &["GET", "kept"]).await,
        Resp::Bulk("value".to_string()),
        "the retained primary must still hold what it held"
    );
}

/// One retain row: the case, the group id, the pairs that create it, and the HTTP status
/// plus `awsQuery` error code (empty on a success) the delete that keeps its primary gets.
type RetainCase = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
    u16,
    &'static str,
);

#[tokio::test]
async fn retaining_the_primary_of_a_partitioned_group_is_refused() {
    let emulator = Emulator::new();
    // "If the replication group has only one primary, you can optionally delete only the
    // read replicas, while retaining the primary": a partitioned group has a primary per
    // shard, so there is no single primary to keep. The axis is the shard count, with the
    // single-shard row showing where the rule stops.
    let cases: [RetainCase; 3] = [
        (
            "two shards",
            "retain-two-rg",
            &[("NumNodeGroups", "2"), ("ReplicasPerNodeGroup", "1")],
            400,
            "InvalidParameterValue",
        ),
        (
            "four shards",
            "retain-four-rg",
            &[("NumNodeGroups", "4"), ("ReplicasPerNodeGroup", "0")],
            400,
            "InvalidParameterValue",
        ),
        (
            "one shard, which has one primary",
            "retain-one-rg",
            &[("ClusterMode", "enabled")],
            200,
            "",
        ),
    ];
    for (case, id, extra, status, code) in cases {
        available_group(&emulator, case, id, extra).await;
        let reply = delete_group(&emulator, id, &[("RetainPrimaryCluster", "true")]).await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (status, code),
            "case {case:?}: expected {status} {code}, got {} {}",
            reply.status,
            reply.body
        );
        if status == 200 {
            assert_group_gone(&emulator, case, id).await;
            continue;
        }
        assert_eq!(
            reply.error_message(),
            "Invalid value true for RetainPrimaryCluster: only the primary of a replication \
             group with one node group (shard) can be retained.",
            "case {case:?}: the refusal must state the rule, got {}",
            reply.body
        );
        let still = describe_group(&emulator, id).await;
        assert_eq!(
            (still.status, still.text("ReplicationGroup/Status")),
            (200, Some(state::STATUS_AVAILABLE)),
            "case {case:?}: a refused delete must leave the group as it was, got {} {}",
            still.status,
            still.body
        );
    }
}
