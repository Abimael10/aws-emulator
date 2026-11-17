// SPDX-License-Identifier: CC0-1.0

//! A target-tracking scaling policy over a `custom-resource` scalable target, driven through
//! the WHOLE assembled router on an emulator that VERIFIES request signatures.
//!
//! RULE: in the `custom-resource` namespace the scalable target is the HTTP endpoint the
//! caller's own application serves ("for a resource provided by your own application or
//! service, use custom-resource instead"), so every scaling decision Application Auto Scaling
//! takes for such a target is a call on that endpoint: it READS the capacity with `GET` and
//! MOVES it with `PATCH` of `{"desiredCapacity": <N>}` on
//! `/scalableTargetDimensions/{id}`. A target-tracking policy is executed by the alarm it
//! minted on the metric it tracks — with `DisableScaleIn` only the scale-out alarm exists,
//! "scale in is disabled and the target tracking scaling policy won't remove capacity" — and
//! the capacity it asks for is the least capacity at which the metric would sit at the target
//! value, clamped into the registered `MinCapacity`/`MaxCapacity` window and recorded as one
//! `Successful` scaling activity.
//!
//! Why this suite is HERE rather than beside the service's own: the endpoint is an API Gateway
//! REST stage whose methods are `AWS_IAM`-authorized and whose integrations are DynamoDB
//! actions made under an IAM role, and the calls Application Auto Scaling makes on it are
//! in-process calls of the emulator this suite assembles — so the authentication the stage
//! applies, the role the integration assumes and the service that owns the item are all the
//! real ones, over one engine, with signatures verified exactly as a client's are.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aws_credential_types::Credentials;
use aws_emulator_router::{build_router, InMemoryEngine};
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every call is signed for.
const REGION: &str = "us-east-1";

/// The stage the endpoint is deployed to.
const STAGE: &str = "prod";

/// The path the custom-resource contract publishes for one scalable dimension.
const DIMENSION_PATH: &str = "scalableTargetDimensions";

/// The dimension id a row's item is keyed by, which is also the last segment of its resource
/// id.
fn dimension_id(index: usize) -> String {
    format!("sim-dimension-{index}")
}

/// The dimension the namespace publishes: "custom resources are not supported with a resource
/// type […] the unique identifier is defined by the service provider".
const SCALABLE_DIMENSION: &str = "custom-resource:ResourceType:Property";

/// The table the endpoint's one item lives in.
const CAPACITY_TABLE: &str = "endpoint_capacity";

/// The capacity the endpoint starts at, and the floor every row registers.
const STARTING_CAPACITY: i64 = 1;
const MIN_CAPACITY: i64 = 1;

/// The policy's target value and the value the published series takes: ten times the target,
/// so the control loop asks for ten times the capacity the endpoint holds.
const TARGET_VALUE: f64 = 10.0;
const PUBLISHED_VALUE: f64 = 100.0;

/// The capacity the control loop asks for: `STARTING_CAPACITY * PUBLISHED_VALUE / TARGET_VALUE`.
const ASKED_CAPACITY: i64 = 10;

/// The metric the policy tracks, in a customer namespace so every datapoint is published the
/// way a client publishes one. One metric per row, so no row grades another's series.
const METRIC_NAMESPACE: &str = "Emulator/CustomResourceTracking";
fn metric_name(index: usize) -> String {
    format!("Load{index}")
}

/// The policy a row's alarm executes.
fn policy_name(index: usize) -> String {
    format!("custom-resource-out-{index}")
}

/// The period, in seconds, the alarms of a customised series watch it on, and how many closed
/// windows the series is published into — more than the three the scale-out alarm grades.
const SERIES_PERIOD: f64 = 60.0;
const PUBLISHED_WINDOWS: i64 = 18;

/// How long a leg is given to reach its observable state, and how often it is looked at.
///
/// Nothing in the emulator sleeps for a leg: each poll is the TEST waiting for what the clocks
/// owe (the table becoming writable, the alarm's evaluation, the policy execution and the call
/// it makes on the endpoint), so a leg that never arrives fails on the value it last read.
const LEG_BUDGET: Duration = Duration::from_secs(60);
/// See [`LEG_BUDGET`].
const LEG_POLL: Duration = Duration::from_millis(200);

