// SPDX-License-Identifier: CC0-1.0

//! MediaConvert publishes a `MediaConvert Job State Change` event when a job moves.
//!
//! The RULE under test: a job that has BEGUN processing has every status it moves into
//! published onto the account's DEFAULT EventBridge bus as `source` `aws.mediaconvert` /
//! `detail-type` `MediaConvert Job State Change`, whose `detail` carries that job's own
//! `jobId` and the `status` it moved into (plus the queue it ran in and, when the record
//! carries them, the error code and message), and whose `resources` names the job's ARN;
//! EventBridge then routes it, so a rule whose `EventPattern` matches delivers it to that
//! rule's targets and a rule that does not match receives nothing. A job that never began
//! — one cancelled while it was still waiting in a paused queue — publishes nothing at
//! all. Source: `CreateJob$StatusUpdateInterval`, "how often MediaConvert sends
//! STATUS_UPDATE events to Amazon CloudWatch Events", which is the model's own statement
//! that this service is an event source, and the recorded behaviour of the event itself.
//!
//! The rows span what the rule generalises over: WHICH door the job is asked for through
//! once its transition is due (one job by id, the whole job set, a search, a queue
//! projection whose counts come off the job statuses), whether the job began at all, and
//! — the negative half of the delivery contract — whether the target queue has granted
//! EventBridge the `sqs:SendMessage` its resource-based policy needs. The transition itself
//! is the CLOCK's, materialised by whichever comes first, the store's own clock pump or the
//! door below; a transition is published exactly once either way, which is what
//! [`a_transition_is_published_once_however_often_the_job_is_read`] pins, and that the
//! event needs no later door at all is `job_events_without_a_later_call`'s own suite.
//!
//! EventBridge and SQS are mounted BESIDE MediaConvert on the SAME engine, so the
//! publication travels the real in-process callback, through the real bus, to a real SQS
//! target: nothing here reads the publisher's own queue.

use serde_json::{json, Value};
use service_com_amazonaws_mediaconvert_mediaconvert::ComAmazonawsMediaconvertMediaconvertState;

use super::job_lifecycle::{
    arn, queue_arn, settings as job_settings, Emulator, ROLE, SHORT_SETTLE,
};

/// The `source` and `detail-type` MediaConvert publishes its job events under, spelled out
/// here rather than imported, so a wrong constant in the implementation cannot make a test
/// pass.
pub(crate) const EVENT_SOURCE: &str = "aws.mediaconvert";
pub(crate) const JOB_STATE_CHANGE: &str = "MediaConvert Job State Change";

/// The rule that matches a MediaConvert job event, and the rule that cannot.
pub(crate) const MATCHING_RULE: &str = "mediaconvert-job-state-change";
const OTHER_RULE: &str = "unrelated-events";

/// How long a case waits for the events it expects to be DELIVERED before it gives up and
/// asserts on what arrived. Generous: a deadline for a machine under load, not a span
/// anything is expected to take.
const DELIVERY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long one poll of the target queue pauses before draining it again.
const DELIVERY_POLL_PAUSE: std::time::Duration = std::time::Duration::from_millis(20);

/// The terminal status a job of this emulator reaches, which is the status the published
/// event carries.
const TERMINAL_STATUS: &str = "ERROR";

/// Call one EventBridge operation the way a client does: `awsJson1_1`, `POST /` with
/// `X-Amz-Target: AWSEvents.<Operation>` and an `events`-scoped credential.
async fn events(emulator: &Emulator, operation: &str, body: Value) -> (u16, Value) {
    emulator
        .rpc("events", "AWSEvents", "1.1", operation, body)
        .await
}

/// Call one SQS operation the way a client does: `awsJson1_0`, `POST /` with
/// `X-Amz-Target: AmazonSQS.<Operation>` and an `sqs`-scoped credential.
async fn sqs(emulator: &Emulator, operation: &str, body: Value) -> (u16, Value) {
    emulator.rpc("sqs", "AmazonSQS", "1.0", operation, body).await
}

/// An SQS queue called `name`, created through SQS itself; answers its url and the ARN a
/// rule names it by.
pub(crate) async fn given_queue(emulator: &Emulator, case: &str, name: &str) -> (String, String) {
    let (status, body) = sqs(emulator, "CreateQueue", json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue answered {status} {body}"
    );
    let url = body["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {body}"))
        .to_string();
    let (status, body) = sqs(
        emulator,
        "GetQueueAttributes",
        json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes answered {status} {body}"
    );
    let queue_arn = body["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {body}"))
        .to_string();
    (url, queue_arn)
}

