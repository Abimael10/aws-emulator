// SPDX-License-Identifier: CC0-1.0

//! The RETENTION a client set on an AWS Lambda function's own log group, and what the delivery
//! of an invocation's records does to it.
//!
//! RULE under test: **a log group that already exists is adopted, never re-created, by what
//! writes to it: AWS Lambda delivers an invocation's records to the group the function's
//! `LoggingConfig` resolves to — by default `/aws/lambda/<function name>` — so a group a
//! client created and set a retention on, whether through a stack's `AWS::Logs::LogGroup` or
//! through Amazon CloudWatch Logs' own `PutRetentionPolicy`, still resolves to exactly one
//! group under that name and still reports that `retentionInDays` after every run, while the
//! records of each run are readable in it.**
//!
//! That is the whole of "log retention as code": the group is declared beside the function
//! that logs into it, and what a client reads back afterwards has to be the DECLARED group
//! with the declared retention — not a second group under the same name, not a group whose
//! retention the first run cleared, and not a group that never becomes observable.
//!
//! The rows differ along what the rule generalises over rather than restating one template:
//!
//! * WHERE the group and its retention came from — an `AWS::Logs::LogGroup` the same stack
//!   declares for the function's default group name, a group created through CloudWatch Logs'
//!   own `CreateLogGroup`/`PutRetentionPolicy` before the function exists, and a retention put
//!   on the group AFTER a first run has already created and written to it (the ordering the
//!   rule has to hold under in both directions);
//! * the RETENTION itself — the shortest and the longest span the owning service publishes,
//!   and two from the middle of that published list;
//! * how many runs have written to the group by the time it is read — every row invokes the
//!   function twice, so a delivery that clears or re-creates the group on any run but the
//!   first fails the row.
//!
//! No expected value here is a literal from a recording: the retention values are the owning
//! service's own published list (quoted on [`PUBLISHED_RETENTIONS`]), the group name is derived
//! from the function name the row declares, and every marker is built by the row.
//!
//! Three crates have to agree for the rule to hold — CloudFormation provisions the group, the
//! role and the function; AWS Lambda runs the function and delivers its log; Amazon CloudWatch
//! Logs holds the group, its retention and its events — so the suite lives here rather than in
//! any one of them.

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

/// The spans Amazon CloudWatch Logs publishes for a log group's retention: "Possible values
/// are: 1, 3, 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557,
/// 2922, 3288, and 3653" (that model's `Days` shape,
/// `smithy_query(query="doc",shape="Days")`). The rows below take their values from this
/// list — its two ends and two entries inside it.
const PUBLISHED_RETENTIONS: [i64; 22] = [
    1, 3, 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557, 2922,
    3288, 3653,
];

/// The prefix of the log group a function whose `LoggingConfig` names none logs to: "a default
/// log group named `/aws/lambda/<function name>`"
/// (`smithy_query(query="doc",shape="LoggingConfig",member="LogGroup")`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// How long one invocation of this handler is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long to wait between polls of a condition another service owes, and how many polls to
/// make. The deadline is generous: nothing here is a budget a row is testing.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 600;

/// The fixed framing the Lambda runtime writes for every run it finishes.
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// The handler every row deploys: it prints the marker its event carries and answers it, so the
/// records of one run can be told from any other run's.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    print("logged %s" % event["marker"])
    return {"logged": event["marker"]}
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

/// The emulator as it is ASSEMBLED, on one engine: the group a stack declares, the group a
/// client's own call creates and the group a run writes to are all the same service's.
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