/// The role the integrations are made under, and the service principal that assumes it.
const INTEGRATION_ROLE: &str = "endpoint-integration-role";
const APIGATEWAY_PRINCIPAL: &str = "apigateway.amazonaws.com";

/// One request dispatched through the router, and what came back.
struct Answer {
    status: u16,
    body: Value,
    text: String,
}

/// Dispatch one request through the router.
async fn dispatch(router: &MultiServiceRouter, request: http::Request<Vec<u8>>) -> Answer {
    let (parts, body) = request.into_parts();
    let request = http::Request::from_parts(parts, bytes::Bytes::from(body));
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(&response.into_body()).to_string();
    Answer {
        status,
        body: serde_json::from_str(&text).unwrap_or(Value::Null),
        text,
    }
}

/// The credential a call carries, or `None` for a call made while the router authenticates
/// nothing.
#[derive(Clone)]
struct Signer {
    key_id: String,
    secret: String,
}

/// Sign `request` for `signing_name`, the way every caller of this emulator signs.
fn signed(
    request: http::Request<Vec<u8>>,
    signing_name: &str,
    signer: &Signer,
) -> http::Request<Vec<u8>> {
    let mut request = request;
    let identity = Credentials::new(&signer.key_id, &signer.secret, None, None, "test").into();
    let params = SigningParams::builder()
        .identity(&identity)
        .region(REGION)
        .name(signing_name)
        .time(SystemTime::now())
        .settings(SigningSettings::default())
        .build()
        .expect("the signing parameters are complete")
        .into();
    let (method, uri) = (request.method().to_string(), request.uri().to_string());
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().expect("a text header").to_string(),
            )
        })
        .collect();
    let signable = SignableRequest::new(
        &method,
        &uri,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        SignableBody::Bytes(request.body()),
    )
    .expect("the request is signable");
    let (instructions, _) = sign(signable, &params)
        .expect("the request signs")
        .into_parts();
    instructions.apply_to_request_http1x(&mut request);
    request
}

/// The `Authorization` header a call carries while the router authenticates nothing: the
/// credential scope, which is what names the target service and its region.
fn credential_scope(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// Call one JSON-protocol operation of `signing_name`, signed when a credential is given.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    host_prefix: &str,
    content_type: &str,
    target: &str,
    body: Value,
    signer: Option<&Signer>,
) -> Answer {
    let host = format!("{host_prefix}.{REGION}.amazonaws.com");
    let mut built = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .body(body.to_string().into_bytes())
        .expect("the test request is well formed");
    built = match signer {
        Some(signer) => signed(built, signing_name, signer),
        None => {
            built.headers_mut().insert(
                http::header::AUTHORIZATION,
                http::HeaderValue::from_str(&credential_scope(signing_name))
                    .expect("the scope is a header value"),
            );
            built
        }
    };
    dispatch(router, built).await
}

/// One Application Auto Scaling call.
async fn application_auto_scaling(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    signer: Option<&Signer>,
) -> Answer {
    json_call(
        router,
        "application-autoscaling",
        "application-autoscaling",
        "application/x-amz-json-1.1",
        &format!("AnyScaleFrontendService.{operation}"),
        body,
        signer,
    )
    .await
}

/// One DynamoDB call.
async fn dynamodb(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    signer: Option<&Signer>,
) -> Answer {
    json_call(
        router,
        "dynamodb",
        "dynamodb",
        "application/x-amz-json-1.0",
        &format!("DynamoDB_20120810.{operation}"),
        body,
        signer,
    )
    .await
}

/// One CloudWatch call.
async fn cloudwatch(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    signer: Option<&Signer>,
) -> Answer {
    json_call(
        router,
        "monitoring",
        "monitoring",
        "application/x-amz-json-1.0",
        &format!("GraniteServiceVersion20100801.{operation}"),
        body,
        signer,
    )
    .await
}

/// One IAM call, which that service serves over a form-encoded query protocol.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> Answer {
    let host = format!("iam.{REGION}.amazonaws.com");
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", credential_scope("iam"))
        .body(body.into_bytes())
        .expect("the test request is well formed");
    dispatch(router, request).await
}

/// Percent-encode one query value, as a form-encoded client does.
fn form_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// One API Gateway control-plane call, which that service binds to a REST route.
async fn api_gateway(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", credential_scope("apigateway"))
        .body(
            body.map(|body| body.to_string().into_bytes())
                .unwrap_or_default(),
        )
        .expect("the test request is well formed");
    let answer = dispatch(router, request).await;
    assert!(
        (200..300).contains(&answer.status),
        "setup {what}: expected the call to be accepted, got {} {}",
        answer.status,
        answer.text
    );
    answer.body
}

