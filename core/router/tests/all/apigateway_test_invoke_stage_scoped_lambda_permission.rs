// SPDX-License-Identifier: CC0-1.0

//! What `TestInvokeMethod` answers for a method whose Lambda function admits API Gateway only
//! SOME of the api's stage segments — the permission-boundary smoke a client runs before it
//! deploys.
//!
//! The rule under test, one level above the recorded case:
//!
//! > A simulated invocation of a method is answered WITHIN THE ONE REQUEST with the envelope
//! > `TestInvokeMethod` models — `status`, `body`, `headers`, `multiValueHeaders`, `log` and
//! > `latency` — computed from the integration's own outcome, and never left pending on a
//! > backend. Whether that integration's function is invoked at all is decided by the FUNCTION's
//! > resource-based policy, judged on `lambda:InvokeFunction` with the invoked method's ARN under
//! > the stage segment AWS reserves for a test invocation as the `aws:SourceArn` such a grant is
//! > narrowed by. A policy that covers no statement for that segment leaves the integration
//! > unperformable, which the api answers through its own configuration-error gateway response
//! > (`500` with `{"message": "Internal server error"}`, the reason named in the execution log);
//! > one that covers it has the function RUN, and the simulation reports the handler's own
//! > response — even where the run itself calls another AWS service in process. The DEPLOYED
//! > caller the policy already admits is decided by its own stage segment and answers the same
//! > throughout.
//!
//! The rows differ along what the rule generalises over: the `SourceArn` of the one statement a
//! client adds beside the deployed stage's own grant — the reserved stage with the path
//! wildcarded and with the concrete path, every stage of the api, another stage name, another
//! verb and another path — plus the row that adds no statement at all.
//!
//! Four crates have to agree for that to hold (CloudFormation archives the inline handler into a
//! package this emulator can run and synthesizes the role it runs as, Lambda holds the function
//! and the policy that admits an invocation of it, DynamoDB serves the read the handler performs
//! in process, API Gateway simulates the method and resolves the deployed stage), so the suite
//! lives here rather than in any one of them.
//!
//! What the neighbouring suites pin and this one does not repeat:
//! `apigateway_iam_authorization_debug_lifecycle` pins the control-plane and IAM reads of an
//! `AWS_IAM` method and the authorization of its DEPLOYED caller, and
//! `apigateway_proxy_lambda_ddb_pipeline` pins what an invocation leaves in the function's log
//! group. What only this suite reaches is the SIMULATED caller's own permission boundary over a
//! handler that reads another service, and the envelope being answered in one request either
//! way.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, physical_ids_of};

/// The Region every request below is signed for and every ARN names.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The `StackStatus` the setup waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stack that archives the inline handler and synthesizes the role it runs as.
const STACK: &str = "xsvc-test-invoke-boundary";

/// The stage the api is deployed to, the collection resource, and the templated child under it
/// that carries the method.
const STAGE: &str = "beta";
const COLLECTION: &str = "data-agreements";
const CHILD: &str = "{agreementId}";

/// The key attribute of the table, the one agreement it holds, and that agreement's text.
const KEY_ATTRIBUTE: &str = "agreementId";
const AGREEMENT: &str = "agr-7712";
const AGREEMENT_TEXT: &str = "the agreement the handler answers with";

/// The verb the method is put on, and so the verb segment of every grant below.
const VERB: &str = "GET";

/// The stage segment a test invocation's method ARN carries, which is the `aws:SourceArn` a
/// `lambda:InvokeFunction` grant has to cover for the console's and the CLI's test invoke to be
/// admitted: a test invocation reaches no deployed stage, and API Gateway names that fact in the
/// ARN rather than leaving it out.
const TEST_INVOKE_STAGE: &str = "test-invoke-stage";

/// The body API Gateway answers a request whose integration it could not perform, and the reason
/// its execution log names for a function no grant admits an invocation of.
const INTERNAL_SERVER_ERROR: &str = "Internal server error";
const INVALID_PERMISSIONS: &str = "Invalid permissions on Lambda function";

