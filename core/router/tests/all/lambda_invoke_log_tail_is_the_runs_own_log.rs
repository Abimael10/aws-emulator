// SPDX-License-Identifier: CC0-1.0

//! What `Invoke` hands a client that asked for the log of a run the handler FAILED: the
//! execution log the runtime produced, the error record included, and not an envelope around
//! it.
//!
//! RULE: `InvocationResponse.LogResult` is "the last 4 KB of the execution log, which is
//! base64-encoded", and that execution log is the stream the runtime wrote for this invocation —
//! the `START RequestId: <id> Version: <version>` frame Lambda opens it with, then EVERY record
//! the handler wrote to its own standard streams, then, for a run the handler failed, the
//! runtime's own error record naming the class and the sentence of the failure, then `END
//! RequestId: <id>`, then the `REPORT RequestId: <id>` frame stating `Duration`, `Billed
//! Duration`, `Memory Size` and `Max Memory Used`. The record and field labels are the fixed
//! text; the invocation's id, the durations, the memory figures and the handler's own text are
//! values of this run and of this function — so `Memory Size` is the memory the function was
//! created with and `Max Memory Used` is a whole number of megabytes between one and it.
//!
//! The log is the only place a client can read why a handler failed, so a tail with the error
//! record and the handler's own output missing makes every fault undiagnosable from the API, and
//! a client parsing the report for what the run held finds no such field.
//!
//! The rows span what the rule generalises over: WHICH managed runtime produced the log — the
//! frames and the report are Lambda's rather than one runtime's, while the error record is each
//! runtime's own. What the sibling suites of the lambda crate already pin for a run that
//! RETURNED is not driven a second time here; what only a cross-service suite reaches is this
//! tail on the ASSEMBLED emulator, where the functions are declared in a CloudFormation stack
//! that archives their inline source into the deployment package Lambda runs — so both services
//! have to agree before there is a log to read at all.

use std::time::{Duration, Instant};
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE reading of a deployed stack's status, the ONE assembled router and the ONE
// execution-role setup this test crate owns, rather than second copies of them.
use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::lambda_invoke_grant_lifetime::{build_router, given_execution_role};
use crate::query_wire::form_encode;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// An access key id neither IAM nor the token service holds a record for: it resolves to the
/// account's own identity, so every call this suite makes on its own behalf presents it.
const ACCOUNT_KEY: &str = "EMULATORSETUPKEY0002";

/// Where Lambda binds the function collection and one invocation.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The stack the functions are declared in.
const STACK: &str = "xsvc-invoke-log-tail";

/// The `State` a function serves invocations in.
const FUNCTION_ACTIVE: &str = "Active";

/// The request header `Invoke$LogType` is bound to, and the value that asks for the tail.
const LOG_TYPE_HEADER: &str = "x-amz-log-type";
/// See [`LOG_TYPE_HEADER`].
const LOG_TYPE_TAIL: &str = "Tail";

/// The response headers the tail, the function-error signal and the invocation's own id are
/// bound to.
const LOG_RESULT_HEADER: &str = "x-amz-log-result";
/// See [`LOG_RESULT_HEADER`].
const FUNCTION_ERROR_HEADER: &str = "x-amz-function-error";
/// See [`LOG_RESULT_HEADER`].
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The signal a run the FUNCTION failed carries: "If present, indicates that an error occurred
/// during function execution" (`Invoke`'s own output).
const UNHANDLED: &str = "Unhandled";

/// The version an unqualified invocation executes, which the opening frame names.
const LATEST: &str = "$LATEST";

