// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `UpdateIntegration`.
//!
//! The rule: **`PATCH /v2/apis/{ApiId}/integrations/{IntegrationId}` is a patch of the
//! STORED integration** — a member the request carries replaces the stored one, a member
//! it omits leaves the stored value as it was, what the call reports is what a later
//! `GetIntegration` reports — **and the MERGED record is held to the same rules
//! `CreateIntegration` holds a fresh one to**, because those rules belong to the record
//! and not to the operation that wrote it. An `ApiId` or `IntegrationId` the store does
//! not hold is `NotFoundException` (404, `smithy_query(query="operation",
//! operation="UpdateIntegration")`).
//!
//! An OPTIONAL string member carried EMPTY is the one exception to "replaces": it asks for
//! the stored member to be REMOVED, so neither the patch's answer nor any later read
//! reports it.
//!
//! The rows span what the rule generalises over: the member's type (string, integer,
//! map, structure), how many members one patch names (none, one, several), the api's
//! protocol, and which rule the merged record would break (a URI dropped from an
//! integration that needs one, a payload format version its new type does not support, a
//! selection expression naming no variable the service evaluates).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator, error_code};

fn http_api(name: &str) -> Value {
    json!({ "name": name, "protocolType": "HTTP" })
}

fn websocket_api(name: &str) -> Value {
    json!({
        "name": name,
        "protocolType": "WEBSOCKET",
        "routeSelectionExpression": "$request.body.action"
    })
}

/// An HTTP_PROXY integration: the one type both protocols support, with the members an
/// HTTP api requires.
fn proxy_integration() -> Value {
    json!({
        "integrationType": "HTTP_PROXY",
        "integrationUri": "https://example.com/upstream",
        "payloadFormatVersion": "1.0",
        "integrationMethod": "ANY",
        "description": "the integration as created",
        "timeoutInMillis": 15_000,
        "requestParameters": { "append:header.x-trace": "$context.requestId" }
    })
}

/// Create one api and answer its `ApiId`.
async fn create_api(emulator: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let (status, created) = call(emulator, "POST", "/v2/apis", Some(request.clone())).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {created}"
    );
    as_str(&created, "apiId")
}

/// Create one integration and answer its `IntegrationId`.
async fn create_integration(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    request: &Value,
) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(request.clone()),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateIntegration, got {status} {created}"
    );
    as_str(&created, "integrationId")
}

/// Read one integration back and answer `(status, body)`.
async fn get_integration(
    emulator: &MultiServiceRouter,
    api_id: &str,
    integration_id: &str,
) -> (u16, Value) {
    call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
        None,
    )
    .await
}

/// Patch one integration and answer `(status, body)`.
async fn update_integration(
    emulator: &MultiServiceRouter,
    api_id: &str,
    integration_id: &str,
    patch: &Value,
) -> (u16, Value) {
    call(
        emulator,
        "PATCH",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
        Some(patch.clone()),
    )
    .await
}

/// The record `created` becomes once `patch` is applied: every member the patch names
/// takes the patch's value, every other member keeps the stored one.
fn patched_record(created: &Value, patch: &Value) -> Value {
    let mut expected = created.clone();
    for (member, value) in patch.as_object().expect("the patch is a JSON object") {
        expected[member] = value.clone();
    }
    expected
}

/// One patch row: the request body sent to `UpdateIntegration`.
struct PatchRow {
    case: &'static str,
    patch: Value,
}

