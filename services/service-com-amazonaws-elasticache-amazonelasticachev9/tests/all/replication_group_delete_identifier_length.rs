// SPDX-License-Identifier: CC0-1.0

//! The sentence `DeleteReplicationGroup` refuses an over-long `ReplicationGroupId` with.
//!
//! The rule under test: an operation that holds a `ReplicationGroupId` to the 40-character
//! maximum a replication-group name may have words BOTH halves of that identifier rule with
//! the subject IT names the identifier by, and the delete names it as the resource in prose —
//! so a longer id is refused 400 `InvalidParameterValue` with `Replication Group ID cannot be
//! longer than <bound> characters.` and an id the grammar does not admit with `Replication
//! Group ID specified is not a valid identifier. …`, neither sentence carrying anything of
//! the value that broke it. The refusal is answered BEFORE the id is resolved, so it does not
//! depend on whether this account holds a group under it; an id the rule admits reaches the
//! lookup, and one at the bound is an id a create stores and this delete serves.
//!
//! The tests are tables. The rows differ along what the rule generalises over — how far past
//! the bound the identifier is (one character, as far as the wider cache-cluster bound, past
//! that too), the case it is spelled in, which other members the request carries
//! (`RetainPrimaryCluster`, `FinalSnapshotIdentifier`, both), and which clause of the grammar
//! a value of admissible length breaks — so the id and the request the recorded probe
//! happened to use are one row of many.
//!
//! Every assertion goes over the wire through the same router the production emulator mounts,
//! using the harness in [`crate::harness`]. The one thing reached through the store is the DUE
//! INSTANT of a change the clock already owes ([`crate::harness::settle_pending_changes`],
//! [`crate::harness::hold_pending_changes`]) — the status itself is the handlers' own — so the
//! row that deletes a group named at the bound does not depend on how fast the host runs.

use service_com_amazonaws_elasticache_amazonelasticachev9::{service, state};

use crate::harness::{
    assert_refusal, hold_pending_changes, params, settle_pending_changes, Emulator,
};

/// A well-formed identifier of exactly `length` characters: it begins with a letter and
/// carries only letters, so the only thing that can be wrong with it is its length.
fn identifier(length: usize) -> String {
    format!("g{}", "a".repeat(length - 1))
}

/// The sentence this operation refuses an over-long `ReplicationGroupId` with, built from the
/// published bound rather than written out, so the test states the RULE and not one number
/// twice.
fn over_length_refusal() -> String {
    format!(
        "Replication Group ID cannot be longer than {} characters.",
        state::REPLICATION_GROUP_ID_MAX_LEN
    )
}

