// SPDX-License-Identifier: CC0-1.0

//! What a request the identifier-FORM gate refuses leaves behind.
//!
//! The lead-in each member's refusal is worded with, and the gate's precedence over the
//! resolution behind it, are pinned in [`crate::refusal_wording_per_class`]. This suite is
//! about the other half of that refusal: the rule that a request refused for the FORM of an
//! identifier changes nothing a later caller sees. Both legs below are over a SET of inputs
//! rather than the one request a probe sends:
//!
//! 1. A create the gate refuses provisions nothing — the account's clusters are exactly the
//!    ones it was given, whichever way the id missed the grammar.
//! 2. A delete the gate refuses removes nothing — every default parameter group of a
//!    published family carries the reserved `default.` prefix, whose `.` the grammar does not
//!    admit, so the delete is refused at the gate ahead of the protection on that namespace,
//!    and the group is still reported exactly as it was. A name the grammar ADMITS reaches
//!    the group itself, so a custom group of the same family still deletes.
//!
//! Every assertion goes over the wire through the same router the production emulator mounts,
//! using the harness in [`crate::harness`]; the only thing read out of the service's own
//! source is the rule that SEEDS an account's default groups, so the table covers every
//! family the service publishes rather than the one a probe happened to name.
//!
//! The malformed values below are the INPUT axis of those two rules, not a second statement of
//! the wording: the sentences come from [`crate::refusal_wording_per_class`], and what each
//! row asserts is the STATE after the refusal — which no suite about the wording reads.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::cache_cluster_lifecycle::{create_available_cluster, create_cluster};
use crate::harness::{assert_refusal, params, Emulator};
// The sentences are stated once for this crate, by the suite about the refusal's own wording.
use crate::refusal_wording_per_class::{
    CLUSTER_ID_LEAD_IN, GRAMMAR, PARAMETER_GROUP_NAME_LEAD_IN,
};

/// The rest of a `CreateCacheCluster` request, so the call reaches the identifier rule rather
/// than being refused for something it is missing.
const CLUSTER_SHAPE: &[(&str, &str)] = &[
    ("Engine", "redis"),
    ("CacheNodeType", "cache.t3.micro"),
    ("NumCacheNodes", "1"),
];

/// Every way a value can miss the grammar: each clause of the rule, one at a time, plus the
/// two forms a caller reaches for most — a name in another product's style, and the `.` the
/// service's own reserved names carry.
const MALFORMED: &[(&str, &str)] = &[
    ("upper case and underscores", "Mixed_Case_Underscored"),
    ("a leading digit", "9starts-with-a-digit"),
    ("a leading hyphen", "-leading-hyphen"),
    ("a trailing hyphen", "trailing-hyphen-"),
    ("two consecutive hyphens", "two--hyphens"),
    ("a dot", "dotted.name"),
    ("a space", "two words"),
    ("a character outside ASCII", "caf\u{e9}-cache"),
];

/// The id of the cluster that proves the account CAN hold one, so the page read after each
/// refused create is a page with something in it.
const CONTROL_CLUSTER: &str = "form-gate-control";

#[tokio::test]
async fn a_create_the_form_gate_refuses_provisions_nothing() {
    let emulator = Emulator::new();
    create_available_cluster(&emulator, CONTROL_CLUSTER, CLUSTER_SHAPE).await;

    let expected = format!("{CLUSTER_ID_LEAD_IN} {GRAMMAR}");
    for &(how, value) in MALFORMED {
        let refused = create_cluster(&emulator, value, CLUSTER_SHAPE).await;
        assert_refusal(
            &format!("a create with {how} ({value:?})"),
            &refused,
            400,
            "InvalidParameterValue",
            &expected,
        );
        // Nothing was provisioned under the refused id, and nothing under any other: the
        // account still holds exactly the one cluster it was given.
        let page = emulator.call("DescribeCacheClusters", &[]).await;
        assert_eq!(
            page.all("CacheClusters/CacheCluster/CacheClusterId"),
            vec![CONTROL_CLUSTER],
            "case {how} ({value:?}): the refused create must leave the account's clusters \
             alone, got {}",
            page.body
        );
    }
}

/// Every default parameter group an account holds, each with the family it belongs to, taken
/// from the seeding rule itself ([`state::default_parameter_groups`]) rather than re-derived:
/// one per published family, plus the cluster-mode-on default of every family that publishes
/// the cluster-mode parameter.
fn default_groups() -> Vec<(String, String)> {
    state::default_parameter_groups()
        .into_values()
        .map(|group| (group.name, group.family))
        .collect()
}

/// Read one parameter group back, asserting the read succeeded, and answer with the
/// projection it reports: every text node but the envelope's own per-call metadata, which is
/// what a later read of an untouched group must repeat verbatim.
async fn group_projection(emulator: &Emulator, case: &str, name: &str) -> Vec<(String, String)> {
    let described = emulator
        .call(
            "DescribeCacheParameterGroups",
            &params(&[("CacheParameterGroupName", name)]),
        )
        .await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: the group {name:?} must be listed, got {} {}",
        described.status, described.body
    );
    described
        .nodes
        .into_iter()
        .filter(|(path, _)| !path.contains("ResponseMetadata"))
        .collect()
}

#[tokio::test]
async fn a_default_group_delete_is_refused_at_the_gate_and_changes_nothing() {
    let emulator = Emulator::new();
    let expected = format!("{PARAMETER_GROUP_NAME_LEAD_IN} {GRAMMAR}");

    for (name, family) in default_groups() {
        let before = group_projection(&emulator, &name, &name).await;
        assert!(
            before
                .iter()
                .any(|(path, text)| path.ends_with("CacheParameterGroupFamily")
                    && *text == family),
            "case {name:?}: the group must report its family {family:?}, got {before:?}"
        );

        let refused = emulator
            .call(
                "DeleteCacheParameterGroup",
                &params(&[("CacheParameterGroupName", name.as_str())]),
            )
            .await;
        assert_refusal(
            &format!("deleting {name:?}"),
            &refused,
            400,
            "InvalidParameterValue",
            &expected,
        );

        let after = group_projection(&emulator, &name, &name).await;
        assert_eq!(
            after, before,
            "case {name:?}: the refused delete must leave the group exactly as it was, got \
             {after:?}"
        );
    }

    // A name the grammar ADMITS reaches the group itself, so a custom group of a published
    // family deletes — which is what makes the rows above refusals of their FORM rather than
    // a delete that refuses everything.
    let family = state::PARAMETER_GROUP_FAMILIES[0];
    let custom = "form-gate-custom-group";
    let created = emulator
        .call(
            "CreateCacheParameterGroup",
            &params(&[
                ("CacheParameterGroupName", custom),
                ("CacheParameterGroupFamily", family),
                ("Description", "a group the grammar admits"),
            ]),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "the custom group must be creatable, got {} {}",
        created.status, created.body
    );
    let deleted = emulator
        .call(
            "DeleteCacheParameterGroup",
            &params(&[("CacheParameterGroupName", custom)]),
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "the custom group must be deletable, got {} {}",
        deleted.status, deleted.body
    );
    let gone = emulator
        .call(
            "DescribeCacheParameterGroups",
            &params(&[("CacheParameterGroupName", custom)]),
        )
        .await;
    assert_eq!(
        (gone.status, gone.error_code()),
        (404, "CacheParameterGroupNotFound"),
        "the deleted group must answer the modelled not-found, got {} {}",
        gone.status,
        gone.body
    );
}
