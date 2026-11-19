// SPDX-License-Identifier: CC0-1.0

//! A MediaConvert job's state changes reach an EventBridge rule's Amazon SQS target on the
//! emulator a client STARTS: the built binary, on its own port, with request authentication on,
//! reached over TLS with SigV4-signed calls — and with NO MediaConvert call made after the
//! submit.
//!
//! The RULE under test: **once a job has begun processing, every status it moves into is
//! published onto the account's default EventBridge bus as `source` `aws.mediaconvert` /
//! `detail-type` `MediaConvert Job State Change`, naming the job's ARN in `resources` and, in
//! `detail`, that job's own id and the status it moved into — each move announced once, and
//! announced because the move HAPPENED rather than because somebody asked about the job. A job
//! that never begins is not a run: one its `PAUSED` queue is holding, cancelled out of the queue
//! before it ever began, publishes nothing at all.** Sources:
//! `CreateJob$StatusUpdateInterval`, "how often MediaConvert sends STATUS_UPDATE events to
//! Amazon CloudWatch Events", which is the model's own statement that this service publishes a
//! job's progress, and `QueueStatus`, "if you pause a queue, jobs in that queue won't begin",
//! for the half that publishes nothing.
//!
//! [`crate::mediaconvert_job_state_change_reaches_its_queue_target`] pins that rule over a
//! router assembled inside the test process. The emulator a client runs is a different
//! configuration of the same code: the clock that moves the job runs on the binary's own
//! multi-thread runtime with no request waiting on it, the publication MediaConvert makes is
//! signed with the credentials THAT process vends, EventBridge's delivery is authorized by the
//! queue's own policy, and the consumer reaches the queue over TLS with long-polling receives.
//! This suite is the one that pins the chain THERE, so a job event that is published in-process
//! and not on the started emulator is a failure a test reports rather than one only a client
//! sees.
//!
//! The legs span the two halves of the rule: the job that RUNS, whose two moves are read off the
//! queue, and the job its `PAUSED` queue held until a cancel took it out, whose silence is read
//! against a witness job submitted after it — once the witness's own run has been announced, a
//! held job's event has either arrived too or is never coming. The run that was already announced
//! is shown over that same window to be announced NO further, so each move is announced once.
//!
//! Starting the binary costs what starting the emulator costs, so this suite does not pay for a
//! start of its own: it is a LEG of [`crate::cfn_deployed_pipeline_on_a_started_emulator`], run
//! on the emulator that suite starts, exactly as the S3-notification and Auto Scaling legs are.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;

/// The Region the started emulator's calls are signed for, which is the Region the jobs belong
/// to and the Region their events are published in. It must stay the one [`Started`] signs with.
const REGION: &str = "us-east-1";

/// What the envelope says about itself, spelled out here rather than imported from the service
/// crate, so a wrong constant in the implementation cannot make this suite pass.
const EVENT_SOURCE: &str = "aws.mediaconvert";
const JOB_STATE_CHANGE: &str = "MediaConvert Job State Change";

/// The service principal EventBridge delivers to a queue target as, which is what the queue's
/// own resource policy has to allow ("for Amazon SQS queues, EventBridge relies on
/// resource-based policies").
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The IAM role the documented workflow creates for MediaConvert, and the role ARN the jobs
/// carry. MediaConvert stores it on the job; it decides nothing about the events.
const ROLE_NAME: &str = "StartedEmulatorMediaConvertRole";

/// The status a job moves into when it has BEGUN processing, which is the first event of a run.
const BEGUN: &str = "PROGRESSING";

/// The statuses a RUN ends in, which is where a job's event stream stops.
///
/// `JobStatus` is "a job's status can be SUBMITTED, PROGRESSING, COMPLETE, CANCELED, or ERROR",
/// of which `SUBMITTED` is where a submit leaves a job and `CANCELED` is what a cancel writes —
/// so a run that was neither held nor cancelled ends in one of these two. WHICH one is the
/// service's own to decide, so the legs below assert the SHAPE of the stream and then its
/// agreement with `GetJob`, never one of the two literals.
const RUN_ENDS_IN: [&str; 2] = ["COMPLETE", "ERROR"];

/// The MediaConvert queues the legs submit to.
const ACTIVE_QUEUE: &str = "started-emulator-active";
const HELD_QUEUE: &str = "started-emulator-held";

