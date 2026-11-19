// SPDX-License-Identifier: CC0-1.0

//! WHICH record a call addressed by a record NESTED under a REST API reports when the
//! address resolves to nothing, asserted on the wire.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the create
//! helpers are this crate's existing harness.
//!
//! The RULE under test: a call addressed by a record nested under a REST API resolves that
//! record by its WHOLE key in ONE lookup, and the refusal states that record's own fixed
//! sentence whether the child label, the `restApiId` or both resolve to nothing: a child
//! cannot exist without its API, so an API this account does not hold leaves the addressed
//! record unresolvable too. The account-qualified identifier sentence
//! (`Invalid API identifier specified <account>:<restApiId>`) belongs to a call whose own
//! target IS the API, which `rest_api_identifier_refusals` asserts, and to the one create
//! that resolves the API as a precondition of its own before it looks at the parent it
//! hangs the new record off (`CreateResource`, asserted below).
//!
//! The rows span what the rule generalises over rather than repeating one recorded call:
//! both record families (the stage and the resource), every operation of this service
//! addressed by one of them — reads, patches, deletes, the two cache flushes, the export a
//! stage renders, the two creates that hang a child off a resource, the records that hang
//! off a resource (a method, its integration, both of their responses and the simulated
//! invocation of a method), and the three tag operations, which address a record by its ARN
//! — each of the three ways the address can fail (an unresolvable API with a stored child
//! label, a stored API with an unresolvable child label, and both unresolvable), and
//! unknown identifiers differing in length, alphabet and case.

use serde_json::{json, Value};

use crate::method_integration::{put_integration, put_method};
use crate::rest_api_lifecycle::{
    assert_failure, create_api, resource_under, Answer, Gateway,
};
use crate::stage_lifecycle::create_deployed_stage;
use crate::tag_reads_by_arn::{apigateway_arn, encode_label, get_tags};

/// The sentence this service states for a stage address that resolves to no stage
/// (`state::NO_SUCH_STAGE`).
const NO_SUCH_STAGE: &str = "Invalid stage identifier specified";

/// The sentence this service states for a resource address that resolves to no resource
/// (`state::NO_SUCH_RESOURCE`).
const NO_SUCH_RESOURCE: &str = "Invalid Resource identifier specified";

/// Identifiers no stored API has, differing in length (one character, the ten characters
/// API Gateway mints, longer), alphabet (letters, digits, a mix, a hyphen) and case.
/// `restApiId` is a plain `String` with no `@pattern`
/// (`smithy_query(query="operation", operation="GetStage")`), so every row reaches the
/// lookup.
const UNKNOWN_APIS: &[&str] = &["q", "zzzzzzzzzz", "0123456789", "Ab3Xy9Zq1W", "no-such-api-01"];

/// Child labels no stored API has a record under, in the same spread: a stage name and a
/// resource id are both plain `String` labels.
const UNKNOWN_CHILDREN: &[&str] = &["x", "zzzzzzzzzz", "0123456789", "Ab3Xy9Zq1W", "no-such-01"];

/// A patch body that is valid in itself, so a row reaches the address lookup instead of
/// being refused for its body.
fn patch(path: &str, value: &str) -> Value {
    json!({ "patchOperations": [{ "op": "replace", "path": path, "value": value }] })
}

/// Every call of this service addressed by a STAGE of the API, with `api` and `stage` the
/// address under test.
fn stage_requests(api: &str, stage: &str) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    vec![
        (
            "GetStage",
            "GET",
            format!("/restapis/{api}/stages/{stage}"),
            None,
        ),
        (
            "UpdateStage",
            "PATCH",
            format!("/restapis/{api}/stages/{stage}"),
            Some(patch("/description", "d")),
        ),
        (
            "DeleteStage",
            "DELETE",
            format!("/restapis/{api}/stages/{stage}"),
            None,
        ),
        (
            "FlushStageCache",
            "DELETE",
            format!("/restapis/{api}/stages/{stage}/cache/data"),
            None,
        ),
        (
            "FlushStageAuthorizersCache",
            "DELETE",
            format!("/restapis/{api}/stages/{stage}/cache/authorizers"),
            None,
        ),
        // The export is a document RENDERED from the stage, so the stage is the record
        // whose existence the call needs.
        (
            "GetExport",
            "GET",
            format!("/restapis/{api}/stages/{stage}/exports/oas30"),
            None,
        ),
    ]
}

