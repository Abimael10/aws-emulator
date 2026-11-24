// SPDX-License-Identifier: CC0-1.0

//! What `ListIdentityProviderConfigs` reports, pinned over a TABLE of inputs.
//!
//! RULE: the listing reports EXACTLY the identity-provider configurations the named
//! cluster holds in the Region the request is signed for — every one of them,
//! whatever status it is in, as the `(type, name)` pair
//! `DescribeIdentityProviderConfig` resolves one by — in pages of at most the
//! `maxResults` the request asks for, resumed through `nextToken`, with a page size
//! outside the modelled range refused before the handler runs.
//!
//! The rows differ along what the rule generalises over: the Region a cluster of the
//! same name lives in, the page size (each published bound, the sizes between them,
//! and one past each bound), and the point of a configuration's lifecycle the listing
//! is taken at (associating, active, deleting, removed).
//!
//! `identity_provider_config` already covers the whole-page / paged walk of one
//! Region's cluster; this suite adds the three the rule also spans and that one does
//! not: the Region scope, the published bounds of `maxResults`, and the listing's
//! agreement with the reads through a configuration's whole lifecycle.
//!
//! The page-size bounds are read through the crate's published
//! `state::MIN_LIST_MAX_RESULTS` / `state::MAX_LIST_MAX_RESULTS`, which carry the
//! model's own range for every `maxResults` of this API, so no row here writes a
//! bound out.
//!
//! Every wait here is for the CONDITION, through `wire`'s poll-until helper, so a
//! loaded host only makes the suite slower. The request harness is `wire`'s, and the
//! cluster setup and identity-provider endpoint drivers are the ones
//! `identity_provider_config_not_found` owns.

use std::collections::BTreeSet;

