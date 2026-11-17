// SPDX-License-Identifier: CC0-1.0

//! The X-RAY DAEMON ADDRESS a Lambda invocation environment publishes, and what a handler's
//! own datagram to it reaches: CloudFormation deploys the functions, Lambda runs them, and AWS
//! X-Ray holds the segments the daemon behind that address submitted.
//!
//! RULE: every Lambda invocation environment publishes `AWS_XRAY_DAEMON_ADDRESS` as an
//! `IP_ADDRESS:PORT` the run's own datagrams reach — the platform's own daemon address, unless
//! the function's configuration names a value of its own, because this is one of the variables
//! Lambda sets without RESERVING — and the daemon behind it reads the X-Ray daemon's framing: a
//! header line declaring the format, then one segment document per line, each of which it
//! submits to X-Ray, while a datagram whose framing it does not admit is dropped and leaves no
//! segment behind.
//!
//! Only a cross-service suite can pin the second half: the segment document leaves the runtime
//! this crate assembles and is held by another service's crate on the same engine, so it is
//! read back where a client reads it — through X-Ray's own `BatchGetTraces`.
//!
//! The rows span what only a suite can read of it: a function naming no `TracingConfig` at all
//! is published the platform's address and its run's datagrams reach the daemon there — one that
//! the framing does not admit (no header line), written first, and then one it does, so the
//! document that must land stands behind the one that must not — and a function whose own
//! environment names an address runs with that value. The same rule is read over a wider table,
//! without a run, by the unit tests of the lambda crate's own `state::execution`, and every other
//! framing by that crate's own suite,
//! `services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/xray_daemon_trace_store.rs`.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE reading in this target of "look until the stack reports the status", taken from the
// suite that owns it rather than copied here.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for and every ARN names.
const REGION: &str = "us-east-1";

/// An access key id neither IAM nor the token service holds a record for: it resolves to the
/// account's own identity, which no identity-based document bounds, so every call this suite
/// makes on its own behalf presents it.
const ACCOUNT_KEY: &str = "EMULATORSETUPKEY0002";

/// Where Lambda binds the function collection and one invocation.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function here runs, and the handler the archived inline source declares.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`. Nothing here tests a
/// timeout, so it is generous: a run shares this process with every other test of this target.
const FUNCTION_TIMEOUT: u64 = 120;

/// The `StackStatus` the setup waits for, and the `State` a function serves invocations in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const FUNCTION_ACTIVE: &str = "Active";

/// How long to wait between looks at work the clock owes, and how many looks to take.
/// Generous: a loaded host must not decide the outcome, and a look costs nothing once the
/// condition holds.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 2400;

/// How many times a row re-drives its invocation while the answer reports one of the bounds this
/// whole test binary shares rather than a run. A SMALL number on purpose: a host that really is
/// out of memory should have the suite say so rather than drive a run per look.
const HOST_BOUND_ATTEMPTS: usize = 6;

/// What one of the emulator's own host bounds says when it is what an answer reports
/// ([`is_host_bound`]): the refusal of an invocation the process has no run slot for, and the
/// class and the reason a run stopped for memory carries.
const RUN_SLOTS_BUSY: &str = "invocations are running in this emulator";
const RUNTIME_OUT_OF_MEMORY: &str = "Runtime.OutOfMemory";
const RUNTIME_KILLED: &str = "signal: killed";

/// The variable the daemon address is published in, and the one beside it naming what a tracer
/// does when it finds no trace context, with the value Lambda gives it.
const DAEMON_ADDRESS_VARIABLE: &str = "AWS_XRAY_DAEMON_ADDRESS";
const CONTEXT_MISSING_VARIABLE: &str = "AWS_XRAY_CONTEXT_MISSING";
const CONTEXT_MISSING_LOG_ERROR: &str = "LOG_ERROR";

/// The port the X-Ray daemon listens for segment documents on (`2000/udp`), which the address
/// the platform publishes ends in.
const DAEMON_PORT: u16 = 2000;

/// The address the one function that names its OWN daemon address carries. A loopback address
/// and a port of its own, so it is told apart from the platform's.
const OWN_DAEMON_ADDRESS: &str = "127.0.0.1:2100";

/// The header line of the daemon's own framing.
const DAEMON_HEADER: &str = r#"{"format": "json", "version": 1}"#;

