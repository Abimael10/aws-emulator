// SPDX-License-Identifier: CC0-1.0

//! What a snapshot records about the SOURCE it was taken of, for each source
//! `CreateSnapshot` accepts.
//!
//! The rule under test: a snapshot is a copy of the source the request names —
//! "the identifier of an existing cluster" or "the identifier of an existing
//! replication group" — as that source stood when the call was made, so it
//! records that source's identity and projects one `NodeSnapshot` per node it
//! captured; a copy of it is an INDEPENDENT record of the same captured content;
//! and the status change and the removal each of those records is owed are the
//! clock's, so the first report issued once one has fallen due answers the
//! settled state.
//!
//! The rows span what the rule generalises over: WHICH source was snapshotted (a
//! cache cluster, a replication group that is not partitioned, a partitioned
//! group) and so HOW MANY nodes the snapshot projects — one per node of a
//! cluster, and one per shard of a group, taken from that shard's primary
//! ("ElastiCache creates the snapshot from the primary node in the cluster,
//! rather than one of the replicas"). What each row expects of a group's
//! snapshot is read off the GROUP (`DescribeReplicationGroups`) rather than
//! written here, so the two reads cannot disagree about what the account runs.
//!
//! Nothing here sleeps and nothing polls: a change the clock owes is made due
//! through the store ([`crate::harness::release_pending_changes`]) and the call
//! that follows materialises it, so what this suite asserts does not depend on
//! how fast the host runs. Everything goes over the wire through the same router
//! the production emulator mounts.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{params, release_pending_changes, Emulator, Reply};

