// SPDX-License-Identifier: CC0-1.0

//! The FORM of the X-Ray trace context a Lambda invoked behind a tracing REST stage reads out
//! of its own execution environment, driven end to end through the whole assembled router.
//!
//! Four services and one engine, so this is a router suite: IAM holds the execution role,
//! CloudFormation archives the inline handler into the deployment package Lambda runs, API
//! Gateway resolves the deployed stage and stamps the trace context on the invocation it makes,
//! and Lambda reports that context to the handler in its environment.
//!
//! The RULE the rows assert, one level above the recorded case:
//!
//! > A function invoked through a REST stage whose `Stage$tracingEnabled` is set ALWAYS runs
//! > with a trace context, reported to the handler as one tracing header written in X-Ray's
//! > three fields in the order X-Ray writes them and no others —
//! > `Root=<trace id>;Parent=<span id>;Sampled=<0|1>`, where a trace id is `1-<8 hex>-<24 hex>`
//! > and a span id is 16 hexadecimal digits. The `Root` is the chain the caller named when it
//! > named one of that form, carried byte for byte, and one the stage started otherwise; the
//! > `Parent` is a span the stage minted for its own call out, never the caller's own; and the
//! > `Sampled` decision is the caller's where it took one and the tracing stage's own where it
//! > did not. Every variable part is minted per request, so two identical requests run in two
//! > contexts.
//!
//! The rows span what that rule generalises over: WHAT the caller supplied on
//! `X-Amzn-Trace-Id` — nothing at all, a root alone, a root with each decision, a root naming
//! the caller's own parent span, a root beside a field X-Ray's three do not name, a root at the
//! edge of the unique part's range, a root that is not a trace id, and a header naming no root
//! at all.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason the sibling `apigateway_proxy_event_caller_identity` suite does.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::apigateway_lambda_ddb_s3_trace_tree::{is_hex, is_trace_id};
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, form_encode};

/// Keep this at or above the size the sibling router suites use.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is signed for, and the Region the integration ARN names.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to, with `tracingEnabled` turned on.
const STAGE: &str = "traced";

/// The resource path the api's one method is bound at.
const RESOURCE_PATH: &str = "probe";

/// The function the method proxies to, and the stack that declares it.
const FUNCTION: &str = "apigw-trace-header-form";
const STACK: &str = "apigw-trace-header-form-stack";

/// The execution role the function is declared with. The handler makes no call of its own, so
/// the role carries the trust policy alone.
const ROLE_NAME: &str = "trace-header-form-lambda-exec";

/// How long one invocation is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// The `StackStatus` the setup waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long to wait between polls of a clock-owed transition, and how many polls to make.
/// Generous: a loaded host must not decide the outcome.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 600;

/// The API version each awsQuery call carries.
const IAM_VERSION: &str = "2010-05-08";
const CLOUDFORMATION_VERSION: &str = "2010-05-15";

/// An access key id every setup call signs with. The router runs with authentication off, so
/// the credential scope is only what routes the request.
const KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The request header a caller's tracing header arrives on, as X-Ray spells it.
const TRACE_HEADER: &str = "x-amzn-trace-id";

/// The three fields a stamped tracing header carries, in the order X-Ray writes them.
const ROOT_FIELD: &str = "Root";
const PARENT_FIELD: &str = "Parent";
const SAMPLED_FIELD: &str = "Sampled";

/// The span id a row supplies as its own `Parent`, which the stage must never pass on as the
/// parent of the backend's work: 16 hexadecimal digits, as a segment id is.
const CALLER_SPAN: &str = "53995c3f42cd8ad8";

/// The handler the stack deploys: it reports the trace context its own environment carries,
/// verbatim and unparsed, so what is asserted is the header the invocation was given — and an
/// environment carrying none reports JSON `null` rather than an empty string.
const HANDLER_SOURCE: &str = r#"
import json
import os


def handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"trace": os.environ.get("_X_AMZN_TRACE_ID")}),
    }
"#;

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The body as JSON, failing with the body itself where it is not JSON at all — so a
    /// malformed answer is reported as one rather than read as a member the body does not
    /// carry.
    fn json(&self, case: &str) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected a JSON body, got {:?} ({error})",
                self.text()
            )
        })
    }
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

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={KEY_ID}/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let credential = authorization("apigateway");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &credential),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answered.status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json(case)
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let credential = authorization("lambda");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &credential),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One awsQuery call to `signing_name`, which must answer 200; answers its XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let credential = authorization(signing_name);
    let mut form = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        form.push('&');
        form.push_str(&form_encode(name));
        form.push('=');
        form.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        "POST",
        host,
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &credential),
        ],
        Bytes::from(form),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup {signing_name} {action}: expected HTTP 200, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.text()
}

