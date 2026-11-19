// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING pipe publishes ABOUT ITSELF, end to end over the wire, with IAM,
//! CloudWatch, Amazon SQS and Step Functions mounted beside pipes on ONE engine — so a
//! metric is asserted where a client reads it (`ListMetrics` and `GetMetricStatistics`)
//! and a stage refusal where a client sees it (the target queue holding nothing).
//!
//! The RULE asserted over the table below: each stage of a RUNNING pipe makes its call AS
//! THE PIPE'S EXECUTION ROLE, so a call the policies in force on that role do not allow is
//! that stage FAILING; and the pipe publishes its own counters to the
//! `AWS/EventBridge/Pipes` namespace dimensioned on `PipeName` at one-minute resolution,
//! each carrying the unit `None`, with the stage that failed counted on its own counter
//! and every counter that saw nothing EXISTING and reading 0 rather than being absent.
//! That 0 is the whole point: a pipe whose target refused it and a pipe that never ran
//! both leave the target empty, and only the counters tell them apart.
//!
//! The rows differ along what the rule generalises over — WHICH stage makes the call
//! (source, enrichment, target), and WHY the role does or does not bar it (a grant, an
//! explicit deny, policies stating nothing about the action, and a role the account does
//! not hold at all, which shows nothing and lets the call through).
//!
//! Every wait here LOOKS AGAIN until the condition it needs holds, on a generous deadline,
//! and every step that depends on an earlier one waits for the EVIDENCE of that step rather
//! than for a span it is assumed to fit in: a counter that moved is the pipe having run, and
//! a refused poll is a narrowed role having reached it. A counter expected to stay quiet is
//! an invariant, so it is looked at repeatedly across its span instead of once at the end of
//! one. Nothing here is asserted about elapsed time, so what a row reports is what the pipe
//! did and never how fast the host it ran on happened to be.
//!
//! A role whose TRUST document withholds the assume is not a row here: such a pipe never
//! reaches `RUNNING` at all, which is `tests/all/pipe_unassumable_role.rs`. Neither is a
//! role that may not take the SOURCE's records when the pipe is brought up: such a pipe is
//! never brought up either (`tests/all/pipe_bring_up.rs`), so the row about the source stage
//! failing withdraws that call from the role once the pipe is RUNNING — a live role being
//! revoked, which is the only way a running pipe's source poll is refused.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use service_com_amazonaws_pipes_pipes::state::execution_role::SERVICE_PRINCIPAL;
use service_com_amazonaws_pipes_pipes::state::metrics::{
    COUNTER_UNIT, ENRICHMENT_STAGE_FAILED_METRIC, EVENT_COUNT_METRIC, EXECUTION_FAILED_METRIC,
    METRIC_NAMESPACE, PIPE_COUNTERS, PIPE_NAME_DIMENSION, TARGET_STAGE_FAILED_METRIC,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::pipe_data_plane::{
    collect_at_least, collect_during, given_express_workflow, given_queue, rpc, send, LOOK_AGAIN,
    QUIET,
};
use crate::pipe_lifecycle::{call, REGION};

/// How long a wait for something the pipe has to do — a delivery, or a counter moving —
/// is given before the row is a failure.
///
/// Every wait below LOOKS AGAIN until the condition it needs holds, so this bounds only how
/// long a row the pipe never did the work for takes to fail, and a row whose work the pipe
/// did spends none of it. It is therefore generous rather than tight: a host that runs this
/// suite beside every other one is slow at everything, and a budget tuned to an idle host is
/// what makes a test report the host's load as the emulator's defect.
const BUDGET: Duration = Duration::from_secs(60);

/// An enrichment that hands back exactly what it was given, so a pipe that reaches its
/// enrichment delivers the records it polled.
const PASS_THROUGH_DEFINITION: &str = r#"{
  "StartAt": "PassThrough",
  "States": {
    "PassThrough": {
      "Type": "Pass",
      "End": true
    }
  }
}"#;

/// Pipes, plus every service a pipe's stages and its own metrics live in, on ONE engine —
/// the shape `core/router/src/lib.rs` mounts them in.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_pipes_pipes::EmulatedService::new(
        std::sync::Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        std::sync::Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(std::sync::Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router
}

/// One CloudWatch call, over the same awsJson path every other RPC here takes.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        "monitoring",
        "GraniteServiceVersion20100801",
        operation,
        body,
    )
    .await
}

