// SPDX-License-Identifier: CC0-1.0

//! What a handler can IMPORT on a managed Python runtime, and what the call it makes with it
//! reaches: Lambda runs the deployment package, EventBridge answers the publication the running
//! handler makes, and IAM and the token service hold the role that publication is made as.
//!
//! RULE: a managed `python3.x` runtime supplies the AWS SDK on the handler's import path, so a
//! package whose module does `import boto3` is imported and run rather than failing to import,
//! and a call it makes with that SDK is a real call: the service it names decides it and
//! answers it, so the handler sees that service's own answer — the published event's id when
//! the policies in force on the function's execution role allow the publication, and that
//! service's own access refusal when they do not.
//!
//! Only a cross-service suite can pin it: the SDK call leaves the runtime this crate assembles
//! and is served by another service's crate on the same engine, so a Lambda-only suite could
//! see no answer to hold the handler's result against.
//!
//! The rows of the first test span what the rule generalises over: WHICH managed Python runtime
//! the function was created with. The identifiers are the `python3.x` members of the model's
//! `Runtime` enum (`smithy_query(query="shape",shape="Runtime")` on the lambda model) that the
//! runtime-deprecation catalog does not name, each row asserting the run reports the runtime it
//! was created with — so a runtime that silently ran as another, or one with no SDK on its
//! import path, fails here. The second test is the other half of the same rule: with the grant
//! withheld, the handler's own call carries EventBridge's refusal rather than an answer the
//! runtime made up.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_lambda_awsgirapiservice::ops::types::RUNTIME_VALUES;
use service_com_amazonaws_lambda_awsgirapiservice::state::is_deprecated_runtime;

use crate::query_wire::{element, form_encode};

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// An access key id neither IAM nor the token service holds a record for: it resolves to the
/// account's own identity, which no identity-based document bounds, so every call this suite
/// makes on its own behalf presents it.
const ACCOUNT_KEY: &str = "EMULATORSETUPKEY0002";

/// Where Lambda binds the function collection and one invocation.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The handler module CloudFormation archives inline source under.
const HANDLER: &str = "index.handler";

/// The managed Python runtimes a function may be created with: the `python3.x` members of the
/// model's own `Runtime` enum (the service crate's `ops::types::RUNTIME_VALUES`, rendered from
/// that enum) that the service's runtime-deprecation catalog does not name
/// (`state::is_deprecated_runtime`) — every one of them bundles the SDK. Taken from those two
/// constants rather than written out here, so a runtime the model gains, or one the catalog
/// comes to name, changes this row set with it.
fn python_runtimes() -> Vec<&'static str> {
    RUNTIME_VALUES
        .iter()
        .copied()
        .filter(|runtime| runtime.starts_with("python3.") && !is_deprecated_runtime(runtime))
        .collect()
}

/// The bus the publications go to, and the `source` and `detail-type` they carry.
const BUS: &str = "default";
const SOURCE: &str = "com.demo.runtime-sdk-probe";
const DETAIL_TYPE: &str = "Runtime SDK Probe";

/// The action a publication onto a bus performs, as a statement names it and as a refusal
/// reports it.
const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// The error code EventBridge refuses a publication no document in force allows with.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The `State` a function serves invocations in, and the status its stack settles at.
const FUNCTION_ACTIVE: &str = "Active";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long this suite waits for work the clock owes before giving up, and how often it looks.
const WAIT: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The timeout every function below is created with. Nothing here tests a function's own
/// timeout, so it is given generous room: a run shares this process with every other test of
/// this target, and a handler that imports the SDK on a loaded host must not be stopped by a
/// limit the suite is not about.
const FUNCTION_TIMEOUT: u64 = 120;

/// The handler every function below runs: it imports the SDK AT MODULE SCOPE — which is where
/// a runtime without it fails to import at all — builds a client from it, and publishes one
/// event, reporting what the publication answered or the refusal it raised.
const PROBE_HANDLER: &str = r#"
import json
import os

import boto3
import botocore
from botocore.exceptions import ClientError

EVENTS = boto3.client("events")


