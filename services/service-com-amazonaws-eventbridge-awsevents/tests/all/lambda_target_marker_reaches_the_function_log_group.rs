// SPDX-License-Identifier: CC0-1.0

//! What a client POLLING a function's log group sees of an EventBridge rule's Lambda-target
//! delivery, wired the way a client wires it from the command line: `PutRule`, `PutTargets`,
//! `AddPermission` on the function, `PutEvents` — and then the function's own
//! `/aws/lambda/<function name>` group, which is the only place the delivery becomes visible.
//!
//! The RULE pinned here: a matched rule's hand-over to a Lambda target IS an invocation of that
//! function, admitted only where the FUNCTION's own resource-based policy allows
//! `events.amazonaws.com` the action that invocation performs — `lambda:InvokeFunction` — for
//! the invoking rule ("for Lambda functions, Amazon SNS topics and Amazon SQS queues,
//! EventBridge relies on resource-based policies", `PutTargets`). Where the policy admits it,
//! the handler really RUNS and what it wrote about the event it was handed is delivered to the
//! function's log group ("by default, Lambda functions send logs to a default log group named
//! `/aws/lambda/<function name>`", `LoggingConfig$LogGroup`); where it does not — no statement
//! at all, a statement scoped by `aws:SourceArn` to another rule, a statement naming another
//! action — nothing is invoked and that group holds no line of the handler, and the same holds
//! for an event the rule's own pattern does not match.
//!
//! Both arms matter to a client equally: a bus that delivers nothing looks exactly like a
//! correct permission refusal when the only observation is the log group, so an emulator that
//! accepted every control-plane call and ran no handler would make an IAM mistake
//! undiagnosable. This suite is the one that reads the marker out of the group a client polls;
//! `crate::lambda_target_handler_records_the_invoking_rule` asserts the permitted arm at the
//! function's own Parameter Store effect, and
//! `crate::lambda_target_handler_failure_is_not_a_delivery_failure` what an undeliverable
//! hand-over is dead-lettered as.
//!
//! The rows differ along what the rule generalises over: the FORM of the statement in force on
//! the target (none, unscoped, scoped by `aws:SourceArn` to the invoking rule, scoped to
//! another rule, naming another action of the same principal) and whether the published event
//! matches the rule's pattern at all. Each row is its OWN test, over an engine of its own: a
//! test of this target runs in its own process, so the rows run beside each other, and no test
//! carries more than one row's work.
//!
//! Every absence asserted here is PACED by something positive rather than by a wait: each row
//! also wires a second rule of the same bus — matching on `source` alone, so it matches every
//! publication this suite makes — to a CONTROL function that is granted the invocation
//! unscoped. Once the control function's own line is in ITS group, the dispatcher has decided
//! this publication and run what it handed over, so a line owed to the subject would be in the
//! subject's group by then.
//!
//! No step waits a span of its own choosing and no assertion reads the clock: each row opens one
//! deadline ([`ROW_BUDGET`]) and every look-again below shares it, so the row is as patient as
//! the emulator's own run limits allow and still terminates.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The `source` every event this suite publishes carries, which both rules of a row match on.
const SOURCE: &str = "com.demo.logged-delivery";

/// The `detail-type` the SUBJECT rule's pattern names, and one it does not — which is how a row
/// publishes an event that rule cannot match while the control rule still matches it.
const DETAIL_TYPE: &str = "Logged Delivery Probe";
/// See [`DETAIL_TYPE`].
const OTHER_DETAIL_TYPE: &str = "Unmatched Probe";

/// The principal EventBridge invokes a resource-policy-governed target as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The IAM action an invocation of a function performs, which is what a grant for this service
/// has to name, and one of the same service that is NOT that action — so a row shows it is the
/// ACTION and not merely the presence of a statement that admits the delivery.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";
/// See [`INVOKE_ACTION`].
const OTHER_ACTION: &str = "lambda:GetFunction";

/// The id the one statement a row's grant is written under carries.
const STATEMENT_ID: &str = "events-invoke";

/// The module and attribute the deployment package carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The id of the one target each rule of a row carries.
const TARGET_ID: &str = "target-1";

/// What the handler prints before the marker of the event it was handed. ONE spelling of it,
/// bound into the handler's source and into what the assertions look for.
const MARKER_PREFIX: &str = "delivered marker ";

/// How long ONE row is given for every state the clock owes it together — two functions leaving
/// `Pending`, the hand-over, the asynchronous invocations and the delivery of their logs.
///
/// PATIENCE, not a weakened expectation: every assertion below is unchanged, and the steps SHARE
/// this one deadline rather than each taking a span of its own, so the row is as patient as it
/// can be and still terminates well inside the time a test may take. The bound is taken from
/// what the EMULATOR itself allows an invocation rather than picked here — the wait for a share
/// of the run capacity ([`PERMIT_WAIT`]) and the wait for the run's own answer
/// ([`HOST_WAIT_CEILING`]), for each of the two invocations a row waits on — with room on top,
/// because a whole-workspace run shares its cores with every other suite.
const ROW_BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_mul(2)
    .saturating_add(Duration::from_secs(60));