fn patch_rows() -> Vec<PatchRow> {
    vec![
        PatchRow {
            case: "one string member",
            patch: json!({ "description": "a rewritten description" }),
        },
        PatchRow {
            case: "the integration's endpoint",
            patch: json!({ "integrationUri": "https://example.com/elsewhere" }),
        },
        PatchRow {
            case: "an integer member",
            patch: json!({ "timeoutInMillis": 20_000 }),
        },
        PatchRow {
            case: "an integer member at the published minimum",
            patch: json!({ "timeoutInMillis": 50 }),
        },
        PatchRow {
            case: "an integer member at the published maximum",
            patch: json!({ "timeoutInMillis": 30_000 }),
        },
        PatchRow {
            case: "a map with different keys replaces the stored map",
            patch: json!({
                "requestParameters": {
                    "overwrite:path": "$request.path",
                    "append:header.x-source": "'api-gateway'"
                }
            }),
        },
        PatchRow {
            case: "an empty map clears the stored map",
            patch: json!({ "requestParameters": {} }),
        },
        PatchRow {
            case: "a structure member",
            patch: json!({ "tlsConfig": { "serverNameToVerify": "upstream.example.com" } }),
        },
        PatchRow {
            case: "the credentials the integration assumes",
            patch: json!({ "credentialsArn": "arn:aws:iam::000000000000:role/apigw" }),
        },
        PatchRow {
            case: "several members of different types at once",
            patch: json!({
                "description": "patched together",
                "integrationMethod": "POST",
                "timeoutInMillis": 1_000,
                "requestParameters": { "overwrite:header.x-key": "'static'" }
            }),
        },
        PatchRow {
            case: "a patch naming no member leaves the record as it is",
            patch: json!({}),
        },
    ]
}

#[tokio::test]
async fn update_integration_replaces_the_members_it_names_and_keeps_the_rest() {
    let emulator = emulator();

    for (index, row) in patch_rows().into_iter().enumerate() {
        let api_id = create_api(
            &emulator,
            row.case,
            &http_api(&format!("integration-patch-{index}")),
        )
        .await;
        let integration_id =
            create_integration(&emulator, row.case, &api_id, &proxy_integration()).await;
        let (status, created) = get_integration(&emulator, &api_id, &integration_id).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from GetIntegration on the fresh record, got {status} {created}",
            row.case
        );

        let (status, updated) =
            update_integration(&emulator, &api_id, &integration_id, &row.patch).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from UpdateIntegration, got {status} {updated}",
            row.case
        );
        let expected = patched_record(&created, &row.patch);
        assert_eq!(
            updated, expected,
            "case {:?}: UpdateIntegration must report the patched record; expected {expected}, got {updated}",
            row.case
        );

        // The write is what a later caller sees, not just what this call answered.
        let (status, fetched) = get_integration(&emulator, &api_id, &integration_id).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from GetIntegration after the patch, got {status} {fetched}",
            row.case
        );
        assert_eq!(
            fetched, expected,
            "case {:?}: GetIntegration must report what UpdateIntegration stored",
            row.case
        );
        assert_eq!(
            as_str(&fetched, "integrationId"),
            integration_id,
            "case {:?}: the patch must not change the IntegrationId, got {fetched}",
            row.case
        );
    }
}