/// A patch body whose op is an `add` of one map entry, valid in itself for the members the
/// rows below name, so a row reaches the address lookup instead of being refused for its
/// body.
fn add(path: &str, value: &str) -> Value {
    json!({ "patchOperations": [{ "op": "add", "path": path, "value": value }] })
}

/// Every call of this service addressed by a record that hangs OFF a resource — a method,
/// its integration, either of their responses, and the simulated invocation of a method —
/// with `api` and `resource` the address under test.
///
/// The verb and the status codes name records the STORED resource carries, so the only
/// thing wrong with a refused row is the address. The rows are ordered so that the whole
/// list can also be driven against the stored address: every read and patch first, then
/// the writes, then the deletes from the innermost record outwards.
fn resource_child_requests(
    api: &str,
    resource: &str,
) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    let method = format!("/restapis/{api}/resources/{resource}/methods/GET");
    vec![
        ("GetMethod", "GET", method.clone(), None),
        ("TestInvokeMethod", "POST", method.clone(), Some(json!({}))),
        (
            "UpdateMethod",
            "PATCH",
            method.clone(),
            Some(patch("/authorizationType", "NONE")),
        ),
        ("GetIntegration", "GET", format!("{method}/integration"), None),
        (
            "UpdateIntegration",
            "PATCH",
            format!("{method}/integration"),
            Some(patch("/timeoutInMillis", "5000")),
        ),
        (
            "GetMethodResponse",
            "GET",
            format!("{method}/responses/200"),
            None,
        ),
        (
            "UpdateMethodResponse",
            "PATCH",
            format!("{method}/responses/200"),
            Some(add("/responseParameters/method.response.header.x-trace", "false")),
        ),
        (
            "PutMethodResponse",
            "PUT",
            format!("{method}/responses/201"),
            Some(json!({})),
        ),
        (
            "GetIntegrationResponse",
            "GET",
            format!("{method}/integration/responses/200"),
            None,
        ),
        (
            "UpdateIntegrationResponse",
            "PATCH",
            format!("{method}/integration/responses/200"),
            Some(add("/responseTemplates/application~1json", "{}")),
        ),
        (
            "PutIntegrationResponse",
            "PUT",
            format!("{method}/integration/responses/201"),
            Some(json!({})),
        ),
        (
            "DeleteIntegrationResponse",
            "DELETE",
            format!("{method}/integration/responses/200"),
            None,
        ),
        (
            "DeleteMethodResponse",
            "DELETE",
            format!("{method}/responses/200"),
            None,
        ),
        // A PUT replaces whatever integration the method had, so it goes after the
        // integration's own responses have been read and removed.
        (
            "PutIntegration",
            "PUT",
            format!("{method}/integration"),
            Some(json!({ "type": "MOCK" })),
        ),
        (
            "DeleteIntegration",
            "DELETE",
            format!("{method}/integration"),
            None,
        ),
        ("DeleteMethod", "DELETE", method, None),
    ]
}

/// Every call of this service addressed by a RESOURCE of the API, with `api` and
/// `resource` the address under test.
///
/// `CreateResource` is not among them: it resolves the `restApiId` as a precondition of its
/// own, ahead of the parent it would hang the new resource off, so an unresolvable API is
/// reported as the API there — see
/// [`a_create_resource_reports_the_api_before_the_parent_it_cannot_resolve`].
fn resource_requests(
    api: &str,
    resource: &str,
) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    vec![
        (
            "GetResource",
            "GET",
            format!("/restapis/{api}/resources/{resource}"),
            None,
        ),
        (
            "UpdateResource",
            "PATCH",
            format!("/restapis/{api}/resources/{resource}"),
            Some(patch("/pathPart", "renamed")),
        ),
        (
            "DeleteResource",
            "DELETE",
            format!("/restapis/{api}/resources/{resource}"),
            None,
        ),
        // The call that hangs a NEW child off the resource: the child is being created, so
        // the resource is the record whose existence the call needs.
        (
            "PutMethod",
            "PUT",
            format!("/restapis/{api}/resources/{resource}/methods/GET"),
            Some(json!({ "authorizationType": "NONE" })),
        ),
    ]
}