/// The SQS queue the rule delivers to.
const TARGET_QUEUE: &str = "started-emulator-job-events";

/// How long a leg waits for the events it expects, and how often it looks.
///
/// Generous on purpose: the wait is on the EVENTS, looked for again until they are there, so a
/// fast machine never waits the whole span and a loaded one is given room to spend.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How long one long-polling receive asks the queue to hold the call open for, in seconds.
///
/// The documented consumer's own value: "the duration (in seconds) for which the call waits for
/// a message to arrive in the queue before returning" (`ReceiveMessage$WaitTimeSeconds`), so a
/// receive returns the moment an event arrives, and a receive that returns EMPTY has waited on
/// the queue itself rather than on a clock of this suite's own.
const WAIT_TIME_SECONDS: i64 = 10;

/// One `X-Amz-Target` RPC call over the started emulator's signed-call plumbing, which must
/// answer 200, as its parsed body.
fn rpc(
    emulator: &Started,
    case: &str,
    signing_name: &str,
    target: &str,
    content_type: &str,
    body: &Value,
) -> Value {
    let (status, text) = emulator.rpc(signing_name, target, content_type, body);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {target}, got {status} with {text}; the \
         emulator's log:\n{}",
        emulator.log()
    );
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// One EventBridge call (`awsJson1_1`), which must answer 200.
fn events(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        case,
        "events",
        &format!("AWSEvents.{operation}"),
        "application/x-amz-json-1.1",
        body,
    )
}

/// One Amazon SQS call (`awsJson1_0`), which must answer 200.
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        case,
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    )
}

/// One MediaConvert call over its REST wire, as the status it answered and its parsed body.
fn mediaconvert(
    emulator: &Started,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let (status, text) = emulator.call(
        "mediaconvert",
        method,
        path,
        &[("content-type", "application/json")],
        payload.into_bytes(),
    );
    let parsed = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A string member of a response, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// The target queue, its policy admitting EventBridge, and the rule that delivers every
/// MediaConvert job event to it: the queue's url.
///
/// The rule is created BEFORE any job, since an event published while the bus carries no matching
/// rule reaches no target.
fn wired_queue(emulator: &Started, case: &str) -> String {
    let created = sqs(
        emulator,
        case,
        "CreateQueue",
        &json!({ "QueueName": TARGET_QUEUE }),
    );
    let url = member(case, &created, "/QueueUrl");
    // The queue's ARN comes off Amazon SQS itself rather than being spelled here, so the target
    // names the queue the way its owner does.
    let attributes = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    );
    let arn = member(case, &attributes, "/Attributes/QueueArn");
    let rule = format!("{TARGET_QUEUE}-rule");
    let put = events(
        emulator,
        case,
        "PutRule",
        &json!({
            "Name": &rule,
            "EventPattern": json!({
                "source": [EVENT_SOURCE],
                "detail-type": [JOB_STATE_CHANGE],
            })
            .to_string(),
            "State": "ENABLED",
        }),
    );
    let rule_arn = member(case, &put, "/RuleArn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": &arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": &rule_arn } },
        }],
    })
    .to_string();
    sqs(
        emulator,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    );
    let targeted = events(
        emulator,
        case,
        "PutTargets",
        &json!({
            "Rule": &rule,
            "Targets": [{ "Id": "sqs-target", "Arn": &arn }],
        }),
    );
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: the queue target must be accepted, got {targeted}"
    );
    url
}

/// The IAM role a MediaConvert job is submitted with, as the documented workflow creates it:
/// answers its ARN.
fn mediaconvert_role(emulator: &Started, case: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "mediaconvert.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, body) = emulator.form_call(
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust),
        ],
    );
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole must succeed, got {status} with {body}"
    );
    crate::query_wire::element(&body, "Arn")
}

/// Create the MediaConvert queue `name` with `status`.
fn given_job_queue(emulator: &Started, case: &str, name: &str, status: &str) {
    let (code, body) = mediaconvert(
        emulator,
        "POST",
        "/2017-08-29/queues",
        Some(&json!({ "name": name, "status": status })),
    );
    assert_eq!(
        code, 201,
        "case {case:?}: CreateQueue must succeed, got {code} with {body}"
    );
}

