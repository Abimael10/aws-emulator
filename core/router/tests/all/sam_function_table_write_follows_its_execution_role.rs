// SPDX-License-Identifier: CC0-1.0

//! What a SAM-deployed function's own table write is judged against, and what a client of the
//! stage in front of it is served when that write is refused: CloudFormation, IAM, the token
//! service, Lambda, Amazon DynamoDB and API Gateway on ONE engine, one SAM template deployed
//! once per row, and the POST a client makes to the invoke URL the stack published.
//!
//! The rule pinned here: **a function runs AS its execution role — the runtime hands the
//! handler temporary credentials of that role, so the service serving a call the handler makes
//! decides it against the identity policies in force on that role, refusing with its own
//! `AccessDeniedException` every call no statement in force allows and doing none of its work.
//! The SDK raises that refusal, an unhandled raise makes the invocation a FUNCTION ERROR
//! (`X-Amz-Function-Error: Unhandled`), and an `AWS_PROXY` integration whose function errored
//! returned no proxy response, which API Gateway answers `502` for.**
//!
//! That is one rule over six services that have to agree, which is why the suite lives here:
//! the SAM transform synthesizes the function's execution role and whatever policies the
//! template declares for it, IAM holds them, the token service vends the role's session,
//! Lambda RUNS the handler with the AWS SDK its runtime supplies, DynamoDB takes the decision,
//! and API Gateway turns the outcome into what the client reads. Every observation is made
//! where a client makes one — `CreateStack`, `DescribeStacks`, `Invoke`, `GetItem` and the
//! stage's own invoke URL — never an emulator internal.
//!
//! The rows differ along exactly what the rule generalises over: WHAT the template's function
//! declares for its role (no `Policies` at all, so the synthesized role carries only the
//! managed policy every execution role carries; the SAM policy template that grants the
//! table's writes; the one that grants only its READS, on that very table; and a document
//! granting another action of the same service on every resource). One template, so a row's
//! served or refused branch differs in the
//! declared policy and in nothing else; a refused write is reached through three different
//! documents, so no verdict rests on one spelling. Both halves of every row are asserted — what the stage
//! served its client, what the direct `Invoke` reported, and what the TABLE holds afterwards,
//! read where a client reads it — so a refusal that still wrote the item, or a grant that did
//! not, fails here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region the stack's resources belong
/// to.
const REGION: &str = "us-east-1";

/// The terminal `StackStatus` a create that succeeded settles to, and every status a create
/// can settle to at all.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const TERMINAL_STATUSES: [&str; 4] = [
    CREATE_COMPLETE,
    "CREATE_FAILED",
    "ROLLBACK_COMPLETE",
    "ROLLBACK_FAILED",
];

/// The logical ids the template declares, and the api the SAM transform synthesizes for an
/// `Api` event that names no `RestApiId` ("the implicit API").
const FUNCTION_ID: &str = "Writer";
const TABLE_ID: &str = "Items";
const IMPLICIT_API: &str = "ServerlessRestApi";

/// The path the api's one method is on, and the stage the implicit api is deployed to.
const RESOURCE_PATH: &str = "items";
const STAGE: &str = "Prod";

/// The output keys the template publishes: what a client deploying it reads the function and
/// the table it addresses off, and the URL it posts to.
const INVOKE_URL_OUTPUT: &str = "InvokeUrl";
const FUNCTION_NAME_OUTPUT: &str = "FunctionName";
const TABLE_NAME_OUTPUT: &str = "TableName";

/// How long one invocation of this handler is given, as the function's own `Timeout`: far past
/// what a run needs, because what is under test is the authorization decision and never how
/// fast a loaded host gets an interpreter through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long between two polls, and how many polls to make. Every loop below exits the instant
/// its CONDITION holds, so the product is a DEADLINE and not a wait: generous room past what a
/// stack create or a resource transition needs even on a host running every other suite
/// beside this one, so a row's outcome never depends on how fast the host is.
const POLL: Duration = Duration::from_millis(100);
const POLLS: usize = 1_800;

/// The error code DynamoDB refuses a call the policies in force do not allow with: the
/// AWS-common `AccessDeniedException`.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The IAM action the handler's write performs ("Actions defined by Amazon DynamoDB"), which
/// the refusal of that write names.
const WRITE_ACTION: &str = "dynamodb:PutItem";