/// Where a framing carries the segment document the handler fills in. The Python below carries
/// the same literal in its own `replace` call, because a `const` cannot be interpolated into
/// another one: the two are written out once each, here and there.
const DOCUMENT_SLOT: &str = "__DOCUMENT__";

/// The handler every function below runs: it reports the daemon address its own environment
/// publishes and writes the datagrams the event names TO THAT ADDRESS, reporting what each
/// write answered — so the invocation's own answer is evidence of what the runtime let it do.
///
/// The framings are the test's and the document is filled in by the run, because the instant a
/// segment covers is the run's own.
const HANDLER_SOURCE: &str = r#"
import json
import os
import socket
import time


def _sent(named, datagram):
    host, _, port = named.rpartition(":")
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as writer:
        return writer.sendto(datagram.encode("utf-8"), (host, int(port)))


def handler(event, context):
    daemon = os.environ.get("AWS_XRAY_DAEMON_ADDRESS")
    report = {
        "daemon": daemon,
        "context_missing": os.environ.get("AWS_XRAY_CONTEXT_MISSING"),
        "sent": [],
        "errors": [],
    }
    if not daemon:
        return report
    for entry in event.get("datagrams") or []:
        started = time.time()
        document = json.dumps(
            {
                "trace_id": entry["trace_id"],
                "id": entry["id"],
                "name": entry["name"],
                "start_time": started,
                "end_time": time.time(),
            }
        )
        datagram = entry["framing"].replace("__DOCUMENT__", document)
        try:
            report["sent"].append(_sent(daemon, datagram))
            report["errors"].append(None)
        except OSError as failed:
            report["sent"].append(None)
            report["errors"].append(str(failed))
    return report
"#;

/// One row: the function's `TracingConfig`, the daemon address its own configuration names, and
/// whether this row also writes datagrams to the address it was published.
struct Row {
    /// The case this row is named by in every assertion.
    case: &'static str,
    /// The `TracingConfig` mode the function carries, or `None` for one naming none at all.
    tracing: Option<&'static str>,
    /// The `AWS_XRAY_DAEMON_ADDRESS` the function's own environment sets, or `None` for one
    /// that sets none and is published the platform's.
    configured: Option<&'static str>,
    /// Whether the row's run WRITES to the address it read: one row does, which is what shows
    /// the address really is the daemon's. The rest read the published value alone.
    delivers: bool,
}

/// The table this suite drives END TO END: a function naming no `TracingConfig` at all — the
/// address is published for every invocation, not only a traced one — and one whose own
/// environment names an address of its own, which wins because the variable is unreserved.
///
/// The same rule is read over a wider table, and without a run, beside the code that publishes
/// it: the unit tests of the lambda crate's own `state::execution` span the trace contexts an
/// invocation can carry and both variables a function can set. What only a suite can read is
/// what this one adds: the address reaches a daemon, and the function really runs with the value
/// its configuration named.
const ROWS: &[Row] = &[
    Row {
        case: "a function naming no tracing configuration at all",
        tracing: None,
        configured: None,
        delivers: true,
    },
    Row {
        case: "a function whose own environment names the daemon address",
        tracing: Some("Active"),
        configured: Some(OWN_DAEMON_ADDRESS),
        delivers: false,
    },
];

/// What one call answered.
struct Answered {
    status: u16,
    /// "X-Amz-Function-Error — If present, indicates that an error occurred during function
    /// execution" (`Invoke`'s own output).
    function_error: Option<String>,
    body: Bytes,
}

impl Answered {
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

/// The emulator as it is ASSEMBLED, on one engine: the submission the daemon makes is dispatched
/// back into this same router, so the trace store this suite queries is the one it reached.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: the credential scope is what names the
    // service a request is routed to.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    content_type: &str,
    signing_name: &str,
    body: Bytes,
) -> Answered {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", content_type)
        .header("authorization", authorization(signing_name))
        .body(body)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answered {
        status: parts.status.as_u16(),
        function_error: parts
            .headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body,
    }
}

/// One call over an awsQuery wire (IAM, CloudFormation) that has to be accepted; answers its
/// XML body.
async fn query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        "POST",
        &format!("{signing_name}.{REGION}.amazonaws.com"),
        "/",
        "application/x-www-form-urlencoded",
        signing_name,
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "setup {case:?}: expected HTTP 200 from {action} on {signing_name}, got {} with {}",
        answered.status,
        answered.text()
    );
    answered.text()
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Answered {
    let payload = body.map_or_else(Vec::new, |document| document.to_string().into_bytes());
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        "application/json",
        "lambda",
        Bytes::from(payload),
    )
    .await
}

