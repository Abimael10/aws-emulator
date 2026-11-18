// SPDX-License-Identifier: CC0-1.0

//! WHICH REQUEST a deployed stage says it answered: the two ids of a call, and which of them
//! a CLIENT may state for itself.
//!
//! API Gateway names every request it answers by two ids, and they differ in who decides
//! them. `$context.requestId` is the id OF THE REQUEST, which a client may override by sending
//! it in the `x-amzn-RequestId` header — so a client that already names the call in its own
//! records has the api name it the same way, and the answer it holds reports that id.
//! `$context.extendedRequestId` is the id API GATEWAY mints, for every request and whatever
//! the request carried: nothing a client sends is echoed into it, which is what makes it the
//! id that identifies a call unambiguously.
//!
//! The RULE read here: `$context.requestId` is the id the request stated where it stated one
//! and an id the endpoint minted otherwise, the response reports THAT id in the header API
//! Gateway reports it in, and `$context.extendedRequestId` is the endpoint's own for every
//! request — reported in its own response header, never the stated one, and never the same
//! for two requests.
//!
//! The rows differ along what the request states for itself: nothing, an id of the client's
//! own spelling, an id spelled the way the endpoint itself mints one, and a header that states
//! nothing at all. Every case drives the real wire path — a request to the api's own invoke
//! host through the multi-service router, with the control plane driven through the same
//! emulator — so the ids asserted are the ones a client of the published invoke URL reads.

use std::collections::BTreeSet;

use serde_json::Value;

use super::{deploy, invoke, json_template, mock_resource};
use crate::rest_api_lifecycle::{create_api, Gateway};

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The header the request id travels in, in both directions: a client states the id of its
/// own request by sending it, and every response reports the id of the request it answered in
/// it. HTTP header names are case-insensitive; this is the lowercased spelling the wire
/// carries.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The response header API Gateway reports the EXTENDED request id in, which a client reads a
/// call's unambiguous id off whatever it stated for itself.
const EXTENDED_REQUEST_ID_HEADER: &str = "x-amz-apigw-id";

/// The resource the stage's one method sits on.
const ROUTE: &str = "named";

/// The response template that reports both ids of the call, which is what makes the
/// `$context` members a client can read.
const TEMPLATE: &str = r#"{"id":"$context.requestId","extended":"$context.extendedRequestId"}"#;

/// Whether `id` has the 8-4-4-4-12 lowercase-hex form the ids this endpoint mints carry.
fn is_minted_form(id: &str) -> bool {
    let mut groups = id.split('-');
    for width in [8usize, 4, 4, 4, 12] {
        let Some(group) = groups.next() else {
            return false;
        };
        if group.len() != width
            || !group
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return false;
        }
    }
    groups.next().is_none()
}

/// The `member` of a JSON body, as text.
fn member(body: &str, member: &str, case: &str) -> String {
    let document: Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("case {case:?}: expected a JSON body, got {body:?} ({error})"));
    document
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member:?} member, got {body:?}"))
        .to_string()
}

