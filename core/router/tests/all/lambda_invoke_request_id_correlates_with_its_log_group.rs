// SPDX-License-Identifier: CC0-1.0

//! The id an INVOCATION's answer names, used the way an operator uses it: as the handle that
//! finds that run's records in the function's own Amazon CloudWatch Logs group.
//!
//! RULE under test: **every answer AWS Lambda sends for an invocation names the request it
//! answered in the `x-amzn-RequestId` response header — which is what an SDK surfaces as
//! `ResponseMetadata.RequestId` — and for an invocation that id is the RUN's own id: the id
//! the run reports as its context request id, the id a failed handler's error document
//! carries, and the id the runtime writes into the `START RequestId:`, `END RequestId:` and
//! `REPORT RequestId:` records it delivers to the function's log group (the group its
//! `LoggingConfig$LogGroup` names, else the default `/aws/lambda/<function name>`). So a
//! client holding the id its own call answered with finds exactly that run's records by
//! searching the group for it, every invocation is named by an id of its own, and an id no
//! invocation was named by finds nothing.**
//!
//! Two services have to agree for that to hold — AWS Lambda names the invocation on its
//! answer and Amazon CloudWatch Logs holds the records written under the same id — and
//! CloudFormation provisions the role, the group and the function, so the suite lives here
//! rather than in any one crate. What the Lambda crate's own suites can reach is the answer
//! and its log tail; only the assembled emulator can show the id reaching the group a
//! separate service holds and being usable as the search term there, which is the whole
//! operator workflow ("invoke, take the request id, look for it in the log group").
//!
//! The rows differ along what the rule generalises over rather than restating one template:
//! the runtime the function runs, WHERE its records go (the default group for the function's
//! own name, and a group the same stack declares and the function's `LoggingConfig` names by
//! `Ref`), how the run ENDS (the handler returns a value, and the handler raises), and
//! whether the call asked for a log tail. Each row is invoked twice, so an id minted once per
//! function rather than once per invocation fails a row.
//!
//! No expected value here comes from a recording: each id is read off the answer the row's own
//! call got, each marker is built by the row, and the group each row searches is the one the
//! stack published.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, outputs_of};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The `StackStatus` a completed create reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` AWS Lambda reports a function it takes calls on in.
const ACTIVE: &str = "Active";

/// The header AWS reports a response's request id in, written out here rather than read from
/// any crate so the assertion is against what a client reads. HTTP header names are
/// case-insensitive, which is how an SDK reads this one back.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The header an answer whose handler did not return reports the failure class in.
const FUNCTION_ERROR_HEADER: &str = "x-amz-function-error";

/// The header the log tail of a run is reported in, base64-encoded.
const LOG_RESULT_HEADER: &str = "x-amz-log-result";

/// The prefix of the default log group of a function whose `LoggingConfig` names none: "a
/// default log group named `/aws/lambda/<function name>`"
/// (`smithy_query(query="doc",shape="LoggingConfig",member="LogGroup")`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The fixed framing AWS Lambda writes around every run, each record naming the invocation it
/// belongs to: `START RequestId: <id> Version: <version>`, `END RequestId: <id>` and `REPORT
/// RequestId: <id>` with what the run spent.
const START_PREFIX: &str = "START RequestId: ";
const END_PREFIX: &str = "END RequestId: ";
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// The three framing records of one run, which is the least a search for its id has to find.
const FRAMING_PREFIXES: &[&str] = &[START_PREFIX, END_PREFIX, REPORT_PREFIX];

/// A well-formed request id no invocation here was named by: a version-4 UUID whose random
/// bits are all zero, which no minted id carries. Searching a group for it stands for "an id
/// this group holds no run for".
const UNNAMED_REQUEST_ID: &str = "00000000-0000-4000-8000-000000000000";

/// How long one invocation of these handlers is given, as the function's own `Timeout`. The
/// handlers return at once; the span only has to cover the host's own scheduling.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long to wait between looks at something a service owes, and how many looks to take. The
/// budget is far longer than any span these services choose, so a loaded host is slower
/// without the rule being any different; nothing here asserts on elapsed time.
const POLL: Duration = Duration::from_millis(100);
const POLLS: usize = 600;

/// How many times a row re-drives its invocation while this PROCESS's own bounds answer
/// instead of a run of the handler, and how long it lets the suites holding those bounds get
/// through a stretch of their own work between attempts. Both are generous: this binary runs
/// its suites in parallel, so a trivial run can be refused for what another suite holds.
const HOST_STOP_ATTEMPTS: usize = 12;
const HOST_STOP_BACKOFF: Duration = Duration::from_millis(250);