/// Submit one job to `queue` and answer the id MediaConvert minted for it.
fn submit(emulator: &Started, case: &str, role: &str, queue: &str, index: usize) -> String {
    let body = json!({
        "role": role,
        "queue": queue,
        "settings": {
            "inputs": [{ "fileInput": format!("s3://example-bucket/in/{index}.mp4") }],
            "outputGroups": [{
                "name": "File Group",
                "outputs": [{
                    "nameModifier": "_out",
                    "containerSettings": { "container": "MP4" },
                }],
                "outputGroupSettings": {
                    "type": "FILE_GROUP_SETTINGS",
                    "fileGroupSettings": { "destination": "s3://example-bucket/out/" },
                },
            }],
        },
    });
    let (code, answered) = mediaconvert(emulator, "POST", "/2017-08-29/jobs", Some(&body));
    assert_eq!(
        code, 201,
        "case {case:?}: CreateJob must succeed, got {code} with {answered}"
    );
    // The span a job waits is never zero, so the submit itself still answers the status a client
    // acting straight after it observes.
    assert_eq!(
        answered.pointer("/job/status").and_then(Value::as_str),
        Some("SUBMITTED"),
        "case {case:?}: CreateJob must answer SUBMITTED, got {answered}"
    );
    member(case, &answered, "/job/id")
}

/// Every event waiting on the queue at `url`, parsed — and DELETED, the way a consumer that has
/// handled a message does, with the long-polling receive the documented consumer uses.
fn drain(emulator: &Started, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        emulator,
        case,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": WAIT_TIME_SECONDS,
        }),
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut events = Vec::new();
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            );
        }
        if let Some(body) = message["Body"].as_str() {
            events.push(serde_json::from_str(body).unwrap_or(Value::Null));
        }
    }
    events
}

/// The events among `seen` that are about the job `id`, oldest move first.
///
/// Ordered by the instant the move happened (`detail.timestamp`), which is the job's own order of
/// moves rather than the order the target queue happened to hand the messages over.
fn events_for(seen: &[Value], id: &str) -> Vec<Value> {
    let mut own: Vec<Value> = seen
        .iter()
        .filter(|event| event["detail"]["jobId"] == json!(id))
        .cloned()
        .collect();
    own.sort_by_key(|event| event["detail"]["timestamp"].as_i64().unwrap_or_default());
    own
}

