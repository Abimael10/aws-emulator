// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for WHICH authorizer a route may be associated with.
//!
//! The rule: a route whose `AuthorizationType` hands the authorization decision to an
//! `Authorizer` resource has to name one of its OWN api whose `AuthorizerType` authorizes
//! that type — `Route$AuthorizationType` pairs "JWT for using JSON Web Tokens" with
//! `Authorizer$AuthorizerType`'s "Specify JWT to use JSON Web Tokens", and "CUSTOM for
//! using a Lambda authorizer" with "Specify REQUEST for a Lambda function using incoming
//! request parameters" — so a route naming an id the api holds no authorizer under, or one
//! belonging to an authorizer of the other kind, is refused `BadRequestException` (400)
//! with the sentence that authorization type states, and nothing is stored. A route whose
//! type decides without an authorizer (`NONE`, `AWS_IAM`) is held only to the id resolving
//! at all.
//!
//! The rows span what the rule generalises over: the api's protocol (an HTTP api, which
//! admits both kinds of authorizer, and a WebSocket api, which admits only `REQUEST`), the
//! authorization type, the kind of authorizer the id names, an id another api minted and an
//! id nothing minted, and both writers of a route (the create and the patch).

use serde_json::json;
use service_com_amazonaws_apigatewayv2_apigatewayv2::state::invalid_authorizer_reference_message;
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator, error_code};

/// A well-formed Lambda authorizer URI, the form `AuthorizerUri` documents for a `REQUEST`
/// authorizer: "a well-formed Lambda function URI, for example,
/// arn:aws:apigateway:{region}:lambda:path/2015-03-31/functions/{function-arn}/invocations".
fn lambda_uri() -> String {
    "arn:aws:apigateway:eu-west-1:lambda:path/2015-03-31/functions/\
     arn:aws:lambda:eu-west-1:000000000000:function:pairing-authorizer/invocations"
        .to_string()
}

/// Create one api of `protocol_type` and answer its `ApiId`.
async fn create_api(emulator: &MultiServiceRouter, name: &str, protocol_type: &str) -> String {
    let mut request = json!({ "name": name, "protocolType": protocol_type });
    if protocol_type == "WEBSOCKET" {
        // "This property is required for WebSocket APIs"
        // (`CreateApi$RouteSelectionExpression`).
        request["routeSelectionExpression"] = json!("$request.body.action");
    }
    let (status, created) = call(emulator, "POST", "/v2/apis", Some(request)).await;
    assert_eq!(
        status, 201,
        "case {name:?}: expected 201 from CreateApi, got {status} {created}"
    );
    as_str(&created, "apiId")
}

/// Create one authorizer of `authorizer_type` under `api_id` and answer its `AuthorizerId`.
///
/// Each type is sent the configuration its own documentation requires: a `JWT` authorizer
/// names the issuer its tokens come from, a `REQUEST` authorizer the Lambda it invokes plus
/// the payload format version that is "Required for HTTP API Lambda authorizers".
async fn create_authorizer(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    protocol_type: &str,
    authorizer_type: &str,
) -> String {
    let mut request = json!({
        "name": format!("pairing-{}", authorizer_type.to_ascii_lowercase()),
        "authorizerType": authorizer_type,
        "identitySource": ["$request.header.Authorization"]
    });
    if authorizer_type == "JWT" {
        request["jwtConfiguration"] = json!({ "issuer": "https://pairing-issuer.test/oauth2" });
    } else {
        request["authorizerUri"] = json!(lambda_uri());
        if protocol_type == "HTTP" {
            request["authorizerPayloadFormatVersion"] = json!("2.0");
        }
    }
    let (status, created) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/authorizers"),
        Some(request),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateAuthorizer, got {status} {created}"
    );
    as_str(&created, "authorizerId")
}

/// The route key a row asks for, written in the grammar `protocol_type` routes on.
fn route_key(protocol_type: &str, case_index: usize) -> String {
    if protocol_type == "WEBSOCKET" {
        return format!("action{case_index}");
    }
    format!("GET /pairing-{case_index}")
}

/// One row: the api's protocol, the route's `AuthorizationType`, the kind of authorizer its
/// `AuthorizerId` names, and whether the association holds.
struct Row {
    case: &'static str,
    protocol_type: &'static str,
    authorization_type: &'static str,
    /// The `AuthorizerType` whose minted id the route names.
    names: &'static str,
    accepted: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an HTTP api's JWT route on its JWT authorizer",
            protocol_type: "HTTP",
            authorization_type: "JWT",
            names: "JWT",
            accepted: true,
        },
        Row {
            case: "an HTTP api's JWT route on its REQUEST authorizer",
            protocol_type: "HTTP",
            authorization_type: "JWT",
            names: "REQUEST",
            accepted: false,
        },
        Row {
            case: "an HTTP api's CUSTOM route on its REQUEST authorizer",
            protocol_type: "HTTP",
            authorization_type: "CUSTOM",
            names: "REQUEST",
            accepted: true,
        },
        Row {
            case: "an HTTP api's CUSTOM route on its JWT authorizer",
            protocol_type: "HTTP",
            authorization_type: "CUSTOM",
            names: "JWT",
            accepted: false,
        },
        Row {
            case: "an HTTP api's open route on its JWT authorizer",
            protocol_type: "HTTP",
            authorization_type: "NONE",
            names: "JWT",
            accepted: true,
        },
        Row {
            case: "an HTTP api's IAM route on its REQUEST authorizer",
            protocol_type: "HTTP",
            authorization_type: "AWS_IAM",
            names: "REQUEST",
            accepted: true,
        },
        // A WebSocket api admits only the REQUEST authorizer
        // (`validate_authorizer_config` refuses a JWT one there), so its rows name that
        // kind and the mismatch is reached through the authorization type instead.
        Row {
            case: "a WebSocket api's CUSTOM route on its REQUEST authorizer",
            protocol_type: "WEBSOCKET",
            authorization_type: "CUSTOM",
            names: "REQUEST",
            accepted: true,
        },
        Row {
            case: "a WebSocket api's JWT route on its REQUEST authorizer",
            protocol_type: "WEBSOCKET",
            authorization_type: "JWT",
            names: "REQUEST",
            accepted: false,
        },
    ]
}