/// Grant `events.amazonaws.com` the `sqs:SendMessage` a rule's target needs on the queue
/// at `url`, for events the rule `rule_arn` delivers — EventBridge makes that call as its
/// own service principal and "relies on resource-based policies" for an SQS target, so a
/// queue that has granted it nothing receives nothing.
pub(crate) async fn grant_delivery(
    emulator: &Emulator,
    case: &str,
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
    let (status, body) = sqs(
        emulator,
        "SetQueueAttributes",
        json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetQueueAttributes answered {status} {body}"
    );
}

/// Put the rule `name` with `pattern` and point it at the SQS queue `queue_arn`; answers
/// the rule's ARN.
pub(crate) async fn given_rule(
    emulator: &Emulator,
    case: &str,
    name: &str,
    pattern: Value,
    queue_arn: &str,
) -> String {
    let (status, body) = events(
        emulator,
        "PutRule",
        json!({ "Name": name, "EventPattern": pattern.to_string(), "State": "ENABLED" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}/{name}: PutRule answered {status} {body}"
    );
    let rule_arn = body["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}/{name}: no RuleArn in {body}"))
        .to_string();
    let (status, body) = events(
        emulator,
        "PutTargets",
        json!({ "Rule": name, "Targets": [{ "Id": "sqs-target", "Arn": queue_arn }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}/{name}: PutTargets answered {status} {body}"
    );
    assert_eq!(
        body["FailedEntryCount"],
        json!(0),
        "case {case:?}/{name}: PutTargets reported a failed entry: {body}"
    );
    rule_arn
}

/// Every message waiting on the queue at `url`, as parsed JSON event envelopes.
pub(crate) async fn drain(emulator: &Emulator, case: &str, url: &str) -> Vec<Value> {
    let (status, body) = sqs(
        emulator,
        "ReceiveMessage",
        json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage answered {status} {body}"
    );
    body["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            let raw = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(raw).unwrap_or(Value::Null)
        })
        .collect()
}

/// The events among `drained` that are about the job `job_id`, oldest move first.
///
/// Ordered by the instant the move happened (`detail.timestamp`), which is the job's own
/// order of moves rather than the order the target queue happened to hand the messages
/// over.
pub(crate) fn events_for(drained: &[Value], job_id: &str) -> Vec<Value> {
    let mut events: Vec<Value> = drained
        .iter()
        .filter(|event| event["detail"]["jobId"] == json!(job_id))
        .cloned()
        .collect();
    events.sort_by_key(|event| event["detail"]["timestamp"].as_i64().unwrap_or_default());
    events
}

/// The `detail.status` of each of `events`, in order.
pub(crate) fn statuses_of(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|event| event["detail"]["status"].as_str().unwrap_or_default())
        .collect()
}

/// Every event delivered to the queue at `url` about the job `job_id`, accumulated across
/// drains until the statuses `expected` have all arrived — or, when `expected` is empty,
/// whatever ONE drain finds.
///
/// A publication is made with no request waiting on it, so WHICH drain of the target queue
/// sees it is not something a case can predict: a case that expects events waits for the
/// CONDITION (the sequence being complete) rather than for a span, with a generous
/// deadline so a broken emulator fails instead of hanging, and then drains once more so a
/// move announced a second time is counted rather than missed. A case that expects NONE has
/// no such condition to wait for, and does not need one: it is called once the job's run is
/// over and the store holds no unpublished change, so a single drain is the whole of what
/// was published.
async fn events_awaited(
    emulator: &Emulator,
    case: &str,
    url: &str,
    job_id: &str,
    expected: &[&str],
) -> Vec<Value> {
    let started = std::time::Instant::now();
    let mut collected: Vec<Value> = Vec::new();
    loop {
        collected.extend(drain(emulator, case, url).await);
        let own = events_for(&collected, job_id);
        if expected.is_empty() {
            return own;
        }
        if statuses_of(&own) == expected || started.elapsed() >= DELIVERY_DEADLINE {
            // One more drain once the sequence is complete, so an event delivered BESIDE
            // it — a move announced twice — is collected here rather than left on the
            // queue, where the count this answers could not see it.
            collected.extend(drain(emulator, case, url).await);
            return events_for(&collected, job_id);
        }
        tokio::time::sleep(DELIVERY_POLL_PAUSE).await;
    }
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The door the job is asked for through once its transition is due — `{id}` is the
    /// job's id. It materialises the status change unless the store's clock pump already
    /// has, and publishes whatever is still queued either way.
    door: &'static str,
    /// `PAUSED` holds the job in its queue, so it never begins.
    queue_status: &'static str,
    /// Whether the job is cancelled before anything is due from it.
    cancel_first: bool,
    /// Whether the target queue grants EventBridge the delivery it needs.
    granted: bool,
    /// The `detail.status` of every event the target queue must end up holding for this
    /// job, in the order the job moved into them — empty for a job that never began.
    delivered: &'static [&'static str],
}

