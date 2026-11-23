// SPDX-License-Identifier: CC0-1.0

//! WHICH RECORD a call that addresses a method's `Integration` reports when that address
//! resolves to nothing, and what `DeleteIntegration` does with a method that carries none.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the
//! `CreateRestApi` / `CreateResource` / `PutMethod` / `PutIntegration` /
//! `PutIntegrationResponse` helpers are this crate's existing harness, reused from
//! `rest_api_lifecycle`, `method_integration`, `method_response`, `integration_response`
//! and `base_path_mapping` (its one `assert_refusal`).
//!
//! THE RULES UNDER TEST:
//!
//! * An `Integration` is the ONE record a method's integration slot holds: it carries no
//!   identifier of its own, so `(restApiId, resourceId, httpMethod)` is ONE address of it
//!   and both halves failing — a verb the resource holds no method under, and a method
//!   carrying no integration — leave that ONE address unresolvable. Every call whose target
//!   is that integration, or a record hanging off it, reports the INTEGRATION it could not
//!   resolve, so a client can tell "this method has no integration" (remediation: create
//!   one) from "this METHOD does not exist" (remediation: create the method), which is what
//!   a call whose own target is the method reports. The records UNDER the integration are
//!   keyed by a `statusCode` the CALLER names, so a code that resolves to no response
//!   reports that status code instead.
//! * `PutIntegration` is the one exception, because it CREATES the integration: the record
//!   it has to find is the METHOD it attaches it to, and that is what its refusal names.
//! * `DeleteIntegration` asks for a method that carries no integration. A method that
//!   carries none is already in that state, so the call answers the modelled 204 and
//!   changes nothing — repeating it, or making it for a method that never had one, answers
//!   204 just the same — while an address that resolves to no METHOD, no resource or no API
//!   is refused 404 and removes nothing.
//!
//! The tables span what the rules generalise over: which half of the integration's address
//! is unresolvable, every operation that addresses an integration or one of its responses
//! (`Get`/`Put`/`Update`/`Delete` on both), the contrasting method-addressed calls, the
//! status-code half, the integration type the deleted integration carried, the verb its
//! method is addressed by, and how many times the delete is repeated.

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    NO_SUCH_INTEGRATION, NO_SUCH_METHOD, NO_SUCH_METHOD_RESPONSE, NO_SUCH_RESOURCE,
};

use crate::base_path_mapping::assert_refusal;
use crate::integration_response::put_integration_response;
use crate::method_integration::{
    aws_integration_uri, delete_integration, get_integration, get_method, put_integration,
    put_method, resource_with_a_method,
};
use crate::rest_api_lifecycle::{create_api, create_resource, string_member, Gateway};

/// An identifier no record of the API answers to, for the rows whose unresolvable half is
/// the resource rather than the integration.
const MISSING: &str = "zzzzzzzzzz";

/// The status code the wired method's integration carries a response under, and one it
/// carries none under — the keyed half of an integration response's address.
const STORED_CODE: &str = "200";
const ABSENT_CODE: &str = "404";

