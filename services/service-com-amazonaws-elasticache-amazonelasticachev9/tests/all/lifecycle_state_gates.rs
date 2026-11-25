// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the lifecycle GATES this service answers a mutation with
//! while the resource it names is busy, and for the work each accepted call
//! leaves behind.
//!
//! The rules under test:
//!
//! * a delete of a cache cluster is accepted in the states it names
//!   (`crate::state::DELETABLE_CLUSTER_STATES`) and refused in every other one
//!   with the sentence that states those states, the cluster the request named
//!   and the state that cluster is in; an accepted delete BEGINS the teardown,
//!   so the cluster is reported `deleting` until the clock removes it;
//! * a test of automatic failover on one shard is still being carried out for a
//!   span after the call that asked for it, and a second test of the SAME shard
//!   inside that span is refused with the operation's own
//!   `TestFailoverNotAvailableFault` and its fixed sentence, changing nothing,
//!   until the work is over and the retry is accepted;
//! * a data migration into a replication group is open from the start that
//!   recorded it, and its data sync is in flight only once the service has
//!   established it: a cutover asked for before then is refused with the
//!   sentence that says to retry, while a group whose sync is not running has a
//!   connection a test can still be run against.
//!
//! The tests are tables: each row is one input along the axis the rule
//! generalises over (which state the cluster is in, which shard a failover
//! names, how the group is partitioned), so no single recorded request is what
//! makes them pass. Nothing waits a fixed time for work the clock owes: a suite
//! either looks again until the condition holds, or holds the due instant out of
//! reach.

use std::time::{Duration, SystemTime};

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::cache_cluster_lifecycle::{create_cluster, describe_cluster};
use crate::harness::{
    hold_pending_changes, params, poll_until, release_pending_changes, Emulator, Reply, REGION,
    SETTLE_DEADLINE,
};

/// Hold the stored cluster `id` in `status`, with nothing the clock owes it
/// left outstanding.
///
/// The states a delete is accepted in beside `available` are ones no operation
/// of this service puts a cluster in (a failed provisioning, a network a cluster
/// no longer fits, a restore that did not finish, a recovery), so a row about
/// them seeds the status on the store the handlers read and then asserts over
/// the wire that the answer is computed FROM it.
fn hold_cluster_status(emulator: &Emulator, id: &str, status: &str) {
    emulator.with_state(|store| {
        let cluster = store
            .cache_clusters
            .get_mut(id)
            .unwrap_or_else(|| panic!("case {id:?}: the cluster just created must be stored"));
        cluster.status = status.to_string();
        cluster.pending_status = None;
        cluster.set_node_status(status);
    });
}

/// `DeleteCacheCluster` for the cluster `id`.
async fn delete_cluster(emulator: &Emulator, id: &str) -> Reply {
    emulator
        .call("DeleteCacheCluster", &params(&[("CacheClusterId", id)]))
        .await
}

