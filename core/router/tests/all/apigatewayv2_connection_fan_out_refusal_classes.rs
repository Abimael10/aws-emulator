// SPDX-License-Identifier: CC0-1.0

//! What a BACKEND's fan-out over the `@connections` surface of one deployed WEBSOCKET stage
//! learns about each connection it addresses, end to end over the assembled router: a
//! handler that loops over its own table of connection ids, pushes to each and prunes the
//! row only for a connection that is GONE.
//!
//! > **RULE: every `@connections` call is decided by the `ConnectionId` it names and by the
//! > connection that id names, and the two refusals are not interchangeable: a value API
//! > Gateway's encoding could never have produced is refused HTTP 400 `BadRequestException`
//! > whose message is `Invalid connectionId: ` followed by the `ConnectionId` the request
//! > sent, decided from the value alone; a value of that encoding naming no connection the
//! > endpoint holds is the memberless `GoneException` (HTTP 410); and a value naming a
//! > connection the endpoint holds is served, the payload delivered to that connection. So a
//! > caller that prunes its record of a connection on the gone class alone — which is what
//! > AWS's own guidance has a backend do — keeps every row it merely mis-spelled, and no id's
//! > refusal closes, forgets or skips any other connection of the same stage, whatever order
//! > the ids are addressed in.**
//!
//! WHY the classes carry that much: a 400 says "this request is wrong" and a 410 says "stop
//! writing to this connection". A backend learns both from one loop over one table, so
//! answering the gone class where the request was merely malformed makes it delete a record it
//! should keep (and a 410 is memberless, so nothing would say what was wrong), while letting
//! one refusal end the loop makes it stop notifying the live clients it had just been told
//! about.
//!
//! Only a cross-service suite can pin it where a backend meets it: the api, the stage and the
//! deployment that make the endpoint resolve a stage at all are API Gateway V2's records, the
//! connection registry and the two refusals are the API Gateway Management API's, the
//! execution role is IAM's, and the handler and the SDK seam its calls are raised through are
//! Lambda's — so what the rows assert on is what a RUNNING handler's own SDK was answered.
//! The suites that own the neighbouring rules are not restated here: the authorization of a
//! push on the stage its client was built with is
//! `lambda_handler_posts_to_its_websocket_connections.rs`, and the same fan-out driven over
//! the api's own socket by a direct caller is the API Gateway V2 crate's
//! `websocket_connection_fan_out.rs`.
//!
//! The rows span what the rule generalises over: the FORM of the id (the shape this stage
//! issues and holds, a token of that shape it never held, and — refused by the alphabet, by
//! the padding, by the decoded length and by the format marker — five it could not have
//! issued, from one character to two hundred), and the id's POSITION in the fan-out (a live id
//! addressed first, and the same table reversed so it is addressed after every refusal).
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_apigatewaymanagementapi_apigatewaymanagementapi as mgmt;

use crate::query_wire::{element, form_encode, outputs_of};

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for, and the one the api is created in.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to, and so the stage the handler's client is built with.
const STAGE: &str = "prod";

/// The connection the stage HOLDS: an id of the shape API Gateway mints one in, registered
/// through the seam that makes a connection exist.
const LIVE_CONNECTION_ID: &str = "L0SM9cOFyjUCIhA=";

/// The payload every push carries, and the one a delivery must carry back.
const PAYLOAD: &str = "{\"event\":\"node_discovered\"}";

/// The stack, the execution role and the function it declares.
const STACK: &str = "connection-fan-out";
const ROLE: &str = "connection-fan-out-role";
const FUNCTION: &str = "connection-fan-out";

/// The inline policy the execution role carries: the one permission AWS publishes for this
/// surface, so what each row asserts is the id's own decision and not an authorization one
/// (which `lambda_handler_posts_to_its_websocket_connections.rs` owns).
const POLICY_NAME: &str = "manage-the-connections";
const MANAGE_CONNECTIONS: &str = "execute-api:ManageConnections";

/// The awsQuery API versions the setup services are called at.
const IAM_VERSION: &str = "2010-05-08";
const CLOUDFORMATION_VERSION: &str = "2010-05-15";

/// The stack status the setup waits for, and the function state an invocation needs.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ACTIVE: &str = "Active";