/// The execution role every function below runs as: trusted by the Lambda service, which
/// `CreateFunction` checks, and granted the submission the daemon makes on the run's behalf —
/// the grants the X-Ray daemon's own managed policy carries. Answers its ARN.
async fn given_execution_role(router: &MultiServiceRouter, case: &str, name: &str) -> String {
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
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "logs:CreateLogGroup",
                "logs:CreateLogStream",
                "logs:PutLogEvents",
                "xray:PutTraceSegments",
                "xray:PutTelemetryRecords",
            ],
            "Resource": "*",
        }],
    })
    .to_string();
    query(
        router,
        case,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "daemon-write"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// Deploy one function per row, each running [`HANDLER_SOURCE`] on `role_arn` with that row's
/// tracing configuration and its own environment, and wait until Lambda reports every one of
/// them active.
///
/// The handler is declared as a stack's inline source, which CloudFormation archives into the
/// deployment package — the way a function whose handler is written out in a suite comes to
/// exist at all.
async fn given_functions(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    role_arn: &str,
    functions: &[(String, &Row)],
) {
    let mut resources = serde_json::Map::new();
    for (index, (name, row)) in functions.iter().enumerate() {
        let mut properties = json!({
            "FunctionName": name,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Role": role_arn,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": {"ZipFile": HANDLER_SOURCE},
        });
        if let Some(mode) = row.tracing {
            properties["TracingConfig"] = json!({"Mode": mode});
        }
        if let Some(address) = row.configured {
            let mut variables = serde_json::Map::new();
            variables.insert(DAEMON_ADDRESS_VARIABLE.to_string(), json!(address));
            properties["Environment"] = json!({"Variables": Value::Object(variables)});
        }
        resources.insert(
            format!("Probe{index}"),
            json!({"Type": "AWS::Lambda::Function", "Properties": properties}),
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
    // The stack's work and each function's provisioning window are owed by the CLOCK, which no
    // fixed sleep names. What this setup needs is the condition the rows invoke under — every
    // function reporting itself active — so that is what is looked for, with the stack's own
    // status read only to say what the setup was waiting on when it never arrives.
    for (name, row) in functions {
        let row_case = row.case;
        let mut configuration = Value::Null;
        let mut active = false;
        for _ in 0..POLLS {
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
            tokio::time::sleep(POLL).await;
        }
        if !active {
            let (reached, answered) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
            panic!(
                "setup {case:?}: expected {row_case:?} to reach {FUNCTION_ACTIVE}, last saw \
                 {configuration} with the stack {stack:?} reporting {reached:?} ({answered})"
            );
        }
    }
}

/// One `Invoke` carrying `event`, re-driven while the answer reports one of the EMULATOR's own
/// host bounds rather than what the run came to; answers the document the handler returned.
///
/// Two of those bounds belong to no suite: the run slots an invocation waits for, whose refusal
/// says to retry, and the memory bound read from the whole process, which stops whichever run is
/// in flight. Neither says anything about the rule, so the request is made again — and never
/// skipped, because an answer nothing was read from asserts nothing.
async fn invoked(router: &MultiServiceRouter, case: &str, name: &str, event: &Value) -> Value {
    let mut answered = invoke_once(router, name, event).await;
    for _ in 1..HOST_BOUND_ATTEMPTS {
        if !is_host_bound(answered.status, &answered.text()) {
            break;
        }
        tokio::time::sleep(POLL).await;
        answered = invoke_once(router, name, event).await;
    }
    assert!(
        !is_host_bound(answered.status, &answered.text()),
        "case {case:?}: every attempt reported a bound this whole test binary shares, so nothing \
         of the rule was read: {}",
        answered.text()
    );
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} with {}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.function_error, None,
        "case {case:?}: the handler reads the published address and writes to it, so the \
         invocation carries no function error, got {:?} with {}",
        answered.function_error,
        answered.text()
    );
    answered.json()
}

/// Whether an answer of `status` carrying `body` reports one of the emulator's own host bounds
/// instead of what the run came to: the run slots it waits for, whose refusal says to retry, and
/// the memory bound read from the whole process, which stops the run in flight.
///
/// Read here rather than shared with the sibling suite that reads the same two bounds for a
/// different error class: widening that passing suite's own predicate would put an unrelated
/// file in this diff, which is what a reviewer of this change asked it not to do.
fn is_host_bound(status: u16, body: &str) -> bool {
    if status != 200 {
        return body.contains(RUN_SLOTS_BUSY);
    }
    body.contains(RUNTIME_OUT_OF_MEMORY) && body.contains(RUNTIME_KILLED)
}

/// One `Invoke` request on the function `name` names.
async fn invoke_once(router: &MultiServiceRouter, name: &str, event: &Value) -> Answered {
    lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        Some(event),
    )
    .await
}

