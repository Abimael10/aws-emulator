// SPDX-License-Identifier: CC0-1.0

//! The domain tagging ROUND TRIP, end to end over the real HTTP surface.
//!
//! The rule under test: a domain's tags are ONE stored set, and every operation that
//! projects a domain or addresses it through the shared `/tags/{resourceArn}` surface
//! answers from that set. `CreateDomain` stores the map it was given and reports it;
//! `TagResource` unions a request's entries onto it; `UntagResource` removes exactly the
//! keys it names; and `GetDomain` and `ListTagsForResource` both report the current set at
//! every point, with the map PRESENT and empty for a domain carrying no tags — while the
//! domain's genuinely unset optional members stay absent, which is what makes the always
//! present tag map a distinction a caller can read.
//!
//! The rows span what the rule generalises over: HOW MANY tags the domain's own create
//! stored — none at all (no member in the request), one, several, and one short of
//! `TagMap`'s modelled maximum, so the key the later write adds lands exactly on that
//! maximum. The HTTP plumbing is this crate's one test harness
//! (`tests/all/domain_profile_lifecycle.rs`) and the tagging calls are the ones
//! `tests/all/resource_tagging.rs` spells.

use std::collections::BTreeMap;

use serde_json::json;

use service_com_amazonaws_customerprofiles_customerprofiles_20200815::state::MAX_TAGS_PER_RESOURCE;

use crate::domain_profile_lifecycle::Emulator;
use crate::resource_tagging::{
    arn, assert_no_content, expected, reported, tag, tags_of, untag,
};

/// The optional domain members that stay ABSENT until something sets them — the contrast
/// the always-present tag map is read against. Each is an optional member of
/// `GetDomainResponse`
/// (`smithy_query(query="operation", operation="GetDomain")`) that none of these requests
/// configures.
const UNSET_MEMBERS: [&str; 4] = [
    "DefaultEncryptionKey",
    "DeadLetterQueueUrl",
    "Matching",
    "RuleBasedMatching",
];

/// `GetDomain` must report `Tags` present and equal to `wanted`, and must leave every
/// member nothing configured absent.
async fn assert_domain_reports(
    emulator: &Emulator,
    case: &str,
    at: &str,
    domain: &str,
    wanted: &BTreeMap<String, String>,
) {
    let (status, read) = emulator
        .send("GET", &format!("/domains/{domain}"), None)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?} ({at}): expected GetDomain 200, got {status} {read:?}"
    );
    assert!(
        read.get("Tags").is_some(),
        "case {case:?} ({at}): expected Tags present (empty when the domain carries none), got {read:?}"
    );
    assert_eq!(
        &tags_of(&read, "Tags"),
        wanted,
        "case {case:?} ({at}): expected GetDomain to report {wanted:?}, got {:?}",
        read.get("Tags")
    );
    for member in UNSET_MEMBERS {
        assert!(
            read.get(member).is_none(),
            "case {case:?} ({at}): expected {member} to stay absent, got {:?}",
            read.get(member)
        );
    }
}

/// The round trip itself: what each write stored is what both readers report, at every
/// point of the sequence.
#[tokio::test]
async fn a_domains_tags_round_trip_through_the_domain_and_the_tagging_surface() {
    let emulator = Emulator::new();

    // One short of the modelled maximum, so the key `TagResource` adds below lands exactly
    // on it rather than past it.
    let nearly_full: BTreeMap<String, String> = (0..MAX_TAGS_PER_RESOURCE - 1)
        .map(|index| (format!("k{index}"), format!("v{index}")))
        .collect();

    // (case, the domain's name, the tags its create stored)
    let rows: Vec<(&str, &str, BTreeMap<String, String>)> = vec![
        ("a create carrying no Tags member", "rtnone", BTreeMap::new()),
        (
            "a create carrying one tag",
            "rtone",
            expected(&[("env", "dev")]),
        ),
        (
            "a create carrying several tags",
            "rtseveral",
            expected(&[("env", "dev"), ("team", "crm"), ("owner", "ada")]),
        ),
        (
            "a create one short of the modelled maximum",
            "rtmax",
            nearly_full,
        ),
    ];

    for (case, domain, at_create) in rows {
        let mut request = json!({ "DefaultExpirationDays": 366 });
        if !at_create.is_empty() {
            // `TagMap` holds at least one entry (its modelled `@length` is 1..50), so a
            // domain carrying no tags carries no member at all in the request.
            request["Tags"] = json!(at_create);
        }
        let (status, created) = emulator.post(&format!("/domains/{domain}"), request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateDomain 200, got {status} {created:?}"
        );
        // The create's OWN response reports the set it stored, present and empty when the
        // request carried none.
        assert!(
            created.get("Tags").is_some(),
            "case {case:?}: expected CreateDomain to report Tags present, got {created:?}"
        );
        assert_eq!(
            &tags_of(&created, "Tags"),
            &at_create,
            "case {case:?}: expected CreateDomain to report {at_create:?}, got {:?}",
            created.get("Tags")
        );

        let resource_arn = arn(&format!("domains/{domain}"));
        assert_domain_reports(&emulator, case, "at create", domain, &at_create).await;
        assert_eq!(
            reported(&emulator, case, &resource_arn).await,
            at_create,
            "case {case:?}: expected the report to answer from what the create stored"
        );

        // A tag applied after create is unioned onto the stored set, and reaches both
        // readers.
        let (status, body) = tag(&emulator, &resource_arn, json!({ "stage": "probe" })).await;
        assert_no_content(case, "TagResource", status, &body);
        let mut after_tag = at_create.clone();
        after_tag.insert("stage".to_string(), "probe".to_string());
        assert_domain_reports(&emulator, case, "after TagResource", domain, &after_tag).await;
        assert_eq!(
            reported(&emulator, case, &resource_arn).await,
            after_tag,
            "case {case:?}: expected the report to carry the added key beside the stored set"
        );

        // The removal is scoped to the keys it names: the set the create stored survives,
        // and a key the domain never carried removes nothing.
        let (status, body) = untag(&emulator, &resource_arn, &["stage", "never-set"]).await;
        assert_no_content(case, "UntagResource", status, &body);
        assert_domain_reports(&emulator, case, "after UntagResource", domain, &at_create).await;
        assert_eq!(
            reported(&emulator, case, &resource_arn).await,
            at_create,
            "case {case:?}: expected only the named keys removed"
        );

        // Reading changed nothing: both readers answer identically again.
        assert_domain_reports(&emulator, case, "read twice", domain, &at_create).await;
        assert_eq!(
            reported(&emulator, case, &resource_arn).await,
            at_create,
            "case {case:?}: expected two reports of an unchanged domain to be identical"
        );
    }
}