/// How long work the clock owes is waited for, and how often it is looked at. Generous — these
/// suites run beside every other test of the workspace, so a stack and a function runtime coming
/// up on a loaded host are given far more room than they need — and no row measures a deadline:
/// each wait is for a CONDITION, looked at again until it holds.
const WAIT: Duration = Duration::from_secs(180);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The `Timeout` the fan-out function is configured with, in seconds: a run's own wall clock,
/// which on a loaded host has to cover starting its runtime as well as the fan-out itself. No case
/// here is about that limit, so it is given generous room rather than held near what a run takes.
const FUNCTION_TIMEOUT_SECONDS: i64 = 300;

/// The three outcomes the rule decides between, and what each carries on the wire.
const SERVED_STATUS: i64 = 200;
const INVALID_STATUS: i64 = 400;
const INVALID_CODE: &str = "BadRequestException";
const GONE_STATUS: i64 = 410;
const GONE_CODE: &str = "GoneException";

/// The fixed text of the refusal for a value that is no connection identifier at all; the rest
/// of that sentence is the `ConnectionId` the request sent.
const INVALID_MESSAGE: &str = "Invalid connectionId: ";

/// The handler the stack deploys: it loops over the connection ids the event hands it — its own
/// table — pushes one payload to each through a client built with the `@connections` endpoint
/// the event names, and reports both what its SDK was answered for each id and the table it
/// kept, having pruned the row only where the connection is GONE.
const HANDLER: &str = r#"
import boto3
import botocore.exceptions

GONE = "GoneException"


def lambda_handler(event, context):
    connections = boto3.client(
        "apigatewaymanagementapi", endpoint_url=event.get("endpoint") or None
    )
    kept = []
    pruned = []
    seen = []
    for target in event.get("targets") or []:
        record = {"target": target}
        try:
            answered = connections.post_to_connection(
                ConnectionId=target, Data=event["data"]
            )
            record["pushed"] = True
            record["status"] = answered["ResponseMetadata"]["HTTPStatusCode"]
            record["code"] = ""
            record["message"] = ""
            kept.append(target)
        except botocore.exceptions.ClientError as error:
            record["pushed"] = False
            record["status"] = error.response["ResponseMetadata"]["HTTPStatusCode"]
            record["code"] = error.response["Error"]["Code"]
            record["message"] = error.response["Error"].get("Message") or ""
            # A record is dropped for a connection that has GONE, and for nothing else.
            if record["code"] == GONE:
                pruned.append(target)
            else:
                kept.append(target)
        seen.append(record)
    return {"ok": True, "kept": kept, "pruned": pruned, "seen": seen}
"#;

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// One JSON call of `service` at `host`, answered with the status and the parsed body.
async fn json_call(
    router: &MultiServiceRouter,
    service: &str,
    host: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match &body {
        Some(body) => Bytes::from(body.to_string()),
        None => Bytes::new(),
    };
    let request = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization(service))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One API Gateway V2 control-plane call, which must answer `expected`.
async fn control_plane(
    router: &MultiServiceRouter,
    operation: &str,
    path: &str,
    body: &Value,
    expected: u16,
) -> Value {
    let (status, answered) = json_call(
        router,
        "apigateway",
        &format!("apigateway.{REGION}.amazonaws.com"),
        "POST",
        path,
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        status, expected,
        "setup: {operation} must answer {expected}, got {status} with {answered}"
    );
    answered
}

/// One Lambda `restJson1` call.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    json_call(
        router,
        "lambda",
        &format!("lambda.{REGION}.amazonaws.com"),
        method,
        uri,
        body,
    )
    .await
}

/// A member of a JSON answer, as text.
fn as_str(what: &str, answered: &Value, member: &str) -> String {
    answered
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: {what} must report {member}, got {answered}"))
        .to_string()
}

/// A WEBSOCKET api with one DEPLOYED stage — what makes an `execute-api` endpoint serve that
/// stage at all. Answers the api's id.
async fn deployed_websocket_api(router: &MultiServiceRouter) -> String {
    let created = control_plane(
        router,
        "CreateApi",
        "/v2/apis",
        &json!({
            "name": "connection-fan-out",
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": "$request.body.action"
        }),
        201,
    )
    .await;
    let api_id = as_str("CreateApi", &created, "apiId");
    let integration = control_plane(
        router,
        "CreateIntegration",
        &format!("/v2/apis/{api_id}/integrations"),
        &json!({ "integrationType": "MOCK" }),
        201,
    )
    .await;
    let integration_id = as_str("CreateIntegration", &integration, "integrationId");
    control_plane(
        router,
        "CreateRoute",
        &format!("/v2/apis/{api_id}/routes"),
        &json!({
            "routeKey": "$connect",
            "target": format!("integrations/{integration_id}")
        }),
        201,
    )
    .await;
    let deployment = control_plane(
        router,
        "CreateDeployment",
        &format!("/v2/apis/{api_id}/deployments"),
        &json!({}),
        201,
    )
    .await;
    let deployment_id = as_str("CreateDeployment", &deployment, "deploymentId");
    control_plane(
        router,
        "CreateStage",
        &format!("/v2/apis/{api_id}/stages"),
        &json!({ "stageName": STAGE, "deploymentId": deployment_id }),
        201,
    )
    .await;
    api_id
}