/// How often it looks while waiting.
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The handler both functions of every row run: it prints the marker of the event it was handed,
/// taken from the envelope's own `detail`, so a line in a log group is a consequence of THAT
/// event having reached THAT function.
///
/// It also checks the envelope EventBridge hands a target over as, so an event delivered in some
/// other shape fails the run instead of being logged.
fn logging_handler() -> String {
    format!(
        r#"
MARKER_PREFIX = "{MARKER_PREFIX}"
SOURCE = "{SOURCE}"


def lambda_handler(event, context):
    if event["source"] != SOURCE:
        raise ValueError("source is " + repr(event.get("source")))
    if not event["id"]:
        raise ValueError("the envelope carries no id")
    print(MARKER_PREFIX + event["detail"]["marker"])
    return event["detail"]["marker"]
"#
    )
}

/// The line the handler writes for the event carrying `marker`.
fn expected_line(marker: &str) -> String {
    format!("{MARKER_PREFIX}{marker}")
}

/// A .zip deployment package holding the handler above as `lambda_function.py`, written in
/// memory the way a caller uploads one and base64-encoded as `FunctionCode$ZipFile` carries it.
fn package() -> String {
    let source = logging_handler();
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    base64::engine::general_purpose::STANDARD.encode(buffer.into_inner())
}

/// EventBridge, Lambda and CloudWatch Logs over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them: the rule hands over in-process, Lambda runs the handler, and the log that run
/// produced is read through CloudWatch Logs' own API.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// An EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One CloudWatch Logs call (`awsJson1_1`, `Logs_20140328.<Operation>`), as its status and
/// parsed body: a group nothing has written to is that service's own refusal.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "logs", "Logs_20140328", "1.1", operation, body).await
}

/// A Lambda `restJson1` call, as its status and parsed body.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: Value) -> (u16, Value) {
    let payload = if body.is_null() {
        bytes::Bytes::new()
    } else {
        bytes::Bytes::from(body.to_string())
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The log group a function's invocations are logged to: "by default, Lambda functions send logs
/// to a default log group named `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
fn log_group(function: &str) -> String {
    format!("/aws/lambda/{function}")
}

/// Create one function running the handler above, and answer the ARN Lambda assigned it.
async fn create_function(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        json!({
            "FunctionName": name,
            "Role": format!(
                "arn:aws:iam::{}:role/service-role/lambda-exec",
                engine::emulator_account_id()
            ),
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 20,
            "Code": { "ZipFile": package() },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda CreateFunction, got {status} ({created})"
    );
    created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a FunctionArn, got {created}"))
        .to_string()
}

/// Wait until the function is `Active`. "During this time, you can't invoke or modify the
/// function" (`CreateFunction`): the created function leaves `Pending` by the clock, so this
/// LOOKS for the state rather than waiting a span out, and a hand-over withheld afterwards is
/// the policy's refusal and not the lifecycle's.
async fn until_active(router: &MultiServiceRouter, case: &str, name: &str, deadline: Instant) {
    let uri = format!("/2015-03-31/functions/{name}/configuration");
    loop {
        let (status, read) = lambda(router, "GET", &uri, Value::Null).await;
        if status == 200 && read["State"] == json!("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the function {name:?} to become Active, last read {read}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Add one statement to the function's resource-based policy through `AddPermission`, which is
/// how a client grants this service the invocation.
async fn add_permission(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    action: &str,
    source_arn: Option<&str>,
) {
    let mut body = json!({
        "StatementId": STATEMENT_ID,
        "Action": action,
        "Principal": EVENTS_PRINCIPAL,
    });
    if let Some(source_arn) = source_arn {
        body["SourceArn"] = json!(source_arn);
    }
    let (status, answered) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        body,
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda AddPermission, got {status} ({answered})"
    );
}

/// An ENABLED rule on the default bus, answered with its ARN: the SUBJECT rule matches the
/// `detail-type` and marker of one row, the CONTROL rule matches this suite's `source` alone, so
/// it matches every publication a row makes.
async fn create_rule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    marker: Option<&str>,
) -> String {
    let pattern = match marker {
        Some(marker) => json!({
            "source": [SOURCE],
            "detail-type": [DETAIL_TYPE],
            "detail": { "marker": [marker] },
        }),
        None => json!({ "source": [SOURCE] }),
    };
    let rule = events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventPattern": pattern.to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    rule["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a RuleArn, got {rule}"))
        .to_string()
}

/// Wire one function as the rule's only target.
async fn wire_target(router: &MultiServiceRouter, case: &str, rule: &str, function_arn: &str) {
    let wired = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": rule,
            "Targets": [{ "Id": TARGET_ID, "Arn": function_arn }],
        }),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the target to be accepted, got {wired}"
    );
}

