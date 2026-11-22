// SPDX-License-Identifier: CC0-1.0

//! The identity a `ListCustomDetectionRuleAssociations` entry advertises belongs to the
//! ASSOCIATION — not to the call that reads it, and not to anything else the account did
//! afterwards.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! serialization and the handlers are exercised together.
//!
//! THE RULE under test: an association's `AssociationId` is established once, by the
//! create that made it, and is a property of that association alone — "the unique
//! identifier for the association"
//! (`smithy_query(shape="AssociationSummary,AssociationDetail")`) — so every later
//! listing, filtered or not, reports the association under that same identity, with the
//! trailing `/association/<id>` segment of that entry's own `arn` equal to it, and that
//! identity is accepted as the `AssociationId` @httpLabel of the association's own read
//! (`smithy_query(operation="GetCustomDetectionRuleAssociation")`). Nothing the account
//! does to any OTHER resource — associating another rule, associating the same rule
//! through another Region's endpoint, moving another association's mode, deleting one,
//! having a duplicate create refused, or creating a detector, each of which advances the
//! identifiers this service mints — may move it.
//!
//! What is new here, beside the sibling association suites (`..._identity`, which walks
//! one association's own lifecycle, `..._listed_handle`, which tabulates rules, Regions,
//! modes and filters, and `..._drawn_listed_handles`, which draws populations and
//! listings): the identity is asserted to be INDEPENDENT of everything around it — of the
//! neighbouring traffic that happens between the create and the listing, and of the ORDER
//! the account made its associations in, which is what decides the characters a run mints.
//!
//! No `AssociationId` and no ARN is written into this suite: each is read off the
//! emulator's own response, because the characters a run mints are a per-run value and
//! only the binding, the agreement with the entry's own ARN and the handle's usability are
//! requirements.

use serde_json::{json, Value};

use crate::custom_detection_rule_association_identity::{
    arn_association_id, associate, entry_with_arn,
};
use crate::custom_detection_rule_association_listed_handle::{
    assert_serialized_identity, listing, Listing, ASSOCIATIONS_PATH, MODES, RULES_PATH,
};
use crate::detector_lifecycle::{call_in, error_code, new_router, published_rule_ids, Router};

/// Two Regions, so an association's identity is asserted to be its own Region's rather
/// than assumed: the store is keyed by `(Region, RuleId)`, so the same rule associated
/// through two endpoints is two associations.
const REGIONS: [&str; 2] = ["us-west-2", "ap-southeast-2"];

/// `MaxResults` is `range 1..100`
/// (`smithy_query(operation="ListCustomDetectionRuleAssociations")`), so one page this
/// size reports every association a case makes at once.
const MAX_RESULTS: i64 = 100;

/// The modelled error class a second create for a rule the account has already associated
/// is refused with (`smithy_query(operation="CreateCustomDetectionRuleAssociation")`).
const CONFLICT: &str = "ConflictException";

/// The text member `name` of `payload`, failing the test when it is not a JSON string.
fn text(payload: &Value, name: &str, case: &str) -> String {
    payload[name]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {name} to be a JSON string, got {payload:?}")
        })
        .to_string()
}

/// The identity one association is known by: the id its create established and the ARN
/// that carries it, both read off the response.
struct Identity {
    region: &'static str,
    rule_id: String,
    id: String,
    arn: String,
}

impl Identity {
    /// The identity the create of `rule_id` in `mode` through `region`'s endpoint
    /// established, asserted to be self-consistent before anything is built on it.
    async fn established(
        router: &Router,
        region: &'static str,
        rule_id: &str,
        mode: &str,
        case: &str,
    ) -> Self {
        let created = associate(router, region, rule_id, mode).await;
        let id = text(&created, "associationId", case);
        let arn = text(&created, "arn", case);
        assert_eq!(
            arn_association_id(&arn),
            Some(id.as_str()),
            "case {case:?}: expected the create's arn to end in the id it minted, got {created:?}"
        );
        Self {
            region,
            rule_id: rule_id.to_string(),
            id,
            arn,
        }
    }
}