/// Every group `DescribeLogGroups` resolves for the prefix `name` whose own `logGroupName` is
/// exactly `name`: what a client holding the group's name finds by it.
async fn groups_named(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<Value> {
    let answered = logs(
        router,
        "DescribeLogGroups",
        json!({ "logGroupNamePrefix": name }),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from DescribeLogGroups for {name:?}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered
        .json()
        .get("logGroups")
        .and_then(Value::as_array)
        .map(|groups| {
            groups
                .iter()
                .filter(|group| group.get("logGroupName").and_then(Value::as_str) == Some(name))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// The ONE group `name` resolves to, polled until the name resolves to exactly one group or the
/// polls run out: a group another service creates for a run of its own is owed from the instant
/// that run is delivered, which no fixed sleep can name.
async fn the_group_named(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    for _ in 0..POLLS {
        let resolved = groups_named(router, case, name).await;
        if resolved.len() == 1 {
            return resolved[0].clone();
        }
        assert!(
            resolved.len() <= 1,
            "case {case:?}: expected the name {name:?} to resolve to at most 1 group, got \
             {resolved:?}"
        );
        tokio::time::sleep(POLL).await;
    }
    panic!("case {case:?}: the name {name:?} never resolved to exactly one log group");
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`. The span is
/// that service's own, so nothing here sleeps a fixed span on its behalf.
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
    panic!("case {case:?}: Lambda never reported {name} Active");
}

/// Every message CloudWatch Logs holds in `group`, polled until every line of `wanted` is among
/// them or the polls run out.
async fn messages_in(
    router: &MultiServiceRouter,
    group: &str,
    wanted: &[String],
) -> Vec<String> {
    let mut held: Vec<String> = Vec::new();
    for _ in 0..POLLS {
        let answered = logs(router, "FilterLogEvents", json!({ "logGroupName": group })).await;
        if answered.status == 200 {
            held = answered
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
                .unwrap_or_default();
            if wanted
                .iter()
                .all(|line| held.iter().any(|message| message.contains(line)))
            {
                return held;
            }
        }
        tokio::time::sleep(POLL).await;
    }
    held
}

/// Set the retention of `group` through CloudWatch Logs' own call, the way a client does.
async fn put_retention(router: &MultiServiceRouter, case: &str, group: &str, days: i64) {
    let answered = logs(
        router,
        "PutRetentionPolicy",
        json!({ "logGroupName": group, "retentionInDays": days }),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from PutRetentionPolicy on {group:?}, got {} with {:?}",
        answered.status,
        answered.text()
    );
}

/// WHERE the group and its retention come from in one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetentionSource {
    /// An `AWS::Logs::LogGroup` the same stack declares for the function's default group name,
    /// created before the function that logs into it.
    DeclaredInTheStack,
    /// A group created through CloudWatch Logs' own API, with its retention put on it, before
    /// the function exists at all.
    CreatedBeforeTheFunction,
    /// A retention put on the group AFTER a first run already created it and wrote to it.
    PutAfterTheFirstRun,
}

/// One row: the function it deploys, where the group's retention comes from, and which
/// published span it is.
struct Row {
    case: &'static str,
    function: &'static str,
    source: RetentionSource,
    /// The index into [`PUBLISHED_RETENTIONS`] of the span this row retains the group for.
    retention: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "a group the stack declares, retained for the shortest published span",
        function: "retention-declared-shortest",
        source: RetentionSource::DeclaredInTheStack,
        retention: 0,
    },
    Row {
        case: "a group the stack declares, retained for the longest published span",
        function: "retention-declared-longest",
        source: RetentionSource::DeclaredInTheStack,
        retention: PUBLISHED_RETENTIONS.len() - 1,
    },
    Row {
        case: "a group created and retained through CloudWatch Logs' own calls before the \
               function",
        function: "retention-put-before",
        source: RetentionSource::CreatedBeforeTheFunction,
        retention: 4,
    },
    Row {
        case: "a retention put on the group the first run created",
        function: "retention-put-after",
        source: RetentionSource::PutAfterTheFirstRun,
        retention: 7,
    },
];

/// The template one row deploys: the function, and — where the row declares it — the log group
/// for the function's own default group name, created before the function that logs into it.
fn template(row: &Row, group: &str, days: i64) -> String {
    let mut resources = serde_json::Map::new();
    let mut function = json!({
        "Type": "AWS::Serverless::Function",
        "Properties": {
            "FunctionName": row.function,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Timeout": FUNCTION_TIMEOUT,
            "InlineCode": HANDLER_SOURCE
        }
    });
    if row.source == RetentionSource::DeclaredInTheStack {
        resources.insert(
            "Group".to_string(),
            json!({
                "Type": "AWS::Logs::LogGroup",
                "Properties": {
                    "LogGroupName": group,
                    "RetentionInDays": days
                }
            }),
        );
        // The group is the function's to log into, so it is provisioned first — the ordering a
        // template that owns a function's log group states.
        function["DependsOn"] = json!("Group");
    }
    resources.insert("Logger".to_string(), function);
    json!({
        "Transform": "AWS::Serverless-2016-10-31",
        "Resources": Value::Object(resources),
        "Outputs": {
            "FunctionName": { "Value": { "Ref": "Logger" } }
        }
    })
    .to_string()
}

/// Invoke the function once with a marker of its own, and answer the line the handler printed
/// for it.
async fn invoke_with_marker(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    marker: &str,
) -> String {
    let invoked = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        Some(json!({ "marker": marker })),
    )
    .await;
    assert_eq!(
        invoked.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke for {marker:?}, got {} with {:?}",
        invoked.status,
        invoked.text()
    );
    assert_eq!(
        invoked.json().get("logged").and_then(Value::as_str),
        Some(marker),
        "case {case:?}: expected the handler's own result for {marker:?}, got {:?}",
        invoked.text()
    );
    format!("logged {marker}")
}

#[tokio::test]
async fn a_functions_log_group_keeps_the_retention_it_was_given_across_its_runs() {
    // ONE emulator for every row: each row owns a function, a group and a stack of its own, so
    // what a row reads back is still that row's alone.
    let router = emulator();
    for row in ROWS {
        let case = row.case;
        let group = format!("{DEFAULT_GROUP_PREFIX}{}", row.function);
        let days = PUBLISHED_RETENTIONS[row.retention];

        // 1. The group as the row's client set it up: created and retained through CloudWatch
        // Logs' own calls, before the function exists.
        if row.source == RetentionSource::CreatedBeforeTheFunction {
            let created = logs(&router, "CreateLogGroup", json!({ "logGroupName": group })).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected HTTP 200 from CreateLogGroup for {group:?}, got {} with \
                 {:?}",
                created.status,
                created.text()
            );
            put_retention(&router, case, &group, days).await;
        }

        // 2. The stack: the function, and the group beside it where the row declares one.
        let stack = format!("{}-stack", row.function);
        let body = template(row, &group, days);
        let created = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &body),
                // A template declaring a macro is acknowledged with CAPABILITY_AUTO_EXPAND, and
                // the role the transform synthesizes for the function with CAPABILITY_IAM.
                ("Capabilities.member.1", "CAPABILITY_IAM"),
                ("Capabilities.member.2", "CAPABILITY_AUTO_EXPAND"),
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
        assert_eq!(
            function, row.function,
            "case {case:?}: expected the stack to publish the declared function name"
        );

        // 3. Before any run: a group the client set up — in the stack or through its own calls
        // — already resolves under the function's default group name, carrying its retention.
        if row.source != RetentionSource::PutAfterTheFirstRun {
            let before = the_group_named(&router, case, &group).await;
            assert_eq!(
                before.get("retentionInDays").and_then(Value::as_i64),
                Some(days),
                "case {case:?}: expected the group {group:?} to report the retention {days} \
                 before any run, got {before}"
            );
        }

        // 4. The runs: two invocations, each with a marker of its own, so a delivery that
        // re-creates or replaces the group on a later run is caught.
        wait_until_active(&router, case, &function).await;
        let first = invoke_with_marker(&router, case, &function, "run-one").await;

        // A row whose retention is put on the group the FIRST run created waits for that group
        // and sets it then — the same rule with the ordering reversed.
        if row.source == RetentionSource::PutAfterTheFirstRun {
            let lazily = the_group_named(&router, case, &group).await;
            assert_eq!(
                lazily.get("retentionInDays").and_then(Value::as_i64),
                None,
                "case {case:?}: expected the group a run created to carry no retention of its \
                 own, got {lazily}"
            );
            put_retention(&router, case, &group, days).await;
        }

        let second = invoke_with_marker(&router, case, &function, "run-two").await;

        // 5. What a client reads back: both runs' records are in that one group, and the group
        // still reports the retention it was given.
        let wanted = vec![first, second];
        let held = messages_in(&router, &group, &wanted).await;
        for line in &wanted {
            assert!(
                held.iter().any(|message| message.contains(line)),
                "case {case:?}: expected the group {group:?} to hold the printed line {line:?}, \
                 got {held:?}"
            );
        }
        let reports = held
            .iter()
            .filter(|message| message.contains(REPORT_PREFIX))
            .count();
        assert_eq!(
            reports, 2,
            "case {case:?}: expected the group {group:?} to hold one {REPORT_PREFIX:?} record per \
             run, got {held:?}"
        );
        let after = the_group_named(&router, case, &group).await;
        assert_eq!(
            after.get("retentionInDays").and_then(Value::as_i64),
            Some(days),
            "case {case:?}: expected the group {group:?} to still report the retention {days} \
             after its runs, got {after}"
        );
    }
}
