// SPDX-License-Identifier: CC0-1.0

//! The CLOSING legs of a WEBSOCKET api's backend-response lifecycle, through the WHOLE assembled
//! router: the `$disconnect` leg a client's socket drives when it ends, and what the
//! connection's id addresses afterwards.
//!
//! > **RULE: the end of a connection of a deployed WEBSOCKET api is as DEFINITE as its start.
//! > When the client's socket ends, the api performs the `$disconnect` route's integration for
//! > that connection — one call per connection, naming the connection, the stage and the route
//! > that served it, so what that integration writes is held by the services that own it and
//! > what its run printed is in the function's log group — and the connection's record then
//! > GOES: its id, which stays one API Gateway issued, addresses nothing, so every
//! > `@connections` call naming it is refused `410 GoneException` at the endpoint the api
//! > publishes. No leg of that is left pending: a backend that pushes to a connection whose
//! > client has gone is told so, promptly, instead of waiting.**
//!
//! WHY it matters: `410` is the "this peer went away" signal a fan-out application prunes its own
//! subscriber row on, and the `$disconnect` call is the only notice a backend gets that it may.
//! A connection that lingers after its socket ended leaves every later push looking successful
//! while nothing is delivered, and a `$disconnect` that never runs leaves the backend's own
//! records of live clients growing forever — and a call that answers neither cannot be told from
//! a hung network.
//!
//! Five crates have to agree for the rule to hold — API Gateway V2 serves the socket and
//! performs the lifecycle routes, AWS Lambda runs the function behind them, Amazon DynamoDB
//! holds what that function writes, Amazon CloudWatch Logs holds the records of its runs, and
//! the API Gateway Management API answers for the id afterwards — with CloudFormation
//! provisioning the function and the role, so the suite lives here rather than in any one of
//! them.
//!
//! The rows differ along what the rule generalises over: the stage the client addressed, the
//! agent it named in its query string, and the `Sec-WebSocket-Key` it offered (the pair RFC 6455
//! §1.3 publishes, and a nonce it publishes no answer for). Each row drives its own connection,
//! so a `$disconnect` attributed to the wrong connection, or a record written once for several,
//! fails here.
//!
//! What the neighbouring suites own and this one does not restate: the status a handshake earns
//! for each state of the api's records and the `$connect` integration behind it are
//! [`crate::apigatewayv2_websocket_connect_registers_its_agent`] (whose handshake writer this
//! suite drives); what the SESSION behind an upgraded handshake carries — the route each frame
//! selects, the reply the backend pushes back through `@connections`, and the state and records
//! that leaves — is the AWS Lambda crate's `websocket_backend_response_lifecycle.rs`, which
//! drives it over the same announced address with a WebSocket client library; and what each
//! `@connections` operation answers for every FORM of id is
//! [`crate::apigatewaymanagementapi_minted_connection_endpoint`].

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigatewayv2_websocket_connect_registers_its_agent::{
    answered_handshake_at, ACCEPT_FIELD, RFC_EXAMPLE_ACCEPT, RFC_EXAMPLE_KEY,
};
use crate::cloudformation_dynamodb_table_in_place_update::dynamodb;
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_lambda_function_log_destination::{
    cloudformation, emulator, lambda, logs_policy, messages_until_condition, trust_policy,
    wait_until_active, CREATE_COMPLETE, DEFAULT_GROUP_PREFIX,
};
use crate::query_wire::outputs_of;

/// The Region every request below is signed for and every address names — the one the shared
/// stack, log and table helpers call in.
const REGION: &str = "us-east-1";

/// The expression AWS documents for a WebSocket api whose messages carry an `action` property.
const SELECTION_EXPRESSION: &str = "$request.body.action";

/// The two lifecycle routes a connection drives, both fronting the one function.
const CONNECT_ROUTE: &str = "$connect";
const DISCONNECT_ROUTE: &str = "$disconnect";

