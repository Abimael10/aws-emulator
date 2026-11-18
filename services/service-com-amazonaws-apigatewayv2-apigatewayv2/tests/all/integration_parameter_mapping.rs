// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the headers an HTTP api's PARAMETER MAPPING may not operate
//! on.
//!
//! The rule: a parameter mapping of an HTTP api's integration — the
//! `<action>:<header|querystring|path>.<location>` map
//! `CreateIntegration$RequestParameters` documents for an integration naming no
//! `IntegrationSubtype`, and the same language in `ResponseParameters` — may not operate on
//! a header API Gateway reserves for itself, and a write that configures one is refused
//! `400 BadRequestException` carrying API Gateway's own sentence, with the header name taken
//! from the request's mapping key. Every writer of an integration is held to it: the create,
//! the patch, and the import that writes one from an OpenAPI definition.
//!
//! The rows span what the rule generalises over: each reserved header and each reserved
//! prefix, each of the three mapping actions, the casing the key is written in, headers a
//! mapping may operate on, the operands that are not a header at all, and the two other
//! mapping languages the rule says nothing about (a subtype integration's action parameters
//! and a WebSocket api's method request parameters). The recorded case that filed the issue
//! — `overwrite:header.authorization` on an `HTTP_PROXY` integration — is ONE row.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_import::{import, stored_integrations};
use crate::api_lifecycle::{as_str, call, emulator, error_code};

/// The headers API Gateway reserves from a parameter mapping, as
/// `crate::state::parameter_mapping` states them: the credential header the gateway's own
/// authorization reads, and the connection-specific and framing headers a proxy owns
/// (RFC 9110 §7.6.1, RFC 9112 §6).
const RESERVED_HEADERS: &[&str] = &[
    "authorization",
    "connection",
    "content-length",
    "host",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// The header-name prefixes AWS reserves for its own signalling.
const RESERVED_PREFIXES: &[&str] = &["apigw-", "x-amz-", "x-amzn-"];

/// Headers a mapping MAY operate on, including names that merely begin with the characters
/// of a reserved one — a different header.
const MAPPABLE_HEADERS: &[&str] = &[
    "x-api-key",
    "content-type",
    "accept",
    "user-agent",
    "x-custom-header",
    "hostname",
    "connection-id",
    "x-amzcustom",
];

/// The three actions a mapping key may name ("action can be append, overwrite or remove",
/// `CreateIntegration$RequestParameters`).
const ACTIONS: &[&str] = &["append", "overwrite", "remove"];

/// The sentence API Gateway refuses a mapping on a reserved header with. Only the header
/// name varies, and it is the one the request's mapping key spelled.
fn restricted(header: &str) -> String {
    format!(
        "Invalid mapping expression specified: Validation Result: warnings : [], errors : \
         [Operations on header {header} are restricted]"
    )
}

/// Create one HTTP api and answer its `ApiId`.
async fn http_api(emulator: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": name, "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateApi for {name}, got {status} {created}"
    );
    as_str(&created, "apiId")
}

/// Create one WEBSOCKET api and answer its `ApiId`.
async fn websocket_api(emulator: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({
            "name": name,
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateApi for {name}, got {status} {created}"
    );
    as_str(&created, "apiId")
}

async fn create_integration(
    emulator: &MultiServiceRouter,
    api_id: &str,
    request: &Value,
) -> (u16, Value) {
    call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(request.clone()),
    )
    .await
}

/// An `HTTP_PROXY` integration of an HTTP api carrying `request_parameters` as its mapping —
/// the integration the recorded case sends.
fn http_proxy(request_parameters: Value) -> Value {
    json!({
        "integrationType": "HTTP_PROXY",
        "integrationUri": "https://upstream.example.com/orders",
        "integrationMethod": "GET",
        "payloadFormatVersion": "1.0",
        "requestParameters": request_parameters
    })
}

/// How many integrations the api holds, so a refused write can be shown to have stored
/// nothing.
async fn integration_count(emulator: &MultiServiceRouter, api_id: &str) -> usize {
    let (status, listed) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/integrations"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from GetIntegrations, got {status} {listed}"
    );
    listed["items"].as_array().map_or(0, Vec::len)
}

/// Every reserved header, in each spelling the test drives it in, paired with the action the
/// mapping key names it under.
fn reserved_cases() -> Vec<(String, &'static str)> {
    let mut cases = Vec::new();
    for (index, header) in RESERVED_HEADERS.iter().enumerate() {
        let action = ACTIONS[index % ACTIONS.len()];
        cases.push(((*header).to_string(), action));
        // A header name is case-insensitive (RFC 9110 §5.1), so the same header spelled in
        // another casing is the same header.
        cases.push((header.to_ascii_uppercase(), ACTIONS[(index + 1) % ACTIONS.len()]));
    }
    for (index, prefix) in RESERVED_PREFIXES.iter().enumerate() {
        cases.push((format!("{prefix}request-id"), ACTIONS[index % ACTIONS.len()]));
    }
    cases
}

