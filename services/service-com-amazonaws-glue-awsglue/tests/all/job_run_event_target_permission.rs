// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TARGET-PERMISSION boundary of the `Glue Job State Change` event a
//! job run publishes when it comes to rest — driven on the SERVICE'S OWN CLOCK, with no
//! further Glue call after the start.
//!
//! The rule under test: a job run that reaches a terminal `JobRunState` is published onto the
//! account's default EventBridge bus as `source` `aws.glue` / `detail-type` `Glue Job State
//! Change`, whose `detail` carries that run's own `jobName`, `jobRunId` and the `state` it
//! came to rest in — whichever state that is and whatever took it there. An ENABLED rule whose
//! `EventPattern` matches those values, INCLUDING a narrowing on `detail.jobName` and
//! `detail.state`, then delivers the envelope to each SQS target whose queue policy grants
//! `events.amazonaws.com` `sqs:SendMessage` for that rule, and to no other target: a sibling
//! queue that has granted EventBridge nothing receives none of it, and a rule whose narrowing
//! the run's state does not satisfy delivers nothing at all.
//!
//! What this suite pins beside [`crate::job_run_state_change_events`] — which drives the same
//! publication with the due instants moved THROUGH THE STORE and a pattern narrowed on nothing
//! but the envelope — is the two things a client that reacts to a run through EventBridge
//! depends on:
//!
//! * the run reaches its terminal state, and the event leaves, with NO further Glue call made
//!   after `StartJobRun`: the service's own clock owes both, so a client that polls only its
//!   queue observes the run;
//! * the queue policy that admits the delivery is the one such a client writes —
//!   `events.amazonaws.com` granted `sqs:SendMessage` under an `aws:SourceArn` condition naming
//!   the RULE — so the permitted and the unpermitted target are told apart by what each
//!   queue's own policy says.
//!
//! The recorded case — a `pythonshell` run whose script exits non-zero, matched by a rule
//! narrowed to that job and `FAILED`, delivered to the queue whose policy names the rule and
//! withheld from the one with no policy — is ONE row of the table below; the rows differ along
//! the state the run comes to rest in and why (a script that exits non-zero, a script that
//! exits cleanly) and whether the rule's narrowing admits that state at all.
//!
//! The role the runs are performed as is created through IAM's OWN API in the same engine, so
//! the admission of each run is decided against the records `CreateRole` and `PutRolePolicy`
//! wrote. Every wait here is a poll for the CONDITION it waits on against a generous deadline,
//! and nothing asserts on how long anything took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::state::{JOB_RUN_STATE_FAILED, JOB_RUN_STATE_SUCCEEDED};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_run_state_change_events::{given_queue, sqs};
use crate::job_script_execution::{create_bucket, stage_script, BUCKET, SCRIPTS};
use crate::wire::{ok, REGION};

/// The `source` and `detail-type` Glue publishes a job run's terminal state under, spelled out
/// here rather than imported, so a wrong constant in the implementation cannot make a test
/// pass.
const EVENT_SOURCE: &str = "aws.glue";
const JOB_STATE_CHANGE: &str = "Glue Job State Change";

/// A script that ends the program with a non-zero exit status, which is what a `pythonshell`
/// job whose own work failed does.
const EXITS_NON_ZERO: &str = r#"
import sys
sys.exit(1)
"#;

/// A script that exits cleanly, which is what a succeeding job does.
const EXITS_CLEANLY: &str = r#"
rows = [row for row in range(4)]
assert sum(rows) == 6, rows
"#;

/// The role every run here is performed as.
const ROLE_NAME: &str = "glue-job-event-target-role";

/// How long a delivery is waited for, and how often the queues are looked at.
///
/// The run's lifecycle and the publication are both the service's own clock's, so what is
/// waited for is the CONDITION (the envelope on the queue) against a deadline far longer than
/// the spans involved: a loaded or a fast host reaches the same verdict, and nothing here
/// asserts on how long it took.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);
const DELIVERY_POLL: Duration = Duration::from_millis(50);

/// An emulator with Glue, IAM, Amazon S3, EventBridge AND SQS on one shared engine.
///
/// IAM because each run is admitted against the role's own record, Amazon S3 because a run
/// reads its script from it, EventBridge and SQS because the publication goes onto the
/// account's default bus and a rule on that bus delivers it to a queue.
fn emulator_with_a_bus_and_iam() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = crate::wire::emulator_on(Arc::clone(&engine));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// One IAM `awsQuery` call, which must answer 200, as its body.
async fn iam(router: &MultiServiceRouter, action: &str, parameters: &str) -> String {
    let request = smithy_server_runtime::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDGLUEEVENTS/20260101/{REGION}/iam/aws4_request, \
                 SignedHeaders=host, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(format!(
            "Action={action}&Version=2010-05-08&{parameters}"
        )))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "IAM {action} must answer 200, got {status} with {body}"
    );
    body
}