#[tokio::test]
async fn a_patch_whose_merged_integration_breaks_a_rule_is_refused_and_changes_nothing() {
    let emulator = emulator();

    let http = create_api(&emulator, "merged rules", &http_api("merged-rules-http")).await;
    let socket = create_api(&emulator, "merged rules", &websocket_api("merged-rules-socket")).await;

    // A Lambda proxy integration, which may carry payload format version 2.0, and a
    // WebSocket `AWS` integration, which may carry a template selection expression.
    let lambda = create_integration(
        &emulator,
        "the lambda proxy",
        &http,
        &json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": "arn:aws:lambda:eu-west-1:000000000000:function:handler",
            "payloadFormatVersion": "2.0"
        }),
    )
    .await;
    let subtype = create_integration(
        &emulator,
        "the service-action integration",
        &http,
        &json!({
            "integrationType": "AWS_PROXY",
            "integrationSubtype": "SQS-SendMessage",
            "payloadFormatVersion": "1.0",
            "credentialsArn": "arn:aws:iam::000000000000:role/apigw",
            // The arguments that action requires, which a first-class subtype integration
            // is written only with.
            "requestParameters": {
                "QueueUrl": "$request.header.queue",
                "MessageBody": "$request.body.message"
            }
        }),
    )
    .await;
    let websocket_aws = create_integration(
        &emulator,
        "the websocket AWS integration",
        &socket,
        &json!({
            "integrationType": "AWS",
            "integrationUri": "arn:aws:lambda:eu-west-1:000000000000:function:ws",
            "integrationMethod": "POST"
        }),
    )
    .await;

    let rows = [
        (
            "a payload format version the new type does not support",
            &http,
            &lambda,
            json!({ "integrationType": "HTTP_PROXY", "integrationUri": "https://example.com/x" }),
        ),
        (
            "payload format version 2.0 moved onto an HTTP_PROXY integration",
            &http,
            &lambda,
            json!({ "integrationType": "HTTP_PROXY", "payloadFormatVersion": "2.0" }),
        ),
        (
            "a subtype moved onto an integration that is not AWS_PROXY",
            &http,
            &lambda,
            json!({ "integrationType": "HTTP_PROXY", "integrationSubtype": "SQS-SendMessage" }),
        ),
        (
            "a service-action integration moved to a type that needs a URI",
            &http,
            &subtype,
            json!({ "integrationType": "HTTP_PROXY" }),
        ),
        (
            "a selection expression naming no variable the service evaluates",
            &socket,
            &websocket_aws,
            json!({ "templateSelectionExpression": "$default" }),
        ),
        (
            "a VPC_LINK connection with no link to connect through",
            &http,
            &lambda,
            json!({ "connectionType": "VPC_LINK" }),
        ),
    ];

    for (case, api_id, integration_id, patch) in rows {
        let (_, before) = get_integration(&emulator, api_id, integration_id).await;
        let (status, body) = update_integration(&emulator, api_id, integration_id, &patch).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from UpdateIntegration, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {body}"
        );
        let (_, after) = get_integration(&emulator, api_id, integration_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused UpdateIntegration must leave the record as it was"
        );
    }

    // And a patch whose merged record DOES hold together is applied: the same move of
    // the Lambda proxy to an HTTP endpoint, with the payload format that type supports.
    let (status, updated) = update_integration(
        &emulator,
        &http,
        &lambda,
        &json!({
            "integrationType": "HTTP_PROXY",
            "integrationUri": "https://example.com/upstream",
            "payloadFormatVersion": "1.0"
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from a patch whose merged record holds together, got {status} {updated}"
    );
    assert_eq!(
        as_str(&updated, "integrationType"),
        "HTTP_PROXY",
        "the patched IntegrationType must be stored, got {updated}"
    );
}

#[tokio::test]
async fn update_integration_on_a_record_the_store_does_not_hold_is_not_found() {
    let emulator = emulator();

    let api_id = create_api(&emulator, "not found", &http_api("integration-patch-404")).await;
    let integration_id =
        create_integration(&emulator, "the live integration", &api_id, &proxy_integration()).await;

    // One integration of this api, deleted again, so its id USED to name one here.
    let gone = create_integration(&emulator, "the deleted one", &api_id, &proxy_integration()).await;
    let (status, body) = call(
        &emulator,
        "DELETE",
        &format!("/v2/apis/{api_id}/integrations/{gone}"),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "expected 204 from DeleteIntegration, got {status} {body}"
    );

    // And another api, holding no integration at all.
    let empty_api = create_api(&emulator, "an empty api", &http_api("integration-patch-empty")).await;

    let rows = [
        ("a deleted integration's id", api_id.clone(), gone),
        (
            "an id no integration was ever minted with",
            api_id.clone(),
            "zzzzzzzzzz".to_string(),
        ),
        (
            "an id of an integration of ANOTHER api",
            empty_api.clone(),
            integration_id.clone(),
        ),
        (
            "an api no record holds",
            "zzzzzzzzzz".to_string(),
            integration_id.clone(),
        ),
    ];

    for (case, api, integration) in rows {
        let (status, body) = update_integration(
            &emulator,
            &api,
            &integration,
            &json!({ "description": "patching the absent" }),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404 from UpdateIntegration, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "NotFoundException",
            "case {case:?}: expected NotFoundException, got {body}"
        );
    }

    // The live integration is untouched by the refusals and still patchable.
    let (status, updated) = update_integration(
        &emulator,
        &api_id,
        &integration_id,
        &json!({ "description": "still here" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 patching the live integration, got {status} {updated}"
    );
    assert_eq!(
        as_str(&updated, "description"),
        "still here",
        "the live integration must carry the patched description, got {updated}"
    );
}

/// One row of the clear table: the api to create the integration under, the integration
/// body, the member cleared, and the value it is patched to afterwards.
struct ClearRow {
    case: &'static str,
    /// `true` for an HTTP api, `false` for a WebSocket one — two of these members are
    /// supported only on an api of one protocol.
    http: bool,
    integration: Value,
    member: &'static str,
    restored: &'static str,
}

/// The integration's OPTIONAL string members, over integrations of both protocols: a
/// description, the ARN of the role the integration assumes, and the template selection
/// expression a WebSocket integration selects a request template with.
fn clear_rows() -> Vec<ClearRow> {
    vec![
        ClearRow {
            case: "the description",
            http: true,
            integration: proxy_integration(),
            member: "description",
            restored: "re-described after the clear",
        },
        ClearRow {
            case: "the credentials the integration assumes",
            http: true,
            integration: {
                let mut integration = proxy_integration();
                integration["credentialsArn"] = json!("arn:aws:iam::000000000000:role/apigw");
                integration
            },
            member: "credentialsArn",
            restored: "arn:aws:iam::000000000000:role/other",
        },
        ClearRow {
            case: "a WebSocket integration's template selection expression",
            http: false,
            integration: json!({
                "integrationType": "MOCK",
                "templateSelectionExpression": "$request.body.action",
                "requestTemplates": { "application/json": "{}" }
            }),
            member: "templateSelectionExpression",
            restored: "$request.body.kind",
        },
    ]
}

#[tokio::test]
async fn an_optional_member_carried_empty_is_removed_from_the_integration() {
    let emulator = emulator();

    for (index, row) in clear_rows().into_iter().enumerate() {
        let case = row.case;
        let member = row.member;
        let request = if row.http {
            http_api(&format!("integration-clear-http-{index}"))
        } else {
            websocket_api(&format!("integration-clear-socket-{index}"))
        };
        let api_id = create_api(&emulator, case, &request).await;
        let integration_id = create_integration(&emulator, case, &api_id, &row.integration).await;
        let (_, created) = get_integration(&emulator, &api_id, &integration_id).await;
        assert!(
            created.get(member).is_some(),
            "case {case:?}: the created integration must carry {member:?}, got {created}"
        );

        // The empty string clears it, in the patch's own answer and for every later reader.
        let (status, cleared) =
            update_integration(&emulator, &api_id, &integration_id, &json!({ member: "" })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from UpdateIntegration, got {status} {cleared}"
        );
        assert_eq!(
            cleared.get(member),
            None,
            "case {case:?}: expected the cleared member to be absent, got {cleared}"
        );
        let (status, fetched) = get_integration(&emulator, &api_id, &integration_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetIntegration, got {status} {fetched}"
        );
        assert_eq!(
            fetched.get(member),
            None,
            "case {case:?}: expected GetIntegration to report no such member, got {fetched}"
        );

        // And a value sent afterwards is stored as written.
        let (status, restored) = update_integration(
            &emulator,
            &api_id,
            &integration_id,
            &json!({ member: row.restored }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 re-setting {member:?}, got {status} {restored}"
        );
        let (_, read_back) = get_integration(&emulator, &api_id, &integration_id).await;
        assert_eq!(
            read_back.get(member).and_then(Value::as_str),
            Some(row.restored),
            "case {case:?}: expected GetIntegration to report the patched value, got {read_back}"
        );
    }
}