/// The stack this suite deploys, the table its handler writes, and the logical ids it declares.
const STACK: &str = "websocket-connection-teardown";
const TABLE: &str = "websocket-connection-teardown-agents";
const FUNCTION_ID: &str = "Lifecycle";
const ROLE_ID: &str = "LifecycleRole";

/// The `TableStatus` DynamoDB reports a table that takes reads and writes in: "you can perform
/// read and write operations only on an ACTIVE table".
const TABLE_ACTIVE: &str = "ACTIVE";

/// The status RFC 6455 §4.2.2 requires of a COMPLETED opening handshake.
const SWITCHING_PROTOCOLS: u16 = 101;

/// What a `@connections` call naming a connection that has ended is refused with: the status,
/// and the shape it names in the header a `restJson1` client reads an error's shape from — which
/// is where this one names itself, `GoneException` being memberless.
const GONE: u16 = 410;
const GONE_CODE: &str = "GoneException";
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The `Timeout` the function is configured with, in seconds: a run's own wall clock, which on a
/// loaded host has to cover starting its runtime as well. No case here is about that limit.
const FUNCTION_TIMEOUT: u64 = 300;

/// How long a leg that is OWED is waited for before this suite calls it pending: the row an
/// integration wrote, the record of its run, and the refusal a closed connection's id earns.
/// Every one of them waits for the CONDITION, and nothing here asserts on elapsed time; the
/// budget is well above any span these services choose while still reporting a leg that answers
/// nothing promptly.
const LEG_DEADLINE: Duration = Duration::from_secs(60);

/// How often a condition another service owes is looked at again.
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The handler both lifecycle routes front. It keeps one row per CONNECTION, stamped with the
/// call the api last made for it, and — for the connection REQUEST, the one call that carries the
/// handshake's query string — a second row under the AGENT that handshake named, which is how a
/// client that knows only its own agent name learns the id its connection was minted. It prints
/// the same, which is what an operator debugging a lifecycle reads.
const HANDLER_SOURCE: &str = r#"
import os

import boto3


def handler(event, context):
    called = event["requestContext"]
    tables = boto3.client("dynamodb")
    table = os.environ["TABLE"]
    record = {
        "connectionId": {"S": called["connectionId"]},
        "stage": {"S": called["stage"]},
        "routeKey": {"S": called["routeKey"]},
    }
    tables.put_item(
        TableName=table,
        Item=dict(record, id={"S": called["connectionId"]}),
    )
    agent = (event.get("queryStringParameters") or {}).get("agent")
    if agent:
        tables.put_item(TableName=table, Item=dict(record, id={"S": agent}))
    print("%s %s on %s" % (called["routeKey"], called["connectionId"], called["stage"]))
    return {"statusCode": 200}
"#;

/// One connection: the stage its client addressed, the agent it named, the key it offered and
/// the accept value RFC 6455 §1.3 publishes for that key, where it publishes one.
struct Row {
    case: &'static str,
    stage: &'static str,
    agent: &'static str,
    key: &'static str,
    published_accept: Option<&'static str>,
}

/// The rows span the stage, the agent and the client's nonce; each drives its own connection.
const ROWS: &[Row] = &[
    Row {
        case: "a connection of the released stage, with the nonce RFC 6455 §1.3 publishes",
        stage: "prod",
        agent: "agent-one",
        key: RFC_EXAMPLE_KEY,
        published_accept: Some(RFC_EXAMPLE_ACCEPT),
    },
    Row {
        case: "a connection of another released stage, another agent and another nonce",
        stage: "preview-2",
        agent: "second-agent",
        key: "AQIDBAUGBwgJCgsMDQ4PEA==",
        published_accept: None,
    },
];

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request. The signature itself is not under test here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One answered request: its status, the error shape it named and its body.
struct Answered {
    status: u16,
    /// The shape a `restJson1` client resolves a failure's error from — the `x-amzn-errortype`
    /// response header — which is the only place a MEMBERLESS error names itself.
    error_type: String,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// Dispatch one request as it arrives on the wire, at `host`, signed for `signing_name`.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    method: &str,
    path: &str,
    body: Bytes,
) -> Answered {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization(signing_name))
        .body(body)
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        error_type: parts
            .headers
            .get(ERROR_TYPE_HEADER)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string())
            .unwrap_or_default(),
        body,
    }
}

