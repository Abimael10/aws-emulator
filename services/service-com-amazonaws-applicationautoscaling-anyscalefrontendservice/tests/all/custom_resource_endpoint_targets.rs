// SPDX-License-Identifier: CC0-1.0

//! Registering a `custom-resource` scalable target, whose resource IS an endpoint the caller's
//! own application serves.
//!
//! RULE: in the `custom-resource` namespace the scalable target is not an AWS resource but the
//! HTTP endpoint the resource id addresses — "for a resource provided by your own application
//! or service, use custom-resource instead", and the id is the capacity document that
//! application serves — so registering one is only possible where this service can INVOKE that
//! endpoint: reading its capacity (`GET`) and moving it (`PATCH`) is the whole of what scaling
//! such a target does. An endpoint that answers the invocation with a refusal is therefore
//! refused HTTP 400 `ValidationException`, naming the invoke permissions the contract requires,
//! and nothing is registered; an endpoint that serves the document has its capacity resolved
//! like any other resource's, so a registration whose window the capacity is outside of moves
//! the endpoint into it and records the one scaling activity for that movement.
//!
//! The rows differ along what the rule generalises over: a stage that serves the document, one
//! that was never deployed, a resource id whose host no endpoint of this emulator serves at
//! all, and one that is no URL at all — and, for the stage that serves it, a window the
//! capacity is inside of (which moves nothing) beside one whose floor is above it.
//!
//! The second suite below pins what the movement a floor owes LOOKS LIKE at the endpoint and in
//! the history: ONE `PATCH` carrying the one member the contract names, ONE activity settling
//! `Successful`, and a target that reads back with the registered bounds and the namespace's own
//! service-linked role — on every addressing the endpoint's URL can take, since which address a
//! client was handed is no part of the rule.
//!
//! This suite builds its OWN emulator rather than using the shared one
//! ([`super::emulator::Emulator`]), because the endpoint is served by a service the other
//! suites do not mount: Amazon API Gateway, through a REST API whose `GET` and `PATCH` on the
//! dimension's own path answer the capacity document.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::emulator::{error_code, error_message, REGION};

/// The dimension the namespace publishes: its resource type and scaling property are
/// placeholders, since "custom resources are not supported with a resource type […] the unique
/// identifier is defined by the service provider".
pub(crate) const DIMENSION: &str = "custom-resource:ResourceType:Property";

/// The table the endpoint's items live in: one item per scalable dimension, carrying the
/// capacity the caller's own resource is at.
pub(crate) const CAPACITY_TABLE: &str = "endpoint_capacity";

/// The capacity the endpoint reports, which is what the registered window is enforced on.
const REPORTED_CAPACITY: i64 = 2;

/// The path the custom-resource contract publishes for one scalable dimension.
pub(crate) const DIMENSION_PATH: &str = "scalableTargetDimensions";

/// The stage the endpoint is deployed to.
pub(crate) const DEPLOYED_STAGE: &str = "prod";

/// A stage name the api carries no deployment of, so the endpoint it addresses serves nothing.
const UNDEPLOYED_STAGE: &str = "never-deployed";

/// How long a case keeps polling for the movement a registration owes to have settled.
const POLL_FOR: Duration = Duration::from_secs(30);

/// How long a poll sleeps between reads.
const POLL_EVERY: Duration = Duration::from_millis(200);

/// The status a capacity movement whose change took effect settles in.
const SUCCESSFUL: &str = "Successful";

/// The permissions a registration that could not invoke the endpoint reports.
const MISSING_PERMISSIONS: &str =
    "User is missing the following permissions: execute-api:Invoke:PATCH, execute-api:Invoke:GET";

