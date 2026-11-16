// SPDX-License-Identifier: CC0-1.0

//! The ONE length bound this service holds a `ReplicationGroupId` to, at every operation
//! that states that bound with the PROSE name of the member.
//!
//! The rule under test: a replication-group name may be at most 40 characters, so an
//! operation that validates a `ReplicationGroupId`'s length refuses a longer one 400
//! `InvalidParameterValue` stating THAT bound — never the wider bound a cache-cluster
//! identifier has — and an identifier the bound admits goes on to be resolved. The number is
//! the same at the create that STORES the id and at the changes that resolve one, because an
//! id a create cannot store names no group a later call could ever find: a client told a
//! wider ceiling builds ids the service will not store.
//!
//! The tests are tables. The rows differ along what the rule generalises over — which
//! operation answers, how far past the bound the identifier is (one character, as far as the
//! cache-cluster bound, past that too), and the case the identifier is spelled in — and each
//! operation is checked AT the bound as well as past it, so the length the recorded probe
//! happened to use is one row of many.
//!
//! Every assertion goes over the wire through the same router the production emulator mounts,
//! using the harness in [`crate::harness`]; nothing reaches into the service's state struct.

use service_com_amazonaws_elasticache_amazonelasticachev9::service::identifier_length_rules;
use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{params, Emulator};

