// SPDX-License-Identifier: CC0-1.0

//! A WEBSOCKET api a CloudFormation stack deployed serves the client exchange its template
//! configured: the estate that stack created, and the opening handshake the deployed stage
//! completes at the address it published.
//!
//! The rule under test:
//!
//! > **Every `AWS::ApiGatewayV2::*` resource a template declares is CREATED in API Gateway
//! > v2 through that service's own create API — the children of an integration and of a
//! > route (`IntegrationResponse`, `RouteResponse`, `Model`) as much as the api, integration,
//! > route, deployment and stage — so a stack that reports `CREATE_COMPLETE` has published a
//! > stage that really serves what the template configured: each of those resources resolves
//! > through that service's own read, under the identifier the stack published, and the
//! > address that api announces as its `ApiEndpoint` completes an RFC 6455 opening handshake
//! > for the declared stage, with the `Sec-WebSocket-Accept` derived from the client's key,
//! > and refuses one naming a stage the template never declared.**
//!
//! Two crates have to agree for that to hold (CloudFormation makes the creates; API Gateway
//! v2 serves them and owns the socket), so the suite lives here rather than in either one.
//! What the socket then does with the FRAMES — the route the api's `RouteSelectionExpression`
//! selects for each message, and the reply that route's selected integration response renders
//! on the same connection — is driven with a WebSocket client library by the API Gateway V2
//! crate's own `tests/all/websocket_transport_data_plane.rs`, which owns the whole of that
//! rule (a `$connect` route bound to a `MOCK` integration included); what this suite adds is
//! the CloudFormation-deployed estate behind it, which is what was missing: the integration
//! responses and route responses a template declares are what a frame is answered FROM.
//!
//! The rows differ along what the handshake generalises over: the stage the client addresses
//! — the one the template declared, and one it did not.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

// The handshake a client writes, and the head it is answered with, as this test crate states
// them ONCE: the request line, the four RFC 6455 fields, the read that stops at the blank
// line ending the head, and the published key/accept pair that says the answer is derived
// from the client's key rather than constant.
use crate::apigatewayv2_websocket_connect_registers_its_agent::{
    answered_handshake_at, ACCEPT_FIELD, RFC_EXAMPLE_ACCEPT, RFC_EXAMPLE_KEY,
};
use crate::cloudformation_apigatewayv2_resources::{
    apigatewayv2, build_router, create_stack, describe_stack, member,
};
use crate::cloudformation_provisions_stack_resources::{outputs_of, xml_text};

/// The `StackStatus` the deploy waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The status line RFC 6455 §4.2.2 requires of a COMPLETED opening handshake, and the status
/// an `execute-api` endpoint answers a stage it does not serve.
const SWITCHING_PROTOCOLS: u16 = 101;
const FORBIDDEN: u16 = 403;

/// The stage the template declares and the client addresses.
const STAGE: &str = "prod";

/// The expression the api selects a route by, which is the one AWS documents for a WebSocket
/// api whose messages carry an `action` property.
const SELECTION_EXPRESSION: &str = "$request.body.action";

/// How often the deploy asks the stack whether it has completed, and how long it goes on
/// asking. The wait is on the CONDITION — the stack reporting itself complete — and the
/// deadline is only there so the wait terminates; nothing here is timed.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// The two action routes the template declares, each with the reply its own MOCK integration
/// renders, and the `$default` route that serves every message neither of them keys.
const ECHO_ACTION: &str = "echo";
const STATUS_ACTION: &str = "status";

/// The model the template declares for the messages those routes carry: the `Name` it is
/// called by and the JSON schema it holds.
const MODEL_NAME: &str = "Action";
const MODEL_SCHEMA: &str =
    r#"{"$schema":"http://json-schema.org/draft-04/schema#","title":"Action","type":"object"}"#;