/// Create, through IAM's own API, the role Glue performs these runs as: it trusts the Glue
/// service principal, and it may read the scripts the runs execute. Answers its ARN.
async fn given_role(router: &MultiServiceRouter) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "glue.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &format!("RoleName={ROLE_NAME}&AssumeRolePolicyDocument={trust}"),
    )
    .await;
    let reads_the_script = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["s3:GetObject", "s3:ListBucket"],
            "Resource": [
                format!("arn:aws:s3:::{BUCKET}"),
                format!("arn:aws:s3:::{BUCKET}/*"),
            ],
        }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &format!(
            "RoleName={ROLE_NAME}&PolicyName=reads-the-script&PolicyDocument={reads_the_script}"
        ),
    )
    .await;
    created
        .split("<Arn>")
        .nth(1)
        .and_then(|rest| rest.split("</Arn>").next())
        .unwrap_or_else(|| panic!("CreateRole must answer the role's Arn, got {created}"))
        .to_string()
}

/// Call one EventBridge operation, which must answer 200.
async fn events(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = smithy_server_runtime::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDGLUEEVENTS/20260101/{REGION}/events/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "EventBridge {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// Put an ENABLED rule matching `pattern` on the default bus, point it at `targets`, and answer
/// the rule's own ARN — the ARN a target's queue policy names as the source of the delivery.
async fn given_rule(
    router: &MultiServiceRouter,
    name: &str,
    pattern: Value,
    targets: &[String],
) -> String {
    let put = events(
        router,
        "PutRule",
        json!({ "Name": name, "EventPattern": pattern.to_string(), "State": "ENABLED" }),
    )
    .await;
    let rule_arn = put["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("PutRule must answer a RuleArn, got {put}"))
        .to_string();
    let entries: Vec<Value> = targets
        .iter()
        .enumerate()
        .map(|(index, arn)| json!({ "Id": format!("target-{index}"), "Arn": arn }))
        .collect();
    let pointed = events(
        router,
        "PutTargets",
        json!({ "Rule": name, "Targets": entries }),
    )
    .await;
    assert_eq!(
        pointed["FailedEntryCount"], json!(0),
        "PutTargets must accept every target, got {pointed}"
    );
    rule_arn
}

/// Grant `events.amazonaws.com` the `sqs:SendMessage` a rule's target needs on the queue at
/// `url`, conditioned on the delivery coming FROM `rule_arn` — the queue policy a client writes
/// for an EventBridge target.
async fn grant_delivery_from(
    router: &MultiServiceRouter,
    url: &str,
    queue_arn: &str,
    rule_arn: &str,
) {
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    })
    .to_string();
    sqs(
        router,
        "SetQueueAttributes",
        json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
}

/// Every envelope the queue at `url` holds, drained and deleted, so nothing read here can come
/// back when its visibility timeout lapses and be read as a second delivery.
async fn drain(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        "ReceiveMessage",
        json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("a delivered event must be JSON, got {body:?} ({error})")
        }));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                "DeleteMessage",
                json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
        }
    }
    envelopes
}

/// The ONE envelope delivered to the queue at `url`, waited for until it is there.
///
/// Nothing of Glue is called while waiting: the run's terminal state and its publication are
/// both owed by the service's own clock, so a client that only polls its queue observes them.
async fn delivered_envelope(router: &MultiServiceRouter, case: &str, url: &str) -> Value {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let envelopes = drain(router, url).await;
        if let Some(first) = envelopes.first() {
            assert_eq!(
                envelopes.len(),
                1,
                "case {case:?}: expected one delivered event, got {envelopes:?}"
            );
            return first.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: no event reached the permitted target queue before the deadline"
        );
        tokio::time::sleep(DELIVERY_POLL).await;
    }
}

/// One row: what the run's script does, the `state` the rule narrows its pattern to, and
/// whether the run the row starts therefore satisfies that rule.
struct Row {
    /// The case name every assertion of the row reports.
    case: &'static str,
    /// The script the job runs.
    script: &'static str,
    /// The `JobRunState` the run comes to rest in.
    rests_in: &'static str,
    /// The `detail.state` the rule's pattern admits.
    pattern_state: &'static str,
    /// Whether that narrowing admits the run's own state.
    delivered: bool,
}

/// The rows: the states a run of a `pythonshell` job comes to rest in, against a rule narrowed
/// to one of them.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a script that exits non-zero, under a rule narrowed to FAILED",
            script: EXITS_NON_ZERO,
            rests_in: JOB_RUN_STATE_FAILED,
            pattern_state: JOB_RUN_STATE_FAILED,
            delivered: true,
        },
        Row {
            case: "a script that exits cleanly, under a rule narrowed to SUCCEEDED",
            script: EXITS_CLEANLY,
            rests_in: JOB_RUN_STATE_SUCCEEDED,
            pattern_state: JOB_RUN_STATE_SUCCEEDED,
            delivered: true,
        },
        Row {
            case: "a script that exits non-zero, under a rule narrowed to SUCCEEDED",
            script: EXITS_NON_ZERO,
            rests_in: JOB_RUN_STATE_FAILED,
            pattern_state: JOB_RUN_STATE_SUCCEEDED,
            delivered: false,
        },
    ]
}