/// The sentence the execution log of an invocation that was MADE carries.
const COMPLETED: &str = "Successfully completed execution";

/// The response header the simulated media type travels in.
const CONTENT_TYPE: &str = "Content-Type";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the permission boundary and not how fast an
/// interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 30;

/// The deadline one `TestInvokeMethod` call is given to come back with its envelope at all.
///
/// Generous by two orders of magnitude over what a simulated invocation needs — the suite is one
/// of thousands a run may be executing at once, so a loaded host cannot decide the outcome — and
/// bounded, so a call that answers NOTHING reports that as a failed assertion naming the case
/// rather than holding the suite open. No assertion here is on how long anything took.
const ANSWERED_WITHIN: Duration = Duration::from_secs(120);

/// How long to wait between polls of a clock-owed transition, and how many polls to make.
const POLL: Duration = Duration::from_millis(100);
const POLLS: usize = 600;

/// The handler behind the method: it reads the agreement the request's path names, with the SDK
/// the runtime supplies, and answers a proxy envelope carrying what the table held. The table is
/// named to it through its own environment.
const READER_HANDLER: &str = r#"
import json
import os

import boto3


def handler(event, context):
    asked = (event.get("pathParameters") or {}).get("agreementId")
    read = boto3.client("dynamodb").get_item(
        TableName=os.environ["TABLE"],
        Key={"agreementId": {"S": asked}},
        ConsistentRead=True,
    )
    item = read.get("Item")
    if item is None:
        return {
            "statusCode": 404,
            "headers": {"Content-Type": "application/json"},
            "body": json.dumps({"error": "no agreement under that id"}),
        }
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(
            {"agreementId": item["agreementId"]["S"], "text": item["text"]["S"]}
        ),
    }
"#;

/// The body the handler answers for the agreement the table holds, computed here rather than read
/// back out of the answer under test.
fn served_body() -> Value {
    json!({ "agreementId": AGREEMENT, "text": AGREEMENT_TEXT })
}

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
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

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched back
/// into this same router, so the table it reads is the one this suite seeded.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: a request carries a credential scope so the
    // router can name the target service, and the signature itself is never checked here.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request.
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
        .uri(format!("https://{host}{path}"))
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
        body,
    }
}

/// Call one CloudFormation operation over its awsQuery wire.
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

/// One Amazon DynamoDB call over its `awsJson1_0` wire.
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

/// One API Gateway control-plane call, answered as it came back.
async fn apigw_call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let answered = apigw_call(router, method, path, body).await;
    assert_eq!(
        answered.status,
        expected,
        "setup {what:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// A required string member of a JSON object.
fn member(what: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("setup {what:?}: no non-empty {name:?} in {body}"))
        .to_string()
}