/// One row of the refusal table: its case, the HTTP method of the call, the request URI,
/// the body it carries, and the sentence the call is refused with.
type Row = (&'static str, &'static str, String, Option<Value>, &'static str);

/// A `patchOperations` body that asks for nothing: the lists are what an `Update*` call
/// carries, and an empty one cannot be what a row is refused for.
fn no_patches() -> Option<Value> {
    Some(json!({ "patchOperations": [] }))
}

#[tokio::test]
async fn an_unresolvable_integration_address_reports_the_integration() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "integration-address").await;
    // One method carrying an integration with a response under `STORED_CODE`, and one
    // carrying no integration at all: the two halves of an integration's address, each
    // unresolvable on its own.
    let wired = resource_with_a_method(&gateway, &api, &root, "wired", "GET", json!({})).await;
    let created = put_integration(&gateway, &api, &wired, "GET", json!({ "type": "MOCK" })).await;
    assert_eq!(
        created.status, 201,
        "case PutIntegration(wired/GET): expected 201, got {} with {:?}",
        created.status, created.body
    );
    let response =
        put_integration_response(&gateway, &api, &wired, "GET", STORED_CODE, json!({})).await;
    assert_eq!(
        response.status, 201,
        "case PutIntegrationResponse(wired/GET/{STORED_CODE}): expected 201, got {} with {:?}",
        response.status, response.body
    );
    let bare = resource_with_a_method(&gateway, &api, &root, "bare", "POST", json!({})).await;

    // `wired` holds a GET and no PUT; `bare` holds a POST and no integration. So
    // `{wired}/PUT` is an address whose METHOD half fails and `{bare}/POST` one whose
    // INTEGRATION half fails, and every row below is one call over one of them.
    let integration = |resource: &str, verb: &str| {
        format!("/restapis/{api}/resources/{resource}/methods/{verb}/integration")
    };
    let integration_response = |resource: &str, verb: &str, code: &str| {
        format!(
            "/restapis/{api}/resources/{resource}/methods/{verb}/integration/responses/{code}"
        )
    };
    let method = |resource: &str, verb: &str| {
        format!("/restapis/{api}/resources/{resource}/methods/{verb}")
    };

    let rows: Vec<Row> = vec![
        // The integration itself, over both halves of its address and every call that
        // READS, PATCHES or REMOVES it.
        (
            "GetIntegration on a method carrying none",
            "GET",
            integration(&bare, "POST"),
            None,
            NO_SUCH_INTEGRATION,
        ),
        (
            "GetIntegration on a verb naming no method",
            "GET",
            integration(&wired, "PUT"),
            None,
            NO_SUCH_INTEGRATION,
        ),
        (
            "UpdateIntegration on a method carrying none",
            "PATCH",
            integration(&bare, "POST"),
            no_patches(),
            NO_SUCH_INTEGRATION,
        ),
        (
            "UpdateIntegration on a verb naming no method",
            "PATCH",
            integration(&wired, "PUT"),
            no_patches(),
            NO_SUCH_INTEGRATION,
        ),
        (
            "DeleteIntegration on a verb naming no method",
            "DELETE",
            integration(&wired, "PUT"),
            None,
            NO_SUCH_INTEGRATION,
        ),
        // A record hanging off the integration: the integration's own address fails first,
        // whichever half of it did, for every call that reaches a response through it.
        (
            "GetIntegrationResponse on a method carrying no integration",
            "GET",
            integration_response(&bare, "POST", STORED_CODE),
            None,
            NO_SUCH_INTEGRATION,
        ),
        (
            "GetIntegrationResponse on a verb naming no method",
            "GET",
            integration_response(&wired, "PUT", STORED_CODE),
            None,
            NO_SUCH_INTEGRATION,
        ),
        (
            "PutIntegrationResponse on a method carrying no integration",
            "PUT",
            integration_response(&bare, "POST", STORED_CODE),
            Some(json!({})),
            NO_SUCH_INTEGRATION,
        ),
        (
            "PutIntegrationResponse on a verb naming no method",
            "PUT",
            integration_response(&wired, "PUT", STORED_CODE),
            Some(json!({})),
            NO_SUCH_INTEGRATION,
        ),
        (
            "UpdateIntegrationResponse on a method carrying no integration",
            "PATCH",
            integration_response(&bare, "POST", STORED_CODE),
            no_patches(),
            NO_SUCH_INTEGRATION,
        ),
        (
            "DeleteIntegrationResponse on a method carrying no integration",
            "DELETE",
            integration_response(&bare, "POST", STORED_CODE),
            None,
            NO_SUCH_INTEGRATION,
        ),
        (
            "DeleteIntegrationResponse on a verb naming no method",
            "DELETE",
            integration_response(&wired, "PUT", STORED_CODE),
            None,
            NO_SUCH_INTEGRATION,
        ),
        // The call that CREATES the integration reports the method it must attach it to.
        (
            "PutIntegration on a verb naming no method",
            "PUT",
            integration(&wired, "PUT"),
            Some(json!({ "type": "MOCK" })),
            NO_SUCH_METHOD,
        ),
        // The contrasting class: a call whose own target IS the method reports the method.
        (
            "GetMethod on a verb naming no method",
            "GET",
            method(&wired, "PUT"),
            None,
            NO_SUCH_METHOD,
        ),
        (
            "PutMethodResponse on a verb naming no method",
            "PUT",
            format!(
                "/restapis/{api}/resources/{wired}/methods/PUT/responses/{STORED_CODE}"
            ),
            Some(json!({})),
            NO_SUCH_METHOD,
        ),
        (
            "UpdateMethod on a verb naming no method",
            "PATCH",
            method(&wired, "PUT"),
            no_patches(),
            NO_SUCH_METHOD,
        ),
        (
            "DeleteMethod on a verb naming no method",
            "DELETE",
            method(&wired, "PUT"),
            None,
            NO_SUCH_METHOD,
        ),
        // The integration resolved and the CALLER's own key did not: the status code is
        // what such a call reports.
        (
            "GetIntegrationResponse for a status code naming no response",
            "GET",
            integration_response(&wired, "GET", ABSENT_CODE),
            None,
            NO_SUCH_METHOD_RESPONSE,
        ),
        (
            "UpdateIntegrationResponse for a status code naming no response",
            "PATCH",
            integration_response(&wired, "GET", ABSENT_CODE),
            no_patches(),
            NO_SUCH_METHOD_RESPONSE,
        ),
        (
            "DeleteIntegrationResponse for a status code naming no response",
            "DELETE",
            integration_response(&wired, "GET", ABSENT_CODE),
            None,
            NO_SUCH_METHOD_RESPONSE,
        ),
        // The walk stops at the RESOURCE: an address whose resource half fails never
        // reaches a method, so it reports the resource whichever record it was for.
        (
            "GetIntegration under an unknown resourceId",
            "GET",
            integration(MISSING, "GET"),
            None,
            NO_SUCH_RESOURCE,
        ),
        (
            "DeleteIntegration under an unknown resourceId",
            "DELETE",
            integration(MISSING, "GET"),
            None,
            NO_SUCH_RESOURCE,
        ),
        (
            "GetIntegrationResponse under an unknown resourceId",
            "GET",
            integration_response(MISSING, "GET", STORED_CODE),
            None,
            NO_SUCH_RESOURCE,
        ),
    ];

    for (case, verb, uri, body, message) in &rows {
        let answer = gateway.call(verb, uri, body.clone()).await;
        assert_refusal(&answer, (404, "NotFoundException"), message, case);
    }

    // Every row above was refused, so each record is exactly as it was: the wired
    // integration and its response are still readable.
    let read = get_integration(&gateway, &api, &wired, "GET").await;
    assert_eq!(
        read.status, 200,
        "case GetIntegration after the refused calls: expected 200, got {} with {:?}",
        read.status, read.body
    );
    assert_eq!(
        read.body["integrationResponses"][STORED_CODE]["statusCode"],
        json!(STORED_CODE),
        "case GetIntegration after the refused calls: expected the stored response, got {:?}",
        read.body["integrationResponses"]
    );
}