/// A run that comes to rest publishes its state change, and the rule that matches it delivers
/// the envelope to the target whose queue policy admits that rule — and to neither the sibling
/// target with no policy nor the targets of a rule whose narrowing the run's state does not
/// satisfy.
#[tokio::test]
async fn a_terminal_job_run_reaches_only_the_target_its_queue_policy_admits() {
    let router = emulator_with_a_bus_and_iam();
    create_bucket(&router).await;
    let role = given_role(&router).await;

    // The CONTROL rule narrows on nothing but the envelope, so every run's state change reaches
    // its queue. That is what bounds the negative halves below: a queue that is empty once the
    // control holds the same run's event is a queue the delivery was WITHHELD from, rather than
    // one the event has not reached yet.
    let (control_url, control_arn) = given_queue(&router, "glue-job-events-control").await;
    let control_rule = given_rule(
        &router,
        "glue-job-state-change-control",
        json!({ "source": [EVENT_SOURCE], "detail-type": [JOB_STATE_CHANGE] }),
        std::slice::from_ref(&control_arn),
    )
    .await;
    grant_delivery_from(&router, &control_url, &control_arn, &control_rule).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let job_name = format!("job_event_target_{index}");
        let location = stage_script(&router, &format!("{SCRIPTS}{job_name}.py"), row.script).await;

        // The two targets of the row's own rule: one whose policy admits that rule, one that has
        // granted EventBridge nothing at all.
        let (admitted_url, admitted_arn) =
            given_queue(&router, &format!("glue-job-events-admitted-{index}")).await;
        let (withheld_url, withheld_arn) =
            given_queue(&router, &format!("glue-job-events-withheld-{index}")).await;
        let rule_arn = given_rule(
            &router,
            &format!("glue-job-state-change-{index}"),
            json!({
                "source": [EVENT_SOURCE],
                "detail-type": [JOB_STATE_CHANGE],
                "detail": { "jobName": [job_name.clone()], "state": [row.pattern_state] },
            }),
            &[admitted_arn.clone(), withheld_arn],
        )
        .await;
        grant_delivery_from(&router, &admitted_url, &admitted_arn, &rule_arn).await;

        ok(
            &router,
            "CreateJob",
            &json!({
                "Name": job_name,
                "Role": role,
                "Command": {
                    "Name": "pythonshell",
                    "PythonVersion": "3.9",
                    "ScriptLocation": location,
                },
                "MaxCapacity": 0.0625,
                "Timeout": 5,
            }),
        )
        .await;
        let started = ok(&router, "StartJobRun", &json!({ "JobName": job_name })).await;
        let run_id = started["JobRunId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: StartJobRun must answer a JobRunId"))
            .to_string();

        // NOTHING of Glue is called from here on: the run's terminal state and the event that
        // reports it are both the service's own clock's work, so the control queue is what the
        // run is observed through.
        let control = delivered_envelope(&router, case, &control_url).await;
        assert_eq!(
            control["source"], json!(EVENT_SOURCE),
            "case {case:?}: expected source {EVENT_SOURCE:?}, got {control}"
        );
        assert_eq!(
            control["detail-type"], json!(JOB_STATE_CHANGE),
            "case {case:?}: expected detail-type {JOB_STATE_CHANGE:?}, got {control}"
        );
        let detail = control["detail"].clone();
        assert_eq!(
            detail["jobName"], json!(job_name),
            "case {case:?}: the detail must name the job, got {detail}"
        );
        assert_eq!(
            detail["jobRunId"], json!(run_id),
            "case {case:?}: the detail must name the run, got {detail}"
        );
        assert_eq!(
            detail["state"], json!(row.rests_in),
            "case {case:?}: expected the run to come to rest in {:?}, got {detail}",
            row.rests_in
        );

        if row.delivered {
            let admitted = delivered_envelope(&router, case, &admitted_url).await;
            assert_eq!(
                admitted["detail"], detail,
                "case {case:?}: the admitted target must receive the run's own state change, got \
                 {admitted}"
            );
        } else {
            let admitted = drain(&router, &admitted_url).await;
            assert!(
                admitted.is_empty(),
                "case {case:?}: a rule whose pattern the run's state does not satisfy delivers \
                 nothing, got {admitted:?}"
            );
        }

        // The sibling target granted EventBridge nothing, so the delivery to it was withheld
        // whether or not the rule matched.
        let withheld = drain(&router, &withheld_url).await;
        assert!(
            withheld.is_empty(),
            "case {case:?}: a target queue with no resource policy receives nothing, got \
             {withheld:?}"
        );
    }
}
