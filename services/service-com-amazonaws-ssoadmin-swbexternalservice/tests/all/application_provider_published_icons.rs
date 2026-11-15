// SPDX-License-Identifier: CC0-1.0

//! The application-provider catalog's PUBLISHED data, end to end over the `awsJson1_1`
//! wire: `ListApplicationProviders` and `DescribeApplicationProvider` report what the
//! catalog record holds and nothing it does not.
//!
//! The rules under test:
//!
//! 1. `DisplayData$IconUrl` is per-provider PUBLISHED catalog data, carried on the record
//!    ([`state::BuiltInApplicationProvider::icon_url`]): a read reports it verbatim for an
//!    entry the catalog publishes an icon asset for, and reports NO `IconUrl` member for an
//!    entry it publishes none for (the member is optional in the model:
//!    `smithy_query(query="shape", shape="DisplayData")` — `DisplayName` 1..100, `IconUrl`
//!    1..768 matching `^(http|https)://.*$`, `Description` 1..128, all three optional). It
//!    is never computed from the provider id, so two entries publish two unrelated asset
//!    ids and an entry that publishes none gets no invented one.
//! 2. The catalog is published in its OWN order, and the enumeration reports it in that
//!    order: the entry at a given position is the same entry on every call, and the
//!    discover-then-inspect flow — enumerate, then describe the ARN the enumeration
//!    reported — comes back with THAT entry, member for member, through the one projection
//!    both reads share.
//!
//! The tables span what the rules generalise over: every entry of the catalog (read from
//! the crate so the test tracks it rather than restating it), entries that publish an icon
//! and entries that do not, both id forms (`catalog/<Name>` and a service slug), the
//! positions a client picks an entry by (the head of the catalog and the last entry of a
//! page), and entries the recorded cases cite (`custom`, `catalog/Statuspage`) beside
//! entries they do not (`glue`, `catalog/Zoom`, `athena`).
//!
//! Every call goes through `MultiServiceRouter::dispatch`, so the modelled constraints and
//! the `X-Amz-Target` routing are exercised rather than bypassed. The harness is the one
//! `ssoadmin_permission_sets.rs` owns.

use serde_json::{json, Value};
use service_com_amazonaws_ssoadmin_swbexternalservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::ssoadmin_permission_sets::{call, router};

/// The members a listed entry and a described provider must agree on: one catalog, two
/// projections.
const PROVIDER_MEMBERS: &[&str] = &[
    "ApplicationProviderArn",
    "FederationProtocol",
    "DisplayData",
    "ResourceServerConfig",
];

/// The fixed text of a provider's published portal icon URL, around the one variable part
/// the catalog record carries for that entry.
///
/// SOURCE: recorded `DescribeApplicationProvider` behavior — a provider's `IconUrl` is
/// `https://static.global.sso.amazonaws.com/app-<the entry's published asset id>/icons/default.png`.
/// Stated here so the expectation is the published FORM and the record supplies only the
/// asset id, rather than the production helper being compared against itself.
const ICON_URL_PREFIX: &str = "https://static.global.sso.amazonaws.com/app-";
const ICON_URL_SUFFIX: &str = "/icons/default.png";

/// The portal icon URL the catalog publishes for `provider`, composed from the record's
/// asset id, or `None` for an entry the catalog publishes no icon asset for.
fn published_icon_url(provider: &state::BuiltInApplicationProvider) -> Option<String> {
    provider
        .icon_asset_id
        .map(|asset_id| format!("{ICON_URL_PREFIX}{asset_id}{ICON_URL_SUFFIX}"))
}

/// The ARN of the built-in provider `id` in the partition the emulator serves.
fn provider_arn(id: &str) -> String {
    format!("arn:aws:sso::aws:applicationProvider/{id}")
}

/// One `DescribeApplicationProvider` call: `(status, body)`.
async fn describe(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    call(
        router,
        "DescribeApplicationProvider",
        &json!({"ApplicationProviderArn": arn}),
    )
    .await
}