/// The logical ids the template below declares.
const ROLE_ID: &str = "ExecutionRole";
const GROUP_ID: &str = "Group";
const FUNCTION_ID: &str = "Logger";

/// The handler a row whose run RETURNS deploys: it prints the marker the event carries and
/// answers the id the runtime gave this invocation, so the run's own view of the id can be
/// compared with the one the answer names.
const RETURNING_SOURCE: &str = r#"
def handler(event, context):
    print("ran %s" % event["marker"])
    return {"request": context.aws_request_id, "marker": event["marker"]}
"#;

/// The handler a row whose run RAISES deploys: it prints its marker and then raises, so the
/// answer is the runtime's error document rather than a value.
const RAISING_SOURCE: &str = r#"
def handler(event, context):
    print("ran %s" % event["marker"])
    raise ValueError("boom %s" % event["marker"])
"#;

/// The error class the runtime reports for an exception the handler raised, and the
/// `FunctionError` an answer carrying one states.
const RAISED_CLASS: &str = "ValueError";
const UNHANDLED: &str = "Unhandled";

/// The trust policy the execution role carries: AWS Lambda assumes it to run the function.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole"
        }]
    })
}

/// What that role may do: the three CloudWatch Logs calls an invocation's log delivery makes,
/// which is what the AWS-managed basic execution policy grants a function. A role short of
/// them leaves no records for a client to find at all, which is that rule rather than this
/// one.
fn logs_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*"
        }]
    })
}

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

    /// The id this answer names the request it answered by.
    fn request_id(&self) -> String {
        self.header(REQUEST_ID_HEADER).unwrap_or_default()
    }

    /// The execution log of the run, decoded from the log tail the answer reported.
    fn log_tail(&self) -> String {
        let encoded = self.header(LOG_RESULT_HEADER).unwrap_or_default();
        let decoded = ::aws_smithy_types::base64::decode(&encoded).unwrap_or_default();
        String::from_utf8_lossy(&decoded).to_string()
    }
}

/// Whether `id` has the hyphenated lowercase 8-4-4-4-12 form an AWS request id is spelled in.
fn is_request_id(id: &str) -> bool {
    let widths: Vec<usize> = id.split('-').map(str::len).collect();
    widths == vec![8, 4, 4, 4, 12]
        && id
            .chars()
            .all(|c| c == '-' || (c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
}

/// The emulator as it is ASSEMBLED, on one engine: the function the stack creates, the role it
/// runs as and the group its runs are delivered to are all held by the services mounted there.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: each call carries a credential scope so the
    // router can name the target service, and the signature itself is not under test here.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name));
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
        "cloudformation",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        Bytes::from(body),
    )
    .await
}

/// One AWS Lambda call over its `restJson1` wire, with the headers `headers` names.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> Answered {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let mut all: Vec<(&str, &str)> = vec![("content-type", "application/json")];
    all.extend_from_slice(headers);
    dispatch(router, "lambda", method, path, &all, Bytes::from(payload)).await
}

