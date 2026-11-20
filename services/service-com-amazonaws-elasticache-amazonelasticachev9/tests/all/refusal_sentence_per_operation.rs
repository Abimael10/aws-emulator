// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SENTENCE a refusal of this service states.
//!
//! The rule under test: a refusal reports the sentence ElastiCache states for
//! that error class and that operation — the fixed text as the service words it,
//! with the variable parts taken from the request and the store — rather than a
//! sentence shared with another gate or a paraphrase of the rule it broke. Three
//! classes are pinned here, each over a table of inputs:
//!
//! * a node group (shard) the replication group does not hold is reported by the
//!   modelled `NodeGroupNotFoundFault` as `Node group <node-group-id> was not
//!   found for replication group <replication-group-id>`, which carries no
//!   trailing stop and names both the shard asked about and the group it was
//!   asked about in;
//! * a shard-configuration change refused on the group's STATUS reports the
//!   reshard's own sentence (`Could not reshard replication group. Please try
//!   again later.`) under `InvalidReplicationGroupState`, so the loser of a race
//!   between two reshards learns the reshard is what could not be carried out,
//!   while a replica-count change refused on the same group keeps the general
//!   modification sentence;
//! * a serverless cache's `SnapshotRetentionLimit` outside the published range
//!   reports `Invalid snapshot retention limit: <n>. Snapshot retention limit
//!   must be between 0 and 35.` under `InvalidParameterValue`, on the create and
//!   on the modify alike, with the rejected value from the request.
//!
//! The expected sentences are written out here rather than read from the service,
//! so a change to the wording this service answers with fails these tests.
//!
//! Every assertion goes over the wire through the same router the production
//! emulator mounts. The rows that need a group in a transient status hold it
//! there through [`crate::harness::Emulator::with_state`] — the same store the
//! handlers read — so a row never races the transition span; the rows that need a
//! settled resource LOOK AGAIN until it reports settled.

use std::time::{Duration, SystemTime};

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{assert_refusal, params, poll_until, Emulator, Reply, REGION};

/// A span far longer than any transition this service publishes, for a record a
/// row holds in a non-terminal status on purpose.
const NEVER_SETTLES: Duration = Duration::from_secs(60 * 60);

/// AWS's `NodeGroupNotFoundFault` sentence: `Node group <node-group-id> was not
/// found for replication group <replication-group-id>` — no trailing stop.
fn node_group_not_found(node_group_id: &str, replication_group_id: &str) -> String {
    format!("Node group {node_group_id} was not found for replication group {replication_group_id}")
}

/// AWS's state sentence for a shard-configuration change it could not carry out:
/// wholly fixed text, with neither the group nor its status in it.
const RESHARD_SENTENCE: &str = "Could not reshard replication group. Please try again later.";

/// AWS's sentence for a `SnapshotRetentionLimit` outside the range a serverless
/// cache publishes, whose one variable part is the rejected value.
fn retention_limit_sentence(limit: &str) -> String {
    format!(
        "Invalid snapshot retention limit: {limit}. Snapshot retention limit must be between 0 \
         and 35."
    )
}

/// Create one replication group over the wire, with the pairs a row adds to the
/// required ones.
async fn create_group(emulator: &Emulator, case: &str, id: &str, extra: &[(&str, &str)]) {
    let mut pairs = params(&[
        ("ReplicationGroupId", id),
        ("ReplicationGroupDescription", "a group for a refusal row"),
        ("Engine", state::DEFAULT_ENGINE),
        ("CacheNodeType", "cache.m7g.large"),
    ]);
    pairs.extend(params(extra));
    let created = emulator.call("CreateReplicationGroup", &pairs).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the group a row needs must be creatable, got {} {}",
        created.status, created.body
    );
}

/// Read one replication group back until it reports `available`: a report answers
/// from the record as it stands, so the status the clock owes is reached by
/// looking again rather than by waiting a fixed time.
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