/// The template the stack is created from: a WEBSOCKET api selecting routes by
/// [`SELECTION_EXPRESSION`], a `$connect` route and the two action routes over MOCK
/// integrations of their own, an integration response and a route response per route (which
/// is what makes a route TWO-WAY and gives its MOCK a reply to render), a deployment of the
/// lot and the stage that serves it.
fn template(stack: &str) -> String {
    let mut resources = json!({
        "Api": {
            "Type": "AWS::ApiGatewayV2::Api",
            "Properties": {
                "Name": format!("{stack}-socket"),
                "ProtocolType": "WEBSOCKET",
                "RouteSelectionExpression": SELECTION_EXPRESSION
            }
        },
        "ConnectIntegration": {
            "Type": "AWS::ApiGatewayV2::Integration",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "IntegrationType": "MOCK",
                "RequestTemplates": { "$default": "{\"statusCode\": 200}" }
            }
        },
        "ConnectIntegrationResponse": {
            "Type": "AWS::ApiGatewayV2::IntegrationResponse",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "IntegrationId": { "Ref": "ConnectIntegration" },
                "IntegrationResponseKey": "/2\\d\\d/"
            }
        },
        "ConnectRoute": {
            "Type": "AWS::ApiGatewayV2::Route",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "RouteKey": "$connect",
                "Target": { "Fn::Join": ["/", ["integrations", { "Ref": "ConnectIntegration" }]] }
            }
        }
    });
    for (logical, route_key, reply) in declared_routes() {
        let integration = format!("{logical}Integration");
        resources[format!("{logical}Integration")] = json!({
            "Type": "AWS::ApiGatewayV2::Integration",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "IntegrationType": "MOCK",
                "RequestTemplates": { "$default": "{\"statusCode\": 200}" }
            }
        });
        resources[format!("{logical}IntegrationResponse")] = json!({
            "Type": "AWS::ApiGatewayV2::IntegrationResponse",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "IntegrationId": { "Ref": integration.clone() },
                "IntegrationResponseKey": "/2\\d\\d/",
                "ResponseTemplates": { "$default": reply }
            }
        });
        resources[format!("{logical}Route")] = json!({
            "Type": "AWS::ApiGatewayV2::Route",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "RouteKey": route_key,
                "RouteResponseSelectionExpression": "$default",
                "Target": { "Fn::Join": ["/", ["integrations", { "Ref": integration }]] }
            }
        });
        resources[format!("{logical}RouteResponse")] = json!({
            "Type": "AWS::ApiGatewayV2::RouteResponse",
            "Properties": {
                "ApiId": { "Ref": "Api" },
                "RouteId": { "Ref": format!("{logical}Route") },
                "RouteResponseKey": "$default"
            }
        });
    }
    // The SCHEMA the api holds for the messages those routes carry: an
    // `AWS::ApiGatewayV2::Model`, the third child of an api this suite's rule covers, which a
    // route's `RequestModels` and a route response's `ResponseModels` name by the `Name` the
    // template states.
    resources["Model"] = json!({
        "Type": "AWS::ApiGatewayV2::Model",
        "Properties": {
            "ApiId": { "Ref": "Api" },
            "Name": MODEL_NAME,
            "ContentType": "application/json",
            "Schema": MODEL_SCHEMA
        }
    });
    resources["Deployment"] = json!({
        "Type": "AWS::ApiGatewayV2::Deployment",
        "DependsOn": [
            "ConnectRoute",
            "ConnectIntegrationResponse",
            "EchoRoute",
            "EchoRouteResponse",
            "EchoIntegrationResponse",
            "StatusRoute",
            "StatusRouteResponse",
            "StatusIntegrationResponse",
            "DefaultRoute",
            "DefaultRouteResponse",
            "DefaultIntegrationResponse"
        ],
        "Properties": { "ApiId": { "Ref": "Api" } }
    });
    resources["Stage"] = json!({
        "Type": "AWS::ApiGatewayV2::Stage",
        "Properties": {
            "ApiId": { "Ref": "Api" },
            "StageName": STAGE,
            "DeploymentId": { "Ref": "Deployment" }
        }
    });
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": {
            "ApiId": { "Value": { "Ref": "Api" } },
            "ApiEndpoint": { "Value": { "Fn::GetAtt": ["Api", "ApiEndpoint"] } },
            "ModelId": { "Value": { "Ref": "Model" } }
        }
    })
    .to_string()
}