/// One API Gateway V2 control-plane call that has to be accepted with `expected`. The V2
/// endpoint shares the control plane's `apigateway` signing name.
async fn control_plane(
    router: &MultiServiceRouter,
    operation: &str,
    path: &str,
    body: Value,
    expected: u16,
) -> Value {
    let answered = dispatch(
        router,
        "apigateway",
        &format!("apigateway.{REGION}.amazonaws.com"),
        "POST",
        path,
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        answered.status,
        expected,
        "setup: {operation} must answer {expected}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// A required string member of a JSON answer.
fn member(what: &str, answered: &Value, name: &str) -> String {
    answered
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: {what} must report {name}, got {answered}"))
        .to_string()
}

/// `PostToConnection` on `connection_id` at the api's published `@connections` endpoint — "the
/// URI of the API, of the form `{api-id}.execute-api.{region}.amazonaws.com`. The stage name is
/// typically appended to this URI" (`Api$ApiEndpoint`) — as it was answered.
async fn post_to_connection(
    router: &MultiServiceRouter,
    api_id: &str,
    stage: &str,
    connection_id: &str,
) -> Answered {
    dispatch(
        router,
        "execute-api",
        &format!("{api_id}.execute-api.{REGION}.amazonaws.com"),
        "POST",
        &format!(
            "/{stage}/@connections/{encoded}",
            encoded = crate::cloudfront_key_value_store_handshake::segment(connection_id)
        ),
        Bytes::from_static(b"after the socket ended"),
    )
    .await
}

/// The stack this suite deploys: the execution role the lifecycle calls are performed under, and
/// the one function both lifecycle routes front.
fn lifecycle_stack() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            ROLE_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [
                        {
                            "PolicyName": "basic-execution",
                            "PolicyDocument": logs_policy()
                        },
                        {
                            "PolicyName": "connection-records",
                            "PolicyDocument": {
                                "Version": "2012-10-17",
                                "Statement": [{
                                    "Effect": "Allow",
                                    "Action": ["dynamodb:PutItem", "dynamodb:GetItem"],
                                    "Resource": "*"
                                }]
                            }
                        }
                    ]
                }
            },
            FUNCTION_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Role": { "Fn::GetAtt": [ROLE_ID, "Arn"] },
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                    "Environment": { "Variables": { "TABLE": TABLE } }
                }
            }
        },
        "Outputs": {
            "FunctionName": { "Value": { "Ref": FUNCTION_ID } }
        }
    })
    .to_string()
}