/// Percent-encode one form value the way an AWS SDK encodes an `awsQuery` body.
///
/// `pub(crate)` because `tests/all/pipe_dead_letter_destination.rs` reaches another
/// `awsQuery` service (Amazon SNS) through the same encoding rather than a copy of it.
pub(crate) fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// One IAM call: IAM speaks `awsQuery`, so its parameters travel as a form body and its
/// answer comes back as XML.
pub(crate) async fn iam(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/iam/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, body)
}

/// The text of the first `<tag>` of an XML body.
pub(crate) fn xml_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

/// A trust policy naming one service principal, written as a client sends it.
pub(crate) fn trust_policy(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// An identity policy allowing `allowed` and denying `denied`, on every resource.
fn identity_policy(allowed: &[&str], denied: Option<&str>) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": allowed,
        "Resource": "*",
    })];
    if let Some(denied) = denied {
        statements.push(json!({
            "Effect": "Deny",
            "Action": denied,
            "Resource": "*",
        }));
    }
    Value::Array(statements)
}

/// A role called `name` trusting `principal` and carrying one inline policy over every
/// resource; answered with the ARN IAM persisted.
async fn given_role(
    router: &MultiServiceRouter,
    name: &str,
    principal: &str,
    allowed: &[&str],
    denied: Option<&str>,
) -> String {
    given_role_stating(router, name, principal, identity_policy(allowed, denied)).await
}

/// The name of the one inline policy [`given_role_stating`] writes, so a later write of it
/// REPLACES what the role may do.
pub(crate) const ROLE_POLICY_NAME: &str = "pipe-stages";

/// Leave the role `name` allowing `allowed` and nothing else: the permissions of a LIVE
/// role being narrowed, which is how a pipe that was brought up loses a call it could make
/// when it came up.
///
/// `pub(crate)` because `tests/all/pipe_metric_attribution.rs` narrows a running pipe's role
/// the same way, through this one path.
pub(crate) async fn withdraw_from_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    allowed: &[&str],
) {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": identity_policy(allowed, None),
    })
    .to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", ROLE_POLICY_NAME),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy answered {status}: {written}"
    );
}

/// A role called `name` trusting `principal` and carrying `statements` as its one inline
/// policy; answered with the ARN IAM persisted.
///
/// `pub(crate)` because `tests/all/pipe_execution_role.rs` writes its statements against
/// the queues of each row rather than over every resource, and reaches the SAME role
/// through this one path.
pub(crate) async fn given_role_stating(
    router: &MultiServiceRouter,
    name: &str,
    principal: &str,
    statements: Value,
) -> String {
    let document = trust_policy(principal);
    let (status, created) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &document)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name}: CreateRole answered {status}: {created}"
    );
    let arn =
        xml_text(&created, "Arn").unwrap_or_else(|| panic!("case {name}: no Arn in {created}"));
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", ROLE_POLICY_NAME),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name}: PutRolePolicy answered {status}: {written}"
    );
    arn
}