/// One page of `ListApplicationProviders`, `MaxResults` entries at a time when one is
/// named.
async fn list_page(router: &MultiServiceRouter, max_results: Option<i64>) -> Vec<Value> {
    let mut request = json!({});
    if let Some(size) = max_results {
        request["MaxResults"] = json!(size);
    }
    let (status, body) = call(router, "ListApplicationProviders", &request).await;
    assert_eq!(
        status, 200,
        "case {max_results:?}: ListApplicationProviders expected 200, got {status} {body}"
    );
    body["ApplicationProviders"]
        .as_array()
        .unwrap_or_else(|| panic!("case {max_results:?}: no ApplicationProviders in {body}"))
        .clone()
}

/// Every entry of the catalog, by paging the enumeration to its end.
async fn list_all(router: &MultiServiceRouter) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    let mut next_token: Option<String> = None;
    loop {
        let mut request = json!({});
        if let Some(token) = next_token.as_deref() {
            request["NextToken"] = json!(token);
        }
        let (status, body) = call(router, "ListApplicationProviders", &request).await;
        assert_eq!(
            status, 200,
            "ListApplicationProviders expected 200, got {status} {body}"
        );
        entries.extend(
            body["ApplicationProviders"]
                .as_array()
                .unwrap_or_else(|| panic!("no ApplicationProviders in {body}"))
                .iter()
                .cloned(),
        );
        assert!(
            entries.len() <= state::BUILT_IN_APPLICATION_PROVIDERS.len(),
            "paging must not report more than the catalog, got {} entries",
            entries.len()
        );
        match body["NextToken"].as_str() {
            Some(token) => next_token = Some(token.to_string()),
            None => break,
        }
    }
    entries
}

/// RULE 1 — over EVERY entry of the catalog, on both reads: `IconUrl` is reported exactly
/// when the record publishes an icon asset, and then it is the record's published URL
/// verbatim.
#[tokio::test]
async fn an_icon_url_is_reported_exactly_for_the_entries_that_publish_one() {
    let router = router();
    let listed = list_all(&router).await;
    let mut published = 0usize;
    let mut unpublished = 0usize;

    for provider in state::BUILT_IN_APPLICATION_PROVIDERS {
        let case = provider.id;
        let arn = provider_arn(case);
        match published_icon_url(provider) {
            Some(_) => published += 1,
            None => unpublished += 1,
        }
        let entry = listed
            .iter()
            .find(|entry| entry["ApplicationProviderArn"].as_str() == Some(arn.as_str()))
            .unwrap_or_else(|| panic!("case {case:?}: the enumeration must list {arn}"));
        let (status, described) = describe(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeApplicationProvider expected 200, got {status} {described}"
        );

        for read in [&entry["DisplayData"], &described["DisplayData"]] {
            match published_icon_url(provider) {
                Some(expected) => {
                    assert_eq!(
                        read["IconUrl"].as_str(),
                        Some(expected.as_str()),
                        "case {case:?}: expected the published IconUrl {expected:?}, got {read}"
                    );
                    assert!(
                        (1..=768).contains(&expected.chars().count())
                            && expected.starts_with("https://"),
                        "case {case:?}: a published IconUrl is 1..768 characters matching \
                         ^(http|https)://.*$, got {expected:?}"
                    );
                }
                None => {
                    assert!(
                        read["IconUrl"].is_null(),
                        "case {case:?}: the catalog publishes no icon for this entry, so no \
                         IconUrl may be reported, got {read}"
                    );
                }
            }
            // Whatever the icon, the rest of the display data is the record's own.
            assert_eq!(
                read["DisplayName"].as_str(),
                Some(provider.display_name),
                "case {case:?}: expected DisplayName {:?}, got {read}",
                provider.display_name
            );
            assert_eq!(
                read["Description"].as_str(),
                Some(provider.description().as_str()),
                "case {case:?}: expected Description {:?}, got {read}",
                provider.description()
            );
        }
    }

    // The table really did span both sides of the rule.
    assert!(
        published > 0,
        "the catalog must publish an icon for at least one entry, got {published}"
    );
    assert!(
        unpublished > 0,
        "the catalog must publish no icon for at least one entry, got {unpublished}"
    );
}