/// The stack that declares the execution role and the fan-out handler.
fn handler_stack() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "FanOutRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": ROLE,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            },
            "FanOut": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": {"Fn::GetAtt": ["FanOutRole", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT_SECONDS,
                    "Code": {"ZipFile": HANDLER},
                },
            },
        },
        "Outputs": {
            "FunctionName": {"Value": {"Ref": "FanOut"}},
        },
    })
    .to_string()
}

/// What `DescribeStacks` reports for `stack`: its status and the whole body.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (_status, body) = query(
        router,
        "cloudformation",
        CLOUDFORMATION_VERSION,
        "DescribeStacks",
        &[("StackName", stack)],
    )
    .await;
    (element(&body, "StackStatus"), body)
}

/// Create the stack, wait for `CREATE_COMPLETE` and answer its outputs.
async fn deployed_stack(
    router: &MultiServiceRouter,
    stack: &str,
    template: &str,
) -> BTreeMap<String, String> {
    let (status, body) = query(
        router,
        "cloudformation",
        CLOUDFORMATION_VERSION,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({stack:?}) must be accepted, got {status} with {body}"
    );
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut reported = stack_status(router, stack).await;
    for _ in 0..looks {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
        reported = stack_status(router, stack).await;
    }
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {:?} with {}",
        reported.0, reported.1
    );
    outputs_of(&reported.1)
}

/// Wait until Lambda reports the function `Active` — the state an invocation needs. The span a
/// created function spends coming up is that service's own, so nothing here sleeps a fixed
/// span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let looks = WAIT.as_millis() / LOOK_INTERVAL.as_millis();
    let mut state = String::new();
    for _ in 0..looks {
        let (_status, body) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        state = body["State"].as_str().unwrap_or_default().to_string();
        if state == ACTIVE {
            return;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("setup: the function {name:?} must reach {ACTIVE}, last reported {state:?}");
}

/// Put the one grant this surface needs on the execution role, so every refusal below is the
/// `ConnectionId`'s own and not an authorization decision.
async fn given_manage_connections(router: &MultiServiceRouter) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": MANAGE_CONNECTIONS,
            "Resource": "*",
        }],
    })
    .to_string();
    let (status, body) = query(
        router,
        "iam",
        IAM_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy must be accepted, got {status} with {body}"
    );
}

/// The `@connections` endpoint of `api_id`'s stage, as AWS publishes it and as a backend's
/// client is configured with it.
fn stage_endpoint(api_id: &str) -> String {
    format!("https://{api_id}.execute-api.{REGION}.amazonaws.com/{STAGE}")
}

/// What the endpoint must answer for one addressed id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The id names a connection this endpoint holds: served, and the payload delivered.
    Served,
    /// The id is of the issued shape and names no connection here: the memberless 410.
    Gone,
    /// The id could never have been issued: 400 naming it.
    Invalid,
}

/// One id the fan-out addresses.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    /// The `ConnectionId` the handler hands its SDK.
    id: String,
    expected: Expected,
}

/// `ConnectionId` values API Gateway's encoding could never have produced, each with the way
/// it fails that encoding.
///
/// The source of the forms is the encoding rule itself — `is_well_formed_connection_id` of the
/// management API's `state` module (standard padded base64 of `CONNECTION_ID_BYTES` bytes
/// carrying the format marker at `CONNECTION_ID_FORMAT_OFFSET`) — and the rows fail it by the
/// alphabet, the padding, the decoded length and the marker in turn.
/// [`the_catalog_is_the_forms_it_claims_to_be`] holds every row to that reader.
const MALFORMED_IDS: [(&str, &str); 5] = [
    ("a single character", "x"),
    ("a dashed id", "cid-with-dashes"),
    ("an unpadded token", "Fabc1cWIPHcCJ7A"),
    ("base64 of too many bytes", "0123456789abcdef"),
    (
        "base64 of eleven zero bytes, so no format marker",
        "AAAAAAAAAAAAAAA=",
    ),
];