/// Epoch seconds, for the window a statistics read asks about.
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Every datapoint CloudWatch holds for one pipe's `metric`, read at the one-minute period
/// the series is published at.
///
/// `pub(crate)` because `tests/all/pipe_metric_attribution.rs` asks the SAME question of
/// several pipes running at once, and reads their series through this one path.
pub(crate) async fn datapoints(router: &MultiServiceRouter, pipe: &str, metric: &str) -> Vec<Value> {
    let now = epoch_seconds();
    let (status, stats) = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": METRIC_NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{ "Name": PIPE_NAME_DIMENSION, "Value": pipe }],
            "StartTime": now - 600.0,
            "EndTime": now + 600.0,
            "Period": 60,
            "Statistics": ["Sum", "SampleCount", "Maximum"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {pipe}: GetMetricStatistics for {metric} answered {status} {stats}"
    );
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// The `Sum` of every datapoint of one pipe's `metric`, and how many datapoints there
/// were: a series that EXISTS reads at least one datapoint, whatever it sums to.
pub(crate) async fn series(router: &MultiServiceRouter, pipe: &str, metric: &str) -> (usize, f64) {
    let points = datapoints(router, pipe, metric).await;
    let sum = points
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum::<f64>();
    (points.len(), sum)
}

/// Watch every counter of `pipe` the row did not expect to move for the whole of `watch`,
/// failing the row at the first look that finds one absent or reading anything but 0.
///
/// A counter that stays quiet is an invariant rather than something to wait for, so it is
/// LOOKED AT repeatedly across the span instead of once at the end of a sleep: a counter
/// that moves at any point in the span fails the row, whatever the host's speed decides
/// about when in the span that is.
///
/// `pub(crate)` because `tests/all/pipe_metric_attribution.rs` asks the same invariant of
/// every pipe of an account at once, and watches their quiet counters through this one
/// path rather than a copy of it.
pub(crate) async fn quiet_counters_hold(
    router: &MultiServiceRouter,
    case: &str,
    moved: &[&str],
    watch: Duration,
) {
    let deadline = Instant::now() + watch;
    loop {
        for metric in PIPE_COUNTERS {
            if moved.contains(metric) {
                continue;
            }
            let (points, held) = series(router, case, metric).await;
            assert!(
                points > 0 && held == 0.0,
                "case {case:?}: expected the quiet counter {metric} to hold at 0, got {held} \
                 over {points} datapoint(s)"
            );
        }
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Wait until one pipe's `metric` sums to at least `want`, answering what it summed to
/// last.
pub(crate) async fn wait_for_sum(
    router: &MultiServiceRouter,
    pipe: &str,
    metric: &str,
    want: f64,
) -> f64 {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (_, sum) = series(router, pipe, metric).await;
        if sum >= want || Instant::now() >= deadline {
            return sum;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Wait until every counter of `pipe` has a datapoint, answering the counter that was
/// still absent when the budget ran out.
///
/// `pub(crate)` because `tests/all/pipe_metric_attribution.rs` asks the same question of
/// several pipes running at once: a counter exists once the pipe it belongs to has opened
/// a window, which is something to WAIT for rather than to assume the host has got round
/// to.
pub(crate) async fn wait_for_every_counter(router: &MultiServiceRouter, pipe: &str) -> Option<&'static str> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let mut absent = None;
        for metric in PIPE_COUNTERS {
            let (points, _) = series(router, pipe, metric).await;
            if points == 0 {
                absent = Some(*metric);
                break;
            }
        }
        if absent.is_none() || Instant::now() >= deadline {
            return absent;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The dimension sets `ListMetrics` reports for one metric of this namespace.
pub(crate) async fn listed_dimension_sets(router: &MultiServiceRouter, metric: &str) -> Vec<Value> {
    let (status, listed) = cloudwatch(
        router,
        "ListMetrics",
        &json!({ "Namespace": METRIC_NAMESPACE }),
    )
    .await;
    assert_eq!(status, 200, "ListMetrics answered {status} {listed}");
    listed["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|series| series["MetricName"] == json!(metric))
        .map(|series| series["Dimensions"].clone())
        .collect()
}

/// One row of the table: which stage's call the role bars, and how.
struct Row {
    case: &'static str,
    /// The service principal the role's trust policy names, or `None` when the account
    /// holds no role at the ARN the pipe names at all.
    trust: Option<&'static str>,
    /// The actions the role's inline policy allows.
    allowed: &'static [&'static str],
    /// The action it explicitly denies, when the row has one.
    denied: Option<&'static str>,
    /// What the role is left allowing once the pipe is RUNNING, for a row whose stage must
    /// fail on a call the role allowed while the pipe was being brought up.
    withdrawn_to: Option<&'static [&'static str]>,
    /// Whether the pipe carries an enrichment between its source and its target.
    enrichment: bool,
    /// Whether the target ends up holding the record.
    delivered: bool,
    /// The counters expected to read at least 1. Every OTHER counter has to exist and
    /// read exactly 0.
    moved: &'static [&'static str],
}

/// RULE: every stage of a RUNNING pipe makes its call as the pipe's execution role, so a
/// call that role may not make is the stage failing — and the pipe publishes, for every
/// minute it runs, its counters in `AWS/EventBridge/Pipes` dimensioned on `PipeName` with
/// the unit `None`: the stage that failed on its own counter, and every counter that saw
/// nothing existing and reading 0.
#[tokio::test]
async fn a_running_pipe_publishes_its_counters_and_fails_the_stage_its_role_bars() {
    let rows = [
        Row {
            case: "every-stage-allowed",
            trust: Some(SERVICE_PRINCIPAL),
            allowed: &["sqs:*"],
            denied: None,
            withdrawn_to: None,
            enrichment: false,
            delivered: true,
            moved: &[EVENT_COUNT_METRIC],
        },
        Row {
            // A role the account does not hold is evidence of nothing — it may belong to
            // another account — so no stage is barred.
            case: "no-role-held-for-that-arn",
            trust: None,
            allowed: &[],
            denied: None,
            withdrawn_to: None,
            enrichment: false,
            delivered: true,
            moved: &[EVENT_COUNT_METRIC],
        },
        Row {
            case: "target-send-not-stated",
            trust: Some(SERVICE_PRINCIPAL),
            allowed: &["sqs:ReceiveMessage", "sqs:DeleteMessage"],
            denied: None,
            withdrawn_to: None,
            enrichment: false,
            delivered: false,
            moved: &[
                EVENT_COUNT_METRIC,
                EXECUTION_FAILED_METRIC,
                TARGET_STAGE_FAILED_METRIC,
            ],
        },
        Row {
            case: "target-send-explicitly-denied",
            trust: Some(SERVICE_PRINCIPAL),
            allowed: &["sqs:*"],
            denied: Some("sqs:SendMessage"),
            withdrawn_to: None,
            enrichment: false,
            delivered: false,
            moved: &[
                EVENT_COUNT_METRIC,
                EXECUTION_FAILED_METRIC,
                TARGET_STAGE_FAILED_METRIC,
            ],
        },
        Row {
            // The source poll is the first call a pipe makes, so a role that may not make
            // it takes no record at all: the execution failed before any stage of its own.
            // The role is narrowed to the delivery once the pipe is RUNNING, because a pipe
            // whose role may not take its source's records when it is brought up is never
            // brought up (`tests/all/pipe_bring_up.rs`) and would publish nothing.
            case: "source-receive-withdrawn-while-running",
            trust: Some(SERVICE_PRINCIPAL),
            allowed: &["sqs:*"],
            denied: None,
            withdrawn_to: Some(&["sqs:SendMessage"]),
            enrichment: false,
            delivered: false,
            moved: &[EXECUTION_FAILED_METRIC],
        },
        Row {
            case: "enrichment-invocation-not-stated",
            trust: Some(SERVICE_PRINCIPAL),
            allowed: &["sqs:*"],
            denied: None,
            withdrawn_to: None,
            enrichment: true,
            delivered: false,
            moved: &[
                EVENT_COUNT_METRIC,
                EXECUTION_FAILED_METRIC,
                ENRICHMENT_STAGE_FAILED_METRIC,
            ],
        },
        Row {
            case: "enrichment-and-every-stage-allowed",
            trust: Some(SERVICE_PRINCIPAL),
            allowed: &["sqs:*", "states:StartSyncExecution"],
            denied: None,
            withdrawn_to: None,
            enrichment: true,
            delivered: true,
            moved: &[EVENT_COUNT_METRIC],
        },
    ];

    for row in rows {
        let router = emulator();
        let (source_url, source_arn) = given_queue(&router, &format!("{}-source", row.case)).await;
        let (target_url, target_arn) = given_queue(&router, &format!("{}-target", row.case)).await;
        let role_arn = match row.trust {
            Some(principal) => {
                given_role(
                    &router,
                    &format!("{}-role", row.case),
                    principal,
                    row.allowed,
                    row.denied,
                )
                .await
            }
            // A syntactically good role ARN of another account, which this account holds
            // no record of.
            None => format!("arn:aws:iam::000000000000:role/{}-absent", row.case),
        };
        let mut body = json!({
            "Source": source_arn,
            "Target": target_arn,
            "RoleArn": role_arn,
            "SourceParameters": { "SqsQueueParameters": { "BatchSize": 1 } },
        });
        if row.enrichment {
            let enrichment = given_express_workflow(
                &router,
                &format!("{}-enrichment", row.case),
                PASS_THROUGH_DEFINITION,
            )
            .await;
            body["Enrichment"] = json!(enrichment);
        }
        let created = call(
            &router,
            "POST",
            &format!("/v1/pipes/{}", row.case),
            Some(body),
            REGION,
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: CreatePipe answered {} {}",
            row.case, created.status, created.body
        );
        // A row whose stage must fail on a call its role allowed while the pipe was being
        // brought up waits for the pipe to be RUNNING and then narrows the role, so the
        // refusal reaches a pipe that is already running rather than its creation.
        if let Some(allowed) = row.withdrawn_to {
            let reported = crate::pipe_execution_role::wait_for_state(
                &router,
                row.case,
                service_com_amazonaws_pipes_pipes::state::pipe_state::RUNNING,
                BUDGET,
            )
            .await;
            assert_eq!(
                reported,
                service_com_amazonaws_pipes_pipes::state::pipe_state::RUNNING,
                "case {:?}: expected the pipe RUNNING before its role is narrowed, it \
                 reports {reported}",
                row.case
            );
            withdraw_from_role(
                &router,
                row.case,
                &format!("{}-role", row.case),
                allowed,
            )
            .await;
            // And the record is sent only once the pipe has ALREADY been refused a poll
            // under the narrowed role, which its own failure counter is the report of. The
            // poller takes one pass at a time, so a pass that was refused is a pass no
            // earlier poll of that source outlived: the record cannot then be taken by a
            // receive the wider role authorized. Waiting for that refusal — rather than
            // sending straight away and trusting the narrowing to land first — is what
            // keeps the row from turning on how fast the host runs.
            let refused =
                wait_for_sum(&router, row.case, EXECUTION_FAILED_METRIC, 1.0).await;
            assert!(
                refused >= 1.0,
                "case {:?}: expected the pipe to be refused a source poll once its role no \
                 longer allows one, its {EXECUTION_FAILED_METRIC} reads {refused}",
                row.case
            );
        }
        // What each counter the row expects to move reads BEFORE the record is sent: 0 for
        // every row but the one whose role was narrowed above, whose failure counter the
        // narrowing itself has already driven. The wait below is for the pipe moving each
        // counter FURTHER than that, so no row's wait is a condition that already held.
        let mut before: Vec<f64> = Vec::with_capacity(row.moved.len());
        for metric in row.moved {
            let (_, sum) = series(&router, row.case, metric).await;
            before.push(sum);
        }
        send(&router, &source_url, &[format!("{}-record", row.case)]).await;

        // Every counter the row expects to move, read as a client reads it — waited for
        // before the target is read, because a counter that moved past what it read before
        // the send is a pass the pipe took AFTER it: it is what makes the reads below about
        // a pipe that did its work rather than about one the host has not got round to yet.
        for (metric, read_before) in row.moved.iter().zip(before) {
            let want = read_before + 1.0;
            let sum = wait_for_sum(&router, row.case, metric, want).await;
            assert!(
                sum >= want,
                "case {:?}: expected {metric} to read at least {want} once the pipe has taken \
                 a pass over the record it was sent (it read {read_before} before that), got \
                 {sum}",
                row.case
            );
        }

        // What a client sees at the target: the record, or nothing at all.
        if row.delivered {
            let delivered = collect_at_least(&router, &target_url, 1, BUDGET).await;
            assert_eq!(
                delivered.len(),
                1,
                "case {:?}: expected the target to hold the one record the pipe moved, got {delivered:?}",
                row.case
            );
        } else {
            let delivered = collect_during(&router, &target_url, QUIET).await;
            assert!(
                delivered.is_empty(),
                "case {:?}: expected the target to hold nothing, got {delivered:?}",
                row.case
            );
        }

        // And every counter at all: a quiet one EXISTS and reads 0 rather than being
        // absent, which is what tells this pipe from one that never ran.
        let absent = wait_for_every_counter(&router, row.case).await;
        assert!(
            absent.is_none(),
            "case {:?}: expected every counter of a running pipe to have a datapoint, {absent:?} had none",
            row.case
        );
        for metric in PIPE_COUNTERS {
            let points = datapoints(&router, row.case, metric).await;
            for point in &points {
                assert_eq!(
                    point["Unit"],
                    json!(COUNTER_UNIT),
                    "case {:?}: expected {metric} to be published in {COUNTER_UNIT}, got {point}",
                    row.case
                );
            }
            if row.moved.contains(metric) {
                continue;
            }
            let sum = points
                .iter()
                .filter_map(|point| point["Sum"].as_f64())
                .sum::<f64>();
            assert_eq!(
                sum, 0.0,
                "case {:?}: expected the quiet counter {metric} to read 0, got {sum}",
                row.case
            );
        }

        // And each quiet counter HOLDS at 0: nothing of its kind happens later either,
        // however long the pipe goes on running — looked at again and again across the
        // span, so a counter that moves anywhere in it fails the row.
        quiet_counters_hold(&router, row.case, row.moved, QUIET).await;

        // The catalogue a dashboard builds from names this pipe's dimension set for every
        // counter it publishes.
        for metric in PIPE_COUNTERS {
            let sets = listed_dimension_sets(&router, metric).await;
            assert!(
                sets.contains(&json!([{ "Name": PIPE_NAME_DIMENSION, "Value": row.case }])),
                "case {:?}: expected ListMetrics to name the {PIPE_NAME_DIMENSION} dimension \
                 set of {metric}, got {sets:?}",
                row.case
            );
        }
    }
}