/// One Amazon CloudWatch Logs call over its `awsJson1_1` wire.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> Answered {
    dispatch(
        router,
        "logs",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("Logs_20140328.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// Poll `GetFunctionConfiguration` until AWS Lambda first reports the function `Active`, and
/// answer the configuration it reported then. The span is that service's own, so nothing here
/// sleeps a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let path = format!("/2015-03-31/functions/{name}/configuration");
    let mut last = Value::Null;
    for _ in 0..POLLS {
        let reported = lambda(router, "GET", &path, None, &[]).await;
        last = reported.json();
        if last.get("State").and_then(Value::as_str) == Some(ACTIVE) {
            return last;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("case {case:?}: AWS Lambda never reported {name} {ACTIVE}, last read {last}");
}

/// What a search of a group for a term came to: the status that service answered and the
/// messages it found.
///
/// The status is carried rather than folded into "nothing found", so an assertion that a
/// search finds NOTHING cannot be satisfied by a refusal of the search itself.
struct Searched {
    status: u16,
    messages: Vec<String>,
}

/// Search `group` for `term`: what a client holding a request id finds by looking for it,
/// expressed as that service's own `filterPattern` — a quoted string term, which is the form a
/// term carrying characters outside the syntax's word characters is spelled in.
///
/// A group nothing has written to yet does not exist, which that service answers as a refusal
/// rather than as an empty list, so a caller waiting for records looks again on such an answer
/// ([`search_until`]) and a caller asserting about an existing group reads its status.
async fn search(router: &MultiServiceRouter, group: &str, term: &str) -> Searched {
    let answered = logs(
        router,
        "FilterLogEvents",
        json!({ "logGroupName": group, "filterPattern": format!("\"{term}\"") }),
    )
    .await;
    let messages = if answered.status == 200 {
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
    } else {
        Vec::new()
    };
    Searched {
        status: answered.status,
        messages,
    }
}

/// Search `group` for `term`, asserting the search itself was answered: a group that exists
/// answers a search of it, so a refusal here is a failure of the row rather than nothing found.
async fn search_answered(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    term: &str,
) -> Vec<String> {
    let searched = search(router, group, term).await;
    assert_eq!(
        searched.status, 200,
        "case {case:?}: expected HTTP 200 from a search of {group:?} for {term:?}, got {}",
        searched.status
    );
    searched.messages
}

/// The records a search of `group` for `term` finds, looked for again until it finds at least
/// `at_least` of them or the looks run out. A run's log is delivered off the answer's path, so
/// this waits for the CONDITION rather than for a span.
async fn search_until(
    router: &MultiServiceRouter,
    group: &str,
    term: &str,
    at_least: usize,
) -> Vec<String> {
    let mut found = search(router, group, term).await.messages;
    for _ in 0..POLLS {
        if found.len() >= at_least {
            return found;
        }
        tokio::time::sleep(POLL).await;
        found = search(router, group, term).await.messages;
    }
    found
}

/// Every message `group` holds right now, whatever run wrote it.
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

/// Whether an answer of `status` carrying `body` reports one of the EMULATOR's own host bounds
/// instead of what the run came to: the run slots an invocation waits for, whose refusal says
/// to retry the request, and the memory bound read from the whole process, which stops
/// whichever run is in flight once this test binary as a whole is over it. Neither says
/// anything about the rule under test, so such an answer is re-driven rather than read.
fn is_host_bound(status: u16, body: &str) -> bool {
    if status != 200 {
        return body.contains("invocations are running in this emulator");
    }
    body.contains("Runtime.ExitError") && body.contains("signal: killed")
}

/// `Invoke` the function `name` with `marker`, re-driven while this process's own host bounds
/// answer instead of a run of the handler.
async fn invoke(
    router: &MultiServiceRouter,
    name: &str,
    marker: &str,
    headers: &[(&str, &str)],
) -> Answered {
    let path = format!("/2015-03-31/functions/{name}/invocations");
    let body = json!({ "marker": marker });
    let mut answered = lambda(router, "POST", &path, Some(body.clone()), headers).await;
    for _ in 1..HOST_STOP_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        tokio::time::sleep(HOST_STOP_BACKOFF).await;
        answered = lambda(router, "POST", &path, Some(body.clone()), headers).await;
    }
    answered
}

/// Where a row's function sends the records of its runs, and how its template says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// The template declares no `LoggingConfig` at all, so the function logs to the default
    /// group for its own name.
    Default,
    /// The SAME stack declares an `AWS::Logs::LogGroup` under this name, and the function's
    /// `LoggingConfig` reaches it by `Ref`.
    Declared(&'static str),
}

/// How a row's run ENDS, which decides where the run's own view of its id is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// The handler returns a value carrying the id the runtime gave the invocation.
    Returns,
    /// The handler raises, so the answer is the runtime's error document, which names the
    /// invocation it was raised in.
    Raises,
}

/// One row: the runtime, the handler, where its records go, how the run ends and whether the
/// call asks for a log tail.
struct Row {
    case: &'static str,
    function: &'static str,
    runtime: &'static str,
    destination: Destination,
    ending: Ending,
    tail: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a python 3.12 run that returns, logging into its default group",
        function: "invoke-id-default-group",
        runtime: "python3.12",
        destination: Destination::Default,
        ending: Ending::Returns,
        tail: false,
    },
    Row {
        case: "a python 3.13 run that returns, logging into a group the stack declares, asked \
               for its log tail",
        function: "invoke-id-declared-group",
        runtime: "python3.13",
        destination: Destination::Declared("/invoke-request-id/declared"),
        ending: Ending::Returns,
        tail: true,
    },
    Row {
        case: "a python 3.12 run that raises, logging into its default group",
        function: "invoke-id-raising",
        runtime: "python3.12",
        destination: Destination::Default,
        ending: Ending::Raises,
        tail: false,
    },
];

