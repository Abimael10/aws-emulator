// SPDX-License-Identifier: CC0-1.0

//! The application-provider catalog's PUBLISHED icons, end to end over the `awsJson1_1`
//! wire and over the address those icons are fetched at: `ListApplicationProviders` and
//! `DescribeApplicationProvider` report where a client fetches an entry's portal icon, and
//! this emulator answers there.
//!
//! The rules under test:
//!
//! 1. `DisplayData$IconUrl` is reported for exactly the entries the catalogue publishes an
//!    icon for ([`state::BuiltInApplicationProvider::publishes_icon`]) and for no other (the
//!    member is optional in the model: `smithy_query(query="shape", shape="DisplayData")` —
//!    `DisplayName` 1..100, `IconUrl` 1..768 matching `^(http|https)://.*$`, `Description`
//!    1..128, all three optional).
//! 2. A reported `IconUrl` leads back to THE ENDPOINT THE REQUEST ARRIVED AT: its scheme and
//!    authority are the endpoint's, never a host this emulator does not serve, and the path
//!    names the entry whose icon it is. A client following it reaches this emulator and is
//!    answered the entry's icon — a real PNG under the media type it is announced with —
//!    while an address naming an entry that publishes no icon, or no catalogue entry at all,
//!    is answered "nothing here".
//! 3. The catalogue is published in its OWN order, and the enumeration reports it in that
//!    order: the entry at a given position is the same entry on every call, and the
//!    discover-then-inspect flow — enumerate, then describe the ARN the enumeration
//!    reported — comes back with THAT entry, member for member, through the one projection
//!    both reads share.
//!
//! The tables span what the rules generalise over: every entry of the catalogue (read from
//! the crate so the test tracks it rather than restating it), entries that publish an icon
//! and entries that do not, both id forms (`catalog/<Name>` and a service slug), the
//! positions a client picks an entry by (the head of the catalogue and the last entry of a
//! page), and entries the recorded cases cite (`custom`, `catalog/Statuspage`) beside
//! entries they do not (`glue`, `catalog/Zoom`, `athena`).
//!
//! Every call goes through `MultiServiceRouter::dispatch`, so the modelled constraints and
//! the `X-Amz-Target` routing are exercised rather than bypassed — and so is the anonymous
//! fetch of an icon, which carries no credential scope and no `X-Amz-Target` at all. The
//! harness is the one `ssoadmin_permission_sets.rs` owns.

use serde_json::{json, Value};
use service_com_amazonaws_ssoadmin_swbexternalservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::application_providers::EMULATOR_BASE;
use crate::ssoadmin_permission_sets::{call, router};

/// The members a listed entry and a described provider must agree on: one catalog, two
/// projections.
const PROVIDER_MEMBERS: &[&str] = &[
    "ApplicationProviderArn",
    "FederationProtocol",
    "DisplayData",
    "ResourceServerConfig",
];

/// The first eight bytes of every PNG file (RFC 2083 section 3.1): what an icon fetch must
/// answer, stated here so the expectation is the FORMAT and not the renderer compared
/// against itself.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

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