/// The `name` of every segment X-Ray holds for `trace_id`, read through `BatchGetTraces` — the
/// operation a client reads a trace back with (`POST /Traces`).
///
/// The sibling suite that reads a whole trace TREE keeps its own reader private, and what this
/// rule needs of the answer is only the names a producer submitted, so this reads that much.
async fn names_of(router: &MultiServiceRouter, trace_id: &str) -> BTreeSet<String> {
    let answered = dispatch(
        router,
        "POST",
        &format!("xray.{REGION}.amazonaws.com"),
        "/Traces",
        "application/json",
        "xray",
        Bytes::from(json!({"TraceIds": [trace_id]}).to_string()),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "expected HTTP 200 from BatchGetTraces, got {} with {}",
        answered.status,
        answered.text()
    );
    answered
        .json()
        .get("Traces")
        .and_then(Value::as_array)
        .map(|traces| {
            traces
                .iter()
                .flat_map(|trace| {
                    trace
                        .get("Segments")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|segment| {
                    segment
                        .get("Document")
                        .and_then(Value::as_str)
                        .and_then(|text| serde_json::from_str::<Value>(text).ok())
                })
                .filter_map(|document| {
                    document
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Look at the trace store until it holds a segment named each of `wanted`, and answer the names
/// it last reported. The submission is the emulator's own work, which no fixed sleep names.
async fn awaited_names(
    router: &MultiServiceRouter,
    case: &str,
    trace_id: &str,
    wanted: &[String],
) -> BTreeSet<String> {
    let mut held = BTreeSet::new();
    for _ in 0..POLLS {
        held = names_of(router, trace_id).await;
        if wanted.iter().all(|name| held.contains(name)) {
            return held;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!(
        "case {case:?}: expected X-Ray to hold a segment named each of {wanted:?} under the \
         chain {trace_id}, got {held:?}"
    );
}

/// A trace id of X-Ray's own form: `1-<8 hex digits of the epoch second>-<24 hex digits>`,
/// unique per row so one row's documents are told from another's.
fn minted_trace_id(index: usize) -> String {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = u32::try_from(since.as_secs() & 0xffff_ffff).unwrap_or(u32::MAX);
    let nanos = u64::from(since.subsec_nanos());
    format!("1-{seconds:08x}-{nanos:016x}{index:08x}")
}

/// What one row writes on its run: the framing, the segment name the document in it carries, and
/// whether a segment of that name must be in the store afterwards.
struct Framing {
    /// The datagram as the handler writes it, with [`DOCUMENT_SLOT`] where the document goes.
    framing: String,
    /// The `name` the document in it carries, which is how the store is read for it.
    name: String,
    /// Whether the daemon admits this framing, so the document it carries is stored.
    stored: bool,
}

/// The framings one row drives, in the order the run writes them: the one the daemon does NOT
/// admit first, so the document that must land stands behind it.
///
/// Two framings, not a framing table: every form a datagram's framing can take is read against
/// the daemon by the owning crate's own suite
/// (`services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/xray_daemon_trace_store.rs`),
/// and what this suite adds is the published ADDRESS across the rows above — so one admitted and
/// one rejected datagram are what each row needs to show the address it was handed really is the
/// daemon's.
fn framings(index: usize) -> Vec<Framing> {
    vec![
        Framing {
            framing: DOCUMENT_SLOT.to_string(),
            name: format!("daemon-probe-{index}-no-header"),
            stored: false,
        },
        Framing {
            framing: format!("{DAEMON_HEADER}\n{DOCUMENT_SLOT}"),
            name: format!("daemon-probe-{index}-well-formed"),
            stored: true,
        },
    ]
}

/// The whole rule, driven END TO END: every row below is a real deployment, a real `Invoke` and a
/// real `BatchGetTraces` over the assembled router.
#[tokio::test]
async fn every_invocation_environment_publishes_a_daemon_address_its_datagrams_reach() {
    let router = build_router();
    let setup = "the traced functions";
    let role = given_execution_role(&router, setup, "xsvc-xray-daemon-address").await;
    let names: Vec<String> = (0..ROWS.len())
        .map(|index| format!("xsvc-xray-daemon-address-{index}"))
        .collect();
    let functions: Vec<(String, &Row)> = names.iter().cloned().zip(ROWS.iter()).collect();
    given_functions(&router, setup, "xsvc-xray-daemon-address-stack", &role, &functions).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let trace_id = minted_trace_id(index);
        // Only the one row that delivers writes: what a datagram addressed to a value a function
        // made up reaches is not this rule's business (on AWS the daemon listens where the
        // platform put it), and every other framing is the owning crate's suite's to drive.
        let written = if row.delivers {
            framings(index)
        } else {
            Vec::new()
        };
        let datagrams: Vec<Value> = written
            .iter()
            .enumerate()
            .map(|(slot, framing)| {
                json!({
                    "framing": framing.framing,
                    "name": framing.name,
                    // 16 hexadecimal digits, as a segment id is, and distinct per datagram.
                    "id": format!("{:016x}", (index + 1) * 0x100 + slot),
                    "trace_id": trace_id,
                })
            })
            .collect();
        let event = json!({"datagrams": datagrams});
        let report = invoked(&router, case, &names[index], &event).await;

        // The ADDRESS the environment publishes: the documented `IP_ADDRESS:PORT`, and the
        // function's own value where its configuration named one. The form is read with the
        // standard library's own parser for it, so `1.2.3.4:2000` is admitted and a near miss
        // is not.
        let daemon = report
            .get("daemon")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let published: std::net::SocketAddrV4 = daemon.parse().unwrap_or_else(|_| {
            panic!("case {case:?}: expected an IP_ADDRESS:PORT daemon address, got {daemon:?}")
        });
        match row.configured {
            Some(configured) => assert_eq!(
                daemon, configured,
                "case {case:?}: expected the function's own value for \
                 {DAEMON_ADDRESS_VARIABLE}, got {daemon:?}"
            ),
            None => assert_eq!(
                published.port(),
                DAEMON_PORT,
                "case {case:?}: expected the platform's address to name the daemon's own port, \
                 got {daemon:?}"
            ),
        }
        assert_eq!(
            report.get("context_missing").and_then(Value::as_str),
            Some(CONTEXT_MISSING_LOG_ERROR),
            "case {case:?}: expected {CONTEXT_MISSING_VARIABLE} beside it, got {report}"
        );

        if written.is_empty() {
            // A row that writes nothing has no write and no stored document to read: the
            // published value above is the whole of it.
            continue;
        }
        // What the WRITES answered: a datagram to the published address is accepted by the
        // socket interface, whether or not the daemon then admits its framing.
        let errors = report
            .get("errors")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let sent = report
            .get("sent")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            errors.len(),
            written.len(),
            "case {case:?}: expected one outcome per datagram the row writes, got {report}"
        );
        assert!(
            errors.iter().all(Value::is_null),
            "case {case:?}: expected every write to the published address to be accepted, got \
             {report}"
        );
        assert!(
            sent.len() == written.len()
                && sent
                    .iter()
                    .all(|count| count.as_u64().is_some_and(|bytes| bytes > 0)),
            "case {case:?}: expected each write to report the bytes it sent, got {report}"
        );

        // And what the STORE holds: the document the daemon's own framing carried, and nothing
        // of the framing it does not admit.
        let wanted: Vec<String> = written
            .iter()
            .filter(|framing| framing.stored)
            .map(|framing| framing.name.clone())
            .collect();
        let held = awaited_names(&router, case, &trace_id, &wanted).await;
        for framing in &written {
            let name = &framing.name;
            if framing.stored {
                assert!(
                    held.contains(name),
                    "case {case:?}: expected X-Ray to hold the segment {name:?} the daemon was \
                     sent, got {held:?}"
                );
            } else {
                assert!(
                    !held.contains(name),
                    "case {case:?}: expected the daemon to drop the datagram carrying {name:?}: \
                     it was written BEFORE the one that landed, on the same run, so by the time \
                     that one is held this one has had its chance; got {held:?}"
                );
            }
        }
    }
}
