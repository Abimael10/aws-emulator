// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack does with the PROPERTIES an `AWS::ApiGatewayV2::Api`
//! declaration carries, against the property set that resource type's schema states.
//!
//! Two rules, which three crates have to agree on (CloudFormation judges and deploys the
//! declaration, API Gateway v2 serves the api, Cloud Control API reports its resource
//! model), so the suite lives here rather than in any one of them:
//!
//! * **A property document carrying a member the resource type's schema does not state is
//!   one no handler of that type could be given, so CloudFormation refuses it BEFORE
//!   provisioning**: the stack ends `ROLLBACK_COMPLETE`, its own `CREATE_FAILED` event
//!   carries the validation summary over the errors found, `DescribeEvents` reports one
//!   `VALIDATION_ERROR` row naming the member and the path it was carried at, and nothing of
//!   the stack is created in the owning service.
//! * **A property the schema marks READ-ONLY is a value the owning service COMPUTES rather
//!   than an input to any handler, so a declaration carrying one is deployed as though it
//!   did not**: the api is created, and what `Ref` and `Fn::GetAtt` publish is the identity,
//!   the endpoint and the invoke ARN built from what API Gateway v2 answered — never the
//!   value the template stated for it.
//!
//! Cloud Control API's model of such a resource is the other half of the same schema
//! classification: its read reports the type's read-only properties and none of its
//! write-only ones, so a client that round-trips a model never sees a property no read of
//! the api would answer with.
//!
//! The rows differ along what the rules generalise over: which member the schema does not
//! state and the FORM of its value (a number, a mis-cased real property, a list), whether
//! the declaration states the api by its members or by an imported OpenAPI definition, which
//! read-only property it carries, and the api's `ProtocolType`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_provisions_stack_resources::{outputs_of, xml_text, xml_texts};
use crate::query_wire::{form_encode, members_of, opt_element};

/// The region every request here is signed for, and so the region the stack's api belongs
/// to.
pub(crate) const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// The resource type under test, and the two Cloud Control vocabularies that address it.
pub(crate) const API_TYPE: &str = "AWS::ApiGatewayV2::Api";
const CLOUD_CONTROL_SIGNING_NAME: &str = "cloudcontrolapi";
const CLOUD_CONTROL_TARGET_PREFIX: &str = "CloudApiService";

/// How long a wait for a stack to reach a terminal status runs before the case fails.
///
/// A stack's create runs a deployment of its own — further requests of this emulator, each
/// with the span the clock owes it — so the budget is far above anything that flow takes,
/// including on a host running the whole workspace's tests at once. No case waits for a
/// span: each re-reads until the stack is no longer in progress or this deadline passes.
const STACK_BUDGET: Duration = Duration::from_secs(120);