/// One IAM call over its awsQuery wire.
async fn iam(router: &MultiServiceRouter, action: &str, parameters: &[(&str, &str)]) -> String {
    query(
        router,
        "iam",
        "iam.amazonaws.com",
        IAM_VERSION,
        action,
        parameters,
    )
    .await
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        CLOUDFORMATION_VERSION,
        action,
        parameters,
    )
    .await
}

/// A required string member of a JSON object.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// Whether `text` is a SPAN id: "a 64-bit identifier for the segment, unique among segments in
/// the same trace, in 16 hexadecimal digits".
///
/// The widths, and the trace-id grammar beside it, are the sibling
/// [`crate::apigateway_lambda_ddb_s3_trace_tree`] suite's one reading, shared rather than
/// copied.
fn is_span_id(text: &str) -> bool {
    is_hex(text, 16)
}

/// Deploy the stack that declares the function: the inline source is archived into the
/// deployment package by CloudFormation, which is how a handler written here comes to be one
/// Lambda can run. Answers the ARN Lambda reports for it, once it reports it `Active`.
///
/// The function declares no `TracingConfig`, so it runs in the default pass-through mode: it
/// records nothing of its own and reports the context it was INVOKED with, which is what makes
/// the header the handler reads the one the stage stamped.
async fn deployed_function(router: &MultiServiceRouter, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Reporter": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
    })
    .to_string();
    cloudformation(
        router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let (reached, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    if reached != CREATE_COMPLETE {
        let events = cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
        panic!(
            "setup CreateStack: expected the stack to reach {CREATE_COMPLETE}, got {described} \
             with the events {events}"
        );
    }
    // The invoke the integration makes needs the function RUNNABLE, which is a transition its
    // own service owes: look until it reports it rather than sleeping a span on its behalf.
    let mut configuration = Value::Null;
    for _ in 0..POLLS {
        configuration = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await
        .json("GetFunctionConfiguration");
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        configuration.get("State").and_then(Value::as_str),
        Some("Active"),
        "setup GetFunctionConfiguration: expected Lambda to report {FUNCTION:?} Active, got \
         {configuration}"
    );
    member("GetFunctionConfiguration", &configuration, "FunctionArn")
}

/// The api every row's request reaches: one `GET` method behind an `AWS_PROXY` integration onto
/// `function_arn`, deployed to [`STAGE`] with `tracingEnabled` turned on. Answers the api's id.
async fn deployed_api(router: &MultiServiceRouter, function_arn: &str) -> String {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "trace-header-form-api" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    let resource = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE_PATH })),
        201,
    )
    .await;
    let resource_id = member("CreateResource", &resource, "id");
    let method = format!("/restapis/{api}/resources/{resource_id}/methods/GET");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {function_arn}/invocations"
            ),
        })),
        201,
    )
    .await;
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    let patched = apigw(
        router,
        "UpdateStage",
        "PATCH",
        &format!("/restapis/{api}/stages/{STAGE}"),
        Some(json!({
            "patchOperations": [
                { "op": "replace", "path": "/tracingEnabled", "value": "true" }
            ]
        })),
        200,
    )
    .await;
    assert_eq!(
        patched.get("tracingEnabled"),
        Some(&json!(true)),
        "setup UpdateStage: expected the stage to report tracingEnabled, got {patched}"
    );
    api
}

/// One invoke of the deployed stage: what the caller supplied on [`TRACE_HEADER`], and what the
/// context the handler reads has to say.
struct Row {
    case: &'static str,
    /// The tracing header the caller sends, or `None` for a caller that sends none.
    sent: Option<&'static str>,
    /// The chain the handler must see, byte for byte, or `None` where the stage starts one of
    /// its own.
    continues: Option<&'static str>,
    /// The sampling decision the handler must see.
    decision: &'static str,
}