/// Every TAG call of this service, which addresses the record by its ARN rather than by a
/// URI label: the `/restapis/{restApiId}/stages/{stageName}` resource portion of a stage's
/// ARN names the STAGE, so these report the stage too.
fn stage_arn_requests(
    api: &str,
    stage: &str,
) -> Vec<(&'static str, &'static str, String, Option<Value>)> {
    let arn = encode_label(&apigateway_arn(&format!("/restapis/{api}/stages/{stage}")));
    vec![
        ("GetTags", "GET", format!("/tags/{arn}"), None),
        (
            "TagResource",
            "PUT",
            format!("/tags/{arn}"),
            Some(json!({ "tags": { "owner": "team" } })),
        ),
        (
            "UntagResource",
            "DELETE",
            format!("/tags/{arn}?tagKeys=owner"),
            None,
        ),
    ]
}

/// Assert `answer` is a 404 `NotFoundException` whose message is exactly `message`, and
/// that it quotes back neither an identifier nor the account.
fn assert_reports(answer: &Answer, case: &str, message: &str) {
    assert_failure(answer, 404, "NotFoundException", case);
    assert_eq!(
        answer.body["message"].as_str(),
        Some(message),
        "case {case}: expected the message {message:?}, got {:?}",
        answer.body["message"]
    );
}

#[tokio::test]
async fn a_stage_addressed_call_reports_the_stage_it_cannot_resolve() {
    let gateway = Gateway::new();
    // One stored API with one stored stage supplies the halves a row keeps valid, so each
    // refusal is about the half under test alone.
    let (stored, _) = create_api(&gateway, "nested-stage-refusals").await;
    create_deployed_stage(&gateway, &stored, "prod").await;

    for (index, unknown) in UNKNOWN_APIS.iter().enumerate() {
        let unknown_stage = UNKNOWN_CHILDREN[index];
        // The three ways a stage address fails: the API, the stage, and both.
        let addresses = [
            (*unknown, "prod"),
            (stored.as_str(), unknown_stage),
            (*unknown, unknown_stage),
        ];
        for (api, stage) in addresses {
            for (operation, method, uri, body) in stage_requests(api, stage) {
                let case = format!("{operation} [{method} {uri}]");
                let answer = gateway.call(method, &uri, body).await;
                assert_reports(&answer, &case, NO_SUCH_STAGE);
            }
            // The tag calls address the same stage by its ARN.
            for (operation, method, uri, body) in stage_arn_requests(api, stage) {
                let case = format!("{operation} [{method} {uri}]");
                let answer = gateway.call(method, &uri, body).await;
                assert_reports(&answer, &case, NO_SUCH_STAGE);
            }
        }
    }

    // The same calls against the STORED address are served, so the refusals above are
    // about the address and not about the requests. The delete of the stage goes last: it
    // is the one row that removes the record the others resolve.
    for (operation, method, uri, body) in stage_requests(&stored, "prod")
        .into_iter()
        .filter(|(operation, ..)| *operation != "DeleteStage")
    {
        let answer = gateway.call(method, &uri, body).await;
        assert!(
            (200..300).contains(&answer.status),
            "case {operation}(stored prod): expected a success, got {} with {:?}",
            answer.status,
            answer.body
        );
    }
    let deleted = gateway
        .call("DELETE", &format!("/restapis/{stored}/stages/prod"), None)
        .await;
    assert!(
        (200..300).contains(&deleted.status),
        "case DeleteStage(stored prod): expected a success, got {} with {:?}",
        deleted.status,
        deleted.body
    );
}