#[tokio::test]
async fn create_integration_refuses_a_request_mapping_on_a_reserved_header() {
    let emulator = emulator();

    for (index, (header, action)) in reserved_cases().into_iter().enumerate() {
        let api_id = http_api(&emulator, &format!("reserved-request-{index}")).await;
        let key = format!("{action}:header.{header}");
        let (status, body) =
            create_integration(&emulator, &api_id, &http_proxy(json!({ &key: "'a-value'" }))).await;

        assert_eq!(
            status, 400,
            "case {key:?}: expected 400 from CreateIntegration, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {key:?}: expected BadRequestException, got {body}"
        );
        assert_eq!(
            as_str(&body, "message"),
            restricted(&header),
            "case {key:?}: wrong refusal sentence, got {body}"
        );
        assert_eq!(
            integration_count(&emulator, &api_id).await,
            0,
            "case {key:?}: a refused create must store no integration"
        );
    }
}

#[tokio::test]
async fn create_integration_stores_a_mapping_on_every_other_operand() {
    let emulator = emulator();

    // A mapping the rule says nothing about: a header that is not reserved, under each
    // action, and the operands that are not a header at all.
    let mut keys: Vec<String> = Vec::new();
    for header in MAPPABLE_HEADERS {
        for action in ACTIONS {
            keys.push(format!("{action}:header.{header}"));
        }
    }
    keys.push("overwrite:querystring.page".to_string());
    keys.push("append:querystring.tag".to_string());
    keys.push("overwrite:path".to_string());

    for (index, key) in keys.into_iter().enumerate() {
        let api_id = http_api(&emulator, &format!("mappable-{index}")).await;
        let mapping = json!({ &key: "'a-value'" });
        let (status, body) =
            create_integration(&emulator, &api_id, &http_proxy(mapping.clone())).await;
        assert_eq!(
            status, 201,
            "case {key:?}: expected 201 from CreateIntegration, got {status} {body}"
        );

        let integration_id = as_str(&body, "integrationId");
        let (status, fetched) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {key:?}: expected 200 from GetIntegration, got {status} {fetched}"
        );
        assert_eq!(
            fetched.get("requestParameters"),
            Some(&mapping),
            "case {key:?}: GetIntegration must report the mapping that was stored, got {fetched}"
        );
    }
}

#[tokio::test]
async fn a_response_mapping_is_held_to_the_same_reserved_headers() {
    let emulator = emulator();

    // `ResponseParameters` is the same language keyed by the status code the mapping is
    // selected for, so the reserved headers are the reserved headers on that leg too — and
    // a mapping of the status code itself is not an operation on a header.
    let rows: &[(&str, Value, Option<&str>)] = &[
        (
            "a reserved header on the 200 selection",
            json!({ "200": { "overwrite:header.transfer-encoding": "'chunked'" } }),
            Some("transfer-encoding"),
        ),
        (
            "a reserved prefix on the 404 selection",
            json!({ "404": { "append:header.x-amzn-trace-id": "'root=1'" } }),
            Some("x-amzn-trace-id"),
        ),
        (
            "a mappable header",
            json!({ "200": { "overwrite:header.x-page": "'1'" } }),
            None,
        ),
        (
            "the status code itself, which is no header",
            json!({ "200": { "overwrite:statuscode": "201" } }),
            None,
        ),
    ];

    for (index, (case, response_parameters, refused_header)) in rows.iter().enumerate() {
        let api_id = http_api(&emulator, &format!("reserved-response-{index}")).await;
        let request = json!({
            "integrationType": "HTTP_PROXY",
            "integrationUri": "https://upstream.example.com/orders",
            "integrationMethod": "GET",
            "payloadFormatVersion": "1.0",
            "responseParameters": response_parameters
        });
        let (status, body) = create_integration(&emulator, &api_id, &request).await;

        match refused_header {
            Some(header) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected 400 from CreateIntegration, got {status} {body}"
                );
                assert_eq!(
                    as_str(&body, "message"),
                    restricted(header),
                    "case {case:?}: wrong refusal sentence, got {body}"
                );
                assert_eq!(
                    integration_count(&emulator, &api_id).await,
                    0,
                    "case {case:?}: a refused create must store no integration"
                );
            }
            None => {
                assert_eq!(
                    status, 201,
                    "case {case:?}: expected 201 from CreateIntegration, got {status} {body}"
                );
                assert_eq!(
                    body.get("responseParameters"),
                    Some(response_parameters),
                    "case {case:?}: the stored mapping must be the one sent, got {body}"
                );
            }
        }
    }
}