/// The chains the rows supply. Each row that supplies one supplies its own, so a continued
/// chain is told from a chain the stage started.
const ROWS: &[Row] = &[
    Row {
        case: "no header at all",
        sent: None,
        continues: None,
        decision: "1",
    },
    Row {
        case: "a root alone, with no decision beside it",
        sent: Some("Root=1-6a1b2c3d-000000000000000000000001"),
        continues: Some("1-6a1b2c3d-000000000000000000000001"),
        decision: "1",
    },
    Row {
        case: "a root the caller is recording",
        sent: Some("Root=1-6a1b2c3d-000000000000000000000002;Sampled=1"),
        continues: Some("1-6a1b2c3d-000000000000000000000002"),
        decision: "1",
    },
    Row {
        case: "a root the caller is not recording",
        sent: Some("Root=1-6a1b2c3d-000000000000000000000003;Sampled=0"),
        continues: Some("1-6a1b2c3d-000000000000000000000003"),
        decision: "0",
    },
    Row {
        case: "a root naming the caller's own parent span",
        sent: Some(
            "Root=1-6a1b2c3d-000000000000000000000004;Parent=53995c3f42cd8ad8;Sampled=1",
        ),
        continues: Some("1-6a1b2c3d-000000000000000000000004"),
        decision: "1",
    },
    Row {
        case: "a root beside a field X-Ray's three do not name",
        sent: Some("Root=1-6a1b2c3d-000000000000000000000005;Self=1;Sampled=0"),
        continues: Some("1-6a1b2c3d-000000000000000000000005"),
        decision: "0",
    },
    Row {
        case: "a root whose unique part is all zeroes",
        sent: Some("Root=1-6a1b2c3d-000000000000000000000000;Sampled=1"),
        continues: Some("1-6a1b2c3d-000000000000000000000000"),
        decision: "1",
    },
    Row {
        case: "a root that is not a trace id",
        sent: Some("Root=not-a-trace-id;Sampled=0"),
        continues: None,
        decision: "0",
    },
    Row {
        case: "a header naming no root at all",
        sent: Some("Sampled=0"),
        continues: None,
        decision: "0",
    },
];

/// The context the handler read for one request to the deployed stage, as the handler reported
/// it: `None` where the invocation's environment carried none at all.
async fn observed_context(
    router: &MultiServiceRouter,
    case: &str,
    invoke_host: &str,
    sent: Option<&str>,
) -> Option<String> {
    let credential = authorization("execute-api");
    let mut headers: Vec<(&str, &str)> = vec![("authorization", &credential)];
    if let Some(sent) = sent {
        headers.push((TRACE_HEADER, sent));
    }
    let answered = dispatch(
        router,
        "GET",
        invoke_host,
        &format!("/{STAGE}/{RESOURCE_PATH}"),
        &headers,
        Bytes::new(),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected the handler's 200 from the stage, got {} with {:?}",
        answered.status,
        answered.text()
    );
    let body = answered.json(case);
    // `trace` is JSON `null` exactly when the invocation's environment named no context, which
    // is the difference this suite exists to pin.
    match body.get("trace") {
        Some(Value::String(header)) => Some(header.clone()),
        Some(Value::Null) | None => None,
        Some(other) => panic!("case {case:?}: expected the context as text, got {other}"),
    }
}

/// The three fields of `header`, asserting it is written exactly as X-Ray writes a stamped
/// tracing header: `Root`, `Parent` and `Sampled`, in that order, and nothing else.
fn stamped_fields(case: &str, header: &str) -> (String, String, String) {
    let fields: Vec<(&str, &str)> = header
        .split(';')
        .map(|field| field.split_once('=').unwrap_or((field, "")))
        .collect();
    let names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        vec![ROOT_FIELD, PARENT_FIELD, SAMPLED_FIELD],
        "case {case:?}: expected the three fields {ROOT_FIELD}, {PARENT_FIELD} and \
         {SAMPLED_FIELD} in that order, got {header:?}"
    );
    let root = fields[0].1.to_string();
    let parent = fields[1].1.to_string();
    let decision = fields[2].1.to_string();
    // Rebuilt from the parts, which pins the punctuation as well as the names: a header with a
    // field repeated, or a trailing separator, does not survive this.
    assert_eq!(
        header,
        format!("{ROOT_FIELD}={root};{PARENT_FIELD}={parent};{SAMPLED_FIELD}={decision}"),
        "case {case:?}: expected a header written in X-Ray's three fields and no others, got \
         {header:?}"
    );
    (root, parent, decision)
}

