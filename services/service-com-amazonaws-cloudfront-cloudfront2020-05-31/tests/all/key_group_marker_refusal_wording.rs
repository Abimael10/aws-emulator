// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE the key group listing refuses an unusable `Marker` with, end to end over
//! HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, the query
//! binding, the handler and the restXml error envelope — the same path a client takes. The
//! rule under test is how CloudFront words this refusal:
//!
//! * `ListKeyGroups` refuses a `Marker` that is no resume point it handed out with the
//!   modelled `InvalidArgument` (400) whose message states the condition over the KIND of
//!   resource the listing pages — "The request contains an invalid key group marker." —
//!   fixed text that carries nothing of the submitted value, so two runs of the same mistake
//!   read alike and no caller can amplify one request into a long answer;
//! * that sentence is ONE template this service fills with the resource each listing pages,
//!   so the listings that state this family cannot word the same condition differently;
//! * the refusal is about the VALUE and not about the parameter: a `Marker` the listing
//!   really did hand out is a resume point and is served.
//!
//! The rows differ along what the rule generalises over: the FORM an unusable marker takes
//! (a word that is no identifier, another resource kind's id, an id of the right shape in
//! the wrong case, each wrong field width, characters outside the id alphabet, a percent
//! escape, and a value far longer than any id), and — for the template — the RESOURCE the
//! listing pages (the key groups plus each policy family that states this sentence).
//!
//! The harness (the router, the request helper and the element scanner) is the distribution
//! lifecycle suite's, and the key group writer is the key group suite's — reused rather than
//! copied.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, Answer, API_VERSION};
use crate::key_group_lifecycle::{create_group, mint_public_keys, Group};

/// The sentence this listing refuses an unusable `Marker` with, written out as a LITERAL
/// rather than read back off the crate's producer: the WORDING is what these cases are held
/// to.
const INVALID_MARKER_MESSAGE: &str = "The request contains an invalid key group marker.";

/// The frame that sentence is one filling of — the text before and after the kind of resource
/// a listing pages. Read off the recorded wording of this family, which the rows in
/// [`a_marker_refusal_is_one_template_filled_with_the_resource_the_listing_pages`] span.
const TEMPLATE: (&str, &str) = ("The request contains an invalid ", " marker.");

/// `GET /2020-05-31/key-group?Marker=<marker>`, from the operation's `http` binding.
async fn resume_groups(router: &MultiServiceRouter, marker: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/key-group?Marker={marker}"),
        None,
        None,
    )
    .await
}

/// `GET <path>?Marker=<marker>`, for the sibling listings the template is read across.
async fn resume(router: &MultiServiceRouter, path: &str, marker: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/{path}?Marker={marker}"),
        None,
        None,
    )
    .await
}

/// Every form a `Marker` can take that is no resume point this listing handed out: each of
/// them is refused in the same words, whatever the value looked like.
fn unusable_markers() -> Vec<(String, String)> {
    vec![
        ("a word that is no identifier".to_string(), "nope".to_string()),
        (
            "another resource kind's identifier".to_string(),
            "K1AAAAAAAAAAAA".to_string(),
        ),
        (
            "an id of the right shape in upper case".to_string(),
            "0123456789AB-CDEF-0123-4567-89ABCDEF0123".to_string(),
        ),
        (
            "too few characters for an id".to_string(),
            "0123456789ab".to_string(),
        ),
        (
            "fields of the wrong width".to_string(),
            "0123456-78ab-cdef-0123-456789abcdef".to_string(),
        ),
        (
            "characters outside the id alphabet".to_string(),
            "g1234567-89ab-cdef-0123-456789abcdef".to_string(),
        ),
        (
            "a percent escape an id cannot hold".to_string(),
            "0123456789ab%20cdef".to_string(),
        ),
        // Far longer than any id this service mints: the refusal still carries none of it.
        ("a value far longer than any id".to_string(), "m".repeat(512)),
    ]
}