/// A required string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("setup {what} must carry a non-empty {member:?}, got {body}"))
}

/// The document the custom-resource contract declares, rendered from the item the table
/// answered at `source`.
///
/// Written the way API Gateway's own mapping-template examples are written — bind the
/// document the service answered, then read its members — because that is how a developer
/// writes one, and it is what the recorded fixture of this scenario carries.
fn capacity_document(source: &str) -> String {
    format!(
        r#"#set($i = $input.path('{source}')) {{"actualCapacity":$i.desired.N,"desiredCapacity":$i.desired.N,"dimensionName":"{SCALABLE_DIMENSION}","resourceName":"$input.params('{DIMENSION_PATH}Id')","scalableTargetDimensionId":"$input.params('{DIMENSION_PATH}Id')","scalingStatus":"Successful","version":"1.0"}}"#
    )
}

/// Create the role the integrations are made under, allowing the two table actions the
/// contract's verbs perform, and answer its ARN.
async fn given_integration_role(router: &MultiServiceRouter) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": APIGATEWAY_PRINCIPAL},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", INTEGRATION_ROLE),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup CreateRole: expected 200, got {} {}",
        created.status, created.text
    );
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["dynamodb:GetItem", "dynamodb:UpdateItem"],
            "Resource": "*",
        }],
    })
    .to_string();
    let attached = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", INTEGRATION_ROLE),
            ("PolicyName", "endpoint-table-access"),
            ("PolicyDocument", &policy),
        ],
    )
    .await;
    assert_eq!(
        attached.status, 200,
        "setup PutRolePolicy: expected 200, got {} {}",
        attached.status, attached.text
    );
    format!(
        "arn:aws:iam::000000000000:role/{INTEGRATION_ROLE}"
    )
}

/// Stand the endpoint up: a REGIONAL REST API whose `/scalableTargetDimensions/{id}` resource
/// serves the capacity document on `GET` and takes a capacity change on `PATCH`, each an
/// `AWS_IAM`-authorized method integrated with the DynamoDB action that serves it under
/// `role_arn`, deployed to one stage — and answer the api id.
async fn given_endpoint(router: &MultiServiceRouter, role_arn: &str) -> String {
    let created = api_gateway(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({
            "name": "capacity-endpoint",
            "endpointConfiguration": {"types": ["REGIONAL"]},
        })),
    )
    .await;
    let api = as_str("CreateRestApi", &created, "id");
    let root = as_str("CreateRestApi", &created, "rootResourceId");

    let dimensions = api_gateway(
        router,
        "CreateResource(dimensions)",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({"pathPart": DIMENSION_PATH})),
    )
    .await;
    let dimensions = as_str("CreateResource(dimensions)", &dimensions, "id");
    let dimension = api_gateway(
        router,
        "CreateResource(id)",
        "POST",
        &format!("/restapis/{api}/resources/{dimensions}"),
        Some(json!({"pathPart": format!("{{{DIMENSION_PATH}Id}}")})),
    )
    .await;
    let dimension = as_str("CreateResource(id)", &dimension, "id");

    for (verb, action, request_template, capacity_source) in [
        (
            "GET",
            "GetItem",
            format!(
                r#"{{"TableName":"{CAPACITY_TABLE}","Key":{{"id":{{"S":"$input.params('{DIMENSION_PATH}Id')"}}}}}}"#
            ),
            "$.Item",
        ),
        (
            "PATCH",
            "UpdateItem",
            // Written the way the recorded fixture writes it: the capacity asked for is BOUND
            // and then compared, so an endpoint asked to move to no capacity at all records
            // the body without moving.
            format!(
                r#"#set($d = $input.path('$.desiredCapacity')) {{"TableName":"{CAPACITY_TABLE}","Key":{{"id":{{"S":"$input.params('{DIMENSION_PATH}Id')"}}}},#if($d < 0)"UpdateExpression":"SET lastBody = :b ADD patches :one","ExpressionAttributeValues":{{":b":{{"S":"$util.escapeJavaScript($input.body)"}},":one":{{"N":"1"}}}},#else"UpdateExpression":"SET desired = :d, lastBody = :b ADD patches :one","ExpressionAttributeValues":{{":d":{{"N":"$d"}},":b":{{"S":"$util.escapeJavaScript($input.body)"}},":one":{{"N":"1"}}}},#end"ReturnValues":"UPDATED_NEW"}}"#
            ),
            "$.Attributes",
        ),
    ] {
        let base = format!("/restapis/{api}/resources/{dimension}/methods/{verb}");
        api_gateway(
            router,
            "PutMethod",
            "PUT",
            &base,
            Some(json!({"authorizationType": "AWS_IAM"})),
        )
        .await;
        api_gateway(
            router,
            "PutIntegration",
            "PUT",
            &format!("{base}/integration"),
            Some(json!({
                "type": "AWS",
                "httpMethod": "POST",
                "uri": format!("arn:aws:apigateway:{REGION}:dynamodb:action/{action}"),
                "credentials": role_arn,
                "requestTemplates": {"application/json": request_template},
            })),
        )
        .await;
        api_gateway(
            router,
            "PutMethodResponse",
            "PUT",
            &format!("{base}/responses/200"),
            Some(json!({})),
        )
        .await;
        api_gateway(
            router,
            "PutIntegrationResponse",
            "PUT",
            &format!("{base}/integration/responses/200"),
            Some(json!({
                "responseTemplates": {"application/json": capacity_document(capacity_source)},
            })),
        )
        .await;
    }

    api_gateway(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({"stageName": STAGE})),
    )
    .await;
    api
}