/// RULE 1 — a published icon URL is the ENTRY's own data: the asset id comes from the
/// record, not from the provider id, so entries whose ids share a prefix or a form carry
/// unrelated asset ids and an entry with no published asset carries none.
///
/// The rows name the entries the recorded cases cite and entries they do not.
#[tokio::test]
async fn a_published_icon_url_carries_the_entrys_own_asset_id() {
    let router = router();

    // SOURCE: recorded `DescribeApplicationProvider` behavior — see [`ICON_URL_PREFIX`]:
    // the asset id is the entry's own published data. The rows are read from the crate's
    // catalog, so this test states the FORM and the record states the value.

    let mut asset_ids: Vec<String> = Vec::new();
    for id in [
        "custom",
        "catalog/Statuspage",
        "glue",
        "athena",
        "catalog/Zoom",
        "catalog/15Five",
    ] {
        let arn = provider_arn(id);
        let record = state::built_in_application_provider(&arn)
            .unwrap_or_else(|| panic!("case {id:?}: the catalog must hold {arn}"));
        let (status, body) = describe(&router, &arn).await;
        assert_eq!(status, 200, "case {id:?}: expected 200, got {status} {body}");
        let reported = body["DisplayData"]["IconUrl"].as_str();

        match record.icon_asset_id {
            Some(asset_id) => {
                let expected = format!("{ICON_URL_PREFIX}{asset_id}{ICON_URL_SUFFIX}");
                assert_eq!(
                    reported,
                    Some(expected.as_str()),
                    "case {id:?}: expected the entry's published icon {expected:?}, got \
                     {reported:?}"
                );
                asset_ids.push(asset_id.to_string());
            }
            None => assert_eq!(
                reported, None,
                "case {id:?}: the catalog publishes no icon asset for this entry, so none \
                 may be reported, got {reported:?}"
            ),
        }
        // The whole-provider envelope reports the same icon as the flat member.
        assert_eq!(
            body["ApplicationProvider"]["DisplayData"]["IconUrl"].as_str(),
            reported,
            "case {id:?}: the reported provider must carry the same icon as the flat \
             member, got {body}"
        );
    }

    assert!(
        asset_ids.len() > 1,
        "the rows must cover more than one published asset id for the per-entry rule to be \
         observable, got {asset_ids:?}"
    );
    let mut distinct = asset_ids.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        asset_ids.len(),
        "each entry publishes its own asset id, got {asset_ids:?}"
    );
}

/// RULE 2 — the enumeration reports the catalog in the catalog's own published order, and
/// describing the ARN a position reported comes back with THAT entry: the
/// discover-then-inspect flow. The rows are the positions a client picks an entry by.
#[tokio::test]
async fn a_listed_position_round_trips_through_describe() {
    let router = router();
    let catalog = state::BUILT_IN_APPLICATION_PROVIDERS;
    assert!(
        catalog.len() > 3,
        "the catalog must hold more than three entries for the position rows to differ"
    );

    // A page of the first entries, and a page of one: the position a client reads an entry
    // from must name the same entry whatever the page size.
    let head = list_page(&router, Some(4)).await;
    let single = list_page(&router, Some(1)).await;
    assert_eq!(
        single.first(),
        head.first(),
        "the first entry of the catalog is the same entry whatever the page size, got {:?} \
         vs {:?}",
        single.first(),
        head.first()
    );

    for (index, entry) in head.iter().enumerate() {
        let expected = &catalog[index];
        let arn = provider_arn(expected.id);
        assert_eq!(
            entry["ApplicationProviderArn"].as_str(),
            Some(arn.as_str()),
            "case {index}: the enumeration reports the catalog's own order, so position \
             {index} is {arn}, got {entry}"
        );

        let (status, described) = describe(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {index}: DescribeApplicationProvider expected 200, got {status} {described}"
        );
        for member in PROVIDER_MEMBERS {
            assert_eq!(
                entry[*member], described[*member],
                "case {index}: member {member} of the listed entry must be the described \
                 one, got {:?} vs {:?}",
                entry[*member], described[*member]
            );
        }
        assert_eq!(
            described["FederationProtocol"].as_str(),
            Some(expected.federation_protocol),
            "case {index}: expected the record's FederationProtocol {:?}, got {described}",
            expected.federation_protocol
        );
    }

    // Reading the catalog twice reports the identical order: the order is data, not
    // something a read imposes.
    let again = list_page(&router, Some(4)).await;
    assert_eq!(
        head, again,
        "two reads of the catalog must report the identical entries in the identical order"
    );
}