#[tokio::test]
async fn update_integration_refuses_a_mapping_on_a_reserved_header_and_keeps_the_stored_one() {
    let emulator = emulator();
    let api_id = http_api(&emulator, "reserved-patch").await;

    // An integration that already carries a mapping the rule admits.
    let stored = json!({ "overwrite:header.x-api-key": "'a-value'" });
    let (status, created) =
        create_integration(&emulator, &api_id, &http_proxy(stored.clone())).await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateIntegration, got {status} {created}"
    );
    let integration_id = as_str(&created, "integrationId");
    let path = format!("/v2/apis/{api_id}/integrations/{integration_id}");

    // A patch that would put a mapping on a reserved header is refused, whichever map it
    // sends it in, and leaves the stored mapping exactly as it was.
    let patches: &[(&str, Value, &str)] = &[
        (
            "a request mapping on the credential header",
            json!({ "requestParameters": { "overwrite:header.authorization": "'Bearer x'" } }),
            "authorization",
        ),
        (
            "a request mapping on a framing header",
            json!({ "requestParameters": { "remove:header.Content-Length": "''" } }),
            "Content-Length",
        ),
        (
            "a response mapping on a reserved prefix",
            json!({
                "responseParameters": { "200": { "append:header.x-amz-meta": "'m'" } }
            }),
            "x-amz-meta",
        ),
    ];
    for (case, patch, header) in patches {
        let (status, body) = call(&emulator, "PATCH", &path, Some((*patch).clone())).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from UpdateIntegration, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {body}"
        );
        assert_eq!(
            as_str(&body, "message"),
            restricted(header),
            "case {case:?}: wrong refusal sentence, got {body}"
        );

        let (status, fetched) = call(&emulator, "GET", &path, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetIntegration, got {status} {fetched}"
        );
        assert_eq!(
            fetched.get("requestParameters"),
            Some(&stored),
            "case {case:?}: a refused patch must leave the stored mapping as it was, got \
             {fetched}"
        );
    }

    // A patch onto a header the rule admits is applied, so the refusal above is the
    // reserved header's and not the patch's.
    let replacement = json!({ "append:header.x-page": "'2'" });
    let (status, patched) = call(
        &emulator,
        "PATCH",
        &path,
        Some(json!({ "requestParameters": replacement.clone() })),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from UpdateIntegration, got {status} {patched}"
    );
    assert_eq!(
        patched.get("requestParameters"),
        Some(&replacement),
        "the patched mapping must be the one sent, got {patched}"
    );
}

#[tokio::test]
async fn the_other_two_mapping_languages_carry_the_same_key_untouched() {
    let emulator = emulator();

    // An AWS_PROXY integration naming a subtype maps the ACTION's own parameters, and a
    // WebSocket api's integration maps a method request parameter onto an integration
    // request parameter: neither map is an operation on a header, so a key that merely
    // reads like one is stored.
    let key = "overwrite:header.authorization";

    let api_id = http_api(&emulator, "subtype-mapping").await;
    let (status, created) = create_integration(
        &emulator,
        &api_id,
        &json!({
            "integrationType": "AWS_PROXY",
            "integrationSubtype": "SQS-SendMessage",
            "payloadFormatVersion": "1.0",
            "credentialsArn": "arn:aws:iam::000000000000:role/apigw",
            "requestParameters": {
                "QueueUrl": "$request.header.queue",
                // The other argument the action requires, so the integration is one this
                // service writes at all.
                "MessageBody": "$request.body.message",
                key: "'a-value'"
            }
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "a subtype integration's action parameters are a different language and must be \
         stored, got {status} {created}"
    );

    let socket_id = websocket_api(&emulator, "socket-mapping").await;
    let (status, created) = create_integration(
        &emulator,
        &socket_id,
        &json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": "arn:aws:lambda:eu-west-1:000000000000:function:ws",
            "requestParameters": {
                "integration.request.header.authorization": "method.request.header.token",
                key: "'a-value'"
            }
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "a WebSocket api's integration request parameters are a different language and must \
         be stored, got {status} {created}"
    );
}

#[tokio::test]
async fn an_imported_definition_cannot_write_a_mapping_on_a_reserved_header() {
    let emulator = emulator();

    // An import holds each integration to the rules of the operation that would have
    // created it, and a problem with one path is a WARNING rather than the import's
    // refusal: the api is created, the route is imported with no target, and the
    // integration the definition declared is not created.
    let definition = json!({
        "openapi": "3.0.1",
        "info": { "title": "imported-reserved-mapping", "version": "1.0" },
        "paths": {
            "/orders": {
                "get": {
                    "x-amazon-apigateway-integration": {
                        "type": "http_proxy",
                        "httpMethod": "GET",
                        "uri": "https://upstream.example.com/orders",
                        "payloadFormatVersion": "1.0",
                        "requestParameters": {
                            "overwrite:header.Authorization": "'Bearer x'"
                        }
                    }
                }
            }
        }
    })
    .to_string();

    let (status, imported) = import(&emulator, "", &definition).await;
    assert_eq!(
        status, 200,
        "expected 200 from ImportApi, got {status} {imported}"
    );
    let api_id = as_str(&imported, "apiId");
    assert!(
        stored_integrations(&emulator, &api_id).await.is_empty(),
        "an integration whose mapping operates on a reserved header must not be created, \
         got {:?}",
        stored_integrations(&emulator, &api_id).await
    );
    let warnings: Vec<String> = imported["warnings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains(&restricted("Authorization"))),
        "the import must report why the integration was not created, got {warnings:?}"
    );
}