#[tokio::test]
async fn the_request_id_is_the_one_the_request_stated_and_the_extended_one_is_the_endpoints() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "stated-request-identity").await;
    mock_resource(
        &gateway,
        &api,
        &root,
        ROUTE,
        "GET",
        "200",
        json_template(TEMPLATE),
    )
    .await;
    deploy(&gateway, &api, STAGE).await;

    /// (case, the value the request states under the header, the id it is answered under)
    type Row<'a> = (&'a str, Option<&'a str>, Option<&'a str>);
    let rows: &[Row<'_>] = &[
        ("a request that states nothing", None, None),
        (
            "an id of the client's own spelling",
            Some("order-4821-attempt-1"),
            Some("order-4821-attempt-1"),
        ),
        (
            "an id spelled the way the endpoint itself mints one",
            Some("11111111-2222-3333-4444-555555555555"),
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        ("a single character", Some("7"), Some("7")),
        ("a header that states nothing at all", Some(""), None),
    ];

    // Every extended id reported, to assert the endpoint mints one PER REQUEST.
    let mut extended_ids: BTreeSet<String> = BTreeSet::new();
    for (case, stated, expected) in rows {
        let headers: Vec<(&str, &str)> = stated
            .map(|value| vec![(REQUEST_ID_HEADER, value)])
            .unwrap_or_default();
        let answered = invoke(
            &gateway,
            &api,
            "GET",
            &format!("/{STAGE}/{ROUTE}"),
            &headers,
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the stage to serve the call, got {} with {:?}",
            answered.status, answered.body
        );
        let reported = member(&answered.body, "id", case);
        match expected {
            // The request named ITSELF: that is the id the call carries and the id the
            // answer reports.
            Some(stated) => assert_eq!(
                reported, *stated,
                "case {case:?}: expected the call to carry the stated id, got {reported:?}"
            ),
            None => assert!(
                is_minted_form(&reported),
                "case {case:?}: expected an id the endpoint minted, got {reported:?}"
            ),
        }
        assert_eq!(
            answered.header(REQUEST_ID_HEADER),
            Some(reported.as_str()),
            "case {case:?}: expected the answer to report the id the call carries, got {:?}",
            answered.header(REQUEST_ID_HEADER)
        );
        // The EXTENDED id is the endpoint's own, whatever the request stated.
        let extended = member(&answered.body, "extended", case);
        assert!(
            is_minted_form(&extended),
            "case {case:?}: expected an extended id the endpoint minted, got {extended:?}"
        );
        assert_ne!(
            extended, reported,
            "case {case:?}: expected the two ids to name the request separately, got \
             {extended:?} twice"
        );
        assert_eq!(
            answered.header(EXTENDED_REQUEST_ID_HEADER),
            Some(extended.as_str()),
            "case {case:?}: expected the answer to report the extended id the call carries, \
             got {:?}",
            answered.header(EXTENDED_REQUEST_ID_HEADER)
        );
        assert!(
            extended_ids.insert(extended.clone()),
            "case {case:?}: expected an extended id no earlier call reported, got {extended:?}"
        );
    }
    assert_eq!(
        extended_ids.len(),
        rows.len(),
        "case \"the table as a whole\": expected one extended id per request, got \
         {extended_ids:?}"
    );
}

#[tokio::test]
async fn two_requests_stating_one_id_are_still_told_apart_by_the_extended_one() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "repeated-request-identity").await;
    mock_resource(
        &gateway,
        &api,
        &root,
        ROUTE,
        "GET",
        "200",
        json_template(TEMPLATE),
    )
    .await;
    deploy(&gateway, &api, STAGE).await;

    // The same call, byte for byte, stating the same id of its own: the id the call carries
    // is that one every time, and the extended id is a fresh one each time — which is what
    // makes it the id that tells two calls apart. Four repeats, so an id derived from the
    // request or from the records cannot pass.
    let stated = "a-client-stated-id";
    let mut extended_ids: BTreeSet<String> = BTreeSet::new();
    for repeat in 1..=4 {
        let case = format!("repeat {repeat}");
        let answered = invoke(
            &gateway,
            &api,
            "GET",
            &format!("/{STAGE}/{ROUTE}"),
            &[(REQUEST_ID_HEADER, stated)],
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the stage to serve the call, got {} with {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            member(&answered.body, "id", &case),
            stated,
            "case {case:?}: expected the stated id to name the call, got {:?}",
            answered.body
        );
        let extended = member(&answered.body, "extended", &case);
        assert!(
            extended_ids.insert(extended.clone()),
            "case {case:?}: expected an extended id no earlier repeat reported, got \
             {extended:?}"
        );
    }
    assert_eq!(
        extended_ids.len(),
        4,
        "case \"four repeats of one call\": expected four distinct extended ids, got \
         {extended_ids:?}"
    );
}
