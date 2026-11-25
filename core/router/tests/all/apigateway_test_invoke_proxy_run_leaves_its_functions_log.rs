// SPDX-License-Identifier: CC0-1.0

//! What a SIMULATED invocation of an `AWS_PROXY` method leaves behind in the function's own
//! Amazon CloudWatch Logs group — the half of the permission boundary a client reads on the
//! BACKEND rather than in the answer it was handed.
//!
//! The rule under test, one level above the recorded case:
//!
//! > `TestInvokeMethod` on a method whose integration is `AWS_PROXY` invokes the function the
//! > integration's `uri` names only where that FUNCTION's resource-based policy admits
//! > `apigateway.amazonaws.com` `lambda:InvokeFunction` for the simulated method's own
//! > `aws:SourceArn`. Where it does not, the integration is unperformable: the api answers
//! > through its configuration-error gateway response and the function does NOT run, so NOTHING
//! > of that call is written to the function's log group. Where it does, the function RUNS, and
//! > the run leaves its log in the group a read of the function reports — every line the handler
//! > printed, and the runtime's own `REPORT` record for the run — exactly as any other
//! > invocation of it does.
//!
//! The rows differ along what the rule generalises over: the `aws:SourceArn` of the one statement
//! the client adds (none at all, the reserved test-invoke stage with the concrete path, every
//! stage and every path of the api, and another verb of the same path) and how many lines the
//! handler prints for the run (one and three).
//!
//! Each denied row proves the absence of a run WITHOUT waiting out a span: after the refused
//! simulation the suite invokes the same function directly, through Lambda's own `Invoke`, and
//! waits for THAT run's line to appear in the group. An invocation the simulation had made would
//! have been written before the later one, so once the later line is readable the earlier line's
//! absence is conclusive.
//!
//! Four crates have to agree for that to hold (CloudFormation archives the inline handler into a
//! package this emulator can run and synthesizes the role it runs as, Lambda holds the function,
//! its policy and the run, API Gateway simulates the method, CloudWatch Logs holds the group), so
//! the suite lives here rather than in any one of them.
//!
//! What the neighbouring suites pin and this one does not repeat:
//! [`crate::apigateway_test_invoke_stage_scoped_lambda_permission`] pins the ENVELOPE the
//! simulation answers for each grant form, and [`crate::apigateway_proxy_lambda_ddb_pipeline`]
//! pins what an invocation a DEPLOYED stage made leaves in the group. What only this suite
//! reaches is the simulated caller's run — and the absence of one — read off the function's own
//! log group.

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
const STACK: &str = "apigw-test-invoke-run-log";

/// The resource the method is put on, and the verb it is put under.
const RESOURCE_PATH: &str = "echo";
const VERB: &str = "GET";

/// The stage the api is deployed to, so that the method under test is a deployed one too — a
/// simulation is of the LIVE method either way.
const STAGE: &str = "test";

/// The stage segment a test invocation's method ARN carries, which is the `aws:SourceArn` a
/// `lambda:InvokeFunction` grant has to cover for a test invoke to be admitted: a test invocation
/// reaches no deployed stage, and API Gateway names that fact in the ARN rather than leaving it
/// out.
const TEST_INVOKE_STAGE: &str = "test-invoke-stage";

/// The header the simulation carries the row's marker in, and the one it carries the number of
/// lines the handler prints in.
const MARKER_HEADER: &str = "X-Marker";
const LINES_HEADER: &str = "X-Lines";

/// The body API Gateway answers a request whose integration it could not perform, and the reason
/// its execution log names for a function no grant admits an invocation of.
const INTERNAL_SERVER_ERROR: &str = "Internal server error";
const INVALID_PERMISSIONS: &str = "Invalid permissions on Lambda function";

/// The sentence the execution log of an invocation that WAS made carries.
const COMPLETED: &str = "Successfully completed execution";

/// The fixed framing of the record the Lambda runtime writes for every run it finishes.
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// How long one invocation of this handler is given, as the function's own `Timeout`: well past
/// what a run needs on any host, because what is under test is the run's LOG and not how fast an
/// interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long to wait between looks at a clock-owed transition, and how many looks to take. The
/// product is generous by orders of magnitude over what any step here needs, so a loaded host
/// cannot decide an outcome; nothing is asserted about how long anything took.
const LOOK: Duration = Duration::from_millis(50);
const LOOKS: usize = 1200;

/// The handler behind the method: it prints the marker it was handed, once per line the caller
/// asked for, and answers a proxy envelope naming it. Nothing it needs comes from another
/// service, so what a row observes is the RUN and not a dependency of it.
const MARKING_HANDLER: &str = r#"
import json


def _header(event, wanted):
    for name, value in (event.get("headers") or {}).items():
        if name.lower() == wanted.lower():
            return value
    return None