#[tokio::test]
async fn a_delete_is_refused_in_every_state_it_is_not_accepted_in() {
    // The axis: the state the cluster is in. The rows span the states a delete
    // IS accepted in and the ones it is not, and the recorded `creating` case is
    // one row. The accepted list is `CacheClusterStatus`'s own values, as the
    // refusal states them (`state::DELETABLE_CLUSTER_STATES`).
    let rows: [(&str, &str, bool); 10] = [
        ("available", state::STATUS_AVAILABLE, true),
        ("failed", state::STATUS_FAILED, true),
        (
            "incompatible parameters",
            state::STATUS_INCOMPATIBLE_PARAMETERS,
            true,
        ),
        (
            "incompatible network",
            state::STATUS_INCOMPATIBLE_NETWORK,
            true,
        ),
        ("restore failed", state::STATUS_RESTORE_FAILED, true),
        ("recovering", state::STATUS_RECOVERING, true),
        ("still creating", state::STATUS_CREATING, false),
        ("modifying", state::STATUS_MODIFYING, false),
        ("snapshotting", state::STATUS_SNAPSHOTTING, false),
        ("rebooting its nodes", state::STATUS_REBOOTING_NODES, false),
    ];

    for (index, (case, status, deletable)) in rows.into_iter().enumerate() {
        let emulator = Emulator::new();
        let id = format!("gate-{index}");
        let created = create_cluster(&emulator, &id, &[]).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateCacheCluster must answer 200, got {} {}",
            created.status, created.body
        );
        hold_cluster_status(&emulator, &id, status);

        let answered = delete_cluster(&emulator, &id).await;
        if deletable {
            assert_eq!(
                answered.status, 200,
                "case {case:?}: a delete is accepted in {status:?}, got {} {}",
                answered.status, answered.body
            );
            assert_eq!(
                answered.text("CacheCluster/CacheClusterStatus"),
                Some(state::STATUS_DELETING),
                "case {case:?}: the answer must report the cluster on its way out, got {:?}",
                answered.text("CacheCluster/CacheClusterStatus")
            );
            // The teardown is only BEGUN by the call: the cluster is reported
            // `deleting` while it runs, and the removal is what the clock owes
            // — reached by looking again rather than by waiting a fixed time.
            let tearing_down = describe_cluster(&emulator, &id).await;
            assert_eq!(
                tearing_down.text("CacheClusterStatus"),
                Some(state::STATUS_DELETING),
                "case {case:?}: a read while the teardown runs must report it, got {} {}",
                tearing_down.status,
                tearing_down.body
            );
            let gone = poll_until(
                &emulator,
                REGION,
                case,
                &format!("the cluster {id} being removed by the clock"),
                "DescribeCacheClusters",
                &params(&[("CacheClusterId", &id)]),
                |reply| reply.status == 404,
            )
            .await;
            assert_eq!(
                (gone.status, gone.error_code()),
                (404, "CacheClusterNotFound"),
                "case {case:?}: once removed, a read must not find the cluster, got {} {}",
                gone.status,
                gone.body
            );
            continue;
        }

        // The sentence states the rule — the states a delete IS accepted in —
        // and then the cluster the request named and the state it is in. Pinned
        // here as text, so the suite checks the WORDING rather than the
        // constant the handler reads.
        let expected = format!(
            "Can only delete cache clusters with state in: available, failed, \
             incompatible-parameters, incompatible-network, restore-failed, recovering. Cluster \
             {id} has state: {status}."
        );
        assert_eq!(
            (answered.status, answered.error_code()),
            (400, "InvalidCacheClusterState"),
            "case {case:?}: expected 400 InvalidCacheClusterState, got {} {}",
            answered.status,
            answered.body
        );
        assert_eq!(
            answered.error_message(),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            answered.error_message()
        );
        // A refusal is not a change: the cluster is still there, in the state
        // it was refused for.
        let described = describe_cluster(&emulator, &id).await;
        assert_eq!(
            described.text("CacheClusterStatus"),
            Some(status),
            "case {case:?}: the refused cluster must be left as it was, got {:?}",
            described.text("CacheClusterStatus")
        );
    }
}