/// The fixed text of the frames Lambda writes around one run, and of each field its closing
/// report states, in the order the report states them. Source: the plain-text execution log a
/// managed runtime writes for an invocation.
const START_PREFIX: &str = "START RequestId: ";
/// See [`START_PREFIX`].
const END_PREFIX: &str = "END RequestId: ";
/// See [`START_PREFIX`].
const REPORT_PREFIX: &str = "REPORT RequestId: ";
/// See [`START_PREFIX`]: each field the report states, as its label and the unit it is stated
/// in. Each is named so an assertion about one field names that field rather than a position in
/// the table, which a field the report gains would re-point.
const DURATION_FIELD: (&str, &str) = ("Duration: ", "ms");
/// See [`DURATION_FIELD`].
const BILLED_DURATION_FIELD: (&str, &str) = ("Billed Duration: ", "ms");
/// See [`DURATION_FIELD`].
const MEMORY_SIZE_FIELD: (&str, &str) = ("Memory Size: ", "MB");
/// See [`DURATION_FIELD`].
const MAX_MEMORY_USED_FIELD: (&str, &str) = ("Max Memory Used: ", "MB");
/// The fields in the ORDER the report states them.
const REPORT_FIELDS: [(&str, &str); 4] = [
    DURATION_FIELD,
    BILLED_DURATION_FIELD,
    MEMORY_SIZE_FIELD,
    MAX_MEMORY_USED_FIELD,
];

/// The level an error record states it was written at.
const ERROR_LEVEL: &str = "ERROR";

/// The memory every function below is created with, in MB: the largest heap one run is given
/// at all, so a handler loaded on a host running every other test of this target at the same
/// time is not ended by a limit this suite is not about.
const MEMORY_SIZE: i64 = 512;

/// Each function's own `Timeout`, in seconds. Nothing here tests a timeout, so it is given
/// generous room: a run shares this process with every other test of this target.
const FUNCTION_TIMEOUT: i64 = 120;

/// How long this suite waits for work the clock owes before the wait itself is the failure, and
/// how long it leaves between two looks. Every wait looks again for the CONDITION.
const WAIT: Duration = Duration::from_secs(180);
const LOOK: Duration = Duration::from_millis(50);

/// The line each handler writes before it fails, and the sentence it fails with.
const MARKER: &str = "the-line-the-handler-wrote";
const RAISED_MESSAGE: &str = "the handler could not finish";

/// The node handler: it writes one line of its own, then raises.
const NODE_HANDLER: &str = r#"
exports.handler = async (event) => {
    console.log(event.marker);
    throw new Error(event.message);
};
"#;

/// The python handler: the same failure in the other managed runtime.
const PYTHON_HANDLER: &str = r#"
def lambda_handler(event, context):
    print(event["marker"])
    raise ValueError(event["message"])
"#;

/// One function this suite deploys: the runtime it runs on, where its handler lives, and what
/// that runtime's own error record names the failure's class in.
struct Runtime {
    /// The name of the function, which is also its logical id's subject.
    function: &'static str,
    runtime: &'static str,
    handler: &'static str,
    source: &'static str,
    /// The text that NAMES the class of the raised value in this runtime's own error record:
    /// the node runtime writes the record as a JSON document whose `errorType` member carries
    /// the constructor's name, while the python runtime writes `[ERROR] <class>: <message>`.
    /// Each row carries its own, so the class assertion is load-bearing for both.
    error_class: &'static str,
}