#[test]
fn a_tracing_stages_invocation_reports_the_trace_context_in_x_rays_three_fields() {
    std::thread::Builder::new()
        .name("apigateway-stage-trace-header-form".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                // The IO and TIME drivers a service's background work needs.
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                // Signature verification belongs to the emulator, not to this check.
                router.set_request_authentication(false);

                // 1. The function the method proxies to, the role Lambda runs it as, and the
                //    grant that admits an invocation from API Gateway.
                let role = iam(
                    &router,
                    "CreateRole",
                    &[
                        ("RoleName", ROLE_NAME),
                        (
                            "AssumeRolePolicyDocument",
                            &json!({
                                "Version": "2012-10-17",
                                "Statement": [{
                                    "Effect": "Allow",
                                    "Principal": { "Service": "lambda.amazonaws.com" },
                                    "Action": "sts:AssumeRole",
                                }],
                            })
                            .to_string(),
                        ),
                    ],
                )
                .await;
                let role_arn = element(&role, "Arn");
                let function_arn = deployed_function(&router, &role_arn).await;
                let granted = lambda(
                    &router,
                    "POST",
                    &format!("/2015-03-31/functions/{FUNCTION}/policy"),
                    Some(json!({
                        "StatementId": "apigw-invoke",
                        "Action": "lambda:InvokeFunction",
                        "Principal": "apigateway.amazonaws.com",
                    })),
                )
                .await;
                assert_eq!(
                    granted.status,
                    201,
                    "setup AddPermission: expected HTTP 201 from Lambda, got {} with {:?}",
                    granted.status,
                    granted.text()
                );

                // 2. The api, deployed to a stage that traces.
                let api = deployed_api(&router, &function_arn).await;
                let invoke_host = format!("{api}.execute-api.{REGION}.amazonaws.com");

                // 3. One invoke per row, and the context the handler was handed.
                let mut spans: BTreeSet<String> = BTreeSet::new();
                let mut started: BTreeSet<String> = BTreeSet::new();
                for row in ROWS {
                    let case = row.case;
                    let header =
                        observed_context(&router, case, &invoke_host, row.sent)
                            .await
                            .unwrap_or_else(|| {
                                panic!(
                                    "case {case:?}: expected the invocation's environment to \
                                     name a trace context, got none"
                                )
                            });
                    let (root, parent, decision) = stamped_fields(case, &header);

                    // The CHAIN: a trace id, the caller's own byte for byte where it named one
                    // of that form, and one the stage started where it did not.
                    assert!(
                        is_trace_id(&root),
                        "case {case:?}: expected a trace id as the {ROOT_FIELD}, got {header:?}"
                    );
                    match row.continues {
                        Some(continued) => assert_eq!(
                            root, continued,
                            "case {case:?}: expected the caller's chain {continued:?} continued, \
                             got {header:?}"
                        ),
                        None => {
                            assert!(
                                ROWS.iter().all(|other| other.continues != Some(root.as_str())),
                                "case {case:?}: expected a chain the stage started, got \
                                 {header:?}"
                            );
                            assert!(
                                started.insert(root.clone()),
                                "case {case:?}: expected a chain of this request's own, got the \
                                 {root:?} another request already ran in"
                            );
                        }
                    }

                    // The PARENT: a span id the stage minted for its own call out, never the
                    // caller's own, and one per call.
                    assert!(
                        is_span_id(&parent),
                        "case {case:?}: expected a span id as the {PARENT_FIELD}, got {header:?}"
                    );
                    assert_ne!(
                        parent, CALLER_SPAN,
                        "case {case:?}: expected the stage's own span rather than the caller's, \
                         got {header:?}"
                    );
                    assert!(
                        spans.insert(parent.clone()),
                        "case {case:?}: expected a span of this call's own, got the {parent:?} \
                         another call already named"
                    );

                    // The DECISION: the caller's where it took one, the tracing stage's own
                    // where it did not.
                    assert_eq!(
                        decision, row.decision,
                        "case {case:?}: expected the decision {:?} to reach the handler, got \
                         {header:?}",
                        row.decision
                    );
                }

                // Every variable part is minted PER REQUEST: the same request sent twice runs in
                // two contexts, so nothing here is a value fixed once.
                let first = ROWS[0].case;
                let once = observed_context(&router, first, &invoke_host, ROWS[0].sent)
                    .await
                    .unwrap_or_else(|| panic!("case {first:?}: expected a trace context, got none"));
                let twice = observed_context(&router, first, &invoke_host, ROWS[0].sent)
                    .await
                    .unwrap_or_else(|| panic!("case {first:?}: expected a trace context, got none"));
                assert_ne!(
                    once, twice,
                    "case {first:?}: expected two requests to run in two contexts, got {once:?} \
                     both times"
                );
                for repeated in [&once, &twice] {
                    let (_root, parent, _decision) = stamped_fields(first, repeated);
                    assert!(
                        spans.insert(parent.clone()),
                        "case {first:?}: expected a span of this call's own, got the {parent:?} \
                         another call already named"
                    );
                }
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