#[tokio::test]
async fn a_stage_arn_is_reported_as_the_stage_and_an_api_arn_as_the_api() {
    let gateway = Gateway::new();
    let (stored, _) = create_api(&gateway, "stage-arn-refusals").await;
    create_deployed_stage(&gateway, &stored, "prod").await;

    // The tag calls against the STORED stage ARN are served, so the refusals the stage
    // suite above asserts are about the address and not about the ARN form.
    for (operation, method, uri, body) in stage_arn_requests(&stored, "prod") {
        let answer = gateway.call(method, &uri, body).await;
        assert!(
            (200..300).contains(&answer.status),
            "case {operation}(the stored stage's ARN): expected a success, got {} with {:?}",
            answer.status,
            answer.body
        );
    }

    // Where the rule STOPS, on the same operation: an ARN whose own record IS the API
    // keeps reporting the account-qualified API identifier.
    for unknown in UNKNOWN_APIS {
        let answer = get_tags(&gateway, &apigateway_arn(&format!("/restapis/{unknown}"))).await;
        assert_reports(
            &answer,
            &format!("GetTags(the ARN of the unknown api {unknown})"),
            &crate::rest_api_identifier_refusals::no_such_api(unknown),
        );
    }
}

#[tokio::test]
async fn a_resource_addressed_call_reports_the_resource_it_cannot_resolve() {
    let gateway = Gateway::new();
    let (stored, root) = create_api(&gateway, "nested-resource-refusals").await;
    let resource = resource_under(&gateway, &stored, &root, "pets").await;

    for (index, unknown) in UNKNOWN_APIS.iter().enumerate() {
        let unknown_resource = UNKNOWN_CHILDREN[index];
        // The three ways a resource address fails — the API, the resource, and both —
        // each reporting the RESOURCE, because the pair resolves as one lookup for it.
        let addresses = [
            (*unknown, resource.as_str()),
            (stored.as_str(), unknown_resource),
            (*unknown, unknown_resource),
        ];
        for (api, addressed) in addresses {
            for (operation, method, uri, body) in resource_requests(api, addressed) {
                let case = format!("{operation} [{method} {uri}]");
                let answer = gateway.call(method, &uri, body).await;
                assert_reports(&answer, &case, NO_SUCH_RESOURCE);
            }
        }
    }

    // The same calls against the STORED address are served. The delete of the resource
    // goes last: it is the one row that removes the record the others resolve.
    for (operation, method, uri, body) in resource_requests(&stored, &resource)
        .into_iter()
        .filter(|(operation, ..)| *operation != "DeleteResource")
    {
        let answer = gateway.call(method, &uri, body).await;
        assert!(
            (200..300).contains(&answer.status),
            "case {operation}(stored pets): expected a success, got {} with {:?}",
            answer.status,
            answer.body
        );
    }
    let deleted = gateway
        .call(
            "DELETE",
            &format!("/restapis/{stored}/resources/{resource}"),
            None,
        )
        .await;
    assert!(
        (200..300).contains(&deleted.status),
        "case DeleteResource(stored pets): expected a success, got {} with {:?}",
        deleted.status,
        deleted.body
    );
}