/// The source a row snapshots, with the form pairs it is created with.
#[derive(Debug, Clone, Copy)]
enum Source {
    /// A cache cluster: `CreateCacheCluster` with these pairs beside its id.
    CacheCluster(&'static [(&'static str, &'static str)]),
    /// A replication group: `CreateReplicationGroup` with these pairs beside its
    /// id and description.
    ReplicationGroup(&'static [(&'static str, &'static str)]),
}

/// One row: the source to snapshot, and the snapshot names used for it.
#[derive(Debug, Clone, Copy)]
struct SourceCase {
    case: &'static str,
    /// The id the source is created under, and so the one the snapshot records.
    source_id: &'static str,
    source: Source,
    /// The snapshot taken of the source, and the copy made of that snapshot.
    taken: &'static str,
    copied: &'static str,
}

/// Create the source `case` names and read it back settled, so a snapshot of it
/// is accepted.
async fn create_settled_source(emulator: &Emulator, case: &SourceCase) {
    let (action, request, read, status_path) = match case.source {
        Source::CacheCluster(extra) => {
            let mut pairs = params(&[("CacheClusterId", case.source_id)]);
            pairs.extend(params(extra));
            (
                "CreateCacheCluster",
                pairs,
                "DescribeCacheClusters",
                "CacheClusterStatus",
            )
        }
        Source::ReplicationGroup(extra) => {
            let mut pairs = params(&[
                ("ReplicationGroupId", case.source_id),
                ("ReplicationGroupDescription", "a snapshotted group"),
            ]);
            pairs.extend(params(extra));
            (
                "CreateReplicationGroup",
                pairs,
                "DescribeReplicationGroups",
                "Status",
            )
        }
    };

    let created = emulator.call(action, &request).await;
    assert_eq!(
        created.status, 200,
        "case {:?}: {action} must answer 200, got {} {}",
        case.case, created.status, created.body
    );

    // The create answers the transient status and the clock owes the source its
    // settled one; the row needs the settled source, so the owed change is made
    // due rather than waited out.
    release_pending_changes(emulator);
    let described = emulator.call(read, &source_request(case)).await;
    assert_eq!(
        described.text(status_path),
        Some(state::STATUS_AVAILABLE),
        "case {:?}: the source must read back {}, got {:?} in {}",
        case.case,
        state::STATUS_AVAILABLE,
        described.text(status_path),
        described.body
    );
}

/// The request that reads the row's source back.
fn source_request(case: &SourceCase) -> Vec<(String, String)> {
    match case.source {
        Source::CacheCluster(_) => params(&[("CacheClusterId", case.source_id)]),
        Source::ReplicationGroup(_) => params(&[("ReplicationGroupId", case.source_id)]),
    }
}

/// `DescribeSnapshots` for one snapshot by name, asserting the read itself
/// succeeded.
async fn snapshot(emulator: &Emulator, case: &str, name: &str) -> Reply {
    let reply = emulator
        .call("DescribeSnapshots", &params(&[("SnapshotName", name)]))
        .await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: DescribeSnapshots for {name} must answer 200, got {} {}",
        reply.status, reply.body
    );
    reply
}

/// What the source itself reports about the nodes a snapshot of it captures: the
/// shard ids one per shard (empty for a cache cluster, which has no shards), and
/// how many `NodeSnapshot`s the snapshot must hold.
async fn captured_nodes(emulator: &Emulator, case: &SourceCase) -> (Vec<String>, usize) {
    match case.source {
        Source::CacheCluster(_) => {
            let described = emulator
                .call(
                    "DescribeCacheClusters",
                    &params(&[
                        ("CacheClusterId", case.source_id),
                        ("ShowCacheNodeInfo", "true"),
                    ]),
                )
                .await;
            let nodes = described.all("CacheNode/CacheNodeId").len();
            assert!(
                nodes > 0,
                "case {:?}: the source cluster must report its nodes, got {}",
                case.case,
                described.body
            );
            (Vec::new(), nodes)
        }
        Source::ReplicationGroup(_) => {
            let described = emulator
                .call(
                    "DescribeReplicationGroups",
                    &params(&[("ReplicationGroupId", case.source_id)]),
                )
                .await;
            let shards: Vec<String> = described
                .all("NodeGroup/NodeGroupId")
                .iter()
                .map(|id| (*id).to_string())
                .collect();
            assert!(
                !shards.is_empty(),
                "case {:?}: the source group must report its shards, got {}",
                case.case,
                described.body
            );
            // One node per shard is captured: the shard's primary.
            (shards.clone(), shards.len())
        }
    }
}

/// Assert `reply` records the row's source and the nodes it captured.
fn assert_records_the_source(
    case: &SourceCase,
    phase: &str,
    reply: &Reply,
    shards: &[String],
    nodes: usize,
) {
    let (cluster_id, group_id) = match case.source {
        Source::CacheCluster(_) => (Some(case.source_id), None),
        Source::ReplicationGroup(_) => (None, Some(case.source_id)),
    };
    assert_eq!(
        reply.text("Snapshot/CacheClusterId"),
        cluster_id,
        "case {:?} ({phase}): expected CacheClusterId {cluster_id:?}, got {:?} in {}",
        case.case,
        reply.text("Snapshot/CacheClusterId"),
        reply.body
    );
    assert_eq!(
        reply.text("Snapshot/ReplicationGroupId"),
        group_id,
        "case {:?} ({phase}): expected ReplicationGroupId {group_id:?}, got {:?} in {}",
        case.case,
        reply.text("Snapshot/ReplicationGroupId"),
        reply.body
    );
    // "The number of node groups (shards) in this snapshot": the source group's,
    // and absent on a snapshot of a cache cluster, which has no shards.
    let expected_shards = (!shards.is_empty()).then(|| shards.len().to_string());
    assert_eq!(
        reply.text("Snapshot/NumNodeGroups").map(str::to_string),
        expected_shards,
        "case {:?} ({phase}): expected NumNodeGroups {expected_shards:?}, got {:?} in {}",
        case.case,
        reply.text("Snapshot/NumNodeGroups"),
        reply.body
    );
    assert_eq!(
        reply.all("NodeSnapshot/CacheNodeId").len(),
        nodes,
        "case {:?} ({phase}): expected one NodeSnapshot per captured node ({nodes}), got {:?} in {}",
        case.case,
        reply.all("NodeSnapshot/CacheNodeId"),
        reply.body
    );
    // Each node snapshot of a GROUP names the shard it was taken from
    // (`NodeSnapshot$NodeGroupId`); a cluster's node snapshots have no shard to
    // name.
    let reported: Vec<String> = reply
        .all("NodeSnapshot/NodeGroupId")
        .iter()
        .map(|id| (*id).to_string())
        .collect();
    assert_eq!(
        reported, shards,
        "case {:?} ({phase}): expected the source's shards {shards:?} on the node snapshots, got \
         {reported:?} in {}",
        case.case, reply.body
    );
    // Every captured node names the cluster it was taken from, and that cluster
    // is one the account runs.
    let captured = reply.all("NodeSnapshot/CacheClusterId");
    assert_eq!(
        captured.len(),
        nodes,
        "case {:?} ({phase}): every NodeSnapshot names its source cluster, got {captured:?} in {}",
        case.case,
        reply.body
    );
}

#[tokio::test]
async fn a_snapshot_records_the_source_it_was_taken_of_and_one_node_snapshot_per_captured_node() {
    // The axis: WHICH source was snapshotted, and so how many nodes the snapshot
    // projects — a single-node Redis OSS cluster, a replication group that is not
    // partitioned (whose replica is not captured: the primary of each shard is),
    // and a group partitioned across shards.
    let rows = [
        SourceCase {
            case: "a single-node cache cluster",
            source_id: "projected-cluster",
            source: Source::CacheCluster(&[
                ("Engine", "redis"),
                ("CacheNodeType", "cache.t4g.micro"),
                ("NumCacheNodes", "1"),
                ("SnapshotRetentionLimit", "0"),
            ]),
            taken: "cluster-snap",
            copied: "cluster-snap-copy",
        },
        SourceCase {
            case: "a replication group with a replica",
            source_id: "projected-group",
            source: Source::ReplicationGroup(&[
                ("Engine", "redis"),
                ("CacheNodeType", "cache.t3.micro"),
                ("NumCacheClusters", "2"),
            ]),
            taken: "group-snap",
            copied: "group-snap-copy",
        },
        SourceCase {
            case: "a replication group partitioned across three shards",
            source_id: "projected-shards",
            source: Source::ReplicationGroup(&[
                ("Engine", "valkey"),
                ("CacheNodeType", "cache.m7g.large"),
                ("NumNodeGroups", "3"),
                ("ReplicasPerNodeGroup", "1"),
            ]),
            taken: "shards-snap",
            copied: "shards-snap-copy",
        },
    ];

    for case in rows {
        let emulator = Emulator::new();
        create_settled_source(&emulator, &case).await;
        let (shards, nodes) = captured_nodes(&emulator, &case).await;

        // The call that takes the snapshot answers with the transient status the
        // service documents for a snapshot being made.
        let mut request = params(&[("SnapshotName", case.taken)]);
        request.extend(match case.source {
            Source::CacheCluster(_) => params(&[("CacheClusterId", case.source_id)]),
            Source::ReplicationGroup(_) => params(&[("ReplicationGroupId", case.source_id)]),
        });
        let taken = emulator.call("CreateSnapshot", &request).await;
        assert_eq!(
            taken.status, 200,
            "case {:?}: CreateSnapshot must answer 200, got {} {}",
            case.case, taken.status, taken.body
        );
        assert_eq!(
            taken.text("Snapshot/SnapshotStatus"),
            Some(state::STATUS_CREATING),
            "case {:?}: the call that takes a snapshot must answer {}, got {:?} in {}",
            case.case,
            state::STATUS_CREATING,
            taken.text("Snapshot/SnapshotStatus"),
            taken.body
        );
        assert_records_the_source(&case, "the create's answer", &taken, &shards, nodes);

        // The first report past the owed change answers the settled snapshot,
        // recording the same source.
        release_pending_changes(&emulator);
        let settled = snapshot(&emulator, case.case, case.taken).await;
        assert_eq!(
            settled.text("Snapshot/SnapshotStatus"),
            Some(state::STATUS_AVAILABLE),
            "case {:?}: the FIRST report past the owed change must report {}, got {:?} in {}",
            case.case,
            state::STATUS_AVAILABLE,
            settled.text("Snapshot/SnapshotStatus"),
            settled.body
        );
        assert_records_the_source(&case, "the settled snapshot", &settled, &shards, nodes);

        // A copy is a snapshot of its own, holding what the SOURCE snapshot
        // captured: the same source identity and the same captured nodes.
        let copied = emulator
            .call(
                "CopySnapshot",
                &params(&[
                    ("SourceSnapshotName", case.taken),
                    ("TargetSnapshotName", case.copied),
                ]),
            )
            .await;
        assert_eq!(
            copied.status, 200,
            "case {:?}: CopySnapshot must answer 200, got {} {}",
            case.case, copied.status, copied.body
        );
        assert_eq!(
            copied.text("Snapshot/SnapshotStatus"),
            Some(state::STATUS_CREATING),
            "case {:?}: the call that copies a snapshot must answer {}, got {:?} in {}",
            case.case,
            state::STATUS_CREATING,
            copied.text("Snapshot/SnapshotStatus"),
            copied.body
        );
        release_pending_changes(&emulator);
        let settled_copy = snapshot(&emulator, case.case, case.copied).await;
        assert_eq!(
            settled_copy.text("Snapshot/SnapshotStatus"),
            Some(state::STATUS_AVAILABLE),
            "case {:?}: the copy must read back {}, got {:?} in {}",
            case.case,
            state::STATUS_AVAILABLE,
            settled_copy.text("Snapshot/SnapshotStatus"),
            settled_copy.body
        );
        assert_records_the_source(&case, "the copy", &settled_copy, &shards, nodes);

        // Both records are held at once: a copy is added beside its source.
        let listed = emulator.call("DescribeSnapshots", &params(&[])).await;
        let mut names = listed.all("Snapshot/SnapshotName");
        names.sort_unstable();
        let mut expected = vec![case.taken, case.copied];
        expected.sort_unstable();
        assert_eq!(
            names, expected,
            "case {:?}: expected both {expected:?} to be held, got {}",
            case.case, listed.body
        );

        // The SOURCE snapshot goes away and the copy is untouched by it: two
        // records, one removal.
        let deleted = emulator
            .call("DeleteSnapshot", &params(&[("SnapshotName", case.taken)]))
            .await;
        assert_eq!(
            deleted.text("Snapshot/SnapshotStatus"),
            Some(state::STATUS_DELETING),
            "case {:?}: DeleteSnapshot must answer with the snapshot on its way out, got {:?} in {}",
            case.case,
            deleted.text("Snapshot/SnapshotStatus"),
            deleted.body
        );
        release_pending_changes(&emulator);
        let gone = snapshot(&emulator, case.case, case.taken).await;
        assert!(
            gone.all("Snapshot/SnapshotName").is_empty(),
            "case {:?}: the FIRST report past the owed removal must find no {}, got {}",
            case.case,
            case.taken,
            gone.body
        );
        let survivor = snapshot(&emulator, case.case, case.copied).await;
        assert_eq!(
            survivor.text("Snapshot/SnapshotStatus"),
            Some(state::STATUS_AVAILABLE),
            "case {:?}: the copy must survive its source's removal, still {}, got {:?} in {}",
            case.case,
            state::STATUS_AVAILABLE,
            survivor.text("Snapshot/SnapshotStatus"),
            survivor.body
        );
        assert_records_the_source(&case, "the survivor", &survivor, &shards, nodes);

        // And the copy's own way out, which leaves the store holding nothing.
        let deleted_copy = emulator
            .call("DeleteSnapshot", &params(&[("SnapshotName", case.copied)]))
            .await;
        assert_eq!(
            deleted_copy.text("Snapshot/SnapshotStatus"),
            Some(state::STATUS_DELETING),
            "case {:?}: DeleteSnapshot must answer with the copy on its way out, got {:?} in {}",
            case.case,
            deleted_copy.text("Snapshot/SnapshotStatus"),
            deleted_copy.body
        );
        release_pending_changes(&emulator);
        let empty = emulator.call("DescribeSnapshots", &params(&[])).await;
        assert!(
            empty.all("Snapshot/SnapshotName").is_empty(),
            "case {:?}: both removals must leave no snapshot held, got {}",
            case.case,
            empty.body
        );
    }
}

#[tokio::test]
async fn a_snapshot_of_a_replication_group_is_refused_by_whether_the_group_can_be_copied() {
    let emulator = Emulator::new();

    // A group that does not exist is not a source: `ReplicationGroupNotFoundFault`
    // is "the specified replication group does not exist", HTTP 404.
    let missing = emulator
        .call(
            "CreateSnapshot",
            &params(&[
                ("SnapshotName", "no-source-snap"),
                ("ReplicationGroupId", "no-such-group"),
            ]),
        )
        .await;
    assert_eq!(
        (missing.status, missing.error_code()),
        (404, "ReplicationGroupNotFoundFault"),
        "case a group the store does not hold: expected 404 ReplicationGroupNotFoundFault, got {} \
         {}",
        missing.status,
        missing.body
    );
    assert_eq!(
        missing.error_message(),
        state::replication_group_not_found_message("no-such-group"),
        "case a group the store does not hold: expected the not-found sentence, got {:?}",
        missing.error_message()
    );

    // A group the clock still owes `available` is not a source either:
    // `InvalidReplicationGroupStateFault` is "the requested replication group is
    // not in the available state", HTTP 400. The create's own answer reports that
    // transient status, so the gate is reached without holding anything.
    let created = emulator
        .call(
            "CreateReplicationGroup",
            &params(&[
                ("ReplicationGroupId", "gated-group"),
                ("ReplicationGroupDescription", "a group being created"),
                ("Engine", "redis"),
                ("CacheNodeType", "cache.t3.micro"),
            ]),
        )
        .await;
    assert_eq!(
        created.text("Status"),
        Some(state::STATUS_CREATING),
        "the create must answer with the transient status, got {}",
        created.body
    );
    let too_early = emulator
        .call(
            "CreateSnapshot",
            &params(&[
                ("SnapshotName", "too-early-snap"),
                ("ReplicationGroupId", "gated-group"),
            ]),
        )
        .await;
    assert_eq!(
        (too_early.status, too_early.error_code()),
        (400, "InvalidReplicationGroupState"),
        "case a group still being created: expected 400 InvalidReplicationGroupState, got {} {}",
        too_early.status,
        too_early.body
    );
    assert_eq!(
        too_early.error_message(),
        state::SNAPSHOT_SOURCE_GROUP_NOT_AVAILABLE_MESSAGE,
        "case a group still being created: expected the state sentence, got {:?}",
        too_early.error_message()
    );

    // Once the clock owes it `available`, the same call is accepted — and the
    // group answers `snapshotting` while the snapshot is being taken, which is
    // one of `ReplicationGroup$Status`'s own values.
    release_pending_changes(&emulator);
    let taken = emulator
        .call(
            "CreateSnapshot",
            &params(&[
                ("SnapshotName", "gated-snap"),
                ("ReplicationGroupId", "gated-group"),
            ]),
        )
        .await;
    assert_eq!(
        taken.status, 200,
        "case a settled group: expected HTTP 200, got {} {}",
        taken.status, taken.body
    );
    let described = emulator
        .call(
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", "gated-group")]),
        )
        .await;
    assert_eq!(
        described.text("Status"),
        Some(state::STATUS_SNAPSHOTTING),
        "case a settled group: the source must report {} while the snapshot is taken, got {:?} in \
         {}",
        state::STATUS_SNAPSHOTTING,
        described.text("Status"),
        described.body
    );
    // ... so a second snapshot of the same group is refused by the same gate,
    // on the status the group carries now.
    let second = emulator
        .call(
            "CreateSnapshot",
            &params(&[
                ("SnapshotName", "second-snap"),
                ("ReplicationGroupId", "gated-group"),
            ]),
        )
        .await;
    assert_eq!(
        (second.status, second.error_code()),
        (400, "InvalidReplicationGroupState"),
        "case a group already being snapshotted: expected 400 InvalidReplicationGroupState, got {} \
         {}",
        second.status,
        second.body
    );

    // Not one refusal stored a record: the store holds exactly the snapshot the
    // accepted call made.
    release_pending_changes(&emulator);
    let listed = emulator.call("DescribeSnapshots", &params(&[])).await;
    assert_eq!(
        listed.all("Snapshot/SnapshotName"),
        vec!["gated-snap"],
        "a refused snapshot must persist nothing, got {}",
        listed.body
    );
    // And the group settles back out of `snapshotting` on the clock.
    let settled = emulator
        .call(
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", "gated-group")]),
        )
        .await;
    assert_eq!(
        settled.text("Status"),
        Some(state::STATUS_AVAILABLE),
        "the source group must settle back to {}, got {:?} in {}",
        state::STATUS_AVAILABLE,
        settled.text("Status"),
        settled.body
    );
}