/// The SAM template the suite deploys: the handler above, and the table it reads, named to the
/// function through the environment variable the transform carries through.
fn template() -> String {
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": {
            "Reader": {
                "Type": "AWS::Serverless::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "InlineCode": READER_HANDLER,
                    "Environment": { "Variables": { "TABLE": { "Ref": "Agreements" } } },
                    "Policies": [
                        { "DynamoDBCrudPolicy": { "TableName": { "Ref": "Agreements" } } }
                    ]
                }
            },
            "Agreements": {
                "Type": "AWS::Serverless::SimpleTable",
                "Properties": { "PrimaryKey": { "Name": KEY_ATTRIBUTE, "Type": "String" } }
            }
        }
    })
    .to_string()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`, and answer
/// the configuration it reported then. The span is that service's own, so nothing here sleeps a
/// fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) -> Value {
    for _ in 0..POLLS {
        let configuration = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await
        .json();
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            return configuration;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("setup: Lambda never reported {name} Active");
}

/// Poll `DescribeTable` until DynamoDB first reports the table `ACTIVE` — "you can perform read
/// and write operations only on an ACTIVE table".
async fn wait_until_table_active(router: &MultiServiceRouter, table: &str) {
    for _ in 0..POLLS {
        let described = dynamodb(router, "DescribeTable", json!({ "TableName": table })).await;
        if described.json()["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("setup: DynamoDB never reported {table} ACTIVE");
}

/// The `SourceArn` a statement is narrowed to: the methods of `api` under `stage`, for `VERB` and
/// `path`.
fn source_arn(api: &str, stage: &str, verb: &str, path: &str) -> String {
    format!("arn:aws:execute-api:{REGION}:{ACCOUNT}:{api}/{stage}/{verb}/{path}")
}

/// Add the statement `statement_id` admitting `apigateway.amazonaws.com`
/// `lambda:InvokeFunction` on the function, narrowed to `source`.
async fn grant(
    router: &MultiServiceRouter,
    what: &str,
    function: &str,
    statement_id: &str,
    source: &str,
) {
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": statement_id,
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": source,
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup {what:?}: expected HTTP 201 from AddPermission({statement_id:?}), got {} with {:?}",
        granted.status,
        granted.text()
    );
}

/// Remove the statement `statement_id` from the function's policy again, as a client taking the
/// authorization away does.
async fn revoke(router: &MultiServiceRouter, what: &str, function: &str, statement_id: &str) {
    let removed = lambda(
        router,
        "DELETE",
        &format!("/2015-03-31/functions/{function}/policy/{statement_id}"),
        None,
    )
    .await;
    assert_eq!(
        removed.status,
        204,
        "setup {what:?}: expected HTTP 204 from RemovePermission({statement_id:?}), got {} with \
         {:?}",
        removed.status,
        removed.text()
    );
}

/// What one `TestInvokeMethod` reported, as the operation models it.
struct Simulated {
    status: i64,
    body: Option<Value>,
    headers: Value,
    multi_value_headers: Value,
    log: String,
    latency: i64,
    envelope: Value,
}

/// Simulate the method once, and report the envelope it answered.
///
/// The call is bounded by [`ANSWERED_WITHIN`] so that a simulation which answers nothing at all
/// fails this case instead of holding the suite: what is asserted is that an envelope WAS
/// answered, never how long it took.
async fn simulate(
    router: &MultiServiceRouter,
    case: &str,
    api: &str,
    resource: &str,
) -> Simulated {
    let answered = tokio::time::timeout(
        ANSWERED_WITHIN,
        apigw_call(
            router,
            "POST",
            &format!("/restapis/{api}/resources/{resource}/methods/{VERB}"),
            Some(json!({
                "pathWithQueryString": format!("/{COLLECTION}/{AGREEMENT}"),
            })),
        ),
    )
    .await
    .unwrap_or_else(|_elapsed| {
        panic!(
            "case {case:?}: TestInvokeMethod must answer its envelope within the one request, \
             got nothing at all"
        )
    });
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected the modelled HTTP 200 of TestInvokeMethod, got {} with {:?}",
        answered.status,
        answered.text()
    );
    let envelope = answered.json();
    let status = envelope
        .get("status")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: no numeric status in {envelope}"));
    let body = envelope
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no body in {envelope}"))
        .to_string();
    let log = envelope
        .get("log")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no execution log in {envelope}"))
        .to_string();
    let latency = envelope
        .get("latency")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: no numeric latency in {envelope}"));
    Simulated {
        status,
        body: serde_json::from_str::<Value>(&body).ok(),
        headers: envelope.get("headers").cloned().unwrap_or(Value::Null),
        multi_value_headers: envelope
            .get("multiValueHeaders")
            .cloned()
            .unwrap_or(Value::Null),
        log,
        latency,
        envelope,
    }
}

/// Every member of the envelope the operation models, whatever the integration's outcome was: a
/// measured latency, the media type of the reported body, and the single-valued view agreeing
/// with the multi-valued one it is the first value of.
fn assert_envelope(case: &str, simulated: &Simulated) {
    assert!(
        simulated.latency >= 0,
        "case {case:?}: expected the measured latency of the simulation, got {}",
        simulated.latency
    );
    let headers = simulated
        .headers
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: no headers map in {}", simulated.envelope));
    assert!(
        headers.contains_key(CONTENT_TYPE),
        "case {case:?}: expected the reported headers to name the body's media type, got {:?}",
        simulated.headers
    );
    let multi = simulated
        .multi_value_headers
        .as_object()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: no multiValueHeaders map in {}",
                simulated.envelope
            )
        });
    for (name, value) in headers {
        assert_eq!(
            multi.get(name).and_then(Value::as_array).and_then(|values| values.first()),
            Some(value),
            "case {case:?}: expected the single-valued {name:?} to be the first value the \
             multi-valued view carries, got {:?}",
            multi.get(name)
        );
    }
}

/// What a client of the api's published invoke URL is served for the deployed method.
async fn invoke_deployed_stage(router: &MultiServiceRouter, api: &str) -> Answered {
    dispatch(
        router,
        VERB,
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}/{COLLECTION}/{AGREEMENT}"),
        &[("authorization", &authorization("execute-api"))],
        Bytes::new(),
    )
    .await
}

/// One row: the `SourceArn` of the one statement a client adds beside the deployed stage's own
/// grant, and whether it admits the SIMULATED invocation of the method.
struct Row {
    case: &'static str,
    /// The stage, verb and path segments the added statement is narrowed to, or `None` for the
    /// row that adds no statement at all.
    source: Option<(&'static str, &'static str, &'static str)>,
    /// Whether the function is then invoked, which decides the reported status and body.
    admits: bool,
}

/// The rows: the grant forms a client adds while chasing a refused test invoke, each covering —
/// or failing to cover — the method ARN a SIMULATED invocation is judged under.
const ROWS: &[Row] = &[
    Row {
        case: "no statement beyond the deployed stage's own grant",
        source: None,
        admits: false,
    },
    Row {
        case: "the reserved stage with the path wildcarded, as the console's fix adds it",
        source: Some((TEST_INVOKE_STAGE, VERB, "data-agreements/*")),
        admits: true,
    },
    Row {
        case: "the reserved stage and the concrete path the simulation names",
        source: Some((TEST_INVOKE_STAGE, VERB, "data-agreements/agr-7712")),
        admits: true,
    },
    Row {
        case: "every stage of the api for this verb and path",
        source: Some(("*", VERB, "data-agreements/*")),
        admits: true,
    },
    Row {
        case: "another stage name, which is not the segment a simulation is judged under",
        source: Some(("staging", VERB, "data-agreements/*")),
        admits: false,
    },
    Row {
        case: "another verb of the same path under the reserved stage",
        source: Some((TEST_INVOKE_STAGE, "POST", "data-agreements/*")),
        admits: false,
    },
    Row {
        case: "another path of the same verb under the reserved stage",
        source: Some((TEST_INVOKE_STAGE, VERB, "catalogue/*")),
        admits: false,
    },
];

#[tokio::test]
async fn a_test_invoke_is_answered_the_envelope_the_functions_policy_decides() {
    let router = emulator();

    // 1. The estate: the function, the table it reads, and the one agreement in it.
    let body = template();
    let created = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            // A template declaring a macro is acknowledged with CAPABILITY_AUTO_EXPAND, and the
            // role the transform synthesizes for the function with CAPABILITY_IAM.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup CreateStack: expected HTTP 200, got {} with {:?}",
        created.status,
        created.text()
    );
    let (reached, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    let resources =
        cloudformation(&router, "DescribeStackResources", &[("StackName", STACK)]).await;
    let physical = physical_ids_of(&resources.text());
    let function = physical
        .get("Reader")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no physical id for Reader in {:?}", resources.text()));
    let table = physical
        .get("Agreements")
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "setup: no physical id for Agreements in {:?}",
                resources.text()
            )
        });
    let configuration = wait_until_active(&router, &function).await;
    let function_arn = member("GetFunctionConfiguration", &configuration, "FunctionArn");
    wait_until_table_active(&router, &table).await;
    let seeded = dynamodb(
        &router,
        "PutItem",
        json!({
            "TableName": table,
            "Item": {
                KEY_ATTRIBUTE: { "S": AGREEMENT },
                "text": { "S": AGREEMENT_TEXT },
            },
        }),
    )
    .await;
    assert_eq!(
        seeded.status,
        200,
        "setup PutItem({AGREEMENT:?}): expected HTTP 200, got {} with {:?}",
        seeded.status,
        seeded.text()
    );

    // 2. The api: GET /<collection>/{agreementId} behind an AWS_PROXY integration on that
    //    function, deployed to the stage.
    let api_record = apigw(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({
            "name": "test-invoke-boundary-api",
            "endpointConfiguration": { "types": ["REGIONAL"] },
        })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &api_record, "id");
    let root = member("CreateRestApi", &api_record, "rootResourceId");
    let collection = member(
        "CreateResource",
        &apigw(
            &router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": COLLECTION })),
            201,
        )
        .await,
        "id",
    );
    let child = member(
        "CreateResource",
        &apigw(
            &router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{collection}"),
            Some(json!({ "pathPart": CHILD })),
            201,
        )
        .await,
        "id",
    );
    let method = format!("/restapis/{api}/resources/{child}/methods/{VERB}");
    apigw(
        &router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        &router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/{function_arn}/\
                 invocations"
            ),
        })),
        201,
    )
    .await;
    apigw(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    // 3. The grant the client wired for its deployed caller, and nothing else: that caller is
    //    served, and stays served whatever the rows below add and remove.
    grant(
        &router,
        "the deployed stage's own grant",
        &function,
        "apigw-deployed",
        &source_arn(&api, STAGE, VERB, &format!("{COLLECTION}/*")),
    )
    .await;
    let served = invoke_deployed_stage(&router, &api).await;
    assert_eq!(
        (served.status, served.json()),
        (200, served_body()),
        "the deployed caller its grant covers must be served the handler's own answer, got {} \
         with {:?}",
        served.status,
        served.text()
    );

    // 4. The rows: one statement added, the simulation answered, the statement removed again.
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let statement = format!("apigw-row-{index}");
        if let Some((stage, verb, path)) = row.source {
            grant(
                &router,
                case,
                &function,
                &statement,
                &source_arn(&api, stage, verb, path),
            )
            .await;
        }

        let simulated = simulate(&router, case, &api, &child).await;
        assert_envelope(case, &simulated);
        if row.admits {
            assert_eq!(
                (simulated.status, simulated.body.clone()),
                (200, Some(served_body())),
                "case {case:?}: expected the simulation to report the handler's own answer, got \
                 {} with {:?} and log {:?}",
                simulated.status,
                simulated.body,
                simulated.log
            );
            assert!(
                simulated.log.contains(COMPLETED),
                "case {case:?}: expected the execution log to report the invocation, got {:?}",
                simulated.log
            );
        } else {
            assert_eq!(
                (simulated.status, simulated.body.clone()),
                (500, Some(json!({ "message": INTERNAL_SERVER_ERROR }))),
                "case {case:?}: expected the api's configuration-error answer for a function no \
                 statement admits this simulation of, got {} with {:?} and log {:?}",
                simulated.status,
                simulated.body,
                simulated.log
            );
            assert!(
                simulated.log.contains(INVALID_PERMISSIONS),
                "case {case:?}: expected the execution log to name the configuration error \
                 {INVALID_PERMISSIONS:?}, got {:?}",
                simulated.log
            );
        }

        if row.source.is_some() {
            revoke(&router, case, &function, &statement).await;
        }
    }

    // 5. And the caller the policy admitted throughout answers exactly as it did before: the
    //    rows decided the SIMULATION and nothing else.
    let served_again = invoke_deployed_stage(&router, &api).await;
    assert_eq!(
        (served_again.status, served_again.json()),
        (served.status, served.json()),
        "the deployed caller must answer what it answered before the rows ({} with {:?}), got {} \
         with {:?}",
        served.status,
        served.text(),
        served_again.status,
        served_again.text()
    );
}
