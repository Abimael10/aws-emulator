// SPDX-License-Identifier: CC0-1.0

//! The bound `CreateReplicationGroup` holds the number of replicas a node group (shard) gets
//! to, and the one sentence it states that bound in.
//!
//! The rule under test: every member of a create that sets how many clusters a shard has sets
//! the same count — `ReplicasPerNodeGroup` and a shard's `NodeGroupConfiguration$ReplicaCount`
//! state the REPLICAS directly, `NumCacheClusters` states them as the primary plus its
//! replicas — and a request implying a count outside the bound that member documents ("Valid
//! values are 0 to 5") is refused 400 `InvalidParameterValue` with the fixed sentence `The
//! number of replicas per node group must be within <min> and <max>.` The sentence states the
//! BOUNDS and nothing else: neither the count that missed them nor the member that carried
//! it, so one sentence answers every way a request can ask for too few or too many. A count
//! the bound admits is stored, and the group reads back with that many clusters per shard.
//!
//! The tests are tables. The rows differ along what the rule generalises over — which member
//! carries the count, whether the value is below the minimum or above the maximum and by how
//! much, and (for the accepted rows) each published bound — so the recorded probe's request is
//! one row of many.
//!
//! Every assertion goes over the wire through the same router the production emulator mounts,
//! using the harness in [`crate::harness`]; nothing reaches into the service's state struct.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{params, poll_until, Emulator, Reply, REGION};

/// The sentence a create is refused with when the replica count it implies is outside the
/// published bounds, built from those bounds rather than written out, so the test states the
/// RULE and not one number twice.
fn range_refusal() -> String {
    format!(
        "The number of replicas per node group must be within {} and {}.",
        state::MIN_REPLICAS_PER_NODE_GROUP,
        state::MAX_REPLICAS_PER_NODE_GROUP
    )
}

/// `NumCacheClusters` for a shard of `replicas` replicas: the primary plus them.
fn clusters_for(replicas: i32) -> i32 {
    replicas + 1
}

/// The requests whose implied replica count is outside the bound, each as the form pairs it
/// adds to a create. Every member that can carry the count is represented, below the minimum
/// and above the maximum, one step out and far out.
fn refused_rows() -> Vec<(String, Vec<(String, String)>)> {
    let max = state::MAX_REPLICAS_PER_NODE_GROUP;
    let min = state::MIN_REPLICAS_PER_NODE_GROUP;
    vec![
        (
            "no cluster at all, so not even a primary".to_string(),
            params(&[("NumCacheClusters", "0")]),
        ),
        (
            "a negative cluster count".to_string(),
            params(&[("NumCacheClusters", "-3")]),
        ),
        (
            "one cluster more than a shard may have".to_string(),
            vec![(
                "NumCacheClusters".to_string(),
                clusters_for(max + 1).to_string(),
            )],
        ),
        (
            "far more clusters than a shard may have".to_string(),
            vec![(
                "NumCacheClusters".to_string(),
                clusters_for(max + 12).to_string(),
            )],
        ),
        (
            "one replica more than a shard may have".to_string(),
            vec![("ReplicasPerNodeGroup".to_string(), (max + 1).to_string())],
        ),
        (
            "far more replicas than a shard may have".to_string(),
            vec![("ReplicasPerNodeGroup".to_string(), (max + 31).to_string())],
        ),
        (
            "one replica below the minimum".to_string(),
            vec![("ReplicasPerNodeGroup".to_string(), (min - 1).to_string())],
        ),
        (
            "a shard configured with more replicas than it may have".to_string(),
            vec![
                ("NumNodeGroups".to_string(), "1".to_string()),
                (
                    "NodeGroupConfiguration.NodeGroupConfiguration.1.ReplicaCount".to_string(),
                    (max + 1).to_string(),
                ),
            ],
        ),
        (
            "a shard configured below the minimum".to_string(),
            vec![
                ("NumNodeGroups".to_string(), "1".to_string()),
                (
                    "NodeGroupConfiguration.NodeGroupConfiguration.1.ReplicaCount".to_string(),
                    (min - 1).to_string(),
                ),
            ],
        ),
        (
            "the second of two shards configured past the bound".to_string(),
            vec![
                ("NumNodeGroups".to_string(), "2".to_string()),
                (
                    "NodeGroupConfiguration.NodeGroupConfiguration.1.ReplicaCount".to_string(),
                    "1".to_string(),
                ),
                (
                    "NodeGroupConfiguration.NodeGroupConfiguration.2.ReplicaCount".to_string(),
                    (max + 2).to_string(),
                ),
            ],
        ),
    ]
}

/// One accepted row: a count the bound admits, the request that asks for it, and the replicas
/// per shard the group must end up with.
struct AcceptedRow {
    case: &'static str,
    request: Vec<(String, String)>,
    replicas: i32,
}