/// A 200-character token: the length extreme, built rather than spelled out.
fn very_long_id() -> String {
    "A".repeat(200)
}

/// A `ConnectionId` of the shape API Gateway issues, carrying `label` — built by the
/// `@connections` registry's own encoder, the one place such a token is rendered.
fn minted(label: &str) -> String {
    mgmt::state::connection_id_for(label.as_bytes())
}

/// The table one fan-out addresses: the live connection of the stage first, then a token of the
/// issued shape the endpoint never held, then every value that could not have been issued.
fn rows() -> Vec<Row> {
    let mut rows = vec![
        Row {
            case: "the live connection of the stage",
            id: LIVE_CONNECTION_ID.to_string(),
            expected: Expected::Served,
        },
        Row {
            case: "a token of the issued shape the endpoint never held",
            id: minted("never-issued"),
            expected: Expected::Gone,
        },
    ];
    for (case, id) in MALFORMED_IDS {
        rows.push(Row {
            case,
            id: id.to_string(),
            expected: Expected::Invalid,
        });
    }
    rows.push(Row {
        case: "a 200-character token",
        id: very_long_id(),
        expected: Expected::Invalid,
    });
    rows
}

/// Register the connection the fan-out's served row addresses, through the seam that makes a
/// connection exist.
fn register(engine: &Arc<engine::InMemoryEngine>) {
    assert!(
        mgmt::state::register_connection(
            &**engine,
            LIVE_CONNECTION_ID,
            mgmt::state::Identity {
                source_ip: "192.0.2.40".to_string(),
                user_agent: Some("ui/1".to_string()),
            },
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        ),
        "setup: the registration seam must register the connection"
    );
}

/// What the registry holds for the live connection: how many payloads were delivered to it, and
/// the last of them as text.
fn delivered(case: &str, engine: &Arc<engine::InMemoryEngine>) -> (u64, String) {
    let held = mgmt::state::connection(&**engine, LIVE_CONNECTION_ID)
        .unwrap_or_else(|| panic!("case {case:?}: the live connection must still be registered"));
    let last = held
        .messages
        .last()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();
    (held.delivered_count, last)
}

/// The record the handler reported for `id`.
fn record_for<'a>(case: &str, reported: &'a Value, id: &str) -> &'a Value {
    reported
        .get("seen")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the handler must report what it saw, got {reported}")
        })
        .iter()
        .find(|record| record.get("target").and_then(Value::as_str) == Some(id))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the handler must report the id {id:?}, got {reported}")
        })
}

/// The ids a reported list holds, in the order the handler wrote them.
fn listed(case: &str, reported: &Value, member: &str) -> Vec<String> {
    reported
        .get(member)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: the handler must report {member}, got {reported}"))
        .iter()
        .map(|entry| entry.as_str().unwrap_or_default().to_string())
        .collect()
}