/// The runtimes driven: the two managed families this emulator embeds an engine for.
const RUNTIMES: &[Runtime] = &[
    Runtime {
        function: "xsvc-log-tail-node",
        runtime: "nodejs22.x",
        handler: "index.handler",
        source: NODE_HANDLER,
        error_class: "\"errorType\":\"Error\"",
    },
    Runtime {
        function: "xsvc-log-tail-python",
        runtime: "python3.12",
        handler: "index.lambda_handler",
        source: PYTHON_HANDLER,
        error_class: "ValueError: ",
    },
];

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
    headers: http::HeaderMap,
    body: Bytes,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// One Lambda `restJson1` call, with the headers `headers` names.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
    headers: &[(&str, &str)],
) -> Answer {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = body.map_or_else(Vec::new, |document| document.to_string().into_bytes());
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
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

/// Deploy one function per entry of [`RUNTIMES`] as `role_arn`, and wait until the stack is
/// complete and Lambda reports every one of them active.
///
/// The handlers are declared as a stack's inline source, which CloudFormation archives into the
/// deployment package — the way a function whose handler is written out in a suite comes to
/// exist at all.
async fn given_functions(router: &MultiServiceRouter, case: &str, role_arn: &str) {
    let mut resources = serde_json::Map::new();
    for (index, declared) in RUNTIMES.iter().enumerate() {
        resources.insert(
            format!("Logger{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": declared.function,
                    "Runtime": declared.runtime,
                    "Handler": declared.handler,
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "MemorySize": MEMORY_SIZE,
                    "Code": {"ZipFile": declared.source},
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
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    // What the invocations need is each function's own `State`, which the clock owes and no
    // fixed sleep names: the wait is on that CONDITION. The stack's own status is read only to
    // explain a function that never came up, so a slow deployment is not itself a failure.
    for declared in RUNTIMES {
        let deadline = Instant::now() + WAIT;
        loop {
            let answered = lambda(
                router,
                "GET",
                &format!("{FUNCTIONS_URI}/{}/configuration", declared.function),
                None,
                &[],
            )
            .await;
            let configuration: Value =
                serde_json::from_slice(&answered.body).unwrap_or(Value::Null);
            if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
                break;
            }
            if Instant::now() >= deadline {
                let last = answered.text();
                let (status, described) = stack_status(router, STACK).await;
                panic!(
                    "case {case:?}: the {} function must reach {FUNCTION_ACTIVE}; last saw \
                     {last}; the stack reports {status}: {described}",
                    declared.runtime
                );
            }
            tokio::time::sleep(LOOK).await;
        }
    }
}

/// The figure the report field `field` states in `report`, asserting it is stated in `unit`.
fn report_figure(case: &str, report: &str, field: &str, unit: &str) -> f64 {
    let (_, after) = report.split_once(field).unwrap_or_else(|| {
        panic!("case {case:?}: the report must state {field:?}, got {report:?}")
    });
    let mut words = after.split_whitespace();
    let figure = words
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: {field:?} must carry a figure, got {report:?}"));
    let stated = words
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: {field:?} must state its unit, got {report:?}"));
    assert_eq!(
        stated, unit,
        "case {case:?}: expected {field:?} stated in {unit}, got {stated:?} in {report:?}"
    );
    figure
        .parse::<f64>()
        .unwrap_or_else(|error| panic!("case {case:?}: {field:?} is {figure:?}, which {error}"))
}

/// The log tail of a run the handler failed is the run's OWN log: Lambda's frames, the handler's
/// own output, the runtime's error record, and a report stating every field it states.
#[tokio::test]
async fn the_invoke_log_tail_carries_the_runtimes_error_record_and_the_whole_report() {
    let router = build_router();
    let setup = "the functions whose failing runs are logged";
    let role = given_execution_role(&router, setup, "xsvc-log-tail-runner").await;
    given_functions(&router, setup, &role).await;

    for declared in RUNTIMES {
        let case = declared.runtime;
        let answered = lambda(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{}/invocations", declared.function),
            Some(&json!({ "marker": MARKER, "message": RAISED_MESSAGE })),
            &[(LOG_TYPE_HEADER, LOG_TYPE_TAIL)],
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: a run that happened is answered 200, got {} with {}",
            answered.status,
            answered.text()
        );
        let signal = answered.header(FUNCTION_ERROR_HEADER);
        assert_eq!(
            signal.as_deref(),
            Some(UNHANDLED),
            "case {case:?}: expected the signal of a run the function failed, got {signal:?} \
             with {}",
            answered.text()
        );
        let request_id = answered.header(REQUEST_ID_HEADER).unwrap_or_else(|| {
            panic!(
                "case {case:?}: the invocation must report its own id, got the headers {:?}",
                answered.headers
            )
        });
        let encoded = answered.header(LOG_RESULT_HEADER).unwrap_or_else(|| {
            panic!(
                "case {case:?}: LogType=Tail must answer the run's log, got the headers {:?}",
                answered.headers
            )
        });
        let tail = String::from_utf8(
            ::aws_smithy_types::base64::decode(&encoded).unwrap_or_else(|error| {
                panic!("case {case:?}: the tail must be base64, got {error}")
            }),
        )
        .unwrap_or_else(|error| panic!("case {case:?}: the tail must be text, got {error}"));

        // The frames Lambda writes around the run, each naming THIS invocation.
        let start = format!("{START_PREFIX}{request_id} Version: {LATEST}");
        assert!(
            tail.lines().any(|line| line == start),
            "case {case:?}: expected the opening frame {start:?}, got {tail:?}"
        );
        let end = format!("{END_PREFIX}{request_id}");
        assert!(
            tail.lines().any(|line| line == end),
            "case {case:?}: expected the closing frame {end:?}, got {tail:?}"
        );

        // The record the handler itself wrote, in between — the line it wrote before it failed.
        assert!(
            tail.lines().any(|line| line.ends_with(MARKER)),
            "case {case:?}: expected the handler's own line {MARKER:?} in the log, got {tail:?}"
        );

        // The runtime's own error record: the class and the sentence of the failure, on a
        // record stating it was written at the error level.
        let record = tail
            .lines()
            .find(|line| line.contains(declared.error_class) && line.contains(RAISED_MESSAGE))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected an error record naming {:?} and {RAISED_MESSAGE:?}, \
                     got {tail:?}",
                    declared.error_class
                )
            });
        assert!(
            record.contains(ERROR_LEVEL),
            "case {case:?}: expected the error record to state its level, got {record:?}"
        );

        // The report: this invocation, then every field it states, in that order.
        let report = tail
            .lines()
            .find(|line| line.starts_with(REPORT_PREFIX))
            .unwrap_or_else(|| panic!("case {case:?}: expected a report frame, got {tail:?}"));
        assert!(
            report.starts_with(&format!("{REPORT_PREFIX}{request_id}")),
            "case {case:?}: expected the report to name this invocation, got {report:?}"
        );
        let mut at = 0;
        for (field, _) in REPORT_FIELDS {
            let found = report[at..].find(field).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected {field:?} after the fields before it, got {report:?}"
                )
            });
            at += found + field.len();
        }
        // What each field STATES, where the rule fixes it. The run took some time, and what it
        // is billed for is that time rounded up to the whole millisecond the field is stated in.
        let (field, unit) = DURATION_FIELD;
        let duration_ms = report_figure(case, report, field, unit);
        let (field, unit) = BILLED_DURATION_FIELD;
        let billed_ms = report_figure(case, report, field, unit);
        assert!(
            billed_ms >= duration_ms && billed_ms >= 1.0 && billed_ms.fract() == 0.0,
            "case {case:?}: expected a whole number of milliseconds billed, at least the \
             {duration_ms} ms measured, got {billed_ms}"
        );
        // `Memory Size` is the function's own configuration, and `Max Memory Used` is what the
        // run was measured holding: a run that executed code held at least the unit the field
        // is stated in, and was never allowed to hold more than the function's own memory.
        let (field, unit) = MEMORY_SIZE_FIELD;
        let memory_size = report_figure(case, report, field, unit);
        assert_eq!(
            memory_size, MEMORY_SIZE as f64,
            "case {case:?}: expected the memory the function was created with, got {memory_size}"
        );
        let (field, unit) = MAX_MEMORY_USED_FIELD;
        let used = report_figure(case, report, field, unit);
        assert!(
            (1.0..=memory_size).contains(&used) && used.fract() == 0.0,
            "case {case:?}: expected a whole number of megabytes between 1 and {memory_size}, \
             got {used}"
        );
    }
}
