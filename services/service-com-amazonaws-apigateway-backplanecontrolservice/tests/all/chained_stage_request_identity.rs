// SPDX-License-Identifier: CC0-1.0

//! WHICH REQUEST each of two CHAINED deployed stages says it answered: the id a client may
//! state for the call it makes, and the id each api mints for the call it answered.
//!
//! A REST API names every request it answers by two ids, and they differ in who decides them.
//! `$context.requestId` is the id OF THE REQUEST, which a client may state for itself by
//! sending it in the `x-amzn-RequestId` header, so a caller that already names the call in its
//! own records has the api name it the same way; a request that states none is named by an id
//! the endpoint mints for it. `$context.extendedRequestId` is the id API GATEWAY mints, for
//! every request and whatever the request carried — nothing a client sends is echoed into it,
//! which is what makes it the id that identifies one call at one api unambiguously.
//!
//! The RULE read here is that second half, over a CHAIN: a request that travels through a
//! front api's non-proxy `HTTP` integration into a second api's deployed stage is named by one
//! extended id at each api it reached, each minted by that api for that call — so the stated
//! id names the call at the front, and no extended id of either api is the stated one or the
//! other api's. The sibling `execute_api_data_plane::stated_request_identity` suite reads the
//! single-api half of the same rule; this one is what a chain adds to it, which one api alone
//! cannot show.
//!
//! The rows differ along what the request states for itself — nothing, an id of the client's
//! own spelling, an id spelled the way an endpoint itself mints one, a single character, and a
//! header that states nothing at all — because the rule is about WHO decides each id and not
//! about any value: ids are minted per request and no value is ever a requirement.
//!
//! Every case drives the real wire path: an HTTP request through the multi-service router onto
//! the front api's own `{rest-api-id}.execute-api.{region}.amazonaws.com` host, whose
//! integration reaches the backend api's published stage inside the same emulator, with the
//! control plane driven through the same emulator ([`Gateway`]) — so what is asserted is what a
//! client of the published invoke URL reads.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::execute_api_data_plane::{
    deploy, integration_response, invoke, method_response, put_integration, put_method,
};
use crate::rest_api_lifecycle::{create_api, resource_under, Gateway};

/// The region both invoke hosts are addressed in; any region is served.
const REGION: &str = "us-east-1";

/// The stage the front api is deployed to, and the one the backend api is deployed to.
const FRONT_STAGE: &str = "live";
const BACKEND_STAGE: &str = "back";

/// The resource each api carries its one `GET` on.
const FRONT_ROUTE: &str = "trace";
const BACKEND_ROUTE: &str = "named";

/// The header the request id travels in, in both directions: a client states the id of its own
/// request by sending it, and every response reports the id of the request it answered in it.
/// HTTP header names are case-insensitive; this is the lowercased spelling the wire carries.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The response header API Gateway reports the EXTENDED request id in, which a client reads a
/// call's unambiguous id off whatever it stated for itself.
const EXTENDED_REQUEST_ID_HEADER: &str = "x-amz-apigw-id";

/// The api's own invoke host, which is the address the data plane is reached by.
fn api_host(api: &str) -> String {
    format!("{api}.execute-api.{REGION}.amazonaws.com")
}

/// Whether `id` has the 8-4-4-4-12 lowercase-hex form the ids these endpoints mint carry.
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
    let document: Value = serde_json::from_str(body).unwrap_or_else(|error| {
        panic!("case {case:?}: expected a JSON body, got {body:?} ({error})")
    });
    document
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member:?} member, got {body:?}"))
        .to_string()
}