/// Create one replication group and let the clock settle it at `available`.
async fn settled_group(emulator: &Emulator, case: &str, id: &str, extra: &[(&str, &str)]) {
    create_group(emulator, case, id, extra).await;
    poll_group_available(emulator, case, id).await;
}

/// Hold the stored replication group `id` in `status`, with the status the clock
/// owes it due far beyond any run of this suite.
///
/// A row that needs a group in a transient status asserts about the GATE, not
/// about the span: holding the record is what keeps it from racing the clock.
fn hold_group_status(emulator: &Emulator, id: &str, status: &str) {
    emulator.with_state(|store| {
        let group = store
            .replication_groups
            .get_mut(id)
            .expect("the group a row created must be stored");
        group.status = status.to_string();
        group.pending_status = Some(state::PendingTransition {
            due: SystemTime::now() + NEVER_SETTLES,
            status: state::STATUS_AVAILABLE.to_string(),
        });
    });
}

/// `TestFailover` on one shard of one group.
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

/// `ModifyReplicationGroupShardConfiguration` with the pairs a row sends.
async fn reshard(emulator: &Emulator, id: &str, extra: &[(&str, &str)]) -> Reply {
    let mut pairs = params(&[("ReplicationGroupId", id), ("ApplyImmediately", "true")]);
    pairs.extend(params(extra));
    emulator
        .call("ModifyReplicationGroupShardConfiguration", &pairs)
        .await
}

#[tokio::test]
async fn an_unknown_node_group_is_refused_with_the_node_group_not_found_sentence() {
    // The axis: the topology the group has, and the FORM of the `NodeGroupId` the
    // request asks about — padded, unpadded, the zero id, and one far past the
    // shards the group holds. `AllowedNodeGroupId` is 1 to 4 digits
    // (`smithy_query(query="shape",shape="AllowedNodeGroupId")`), so every row
    // asks about a value the model admits.
    type Row = (
        &'static str,
        &'static [(&'static str, &'static str)],
        &'static str,
    );
    let rows: &[Row] = &[
        (
            "one shard, an id far past it",
            &[("NumCacheClusters", "2"), ("AutomaticFailoverEnabled", "true")],
            "9999",
        ),
        (
            "one shard, the second asked for unpadded",
            &[("NumCacheClusters", "2"), ("AutomaticFailoverEnabled", "true")],
            "2",
        ),
        (
            "one shard, the zero id",
            &[("NumCacheClusters", "2"), ("AutomaticFailoverEnabled", "true")],
            "0000",
        ),
        (
            "three shards, the fourth",
            &[("NumNodeGroups", "3"), ("ReplicasPerNodeGroup", "1")],
            "0004",
        ),
        (
            "three shards, a single digit past them",
            &[("NumNodeGroups", "3"), ("ReplicasPerNodeGroup", "1")],
            "7",
        ),
    ];

    for (index, (case, extra, node_group_id)) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        let id = format!("shard-sentence-{index}");
        settled_group(&emulator, case, &id, extra).await;

        let refused = test_failover(&emulator, &id, node_group_id).await;
        assert_refusal(
            case,
            &refused,
            404,
            "NodeGroupNotFoundFault",
            &node_group_not_found(node_group_id, &id),
        );
        // The service states this one without a trailing stop, so a client
        // matching it byte for byte must not be handed one.
        assert!(
            !refused.error_message().ends_with('.'),
            "case {case:?}: the sentence carries no trailing stop, got {:?}",
            refused.error_message()
        );
    }
}