/// Every line the group `group` holds, as `FilterLogEvents` reports them — none while the group
/// is not there yet, which is what a function that never ran leaves behind.
async fn log_lines(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, answered) =
        logs(router, "FilterLogEvents", &json!({ "logGroupName": group })).await;
    if status != 200 {
        return Vec::new();
    }
    answered["events"]
        .as_array()
        .map(|found| {
            found
                .iter()
                .filter_map(|event| event["message"].as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Assert that the group holds no line of any handler, and that the read which says so is one
/// CloudWatch Logs answered as such: either the group is there and empty, or it was never
/// created. A read that failed for any other reason is not an absence.
async fn assert_nothing_logged(router: &MultiServiceRouter, case: &str, group: &str) {
    let (status, answered) =
        logs(router, "FilterLogEvents", &json!({ "logGroupName": group })).await;
    if status == 200 {
        let held = answered["events"].as_array().cloned().unwrap_or_default();
        assert!(
            held.is_empty(),
            "case {case:?}: nothing may be invoked, so {group:?} holds no line, got {held:?}"
        );
        return;
    }
    let reported = answered["__type"].as_str().unwrap_or_default();
    assert!(
        reported.contains("ResourceNotFound"),
        "case {case:?}: the absence of a line in {group:?} must be CloudWatch Logs reporting no \
         such group, got {status} with {answered}"
    );
}

/// Wait until the group holds a line carrying `needle`, or `deadline` passes, and answer every
/// line it then holds.
async fn until_logged(
    router: &MultiServiceRouter,
    group: &str,
    needle: &str,
    deadline: Instant,
) -> Vec<String> {
    loop {
        let held = log_lines(router, group).await;
        if held.iter().any(|line| line.contains(needle)) || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// What a row's grant on the subject function says.
#[derive(Debug, Clone, Copy)]
enum Grant {
    /// No `AddPermission` at all, so the function carries no policy.
    None,
    /// The service principal allowed the invocation, unscoped.
    Unscoped,
    /// The same grant, scoped by `aws:SourceArn` to the rule that invokes the function.
    ThisRule,
    /// The same grant, scoped by `aws:SourceArn` to ANOTHER rule of the same bus.
    OtherRule,
    /// A statement for the same principal naming an action that is not the invocation.
    OtherAction,
}

/// One row: the statement in force on the subject function, and whether the published event is
/// one the subject rule's own pattern matches.
struct Row {
    case: &'static str,
    /// What keeps this row's resources apart from another row's, for a reader of a failure.
    label: &'static str,
    grant: Grant,
    matched: bool,
    /// Whether the subject function's handler must have run, which is what the row asserts.
    invoked: bool,
}

/// Stand one row up — two functions, the subject rule, the control rule and a third rule a
/// scoped grant can name — publish its event, and assert what the two groups then hold.
///
/// Every state the clock owes the row is waited for against ONE deadline opened here, so the
/// steps share the row's patience instead of each taking a span of its own.
async fn assert_row(row: &Row) {
    let case = row.case;
    let deadline = Instant::now() + ROW_BUDGET;
    let router = router();
    let label = row.label;
    let subject_function = format!("logged-delivery-subject-{label}");
    let control_function = format!("logged-delivery-control-{label}");
    let subject_rule = format!("logged-delivery-rule-{label}");
    let control_rule = format!("logged-delivery-control-rule-{label}");
    let other_rule = format!("logged-delivery-other-rule-{label}");
    let marker = format!("marker-{label}");

    let subject_arn = create_function(&router, case, &subject_function).await;
    let control_arn = create_function(&router, case, &control_function).await;

    let subject_rule_arn = create_rule(&router, case, &subject_rule, Some(&marker)).await;
    create_rule(&router, case, &control_rule, None).await;
    // A rule of the same bus that targets nothing: it is what the `OtherRule` row's grant is
    // scoped to, so that grant names a real rule which is not the invoking one.
    let other_rule_arn = create_rule(&router, case, &other_rule, Some("marker-elsewhere")).await;

    match row.grant {
        Grant::None => {}
        Grant::Unscoped => {
            add_permission(&router, case, &subject_function, INVOKE_ACTION, None).await;
        }
        Grant::ThisRule => {
            add_permission(
                &router,
                case,
                &subject_function,
                INVOKE_ACTION,
                Some(&subject_rule_arn),
            )
            .await;
        }
        Grant::OtherRule => {
            add_permission(
                &router,
                case,
                &subject_function,
                INVOKE_ACTION,
                Some(&other_rule_arn),
            )
            .await;
        }
        Grant::OtherAction => {
            add_permission(&router, case, &subject_function, OTHER_ACTION, None).await;
        }
    }
    // The control target is always granted the invocation unscoped, so its run is owed for
    // every publication this row makes.
    add_permission(&router, case, &control_function, INVOKE_ACTION, None).await;

    wire_target(&router, case, &subject_rule, &subject_arn).await;
    wire_target(&router, case, &control_rule, &control_arn).await;

    until_active(&router, case, &subject_function, deadline).await;
    until_active(&router, case, &control_function, deadline).await;

    let detail_type = if row.matched {
        DETAIL_TYPE
    } else {
        OTHER_DETAIL_TYPE
    };
    let published = events(
        &router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": detail_type,
                "Detail": json!({ "marker": marker }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the entry to be accepted, got {published}"
    );

    let wanted = expected_line(&marker);
    let control_group = log_group(&control_function);
    let subject_group = log_group(&subject_function);

    // The control's own line is what paces this row: once it is there, the dispatcher has
    // decided this publication and Lambda has run what it handed over.
    let control_lines = until_logged(&router, &control_group, &wanted, deadline).await;
    assert!(
        control_lines.iter().any(|line| line.contains(&wanted)),
        "case {case:?}: the control rule's target must run and log {wanted:?} in \
         {control_group:?}, got {control_lines:?}"
    );

    if row.invoked {
        let lines = until_logged(&router, &subject_group, &wanted, deadline).await;
        assert!(
            lines.iter().any(|line| line.contains(&wanted)),
            "case {case:?}: the permitted target's handler must log {wanted:?} in \
             {subject_group:?}, got {lines:?}"
        );
    } else {
        assert_nothing_logged(&router, case, &subject_group).await;
    }
}

/// RULE, the admitted arm: a matched rule's Lambda target is invoked where the FUNCTION's own
/// resource-based policy admits `events.amazonaws.com` the invoking action, and the handler's
/// own output about the event it was handed then reaches the function's log group.
#[tokio::test]
async fn a_target_granted_the_invocation_unscoped_logs_the_event_it_was_handed() {
    assert_row(&Row {
        case: "an unscoped grant, and an event the rule matches",
        label: "unscoped",
        grant: Grant::Unscoped,
        matched: true,
        invoked: true,
    })
    .await;
}

/// The same arm with the grant SCOPED to the delivery it admits: a statement whose
/// `aws:SourceArn` names the invoking rule admits that rule's hand-over.
#[tokio::test]
async fn a_target_granted_the_invocation_for_the_invoking_rule_logs_the_event() {
    assert_row(&Row {
        case: "a grant scoped by SourceArn to the invoking rule",
        label: "this-rule",
        grant: Grant::ThisRule,
        matched: true,
        invoked: true,
    })
    .await;
}

/// RULE, the refused arm: a function carrying no statement at all is never invoked, so its
/// group holds no line of its handler.
#[tokio::test]
async fn a_target_with_no_resource_policy_logs_nothing() {
    assert_row(&Row {
        case: "no statement on the function at all",
        label: "no-grant",
        grant: Grant::None,
        matched: true,
        invoked: false,
    })
    .await;
}

/// The same arm, scoped AWAY from this delivery: a statement whose `aws:SourceArn` names
/// another rule of the same bus does not admit this rule's hand-over.
#[tokio::test]
async fn a_target_granted_only_for_another_rule_logs_nothing() {
    assert_row(&Row {
        case: "a grant scoped by SourceArn to another rule of the same bus",
        label: "other-rule",
        grant: Grant::OtherRule,
        matched: true,
        invoked: false,
    })
    .await;
}

/// The same arm, naming another ACTION: it is the action the invocation performs that a
/// statement has to admit, not merely the presence of a statement for the principal.
#[tokio::test]
async fn a_target_granted_another_action_logs_nothing() {
    assert_row(&Row {
        case: "a statement naming another action for the same principal",
        label: "other-action",
        grant: Grant::OtherAction,
        matched: true,
        invoked: false,
    })
    .await;
}

/// RULE, the other way nothing is handed over: a grant admits a delivery, it does not make one,
/// so an event the rule's own pattern does not match leaves the granted target's group empty.
#[tokio::test]
async fn a_granted_target_of_a_rule_that_does_not_match_logs_nothing() {
    assert_row(&Row {
        case: "an unscoped grant, and an event the rule's pattern does not match",
        label: "unmatched",
        grant: Grant::Unscoped,
        matched: false,
        invoked: false,
    })
    .await;
}