/// The BACKEND api: one `MOCK` `GET` whose integration response reports the id THAT api minted
/// for the call it answered, so the front api can carry it into its own answer. As the api id.
async fn reporting_backend(gateway: &Gateway) -> String {
    let (api, root) = create_api(gateway, "chained-identity-backend").await;
    let resource = resource_under(gateway, &api, &root, BACKEND_ROUTE).await;
    put_method(
        gateway,
        &api,
        &resource,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        &api,
        &resource,
        "GET",
        json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": "{\"statusCode\": 200}" },
        }),
    )
    .await;
    method_response(gateway, &api, &resource, "GET", "200", json!({})).await;
    integration_response(
        gateway,
        &api,
        &resource,
        "GET",
        "200",
        json!({
            "selectionPattern": "",
            "responseTemplates": {
                "application/json": r#"{"backendExtended":"$context.extendedRequestId"}"#,
            },
        }),
    )
    .await;
    deploy(gateway, &api, BACKEND_STAGE).await;
    api
}

/// The FRONT api: one `GET` whose non-proxy `HTTP` integration calls `backend`'s deployed
/// stage, and whose integration response reports both ids of the call IT answered beside the
/// extended id the backend reported for the call it answered. As the api id.
async fn chaining_front(gateway: &Gateway, backend: &str) -> String {
    let (api, root) = create_api(gateway, "chained-identity-front").await;
    let resource = resource_under(gateway, &api, &root, FRONT_ROUTE).await;
    put_method(
        gateway,
        &api,
        &resource,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        &api,
        &resource,
        "GET",
        json!({
            "type": "HTTP",
            "httpMethod": "GET",
            "uri": format!(
                "https://{host}/{BACKEND_STAGE}/{BACKEND_ROUTE}",
                host = api_host(backend),
            ),
        }),
    )
    .await;
    method_response(gateway, &api, &resource, "GET", "200", json!({})).await;
    integration_response(
        gateway,
        &api,
        &resource,
        "GET",
        "200",
        json!({
            "selectionPattern": "",
            "responseTemplates": {
                "application/json": concat!(
                    r#"{"frontRequestId":"$context.requestId","#,
                    r#""frontExtended":"$context.extendedRequestId","#,
                    r#""backendExtended":"$input.path('$.backendExtended')"}"#,
                ),
            },
        }),
    )
    .await;
    deploy(gateway, &api, FRONT_STAGE).await;
    api
}