/// Assert the listing `region`'s endpoint answers for `query` reports `identity` under the
/// id its create established, and that the id the entry advertises is accepted as the
/// association's own read.
async fn assert_listed_under_established_identity(
    router: &Router,
    query: &str,
    identity: &Identity,
    case: &str,
) {
    let page: Listing = listing(router, identity.region, query, case).await;
    let entry = entry_with_arn(&page.entries, &identity.arn, case);
    assert_serialized_identity(&page.raw, &entry, &identity.id, &identity.arn, case);
    assert_eq!(
        text(&entry, "ruleId", case),
        identity.rule_id,
        "case {case:?}: expected the entry to name the rule it was made for, got {entry:?}"
    );

    // The handle is taken out of the LISTING, as a list-then-act client takes it, and has
    // to address the association the listing reported it for.
    let listed = text(&entry, "associationId", case);
    let (status, body) = call_in(
        router,
        identity.region,
        "GET",
        &format!("{RULES_PATH}/{}/association/{listed}", identity.rule_id),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the listed handle {listed:?} must resolve, got {status} {body:?}"
    );
    let read = body["ruleAssociation"].clone();
    assert_eq!(
        text(&read, "associationId", case),
        identity.id,
        "case {case:?}: the read must answer under the established identity, got {read:?}"
    );
    assert_eq!(
        text(&read, "arn", case),
        identity.arn,
        "case {case:?}: the read must report the association's own arn, got {read:?}"
    );
}

/// Assert both the unfiltered listing and the `RuleId`-filtered one report `identity`
/// under the id its create established: a filter changes WHICH entries appear, never the
/// identity an entry advertises.
async fn assert_every_listing_reports(router: &Router, identity: &Identity, case: &str) {
    for (query, label) in [
        (
            format!("?maxResults={MAX_RESULTS}"),
            "unfiltered listing".to_string(),
        ),
        (
            format!("?maxResults={MAX_RESULTS}&ruleId={}", identity.rule_id),
            "listing filtered by rule".to_string(),
        ),
    ] {
        assert_listed_under_established_identity(
            router,
            &query,
            identity,
            &format!("{case}: {label}"),
        )
        .await;
    }
}

/// The identity the listing of `region` advertises for the one association of `rule_id`,
/// the way a client that kept no id of its own reaches it.
async fn listed_identity(router: &Router, region: &str, rule_id: &str, case: &str) -> String {
    let page = listing(
        router,
        region,
        &format!("?maxResults={MAX_RESULTS}&ruleId={rule_id}"),
        case,
    )
    .await;
    let entry = page.entries.first().cloned().unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected {rule_id} to be listed in {region}, got {:?}",
            page.entries
        )
    });
    text(&entry, "associationId", case)
}

/// What happens between a create and a listing, each of which either mints an identifier
/// of its own or moves another resource — the traffic an association's identity has to
/// survive.
#[derive(Debug, Clone, Copy)]
enum Neighbouring {
    /// Another rule associated in the same Region.
    AssociateNeighbour,
    /// The SUBJECT's rule associated through the other Region's endpoint: a second
    /// association of the same rule, under an identity of its own.
    AssociateSameRuleElsewhere,
    /// A third rule associated through the other Region's endpoint.
    AssociateThirdElsewhere,
    /// The neighbour's mode moved.
    UpdateNeighbour,
    /// A second create for the subject's own rule, which is the modelled conflict and
    /// mints nothing.
    DuplicateCreateRefused,
    /// The neighbour deleted again.
    DeleteNeighbour,
    /// A detector created — another resource of this service taking a minted id.
    CreateDetector,
}

/// Every neighbouring event, in the order one case walks them: each is applied to the
/// store the subject's association lives in, and the subject's listed identity is asserted
/// again after each.
const NEIGHBOURING: [Neighbouring; 7] = [
    Neighbouring::AssociateNeighbour,
    Neighbouring::AssociateSameRuleElsewhere,
    Neighbouring::AssociateThirdElsewhere,
    Neighbouring::UpdateNeighbour,
    Neighbouring::DuplicateCreateRefused,
    Neighbouring::DeleteNeighbour,
    Neighbouring::CreateDetector,
];