#[tokio::test]
async fn a_route_is_authorized_only_by_an_authorizer_of_the_kind_its_type_names() {
    let emulator = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        // One api per row, so a refused row's route key is free and the store it must
        // leave untouched holds nothing but this row's own records.
        let api_id = create_api(
            &emulator,
            &format!("pairing-{index}-{}", row.protocol_type.to_ascii_lowercase()),
            row.protocol_type,
        )
        .await;
        let authorizer_id = create_authorizer(
            &emulator,
            row.case,
            &api_id,
            row.protocol_type,
            row.names,
        )
        .await;

        let key = route_key(row.protocol_type, index);
        let request = json!({
            "routeKey": key,
            "authorizationType": row.authorization_type,
            "authorizerId": authorizer_id
        });
        let (status, body) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(request.clone()),
        )
        .await;

        if row.accepted {
            assert_eq!(
                status, 201,
                "case {:?}: expected 201 from CreateRoute, got {status} {body}",
                row.case
            );
            assert_eq!(
                as_str(&body, "authorizerId"),
                authorizer_id,
                "case {:?}: expected the route to report the authorizer it names, got {body}",
                row.case
            );
            // The association is what a later caller sees, not just what the create said.
            let route_id = as_str(&body, "routeId");
            let (status, fetched) = call(
                &emulator,
                "GET",
                &format!("/v2/apis/{api_id}/routes/{route_id}"),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: expected 200 from GetRoute, got {status} {fetched}",
                row.case
            );
            assert_eq!(
                fetched, body,
                "case {:?}: GetRoute must report what CreateRoute stored",
                row.case
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {:?}: expected 400 from CreateRoute, got {status} {body}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {:?}: expected BadRequestException, got {body}",
            row.case
        );
        assert_eq!(
            as_str(&body, "message"),
            invalid_authorizer_reference_message(row.authorization_type),
            "case {:?}: expected the authorization type's own refusal, got {body}",
            row.case
        );
        // A refused create stored nothing: the api holds no route under that key.
        let (status, listed) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/routes"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from GetRoutes, got {status} {listed}",
            row.case
        );
        let keys: Vec<String> = listed["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|route| as_str(route, "routeKey"))
            .collect();
        assert!(
            !keys.contains(&key),
            "case {:?}: a refused CreateRoute must store no route, got {keys:?}",
            row.case
        );

        // And the PATCH is held to the same rule: an open route cannot be moved onto that
        // association either, and the refused patch leaves the record as it was.
        let open_key = route_key(row.protocol_type, index + rows().len());
        let (status, open) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(json!({ "routeKey": open_key })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {:?}: expected 201 for the open route, got {status} {open}",
            row.case
        );
        let open_route_id = as_str(&open, "routeId");
        let uri = format!("/v2/apis/{api_id}/routes/{open_route_id}");
        let (status, patched) = call(
            &emulator,
            "PATCH",
            &uri,
            Some(json!({
                "authorizationType": row.authorization_type,
                "authorizerId": authorizer_id
            })),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {:?}: expected 400 from UpdateRoute, got {status} {patched}",
            row.case
        );
        assert_eq!(
            as_str(&patched, "message"),
            invalid_authorizer_reference_message(row.authorization_type),
            "case {:?}: expected the authorization type's own refusal from UpdateRoute, got \
             {patched}",
            row.case
        );
        let (_, after) = call(&emulator, "GET", &uri, None).await;
        assert_eq!(
            after, open,
            "case {:?}: a refused UpdateRoute must leave the record as it was, got {after}",
            row.case
        );
    }
}

#[tokio::test]
async fn an_authorizer_of_another_api_authorizes_no_route_of_this_one() {
    let emulator = emulator();

    // Two HTTP apis, each holding a JWT authorizer: the ids are minted per api, so the
    // other api's id is one this api holds no authorizer under.
    let api_id = create_api(&emulator, "pairing-own-api", "HTTP").await;
    let other_api = create_api(&emulator, "pairing-other-api", "HTTP").await;
    create_authorizer(&emulator, "this api's authorizer", &api_id, "HTTP", "JWT").await;
    let foreign =
        create_authorizer(&emulator, "another api's authorizer", &other_api, "HTTP", "JWT").await;

    for authorization_type in ["JWT", "CUSTOM"] {
        let (status, body) = call(
            &emulator,
            "POST",
            &format!("/v2/apis/{api_id}/routes"),
            Some(json!({
                "routeKey": format!("GET /foreign-{authorization_type}"),
                "authorizationType": authorization_type,
                "authorizerId": foreign
            })),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {authorization_type:?}: expected 400 from CreateRoute, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "BadRequestException",
            "case {authorization_type:?}: expected BadRequestException, got {body}"
        );
        assert_eq!(
            as_str(&body, "message"),
            invalid_authorizer_reference_message(authorization_type),
            "case {authorization_type:?}: expected the authorization type's own refusal, got \
             {body}"
        );
    }
}