impl Row {
    /// The handler source this row deploys.
    fn source(&self) -> &'static str {
        match self.ending {
            Ending::Returns => RETURNING_SOURCE,
            Ending::Raises => RAISING_SOURCE,
        }
    }

    /// The group this row's runs are delivered to, given the function id the stack published.
    fn expected_group(&self, function: &str) -> String {
        match self.destination {
            Destination::Default => format!("{DEFAULT_GROUP_PREFIX}{function}"),
            Destination::Declared(name) => name.to_string(),
        }
    }

    /// The template this row deploys: the execution role, the group it declares (if any), and
    /// the function pointed at its destination.
    fn template(&self) -> String {
        let mut function_properties = serde_json::Map::new();
        function_properties.insert("FunctionName".to_string(), json!(self.function));
        function_properties.insert(
            "Role".to_string(),
            json!({ "Fn::GetAtt": [ROLE_ID, "Arn"] }),
        );
        function_properties.insert("Runtime".to_string(), json!(self.runtime));
        function_properties.insert("Handler".to_string(), json!("index.handler"));
        function_properties.insert("Timeout".to_string(), json!(FUNCTION_TIMEOUT));
        function_properties.insert("Code".to_string(), json!({ "ZipFile": self.source() }));
        if let Destination::Declared(_) = self.destination {
            function_properties.insert(
                "LoggingConfig".to_string(),
                json!({ "LogGroup": { "Ref": GROUP_ID } }),
            );
        }

        let mut resources = serde_json::Map::new();
        resources.insert(
            ROLE_ID.to_string(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "basic-execution",
                        "PolicyDocument": logs_policy()
                    }]
                }
            }),
        );
        if let Destination::Declared(name) = self.destination {
            resources.insert(
                GROUP_ID.to_string(),
                json!({
                    "Type": "AWS::Logs::LogGroup",
                    "Properties": { "LogGroupName": name }
                }),
            );
        }
        resources.insert(
            FUNCTION_ID.to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": Value::Object(function_properties)
            }),
        );

        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": Value::Object(resources),
            "Outputs": {
                "FunctionName": { "Value": { "Ref": FUNCTION_ID } }
            }
        })
        .to_string()
    }
}