/// Create one replication group with `shards` shards, automatic failover on and
/// one replica per shard, and look again until the clock has settled it.
async fn create_failover_group(emulator: &Emulator, case: &str, id: &str, shards: i32) {
    let created = emulator
        .call(
            "CreateReplicationGroup",
            &params(&[
                ("ReplicationGroupId", id),
                ("ReplicationGroupDescription", "a state-gate target"),
                ("Engine", state::DEFAULT_ENGINE),
                ("CacheNodeType", "cache.m7g.large"),
                ("NumNodeGroups", &shards.to_string()),
                ("ReplicasPerNodeGroup", "1"),
                ("AutomaticFailoverEnabled", "true"),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateReplicationGroup must answer 200, got {} {}",
        created.status, created.body
    );
    poll_group_available(emulator, case, id).await;
}

/// Read the replication group `id` back until it reports `available`.
async fn poll_group_available(emulator: &Emulator, case: &str, id: &str) -> Reply {
    poll_until(
        emulator,
        REGION,
        case,
        &format!("the replication group {id} settling at available"),
        "DescribeReplicationGroups",
        &params(&[("ReplicationGroupId", id)]),
        |reply| reply.text("Status") == Some(state::STATUS_AVAILABLE),
    )
    .await
}

/// `TestFailover` on the shard `node_group_id` of the group `id`.
async fn test_failover(emulator: &Emulator, id: &str, node_group_id: &str) -> Reply {
    emulator
        .call(
            "TestFailover",
            &params(&[
                ("ReplicationGroupId", id),
                ("NodeGroupId", node_group_id),
            ]),
        )
        .await
}

/// The member cluster ids of the group `id`, in the order it reports them.
async fn members(emulator: &Emulator, case: &str, id: &str) -> Vec<String> {
    let described = emulator
        .call(
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", id)]),
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: DescribeReplicationGroups must answer 200, got {} {}",
        described.status, described.body
    );
    described
        .all("NodeGroupMember/CacheClusterId")
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn a_second_test_failover_on_the_same_shard_is_refused_while_the_first_is_carried_out() {
    // The axis: how the group is partitioned and WHICH shard the test names, so
    // the conflict is not keyed to a one-shard group or to the first shard. The
    // recorded two-member, one-shard case is the first row.
    let rows: [(&str, &str, i32, &str); 3] = [
        ("one shard", "gated-one", 1, "0001"),
        ("two shards, the first", "gated-first", 2, "0001"),
        ("three shards, the last", "gated-last", 3, "0003"),
    ];

    for (case, id, shards, node_group_id) in rows {
        let emulator = Emulator::new();
        create_failover_group(&emulator, case, id, shards).await;

        let accepted = test_failover(&emulator, id, node_group_id).await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: the first TestFailover must be accepted, got {} {}",
            accepted.status, accepted.body
        );
        let after_first = members(&emulator, case, id).await;

        // The work the accepted call started is still being carried out, so the
        // operation's own fault refuses a second test of that shard — with the
        // fixed sentence, pinned here as text — and the refusal changes nothing.
        let refused = test_failover(&emulator, id, node_group_id).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "TestFailoverNotAvailableFault"),
            "case {case:?}: expected 400 TestFailoverNotAvailableFault, got {} {}",
            refused.status,
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            "Test failover already in progress",
            "case {case:?}: expected the fixed in-progress sentence, got {:?}",
            refused.error_message()
        );
        assert_eq!(
            members(&emulator, case, id).await,
            after_first,
            "case {case:?}: a refused test must move no member's role"
        );

        // The conflict is waited out by retrying the call itself: it is answered
        // for as long as the work runs, and never with the group's own state
        // refusal, until the retry is accepted. The loop looks again until that
        // condition holds rather than for a fixed time.
        let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
        loop {
            let retried = test_failover(&emulator, id, node_group_id).await;
            if retried.status == 200 {
                break;
            }
            assert_eq!(
                (retried.status, retried.error_code()),
                (400, "TestFailoverNotAvailableFault"),
                "case {case:?}: a retry inside the work must carry the operation's own fault, \
                 got {} {}",
                retried.status,
                retried.body
            );
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: the test failover never stopped being refused"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        // The accepted retry failed the shard over again, so the roles moved on
        // from where the refused calls left them.
        let after_retry = members(&emulator, case, id).await;
        assert_ne!(
            after_retry, after_first,
            "case {case:?}: the accepted retry must move the shard's primary role again"
        );
    }
}

#[tokio::test]
async fn a_test_failover_on_another_shard_is_refused_while_the_group_applies_one() {
    // "If calling this operation multiple times on different shards in the same
    // … replication group, the first node replacement must complete before a
    // subsequent call can be made": the group is not available while it applies
    // one, which is the state refusal rather than the shard's own conflict.
    let emulator = Emulator::new();
    create_failover_group(&emulator, "two shards", "cross-shard", 2).await;
    let accepted = test_failover(&emulator, "cross-shard", "0001").await;
    assert_eq!(
        accepted.status, 200,
        "the first TestFailover must be accepted, got {} {}",
        accepted.status, accepted.body
    );
    // The change the clock owes the group is held out of reach, so the row is
    // about what the service answers WHILE the replacement runs rather than
    // about how fast the host is.
    hold_pending_changes(&emulator);
    let refused = test_failover(&emulator, "cross-shard", "0002").await;
    assert_eq!(
        (refused.status, refused.error_code()),
        (400, "InvalidReplicationGroupState"),
        "expected the group's state refusal for another shard, got {} {}",
        refused.status,
        refused.body
    );
}

/// The `CustomerNodeEndpoint` form pairs for one source endpoint.
fn source_endpoint(address: &str, port: i32) -> Vec<(String, String)> {
    params(&[
        ("CustomerNodeEndpointList.member.1.Address", address),
        ("CustomerNodeEndpointList.member.1.Port", &port.to_string()),
    ])
}

/// `StartMigration` into the replication group `id` from one source endpoint.
async fn start_migration(emulator: &Emulator, id: &str) -> Reply {
    let mut pairs = params(&[("ReplicationGroupId", id)]);
    pairs.extend(source_endpoint("198.51.100.10", 6379));
    emulator.call("StartMigration", &pairs).await
}

#[tokio::test]
async fn a_cutover_is_served_only_once_the_data_sync_is_established() {
    // The axis: how the group is partitioned — the rule is about the sync's
    // lifetime, which the shape of the group does not change.
    let rows: [(&str, &str, i32); 2] = [
        ("one shard", "sync-one", 1),
        ("two shards", "sync-two", 2),
    ];

    for (case, id, shards) in rows {
        let emulator = Emulator::new();
        create_failover_group(&emulator, case, id, shards).await;

        let started = start_migration(&emulator, id).await;
        assert_eq!(
            started.status, 200,
            "case {case:?}: StartMigration must answer 200, got {} {}",
            started.status, started.body
        );
        assert_eq!(
            started.text("Status"),
            Some(state::STATUS_MODIFYING),
            "case {case:?}: the group must answer with the work the start left, got {:?}",
            started.text("Status")
        );
        // While the group applies the start, a second one is the state refusal
        // worded for the statuses a start is accepted in.
        let during = start_migration(&emulator, id).await;
        assert_eq!(
            (during.status, during.error_code()),
            (400, "InvalidReplicationGroupState"),
            "case {case:?}: a start while one applies must be refused, got {} {}",
            during.status,
            during.body
        );
        assert_eq!(
            during.error_message(),
            "Replication group must be in available or migrating status to start migration.",
            "case {case:?}: expected the start-state sentence, got {:?}",
            during.error_message()
        );

        // The sync the start opened is work of its own, so it is held out of
        // reach here: what the rows below assert is what the service answers
        // WHILE it is being established, rather than how fast the host is.
        hold_pending_changes(&emulator);
        for force in ["false", "true"] {
            let too_early = emulator
                .call(
                    "CompleteMigration",
                    &params(&[("ReplicationGroupId", id), ("Force", force)]),
                )
                .await;
            assert_eq!(
                (too_early.status, too_early.error_code()),
                (400, "InvalidParameterValue"),
                "case {case:?}: a cutover before the sync is established must be refused with \
                 Force {force}, got {} {}",
                too_early.status,
                too_early.body
            );
            assert_eq!(
                too_early.error_message(),
                state::MIGRATION_NOT_MIGRATING_MESSAGE,
                "case {case:?}: expected the migrating-state sentence, got {:?}",
                too_early.error_message()
            );
        }
        // A connection to a source is testable while the sync is not running
        // yet: the test opens nothing and changes nothing. The group's OWN
        // change is made due for that — a test needs a provisioned, idle group
        // — while the sync it opened stays held out of reach.
        emulator.with_state(|store| {
            let group = store
                .replication_groups
                .get_mut(id)
                .expect("the group just created must be stored");
            if let Some(pending) = group.pending_status.as_mut() {
                pending.due = SystemTime::now();
            }
        });
        let mut tested = params(&[("ReplicationGroupId", id)]);
        tested.extend(source_endpoint("198.51.100.11", 6380));
        let test = emulator.call("TestMigration", &tested).await;
        assert_eq!(
            test.status, 200,
            "case {case:?}: a group whose sync is not running must take a test, got {} {}",
            test.status, test.body
        );

        // Once the service has established that sync, the cutover a client
        // retries is served — which is what the refusal above said to do.
        release_pending_changes(&emulator);
        let completed = poll_until(
            &emulator,
            REGION,
            case,
            "the cutover being served once the data sync is established",
            "CompleteMigration",
            &params(&[("ReplicationGroupId", id)]),
            |reply| reply.status == 200,
        )
        .await;
        assert_eq!(
            completed.text("Status"),
            Some(state::STATUS_MODIFYING),
            "case {case:?}: the cutover is work the service carries out after the call, got {:?}",
            completed.text("Status")
        );
        // The cutover CLOSED the migration: there is nothing left to complete,
        // and the group takes a new start.
        let again = emulator
            .call(
                "CompleteMigration",
                &params(&[("ReplicationGroupId", id)]),
            )
            .await;
        assert_eq!(
            (again.status, again.error_code()),
            (400, "InvalidParameterValue"),
            "case {case:?}: a second cutover must be refused, got {} {}",
            again.status,
            again.body
        );
        poll_group_available(&emulator, case, id).await;
        let restarted = start_migration(&emulator, id).await;
        assert_eq!(
            restarted.status, 200,
            "case {case:?}: a group whose migration was completed must take another, got {} {}",
            restarted.status, restarted.body
        );
    }
}

#[tokio::test]
async fn a_group_whose_sync_is_running_has_no_connection_left_to_test() {
    // The other side of the test-migration gate: once the sync IS in flight,
    // the connection this call would test is the one that migration reads
    // through, which is the modelled already-under-migration fault.
    let emulator = Emulator::new();
    create_failover_group(&emulator, "the running sync", "running-sync", 1).await;
    let started = start_migration(&emulator, "running-sync").await;
    assert_eq!(
        started.status, 200,
        "StartMigration must answer 200, got {} {}",
        started.status, started.body
    );
    // The sync is put in flight through the store the handlers read — established
    // now and given up far from now — so the row meets a RUNNING sync rather than
    // racing the spans the service establishes and gives it up over.
    emulator.with_state(|store| {
        let group = store
            .replication_groups
            .get_mut("running-sync")
            .expect("the group just created must be stored");
        if let Some(pending) = group.pending_status.as_mut() {
            pending.due = SystemTime::now();
        }
        let migration = group
            .migration
            .as_mut()
            .expect("the accepted start must have recorded the migration");
        migration.established_due = Some(SystemTime::now());
    });
    poll_group_available(&emulator, "the running sync", "running-sync").await;
    let mut tested = params(&[("ReplicationGroupId", "running-sync")]);
    tested.extend(source_endpoint("198.51.100.11", 6380));
    let refused = emulator.call("TestMigration", &tested).await;
    assert_eq!(
        (refused.status, refused.error_code()),
        (400, "ReplicationGroupAlreadyUnderMigrationFault"),
        "expected the already-under-migration fault, got {} {}",
        refused.status,
        refused.body
    );
    assert_eq!(
        refused.error_message(),
        "Replication group running-sync is already under migration.",
        "the fault must name the group it refused, got {:?}",
        refused.error_message()
    );
}

/// The seeded statuses above are the ones `CacheClusterStatus` publishes, and
/// the list the delete refusal names is the one the gate accepts: a status in
/// the list that the gate refused, or one outside it that it accepted, would be
/// a defect this row fails on.
#[test]
fn the_states_a_delete_is_accepted_in_are_the_ones_its_refusal_names() {
    assert_eq!(
        state::DELETABLE_CLUSTER_STATES.join(", "),
        "available, failed, incompatible-parameters, incompatible-network, restore-failed, \
         recovering",
        "the delete refusal names the states `CacheClusterStatus` publishes for a cluster \
         nothing is being done to, got {:?}",
        state::DELETABLE_CLUSTER_STATES
    );
    let message = state::delete_state_refusal("some-cluster", state::STATUS_CREATING);
    assert_eq!(
        message,
        "Can only delete cache clusters with state in: available, failed, \
         incompatible-parameters, incompatible-network, restore-failed, recovering. Cluster \
         some-cluster has state: creating.",
        "the refusal must state the rule, the cluster and the state, got {message:?}"
    );
    // The in-progress conflict of a test failover carries no request or state
    // value at all, and no trailing stop.
    assert_eq!(
        state::TEST_FAILOVER_IN_PROGRESS_MESSAGE, "Test failover already in progress",
        "the test-failover conflict is a fixed sentence, got {:?}",
        state::TEST_FAILOVER_IN_PROGRESS_MESSAGE
    );
    // The span a shard is held conflicting for is never zero: a caller acting
    // straight after the accepted call has to meet the refusal.
    assert!(
        state::TEST_FAILOVER_WORK > Duration::ZERO,
        "the test-failover work span must be non-zero, got {:?}",
        state::TEST_FAILOVER_WORK
    );
}