/// The statuses a job that RUNS moves into, and so the events it publishes: it begins
/// processing (`PROGRESSING`) and then its run ends — in this emulator in [`TERMINAL_STATUS`],
/// because nothing transcoded.
const RUN: &[&str] = &["PROGRESSING", TERMINAL_STATUS];

/// The table spans the rule: which door the job is read through, whether the job began at
/// all, and whether the target queue has granted the delivery.
const ROWS: &[Row] = &[
    Row {
        case: "GetJob materialises the transition",
        door: "/2017-08-29/jobs/{id}",
        queue_status: "ACTIVE",
        cancel_first: false,
        granted: true,
        delivered: RUN,
    },
    Row {
        case: "ListJobs materialises the transition",
        door: "/2017-08-29/jobs",
        queue_status: "ACTIVE",
        cancel_first: false,
        granted: true,
        delivered: RUN,
    },
    Row {
        case: "SearchJobs materialises the transition",
        door: "/2017-08-29/search?inputFile=.mp4",
        queue_status: "ACTIVE",
        cancel_first: false,
        granted: true,
        delivered: RUN,
    },
    Row {
        case: "a queue projection materialises the transition",
        door: "/2017-08-29/queues/events-queue",
        queue_status: "ACTIVE",
        cancel_first: false,
        granted: true,
        delivered: RUN,
    },
    Row {
        case: "a job cancelled before it began publishes nothing",
        door: "/2017-08-29/jobs/{id}",
        queue_status: "PAUSED",
        cancel_first: true,
        granted: true,
        delivered: &[],
    },
    Row {
        case: "a target queue that granted no delivery receives nothing",
        door: "/2017-08-29/jobs/{id}",
        queue_status: "ACTIVE",
        cancel_first: false,
        granted: false,
        delivered: &[],
    },
];

/// The MediaConvert queue every row submits its job to.
const JOB_QUEUE: &str = "events-queue";

#[tokio::test]
async fn a_job_that_moves_publishes_a_job_state_change_event() {
    for row in ROWS {
        let case = row.case;
        // The settle span is state, so each row pins it instead of racing the real clock.
        let emulator = Emulator::with_settle_span(SHORT_SETTLE)
            .mount(service_com_amazonaws_eventbridge_awsevents::EmulatedService::new)
            .mount(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new);

        // The target of the rule that matches, and the target of a rule that cannot.
        let (matching_url, matching_arn) = given_queue(&emulator, case, "mediaconvert-events").await;
        let (other_url, other_arn) = given_queue(&emulator, case, "unrelated-events").await;
        let matching_rule = given_rule(
            &emulator,
            case,
            MATCHING_RULE,
            json!({ "source": [EVENT_SOURCE], "detail-type": [JOB_STATE_CHANGE] }),
            &matching_arn,
        )
        .await;
        let other_rule = given_rule(
            &emulator,
            case,
            OTHER_RULE,
            // Another service's events: nothing MediaConvert publishes can match it.
            json!({ "source": ["aws.ecs"], "detail-type": ["ECS Task State Change"] }),
            &other_arn,
        )
        .await;
        if row.granted {
            grant_delivery(&emulator, case, &matching_url, &matching_arn, &matching_rule).await;
        }
        grant_delivery(&emulator, case, &other_url, &other_arn, &other_rule).await;

        let created = emulator
            .create_queue(json!({ "name": JOB_QUEUE, "status": row.queue_status }))
            .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: CreateQueue answered {} with body {}",
            created.status, created.body
        );
        let submitted = emulator
            .create_job(json!({
                "role": ROLE,
                "queue": JOB_QUEUE,
                "settings": job_settings("s3://example-bucket/in/a.mp4", "_a"),
                "userMetadata": { "crew": "alpha" },
            }))
            .await;
        assert_eq!(
            submitted.status, 201,
            "case {case:?}: CreateJob answered {} with body {}",
            submitted.status, submitted.body
        );
        let job_id = submitted.string("/job/id");
        assert!(
            !job_id.is_empty(),
            "case {case:?}: expected a minted job id, got {}",
            submitted.body
        );

        if row.cancel_first {
            let canceled = emulator.cancel_job(&job_id).await;
            assert_eq!(
                canceled.status, 202,
                "case {case:?}: CancelJob answered {} with body {}",
                canceled.status, canceled.body
            );
        }

        // The CONDITION, not a span: a job its queue let begin has run — out of
        // `SUBMITTED` and on to the end of its run — so both of its moves are published by
        // the time the door below answers, and a job a cancel took out of its queue is
        // already terminal.
        emulator.await_run(case, &job_id).await;
        let reply = emulator
            .send("GET", &row.door.replace("{id}", &job_id), None)
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: the door {} answered {} with body {}",
            row.door, reply.status, reply.body
        );

        let delivered = events_awaited(&emulator, case, &matching_url, &job_id, row.delivered).await;
        let statuses = statuses_of(&delivered);
        assert_eq!(
            statuses, row.delivered,
            "case {case:?}: expected the statuses {:?} delivered for job {job_id}, got \
             {delivered:?}",
            row.delivered
        );

        if row.cancel_first {
            // Nothing was merely undelivered: a job cancelled before it began RECORDS no
            // change at all, which is read off the store the handlers write to.
            let queued = emulator
                .engine
                .state::<ComAmazonawsMediaconvertMediaconvertState>()
                .lock()
                .pending_job_event_count();
            assert_eq!(
                queued, 0,
                "case {case:?}: a job cancelled before it began must queue no event, got \
                 {queued}"
            );
        }

        for event in &delivered {
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
            // The event reports the job the control plane reports: its queue, named by the
            // same ARN `Job$Queue` carries, and its own metadata.
            assert_eq!(
                event["detail"]["queue"],
                json!(queue_arn(JOB_QUEUE)),
                "case {case:?}: expected detail.queue {}, got {event}",
                queue_arn(JOB_QUEUE)
            );
            assert_eq!(
                event["detail"]["userMetadata"],
                json!({ "crew": "alpha" }),
                "case {case:?}: expected the job's own userMetadata, got {event}"
            );
            assert_eq!(
                event["resources"],
                json!([arn(&format!("jobs/{job_id}"))]),
                "case {case:?}: expected the job's ARN in resources, got {event}"
            );
        }

        if let Some(event) = delivered.last() {
            // The status and the failure the LAST event reports are the ones `GetJob`
            // reports for the same job, so the event stream ends where the job stands and
            // is not a second copy of it.
            let read = emulator.get_job(&job_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: GetJob answered {} with body {}",
                read.status, read.body
            );
            assert_eq!(
                event["detail"]["status"],
                read.at("/job/status").clone(),
                "case {case:?}: the event's status must be the job's, got {event} and {}",
                read.body
            );
            assert_eq!(
                event["detail"]["errorMessage"],
                read.at("/job/errorMessage").clone(),
                "case {case:?}: the event's errorMessage must be the job's, got {event} and {}",
                read.body
            );
        }

        // The rule that cannot match this event never receives it, whatever the row did.
        let unrelated = drain(&emulator, case, &other_url).await;
        assert!(
            unrelated.is_empty(),
            "case {case:?}: the non-matching rule's target must receive nothing, got \
             {unrelated:?}"
        );
    }
}