/// The id the RUN itself reports, read where this row's ending states it: a returning
/// handler's own value, or the error document of one that raised.
fn run_reported_id(row: &Row, case: &str, answered: &Answered) -> String {
    let document = answered.json();
    match row.ending {
        Ending::Returns => {
            assert_eq!(
                answered.header(FUNCTION_ERROR_HEADER),
                None,
                "case {case:?}: the handler returns a value, so the run must not fail, got {:?}",
                answered.text()
            );
            document
                .get("request")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        }
        Ending::Raises => {
            assert_eq!(
                answered.header(FUNCTION_ERROR_HEADER).as_deref(),
                Some(UNHANDLED),
                "case {case:?}: a handler that raised is reported as {UNHANDLED}, got {:?}",
                answered.text()
            );
            assert_eq!(
                document.get("errorType").and_then(Value::as_str),
                Some(RAISED_CLASS),
                "case {case:?}: expected the raised class {RAISED_CLASS}, got {document}"
            );
            document
                .get("requestId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        }
    }
}

#[tokio::test]
async fn an_invocations_answer_names_the_run_a_client_then_finds_in_its_log_group() {
    // ONE emulator for every row: each row deploys a stack of its own, so the function and the
    // group a row reads are still that row's alone.
    let router = emulator();
    // Every id any row's invocation was named by: no two invocations share one.
    let mut named: BTreeSet<String> = BTreeSet::new();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;

        // 1. The stack: the execution role, the declared group where the row has one, and the
        //    function pointed at its destination.
        let stack = format!("invoke-request-id-{index}");
        let template = row.template();
        let created = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &template),
                // The template declares an IAM role, which is acknowledged with CAPABILITY_IAM.
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
            created.status,
            created.text()
        );
        let (reached, described) = wait_for_stack_status(&router, &stack, CREATE_COMPLETE).await;
        assert_eq!(
            reached, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described}"
        );
        let function = outputs_of(&described)
            .get("FunctionName")
            .cloned()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the stack published no function name in {described}")
            });
        let group = row.expected_group(&function);

        // 2. The function has to be invocable before any of it can be read: "During this
        //    time, you can't invoke or modify the function" (`CreateFunction`).
        let configuration = wait_until_active(&router, case, &function).await;
        if let Destination::Declared(declared) = row.destination {
            assert_eq!(
                configuration
                    .get("LoggingConfig")
                    .and_then(|logging| logging.get("LogGroup"))
                    .and_then(Value::as_str),
                Some(declared),
                "case {case:?}: expected AWS Lambda to report the declared group as the \
                 function's log group, got {configuration}"
            );
        }

        // 3. Two invocations of the SAME function: each answer names an invocation of its own,
        //    and each id finds that run's records in the group.
        let headers: Vec<(&str, &str)> = if row.tail {
            vec![("x-amz-log-type", "Tail")]
        } else {
            Vec::new()
        };
        let mut ids_of_this_function: Vec<String> = Vec::new();
        for delivery in 0..2 {
            let marker = format!("{stack}-{delivery}");
            let answered = invoke(&router, &function, &marker, &headers).await;
            assert!(
                !is_host_bound(answered.status, &answered.text()),
                "case {case:?} delivery {delivery}: this emulator's own host bounds answered \
                 every attempt, so no run of the handler was observed: {:?}",
                answered.text()
            );
            assert_eq!(
                answered.status, 200,
                "case {case:?} delivery {delivery}: expected HTTP 200 from Invoke, got {} with \
                 {:?}",
                answered.status,
                answered.text()
            );

            // 3a. The answer names the request it answered, in the form an AWS request id is
            //     spelled in.
            let stated = answered.request_id();
            assert!(
                is_request_id(&stated),
                "case {case:?} delivery {delivery}: expected the answer to name its request in \
                 {REQUEST_ID_HEADER} as an AWS request id, got {stated:?}"
            );

            // 3b. ... and the id it names is the RUN's own, as the run itself reports it.
            let reported = run_reported_id(row, case, &answered);
            assert_eq!(
                reported, stated,
                "case {case:?} delivery {delivery}: the run reports its invocation as \
                 {reported:?}, so the answer must name that invocation, got {stated:?}"
            );

            // 3c. ... and no other invocation of this suite was named by it.
            assert!(
                named.insert(stated.clone()),
                "case {case:?} delivery {delivery}: every invocation is named by an id of its \
                 own, and {stated} was already named"
            );
            ids_of_this_function.push(stated.clone());

            // 3d. A client holding that id finds the run's records by searching the group for
            //     it: the runtime's own framing, each record naming this invocation.
            let found = search_until(&router, &group, &stated, FRAMING_PREFIXES.len()).await;
            for prefix in FRAMING_PREFIXES {
                let expected = format!("{prefix}{stated}");
                assert!(
                    found.iter().any(|message| message.starts_with(&expected)),
                    "case {case:?} delivery {delivery}: expected a search of {group:?} for \
                     {stated:?} to find a record starting {expected:?}, got {found:?}"
                );
            }
            for message in &found {
                assert!(
                    message.contains(&stated),
                    "case {case:?} delivery {delivery}: a search for {stated:?} answers the \
                     records naming it, got {message:?}"
                );
            }

            // 3e. ... in the same group the run's own output went to, so the id really is the
            //     handle on THIS run's log and not on the function's log as a whole.
            let printed = format!("ran {marker}");
            let held = messages_in(&router, &group).await;
            assert!(
                held.iter().any(|message| message.contains(&printed)),
                "case {case:?} delivery {delivery}: expected {group:?} to hold the line the run \
                 printed, {printed:?}, got {held:?}"
            );

            // 3f. Where the call asked for a log tail, the tail it answered is the log of the
            //     same run, framed with the same id.
            if row.tail {
                let tail = answered.log_tail();
                for prefix in FRAMING_PREFIXES {
                    let expected = format!("{prefix}{stated}");
                    assert!(
                        tail.contains(&expected),
                        "case {case:?} delivery {delivery}: expected the log tail to carry \
                         {expected:?}, got {tail:?}"
                    );
                }
            }
        }

        // 4. The two runs are told apart by their ids: a search for one finds none of the
        //    other's records, so the id is what correlates a call with its own log.
        let [first, second] = <[String; 2]>::try_from(ids_of_this_function.clone())
            .unwrap_or_else(|ids| panic!("case {case:?}: expected two invocations, got {ids:?}"));
        for (searched, other) in [(&first, &second), (&second, &first)] {
            let found = search_answered(&router, case, &group, searched).await;
            assert!(
                !found.is_empty(),
                "case {case:?}: expected a search of {group:?} for {searched:?} to find that \
                 run's records"
            );
            assert!(
                found.iter().all(|message| !message.contains(other)),
                "case {case:?}: a search for {searched:?} must find none of {other:?}'s \
                 records, got {found:?}"
            );
        }

        // 5. An id no invocation was named by finds nothing at all, which is what makes the
        //    search a correlation rather than a listing of the group.
        let unnamed = search_answered(&router, case, &group, UNNAMED_REQUEST_ID).await;
        assert!(
            unnamed.is_empty(),
            "case {case:?}: expected a search of {group:?} for the unnamed id \
             {UNNAMED_REQUEST_ID:?} to find nothing, got {unnamed:?}"
        );
    }
}