/// Create the table the handler records each call in, and wait for DynamoDB to report it ready
/// for the writes that handler makes.
async fn agents_table(router: &MultiServiceRouter) {
    let (status, created) = dynamodb(
        router,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST"
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateTable({TABLE:?}) must be accepted, got {status} with {created}"
    );
    let looks = LEG_DEADLINE.as_millis() / LOOK_INTERVAL.as_millis();
    let mut reported = Value::Null;
    for _ in 0..looks {
        let (_status, described) =
            dynamodb(router, "DescribeTable", &json!({ "TableName": TABLE })).await;
        reported = described;
        if reported
            .get("Table")
            .and_then(|table| table.get("TableStatus"))
            .and_then(Value::as_str)
            == Some(TABLE_ACTIVE)
        {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("setup: DynamoDB never reported {TABLE} {TABLE_ACTIVE}, last read {reported}");
}

/// The row the table holds under `id`, read strongly consistently so that what is read is what
/// the handler wrote, polled until it names `route_key` as the call it last served.
async fn row_for_route(
    router: &MultiServiceRouter,
    case: &str,
    id: &str,
    route_key: &str,
) -> Value {
    let looks = LEG_DEADLINE.as_millis() / LOOK_INTERVAL.as_millis();
    let mut held = Value::Null;
    for _ in 0..looks {
        let (status, read) = dynamodb(
            router,
            "GetItem",
            &json!({
                "TableName": TABLE,
                "Key": { "id": { "S": id } },
                "ConsistentRead": true
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetItem on {TABLE:?} must answer 200, got {status} with {read}"
        );
        held = read.get("Item").cloned().unwrap_or(Value::Null);
        if string_member(&held, "routeKey").as_deref() == Some(route_key) {
            return held;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!(
        "case {case:?}: the {route_key} integration must leave its own row under {id:?} in \
         {TABLE:?}, last read {held}"
    );
}

/// The string value an item holds for `name`.
fn string_member(item: &Value, name: &str) -> Option<String> {
    item.get(name)
        .and_then(|value| value.get("S"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Wait until the function's own log group holds a record containing `marker`.
async fn logged(router: &MultiServiceRouter, case: &str, function: &str, marker: &str) {
    let group = format!("{DEFAULT_GROUP_PREFIX}{function}");
    let held = messages_until_condition(router, &group, |records| {
        records.iter().any(|record| record.contains(marker))
    })
    .await;
    assert!(
        held.iter().any(|record| record.contains(marker)),
        "case {case:?}: expected the run's log to hold a record containing {marker:?}, got {held:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_websocket_connection_ends_as_definitely_as_it_began() {
    let router = emulator();

    // 1. The table the lifecycle routes record each call in.
    agents_table(&router).await;

    // 2. The execution role and the function both lifecycle routes front.
    let template = lifecycle_stack();
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateStack must be accepted, got {} with {:?}",
        created.status,
        created.text()
    );
    let (reached, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {described}"
    );
    let function = outputs_of(&described)
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| panic!("setup: the stack must publish the function it created"));
    let configuration = wait_until_active(&router, "setup", &function).await;
    let function_arn = member("GetFunctionConfiguration", &configuration, "FunctionArn");

    // The grant that admits an invocation from API Gateway, without which the gateway makes
    // none.
    let granted = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": "apigw-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com"
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup: AddPermission must answer 201, got {} with {:?}",
        granted.status,
        granted.text()
    );

    // 3. The api: one AWS_PROXY integration on that function, both lifecycle routes bound to it,
    //    and a deployment released to every stage a row addresses.
    let api = control_plane(
        &router,
        "CreateApi",
        "/v2/apis",
        json!({
            "name": "connection-teardown",
            "protocolType": "WEBSOCKET",
            "routeSelectionExpression": SELECTION_EXPRESSION
        }),
        201,
    )
    .await;
    let api_id = member("CreateApi", &api, "apiId");
    let announced = member("CreateApi", &api, "apiEndpoint");
    let integration = control_plane(
        &router,
        "CreateIntegration",
        &format!("/v2/apis/{api_id}/integrations"),
        json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": function_arn,
            "integrationMethod": "POST"
        }),
        201,
    )
    .await;
    let integration_id = member("CreateIntegration", &integration, "integrationId");
    for route_key in [CONNECT_ROUTE, DISCONNECT_ROUTE] {
        control_plane(
            &router,
            "CreateRoute",
            &format!("/v2/apis/{api_id}/routes"),
            json!({
                "routeKey": route_key,
                "target": format!("integrations/{integration_id}")
            }),
            201,
        )
        .await;
    }
    for row in ROWS {
        control_plane(
            &router,
            "CreateStage",
            &format!("/v2/apis/{api_id}/stages"),
            json!({ "stageName": row.stage }),
            201,
        )
        .await;
        control_plane(
            &router,
            "CreateDeployment",
            &format!("/v2/apis/{api_id}/deployments"),
            json!({ "stageName": row.stage }),
            201,
        )
        .await;
    }

    for row in ROWS {
        let case = row.case;

        // 4. The handshake, at the address the api announced. The client's socket is ENDED by
        //    the writer that made it, as a client whose process goes away ends one: what the
        //    rows assert is what the api does with a connection whose client is gone.
        let head = answered_handshake_at(
            case,
            &announced,
            &format!("/{}?agent={}", row.stage, row.agent),
            row.key,
        )
        .await;
        assert_eq!(
            head.status, SWITCHING_PROTOCOLS,
            "case {case:?}: expected the handshake to be answered {SWITCHING_PROTOCOLS}, got the \
             head {:?}",
            head.line
        );
        let accept = head.field(ACCEPT_FIELD).unwrap_or_default().to_string();
        assert!(
            !accept.is_empty(),
            "case {case:?}: a completed handshake carries the accept value the client's key \
             derives (RFC 6455 §4.2.2), got the fields {:?}",
            head.fields
        );
        match row.published_accept {
            // The one pair RFC 6455 §1.3 publishes: this key owes exactly this value.
            Some(published) => assert_eq!(
                accept, published,
                "case {case:?}: expected the accept value RFC 6455 §1.3 publishes for the key \
                 {:?}, got {accept:?}",
                row.key
            ),
            // A key the specification publishes no answer for still cannot be answered the
            // published one: the value is derived from the key the client offered.
            None => assert_ne!(
                accept, RFC_EXAMPLE_ACCEPT,
                "case {case:?}: the accept value is derived from the key the client offered \
                 ({:?}), so it cannot be the one another key owes, got {accept:?}",
                row.key
            ),
        }

        // 5. The `$connect` leg, which the handshake was answered behind: the row that
        //    integration wrote names the connection it was admitting and the stage that
        //    admitted it, and the record of its run says the same.
        let connected = row_for_route(&router, case, row.agent, CONNECT_ROUTE).await;
        let connection_id = string_member(&connected, "connectionId").unwrap_or_default();
        assert!(
            !connection_id.is_empty(),
            "case {case:?}: expected the $connect integration to record the id of the connection \
             it was admitting, got {connected}"
        );
        assert_eq!(
            string_member(&connected, "stage").as_deref(),
            Some(row.stage),
            "case {case:?}: expected the recorded row to name the stage that admitted the \
             connection, got {connected}"
        );
        logged(
            &router,
            case,
            &function,
            &format!("{CONNECT_ROUTE} {connection_id} on {}", row.stage),
        )
        .await;

        // 6. The END of that connection: the `$disconnect` route's integration is performed for
        //    it — one call, naming this connection and this stage — and its own record is in the
        //    group the function logs to.
        let disconnected = row_for_route(&router, case, &connection_id, DISCONNECT_ROUTE).await;
        assert_eq!(
            (
                string_member(&disconnected, "connectionId").as_deref(),
                string_member(&disconnected, "stage").as_deref()
            ),
            (Some(connection_id.as_str()), Some(row.stage)),
            "case {case:?}: expected the $disconnect integration to be performed for the very \
             connection that ended, got {disconnected}"
        );
        logged(
            &router,
            case,
            &function,
            &format!("{DISCONNECT_ROUTE} {connection_id} on {}", row.stage),
        )
        .await;

        // 7. And the id then addresses nothing: the definite refusal a backend prunes its own
        //    record on, rather than a push that looks delivered or a call left pending.
        let looks = LEG_DEADLINE.as_millis() / LOOK_INTERVAL.as_millis();
        let mut refused = post_to_connection(&router, &api_id, row.stage, &connection_id).await;
        for _ in 0..looks {
            if refused.status == GONE {
                break;
            }
            tokio::time::sleep(LOOK_INTERVAL).await;
            refused = post_to_connection(&router, &api_id, row.stage, &connection_id).await;
        }
        assert_eq!(
            refused.status,
            GONE,
            "case {case:?}: expected HTTP {GONE} for the id of a connection that has ended, got \
             {} with {:?}",
            refused.status,
            refused.text()
        );
        assert!(
            refused.error_type.contains(GONE_CODE),
            "case {case:?}: expected the refusal to name the {GONE_CODE} class in \
             {ERROR_TYPE_HEADER}, got {:?} with {:?}",
            refused.error_type,
            refused.text()
        );
    }
}