use service_com_amazonaws_eks_awswesleyfrontend::state::{
    max_results_range_message, MAX_LIST_MAX_RESULTS, MIN_LIST_MAX_RESULTS,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::REGION;
// The cluster setup and the two identity-provider endpoint drivers are the
// `pub(crate)` ones `identity_provider_config_not_found` owns, so this suite stands a
// cluster up and drives those endpoints the way every other suite does rather than
// keeping a second copy of them.
use crate::identity_provider_config_not_found::{
    active_cluster_in, associate_in, describe_config_in,
};
use crate::wire::{call, emulator, encode_label, error_code, read_until, Answer};

/// A second Region, so a cluster name can name a different cluster in each.
const OTHER_REGION: &str = "eu-west-1";

/// The one kind of identity provider this API defines (`IdentityProviderConfig$type`,
/// "the only type available is `oidc`").
const OIDC: &str = "oidc";

async fn disassociate_in(
    router: &MultiServiceRouter,
    region: &str,
    cluster: &str,
    name: &str,
) -> Answer {
    let body = serde_json::json!({
        "identityProviderConfig": { "type": OIDC, "name": name },
    });
    call(
        router,
        region,
        "POST",
        &format!(
            "/clusters/{}/identity-provider-configs/disassociate",
            encode_label(cluster)
        ),
        &body.to_string(),
    )
    .await
}

async fn list_in(
    router: &MultiServiceRouter,
    region: &str,
    cluster: &str,
    query: &str,
) -> Answer {
    let uri = if query.is_empty() {
        format!(
            "/clusters/{}/identity-provider-configs",
            encode_label(cluster)
        )
    } else {
        format!(
            "/clusters/{}/identity-provider-configs?{query}",
            encode_label(cluster)
        )
    };
    call(router, region, "GET", &uri, "").await
}

/// The `(type, name)` pairs an answer lists, in the order it listed them.
fn listed_pairs(answer: &Answer) -> Vec<(String, String)> {
    answer.body["identityProviderConfigs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            (
                entry["type"].as_str().unwrap_or_default().to_string(),
                entry["name"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The pairs `names` are expected to be listed as, in the order the listing walks
/// them (by name, which is the order a page resumes through).
fn expected_pairs(names: &[&str]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = names
        .iter()
        .map(|name| (OIDC.to_string(), (*name).to_string()))
        .collect();
    pairs.sort();
    pairs
}

/// Associate `name` with `cluster` and look again until the configuration reports
/// `ACTIVE` — which is also when the update that associated it is done, and so when
/// the cluster is free to take the next one.
async fn associated_and_active(
    router: &MultiServiceRouter,
    region: &str,
    cluster: &str,
    name: &str,
) {
    let answer = associate_in(router, region, cluster, name).await;
    assert_eq!(
        answer.status, 200,
        "setup {name:?} on {cluster:?} in {region:?}: expected 200 from the association, \
         got {answer:?}"
    );
    read_until(
        name,
        "the configuration the clock owes ACTIVE became ACTIVE",
        || describe_config_in(router, region, cluster, name),
        |read| {
            read.body["identityProviderConfig"]["oidc"]["status"]
                == serde_json::json!("ACTIVE")
        },
    )
    .await;
}

// ---------------------------------------------------------------------------
// RULE (the Region half): a cluster name identifies a cluster only within one
// Region, so the listing reports the configurations of the cluster THAT Region
// holds — never another Region's cluster of the same name.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_listing_reports_only_the_configurations_of_this_regions_cluster() {
    let router = emulator();
    // ONE cluster name, two Regions, different configurations in each — so a listing
    // that ignored the Region would have to report the other Region's names.
    let cluster = "idp-scoped";
    active_cluster_in(&router, REGION, cluster).await;
    active_cluster_in(&router, OTHER_REGION, cluster).await;

    let here = ["home-provider", "shared-provider"];
    let there = ["away-provider"];
    for name in here {
        associated_and_active(&router, REGION, cluster, name).await;
    }
    for name in there {
        associated_and_active(&router, OTHER_REGION, cluster, name).await;
    }

    for (case, region, expected) in [
        ("the Region holding two", REGION, &here[..]),
        ("the Region holding one", OTHER_REGION, &there[..]),
    ] {
        let listed = list_in(&router, region, cluster, "").await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected 200, got {listed:?}"
        );
        assert_eq!(
            listed_pairs(&listed),
            expected_pairs(expected),
            "case {case:?}: expected exactly that Region's configurations, got {listed:?}"
        );
        assert_eq!(
            listed.body["nextToken"],
            serde_json::Value::Null,
            "case {case:?}: a complete page carries no token, got {:?}",
            listed.body["nextToken"]
        );
        // Each listed pair resolves through the read IN THE SAME REGION, so the
        // listing and the reads a caller then makes cannot disagree.
        for (_, name) in listed_pairs(&listed) {
            let read = describe_config_in(&router, region, cluster, &name).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the listed pair {name:?} must resolve through the read, \
                 got {read:?}"
            );
        }
    }

    // And the other Region's name is not a resource of this one: the listing did not
    // report it, and the read of it is a 404.
    let foreign = describe_config_in(&router, REGION, cluster, there[0]).await;
    assert_eq!(
        foreign.status, 404,
        "case \"another Region's configuration\": expected HTTP 404, got {foreign:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE (the page-size half): a page carries at most the `maxResults` asked for, the
// pages concatenate to the whole listing exactly once each, and a size outside the
// modelled range is refused before the handler runs.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_page_size_the_model_admits_walks_the_whole_listing_once() {
    let router = emulator();
    let cluster = "idp-paged";
    active_cluster_in(&router, REGION, cluster).await;

    // Names that differ in length and whose creation order is NOT their listing
    // order, so a page boundary is about the listing's own order rather than about
    // when each was associated.
    let names = ["zeta", "alpha-provider", "mid-provider", "beta"];
    for name in names {
        associated_and_active(&router, REGION, cluster, name).await;
    }
    let expected = expected_pairs(&names);

    // Rows: the published lower bound, every size between it and the number stored,
    // one above that number, and the published upper bound — the bounds read through
    // the crate's published constants rather than written out, so a row cannot drift
    // away from the range the model declares for `maxResults`.
    for page_size in [MIN_LIST_MAX_RESULTS, 2, 3, 4, 5, MAX_LIST_MAX_RESULTS] {
        let mut walked: Vec<(String, String)> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let query = match &token {
                None => format!("maxResults={page_size}"),
                Some(value) => format!("maxResults={page_size}&nextToken={value}"),
            };
            let page = list_in(&router, REGION, cluster, &query).await;
            assert_eq!(
                page.status, 200,
                "case maxResults={page_size}: expected 200, got {page:?}"
            );
            let pairs = listed_pairs(&page);
            assert!(
                pairs.len() <= page_size as usize,
                "case maxResults={page_size}: a page carries at most that many, got {pairs:?}"
            );
            walked.extend(pairs.clone());
            pages += 1;
            assert!(
                pages <= names.len() + 1,
                "case maxResults={page_size}: the walk did not terminate after {pages} pages, \
                 got {walked:?}"
            );
            match page.body["nextToken"].as_str() {
                Some(next) => {
                    assert!(
                        !pairs.is_empty(),
                        "case maxResults={page_size}: a token was returned for an empty page, \
                         got {page:?}"
                    );
                    token = Some(next.to_owned());
                }
                None => break,
            }
        }
        assert_eq!(
            walked, expected,
            "case maxResults={page_size}: the pages must concatenate to the whole listing, \
             got {walked:?}"
        );
        let unique: BTreeSet<&(String, String)> = walked.iter().collect();
        assert_eq!(
            unique.len(),
            walked.len(),
            "case maxResults={page_size}: a configuration was repeated across pages, got \
             {walked:?}"
        );
    }

    // One past each published bound is outside the modelled range, so it is the bad
    // parameter it is rather than a page.
    for page_size in [MIN_LIST_MAX_RESULTS - 1, MAX_LIST_MAX_RESULTS + 1] {
        let answer = list_in(&router, REGION, cluster, &format!("maxResults={page_size}")).await;
        assert_eq!(
            answer.status, 400,
            "case maxResults={page_size}: outside the modelled range expected HTTP 400, got \
             {answer:?}"
        );
        let code = error_code(&answer);
        assert_eq!(
            code, "InvalidParameterException",
            "case maxResults={page_size}: expected InvalidParameterException, got {code:?}"
        );
        // The sentence the whole service refuses a page size outside those bounds
        // with: fixed text whose two numbers are the bounds themselves, naming no
        // resource of the request.
        assert_eq!(
            answer.message(),
            max_results_range_message(),
            "case maxResults={page_size}: expected the range sentence, got {:?}",
            answer.message()
        );
    }
}

// ---------------------------------------------------------------------------
// RULE (the lifecycle half): the listing reports what the cluster HOLDS, in whatever
// status — a configuration is listed from the instant it is associated until the
// clock has completed the update that removes it, and the reads agree at every point.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_listing_follows_a_configurations_whole_lifecycle() {
    let router = emulator();
    let cluster = "idp-lifecycle";
    active_cluster_in(&router, REGION, cluster).await;

    // A configuration that stays put for the whole test, so every assertion below is
    // about the one being changed rather than about the listing emptying out.
    associated_and_active(&router, REGION, cluster, "kept-provider").await;

    // ASSOCIATING: listed from the instant the association is accepted, before the
    // clock has made it ACTIVE.
    let started = associate_in(&router, REGION, cluster, "moving-provider").await;
    assert_eq!(
        started.status, 200,
        "case \"the association\": expected 200, got {started:?}"
    );
    let listed = list_in(&router, REGION, cluster, "").await;
    assert_eq!(
        listed_pairs(&listed),
        expected_pairs(&["kept-provider", "moving-provider"]),
        "case \"while the association runs\": a configuration is listed from the instant it \
         is associated, got {listed:?}"
    );

    // ACTIVE: still listed once the clock has settled it.
    read_until(
        "moving-provider",
        "the configuration the clock owes ACTIVE became ACTIVE",
        || describe_config_in(&router, REGION, cluster, "moving-provider"),
        |read| {
            read.body["identityProviderConfig"]["oidc"]["status"] == serde_json::json!("ACTIVE")
        },
    )
    .await;
    let listed = list_in(&router, REGION, cluster, "").await;
    assert_eq!(
        listed_pairs(&listed),
        expected_pairs(&["kept-provider", "moving-provider"]),
        "case \"once the association settles\": both configurations are still held, got \
         {listed:?}"
    );

    // DELETING: the cluster still HOLDS it while the removal runs, so it is still
    // listed — and the read says it is on its way out.
    let teardown = disassociate_in(&router, REGION, cluster, "moving-provider").await;
    assert_eq!(
        teardown.status, 200,
        "case \"the disassociation\": expected 200, got {teardown:?}"
    );
    // The two must agree: as long as the read still resolves the configuration, the
    // cluster holds it and the listing reports it. Whether the clock has already
    // finished the removal by the time the read is taken is the host's business, so
    // the assertion is on the AGREEMENT rather than on which of the two states it is in.
    let during_read = describe_config_in(&router, REGION, cluster, "moving-provider").await;
    let still_held = during_read.status == 200;
    let during_list = list_in(&router, REGION, cluster, "").await;
    let expected_during: Vec<(String, String)> = if still_held {
        expected_pairs(&["kept-provider", "moving-provider"])
    } else {
        expected_pairs(&["kept-provider"])
    };
    assert_eq!(
        listed_pairs(&during_list),
        expected_during,
        "case \"while the removal runs\": the listing must report what the read resolves \
         (read {during_read:?}), got {during_list:?}"
    );

    // REMOVED: once the clock has completed the update the configuration is gone from
    // the cluster, so the listing drops it — and only it.
    read_until(
        "moving-provider",
        "the removal the clock owes finished",
        || describe_config_in(&router, REGION, cluster, "moving-provider"),
        |read| read.status == 404,
    )
    .await;
    let listed = list_in(&router, REGION, cluster, "").await;
    assert_eq!(
        listed.status, 200,
        "case \"once the removal finishes\": expected 200, got {listed:?}"
    );
    assert_eq!(
        listed_pairs(&listed),
        expected_pairs(&["kept-provider"]),
        "case \"once the removal finishes\": only the removed configuration is dropped, got \
         {listed:?}"
    );
    assert_eq!(
        listed.body["nextToken"],
        serde_json::Value::Null,
        "case \"once the removal finishes\": a complete page carries no token, got {:?}",
        listed.body["nextToken"]
    );
}