/// What `X-Amz-Function-Error` reports for an invocation whose handler raised.
const UNHANDLED: &str = "Unhandled";

/// The status API Gateway answers a proxy integration whose function returned no proxy
/// response with: "API Gateway returns a 502 Bad Gateway error response".
const BAD_GATEWAY: u16 = 502;

/// The handler the template deploys: it writes the posted item and answers the proxy envelope,
/// catching nothing — so a refused write raises out of the handler, which is what makes the
/// invocation a function error.
const HANDLER_SOURCE: &str = r#"
import json
import os

import boto3

CLIENT = boto3.client("dynamodb")


def handler(event, context):
    posted = json.loads(event.get("body") or "{}")
    CLIENT.put_item(
        TableName=os.environ["TABLE"],
        Item={"id": {"S": posted["id"]}, "data": {"S": posted["data"]}},
    )
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"stored": posted["id"]}),
    }
"#;

/// One answered request: its status, its headers and its body verbatim.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// The emulator as it is ASSEMBLED, on one engine (`aws_emulator_router::build_router`): the
/// handler's own SDK call is dispatched back into this same router, so the table it writes is
/// the table this stack deployed and the role it runs as is the record IAM holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a control-plane call carries a credential
    // scope so the router can name the target service, and the POST to the published invoke
    // URL carries none, as a client of a stage sends none.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here, and the key it names is one no