def handler(event, context):
    report = {
        "modules": [boto3.__name__, botocore.__name__],
        "execution_env": os.environ.get("AWS_EXECUTION_ENV", ""),
    }
    try:
        answered = EVENTS.put_events(
            Entries=[
                {
                    "EventBusName": event["bus"],
                    "Source": event["source"],
                    "DetailType": event["detail_type"],
                    "Detail": json.dumps({"runtime": event["runtime"]}),
                }
            ]
        )
    except ClientError as refused:
        report["published"] = False
        report["error_code"] = refused.response["Error"]["Code"]
        report["error_message"] = refused.response["Error"]["Message"]
        return report
    report["published"] = answered["FailedEntryCount"] == 0
    report["event_ids"] = [entry["EventId"] for entry in answered["Entries"]]
    return report
"#;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the bus it publishes to is the one this suite's services hold.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite signs nothing; the credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// What one call answered.
struct Answer {
    status: u16,
    function_error: Option<String>,
    body: Vec<u8>,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or_else(|_| Value::String(self.text()))
        }
    }
}

/// One Lambda `restJson1` call, made with the account's own credential.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = body.map_or_else(Vec::new, |document| document.to_string().into_bytes());
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        // "X-Amz-Function-Error — If present, indicates that an error occurred during function
        // execution" (`Invoke`'s own output): the signal a run that never imported its module
        // comes back with.
        function_error: parts
            .headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: answered.to_vec(),
    }
}

/// One awsQuery call against `signing_name` that must answer 200; answers its XML body.
async fn query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from {signing_name}, got {status} with {text}"
    );
    text
}

/// How many looks fit in [`WAIT`] at [`LOOK`] apart.
fn looks() -> u32 {
    u32::try_from(WAIT.as_millis() / LOOK.as_millis().max(1)).unwrap_or(u32::MAX)
}

/// Create the execution role `name`, trusted by Lambda — which `CreateFunction` checks — and
/// carrying `statements` as its inline document when any are given; answers its ARN.
async fn given_execution_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    statements: Option<Value>,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = query(
        router,
        case,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    if let Some(statements) = statements {
        let document = json!({"Version": "2012-10-17", "Statement": statements}).to_string();
        query(
            router,
            case,
            "iam",
            "2010-05-08",
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "in-force"),
                ("PolicyDocument", &document),
            ],
        )
        .await;
    }
    element(&created, "Arn")
}

/// Deploy one function per entry of `runtimes`, each running [`PROBE_HANDLER`] on that runtime
/// as `role_arn`, and wait until Lambda reports every one of them active.
///
/// The handler is declared as a stack's inline source, which CloudFormation archives into the
/// deployment package — the way a function whose handler is written out in a suite comes to
/// exist at all.
async fn given_functions(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    role_arn: &str,
    functions: &[(String, &str)],
) {
    let mut resources = serde_json::Map::new();
    for (index, (name, runtime)) in functions.iter().enumerate() {
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": name,
                    "Runtime": runtime,
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            }),
        );
    }
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string();
    query(
        router,
        case,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    // The stack's completion and each function's provisioning window are owed by the clock,
    // which no fixed sleep names: this looks until they hold.
    let mut last = String::new();
    for _ in 0..looks() {
        last = query(
            router,
            case,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", stack)],
        )
        .await;
        if element(&last, "StackStatus") == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        element(&last, "StackStatus"),
        CREATE_COMPLETE,
        "case {case:?}: the stack {stack:?} must reach {CREATE_COMPLETE}, last saw {last}"
    );
    for (name, runtime) in functions {
        let mut configuration = Value::Null;
        let mut active = false;
        for _ in 0..looks() {
            configuration = lambda(
                router,
                "GET",
                &format!("{FUNCTIONS_URI}/{name}/configuration"),
                None,
            )
            .await
            .json();
            if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
                active = true;
                break;
            }
            tokio::time::sleep(LOOK).await;
        }
        assert!(
            active,
            "case {case:?}: the {runtime} function must reach {FUNCTION_ACTIVE}, last saw \
             {configuration}"
        );
    }
}

/// Invoke `name` synchronously, handing the handler the bus and the runtime it was created
/// with.
async fn invoke(router: &MultiServiceRouter, name: &str, runtime: &str) -> Answer {
    lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        Some(&json!({
            "bus": BUS,
            "source": SOURCE,
            "detail_type": DETAIL_TYPE,
            "runtime": runtime,
        })),
    )
    .await
}