#[tokio::test]
async fn an_unknown_node_group_is_reported_before_the_groups_own_status() {
    // The shard the request names is part of the request: a group that is not
    // `available` is still answered about the SHARD, so a caller is never told
    // the group is busy about a shard that does not exist.
    let emulator = Emulator::new();
    settled_group(
        &emulator,
        "a held group",
        "shard-before-status",
        &[("NumNodeGroups", "2"), ("ReplicasPerNodeGroup", "1")],
    )
    .await;

    for status in [
        state::STATUS_CREATING,
        state::STATUS_MODIFYING,
        state::STATUS_DELETING,
    ] {
        hold_group_status(&emulator, "shard-before-status", status);
        let refused = test_failover(&emulator, "shard-before-status", "0009").await;
        assert_refusal(
            status,
            &refused,
            404,
            "NodeGroupNotFoundFault",
            &node_group_not_found("0009", "shard-before-status"),
        );
    }
}

#[tokio::test]
async fn a_reshard_refused_on_the_groups_status_states_the_reshard_sentence() {
    // The axis: the status the group is held in, and the direction of the change
    // the request asks for — out, and in with each side of the selection.
    type Row = (
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
    );
    let rows: &[Row] = &[
        (
            "still creating, a scale-out",
            state::STATUS_CREATING,
            &[("NodeGroupCount", "3")],
        ),
        (
            "modifying from an earlier change, a wider scale-out",
            state::STATUS_MODIFYING,
            &[("NodeGroupCount", "4")],
        ),
        (
            "modifying, a scale-in that selects what it keeps",
            state::STATUS_MODIFYING,
            &[
                ("NodeGroupCount", "1"),
                ("NodeGroupsToRetain.NodeGroupToRetain.1", "0001"),
            ],
        ),
        (
            "modifying, a scale-in that selects what it drops",
            state::STATUS_MODIFYING,
            &[
                ("NodeGroupCount", "1"),
                ("NodeGroupsToRemove.NodeGroupToRemove.1", "0002"),
            ],
        ),
        (
            "being snapshotted, a scale-out",
            state::STATUS_SNAPSHOTTING,
            &[("NodeGroupCount", "3")],
        ),
        (
            "on its way out, a scale-out",
            state::STATUS_DELETING,
            &[("NodeGroupCount", "3")],
        ),
    ];

    for (index, (case, status, extra)) in rows.iter().enumerate() {
        let emulator = Emulator::new();
        let id = format!("reshard-sentence-{index}");
        settled_group(
            &emulator,
            case,
            &id,
            &[("NumNodeGroups", "2"), ("ReplicasPerNodeGroup", "1")],
        )
        .await;
        hold_group_status(&emulator, &id, status);

        let refused = reshard(&emulator, &id, extra).await;
        assert_refusal(
            case,
            &refused,
            400,
            "InvalidReplicationGroupState",
            RESHARD_SENTENCE,
        );

        // A replica-count change refused on the very same group keeps the
        // general modification sentence: the reshard sentence belongs to the
        // operation that could not reshard, not to the state fault as such.
        let other = emulator
            .call(
                "IncreaseReplicaCount",
                &params(&[
                    ("ReplicationGroupId", &id),
                    ("NewReplicaCount", "2"),
                    ("ApplyImmediately", "true"),
                ]),
            )
            .await;
        assert_refusal(
            case,
            &other,
            400,
            "InvalidReplicationGroupState",
            "Replication group must be in available state to modify.",
        );
    }
}