/// store holds — the account's own published credential.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> Answered {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        "POST",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("cloudformation")),
        ],
        Bytes::from(body),
    )
    .await
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One Amazon DynamoDB call over its `awsJson1_0` wire, made with the account's own published
/// credential — where a client reads the table.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: Value) -> Answered {
    dispatch(
        router,
        "POST",
        &format!("dynamodb.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("authorization", &authorization("dynamodb")),
            ("x-amz-target", &format!("DynamoDB_20120810.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// POST `body` to the invoke URL the stack published, exactly as a client reaches a deployed
/// stage: the host is the api's own `{rest-api-id}.execute-api.{region}.amazonaws.com` endpoint
/// and the path is the stage and the resource path. The URL is taken apart rather than rebuilt,
/// so what is driven is the value the stack told the caller to use.
async fn post_to_stage(router: &MultiServiceRouter, invoke_url: &str, body: &str) -> Answered {
    let without_scheme = invoke_url
        .strip_prefix("https://")
        .or_else(|| invoke_url.strip_prefix("http://"))
        .unwrap_or(invoke_url);
    let (host, path) = match without_scheme.split_once('/') {
        Some((host, path)) => (host.to_string(), format!("/{path}")),
        None => (without_scheme.to_string(), "/".to_string()),
    };
    dispatch(
        router,
        "POST",
        &host,
        &path,
        &[("content-type", "application/json")],
        Bytes::from(body.to_owned()),
    )
    .await
}

/// Poll `DescribeStacks` until the stack reaches a terminal status, and answer it beside the
/// body it was read from.
async fn wait_for_stack(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    for _ in 0..POLLS {
        let described = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        let body = described.text();
        let status = opt_element(&body, "StackStatus").unwrap_or_default();
        if TERMINAL_STATUSES.contains(&status.as_str()) {
            return (status, body);
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("setup: the stack {stack:?} never reached a terminal status");
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`. The span a
/// function spends `Pending` is that service's own, so no fixed sleep names the instant it
/// ends.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, name: &str) {
    for _ in 0..POLLS {
        let reported = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        if reported.json().get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("setup {case:?}: Lambda never reported {name} Active");
}

/// Poll `DescribeTable` until DynamoDB first reports the table `ACTIVE` — "you can perform
/// read and write operations only on an ACTIVE table".
async fn wait_until_table_active(router: &MultiServiceRouter, case: &str, table: &str) {
    for _ in 0..POLLS {
        let described = dynamodb(router, "DescribeTable", json!({ "TableName": table })).await;
        if described
            .json()
            .get("Table")
            .and_then(|table| table.get("TableStatus"))
            .and_then(Value::as_str)
            == Some("ACTIVE")
        {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("setup {case:?}: DynamoDB never reported {table} ACTIVE");
}

/// The SAM template one row deploys: the handler above behind the implicit api's `POST /items`,
/// the table it writes, and whatever `Policies` the row declares on the function.
fn template(policies: Option<Value>) -> String {
    let mut properties = json!({
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Timeout": FUNCTION_TIMEOUT,
        "InlineCode": HANDLER_SOURCE,
        "Environment": { "Variables": { "TABLE": { "Ref": TABLE_ID } } },
        "Events": {
            "Post": {
                "Type": "Api",
                "Properties": { "Path": format!("/{RESOURCE_PATH}"), "Method": "post" },
            },
        },
    });
    if let Some(policies) = policies {
        properties
            .as_object_mut()
            .expect("the function's properties are an object")
            .insert("Policies".to_string(), policies);
    }
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": {
            FUNCTION_ID: {
                "Type": "AWS::Serverless::Function",
                "Properties": properties,
            },
            TABLE_ID: {
                "Type": "AWS::Serverless::SimpleTable",
                "Properties": { "PrimaryKey": { "Name": "id", "Type": "String" } },
            },
        },
        "Outputs": {
            INVOKE_URL_OUTPUT: {
                "Value": {
                    "Fn::Sub": format!(
                        "https://${{{IMPLICIT_API}}}.execute-api.${{AWS::Region}}\
                         .amazonaws.com/{STAGE}/{RESOURCE_PATH}"
                    ),
                },
            },
            FUNCTION_NAME_OUTPUT: { "Value": { "Ref": FUNCTION_ID } },
            TABLE_NAME_OUTPUT: { "Value": { "Ref": TABLE_ID } },
        },
    })
    .to_string()
}

/// One row: what the template declares for the function's role, and what that means for the
/// write its handler makes.
struct Row {
    case: &'static str,
    /// The `Policies` the function declares, or `None` for a declaration that states none —
    /// which leaves the synthesized role carrying only the managed policy every execution role
    /// carries.
    policies: fn() -> Option<Value>,
    /// Whether the policies in force allow the write the handler makes.
    allowed: bool,
}

/// A declaration with no `Policies` member at all.
fn no_policies() -> Option<Value> {
    None
}

/// The SAM policy template that grants the writes (and reads) of one table: "DynamoDBCrudPolicy
/// … Gives create, read, update, delete permissions to a DynamoDB Table".
fn crud_policy() -> Option<Value> {
    Some(json!([{ "DynamoDBCrudPolicy": { "TableName": { "Ref": TABLE_ID } } }]))
}

/// The SAM policy template that grants only the READS of one table: "DynamoDBReadPolicy …
/// Gives read-only permission to a DynamoDB Table" — of the table the handler writes, so the
/// grant is about that very resource and withholds the write all the same.
fn read_policy() -> Option<Value> {
    Some(json!([{ "DynamoDBReadPolicy": { "TableName": { "Ref": TABLE_ID } } }]))
}

/// A document granting an action of the SAME service the handler never calls, on every
/// resource, so nothing in force covers its write: a grant is about the action it names and no
/// other.
fn other_action_policy() -> Option<Value> {
    Some(json!([{
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            // "Actions defined by Amazon DynamoDB": the account's table listing, which is not
            // the write this handler makes.
            "Action": "dynamodb:ListTables",
            "Resource": "*",
        }],
    }]))
}

const ROWS: [Row; 4] = [
    Row {
        case: "no Policies at all: the role carries only the basic execution policy",
        policies: no_policies,
        allowed: false,
    },
    Row {
        case: "the SAM policy template that grants the table's writes",
        policies: crud_policy,
        allowed: true,
    },
    Row {
        case: "the SAM policy template that grants only the table's reads",
        policies: read_policy,
        allowed: false,
    },
    Row {
        case: "a grant of another action of the same service, on every resource",
        policies: other_action_policy,
        allowed: false,
    },
];

/// RULE: a function runs as its execution role, so a call its handler makes is decided against
/// the identity policies in force on that role — a write no statement in force allows is
/// refused with the serving service's own `AccessDeniedException` and writes nothing, the
/// unhandled raise makes the invocation a function error, and the proxy stage in front of the
/// function answers its client `502` rather than the handler's own result.
#[tokio::test]
async fn a_sam_functions_table_write_is_decided_by_the_role_the_template_gave_it() {
    // ONE emulator for every row: each row deploys a stack of its own, so the function and the
    // table a row addresses are still that row's alone.
    let router = emulator();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;

        // 1. The stack: the function, the api in front of it, the table it writes, and the
        // role the transform synthesizes carrying whatever the row declared.
        let stack = format!("sam-role-grant-{index}");
        let body = template((row.policies)());
        let created = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &body),
                // A template declaring a macro is acknowledged with CAPABILITY_AUTO_EXPAND,
                // and the role the transform synthesizes for the function with CAPABILITY_IAM.
                ("Capabilities.member.1", "CAPABILITY_IAM"),
                ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
            ],
        )
        .await;
        assert_eq!(
            created.status, 200,
            "setup {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
            created.status,
            created.text()
        );
        let (status, described) = wait_for_stack(&router, &stack).await;
        assert_eq!(
            status, CREATE_COMPLETE,
            "setup {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described}"
        );
        let outputs: BTreeMap<String, String> = outputs_of(&described);
        let read = |key: &str| {
            outputs
                .get(key)
                .cloned()
                .unwrap_or_else(|| panic!("setup {case:?}: the stack publishes no {key:?}"))
        };
        let invoke_url = read(INVOKE_URL_OUTPUT);
        let function_name = read(FUNCTION_NAME_OUTPUT);
        let table = read(TABLE_NAME_OUTPUT);
        wait_until_active(&router, case, &function_name).await;
        wait_until_table_active(&router, case, &table).await;

        // 2. The client's POST to the stage the stack published.
        let staged_id = format!("staged-{index}");
        let posted = json!({ "id": staged_id, "data": "written through the stage" });
        let served = post_to_stage(&router, &invoke_url, &posted.to_string()).await;
        if row.allowed {
            assert_eq!(
                served.status, 200,
                "case {case:?}: expected the stage to serve the handler's own answer, got {} \
                 with {:?}",
                served.status,
                served.text()
            );
            assert_eq!(
                served.json().get("stored").and_then(Value::as_str),
                Some(staged_id.as_str()),
                "case {case:?}: expected the handler's own result for {staged_id:?}, got {:?}",
                served.text()
            );
        } else {
            assert_eq!(
                served.status, BAD_GATEWAY,
                "case {case:?}: expected the stage to answer {BAD_GATEWAY} for a function that \
                 errored, got {} with {:?}",
                served.status,
                served.text()
            );
        }

        // 3. The direct `Invoke` of the same function, which is where the function error and
        // the SDK's own view of the refusal are read.
        let invoked_id = format!("invoked-{index}");
        let event = json!({
            "body": json!({ "id": invoked_id, "data": "written through Invoke" }).to_string(),
        });
        let invoked = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{function_name}/invocations"),
            Some(event),
        )
        .await;
        assert_eq!(
            invoked.status, 200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} with {:?}",
            invoked.status,
            invoked.text()
        );
        let signal = invoked.header("x-amz-function-error");
        if row.allowed {
            assert_eq!(
                signal, None,
                "case {case:?}: expected the handler to run to completion, got the function \
                 error {signal:?} with {:?}",
                invoked.text()
            );
        } else {
            assert_eq!(
                signal.as_deref(),
                Some(UNHANDLED),
                "case {case:?}: expected the refused write to raise out of the handler, got \
                 {signal:?} with {:?}",
                invoked.text()
            );
            let reported = invoked.json();
            let message = reported
                .get("errorMessage")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert!(
                message.contains(ACCESS_DENIED),
                "case {case:?}: expected the raised error to be the service's own refusal, got \
                 {reported}"
            );
            assert!(
                message.contains(WRITE_ACTION),
                "case {case:?}: expected the refusal to name {WRITE_ACTION}, got {reported}"
            );
        }

        // 4. What the table holds, read where a client reads it: both items for a row the
        // policies allow, and neither for a row they do not.
        for key in [&staged_id, &invoked_id] {
            let read = dynamodb(
                &router,
                "GetItem",
                json!({
                    "TableName": table,
                    "Key": { "id": { "S": key } },
                    "ConsistentRead": true,
                }),
            )
            .await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected HTTP 200 from GetItem, got {} with {:?}",
                read.status,
                read.text()
            );
            let held = read.json().get("Item").cloned().unwrap_or(Value::Null);
            assert_eq!(
                !held.is_null(),
                row.allowed,
                "case {case:?}: expected the table to hold an item under {key:?} to be \
                 {}, got {held}",
                row.allowed
            );
        }
    }
}