/// HOW far past the bound an identifier is, paired with the identifier itself.
///
/// The middle rows sit in the interval between this member's bound and the WIDER bound a
/// cache-cluster identifier has: an id in there is the one a client builds when it is told
/// the wrong ceiling, so the row is checked at both ends of that interval and past it.
fn past_the_bound() -> Vec<(String, String)> {
    let bound = state::REPLICATION_GROUP_ID_MAX_LEN;
    let cluster_bound = state::CLUSTER_ID_MAX_LEN;
    let mut rows = vec![
        (
            "one character past the bound".to_string(),
            identifier(bound + 1),
        ),
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
    // measures is the LENGTH and the case cannot change which answer it gets.
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

/// The axis of the REST of the request: the two members that change what a delete takes, and
/// the combination of them the service has its own rule about — the length rule runs ahead of
/// every one of them, so none of them changes the sentence.
const REQUEST_SHAPES: &[(&str, &[(&str, &str)])] = &[
    ("the id alone", &[]),
    (
        "retaining the primary",
        &[("RetainPrimaryCluster", "true")],
    ),
    (
        "not retaining the primary",
        &[("RetainPrimaryCluster", "false")],
    ),
    (
        "taking a final snapshot",
        &[("FinalSnapshotIdentifier", "final-snap")],
    ),
    (
        "retaining the primary and taking a final snapshot",
        &[
            ("RetainPrimaryCluster", "true"),
            ("FinalSnapshotIdentifier", "final-snap"),
        ],
    ),
];

#[tokio::test]
async fn the_delete_refuses_an_over_long_identifier_with_the_resource_named_in_prose() {
    // Where the bound comes from: the documented maximum of this member ("A name must contain
    // from 1 to 40 alphanumeric characters or hyphens"), which is NOT the wider maximum a
    // cache-cluster identifier has — a drift that made them equal would hide what this suite
    // is about.
    assert_eq!(
        state::REPLICATION_GROUP_ID_MAX_LEN, 40,
        "ReplicationGroupId is documented as at most 40 characters, got {}",
        state::REPLICATION_GROUP_ID_MAX_LEN
    );
    let expected = over_length_refusal();
    let emulator = Emulator::new();

    for (how, value) in past_the_bound() {
        for (shape, extra) in REQUEST_SHAPES {
            let case = format!("{how} / {shape}");
            let mut pairs = params(&[("ReplicationGroupId", value.as_str())]);
            pairs.extend(params(extra));
            let refused = emulator.call("DeleteReplicationGroup", &pairs).await;
            assert_refusal(&case, &refused, 400, "InvalidParameterValue", &expected);
            // The sentence states the rule, so it carries nothing from the request — neither
            // the identifier nor its actual length.
            assert!(
                !refused.error_message().contains(value.as_str()),
                "case {case:?}: the rule carries no request value, got {:?}",
                refused.error_message()
            );
            assert!(
                !refused.error_message().contains(&value.len().to_string()),
                "case {case:?}: the rule states the bound, not the length, got {:?}",
                refused.error_message()
            );
            // And it names the resource in prose rather than the wire member, which is the
            // whole difference a client branching on the text sees.
            assert!(
                !refused.error_message().contains("ReplicationGroupId"),
                "case {case:?}: the delete names the resource in prose, got {:?}",
                refused.error_message()
            );
        }
    }
}

#[tokio::test]
async fn the_delete_states_the_grammar_half_of_the_rule_with_the_same_subject() {
    // Every way a value of admissible length can miss the form every identifier of this
    // service shares: a leading digit, a leading hyphen, a trailing hyphen, two consecutive
    // hyphens, a character outside the set, and the '.' a family's own default group name
    // carries.
    let malformed = [
        "9starts-with-a-digit",
        "-leading-hyphen",
        "trailing-hyphen-",
        "two--hyphens",
        "under_score",
        "default.redis7",
    ];
    let expected = "Replication Group ID specified is not a valid identifier. Identifiers must \
                    begin with a letter; must contain only ASCII letters, digits, and hyphens; \
                    and must not end with a hyphen or contain two consecutive hyphens.";
    let emulator = Emulator::new();

    for value in malformed {
        let refused = emulator
            .call(
                "DeleteReplicationGroup",
                &params(&[("ReplicationGroupId", value)]),
            )
            .await;
        assert_refusal(
            &format!("the identifier {value:?}"),
            &refused,
            400,
            "InvalidParameterValue",
            expected,
        );
        // The two halves of the rule are told apart: a value of admissible length is not
        // answered with the length sentence.
        assert_ne!(
            refused.error_message(),
            over_length_refusal(),
            "case the identifier {value:?}: the length half must not fire, got {:?}",
            refused.error_message()
        );
    }
}

#[tokio::test]
async fn an_identifier_at_the_bound_reaches_the_lookup_and_a_group_named_at_it_is_deletable() {
    let at_bound = identifier(state::REPLICATION_GROUP_ID_MAX_LEN);
    let emulator = Emulator::new();

    // The rule did not fire, so the answer is about the GROUP the id names: this account
    // holds none under it yet, which is the delete's own not-found sentence.
    for (shape, extra) in REQUEST_SHAPES {
        let mut pairs = params(&[("ReplicationGroupId", at_bound.as_str())]);
        pairs.extend(params(extra));
        let answered = emulator.call("DeleteReplicationGroup", &pairs).await;
        assert_refusal(
            &format!("at the bound / {shape}"),
            &answered,
            404,
            "ReplicationGroupNotFoundFault",
            service::replication_group_not_found::DELETE_MESSAGE,
        );
    }

    // And the bound is the one a create stores against, so an id AT it names a group this
    // delete serves: the rule refuses no request the service used to accept.
    let created = emulator
        .call(
            "CreateReplicationGroup",
            &params(&[
                ("ReplicationGroupId", at_bound.as_str()),
                ("ReplicationGroupDescription", "a group named at the bound"),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the create must store an id at the bound, got {} {}",
        created.status, created.body
    );
    // Carry the work the create started through, so the group is in the state a delete may
    // begin from without the suite waiting out the span the clock owes it.
    settle_pending_changes(&emulator);
    let settled = emulator
        .call(
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", at_bound.as_str())]),
        )
        .await;
    assert_eq!(
        settled.text("ReplicationGroup/Status"),
        Some(state::STATUS_AVAILABLE),
        "the group named at the bound must settle at available, got {} {}",
        settled.status,
        settled.body
    );
    let deleted = emulator
        .call(
            "DeleteReplicationGroup",
            &params(&[("ReplicationGroupId", at_bound.as_str())]),
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "the delete of a group named at the bound must be served, got {} {}",
        deleted.status, deleted.body
    );
    // Read it back through the implemented reader: the accepted delete began the teardown the
    // group now reports, so the 200 above is backed by state and not a bare status. The
    // teardown is held out of reach first, so what the reader sees is the phase the delete put
    // the group in rather than a race with the clock.
    hold_pending_changes(&emulator);
    let described = emulator
        .call(
            "DescribeReplicationGroups",
            &params(&[("ReplicationGroupId", at_bound.as_str())]),
        )
        .await;
    assert_eq!(
        described.text("ReplicationGroup/Status"),
        Some(state::STATUS_DELETING),
        "the deleted group must report its teardown, got {} {}",
        described.status,
        described.body
    );
}