#[tokio::test]
async fn of_two_reshards_released_together_the_loser_states_the_reshard_sentence() {
    // The recorded instance of the rule: two reshards of one group, released
    // together, each asking for a shard configuration that would be served on
    // its own. Exactly one is carried out and the other is told the reshard
    // could not be.
    type Row = (
        &'static str,
        &'static [(&'static str, &'static str)],
        &'static [(&'static str, &'static str)],
    );
    let rows: &[Row] = &[
        (
            "a scale-out against a wider scale-out",
            &[("NodeGroupCount", "3")],
            &[("NodeGroupCount", "4")],
        ),
        (
            "a scale-out against a scale-in",
            &[("NodeGroupCount", "5")],
            &[
                ("NodeGroupCount", "1"),
                ("NodeGroupsToRetain.NodeGroupToRetain.1", "0001"),
            ],
        ),
        (
            "two scale-ins selecting opposite sides",
            &[
                ("NodeGroupCount", "1"),
                ("NodeGroupsToRetain.NodeGroupToRetain.1", "0001"),
            ],
            &[
                ("NodeGroupCount", "1"),
                ("NodeGroupsToRemove.NodeGroupToRemove.1", "0001"),
            ],
        ),
    ];

    for (case, first_extra, second_extra) in rows.iter() {
        let emulator = Emulator::new();
        settled_group(
            &emulator,
            case,
            "raced-group",
            &[("NumNodeGroups", "2"), ("ReplicasPerNodeGroup", "1")],
        )
        .await;

        let (first, second) = ::tokio::join!(
            reshard(&emulator, "raced-group", first_extra),
            reshard(&emulator, "raced-group", second_extra),
        );
        let accepted = [&first, &second]
            .into_iter()
            .filter(|reply| reply.status == 200)
            .count();
        assert_eq!(
            accepted, 1,
            "case {case:?}: exactly one of two reshards released together wins, got {} {} and {} {}",
            first.status, first.body, second.status, second.body
        );
        let loser = if first.status == 200 { &second } else { &first };
        assert_refusal(
            case,
            loser,
            400,
            "InvalidReplicationGroupState",
            RESHARD_SENTENCE,
        );
    }
}

#[tokio::test]
async fn a_serverless_retention_limit_outside_the_range_states_the_limit_sentence() {
    let emulator = Emulator::new();
    // One cache the modify rows run against, settled before they do.
    let created = emulator
        .call(
            "CreateServerlessCache",
            &params(&[("ServerlessCacheName", "retention-cache"), ("Engine", "valkey")]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the cache the modify rows need must be creatable, got {} {}",
        created.status, created.body
    );
    poll_until(
        &emulator,
        REGION,
        "the modify rows",
        "the serverless cache settling at AVAILABLE",
        "DescribeServerlessCaches",
        &params(&[("ServerlessCacheName", "retention-cache")]),
        |reply| reply.text("Status") == Some(state::SERVERLESS_STATUS_AVAILABLE),
    )
    .await;

    // The axis: which side of the published range the value falls on, how far
    // past it, and which operation the limit is named on. The range is the one
    // `CreateServerlessCache$SnapshotRetentionLimit` publishes ("The maximum
    // value allowed is 35 days"), so the rows sit one past each bound and far
    // past the maximum.
    for limit in ["-1", "-35", "36", "100", "2147483647"] {
        let refused = emulator
            .call(
                "CreateServerlessCache",
                &params(&[
                    ("ServerlessCacheName", "refused-cache"),
                    ("Engine", "valkey"),
                    ("SnapshotRetentionLimit", limit),
                ]),
            )
            .await;
        assert_refusal(
            &format!("a create naming {limit}"),
            &refused,
            400,
            "InvalidParameterValue",
            &retention_limit_sentence(limit),
        );

        let refused = emulator
            .call(
                "ModifyServerlessCache",
                &params(&[
                    ("ServerlessCacheName", "retention-cache"),
                    ("SnapshotRetentionLimit", limit),
                ]),
            )
            .await;
        assert_refusal(
            &format!("a modify naming {limit}"),
            &refused,
            400,
            "InvalidParameterValue",
            &retention_limit_sentence(limit),
        );
    }

    // Each published bound is still accepted, so the sentence above reports a
    // rule rather than a range this suite invented.
    for limit in ["0", "35"] {
        let accepted = emulator
            .call(
                "ModifyServerlessCache",
                &params(&[
                    ("ServerlessCacheName", "retention-cache"),
                    ("SnapshotRetentionLimit", limit),
                ]),
            )
            .await;
        assert_eq!(
            accepted.status, 200,
            "case {limit:?}: a published bound must be accepted, got {} {}",
            accepted.status, accepted.body
        );
    }
}