/// Run ONE fan-out over `rows` and assert every half of the rule: what the handler's own SDK was
/// answered for each id, which rows it kept and pruned, and what the registry holds for the live
/// connection afterwards. `deliveries` is the number of payloads that connection must have been
/// delivered by the end of this pass.
async fn drive(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    function: &str,
    api_id: &str,
    order: &str,
    rows: &[Row],
    deliveries: u64,
) {
    let endpoint = stage_endpoint(api_id);
    let targets: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
    let (status, reported) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        Some(json!({
            "endpoint": endpoint,
            "targets": targets,
            "data": PAYLOAD,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {order:?}: expected HTTP 200 from Invoke, got {status} ({reported})"
    );
    assert_eq!(
        reported.get("ok").and_then(Value::as_bool),
        Some(true),
        "case {order:?}: the handler must have run the whole fan-out, got {reported}"
    );

    for row in rows {
        let case = format!("{order}: {}", row.case);
        let record = record_for(&case, &reported, &row.id);
        let answered_status = record.get("status").and_then(Value::as_i64);
        let answered_code = record.get("code").and_then(Value::as_str).unwrap_or_default();
        let answered_message = record
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match row.expected {
            Expected::Served => {
                assert_eq!(
                    answered_status,
                    Some(SERVED_STATUS),
                    "case {case:?}: expected the operation's own {SERVED_STATUS}, got {record}"
                );
                assert_eq!(
                    record.get("pushed").and_then(Value::as_bool),
                    Some(true),
                    "case {case:?}: the push must be served, got {record}"
                );
            }
            Expected::Gone => {
                assert_eq!(
                    answered_status,
                    Some(GONE_STATUS),
                    "case {case:?}: expected HTTP {GONE_STATUS}, got {record}"
                );
                assert_eq!(
                    answered_code, GONE_CODE,
                    "case {case:?}: expected {GONE_CODE}, got {answered_code:?}"
                );
            }
            Expected::Invalid => {
                assert_eq!(
                    answered_status,
                    Some(INVALID_STATUS),
                    "case {case:?}: expected HTTP {INVALID_STATUS}, got {record}"
                );
                assert_eq!(
                    answered_code, INVALID_CODE,
                    "case {case:?}: expected {INVALID_CODE}, got {answered_code:?}"
                );
                assert_eq!(
                    answered_message,
                    format!("{INVALID_MESSAGE}{}", row.id),
                    "case {case:?}: expected the message to name the id the request sent, got \
                     {answered_message:?}"
                );
            }
        }
    }

    // The table the backend is left holding: every row but the one that is GONE, in the order
    // it addressed them — so no refusal of one id dropped or skipped another.
    let expected_kept: Vec<String> = rows
        .iter()
        .filter(|row| row.expected != Expected::Gone)
        .map(|row| row.id.clone())
        .collect();
    let expected_pruned: Vec<String> = rows
        .iter()
        .filter(|row| row.expected == Expected::Gone)
        .map(|row| row.id.clone())
        .collect();
    assert_eq!(
        listed(order, &reported, "kept"),
        expected_kept,
        "case {order:?}: a backend pruning only the gone class must keep every other row"
    );
    assert_eq!(
        listed(order, &reported, "pruned"),
        expected_pruned,
        "case {order:?}: only the connection that is gone may be pruned"
    );

    // And the live connection of the same stage was notified, whatever the refusals beside it.
    let (count, last) = delivered(order, engine);
    assert_eq!(
        (count, last.as_str()),
        (deliveries, PAYLOAD),
        "case {order:?}: expected {deliveries} delivery/deliveries of {PAYLOAD:?} to the live \
         connection, got {count} with {last:?}"
    );
}

/// The function one output of the stack named, falling back on the name it was declared with —
/// which [`wait_until_active`] then holds to the function really existing, so a fallback onto a
/// name Lambda does not hold fails the setup rather than passing silently.
fn named(outputs: &BTreeMap<String, String>, output: &str, declared: &str) -> String {
    outputs
        .get(output)
        .cloned()
        .unwrap_or_else(|| declared.to_string())
}

/// The catalog above is the forms it claims to be, read against the encoding rule itself: no
/// malformed row could have been issued, and both ids the rows expect an answer FROM the
/// registry for could have been.
#[test]
fn the_catalog_is_the_forms_it_claims_to_be() {
    for row in rows() {
        let could_be_issued = mgmt::state::is_well_formed_connection_id(&row.id);
        match row.expected {
            Expected::Invalid => assert!(
                !could_be_issued,
                "case {:?}: {:?} must NOT be a shape API Gateway could have issued",
                row.case, row.id
            ),
            Expected::Served | Expected::Gone => assert!(
                could_be_issued,
                "case {:?}: {:?} must be a shape API Gateway issues",
                row.case, row.id
            ),
        }
    }
}

#[test]
fn a_fan_outs_refusals_keep_every_other_connection_of_the_stage() {
    std::thread::Builder::new()
        .name("connection-fan-out-refusal-classes".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            // `enable_io`: a WEBSOCKET api's data plane is a socket the core opens when the api
            // is created, so the runtime this suite creates one on has to bind one.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
                let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
                // What is under test is the answer each `ConnectionId` earns, not a signature.
                router.set_request_authentication(false);

                let api_id = deployed_websocket_api(&router).await;
                let outputs = deployed_stack(&router, STACK, &handler_stack()).await;
                let function = named(&outputs, "FunctionName", FUNCTION);
                wait_until_active(&router, &function).await;
                given_manage_connections(&router).await;
                register(&engine);

                let forward = rows();
                drive(
                    &router,
                    &engine,
                    &function,
                    &api_id,
                    "the live id addressed first",
                    &forward,
                    1,
                )
                .await;

                // The same table REVERSED: the live id is addressed after every refusal, so a
                // loop a refusal had ended would leave it unnotified.
                let mut reversed = forward;
                reversed.reverse();
                drive(
                    &router,
                    &engine,
                    &function,
                    &api_id,
                    "the live id addressed last",
                    &reversed,
                    2,
                )
                .await;
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