#[tokio::test]
async fn a_chained_call_is_named_by_the_stated_id_and_by_one_minted_id_per_api() {
    let gateway = Gateway::new();
    let backend = reporting_backend(&gateway).await;
    let front = chaining_front(&gateway, &backend).await;

    /// (case, the value the request states under the header, the id the front call is named by)
    type Row<'a> = (&'a str, Option<&'a str>, Option<&'a str>);
    let rows: &[Row<'_>] = &[
        ("a request that states nothing", None, None),
        (
            "an id of the client's own spelling",
            Some("sim-order-4821-attempt-1"),
            Some("sim-order-4821-attempt-1"),
        ),
        (
            "an id spelled the way an endpoint itself mints one",
            Some("11111111-2222-3333-4444-555555555555"),
            Some("11111111-2222-3333-4444-555555555555"),
        ),
        ("a single character", Some("7"), Some("7")),
        ("a header that states nothing at all", Some(""), None),
    ];

    // Every extended id either api reported, to assert one is minted PER API PER REQUEST.
    let mut extended_ids: BTreeSet<String> = BTreeSet::new();
    for (case, stated, expected) in rows {
        let headers: Vec<(&str, &str)> = stated
            .map(|value| vec![(REQUEST_ID_HEADER, value)])
            .unwrap_or_default();
        let answered = invoke(
            &gateway,
            &front,
            "GET",
            &format!("/{FRONT_STAGE}/{FRONT_ROUTE}"),
            &headers,
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the front stage to serve the chained call, got {} with \
             {:?}",
            answered.status, answered.body
        );

        // The id of the REQUEST at the front api: the one it stated, or one minted for it.
        let front_request_id = member(&answered.body, "frontRequestId", case);
        match expected {
            Some(stated) => assert_eq!(
                front_request_id, *stated,
                "case {case:?}: expected the front call to carry the stated id, got \
                 {front_request_id:?}"
            ),
            None => assert!(
                is_minted_form(&front_request_id),
                "case {case:?}: expected an id the front endpoint minted, got \
                 {front_request_id:?}"
            ),
        }
        assert_eq!(
            answered.header(REQUEST_ID_HEADER),
            Some(front_request_id.as_str()),
            "case {case:?}: expected the answer to report the id the front call carries, got \
             {:?}",
            answered.header(REQUEST_ID_HEADER)
        );

        // One extended id per api, each that api's own for the call it answered.
        let front_extended = member(&answered.body, "frontExtended", case);
        let backend_extended = member(&answered.body, "backendExtended", case);
        for (which, extended) in [("front", &front_extended), ("backend", &backend_extended)] {
            assert!(
                is_minted_form(extended),
                "case {case:?}: expected an extended id the {which} api minted, got \
                 {extended:?}"
            );
            assert_ne!(
                *extended, front_request_id,
                "case {case:?}: expected the {which} api's extended id to name the call \
                 separately from the id of the request, got {extended:?} twice"
            );
            if let Some(stated) = stated {
                assert_ne!(
                    extended, stated,
                    "case {case:?}: expected the {which} api's extended id to be the api's \
                     own and not the value the request stated, got {extended:?}"
                );
            }
        }
        assert_ne!(
            front_extended, backend_extended,
            "case {case:?}: expected each api to name the call it answered by its own extended \
             id, got {front_extended:?} twice"
        );
        assert_eq!(
            answered.header(EXTENDED_REQUEST_ID_HEADER),
            Some(front_extended.as_str()),
            "case {case:?}: expected the answer to report the front api's own extended id, got \
             {:?}",
            answered.header(EXTENDED_REQUEST_ID_HEADER)
        );
        for extended in [&front_extended, &backend_extended] {
            assert!(
                extended_ids.insert(extended.clone()),
                "case {case:?}: expected an extended id no earlier call of either api \
                 reported, got {extended:?}"
            );
        }
    }

    // The point of the table: one extended id per api per request, so a chain of two apis over
    // five requests names ten calls by ten ids.
    assert_eq!(
        extended_ids.len(),
        2 * rows.len(),
        "case \"the table as a whole\": expected one extended id per api per request, got \
         {extended_ids:?}"
    );
}

#[tokio::test]
async fn repeating_one_chained_call_keeps_the_stated_id_and_mints_both_extended_ones_again() {
    let gateway = Gateway::new();
    let backend = reporting_backend(&gateway).await;
    let front = chaining_front(&gateway, &backend).await;

    // The same call, byte for byte, stating the same id of its own: the id the front call is
    // named by is that one every time — a client that states an id reads it back, which is what
    // lets it correlate its own records with the api's — while each api mints a fresh extended
    // id for every call it answers. Four repeats, so an id derived from the request or cached
    // per stage cannot pass.
    let stated = "a-client-stated-id";
    let mut extended_ids: BTreeSet<String> = BTreeSet::new();
    for repeat in 1..=4 {
        let case = format!("repeat {repeat}");
        let answered = invoke(
            &gateway,
            &front,
            "GET",
            &format!("/{FRONT_STAGE}/{FRONT_ROUTE}"),
            &[(REQUEST_ID_HEADER, stated)],
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the front stage to serve the chained call, got {} with \
             {:?}",
            answered.status, answered.body
        );
        assert_eq!(
            member(&answered.body, "frontRequestId", &case),
            stated,
            "case {case:?}: expected the stated id to name the front call, got {:?}",
            answered.body
        );
        for which in ["frontExtended", "backendExtended"] {
            let extended = member(&answered.body, which, &case);
            assert!(
                extended_ids.insert(extended.clone()),
                "case {case:?}: expected a {which} id no earlier repeat reported, got \
                 {extended:?}"
            );
        }
    }
    assert_eq!(
        extended_ids.len(),
        8,
        "case \"four repeats of one chained call\": expected two fresh extended ids per \
         repeat, got {extended_ids:?}"
    );
}