/// How often that wait re-reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The suffix every status a stack operation is still running carries.
const IN_PROGRESS_SUFFIX: &str = "_IN_PROGRESS";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them.
///
/// `pub(crate)` because the sibling suite about this type's Cloud Control LIFECYCLE drives
/// the same three services, so the mounting is spelled once.
pub(crate) fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudcontrol_cloudapiservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one API Gateway v2 operation over its restJson1 wire: this service signs as
/// `apigateway` and binds every operation under `/v2/…`.
pub(crate) async fn apigatewayv2(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Call one Cloud Control API operation over its awsJson1_0 wire.
pub(crate) async fn cloud_control(
    router: &MultiServiceRouter,
    operation: &str,
    payload: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "host",
            format!("{CLOUD_CONTROL_SIGNING_NAME}.{REGION}.amazonaws.com"),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUD_CONTROL_TARGET_PREFIX}.{operation}"),
        )
        .header("authorization", authorization(CLOUD_CONTROL_SIGNING_NAME))
        .body(Bytes::from(payload.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// `CreateStack` with `template`, which every row's stack is deployed from.
async fn create_stack(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
}

/// Read the stack until it reports a status that is no longer in progress, answering
/// `(StackStatus, the whole answer)`.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let started = Instant::now();
    loop {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let reached = xml_text(&body, "StackStatus").unwrap_or_default();
        if !reached.ends_with(IN_PROGRESS_SUFFIX) {
            return (reached, body);
        }
        assert!(
            started.elapsed() < STACK_BUDGET,
            "case {case:?}: expected the stack to reach a terminal status within \
             {STACK_BUDGET:?}, it is still {reached:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `ResourceStatusReason` of the STACK's own `CREATE_FAILED` event — the one event that
/// says why the operation deployed nothing.
async fn stack_failure_reason(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body {body}"
    );
    members_of(&body)
        .into_iter()
        .filter(|event| {
            opt_element(event, "ResourceType").as_deref() == Some("AWS::CloudFormation::Stack")
                && opt_element(event, "ResourceStatus").as_deref() == Some("CREATE_FAILED")
        })
        .filter_map(|event| opt_element(event, "ResourceStatusReason"))
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: no stack CREATE_FAILED event in {body}"))
}

/// Every `VALIDATION_ERROR` row `DescribeEvents` reports for the stack, as the members a
/// client reads a validation result by.
async fn validation_rows(router: &MultiServiceRouter, case: &str, stack: &str) -> Vec<Vec<String>> {
    let (status, body) = cloudformation(router, "DescribeEvents", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeEvents, got {status} with body {body}"
    );
    members_of(&body)
        .into_iter()
        .filter(|event| opt_element(event, "EventType").as_deref() == Some("VALIDATION_ERROR"))
        .map(|event| {
            [
                "LogicalResourceId",
                "ValidationName",
                "ValidationStatus",
                "ValidationFailureMode",
                "ValidationStatusReason",
                "ValidationPath",
            ]
            .into_iter()
            .map(|member| opt_element(event, member).unwrap_or_default())
            .collect()
        })
        .collect()
}

/// Every api API Gateway v2 holds, as `Name -> ApiId`, over every page of `GetApis`.
async fn held_apis(router: &MultiServiceRouter, case: &str) -> BTreeMap<String, String> {
    let (status, listed) = apigatewayv2(router, "GET", "/v2/apis").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetApis, got {status} {listed}"
    );
    listed["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|api| {
                    Some((
                        api.get("name")?.as_str()?.to_string(),
                        api.get("apiId")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The route keys an api really serves, read back through `GetRoutes`.
pub(crate) async fn held_routes(
    router: &MultiServiceRouter,
    case: &str,
    api_id: &str,
) -> BTreeSet<String> {
    let (status, listed) =
        apigatewayv2(router, "GET", &format!("/v2/apis/{api_id}/routes")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetRoutes, got {status} {listed}"
    );
    listed["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|route| Some(route.get("routeKey")?.as_str()?.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// The OpenAPI definition a row's api is IMPORTED from: its name comes from `info.title`,
/// and its one route from the path and the integration extension the document declares.
pub(crate) fn definition(title: &str) -> Value {
    json!({
        "openapi": "3.0.1",
        "info": { "title": title, "version": "2026-01-01" },
        "paths": {
            "/probe": {
                "get": {
                    "x-amazon-apigateway-integration": {
                        "type": "http_proxy",
                        "httpMethod": "GET",
                        "uri": "https://probe.example.com/probe",
                        "payloadFormatVersion": "1.0"
                    }
                }
            }
        }
    })
}

/// A value no api of this emulator ever carries, which is what a row states for a read-only
/// property: an endpoint nothing serves, so a published value equal to it would be the
/// template's rather than the owning service's.
const OVERRIDDEN_ENDPOINT: &str = "https://this-endpoint-is-not-the-apis.example.com";

/// An identifier no api of this emulator is minted under, stated where a row carries the
/// read-only `ApiId`.
const OVERRIDDEN_API_ID: &str = "not-an-api-id";

/// A one-api template over `properties`, publishing the three values the type's schema
/// marks read-only and publishes through `Fn::GetAtt`.
fn api_template(properties: Value) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": { "Api": { "Type": API_TYPE, "Properties": properties } },
        "Outputs": {
            "Ref": { "Value": { "Ref": "Api" } },
            "ApiId": { "Value": { "Fn::GetAtt": ["Api", "ApiId"] } },
            "ApiEndpoint": { "Value": { "Fn::GetAtt": ["Api", "ApiEndpoint"] } },
            "ExecuteApiArn": { "Value": { "Fn::GetAtt": ["Api", "ExecuteApiArn"] } }
        }
    })
    .to_string()
}

/// One row of the REFUSED table: a declaration carrying a member the type's schema does not
/// state.
struct Unsupported {
    case: &'static str,
    /// The properties the row declares, built from the stack name so each row's api name is
    /// its own.
    properties: fn(&str) -> Value,
    /// The member the schema does not state, which the validation must name.
    member: &'static str,
}

const UNSUPPORTED: &[Unsupported] = &[
    Unsupported {
        case: "a member no schema states at all, carried as a number",
        properties: |stack| {
            json!({
                "Name": stack,
                "ProtocolType": "HTTP",
                "RouteSelectionExpression": "$request.method $request.path",
                "ThisPropertyDoesNotExist": 1,
            })
        },
        member: "ThisPropertyDoesNotExist",
    },
    Unsupported {
        case: "the owning API's spelling of a real property rather than the type's",
        properties: |stack| json!({ "Name": stack, "protocolType": "HTTP" }),
        member: "protocolType",
    },
    Unsupported {
        case: "a member the api's READ answers with that the type has no property for, \
               carried as a list, beside an imported definition",
        properties: |stack| json!({ "Body": definition(stack), "Warnings": ["none"] }),
        member: "Warnings",
    },
];

/// One row of the DEPLOYED table: a declaration carrying a property the type's schema marks
/// read-only, which the deployment ignores.
struct ReadOnlyCarried {
    case: &'static str,
    properties: fn(&str) -> Value,
    /// The `ProtocolType` the api really speaks, which decides the scheme of the endpoint
    /// that service reports.
    protocol_type: &'static str,
    /// The route keys the api must serve: the ones an imported definition declares, and none
    /// for an api declared by its members.
    routes: &'static [&'static str],
}

const READ_ONLY_CARRIED: &[ReadOnlyCarried] = &[
    ReadOnlyCarried {
        case: "an HTTP api declared by its members, carrying both read-only properties",
        properties: |stack| {
            json!({
                "Name": stack,
                "ProtocolType": "HTTP",
                "RouteSelectionExpression": "$request.method $request.path",
                "ApiEndpoint": OVERRIDDEN_ENDPOINT,
                "ApiId": OVERRIDDEN_API_ID,
            })
        },
        protocol_type: "HTTP",
        routes: &[],
    },
    ReadOnlyCarried {
        case: "an api declared ONLY by its imported definition, carrying the read-only \
               endpoint beside it",
        properties: |stack| {
            json!({ "Body": definition(stack), "ApiEndpoint": OVERRIDDEN_ENDPOINT })
        },
        protocol_type: "HTTP",
        routes: &["GET /probe"],
    },
    ReadOnlyCarried {
        case: "a WEBSOCKET api carrying the read-only endpoint",
        properties: |stack| {
            json!({
                "Name": stack,
                "ProtocolType": "WEBSOCKET",
                "RouteSelectionExpression": "$request.body.action",
                "ApiEndpoint": OVERRIDDEN_ENDPOINT,
            })
        },
        protocol_type: "WEBSOCKET",
        routes: &[],
    },
];

/// RULE: a property document carrying a member its resource type's schema does not state is
/// refused before anything is provisioned — the stack rolls back carrying the validation
/// summary, the validation result names the member and the path it was carried at, and the
/// owning service holds no api of that declaration.
#[tokio::test]
async fn a_member_the_resource_schema_does_not_state_rolls_the_stack_back() {
    let router = build_router();
    for (index, row) in UNSUPPORTED.iter().enumerate() {
        let case = row.case;
        let stack = format!("apigwv2-unsupported-{index}");
        create_stack(&router, case, &stack, &api_template((row.properties)(&stack))).await;
        let (status, body) = settled(&router, case, &stack).await;
        assert_eq!(
            status, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the stack to roll back, got {status:?} with {body}"
        );
        // The stack's own failure carries the SUMMARY over the errors the stage found, and
        // says where each one's detail is; the count is how many declarations it refused.
        let reason = stack_failure_reason(&router, case, &stack).await;
        assert_eq!(
            reason,
            "Validation failed with 1 error(s). Call DescribeEvents to retrieve the full list \
             of issues with resource and property details, resolve each error, then retry the \
             operation.",
            "case {case:?}: expected the validation summary on the stack's own failure, got \
             {reason:?}"
        );
        // ...and the detail is the validation result: which validation failed, on which
        // declaration, why, and at which path of the submitted template.
        let rows = validation_rows(&router, case, &stack).await;
        assert_eq!(
            rows,
            vec![vec![
                "Api".to_string(),
                "PROPERTY_VALIDATION".to_string(),
                "FAILED".to_string(),
                "FAIL".to_string(),
                format!("Unsupported property [{}]", row.member),
                format!("/Resources/Api/Properties/{}", row.member),
            ]],
            "case {case:?}: expected one validation result naming {:?}, got {rows:?}",
            row.member
        );
        // Nothing was asked of the owning service, so it holds no api of this declaration.
        let held = held_apis(&router, case).await;
        assert!(
            !held.contains_key(&stack),
            "case {case:?}: expected no api named {stack:?} to have been created, got {held:?}"
        );
    }
}

/// RULE: a property the type's schema marks read-only is a value the owning service
/// computes, so a declaration carrying one deploys as though it did not — the api is
/// created, and what the stack publishes is built from what that service answered rather
/// than from the value the template stated.
#[tokio::test]
async fn a_read_only_property_is_ignored_and_the_api_is_created() {
    let router = build_router();
    for (index, row) in READ_ONLY_CARRIED.iter().enumerate() {
        let case = row.case;
        let stack = format!("apigwv2-readonly-{index}");
        create_stack(&router, case, &stack, &api_template((row.properties)(&stack))).await;
        let (status, body) = settled(&router, case, &stack).await;
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to complete, got {status:?} with {body}"
        );
        // Every output resolved: an `Fn::GetAtt` over an attribute the type publishes is a
        // value the stack HAS, so no entry is reported without one.
        let keys = xml_texts(&body, "OutputKey");
        let outputs = outputs_of(&body);
        assert_eq!(
            keys.len(),
            outputs.len(),
            "case {case:?}: every Outputs entry must carry an OutputValue, got {keys:?} and \
             {outputs:?}"
        );

        // The api the declaration asked for exists in API Gateway v2, under the id the stack
        // published — which is what `Ref` returns for the type.
        let api_id = outputs
            .get("Ref")
            .unwrap_or_else(|| panic!("case {case:?}: no Ref output in {outputs:?}"))
            .clone();
        assert_ne!(
            api_id, OVERRIDDEN_API_ID,
            "case {case:?}: expected the identity API Gateway v2 minted, got the template's"
        );
        let (status, api) = apigatewayv2(&router, "GET", &format!("/v2/apis/{api_id}")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetApi for the published {api_id:?}, got {status} \
             {api}"
        );
        // The api's own name and protocol: the declaration's where it states them, and the
        // imported document's where the definition IS the api (`info.title`, and the HTTP
        // protocol an imported api speaks).
        assert_eq!(
            api.get("name").and_then(Value::as_str),
            Some(stack.as_str()),
            "case {case:?}: expected the api the declaration asked for, got {api}"
        );
        assert_eq!(
            api.get("protocolType").and_then(Value::as_str),
            Some(row.protocol_type),
            "case {case:?}: expected the declared protocol, got {api}"
        );
        assert_eq!(
            held_routes(&router, case, &api_id).await,
            row.routes
                .iter()
                .map(|route| (*route).to_string())
                .collect::<BTreeSet<String>>(),
            "case {case:?}: expected the routes the declaration brings, got {api}"
        );

        // The read-only properties the stack publishes are the OWNING service's values: the
        // endpoint it reports, the identity it minted, and the ARN of that api's own invoke
        // surface — never what the template stated for them.
        let reported_endpoint = api
            .get("apiEndpoint")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: no apiEndpoint in {api}"));
        assert_ne!(
            reported_endpoint, OVERRIDDEN_ENDPOINT,
            "case {case:?}: expected the endpoint API Gateway v2 serves the api on, got the \
             template's override"
        );
        assert_eq!(
            outputs.get("ApiEndpoint").map(String::as_str),
            Some(reported_endpoint),
            "case {case:?}: expected the published ApiEndpoint to be the one that service \
             reports, got {outputs:?}"
        );
        assert_eq!(
            outputs.get("ApiId").map(String::as_str),
            Some(api_id.as_str()),
            "case {case:?}: expected the published ApiId to be the identity Ref resolves to, \
             got {outputs:?}"
        );
        assert_eq!(
            outputs.get("ExecuteApiArn").map(String::as_str),
            Some(format!("arn:aws:execute-api:{REGION}:{ACCOUNT_ID}:{api_id}").as_str()),
            "case {case:?}: expected the ARN of the api's own invoke surface, got {outputs:?}"
        );
    }
}

/// RULE: Cloud Control API's model of a resource is what the type's own read handler
/// reports — the schema's read-only properties are in it, and its write-only ones, which no
/// read of the api answers with, are not.
#[tokio::test]
async fn the_cloud_control_model_of_a_deployed_api_hides_its_write_only_definition() {
    let router = build_router();
    let case = "an api a stack imported from a definition, read through Cloud Control API";
    let stack = "apigwv2-cloudcontrol";
    create_stack(
        &router,
        case,
        stack,
        &api_template(json!({ "Body": definition(stack), "ApiEndpoint": OVERRIDDEN_ENDPOINT })),
    )
    .await;
    let (status, body) = settled(&router, case, stack).await;
    assert_eq!(
        status, "CREATE_COMPLETE",
        "case {case:?}: expected the stack to complete, got {status:?} with {body}"
    );
    let outputs = outputs_of(&body);
    let api_id = outputs
        .get("Ref")
        .unwrap_or_else(|| panic!("case {case:?}: no Ref output in {outputs:?}"))
        .clone();

    let (status, answered) = cloud_control(
        &router,
        "GetResource",
        json!({ "TypeName": API_TYPE, "Identifier": api_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetResource for {api_id:?}, got {status} \
         {answered}"
    );
    let described = &answered["ResourceDescription"];
    assert_eq!(
        described.get("Identifier").and_then(Value::as_str),
        Some(api_id.as_str()),
        "case {case:?}: expected the resource to answer to the id that service minted, got \
         {answered}"
    );
    let model: Value = described
        .get("Properties")
        .and_then(Value::as_str)
        .map(|properties| serde_json::from_str(properties).expect("a resource model document"))
        .unwrap_or_else(|| panic!("case {case:?}: no Properties in {answered}"));
    // What the api's own read reports, under the property names the type's schema states.
    let (status, api) = apigatewayv2(&router, "GET", &format!("/v2/apis/{api_id}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetApi for {api_id:?}, got {status} {api}"
    );
    for (property, reported) in [
        ("ApiId", api.get("apiId")),
        ("Name", api.get("name")),
        ("ApiEndpoint", api.get("apiEndpoint")),
        ("ProtocolType", api.get("protocolType")),
    ] {
        assert_eq!(
            model.get(property),
            reported,
            "case {case:?}: expected the model's {property} to be what the read reports, got \
             {model}"
        );
    }
    // ...and the definition the api was imported FROM is write-only: no read of the api
    // answers with it, so no model this service reports carries it.
    for write_only in ["Body", "BodyS3Location", "BasePath", "FailOnWarnings"] {
        assert!(
            model.get(write_only).is_none(),
            "case {case:?}: expected the write-only {write_only} to be out of the model, got \
             {model}"
        );
    }
}