/// Assert that the run IMPORTED its module and returned, rather than coming back as a function
/// error.
fn assert_ran(case: &str, answer: &Answer) -> Value {
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} with {}",
        answer.status,
        answer.text()
    );
    assert_eq!(
        answer.function_error, None,
        "case {case:?}: expected no function error, got {:?} with {}",
        answer.function_error,
        answer.text()
    );
    let report = answer.json();
    assert_eq!(
        report["modules"],
        json!(["boto3", "botocore"]),
        "case {case:?}: expected the SDK's own modules to have been imported, got {report}"
    );
    report
}

/// Every managed Python runtime supplies the SDK, and the publication a handler makes with it
/// is served by EventBridge under a role whose policies allow it.
#[tokio::test]
async fn every_managed_python_runtime_supplies_the_aws_sdk() {
    let router = build_router();
    let setup = "the granted execution role";
    let role = given_execution_role(
        &router,
        setup,
        "xsvc-runtime-sdk-publisher",
        Some(json!([{
            "Effect": "Allow",
            "Action": PUT_EVENTS_ACTION,
            "Resource": "*",
        }])),
    )
    .await;
    let runtimes = python_runtimes();
    assert!(
        !runtimes.is_empty(),
        "the model's Runtime enum must name at least one supported python3.x runtime"
    );
    let functions: Vec<(String, &str)> = runtimes
        .iter()
        .map(|runtime| {
            (
                format!("xsvc-runtime-sdk-{}", runtime.replace('.', "-")),
                *runtime,
            )
        })
        .collect();
    given_functions(
        &router,
        setup,
        "xsvc-runtime-sdk-stack",
        &role,
        &functions,
    )
    .await;

    for (name, runtime) in &functions {
        let case = *runtime;
        let answer = invoke(&router, name, runtime).await;
        let report = assert_ran(case, &answer);
        assert_eq!(
            report["execution_env"],
            json!(format!("AWS_Lambda_{runtime}")),
            "case {case:?}: expected the run to report the runtime it was created with, got \
             {report}"
        );
        assert_eq!(
            report["published"],
            json!(true),
            "case {case:?}: expected the publication to be served, got {report}"
        );
        let ids = report["event_ids"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            ids.len(),
            1,
            "case {case:?}: expected one published event id, got {report}"
        );
        assert!(
            ids[0].as_str().is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected EventBridge's own event id, got {report}"
        );
    }
}

/// The same SDK call under a role granted nothing: the handler is handed EventBridge's OWN
/// refusal, naming the session the run was made as and the action it asked for — so the call
/// really left the runtime and was decided by the service it named.
#[tokio::test]
async fn the_handlers_own_sdk_call_is_decided_by_the_service_it_names() {
    let router = build_router();
    let case = "a role granted no publication";
    let role = given_execution_role(&router, case, "xsvc-runtime-sdk-ungranted", None).await;
    let runtime = *python_runtimes()
        .last()
        .expect("the model's Runtime enum must name at least one supported python3.x runtime");
    let name = "xsvc-runtime-sdk-refused".to_string();
    given_functions(
        &router,
        case,
        "xsvc-runtime-sdk-refused-stack",
        &role,
        &[(name.clone(), runtime)],
    )
    .await;

    let answer = invoke(&router, &name, runtime).await;
    let report = assert_ran(case, &answer);
    assert_eq!(
        report["published"],
        json!(false),
        "case {case:?}: expected the publication to be refused, got {report}"
    );
    assert_eq!(
        report["error_code"],
        json!(ACCESS_DENIED),
        "case {case:?}: expected EventBridge's own refusal code, got {report}"
    );
    let message = report["error_message"].as_str().unwrap_or_default();
    assert!(
        message.contains(PUT_EVENTS_ACTION),
        "case {case:?}: expected the refusal to name the action, got {report}"
    );
    assert!(
        message.contains(&format!(":assumed-role/xsvc-runtime-sdk-ungranted/{name}")),
        "case {case:?}: expected the refusal to blame the run's own session, got {report}"
    );
}