/// The `detail.status` of each of `events`, in order.
fn statuses_of(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            event["detail"]["status"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Drain the queue at `url` into `seen` until `enough` holds or [`DELIVERY_DEADLINE`] passes.
///
/// The CONDITION is what is waited for, not a span, and no MediaConvert call is made while
/// waiting: this is the client that only listens.
fn collect_until(
    emulator: &Started,
    case: &str,
    url: &str,
    seen: &mut Vec<Value>,
    enough: impl Fn(&[Value]) -> bool,
) {
    let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
    loop {
        seen.extend(drain(emulator, case, url));
        if enough(seen) || Instant::now() >= giving_up_at {
            return;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Whether `statuses` is the whole stream of a job whose run has ENDED: it began, and then
/// stopped in one of the statuses a run ends in.
fn ran(statuses: &[String]) -> bool {
    statuses.len() == 2
        && statuses.first().map(String::as_str) == Some(BEGUN)
        && statuses
            .last()
            .is_some_and(|status| RUN_ENDS_IN.contains(&status.as_str()))
}

/// Whether a run's terminal status has been announced among `statuses`, which is what bounds a
/// wait on the queue.
fn ended(statuses: &[String]) -> bool {
    statuses
        .iter()
        .any(|status| RUN_ENDS_IN.contains(&status.as_str()))
}

/// The whole rule on the emulator a client starts: a job's run is announced on the queue with
/// nobody asking MediaConvert about it, each move announced once, and a job that never began is
/// never announced at all.
///
/// A LEG of [`crate::cfn_deployed_pipeline_on_a_started_emulator`], run on the emulator that
/// suite starts rather than on a second one of its own.
pub(crate) fn a_jobs_state_changes_reach_its_rules_queue_on_the_emulator_a_client_starts(
    emulator: &Started,
) {
    let case = "a job's run announced on the emulator a client starts";

    let url = wired_queue(emulator, case);
    let role = mediaconvert_role(emulator, case);
    given_job_queue(emulator, case, ACTIVE_QUEUE, "ACTIVE");
    given_job_queue(emulator, case, HELD_QUEUE, "PAUSED");

    // Leg one: the job that RUNS. After this submit not one MediaConvert call is made until the
    // whole run has been read off the queue.
    let running = submit(emulator, case, &role, ACTIVE_QUEUE, 0);
    let mut seen: Vec<Value> = Vec::new();
    collect_until(emulator, case, &url, &mut seen, |seen| {
        ended(&statuses_of(&events_for(seen, &running)))
    });
    let announced = events_for(&seen, &running);
    assert!(
        ran(&statuses_of(&announced)),
        "case {case:?}: the job {running}'s own run must be announced — {BEGUN} and then one of \
         {RUN_ENDS_IN:?} — got {announced:?}; the emulator's log:\n{}",
        emulator.log()
    );
    for event in &announced {
        assert_eq!(
            event["source"],
            json!(EVENT_SOURCE),
            "case {case:?}: expected source {EVENT_SOURCE}, got {event}"
        );
        assert_eq!(
            event["detail-type"],
            json!(JOB_STATE_CHANGE),
            "case {case:?}: expected detail-type {JOB_STATE_CHANGE}, got {event}"
        );
        // The event names the resource it is about, which is what a rule filtering on
        // `resources` reads.
        assert_eq!(
            event["resources"],
            json!([format!(
                "arn:aws:mediaconvert:{REGION}:{}:jobs/{running}",
                engine::EMULATOR_ACCOUNT_ID
            )]),
            "case {case:?}: expected the job's own ARN in resources, got {event}"
        );
    }

    // Leg two: the job its PAUSED queue holds, cancelled out of the queue before it ever began.
    // It publishes nothing — read against a WITNESS job submitted after it, whose own run the
    // clock is bound to announce.
    let held = submit(emulator, case, &role, HELD_QUEUE, 1);
    let (code, body) = mediaconvert(emulator, "DELETE", &format!("/2017-08-29/jobs/{held}"), None);
    assert_eq!(
        code, 202,
        "case {case:?}: CancelJob must succeed, got {code} with {body}"
    );
    let witness = submit(emulator, case, &role, ACTIVE_QUEUE, 2);
    let mut later: Vec<Value> = Vec::new();
    collect_until(emulator, case, &url, &mut later, |seen| {
        ended(&statuses_of(&events_for(seen, &witness)))
    });
    // One more look, so an event delivered BESIDE the witness's last is counted here rather than
    // left on the queue.
    later.extend(drain(emulator, case, &url));
    let witnessed = events_for(&later, &witness);
    assert!(
        ran(&statuses_of(&witnessed)),
        "case {case:?}: the witness job {witness} must have its own run announced, got \
         {witnessed:?}; the emulator's log:\n{}",
        emulator.log()
    );
    assert!(
        events_for(&later, &held).is_empty(),
        "case {case:?}: the job {held} never began, so nothing may be announced for it, got {:?}",
        events_for(&later, &held)
    );
    // ...and the run announced in leg one is announced NO further over that same window, so each
    // move is announced once rather than for as long as somebody keeps listening.
    assert!(
        events_for(&later, &running).is_empty(),
        "case {case:?}: the job {running}'s run is over, so nothing more may be announced for \
         it, got {:?}",
        events_for(&later, &running)
    );

    // Only NOW is MediaConvert asked about the jobs: each reports the status its OWN stream ended
    // on — or, for the job that never began, the status that explains the silence — so the event
    // stream and the control plane agree.
    for (id, stream) in [
        (&running, Some(&announced)),
        (&held, None),
        (&witness, Some(&witnessed)),
    ] {
        let (code, body) = mediaconvert(emulator, "GET", &format!("/2017-08-29/jobs/{id}"), None);
        assert_eq!(
            code, 200,
            "case {case:?}: GetJob must succeed, got {code} with {body}"
        );
        let expected = match stream {
            // The status the LAST event of that job's stream carried, which is the status the
            // control plane must stand at.
            Some(stream) => stream
                .last()
                .and_then(|event| event["detail"]["status"].as_str())
                .unwrap_or_default()
                .to_string(),
            // A job a cancel took out of its queue before it began: nothing was announced, and
            // `CancelJob` is what wrote the status it reports.
            None => "CANCELED".to_string(),
        };
        assert_eq!(
            body.pointer("/job/status").and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: the job {id} must report {expected}, got {body}"
        );
    }
}