/// RULE: `CreateResource` resolves the `restApiId` as a precondition of its OWN, before the
/// `parentId` the new resource would hang off, so an API this account does not hold is
/// reported as the API — the account-qualified identifier sentence — and the fixed resource
/// sentence answers only a `parentId` that names no resource of an API that DOES exist.
#[tokio::test]
async fn a_create_resource_reports_the_api_before_the_parent_it_cannot_resolve() {
    let gateway = Gateway::new();
    let (stored, root) = create_api(&gateway, "create-resource-precondition").await;
    let parent = resource_under(&gateway, &stored, &root, "pets").await;

    // The three ways the address of a create fails, each over unknown identifiers differing
    // in length, alphabet and case: the API alone, the parent alone, and both at once. Where
    // the API is unresolvable the API is what the refusal names, whatever the parent is.
    for (index, unknown) in UNKNOWN_APIS.iter().enumerate() {
        let unknown_parent = UNKNOWN_CHILDREN[index];
        let rows = [
            (
                "an unresolvable api with a stored parent",
                *unknown,
                parent.as_str(),
                crate::rest_api_identifier_refusals::no_such_api(unknown),
            ),
            (
                "a stored api with an unresolvable parent",
                stored.as_str(),
                unknown_parent,
                NO_SUCH_RESOURCE.to_string(),
            ),
            (
                "an unresolvable api and parent",
                *unknown,
                unknown_parent,
                crate::rest_api_identifier_refusals::no_such_api(unknown),
            ),
        ];
        for (position, api, addressed, message) in rows {
            let case = format!("CreateResource with {position} [{api} {addressed}]");
            let answer = gateway
                .call(
                    "POST",
                    &format!("/restapis/{api}/resources/{addressed}"),
                    Some(json!({ "pathPart": "kittens" })),
                )
                .await;
            assert_reports(&answer, &case, &message);
        }
    }

    // The same call against the STORED address is served, so the refusals above are about
    // the address and not about the request.
    let created = gateway
        .call(
            "POST",
            &format!("/restapis/{stored}/resources/{parent}"),
            Some(json!({ "pathPart": "kittens" })),
        )
        .await;
    assert!(
        (200..300).contains(&created.status),
        "case CreateResource(the stored parent): expected a success, got {} with {:?}",
        created.status,
        created.body
    );
}

#[tokio::test]
async fn a_call_addressed_by_a_record_under_a_resource_reports_that_resource() {
    let gateway = Gateway::new();
    let (stored, root) = create_api(&gateway, "nested-resource-child-refusals").await;
    let resource = resource_under(&gateway, &stored, &root, "pets").await;
    // One stored method, with an integration and one response on each of the two records
    // that carry responses, so every row below names a record the STORED address holds and
    // the refusals are about the address alone.
    put_method(&gateway, &stored, &resource, "GET").await;
    let integration = put_integration(
        &gateway,
        &stored,
        &resource,
        "GET",
        json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": "{\"statusCode\": 200}" }
        }),
    )
    .await;
    assert!(
        (200..300).contains(&integration.status),
        "case PutIntegration(stored pets GET): expected a success, got {} with {:?}",
        integration.status,
        integration.body
    );
    let method_response = crate::method_response::put_method_response(
        &gateway,
        &stored,
        &resource,
        "GET",
        "200",
        json!({}),
    )
    .await;
    assert!(
        (200..300).contains(&method_response.status),
        "case PutMethodResponse(stored pets GET 200): expected a success, got {} with {:?}",
        method_response.status,
        method_response.body
    );
    let integration_response = crate::integration_response::put_integration_response(
        &gateway,
        &stored,
        &resource,
        "GET",
        "200",
        json!({ "responseTemplates": { "application/json": "{\"pets\":[]}" } }),
    )
    .await;
    assert!(
        (200..300).contains(&integration_response.status),
        "case PutIntegrationResponse(stored pets GET 200): expected a success, got {} with {:?}",
        integration_response.status,
        integration_response.body
    );

    for (index, unknown) in UNKNOWN_APIS.iter().enumerate() {
        let unknown_resource = UNKNOWN_CHILDREN[index];
        // The same three ways the address fails, reported as the RESOURCE the record hangs
        // off: the records UNDER a resource are addressed through the same pair, so an API
        // this account does not hold leaves that resource unresolvable too.
        let addresses = [
            (*unknown, resource.as_str()),
            (stored.as_str(), unknown_resource),
            (*unknown, unknown_resource),
        ];
        for (api, addressed) in addresses {
            for (operation, method, uri, body) in resource_child_requests(api, addressed) {
                let case = format!("{operation} [{method} {uri}]");
                let answer = gateway.call(method, &uri, body).await;
                assert_reports(&answer, &case, NO_SUCH_RESOURCE);
            }
        }
    }

    // The same calls against the STORED address are served, so the refusals above are
    // about the address and not about the requests.
    for (operation, method, uri, body) in resource_child_requests(&stored, &resource) {
        let answer = gateway.call(method, &uri, body).await;
        assert!(
            (200..300).contains(&answer.status),
            "case {operation}(stored pets GET): expected a success, got {} with {:?}",
            answer.status,
            answer.body
        );
    }
}