// ---------------------------------------------------------------------------
// Rule: nothing the account does to another resource moves an association's
// established identity.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn neighbouring_traffic_never_moves_an_associations_listed_identity() {
    let router = new_router();
    let rules = published_rule_ids(&router).await;
    let home = REGIONS[0];
    let elsewhere = REGIONS[1];
    let subject_rule = rules[0].clone();
    let neighbour_rule = rules[1].clone();
    let third_rule = rules[2].clone();

    let subject = Identity::established(
        &router,
        home,
        &subject_rule,
        MODES[0],
        "the subject's create",
    )
    .await;
    assert_every_listing_reports(&router, &subject, "straight after the create").await;

    // Every identity this case sees, so two associations are asserted never to share one.
    let mut seen: Vec<String> = vec![subject.id.clone()];
    let mut same_rule_elsewhere: Option<Identity> = None;

    for event in NEIGHBOURING {
        let case = format!("after {event:?}");
        match event {
            Neighbouring::AssociateNeighbour => {
                let made =
                    Identity::established(&router, home, &neighbour_rule, MODES[1], &case).await;
                seen.push(made.id);
            }
            Neighbouring::AssociateSameRuleElsewhere => {
                let made =
                    Identity::established(&router, elsewhere, &subject_rule, MODES[1], &case).await;
                seen.push(made.id.clone());
                same_rule_elsewhere = Some(made);
            }
            Neighbouring::AssociateThirdElsewhere => {
                let made =
                    Identity::established(&router, elsewhere, &third_rule, MODES[0], &case).await;
                seen.push(made.id);
            }
            Neighbouring::UpdateNeighbour => {
                // The neighbour's mode moves, so its record is written while the subject's
                // is not touched at all.
                let neighbour_id = listed_identity(&router, home, &neighbour_rule, &case).await;
                let (status, body) = call_in(
                    &router,
                    home,
                    "PUT",
                    &format!("{RULES_PATH}/{neighbour_rule}/association/{neighbour_id}"),
                    Some(json!({"mode": MODES[0]})),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the neighbour's update must succeed, got {status} {body:?}"
                );
            }
            Neighbouring::DuplicateCreateRefused => {
                // A create for a rule this account has already associated is the modelled
                // conflict — and it mints nothing, so the subject keeps the identity its
                // first create established. Asked in the OTHER mode, so the refusal is not
                // a property of the mode the request names.
                let (status, body) = call_in(
                    &router,
                    home,
                    "POST",
                    ASSOCIATIONS_PATH,
                    Some(json!({"ruleId": subject_rule, "mode": MODES[1]})),
                )
                .await;
                assert_eq!(
                    status, 409,
                    "case {case:?}: a duplicate create must be refused, got {status} {body:?}"
                );
                assert_eq!(
                    error_code(&body),
                    CONFLICT,
                    "case {case:?}: expected the {CONFLICT} class, got {body:?}"
                );
            }
            Neighbouring::DeleteNeighbour => {
                let neighbour_id = listed_identity(&router, home, &neighbour_rule, &case).await;
                let (status, body) = call_in(
                    &router,
                    home,
                    "DELETE",
                    &format!("{RULES_PATH}/{neighbour_rule}/association/{neighbour_id}"),
                    None,
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the neighbour's delete must succeed, got {status} {body:?}"
                );
            }
            Neighbouring::CreateDetector => {
                let (status, body) =
                    call_in(&router, home, "POST", "/detector", Some(json!({"enable": true}))).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the detector create must succeed, got {status} {body:?}"
                );
            }
        }

        // The subject is still reported under the identity its own create established —
        // by the whole Region's listing and by the listing narrowed to its rule.
        assert_every_listing_reports(&router, &subject, &case).await;

        // And the same rule's association in the OTHER Region keeps its own identity:
        // the two are distinct resources, so neither listing may report the other's.
        if let Some(other) = &same_rule_elsewhere {
            assert_every_listing_reports(&router, other, &format!("{case}: the other Region"))
                .await;
            let home_page = listing(
                &router,
                home,
                &format!("?maxResults={MAX_RESULTS}"),
                &case,
            )
            .await;
            assert!(
                !home_page
                    .entries
                    .iter()
                    .any(|entry| entry["arn"].as_str() == Some(other.arn.as_str())),
                "case {case:?}: another Region's association must not be reported here, got {:?}",
                home_page.entries
            );
        }
    }

    for (index, id) in seen.iter().enumerate() {
        assert!(
            !seen[index + 1..].contains(id),
            "expected every association to hold an identity of its own, got {seen:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: the identity is the create's, whatever order the account associated in.
// ---------------------------------------------------------------------------

/// Associate `rules` in the order given, through `region`'s endpoint of a store of its
/// own, and answer the identity each create established.
async fn population(router: &Router, region: &'static str, rules: &[String], case: &str) -> Vec<Identity> {
    let mut made: Vec<Identity> = Vec::new();
    for (index, rule_id) in rules.iter().enumerate() {
        let mode = MODES[index % MODES.len()];
        made.push(
            Identity::established(
                router,
                region,
                rule_id,
                mode,
                &format!("{case}: {rule_id}/{mode}"),
            )
            .await,
        );
    }
    made
}

/// The ARN with its `/association/<id>` segment dropped: the part of an association's ARN
/// that is a function of the Region, the account and the rule rather than of the identity
/// a run minted, so two runs can be compared on it.
fn arn_without_identity(arn: &str, case: &str) -> String {
    arn.rsplit_once('/')
        .map(|(head, _)| head.to_string())
        .unwrap_or_else(|| panic!("case {case:?}: expected an arn naming an association, got {arn:?}"))
}

#[tokio::test]
async fn the_order_the_account_associated_in_does_not_decide_which_identity_an_entry_gets() {
    let catalogue = published_rule_ids(&new_router()).await;
    let region = REGIONS[0];

    // The same population, made in opposite orders over two independent stores: the
    // characters each run mints are its own, so what is asserted is the BINDING — each
    // entry reports the identity ITS create established.
    let forward: Vec<String> = catalogue.iter().take(3).cloned().collect();
    let reverse: Vec<String> = forward.iter().rev().cloned().collect();

    let mut runs: Vec<Vec<Identity>> = Vec::new();
    for (order, rules) in [("forward", &forward), ("reverse", &reverse)] {
        let case = format!("{order} order");
        let router = new_router();
        let made = population(&router, region, rules, &case).await;

        for identity in &made {
            assert_every_listing_reports(&router, identity, &case).await;
        }

        // Within one run every association holds an identity of its own.
        let ids: Vec<&String> = made.iter().map(|identity| &identity.id).collect();
        for (index, id) in ids.iter().enumerate() {
            assert!(
                !ids[index + 1..].contains(id),
                "case {case:?}: expected every association to hold an identity of its own, got {ids:?}"
            );
        }
        runs.push(made);
    }

    // Across the two runs the entries agree on everything that is NOT the minted
    // identity: the rule each association binds and the ARN path that carries it. The
    // identities themselves are per-run values, so nothing here compares their
    // characters — only that each entry carries the one its own create reported, which
    // the listings above already asserted.
    for rule_id in &forward {
        let paths: Vec<String> = runs
            .iter()
            .map(|run| {
                let identity = run
                    .iter()
                    .find(|identity| identity.rule_id == *rule_id)
                    .unwrap_or_else(|| panic!("case {rule_id:?}: expected the rule to be associated in every run"));
                arn_without_identity(&identity.arn, rule_id)
            })
            .collect();
        assert!(
            paths.windows(2).all(|pair| pair[0] == pair[1]),
            "case {rule_id:?}: expected the association's arn path to be the same whatever order \
             the account associated in, got {paths:?}"
        );
    }
}