def handler(event, context):
    marker = _header(event, "X-Marker") or "no-marker"
    lines = int(_header(event, "X-Lines") or 1)
    for index in range(lines):
        print("ran for %s line %d" % (marker, index))
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"marker": marker, "lines": lines}),
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

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator as it is ASSEMBLED, on one engine: the group the function's run writes to is the
/// one this suite reads.
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

/// One Amazon CloudWatch Logs call over its `awsJson1_1` wire.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> Answered {
    dispatch(
        router,
        "POST",
        &format!("logs.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("authorization", &authorization("logs")),
            ("x-amz-target", &format!("Logs_20140328.{operation}")),
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
        answered.status, expected,
        "setup {what:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// A required non-empty string member of a JSON object.
fn member(what: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("setup {what:?}: no non-empty {name:?} in {body}"))
        .to_string()
}

/// The template the suite deploys: the handler above, archived into a package this emulator can
/// run, under a role the transform synthesizes.
fn template() -> String {
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": {
            "Marker": {
                "Type": "AWS::Serverless::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "InlineCode": MARKING_HANDLER
                }
            }
        }
    })
    .to_string()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`, and answer
/// the configuration it reported then. The span is that service's own, so nothing here sleeps a
/// fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) -> Value {
    for _ in 0..LOOKS {
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
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda never reported {name} Active");
}

/// Every message CloudWatch Logs holds in `group` right now.
async fn messages_in(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let answered = logs(router, "FilterLogEvents", json!({ "logGroupName": group })).await;
    if answered.status != 200 {
        return Vec::new();
    }
    answered
        .json()
        .get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event.get("message").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Look at `group` until it holds a message containing `needle`, and answer every message it
/// held then. Waits for the CONDITION rather than for a span, and bounded so a line that never
/// arrives fails the case naming it.
async fn messages_once_holding(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    needle: &str,
) -> Vec<String> {
    let mut held = Vec::new();
    for _ in 0..LOOKS {
        held = messages_in(router, group).await;
        if held.iter().any(|message| message.contains(needle)) {
            return held;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the group {group:?} never came to hold {needle:?}, it holds {held:?}");
}

/// The `SourceArn` a statement is narrowed to: the methods of `api` under `stage`, for `verb` and
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
        granted.status, 201,
        "setup {what:?}: expected HTTP 201 from AddPermission({statement_id:?}), got {} with {:?}",
        granted.status,
        granted.text()
    );
}

/// Remove the statement `statement_id` from the function's policy again.
async fn revoke(router: &MultiServiceRouter, what: &str, function: &str, statement_id: &str) {
    let removed = lambda(
        router,
        "DELETE",
        &format!("/2015-03-31/functions/{function}/policy/{statement_id}"),
        None,
    )
    .await;
    assert_eq!(
        removed.status, 204,
        "setup {what:?}: expected HTTP 204 from RemovePermission({statement_id:?}), got {} with \
         {:?}",
        removed.status,
        removed.text()
    );
}

/// Simulate the method once, carrying `marker` and `lines` in the headers the handler reads, and
/// answer the envelope `TestInvokeMethod` modelled.
async fn simulate(
    router: &MultiServiceRouter,
    case: &str,
    api: &str,
    resource: &str,
    marker: &str,
    lines: usize,
) -> Value {
    let answered = apigw_call(
        router,
        "POST",
        &format!("/restapis/{api}/resources/{resource}/methods/{VERB}"),
        Some(json!({
            "pathWithQueryString": format!("/{RESOURCE_PATH}"),
            "headers": {
                MARKER_HEADER: marker,
                LINES_HEADER: lines.to_string(),
            },
        })),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected the modelled HTTP 200 of TestInvokeMethod, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// The reported `status` and `body` of a simulation, and its execution log.
fn reported(case: &str, envelope: &Value) -> (i64, Value, String) {
    let status = envelope
        .get("status")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: no numeric status in {envelope}"));
    let body = envelope
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no body in {envelope}"));
    let log = envelope
        .get("log")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no execution log in {envelope}"))
        .to_string();
    (
        status,
        serde_json::from_str::<Value>(body).unwrap_or(Value::Null),
        log,
    )
}

/// One row: the `aws:SourceArn` of the one statement the client adds, whether it admits the
/// SIMULATED invocation, and how many lines the handler prints for the run.
struct Row {
    case: &'static str,
    /// The stage, verb and path segments the added statement is narrowed to, or `None` for the
    /// row that adds no statement at all.
    source: Option<(&'static str, &'static str, &'static str)>,
    admits: bool,
    lines: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "no statement at all, so nothing admits the simulation",
        source: None,
        admits: false,
        lines: 1,
    },
    Row {
        case: "the reserved stage and the concrete path, printing three lines",
        source: Some((TEST_INVOKE_STAGE, VERB, RESOURCE_PATH)),
        admits: true,
        lines: 3,
    },
    Row {
        case: "every stage and every path of the api, printing one line",
        source: Some(("*", "*", "*")),
        admits: true,
        lines: 1,
    },
    Row {
        case: "another verb of the same path under the reserved stage",
        source: Some((TEST_INVOKE_STAGE, "POST", RESOURCE_PATH)),
        admits: false,
        lines: 1,
    },
];

#[tokio::test]
async fn a_simulated_proxy_invocation_logs_its_run_only_where_the_policy_admits_it() {
    let router = emulator();

    // 1. The function: the marking handler, archived and provisioned by the stack.
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
        created.status, 200,
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
    let function = physical_ids_of(&resources.text())
        .get("Marker")
        .cloned()
        .unwrap_or_else(|| panic!("setup: no physical id for Marker in {:?}", resources.text()));
    let configuration = wait_until_active(&router, &function).await;
    let function_arn = member("GetFunctionConfiguration", &configuration, "FunctionArn");
    // The group the function itself reports is the one every run of it has to write to.
    let group = configuration
        .get("LoggingConfig")
        .and_then(|logging| logging.get("LogGroup"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        group,
        format!("/aws/lambda/{function}"),
        "setup: expected the function's default log group, got {group:?}"
    );

    // 2. The api: GET /echo behind an AWS_PROXY integration on that function, deployed.
    let api_record = apigw(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({
            "name": "test-invoke-run-log-api",
            "endpointConfiguration": { "types": ["REGIONAL"] },
        })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &api_record, "id");
    let root = member("CreateRestApi", &api_record, "rootResourceId");
    let resource = member(
        "CreateResource",
        &apigw(
            &router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": RESOURCE_PATH })),
            201,
        )
        .await,
        "id",
    );
    let method = format!("/restapis/{api}/resources/{resource}/methods/{VERB}");
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

    // 3. The rows: one statement added, the method simulated, the group read, the statement
    //    removed again.
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

        let marker = format!("simulated-{index}");
        let envelope = simulate(&router, case, &api, &resource, &marker, row.lines).await;
        let (status, reported_body, log) = reported(case, &envelope);

        // The LATER run, which every row makes and which the function's own policy has no say
        // over: once its line is readable, a line the simulation's run would have written is
        // readable too, so a row that must observe NOTHING concludes on evidence rather than on
        // a span that has passed.
        let probe = format!("probe-{index}");
        let probed = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{function}/invocations"),
            Some(json!({ "headers": { MARKER_HEADER: probe, LINES_HEADER: "1" } })),
        )
        .await;
        assert_eq!(
            probed.status, 200,
            "case {case:?}: expected HTTP 200 from the direct Invoke that orders this reading, \
             got {} with {:?}",
            probed.status,
            probed.text()
        );
        let ordering_line = format!("ran for {probe} line 0");
        let held = messages_once_holding(&router, case, &group, &ordering_line).await;

        if row.admits {
            assert_eq!(
                (status, reported_body.clone()),
                (200, json!({ "marker": marker, "lines": row.lines })),
                "case {case:?}: expected the simulation to report the handler's own answer, got \
                 {status} with {reported_body:?} and log {log:?}"
            );
            assert!(
                log.contains(COMPLETED),
                "case {case:?}: expected the execution log to report the invocation \
                 ({COMPLETED:?}), got {log:?}"
            );
            for line in 0..row.lines {
                let printed = format!("ran for {marker} line {line}");
                assert!(
                    held.iter().any(|message| message.contains(&printed)),
                    "case {case:?}: expected the group {group:?} to hold the line the run \
                     printed ({printed:?}), it holds {held:?}"
                );
            }
            assert!(
                held.iter().any(|message| message.contains(REPORT_PREFIX)),
                "case {case:?}: expected the group {group:?} to hold a {REPORT_PREFIX:?} record \
                 for the run, it holds {held:?}"
            );
        } else {
            assert_eq!(
                (status, reported_body.clone()),
                (500, json!({ "message": INTERNAL_SERVER_ERROR })),
                "case {case:?}: expected the api's configuration-error answer for a function no \
                 statement admits this simulation of, got {status} with {reported_body:?} and \
                 log {log:?}"
            );
            assert!(
                log.contains(INVALID_PERMISSIONS),
                "case {case:?}: expected the execution log to name the configuration error \
                 {INVALID_PERMISSIONS:?}, got {log:?}"
            );
            let absent = format!("ran for {marker} ");
            assert!(
                !held.iter().any(|message| message.contains(&absent)),
                "case {case:?}: expected the group {group:?} to hold NOTHING of a run no \
                 statement admits ({absent:?}), it holds {held:?}"
            );
        }

        if row.source.is_some() {
            revoke(&router, case, &function, &statement).await;
        }
    }
}