/// One create of `id`, with `extra` beside the two members every create names.
async fn create_group(emulator: &Emulator, id: &str, extra: &[(String, String)]) -> Reply {
    let mut pairs = params(&[
        ("ReplicationGroupId", id),
        ("ReplicationGroupDescription", "a replica-count row"),
    ]);
    pairs.extend(extra.iter().cloned());
    emulator.call("CreateReplicationGroup", &pairs).await
}

#[tokio::test]
async fn a_replica_count_outside_the_published_bound_is_refused_with_that_bound() {
    // Where the bounds come from: `ReplicasPerNodeGroup`'s own documentation ("Valid values
    // are 0 to 5"), and the cluster count that bound implies — "The maximum permitted value
    // for NumCacheClusters is 6 (1 primary plus 5 replicas)".
    assert_eq!(
        state::MIN_REPLICAS_PER_NODE_GROUP, 0,
        "a shard may have no replica at all, got {}",
        state::MIN_REPLICAS_PER_NODE_GROUP
    );
    assert_eq!(
        state::MAX_REPLICAS_PER_NODE_GROUP, 5,
        "a shard may have at most 5 replicas, got {}",
        state::MAX_REPLICAS_PER_NODE_GROUP
    );
    assert_eq!(
        state::MAX_CACHE_CLUSTERS_PER_GROUP,
        clusters_for(state::MAX_REPLICAS_PER_NODE_GROUP),
        "the cluster maximum is the primary plus the replicas, got {}",
        state::MAX_CACHE_CLUSTERS_PER_GROUP
    );

    let expected = range_refusal();
    for (index, (case, extra)) in refused_rows().into_iter().enumerate() {
        let emulator = Emulator::new();
        let id = format!("replica-bound-{index}");
        let refused = create_group(&emulator, &id, &extra).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "InvalidParameterValue"),
            "case {case:?}: expected 400 InvalidParameterValue, got {} {} {}",
            refused.status,
            refused.error_code(),
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            refused.error_message()
        );
        // Nothing of the count that missed the bounds appears in the sentence: every row
        // above carries a different value under a different member and they are all answered
        // by the one sentence asserted here, which states only the published bounds.
        // A refused create stored nothing.
        let described = emulator
            .call(
                "DescribeReplicationGroups",
                &params(&[("ReplicationGroupId", id.as_str())]),
            )
            .await;
        assert_ne!(
            described.status, 200,
            "case {case:?}: a refused create stores no group, got {}",
            described.body
        );
    }
}

#[tokio::test]
async fn a_replica_count_the_bound_admits_is_stored_on_every_shard() {
    let max = state::MAX_REPLICAS_PER_NODE_GROUP;
    let min = state::MIN_REPLICAS_PER_NODE_GROUP;
    // Each published bound and a count between them, asked for through both members that set
    // it group-wide: the clusters a shard ends with are the primary plus the replicas asked
    // for, read back through the implemented describe.
    let accepted = vec![
        AcceptedRow {
            case: "the minimum, as a cluster count",
            request: vec![(
                "NumCacheClusters".to_string(),
                clusters_for(min).to_string(),
            )],
            replicas: min,
        },
        AcceptedRow {
            case: "the maximum, as a cluster count",
            request: vec![(
                "NumCacheClusters".to_string(),
                clusters_for(max).to_string(),
            )],
            replicas: max,
        },
        AcceptedRow {
            case: "the minimum, as a replica count",
            request: vec![("ReplicasPerNodeGroup".to_string(), min.to_string())],
            replicas: min,
        },
        AcceptedRow {
            case: "the maximum, as a replica count",
            request: vec![("ReplicasPerNodeGroup".to_string(), max.to_string())],
            replicas: max,
        },
        AcceptedRow {
            case: "a count between the bounds",
            request: vec![("ReplicasPerNodeGroup".to_string(), (max / 2).to_string())],
            replicas: max / 2,
        },
    ];

    for (index, row) in accepted.into_iter().enumerate() {
        let emulator = Emulator::new();
        let case = row.case;
        let id = format!("replica-admitted-{index}");
        let created = create_group(&emulator, &id, &row.request).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the create must be accepted, got {} {}",
            created.status, created.body
        );
        // Read the group back once the clock owes it its settled status: a shard holds the
        // primary and the replicas the request asked for.
        let described = poll_until(
            &emulator,
            REGION,
            case,
            &format!("the replication group {id} settling at available"),
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", id.as_str())]),
            |reply| reply.text("ReplicationGroup/Status") == Some("available"),
        )
        .await;
        let members = described.all("MemberClusters/ClusterId").len();
        assert_eq!(
            members,
            clusters_for(row.replicas) as usize,
            "case {case:?}: expected {} member clusters, got {members} in {}",
            clusters_for(row.replicas),
            described.body
        );
    }
}