#[tokio::test]
async fn deleting_an_integration_a_method_does_not_carry_answers_the_modelled_204() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "integration-delete-idempotent").await;

    // Rows differ along the integration the method carried before the first delete (every
    // family of the modelled `IntegrationType`, and none at all), the verb the method is
    // addressed by, and how many times the delete is repeated.
    type Case = (&'static str, &'static str, &'static str, Option<Value>, usize);
    let cases: Vec<Case> = vec![
        ("a method that never carried one", "never", "GET", None, 1),
        (
            "a mock integration deleted twice",
            "mock",
            "POST",
            Some(json!({ "type": "MOCK" })),
            2,
        ),
        (
            "an http integration deleted three times",
            "http",
            "PUT",
            Some(json!({
                "type": "HTTP",
                "httpMethod": "GET",
                "uri": "https://example.com/pets"
            })),
            3,
        ),
        (
            "an aws integration deleted twice",
            "aws",
            "ANY",
            Some(json!({
                "type": "AWS",
                "httpMethod": "POST",
                "uri": aws_integration_uri("dynamodb:action/PutItem"),
                "credentials": "arn:aws:iam::123456789012:role/apigateway-dynamodb"
            })),
            2,
        ),
        (
            "a lambda proxy integration deleted four times",
            "lambda",
            "DELETE",
            Some(json!({
                "type": "AWS_PROXY",
                "httpMethod": "POST",
                "uri": aws_integration_uri(
                    "lambda:path/2015-03-31/functions/\
                     arn:aws:lambda:us-east-1:123456789012:function:pets/invocations"
                )
            })),
            4,
        ),
    ];

    for (case, path_part, verb, request, repeats) in &cases {
        let resource =
            resource_with_a_method(&gateway, &api, &root, path_part, verb, json!({})).await;
        if let Some(request) = request {
            let created =
                put_integration(&gateway, &api, &resource, verb, request.clone()).await;
            assert_eq!(
                created.status, 201,
                "case {case}: expected 201 from PutIntegration, got {} with {:?}",
                created.status, created.body
            );
        }
        // Every repeat asks for the same state — a method carrying no integration — so
        // every repeat answers the modelled 204 with an empty body.
        for attempt in 1..=*repeats {
            let deleted = delete_integration(&gateway, &api, &resource, verb).await;
            assert_eq!(
                deleted.status, 204,
                "case {case}: expected the modelled 204 from DeleteIntegration attempt \
                 {attempt}, got {} with {:?}",
                deleted.status, deleted.body
            );
            assert!(
                deleted.body.is_null() || deleted.body == json!({}),
                "case {case}: expected an empty body from attempt {attempt}, got {:?}",
                deleted.body
            );
        }
        // The method survives every repeat and carries no integration, so the integration
        // is unresolvable for a reader.
        let read = get_method(&gateway, &api, &resource, verb).await;
        assert_eq!(
            read.status, 200,
            "case {case}: the method must survive the deletes, got {} with {:?}",
            read.status, read.body
        );
        assert!(
            read.body["methodIntegration"].is_null(),
            "case {case}: expected no methodIntegration, got {:?}",
            read.body["methodIntegration"]
        );
        assert_refusal(
            &get_integration(&gateway, &api, &resource, verb).await,
            (404, "NotFoundException"),
            NO_SUCH_INTEGRATION,
            case,
        );
        // And the method is free to carry one again, which the delete then removes once
        // more: the 204 is about the state asked for, not about a record being there.
        let again = put_integration(&gateway, &api, &resource, verb, json!({ "type": "MOCK" })).await;
        assert_eq!(
            again.status, 201,
            "case {case}: expected 201 from PutIntegration after the deletes, got {} with {:?}",
            again.status, again.body
        );
        let removed = delete_integration(&gateway, &api, &resource, verb).await;
        assert_eq!(
            removed.status, 204,
            "case {case}: expected 204 from the delete of the recreated integration, got {} \
             with {:?}",
            removed.status, removed.body
        );
    }

    // The 204 is the state the METHOD is in, so it needs a method: an address that resolves
    // to no method, no resource or no API is refused and removes nothing.
    let standing = create_resource(&gateway, &api, &root, "standing").await;
    let standing = string_member(&standing, "id", "standing");
    put_method(&gateway, &api, &standing, "GET").await;
    let created = put_integration(&gateway, &api, &standing, "GET", json!({ "type": "MOCK" })).await;
    assert_eq!(
        created.status, 201,
        "case PutIntegration(standing/GET): expected 201, got {} with {:?}",
        created.status, created.body
    );
    let unresolvable: Vec<(&str, String, &str)> = vec![
        (
            "a verb naming no method",
            format!("/restapis/{api}/resources/{standing}/methods/PATCH/integration"),
            NO_SUCH_INTEGRATION,
        ),
        (
            "the same verb in another letter case",
            format!("/restapis/{api}/resources/{standing}/methods/get/integration"),
            NO_SUCH_INTEGRATION,
        ),
        (
            "an unknown resourceId",
            format!("/restapis/{api}/resources/{MISSING}/methods/GET/integration"),
            NO_SUCH_RESOURCE,
        ),
        (
            "an unknown restApiId",
            format!("/restapis/{MISSING}/resources/{standing}/methods/GET/integration"),
            NO_SUCH_RESOURCE,
        ),
    ];
    for (case, uri, message) in &unresolvable {
        let answer = gateway.call("DELETE", uri, None).await;
        assert_refusal(&answer, (404, "NotFoundException"), message, case);
    }
    // Nothing those refusals named was removed: the standing integration is still there.
    let still = get_integration(&gateway, &api, &standing, "GET").await;
    assert_eq!(
        still.status, 200,
        "case GetIntegration after the refused deletes: expected 200, got {} with {:?}",
        still.status, still.body
    );
}