/// The routes the template declares beside `$connect`: the logical id each is declared
/// under, its `RouteKey`, and the payload its own integration response renders.
fn declared_routes() -> Vec<(&'static str, &'static str, &'static str)> {
    vec![
        ("Echo", ECHO_ACTION, r#"{"served":"echo"}"#),
        ("Status", STATUS_ACTION, r#"{"served":"status"}"#),
        ("Default", "$default", r#"{"served":"default"}"#),
    ]
}

/// Deploy `template` as `stack` and answer the outputs it published, having waited for the
/// stack to report itself complete.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> BTreeMap<String, String> {
    create_stack(router, case, stack, &template(stack)).await;
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut body = describe_stack(router, case, stack).await;
    while xml_text(&body, "StackStatus").as_deref() != Some(CREATE_COMPLETE)
        && Instant::now() < giving_up_at
    {
        tokio::time::sleep(POLL_INTERVAL).await;
        body = describe_stack(router, case, stack).await;
    }
    assert_eq!(
        xml_text(&body, "StackStatus").as_deref(),
        Some(CREATE_COMPLETE),
        "case {case:?}: expected the stack to complete, got {body}"
    );
    outputs_of(&body)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stack_deployed_websocket_api_serves_the_stage_it_published() {
    let case = "a CFN-deployed WEBSOCKET api with a RouteSelectionExpression";
    let router = build_router();
    let outputs = deployed(&router, case, "cfn-ws-dispatch").await;

    // The api the template declared, read back through API Gateway v2 itself: the stack's
    // `ApiEndpoint` output is the address that service announces for the api's data plane.
    let api_id = outputs
        .get("ApiId")
        .unwrap_or_else(|| panic!("case {case:?}: no ApiId output in {outputs:?}"))
        .clone();
    let (status, api) = apigatewayv2(&router, "GET", &format!("/v2/apis/{api_id}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetApi for {api_id:?}, got {status} {api}"
    );
    assert_eq!(
        member(case, &api, "routeSelectionExpression"),
        SELECTION_EXPRESSION,
        "case {case:?}: expected the declared RouteSelectionExpression, got {api}"
    );
    let announced = member(case, &api, "apiEndpoint");
    assert_eq!(
        outputs.get("ApiEndpoint"),
        Some(&announced),
        "case {case:?}: expected the ApiEndpoint output to be the address API Gateway \
         announces, got {outputs:?}"
    );

    // Each child of each route the template declared EXISTS in API Gateway v2, read through
    // that service's own listing: a stack that reported itself complete created them.
    for (collection, parent, expected) in [
        ("routeresponses", "routes", declared_routes().len()),
        ("integrationresponses", "integrations", declared_routes().len() + 1),
    ] {
        let (status, parents) =
            apigatewayv2(&router, "GET", &format!("/v2/apis/{api_id}/{parent}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 listing {parent}, got {status} {parents}"
        );
        let ids: Vec<String> = parents["items"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|item| {
                        member(
                            case,
                            item,
                            if parent == "routes" {
                                "routeId"
                            } else {
                                "integrationId"
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut children = 0usize;
        for id in &ids {
            let (status, listed) = apigatewayv2(
                &router,
                "GET",
                &format!("/v2/apis/{api_id}/{parent}/{id}/{collection}"),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 listing {collection} of {id}, got {status} {listed}"
            );
            children += listed["items"].as_array().map_or(0, Vec::len);
        }
        assert_eq!(
            children, expected,
            "case {case:?}: expected the {expected} {collection} the template declared, got \
             {children}"
        );
    }

    // The MODEL the template declared, read back by the id the stack published — which is
    // the id `GetModel` addresses it by, and which that service minted rather than this
    // template stating it.
    let model_id = outputs
        .get("ModelId")
        .unwrap_or_else(|| panic!("case {case:?}: no ModelId output in {outputs:?}"))
        .clone();
    let (status, model) = apigatewayv2(
        &router,
        "GET",
        &format!("/v2/apis/{api_id}/models/{model_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetModel for the published ModelId {model_id:?}, \
         got {status} {model}"
    );
    for (field, expected) in [
        ("name", MODEL_NAME),
        ("contentType", "application/json"),
        ("schema", MODEL_SCHEMA),
    ] {
        assert_eq!(
            member(case, &model, field),
            expected,
            "case {case:?}: expected the declared {field}, got {model}"
        );
    }

    // The handshake the deployed stage answers at the address the stack published, for the
    // stage the template declared and for one it did not. The bytes a client writes and the
    // head it reads are this crate's one statement of them (`answered_handshake_at`).
    struct Row {
        case: &'static str,
        /// The stage segment the client's handshake addresses.
        stage: &'static str,
        expected: u16,
    }

    // The rows span what the handshake generalises over here: the stage the template declared
    // and served, and a stage name the api does not hold — which an `execute-api` endpoint
    // answers `403`, the status it answers for anything it does not serve.
    let rows = &[
        Row {
            case: "the stage the template declared, with a deployment released to it",
            stage: STAGE,
            expected: SWITCHING_PROTOCOLS,
        },
        Row {
            case: "a stage name the template never declared",
            stage: "not-declared",
            expected: FORBIDDEN,
        },
    ];

    for row in rows {
        let head = answered_handshake_at(
            row.case,
            &announced,
            &format!("/{}", row.stage),
            RFC_EXAMPLE_KEY,
        )
        .await;
        assert_eq!(
            head.status, row.expected,
            "case {:?}: expected the handshake answered {}, got {} ({:?})",
            row.case, row.expected, head.status, head.line
        );
        if row.expected == SWITCHING_PROTOCOLS {
            // The completed handshake's accept value is DERIVED from the key this client
            // offered — the pair RFC 6455 §1.3 publishes — so the socket really performed the
            // handshake rather than answering a constant head.
            assert_eq!(
                head.field(ACCEPT_FIELD),
                Some(RFC_EXAMPLE_ACCEPT),
                "case {:?}: expected the accept value derived from the offered key, got {:?}",
                row.case,
                head.fields
            );
        }
    }
}