/// Application Auto Scaling, Amazon API Gateway — which serves the endpoint — and the two
/// services a registration also touches, over ONE engine.
pub(crate) fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suites drive unsigned requests; the `Authorization` header is still what routes each
    // request and names its region, as a signed client's would.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_applicationautoscaling_anyscalefrontendservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    // The endpoint's items live in DynamoDB, which is where the api's own integrations read
    // and write them.
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// Call one Application Auto Scaling operation.
pub(crate) async fn application_auto_scaling(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AnyScaleFrontendService.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/application-autoscaling\
                 /aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body().to_vec();
    (status, serde_json::from_slice(&payload).unwrap_or(Value::Null))
}

/// Call one API Gateway control-plane operation, which that service binds to a REST route.
async fn api_gateway(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("apigateway.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/apigateway\
                 /aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body().to_vec();
    (status, serde_json::from_slice(&payload).unwrap_or(Value::Null))
}

/// Call one DynamoDB operation, which is where the endpoint's items live.
pub(crate) async fn dynamodb(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let host = format!("dynamodb.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/dynamodb\
                 /aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body().to_vec();
    (status, serde_json::from_slice(&payload).unwrap_or(Value::Null))
}

/// Create the table the endpoint's items live in, with one item per dimension at
/// [`REPORTED_CAPACITY`].
async fn given_items(router: &MultiServiceRouter, dimensions: &[String]) {
    given_capacity_table(router).await;
    for dimension in dimensions {
        given_item(router, dimension, REPORTED_CAPACITY).await;
    }
}

/// Create the table the endpoint's items live in, and wait for the owning service to make it
/// writable.
pub(crate) async fn given_capacity_table(router: &MultiServiceRouter) {
    let (status, created) = dynamodb(
        router,
        "CreateTable",
        json!({
            "TableName": CAPACITY_TABLE,
            "KeySchema": [{"AttributeName": "id", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "id", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateTable: expected 200, got {status} {created:?}"
    );
    // A table is writable once the owning service has made it ACTIVE, which its own clock
    // owes: polled for the condition rather than waited out for a fixed span.
    let deadline = SystemTime::now() + POLL_FOR;
    loop {
        let (_, described) =
            dynamodb(router, "DescribeTable", json!({"TableName": CAPACITY_TABLE})).await;
        if described["Table"]["TableStatus"] == json!("ACTIVE") {
            break;
        }
        assert!(
            SystemTime::now() < deadline,
            "setup: expected the table to become ACTIVE, last saw {described:?}"
        );
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// Put the item the endpoint serves for one dimension, at `capacity`, with the counter its
/// `PATCH` integration adds to.
pub(crate) async fn given_item(router: &MultiServiceRouter, dimension: &str, capacity: i64) {
    let (status, written) = dynamodb(
        router,
        "PutItem",
        json!({
            "TableName": CAPACITY_TABLE,
            "Item": {
                "id": {"S": dimension},
                "desired": {"N": capacity.to_string()},
                "patches": {"N": "0"},
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup PutItem({dimension}): expected 200, got {status} {written:?}"
    );
}

/// The capacity the endpoint's own item holds for one dimension right now, as the service
/// that owns it reports.
pub(crate) async fn item_capacity(router: &MultiServiceRouter, dimension: &str) -> Option<f64> {
    let (status, read) = dynamodb(
        router,
        "GetItem",
        json!({"TableName": CAPACITY_TABLE, "Key": {"id": {"S": dimension}}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected GetItem to answer 200, got {status} {read:?}"
    );
    read["Item"]["desired"]["N"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok())
}

/// A required string member of an answer.
fn member(body: &Value, name: &str, case: &str) -> String {
    body[name]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name} member, got {body:?}"))
        .to_string()
}

/// Stand the endpoint up: a REST API whose `/scalableTargetDimensions/{id}` resource serves
/// the capacity document on `GET` and takes a capacity change on `PATCH`, each backed by the
/// item one DynamoDB table holds for that dimension, deployed to one stage — and answer the
/// api id.
///
/// This is the endpoint the custom-resource contract publishes, built the way it is built: the
/// `GET` reads the item and renders the document the contract declares, and the `PATCH` writes
/// the `desiredCapacity` its body carries into that item. So what this suite observes is the
/// caller's own resource moving, not an answer shaped like one.
pub(crate) async fn given_endpoint(router: &MultiServiceRouter) -> String {
    let (status, created) = api_gateway(
        router,
        "POST",
        "/restapis",
        Some(json!({"name": "capacity-endpoint"})),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateRestApi: expected 201, got {status} {created:?}"
    );
    let api = member(&created, "id", "CreateRestApi");
    let root = member(&created, "rootResourceId", "CreateRestApi");

    let (status, dimensions) = api_gateway(
        router,
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({"pathPart": DIMENSION_PATH})),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateResource({DIMENSION_PATH}): expected 201, got {status} {dimensions:?}"
    );
    let dimensions = member(&dimensions, "id", "CreateResource");
    let (status, dimension) = api_gateway(
        router,
        "POST",
        &format!("/restapis/{api}/resources/{dimensions}"),
        Some(json!({"pathPart": "{id}"})),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateResource({{id}}): expected 201, got {status} {dimension:?}"
    );
    let dimension = member(&dimension, "id", "CreateResource");

    // One method per verb the contract invokes, each an `AWS` integration naming the table
    // action that serves it.
    for (verb, action, request_template, capacity_source) in [
        (
            "GET",
            "GetItem",
            format!(
                r#"{{"TableName":"{CAPACITY_TABLE}","Key":{{"id":{{"S":"$input.params('id')"}}}}}}"#
            ),
            "$.Item.desired.N",
        ),
        (
            "PATCH",
            "UpdateItem",
            format!(
                r#"{{"TableName":"{CAPACITY_TABLE}","Key":{{"id":{{"S":"$input.params('id')"}}}},"UpdateExpression":"SET desired = :d, lastBody = :b ADD patches :one","ExpressionAttributeValues":{{":d":{{"N":"$input.path('$.desiredCapacity')"}},":b":{{"S":"$util.escapeJavaScript($input.body)"}},":one":{{"N":"1"}}}},"ReturnValues":"UPDATED_NEW"}}"#
            ),
            "$.Attributes.desired.N",
        ),
    ] {
        let answered = capacity_document(capacity_source);
        let steps: &[(&str, String, Value)] = &[
            (
                "PUT",
                format!("/restapis/{api}/resources/{dimension}/methods/{verb}"),
                json!({"authorizationType": "AWS_IAM"}),
            ),
            (
                "PUT",
                format!("/restapis/{api}/resources/{dimension}/methods/{verb}/integration"),
                json!({
                    "type": "AWS",
                    "httpMethod": "POST",
                    "uri": format!("arn:aws:apigateway:{REGION}:dynamodb:action/{action}"),
                    "requestTemplates": {"application/json": request_template},
                }),
            ),
            (
                "PUT",
                format!("/restapis/{api}/resources/{dimension}/methods/{verb}/responses/200"),
                json!({}),
            ),
            (
                "PUT",
                format!(
                    "/restapis/{api}/resources/{dimension}/methods/{verb}/integration/responses/200"
                ),
                json!({"responseTemplates": {"application/json": answered}}),
            ),
        ];
        for (method, uri, request) in steps {
            let (status, body) = api_gateway(router, method, uri, Some(request.clone())).await;
            assert!(
                (200..300).contains(&status),
                "setup {method} {uri}: expected the call to be accepted, got {status} {body:?}"
            );
        }
    }

    let (status, deployed) = api_gateway(
        router,
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({"stageName": DEPLOYED_STAGE})),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateDeployment: expected 201, got {status} {deployed:?}"
    );
    api
}

/// The document the custom-resource contract declares, rendered from the capacity the table
/// answered at `source`.
///
/// Source: the response body Application Auto Scaling publishes for a custom resource — the
/// capacity the resource IS at, the capacity it was last asked for, the dimension and the
/// resource it names, the status of the last change and the document's version.
fn capacity_document(source: &str) -> String {
    format!(
        r#"{{"actualCapacity":$input.path('{source}'),"desiredCapacity":$input.path('{source}'),"dimensionName":"{DIMENSION}","resourceName":"$input.params('id')","scalableTargetDimensionId":"$input.params('id')","scalingStatus":"Successful","version":"1.0"}}"#
    )
}

/// What a row's resource id addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Address {
    /// The deployed stage of the endpoint this suite stood up.
    Deployed,
    /// A stage of that api that was never deployed, so the endpoint serves nothing.
    Undeployed,
    /// A URL whose host no endpoint of this emulator serves.
    Unserved,
    /// An id that is no URL at all, so it addresses no endpoint to invoke.
    NotAnEndpoint,
}

/// One registration against a custom-resource address.
struct Row {
    case: &'static str,
    address: Address,
    /// The window the registration declares.
    window: (i64, i64),
    /// `true` when the registration is accepted.
    accepted: bool,
    /// The capacity the movement sets, for a row whose window the endpoint's capacity is
    /// outside of; `None` for a row that moves nothing.
    moved_to: Option<i64>,
}

const ROWS: &[Row] = &[
    Row {
        case: "the deployed stage, with the reported capacity inside the window",
        address: Address::Deployed,
        window: (1, 4),
        accepted: true,
        moved_to: None,
    },
    Row {
        case: "the deployed stage, with a floor above the reported capacity",
        address: Address::Deployed,
        window: (3, 6),
        accepted: true,
        moved_to: Some(3),
    },
    Row {
        case: "the deployed stage, with a ceiling below the reported capacity",
        address: Address::Deployed,
        window: (1, 1),
        accepted: true,
        moved_to: Some(1),
    },
    Row {
        case: "a stage of the api that was never deployed",
        address: Address::Undeployed,
        window: (3, 4),
        accepted: false,
        moved_to: None,
    },
    Row {
        case: "a URL no endpoint of this emulator serves",
        address: Address::Unserved,
        window: (1, 4),
        accepted: true,
        moved_to: None,
    },
    Row {
        case: "an id that is no URL at all",
        address: Address::NotAnEndpoint,
        window: (1, 4),
        accepted: true,
        moved_to: None,
    },
];

/// The dimension id a row's endpoint item is keyed by, which is also the last segment of the
/// resource id it registers.
fn dimension_id(index: usize) -> String {
    format!("dimension-{index}")
}

/// The resource id a row registers, which for the two served forms is the endpoint's own URL.
fn resource_id(row: &Row, api: &str, index: usize) -> String {
    let host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    let dimension = dimension_id(index);
    match row.address {
        Address::Deployed => {
            format!("https://{host}/{DEPLOYED_STAGE}/{DIMENSION_PATH}/{dimension}")
        }
        Address::Undeployed => {
            format!("https://{host}/{UNDEPLOYED_STAGE}/{DIMENSION_PATH}/{dimension}")
        }
        Address::Unserved => {
            format!("https://capacity.example.test/{DIMENSION_PATH}/{dimension}")
        }
        Address::NotAnEndpoint => format!("my-own-resource-{index}"),
    }
}

/// The scalable targets this service reports for one resource id.
async fn targets(router: &MultiServiceRouter, resource_id: &str) -> Vec<Value> {
    let (status, body) = application_auto_scaling(
        router,
        "DescribeScalableTargets",
        json!({
            "ServiceNamespace": "custom-resource",
            "ResourceIds": [resource_id],
            "ScalableDimension": DIMENSION,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected DescribeScalableTargets to answer 200, got {status} {body:?}"
    );
    body["ScalableTargets"].as_array().cloned().unwrap_or_default()
}

/// The scaling activities of one target, newest first.
async fn activities(router: &MultiServiceRouter, resource_id: &str) -> Vec<Value> {
    let (status, body) = application_auto_scaling(
        router,
        "DescribeScalingActivities",
        json!({
            "ServiceNamespace": "custom-resource",
            "ResourceId": resource_id,
            "ScalableDimension": DIMENSION,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected DescribeScalingActivities to answer 200, got {status} {body:?}"
    );
    body["ScalingActivities"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Poll the activity log until the movement the row expects has settled, or the budget is up.
async fn poll_until_settled(
    router: &MultiServiceRouter,
    row: &Row,
    resource_id: &str,
) -> Vec<Value> {
    let settled = |listed: &[Value]| match row.moved_to {
        None => listed.is_empty(),
        Some(_) => {
            !listed.is_empty()
                && listed
                    .iter()
                    .all(|activity| activity["StatusCode"] == json!(SUCCESSFUL))
        }
    };
    let deadline = SystemTime::now() + POLL_FOR;
    let mut listed = activities(router, resource_id).await;
    while !settled(&listed) && SystemTime::now() < deadline {
        tokio::time::sleep(POLL_EVERY).await;
        listed = activities(router, resource_id).await;
    }
    listed
}

#[tokio::test]
async fn a_custom_resource_target_is_registered_only_on_an_endpoint_this_service_can_invoke() {
    let router = router();
    let api = given_endpoint(&router).await;
    let dimensions: Vec<String> = (0..ROWS.len()).map(dimension_id).collect();
    given_items(&router, &dimensions).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let resource_id = resource_id(row, &api, index);
        let (min, max) = row.window;
        let (status, body) = application_auto_scaling(
            &router,
            "RegisterScalableTarget",
            json!({
                "ServiceNamespace": "custom-resource",
                "ResourceId": resource_id,
                "ScalableDimension": DIMENSION,
                "MinCapacity": min,
                "MaxCapacity": max,
            }),
        )
        .await;

        if !row.accepted {
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {body:?}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?}: expected a ValidationException, got {body:?}"
            );
            let message = error_message(&body);
            assert_eq!(
                message, MISSING_PERMISSIONS,
                "case {case:?}: expected {MISSING_PERMISSIONS:?}, got {message:?}"
            );
            let registered = targets(&router, &resource_id).await;
            assert!(
                registered.is_empty(),
                "case {case:?}: expected a refused registration to register nothing, got \
                 {registered:?}"
            );
            let held = item_capacity(&router, &dimension_id(index)).await;
            assert_eq!(
                held,
                Some(REPORTED_CAPACITY as f64),
                "case {case:?}: expected a refused registration to leave the endpoint's \
                 capacity untouched, got {held:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected the registration to be accepted, got {status} {body:?}"
        );
        let registered = targets(&router, &resource_id).await;
        assert_eq!(
            registered.len(),
            1,
            "case {case:?}: expected the registered target to be reported, got {registered:?}"
        );
        assert_eq!(
            (
                registered[0]["MinCapacity"].clone(),
                registered[0]["MaxCapacity"].clone()
            ),
            (json!(min), json!(max)),
            "case {case:?}: expected the registered window, got {:?}",
            registered[0]
        );

        let listed = poll_until_settled(&router, row, &resource_id).await;
        let Some(moved_to) = row.moved_to else {
            assert!(
                listed.is_empty(),
                "case {case:?}: expected no movement, got {listed:?}"
            );
            let held = item_capacity(&router, &dimension_id(index)).await;
            assert_eq!(
                held,
                Some(REPORTED_CAPACITY as f64),
                "case {case:?}: expected a capacity already inside the window to be left \
                 untouched, got {held:?}"
            );
            continue;
        };
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected exactly one activity for the movement, got {listed:?}"
        );
        let activity = &listed[0];
        let bound = if moved_to > REPORTED_CAPACITY {
            format!("minimum capacity was set to {moved_to}")
        } else {
            format!("maximum capacity was set to {moved_to}")
        };
        let expected = json!({
            "Cause": bound,
            "Description": format!("Setting desired capacity to {moved_to}."),
            "StatusCode": SUCCESSFUL,
            "StatusMessage": format!(
                "Successfully set desired capacity to {moved_to}. Change successfully \
                 fulfilled by custom-resource."
            ),
        });
        for (name, want) in expected.as_object().expect("an object of members") {
            assert_eq!(
                &activity[name], want,
                "case {case:?}: expected {name} {want:?}, got {activity:?}"
            );
        }

        // The movement reached the ENDPOINT: the capacity the caller's own resource holds is
        // the one this service set, read back where that resource lives.
        let dimension = dimension_id(index);
        let deadline = SystemTime::now() + POLL_FOR;
        let mut held = item_capacity(&router, &dimension).await;
        while held != Some(moved_to as f64) && SystemTime::now() < deadline {
            tokio::time::sleep(POLL_EVERY).await;
            held = item_capacity(&router, &dimension).await;
        }
        assert_eq!(
            held,
            Some(moved_to as f64),
            "case {case:?}: expected the endpoint's own capacity to be {moved_to}, got {held:?}"
        );
    }
}

/// The service-linked role a `custom-resource` scalable target is scaled through.
///
/// Source: the `RoleARN` a scalable target reports is the role "that allows Application Auto
/// Scaling to modify the scalable target on your behalf", and the service "uses a service-linked
/// role, which it creates if it does not yet exist" — one per namespace, named
/// `AWSServiceRoleForApplicationAutoScaling_<resource type>`.
const CUSTOM_RESOURCE_ROLE_NAME: &str = "AWSServiceRoleForApplicationAutoScaling_CustomResource";

/// The emulator's own endpoint, as a client that resolves no AWS host dials it: a loopback
/// authority with a port, which names no service and so carries the api in the path.
const EMULATOR_ENDPOINT: &str = "127.0.0.1:34991";

/// How a row's resource id ADDRESSES the endpoint.
///
/// The three spellings of one deployed stage: the api's own invoke host, and the two the
/// emulator publishes for a client that resolves that host nowhere — the reserved data-plane
/// prefix followed by the api id, and the same prefix followed by the endpoint host itself.
/// Which one a client was handed is no part of the rule, so each of them is a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressing {
    /// `https://{api-id}.execute-api.{region}.amazonaws.com/{stage}/…`.
    ApiEndpointHost,
    /// `https://{host}:{port}/_aws/execute-api/{api-id}/{stage}/…`.
    EmulatorPrefix,
    /// `https://{host}:{port}/_aws/{api-id}.execute-api.{region}.amazonaws.com/{stage}/…`.
    EmulatorPrefixedHost,
}

/// One registration whose floor is ABOVE the capacity the endpoint reports.
struct FloorRow {
    case: &'static str,
    addressing: Addressing,
    /// The capacity the endpoint's own item holds before the registration.
    reported: i64,
    /// The window the registration declares, whose floor is above [`FloorRow::reported`].
    window: (i64, i64),
}

/// The catalogue: the floor and the capacity it is above differ in value, in distance and in
/// how many digits the floor has, and each addressing of the endpoint is exercised.
const FLOOR_ROWS: &[FloorRow] = &[
    FloorRow {
        case: "a floor one above the reported capacity, on the api's own endpoint host",
        addressing: Addressing::ApiEndpointHost,
        reported: 2,
        window: (3, 6),
    },
    FloorRow {
        case: "the same floor, reached on the emulator's own endpoint",
        addressing: Addressing::EmulatorPrefix,
        reported: 2,
        window: (3, 6),
    },
    FloorRow {
        case: "a two-digit floor nine above the reported capacity",
        addressing: Addressing::EmulatorPrefix,
        reported: 3,
        window: (12, 20),
    },
    FloorRow {
        case: "a floor of one above an endpoint reporting no capacity at all",
        addressing: Addressing::EmulatorPrefixedHost,
        reported: 0,
        window: (1, 4),
    },
];

/// The dimension id a floor row's endpoint item is keyed by, which is also the last segment of
/// the resource id it registers.
fn floor_dimension_id(index: usize) -> String {
    format!("floor-{index}")
}

/// The resource id a floor row registers: the URL of one deployed stage's dimension document,
/// spelled the way that row's client was handed it.
fn floor_resource_id(row: &FloorRow, api: &str, dimension: &str) -> String {
    let path = format!("{DEPLOYED_STAGE}/{DIMENSION_PATH}/{dimension}");
    let endpoint_host = format!("{api}.execute-api.{REGION}.amazonaws.com");
    match row.addressing {
        Addressing::ApiEndpointHost => format!("https://{endpoint_host}/{path}"),
        Addressing::EmulatorPrefix => {
            format!("https://{EMULATOR_ENDPOINT}/_aws/execute-api/{api}/{path}")
        }
        Addressing::EmulatorPrefixedHost => {
            format!("https://{EMULATOR_ENDPOINT}/_aws/{endpoint_host}/{path}")
        }
    }
}

/// The item the endpoint serves for one dimension, as the service that owns it reports — the
/// capacity it holds, how many `PATCH`es it has taken, and the body of the last one.
async fn endpoint_item(router: &MultiServiceRouter, dimension: &str) -> Value {
    let (status, read) = dynamodb(
        router,
        "GetItem",
        json!({
            "TableName": CAPACITY_TABLE,
            "Key": {"id": {"S": dimension}},
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected GetItem to answer 200, got {status} {read:?}"
    );
    read["Item"].clone()
}

/// The number one of that item's members holds, which DynamoDB carries as the decimal it spells.
fn item_number(item: &Value, member: &str) -> Option<f64> {
    item[member]["N"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok())
}

/// Poll the endpoint's own item until it reports `floor`, or the budget is up — so a failing
/// case reports the item it last saw rather than hanging.
async fn poll_until_endpoint_moved(
    router: &MultiServiceRouter,
    dimension: &str,
    floor: i64,
) -> Value {
    let deadline = SystemTime::now() + POLL_FOR;
    let mut item = endpoint_item(router, dimension).await;
    while item_number(&item, "desired") != Some(floor as f64) && SystemTime::now() < deadline {
        tokio::time::sleep(POLL_EVERY).await;
        item = endpoint_item(router, dimension).await;
    }
    item
}

/// Poll the target's activity log until every activity it holds has settled `Successful` and
/// there is at least one, or the budget is up.
async fn poll_until_activities_settled(
    router: &MultiServiceRouter,
    resource_id: &str,
) -> Vec<Value> {
    let settled = |listed: &[Value]| {
        !listed.is_empty()
            && listed
                .iter()
                .all(|activity| activity["StatusCode"] == json!(SUCCESSFUL))
    };
    let deadline = SystemTime::now() + POLL_FOR;
    let mut listed = activities(router, resource_id).await;
    while !settled(&listed) && SystemTime::now() < deadline {
        tokio::time::sleep(POLL_EVERY).await;
        listed = activities(router, resource_id).await;
    }
    listed
}

/// RULE: registering a `custom-resource` target whose `MinCapacity` is above the capacity the
/// endpoint reports raises that resource to the floor as part of the registration — ONE `PATCH`
/// carrying exactly the one member the contract names (`{"desiredCapacity": <min>}`) — and
/// records exactly ONE scaling activity for the movement, which settles `Successful` and states
/// the bound that caused it, the capacity it set and the namespace that fulfilled it. The target
/// then reads back with the window it registered and the `RoleARN` of the namespace's own
/// service-linked role.
#[tokio::test]
async fn a_registration_floor_patches_the_endpoint_once_and_records_one_activity() {
    let router = router();
    let api = given_endpoint(&router).await;
    given_capacity_table(&router).await;
    for (index, row) in FLOOR_ROWS.iter().enumerate() {
        given_item(&router, &floor_dimension_id(index), row.reported).await;
    }

    for (index, row) in FLOOR_ROWS.iter().enumerate() {
        let case = row.case;
        let dimension = floor_dimension_id(index);
        let resource_id = floor_resource_id(row, &api, &dimension);
        let (min, max) = row.window;

        let (status, body) = application_auto_scaling(
            &router,
            "RegisterScalableTarget",
            json!({
                "ServiceNamespace": "custom-resource",
                "ResourceId": resource_id,
                "ScalableDimension": DIMENSION,
                "MinCapacity": min,
                "MaxCapacity": max,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the registration to be accepted, got {status} {body:?}"
        );
        assert!(
            body["ScalableTargetARN"].as_str().is_some(),
            "case {case:?}: expected the registration to answer the target's ARN, got {body:?}"
        );

        // The endpoint itself moved: the item its integrations read and write holds the floor.
        let item = poll_until_endpoint_moved(&router, &dimension, min).await;
        assert_eq!(
            item_number(&item, "desired"),
            Some(min as f64),
            "case {case:?}: expected the endpoint's own capacity to be raised to {min}, got \
             {item:?}"
        );
        // ONE request moved it, and its body is the one member the contract carries.
        assert_eq!(
            item_number(&item, "patches"),
            Some(1.0),
            "case {case:?}: expected exactly one PATCH to reach the endpoint, got {item:?}"
        );
        let expected_body = format!("{{\"desiredCapacity\": {min}.0}}");
        assert_eq!(
            item["lastBody"]["S"],
            json!(expected_body),
            "case {case:?}: expected the endpoint to have received {expected_body:?}, got {item:?}"
        );

        // And the movement is auditable: one activity, settled, in the service's own wording.
        let listed = poll_until_activities_settled(&router, &resource_id).await;
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected exactly one activity for the movement, got {listed:?}"
        );
        let activity = &listed[0];
        let expected = json!({
            "ServiceNamespace": "custom-resource",
            "ResourceId": resource_id,
            "ScalableDimension": DIMENSION,
            "Cause": format!("minimum capacity was set to {min}"),
            "Description": format!("Setting desired capacity to {min}."),
            "StatusCode": SUCCESSFUL,
            "StatusMessage": format!(
                "Successfully set desired capacity to {min}. Change successfully fulfilled by \
                 custom-resource."
            ),
        });
        for (name, want) in expected.as_object().expect("an object of members") {
            assert_eq!(
                &activity[name], want,
                "case {case:?}: expected {name} {want:?}, got {activity:?}"
            );
        }

        // Nothing moved the endpoint a second time once the movement had settled.
        let settled_item = endpoint_item(&router, &dimension).await;
        assert_eq!(
            (
                item_number(&settled_item, "desired"),
                item_number(&settled_item, "patches")
            ),
            (Some(min as f64), Some(1.0)),
            "case {case:?}: expected the settled endpoint to hold the floor after one PATCH, got \
             {settled_item:?}"
        );

        // The registered target reads back with its window and the role it is scaled through.
        let registered = targets(&router, &resource_id).await;
        assert_eq!(
            registered.len(),
            1,
            "case {case:?}: expected the registered target to be reported, got {registered:?}"
        );
        assert_eq!(
            (
                registered[0]["MinCapacity"].clone(),
                registered[0]["MaxCapacity"].clone()
            ),
            (json!(min), json!(max)),
            "case {case:?}: expected the registered window, got {:?}",
            registered[0]
        );
        let role_arn = registered[0]["RoleARN"].as_str().unwrap_or_default();
        assert!(
            role_arn.ends_with(&format!("/{CUSTOM_RESOURCE_ROLE_NAME}")),
            "case {case:?}: expected a RoleARN naming {CUSTOM_RESOURCE_ROLE_NAME:?}, got \
             {role_arn:?}"
        );
    }
}