/// What an operation answers for an identifier the length rule ADMITS: the rule did not
/// fire, so the answer is about the replication group the identifier names.
#[derive(Debug, Clone, Copy)]
enum AtBound {
    /// The group was looked up and this account does not hold it; this is the status and
    /// `awsQuery` code that operation reports that with.
    NotHeld(u16, &'static str),
    /// The create accepted it: an id at the bound is one the service stores.
    Stored,
}

/// One row: an operation that validates a `ReplicationGroupId`'s length, the rest of the
/// request it needs to reach that rule, and what it answers for an id the rule admits.
#[derive(Debug, Clone, Copy)]
struct BoundCase {
    case: &'static str,
    action: &'static str,
    extra: &'static [(&'static str, &'static str)],
    at_bound: AtBound,
}

/// The axis: WHICH operation holds the identifier to the replication-group name bound — the
/// create that stores it, the replica-count decrease and the shard reconfiguration that
/// resolve one.
const BOUND_CASES: &[BoundCase] = &[
    BoundCase {
        case: "the create that would store the id",
        action: "CreateReplicationGroup",
        extra: &[("ReplicationGroupDescription", "a group named at the bound")],
        at_bound: AtBound::Stored,
    },
    BoundCase {
        case: "a replica count decrease",
        action: "DecreaseReplicaCount",
        extra: &[("ApplyImmediately", "true"), ("NewReplicaCount", "0")],
        at_bound: AtBound::NotHeld(400, "InvalidParameterValue"),
    },
    BoundCase {
        case: "a shard reconfiguration",
        action: "ModifyReplicationGroupShardConfiguration",
        extra: &[("ApplyImmediately", "true"), ("NodeGroupCount", "2")],
        at_bound: AtBound::NotHeld(404, "ReplicationGroupNotFoundFault"),
    },
];

/// A well-formed identifier of exactly `length` characters: it begins with a letter and
/// carries only letters, so the only thing that can be wrong with it is its length.
fn identifier(length: usize) -> String {
    format!("g{}", "a".repeat(length - 1))
}

/// HOW far past the bound an identifier is, paired with the identifier itself.
///
/// The middle rows are the interval between the replication-group bound and the WIDER
/// cache-cluster bound: an id in there is the one a client builds when it is told the wrong
/// ceiling, so each operation is checked at both ends of it and past it.
fn past_the_bound() -> Vec<(String, String)> {
    let bound = state::REPLICATION_GROUP_ID_MAX_LEN;
    let cluster_bound = state::CLUSTER_ID_MAX_LEN;
    let mut rows = vec![
        ("one character past the bound".to_string(), identifier(bound + 1)),
        (
            "inside the cache-cluster identifier bound".to_string(),
            identifier((bound + cluster_bound) / 2),
        ),
        (
            "at the cache-cluster identifier bound".to_string(),
            identifier(cluster_bound),
        ),
        (
            "past the cache-cluster identifier bound".to_string(),
            identifier(cluster_bound + 1),
        ),
        ("far past the bound".to_string(), identifier(bound + 37)),
    ];
    // The same length spelled in mixed case: the id is stored lowercased, so what the rule
    // measures is the LENGTH and the case cannot change which answer it gets. Derived from the
    // same generator as the rows above, so the row stays one character past the bound whatever
    // that bound is.
    let alternating: String = identifier(bound + 1)
        .chars()
        .enumerate()
        .map(|(index, letter)| {
            if index % 2 == 0 {
                letter.to_ascii_uppercase()
            } else {
                letter
            }
        })
        .collect();
    rows.push((
        "mixed case, one character past the bound".to_string(),
        alternating,
    ));
    rows
}

/// The sentence this service refuses an over-long `ReplicationGroupId` with, built from the
/// bound rather than written out, so the test states the RULE and not one number twice.
fn refusal() -> String {
    format!(
        "Replication Group ID cannot be longer than {} characters.",
        state::REPLICATION_GROUP_ID_MAX_LEN
    )
}

#[tokio::test]
async fn the_bound_an_over_long_replication_group_id_is_refused_with_is_the_name_maximum() {
    // Where the bound comes from: the documented maximum of this member ("A name must
    // contain from 1 to 40 alphanumeric characters or hyphens"), which is NOT the wider
    // maximum a cache-cluster identifier has — the two are different members, so a drift
    // that made them equal would hide the defect this suite is about.
    assert_eq!(
        state::REPLICATION_GROUP_ID_MAX_LEN,
        40,
        "ReplicationGroupId is documented as at most 40 characters, got {}",
        state::REPLICATION_GROUP_ID_MAX_LEN
    );
    // That the two bounds are DIFFERENT, asserted through the rule rather than by comparing
    // the numbers: an identifier as long as a cache-cluster identifier may be is one this
    // member's rule refuses, which is exactly what a handler holding this member to the
    // cluster bound would let through.
    assert_eq!(
        identifier_length_rules::replication_group_id_prose_refusal(&identifier(
            state::CLUSTER_ID_MAX_LEN
        )),
        Some(refusal()),
        "an id at the cache-cluster bound is past the replication-group bound, got {:?}",
        identifier_length_rules::replication_group_id_prose_refusal(&identifier(
            state::CLUSTER_ID_MAX_LEN
        ))
    );
    // And the rule is stated once: the helper every one of these operations words its
    // refusal through answers that same sentence.
    assert_eq!(
        identifier_length_rules::replication_group_id_prose_refusal(&identifier(
            state::REPLICATION_GROUP_ID_MAX_LEN + 1
        )),
        Some(refusal()),
        "the shared rule must word the bound the operations state, got {:?}",
        identifier_length_rules::replication_group_id_prose_refusal(&identifier(
            state::REPLICATION_GROUP_ID_MAX_LEN + 1
        ))
    );

    for row in BOUND_CASES {
        let emulator = Emulator::new();
        let name = row.case;
        for (how, value) in past_the_bound() {
            let mut request = params(&[("ReplicationGroupId", value.as_str())]);
            request.extend(params(row.extra));
            let refused = emulator.call(row.action, &request).await;
            assert_eq!(
                (refused.status, refused.error_code()),
                (400, "InvalidParameterValue"),
                "case {name} / {how}: expected 400 InvalidParameterValue, got {} {} {}",
                refused.status,
                refused.error_code(),
                refused.body
            );
            assert_eq!(
                refused.error_message(),
                refusal(),
                "case {name} / {how}: expected the name-maximum rule, got {:?}",
                refused.error_message()
            );
            // The sentence states the rule, so it carries nothing from the request —
            // neither the identifier nor its actual length.
            assert!(
                !refused.error_message().contains(value.as_str()),
                "case {name} / {how}: the rule carries no request value, got {:?}",
                refused.error_message()
            );
            assert!(
                !refused.error_message().contains(&value.len().to_string()),
                "case {name} / {how}: the rule states the bound, not the length, got {:?}",
                refused.error_message()
            );
            // A refused request stored nothing: the id it named resolves to no group.
            let described = emulator
                .call(
                    "DescribeReplicationGroups",
                    &params(&[("ReplicationGroupId", value.as_str())]),
                )
                .await;
            assert_ne!(
                described.status, 200,
                "case {name} / {how}: a refused request stores no group, got {}",
                described.body
            );
            assert!(
                described.all("ReplicationGroupId").is_empty(),
                "case {name} / {how}: a refused request stores no group, got {}",
                described.body
            );
        }
    }
}

#[tokio::test]
async fn an_identifier_at_the_bound_is_resolved_rather_than_refused() {
    let at_bound = identifier(state::REPLICATION_GROUP_ID_MAX_LEN);
    for row in BOUND_CASES {
        let emulator = Emulator::new();
        let name = row.case;
        let mut request = params(&[("ReplicationGroupId", at_bound.as_str())]);
        request.extend(params(row.extra));
        let answered = emulator.call(row.action, &request).await;
        match row.at_bound {
            AtBound::NotHeld(status, code) => {
                assert_eq!(
                    (answered.status, answered.error_code()),
                    (status, code),
                    "case {name} / at the bound: expected {status} {code}, got {} {} {}",
                    answered.status,
                    answered.error_code(),
                    answered.body
                );
                assert_ne!(
                    answered.error_message(),
                    refusal(),
                    "case {name} / at the bound: the lookup must have run, got {:?}",
                    answered.error_message()
                );
            }
            AtBound::Stored => {
                assert_eq!(
                    answered.status, 200,
                    "case {name} / at the bound: the create must store it, got {} {}",
                    answered.status, answered.body
                );
                // Read back through the implemented reader: the id at the bound is one the
                // store holds and every later call can resolve.
                let described = emulator
                    .call(
                        "DescribeReplicationGroups",
                        &params(&[("ReplicationGroupId", at_bound.as_str())]),
                    )
                    .await;
                assert_eq!(
                    described.status, 200,
                    "case {name} / at the bound: the stored group must be readable, got {} {}",
                    described.status, described.body
                );
                assert_eq!(
                    described.text("ReplicationGroupId"),
                    Some(at_bound.as_str()),
                    "case {name} / at the bound: the read must report the stored id, got {:?}",
                    described.text("ReplicationGroupId")
                );
            }
        }
    }
}