/// Whatever form an unusable `Marker` takes, the key group listing refuses it with the one
/// sentence that names the resource it pages — and with nothing of the value submitted.
#[tokio::test]
async fn an_unusable_key_group_marker_is_refused_in_the_services_own_words() {
    let router = emulator();

    for (case, marker) in unusable_markers() {
        let answer = resume_groups(&router, &marker).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400 for {marker:?}, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "InvalidArgument",
            "case {case:?}: expected InvalidArgument for {marker:?}, got {:?} in {}",
            answer.error_code(),
            answer.raw
        );
        let message = answer.root().find_text("Message").unwrap_or_default();
        assert_eq!(
            message, INVALID_MARKER_MESSAGE,
            "case {case:?}: expected the sentence {INVALID_MARKER_MESSAGE:?} for {marker:?}, \
             got {message:?}"
        );
        assert!(
            !message.contains(marker.as_str()),
            "case {case:?}: the refusal must carry nothing of the submitted marker \
             {marker:?}, got {message:?}"
        );
    }
}

/// The sentence is ONE template this service fills with the kind of resource the listing
/// pages: the key group listing and each policy listing that states this family read alike
/// apart from that one name.
#[tokio::test]
async fn a_marker_refusal_is_one_template_filled_with_the_resource_the_listing_pages() {
    let router = emulator();

    // (case, the listing's route, the resource it pages as the sentence names it)
    let listings: [(&str, &str, &str); 5] = [
        ("the key group listing", "key-group", "key group"),
        ("the cache policy listing", "cache-policy", "cache policy"),
        (
            "the origin request policy listing",
            "origin-request-policy",
            "origin request policy",
        ),
        (
            "the response headers policy listing",
            "response-headers-policy",
            "response headers policy",
        ),
        (
            "the continuous deployment policy listing",
            "continuous-deployment-policy",
            "continuous deployment policy",
        ),
    ];
    let marker = "not-a-resume-point";
    for (case, path, resource) in listings {
        let answer = resume(&router, path, marker).await;
        assert!(
            (400..500).contains(&answer.status),
            "case {case:?}: expected a client error for {marker:?}, got {} with {}",
            answer.status,
            answer.raw
        );
        let message = answer.root().find_text("Message").unwrap_or_default();
        let (before, after) = TEMPLATE;
        let expected = format!("{before}{resource}{after}");
        assert_eq!(
            message, expected,
            "case {case:?}: expected the sentence {expected:?}, got {message:?} in {}",
            answer.raw
        );
    }
}

/// The control: a `Marker` the listing really did hand out is a resume POSITION, so the
/// refusals above are about the value being no resume point and not about the parameter
/// being carried at all.
#[tokio::test]
async fn a_marker_the_listing_handed_out_resumes_the_page() {
    let router = emulator();
    let keys = mint_public_keys(&router, "key-group-marker", 2).await;

    // Two groups, so one page of size one leaves a `NextMarker` to resume at.
    let mut ids: Vec<String> = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let row = Group {
            case: format!("group {index}"),
            name: format!("key-group-marker-{index}"),
            items: vec![key.clone()],
            comment: None,
        };
        let answer = create_group(&router, &row.xml()).await;
        assert_eq!(
            answer.status, 201,
            "case {:?}: expected HTTP 201 from CreateKeyGroup, got {} with {}",
            row.case, answer.status, answer.raw
        );
        ids.push(answer.root().text_at("Id").unwrap_or_default().to_string());
    }
    ids.sort();

    let first = call(
        &router,
        "GET",
        &format!("/{API_VERSION}/key-group?MaxItems=1"),
        None,
        None,
    )
    .await;
    assert_eq!(
        first.status, 200,
        "case \"the first page\": expected HTTP 200, got {} with {}",
        first.status, first.raw
    );
    let next = first
        .root()
        .find_text("NextMarker")
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        next,
        ids[0],
        "case \"the first page\": expected NextMarker to name the group the page ended at, \
         got {next:?} in {}",
        first.raw
    );

    let page = resume_groups(&router, &next).await;
    assert_eq!(
        page.status, 200,
        "case \"a marker the listing handed out\": expected HTTP 200, got {} with {}",
        page.status, page.raw
    );
    let served: Vec<String> = page
        .root()
        .at("Items")
        .map(|items| {
            items
                .children_named("KeyGroupSummary")
                .into_iter()
                .filter_map(|summary| summary.at("KeyGroup"))
                .map(|group| group.text_at("Id").unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        served,
        vec![ids[1].clone()],
        "case \"a marker the listing handed out\": expected the groups after {next:?}, got \
         {served:?} in {}",
        page.raw
    );
}
