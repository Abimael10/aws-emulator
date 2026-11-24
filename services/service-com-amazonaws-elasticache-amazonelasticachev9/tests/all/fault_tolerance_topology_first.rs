// SPDX-License-Identifier: CC0-1.0

//! What a `CreateReplicationGroup` that asks for Multi-AZ asks of the group's TOPOLOGY, and
//! in which order the two settings behind fault tolerance are judged.
//!
//! The rule under test: Multi-AZ fault tolerance IS the promotion of a replica in another
//! Availability Zone, so a request that asks for it asks for automatic failover, and the
//! cluster-count rule failover carries — "If AutomaticFailoverEnabled is true, the value of
//! [NumCacheClusters] must be at least 2" — is applied to it. That rule is answered BEFORE the
//! two flags are judged against each other: a group with no replica to promote is refused with
//! the count sentence however the request asked for the promotion, and only a request whose
//! topology CAN fail over is then held to naming both settings. Both sentences are wholly fixed
//! text, refused 400 `InvalidParameterCombination`, and a refused create stores nothing.
//!
//! The tests are tables. The rows differ along what the rule generalises over — which setting
//! asks for the promotion (the failover flag, the Multi-AZ flag, or both), and which member
//! says how many clusters the group's shard would hold (nothing at all, a cluster count, a
//! replica count) — so the recorded probe's request is one row of many.
//!
//! Every assertion goes over the wire through the same router the production emulator mounts,
//! using the harness in [`crate::harness`]; nothing reaches into the service's state struct.

use crate::harness::{params, poll_until, Emulator, Reply, REGION};

/// How ElastiCache refuses fault tolerance on a group whose shard holds nothing to promote:
/// the COUNT of cache clusters such a group needs, in wholly fixed text.
const NEEDS_TWO_CLUSTERS: &str = "When using automatic failover, there must be at least 2 cache \
                                  clusters in the replication group.";

/// How it refuses Multi-AZ on a group that CAN fail over but whose request does not ask for
/// the failover: the two settings name one capability, so one without the other is the
/// combination that cannot hold.
const MULTI_AZ_NEEDS_FAILOVER: &str =
    "MultiAZEnabled requires AutomaticFailoverEnabled: Multi-AZ fault tolerance is the \
     promotion of a replica in another Availability Zone.";

/// One row: how the request asks for fault tolerance and what topology it asks for, and the
/// sentence it is refused with.
struct PromotionCase {
    case: &'static str,
    extra: &'static [(&'static str, &'static str)],
    refusal: &'static str,
}

/// The axis: which flag asks for the promotion, and which member (if any) says how many
/// clusters the shard would hold. The last two rows have a replica to promote, so they reach
/// the pairing rule instead of the count rule — which is what pins the ORDER.
const PROMOTION_CASES: &[PromotionCase] = &[
    PromotionCase {
        case: "the failover flag on a group that named no cluster count",
        extra: &[("AutomaticFailoverEnabled", "true")],
        refusal: NEEDS_TWO_CLUSTERS,
    },
    PromotionCase {
        case: "the failover flag on a group of one cluster",
        extra: &[
            ("AutomaticFailoverEnabled", "true"),
            ("NumCacheClusters", "1"),
        ],
        refusal: NEEDS_TWO_CLUSTERS,
    },
    PromotionCase {
        case: "the Multi-AZ flag on a group that named no cluster count",
        extra: &[("MultiAZEnabled", "true")],
        refusal: NEEDS_TWO_CLUSTERS,
    },
    PromotionCase {
        case: "the Multi-AZ flag on a group of one cluster",
        extra: &[("MultiAZEnabled", "true"), ("NumCacheClusters", "1")],
        refusal: NEEDS_TWO_CLUSTERS,
    },
    PromotionCase {
        case: "the Multi-AZ flag on a shard of no replica",
        extra: &[("MultiAZEnabled", "true"), ("ReplicasPerNodeGroup", "0")],
        refusal: NEEDS_TWO_CLUSTERS,
    },
    PromotionCase {
        case: "both flags on a group of one cluster",
        extra: &[
            ("MultiAZEnabled", "true"),
            ("AutomaticFailoverEnabled", "true"),
            ("NumCacheClusters", "1"),
        ],
        refusal: NEEDS_TWO_CLUSTERS,
    },
    PromotionCase {
        case: "the Multi-AZ flag alone on a group of two clusters",
        extra: &[("MultiAZEnabled", "true"), ("NumCacheClusters", "2")],
        refusal: MULTI_AZ_NEEDS_FAILOVER,
    },
    PromotionCase {
        case: "the Multi-AZ flag alone on a shard of one replica",
        extra: &[("MultiAZEnabled", "true"), ("ReplicasPerNodeGroup", "1")],
        refusal: MULTI_AZ_NEEDS_FAILOVER,
    },
];

/// One create of `id`, with `extra` beside the two members every create names.
async fn create_group(emulator: &Emulator, id: &str, extra: &[(&str, &str)]) -> Reply {
    let mut pairs = params(&[
        ("ReplicationGroupId", id),
        ("ReplicationGroupDescription", "a fault-tolerance row"),
    ]);
    pairs.extend(params(extra));
    emulator.call("CreateReplicationGroup", &pairs).await
}

#[tokio::test]
async fn fault_tolerance_is_refused_by_the_topology_before_the_flags_are_paired() {
    for (index, row) in PROMOTION_CASES.iter().enumerate() {
        let emulator = Emulator::new();
        let case = row.case;
        let id = format!("promotion-row-{index}");
        let refused = create_group(&emulator, &id, row.extra).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (400, "InvalidParameterCombination"),
            "case {case:?}: expected 400 InvalidParameterCombination, got {} {} {}",
            refused.status,
            refused.error_code(),
            refused.body
        );
        assert_eq!(
            refused.error_message(),
            row.refusal,
            "case {case:?}: expected {:?}, got {:?}",
            row.refusal,
            refused.error_message()
        );
        // A refused create stored nothing: the id it named resolves to no group.
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
async fn a_group_that_can_fail_over_and_asks_for_both_settings_is_served() {
    // The same topologies the rows above name, now with a replica to promote AND both
    // settings asked for: what the rule refuses is the missing replica or the missing
    // pairing, never Multi-AZ itself.
    let served: &[(&str, &[(&str, &str)])] = &[
        (
            "two clusters in the shard",
            &[
                ("MultiAZEnabled", "true"),
                ("AutomaticFailoverEnabled", "true"),
                ("NumCacheClusters", "2"),
            ],
        ),
        (
            "one replica in the shard",
            &[
                ("MultiAZEnabled", "true"),
                ("AutomaticFailoverEnabled", "true"),
                ("ReplicasPerNodeGroup", "1"),
            ],
        ),
    ];

    for (index, (case, extra)) in served.iter().enumerate() {
        let emulator = Emulator::new();
        let id = format!("promotion-served-{index}");
        let created = create_group(&emulator, &id, extra).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the create must be accepted, got {} {}",
            created.status, created.body
        );
        // Read it back through the implemented describe: the group carries both settings the
        // request asked for.
        let settled = poll_until(
            &emulator,
            REGION,
            case,
            &format!("the replication group {id} settling at available"),
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", id.as_str())]),
            |reply| reply.text("ReplicationGroup/Status") == Some("available"),
        )
        .await;
        assert_eq!(
            (
                settled.text("ReplicationGroup/AutomaticFailover"),
                settled.text("ReplicationGroup/MultiAZ")
            ),
            (Some("enabled"), Some("enabled")),
            "case {case:?}: both settings must be reported enabled, got {}",
            settled.body
        );
    }
}