/// One ANONYMOUS fetch of an address a provider read handed out — no SigV4, no
/// `X-Amz-Target`, exactly what a browser rendering a portal tile sends — as
/// `(status, media type, body)`.
///
/// The URL's own path is what is dispatched: a client of the emulator reaches this process
/// at the endpoint the URL names, so the path is what identifies the asset.
async fn fetch(
    router: &MultiServiceRouter,
    method: &str,
    url: &str,
    case: &str,
) -> (u16, String, Vec<u8>) {
    let path = url
        .split_once("://")
        .and_then(|(_scheme, rest)| rest.find('/').map(|at| rest[at..].to_string()))
        .unwrap_or_else(|| panic!("case {case:?}: expected a URL with a path, got {url:?}"));
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let media_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (status, media_type, response.into_body().to_vec())
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
/// when the catalogue publishes an icon for the entry, and then it is the address this
/// emulator serves that entry's icon at.
#[tokio::test]
async fn an_icon_url_is_reported_exactly_for_the_entries_that_publish_one() {
    let router = router();
    let listed = list_all(&router).await;
    let mut published = 0usize;
    let mut unpublished = 0usize;

    for provider in state::BUILT_IN_APPLICATION_PROVIDERS {
        let case = provider.id;
        let arn = provider_arn(case);
        match provider.publishes_icon {
            true => published += 1,
            false => unpublished += 1,
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
            match provider.icon_url(EMULATOR_BASE) {
                Some(expected) => {
                    assert_eq!(
                        read["IconUrl"].as_str(),
                        Some(expected.as_str()),
                        "case {case:?}: expected the IconUrl {expected:?}, got {read}"
                    );
                    assert!(
                        (1..=768).contains(&expected.chars().count())
                            && expected.starts_with("https://"),
                        "case {case:?}: an IconUrl is 1..768 characters matching \
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

/// RULE 2 — every `IconUrl` a read reports is an address on THIS EMULATOR'S endpoint,
/// naming the entry whose icon it is: no entry of the catalogue, whichever id form it
/// carries, is reported with an address on a host this emulator does not serve.
///
/// Asserted over every entry the enumeration reports, which is the table RULE 1's own rows
/// are drawn from.
#[tokio::test]
async fn every_reported_icon_url_is_addressed_on_this_emulators_endpoint() {
    let router = router();
    let listed = list_all(&router).await;
    let mut addressed = 0usize;

    for entry in &listed {
        let arn = entry["ApplicationProviderArn"].as_str().unwrap_or_default();
        let Some(icon_url) = entry["DisplayData"]["IconUrl"].as_str() else {
            continue;
        };
        addressed += 1;
        let id = arn
            .split_once(":sso::aws:applicationProvider/")
            .map(|(_prefix, id)| id)
            .unwrap_or_else(|| panic!("case {arn:?}: expected a provider ARN"));
        assert_eq!(
            icon_url,
            state::provider_icon_url(EMULATOR_BASE, id),
            "case {arn:?}: expected the icon address on the endpoint the read arrived at, got \
             {icon_url:?}"
        );
        // The path this emulator serves the icon on reads the entry back out of the address,
        // so the address and the route cannot name two different entries.
        assert_eq!(
            state::provider_icon_route(icon_url),
            Some(id),
            "case {arn:?}: expected the address to name this entry, got {icon_url:?}"
        );
    }

    assert!(
        addressed > 1,
        "the catalogue must report more than one icon address for this rule to be observable, \
         got {addressed}"
    );
}

/// RULE 2 — a client that FOLLOWS a reported `IconUrl` is answered that entry's icon: a
/// real PNG, under the media type it is announced with, the same bytes on every fetch and a
/// different image per entry. A `HEAD` of the same address reports what the asset is with
/// no body.
///
/// The rows name entries the recorded cases cite (`custom`, `catalog/Statuspage`) and
/// entries they do not (`catalog/DuoNetworkGateway`, `catalog/DruvaInSync`, `catalog/Zoom`),
/// spanning both id forms.
#[tokio::test]
async fn a_reported_icon_url_is_served_by_this_emulator() {
    let router = router();
    let mut images: Vec<Vec<u8>> = Vec::new();

    for id in [
        "custom",
        "catalog/Statuspage",
        "catalog/DuoNetworkGateway",
        "catalog/DruvaInSync",
        "catalog/Zoom",
    ] {
        let arn = provider_arn(id);
        let (status, described) = describe(&router, &arn).await;
        assert_eq!(
            status, 200,
            "case {id:?}: DescribeApplicationProvider expected 200, got {status} {described}"
        );
        let icon_url = described["DisplayData"]["IconUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {id:?}: expected a reported IconUrl, got {described}"))
            .to_string();

        let (status, media_type, body) = fetch(&router, "GET", &icon_url, id).await;
        assert_eq!(
            status, 200,
            "case {id:?}: fetching {icon_url:?} expected 200, got {status} with {} bytes",
            body.len()
        );
        assert_eq!(
            media_type,
            state::PROVIDER_ICON_MEDIA_TYPE,
            "case {id:?}: expected the icon announced as {:?}, got {media_type:?}",
            state::PROVIDER_ICON_MEDIA_TYPE
        );
        assert!(
            body.starts_with(&PNG_SIGNATURE),
            "case {id:?}: expected PNG bytes, got {} bytes starting {:?}",
            body.len(),
            body.get(..PNG_SIGNATURE.len())
        );

        // The same address answers the same image on a later fetch: the icon is the
        // catalogue's data, not something a fetch invents.
        let (_, _, again) = fetch(&router, "GET", &icon_url, id).await;
        assert_eq!(
            again, body,
            "case {id:?}: every fetch of one address must answer the same image"
        );
        assert!(
            !images.contains(&body),
            "case {id:?}: each entry must be served an image of its own"
        );
        images.push(body.clone());

        // A client asking only WHAT the asset is gets its type and its size, and no body.
        let (status, media_type, probed) = fetch(&router, "HEAD", &icon_url, id).await;
        assert_eq!(
            status, 200,
            "case {id:?}: a HEAD of {icon_url:?} expected 200, got {status}"
        );
        assert_eq!(
            media_type,
            state::PROVIDER_ICON_MEDIA_TYPE,
            "case {id:?}: a HEAD must announce the asset's media type, got {media_type:?}"
        );
        assert!(
            probed.is_empty(),
            "case {id:?}: a HEAD answers no body, got {} bytes",
            probed.len()
        );
    }

    assert!(
        images.len() > 1,
        "the rows must cover more than one served icon for the per-entry rule to be \
         observable, got {}",
        images.len()
    );
}

/// RULE 2 — an address that names no icon of this catalogue is answered "nothing here", and
/// nothing is invented for it.
///
/// The rows span the ways such an address can fail: an entry the catalogue publishes no icon
/// for, ids the catalogue does not hold in either form, and a path of the right shape under
/// no provider at all.
#[tokio::test]
async fn an_address_naming_no_published_icon_is_answered_nothing() {
    let router = router();

    let unpublished = state::BUILT_IN_APPLICATION_PROVIDERS
        .iter()
        .find(|provider| !provider.publishes_icon)
        .expect("the catalogue must hold an entry that publishes no icon");
    for case in [
        unpublished.id,
        "catalog/NoSuchApplication",
        "no-such-service",
    ] {
        let url = state::provider_icon_url(EMULATOR_BASE, case);
        let (status, _media_type, body) = fetch(&router, "GET", &url, case).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 for {url:?}, got {status} with {} bytes",
            body.len()
        );
        assert!(
            !body.starts_with(&PNG_SIGNATURE),
            "case {case:?}: no image may be served for an address naming no published icon"
        );
    }

    // The plane itself, with no entry under it, is not an icon address at all.
    let bare = format!("{EMULATOR_BASE}/_aws/{}", state::PROVIDER_ICON_PLANE);
    let (status, _media_type, body) = fetch(&router, "GET", &bare, "the bare plane").await;
    assert_ne!(
        status, 200,
        "case {bare:?}: the plane itself names no icon, got {status} with {} bytes",
        body.len()
    );
}

/// RULE 3 — the enumeration reports the catalog in the catalog's own published order, and
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