/// A job that is read REPEATEDLY publishes each of its moves once: the event stream is
/// the job's MOVES, not the reads of it, so polling a job that has run does not keep
/// announcing it.
#[tokio::test]
async fn a_transition_is_published_once_however_often_the_job_is_read() {
    let case = "repeated reads of one job that has run";
    let emulator = Emulator::with_settle_span(SHORT_SETTLE)
        .mount(service_com_amazonaws_eventbridge_awsevents::EmulatedService::new)
        .mount(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new);

    let (url, queue_arn_value) = given_queue(&emulator, case, "mediaconvert-events").await;
    let rule = given_rule(
        &emulator,
        case,
        MATCHING_RULE,
        json!({ "source": [EVENT_SOURCE], "detail-type": [JOB_STATE_CHANGE] }),
        &queue_arn_value,
    )
    .await;
    grant_delivery(&emulator, case, &url, &queue_arn_value, &rule).await;

    let created = emulator.create_queue(json!({ "name": JOB_QUEUE })).await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: CreateQueue answered {} with body {}",
        created.status, created.body
    );
    let submitted = emulator
        .create_job(json!({
            "role": ROLE,
            "queue": JOB_QUEUE,
            "settings": job_settings("s3://example-bucket/in/a.mp4", "_a"),
        }))
        .await;
    assert_eq!(
        submitted.status, 201,
        "case {case:?}: CreateJob answered {} with body {}",
        submitted.status, submitted.body
    );
    let job_id = submitted.string("/job/id");

    emulator.await_run(case, &job_id).await;
    // Four reads through three different doors: not one of them announces a move again.
    for door in [
        "/2017-08-29/jobs/{id}",
        "/2017-08-29/jobs/{id}",
        "/2017-08-29/jobs",
        "/2017-08-29/search?inputFile=.mp4",
    ] {
        let reply = emulator
            .send("GET", &door.replace("{id}", &job_id), None)
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: the door {door} answered {} with body {}",
            reply.status, reply.body
        );
    }

    let delivered = events_awaited(&emulator, case, &url, &job_id, RUN).await;
    assert_eq!(
        statuses_of(&delivered),
        RUN,
        "case {case:?}: expected exactly the job's own two moves {RUN:?} for job {job_id}, \
         got {delivered:?}"
    );
}