/// Create the table the endpoint's items live in, once the owning service has made it
/// writable.
async fn given_capacity_table(router: &MultiServiceRouter) {
    let created = dynamodb(
        router,
        "CreateTable",
        json!({
            "TableName": CAPACITY_TABLE,
            "KeySchema": [{"AttributeName": "id", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "id", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
        None,
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup CreateTable: expected 200, got {} {}",
        created.status, created.text
    );
    let deadline = Instant::now() + LEG_BUDGET;
    loop {
        let described = dynamodb(
            router,
            "DescribeTable",
            json!({"TableName": CAPACITY_TABLE}),
            None,
        )
        .await;
        if described.body["Table"]["TableStatus"] == json!("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: expected the table to become ACTIVE, last saw {}",
            described.text
        );
        tokio::time::sleep(LEG_POLL).await;
    }
}

/// Put the item the endpoint serves for one dimension, at [`STARTING_CAPACITY`], with the
/// counter its `PATCH` integration adds to.
async fn given_item(router: &MultiServiceRouter, dimension: &str, signer: &Signer) {
    let written = dynamodb(
        router,
        "PutItem",
        json!({
            "TableName": CAPACITY_TABLE,
            "Item": {
                "id": {"S": dimension},
                "desired": {"N": STARTING_CAPACITY.to_string()},
                "patches": {"N": "0"},
            },
        }),
        Some(signer),
    )
    .await;
    assert_eq!(
        written.status, 200,
        "setup PutItem: expected 200, got {} {}",
        written.status, written.text
    );
}

/// The middle of the `index`-th most recent period window that has already CLOSED.
fn graded_window_stamp(index: i64) -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64();
    ((now / SERIES_PERIOD).floor() - index as f64) * SERIES_PERIOD + SERIES_PERIOD / 2.0
}

/// The item the endpoint serves: the capacity it holds, how many `PATCH`es it has taken, and
/// the body of the last one.
async fn item_state(
    router: &MultiServiceRouter,
    signer: &Signer,
    dimension: &str,
) -> (Option<f64>, i64, Option<String>) {
    let read = dynamodb(
        router,
        "GetItem",
        json!({
            "TableName": CAPACITY_TABLE,
            "Key": {"id": {"S": dimension}},
            "ConsistentRead": true,
        }),
        Some(signer),
    )
    .await;
    assert_eq!(
        read.status, 200,
        "expected GetItem to answer 200, got {} {}",
        read.status, read.text
    );
    let item = &read.body["Item"];
    (
        item["desired"]["N"]
            .as_str()
            .and_then(|value| value.parse::<f64>().ok()),
        item["patches"]["N"]
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or_default(),
        item["lastBody"]["S"].as_str().map(str::to_string),
    )
}

/// The three identity members of the scalable target.
fn target(resource_id: &str) -> Value {
    json!({
        "ServiceNamespace": "custom-resource",
        "ResourceId": resource_id,
        "ScalableDimension": SCALABLE_DIMENSION,
    })
}

/// How a row's resource id addresses the deployed stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressing {
    /// The api's own `execute-api` host, which is the address AWS publishes for a stage.
    ExecuteApiHost,
    /// This emulator's own endpoint, with the api behind the reserved data-plane path prefix —
    /// the address a client that resolves no AWS host reaches the stage at.
    EmulatorEndpoint,
}

/// One execution to pin: how the resource id addresses the endpoint, and the ceiling the
/// registered window puts on the capacity the control loop asks for.
struct Row {
    case: &'static str,
    addressing: Addressing,
    /// The `MaxCapacity` the target is registered with.
    max: i64,
    /// The capacity the endpoint must land on: the asked-for capacity, clamped to `max`.
    moved_to: i64,
}

/// The rows differ along what the rule generalises over: which of the two addresses the
/// resource id names the stage by, and whether the capacity the control loop asks for lands
/// inside the registered window or past its ceiling.
const ROWS: &[Row] = &[
    // The recorded case: the asked-for capacity is past the ceiling, so the movement happens AT
    // the ceiling.
    Row {
        case: "a breach past the registered ceiling",
        addressing: Addressing::EmulatorEndpoint,
        max: 4,
        moved_to: 4,
    },
    // The same rule inside the window, addressed by the api's own host.
    Row {
        case: "a breach inside the registered window, on the api's own host",
        addressing: Addressing::ExecuteApiHost,
        max: 20,
        moved_to: ASKED_CAPACITY,
    },
];

/// The resource id a row registers: the URL of the capacity document the endpoint serves.
fn resource_id(row: &Row, api: &str, dimension: &str) -> String {
    match row.addressing {
        Addressing::ExecuteApiHost => format!(
            "https://{api}.execute-api.{REGION}.amazonaws.com/{STAGE}/{DIMENSION_PATH}/{dimension}"
        ),
        Addressing::EmulatorEndpoint => format!(
            "https://127.0.0.1:9297/_aws/execute-api/{api}/{STAGE}/{DIMENSION_PATH}/{dimension}"
        ),
    }
}

/// Drive one row end to end.
async fn drives_the_endpoint(
    router: &MultiServiceRouter,
    api: &str,
    signer: &Signer,
    index: usize,
    row: &Row,
) {
    let case = row.case;
    let dimension = dimension_id(index);
    let metric = metric_name(index);
    let policy = policy_name(index);
    given_item(router, &dimension, signer).await;
    let resource_id = resource_id(row, api, &dimension);

    let mut registration = target(&resource_id);
    registration["MinCapacity"] = json!(MIN_CAPACITY);
    registration["MaxCapacity"] = json!(row.max);
    let registered = application_auto_scaling(
        router,
        "RegisterScalableTarget",
        registration,
        Some(signer),
    )
    .await;
    assert_eq!(
        registered.status, 200,
        "case {case:?}: expected RegisterScalableTarget to answer 200, got {} {}",
        registered.status, registered.text
    );

    // The load, published the way a client publishes it, into every window the policy's
    // scale-out alarm grades.
    let data: Vec<Value> = (1..=PUBLISHED_WINDOWS)
        .map(|window| {
            json!({
                "MetricName": metric,
                "Timestamp": graded_window_stamp(window),
                "Value": PUBLISHED_VALUE,
            })
        })
        .collect();
    let published_data = cloudwatch(
        router,
        "PutMetricData",
        json!({"Namespace": METRIC_NAMESPACE, "MetricData": data}),
        Some(signer),
    )
    .await;
    assert_eq!(
        published_data.status, 200,
        "case {case:?}: expected PutMetricData to answer 200, got {} {}",
        published_data.status, published_data.text
    );

    let mut request = target(&resource_id);
    request["PolicyName"] = json!(policy);
    request["PolicyType"] = json!("TargetTrackingScaling");
    request["TargetTrackingScalingPolicyConfiguration"] = json!({
        "TargetValue": TARGET_VALUE,
        "DisableScaleIn": true,
        // Nothing here breaches twice, so the wait a previous activity is given does not come
        // into it; it is named rather than defaulted so the row states it.
        "ScaleOutCooldown": 0,
        "ScaleInCooldown": 0,
        "CustomizedMetricSpecification": {
            "Namespace": METRIC_NAMESPACE,
            "MetricName": metric,
            "Statistic": "Average",
        },
    });
    let put = application_auto_scaling(router, "PutScalingPolicy", request, Some(signer)).await;
    assert_eq!(
        put.status, 200,
        "case {case:?}: expected PutScalingPolicy to answer 200, got {} {}",
        put.status, put.text
    );
    let names: Vec<String> = put.body["Alarms"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|alarm| alarm["AlarmName"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        names.len(),
        1,
        "case {case:?}: expected a scale-in-disabled policy to mint only its high alarm, got \
         {names:?}"
    );
    let alarm_name = names[0].clone();
    assert!(
        alarm_name.contains("-AlarmHigh-"),
        "case {case:?}: expected the one alarm to be the high one, got {alarm_name:?}"
    );

    // The movement reaches the ENDPOINT: the caller's own resource holds the capacity the
    // control loop asked for, clamped into the registered window, and the body it received is
    // the one member the contract names.
    let moved_to = row.moved_to;
    let expected_body = format!("{{\"desiredCapacity\": {:?}}}", moved_to as f64);
    let deadline = Instant::now() + LEG_BUDGET;
    let mut observed = item_state(router, signer, &dimension).await;
    while observed.0 != Some(moved_to as f64) && Instant::now() < deadline {
        tokio::time::sleep(LEG_POLL).await;
        observed = item_state(router, signer, &dimension).await;
    }
    let (held, patches, last_body) = observed;
    assert_eq!(
        held,
        Some(moved_to as f64),
        "case {case:?}: expected the endpoint's own capacity to reach {moved_to}, got {held:?} \
         after {patches} PATCH(es) with {last_body:?}"
    );
    assert_eq!(
        (patches, last_body.clone()),
        (1, Some(expected_body.clone())),
        "case {case:?}: expected exactly one PATCH carrying {expected_body:?}, got {patches} \
         with {last_body:?}"
    );

    // And the decision is recorded as ONE activity, naming the alarm and the policy that
    // caused it and the capacity it set.
    let deadline = Instant::now() + LEG_BUDGET;
    let mut listed;
    loop {
        let answer = application_auto_scaling(
            router,
            "DescribeScalingActivities",
            target(&resource_id),
            Some(signer),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected DescribeScalingActivities to answer 200, got {} {}",
            answer.status, answer.text
        );
        listed = answer.body["ScalingActivities"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let settled = !listed.is_empty()
            && listed
                .iter()
                .all(|activity| activity["StatusCode"] == json!("Successful"));
        if settled || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(LEG_POLL).await;
    }
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: expected exactly one scaling activity, got {listed:?}"
    );
    let expected = json!({
        "Cause": format!("monitor alarm {alarm_name} in state ALARM triggered policy {policy}"),
        "Description": format!("Setting desired capacity to {moved_to}."),
        "StatusCode": "Successful",
    });
    for (name, want) in expected.as_object().expect("an object of members") {
        assert_eq!(
            &listed[0][name], want,
            "case {case:?}: expected {name} {want:?}, got {:?}",
            listed[0]
        );
    }
}

#[test]
fn a_custom_resource_target_tracking_policy_patches_its_endpoint() {
    std::thread::Builder::new()
        .name("custom-resource-target-tracking".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);

                // SETUP, with authentication off so the control-plane calls need no real
                // signature: the role, the endpoint and the table behind it.
                router.set_request_authentication(false);
                let role_arn = given_integration_role(&router).await;
                let api = given_endpoint(&router, &role_arn).await;
                given_capacity_table(&router).await;

                // From here on the SAME router verifies every signature, as the emulator does
                // when it is started with authentication on — including the signature
                // Application Auto Scaling puts on the calls it makes on the endpoint.
                router.set_request_authentication(true);
                let published = ::emulator_server::SigV4Authenticator::minted_credentials()
                    .expect("turning authentication on publishes this process's keys");
                let signer = Signer {
                    key_id: published.access_key_id.clone(),
                    secret: published.secret_access_key.clone(),
                };

                for (index, row) in ROWS.iter().enumerate() {
                    drives_the_endpoint(&router, &api, &signer, index, row).await;
                }
            });
        })
        .expect("spawn the suite thread")
        .join()
        .expect("the custom-resource target-tracking suite must pass");
}
