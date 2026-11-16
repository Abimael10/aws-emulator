// SPDX-License-Identifier: CC0-1.0

//! MediaConnect publishes a `MediaConnect Flow Status Change` event when a flow moves.
//!
//! The RULE under test: every status a flow MOVES INTO is published onto the account's
//! DEFAULT EventBridge bus as `source` `aws.mediaconnect` / `detail-type` `MediaConnect Flow
//! Status Change`, with the flow's own ARN in the envelope's `resources` and the status it
//! moved into in `detail.currentStatus` (beside the status it left, `detail.previousStatus`);
//! EventBridge then routes it, so an ENABLED rule whose `EventPattern` matches delivers it to
//! that rule's targets, while a rule that cannot match it, and a target queue that has not
//! granted `events.amazonaws.com` the `sqs:SendMessage` an SQS target needs, receive nothing.
//!
//! The moves are the modelled `Status` values this service's lifecycle reaches
//! (`smithy_query(query="shape",shape="Status")`): a start moves a flow `STANDBY` ->
//! `STARTING` and then, one transition span later, `STARTING` -> `ACTIVE`; a stop moves it
//! `ACTIVE` -> `STOPPING` -> `STANDBY`; a delete moves it into `DELETING`
//! (`DeleteFlowResponse$Status`, "the status of the flow when the DeleteFlow process
//! begins"). The rows below span those transitions and both halves of the delivery contract,
//! because the rule generalises over WHICH move a flow makes and over whether the rule and
//! its target are allowed to receive it — not over one call.
//!
//! The clock-driven half matters on its own: the second status of a start or a stop is owed
//! by the CLOCK, so a client that waits on the event alone makes no further MediaConnect
//! call, and the cases below never call MediaConnect again after the transition they drive.
//!
//! EventBridge and SQS are mounted BESIDE MediaConnect on the SAME engine, so the publication
//! travels the real in-process callback, through the real bus, to a real SQS target: nothing
//! here reads the publisher's own queue.

use std::time::Duration;

use serde_json::{json, Value};

use crate::flow_lifecycle::Emulator;
use crate::lifecycle_preconditions::created_flow_arn;

/// The `source` and `detail-type` MediaConnect publishes a flow's status change under,
/// spelled out here rather than imported, so a wrong constant in the implementation cannot
/// make a test pass.
const EVENT_SOURCE: &str = "aws.mediaconnect";
const FLOW_STATUS_CHANGE: &str = "MediaConnect Flow Status Change";

/// A `detail-type` this service never emits — MediaConnect's own alert event, which no flow
/// status change can be mistaken for.
const OTHER_DETAIL_TYPE: &str = "MediaConnect Flow Alert";

/// How long a case waits for a delivery before it gives up.
///
/// Generous on purpose: the only timing this suite is about is the ORDER of the moves, not
/// how fast the host runs, so every wait polls for the delivery until it arrives and only
/// this backstop ends a wait that never will.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

/// How long one turn of a poll loop waits before looking again. Far below
/// [`DELIVERY_DEADLINE`], so a case that is waiting on the clock's own transition span still
/// takes several turns.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One EventBridge call, the way a client makes it: `awsJson1_1`, `POST /` with
/// `X-Amz-Target: AWSEvents.<Operation>`.
async fn events(emulator: &Emulator, operation: &str, body: Value) -> (u16, Value) {
    emulator
        .rpc("events", "AWSEvents", "1.1", operation, body)
        .await
}

/// One SQS call, the way a client makes it: `awsJson1_0`, `POST /` with
/// `X-Amz-Target: AmazonSQS.<Operation>`.
async fn sqs(emulator: &Emulator, operation: &str, body: Value) -> (u16, Value) {
    emulator
        .rpc("sqs", "AmazonSQS", "1.0", operation, body)
        .await
}

/// An SQS queue called `name`, created through SQS itself; answers its url and the ARN a rule
/// names it by.
async fn given_queue(emulator: &Emulator, case: &str, name: &str) -> (String, String) {
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

/// Grant `events.amazonaws.com` the `sqs:SendMessage` a rule's target needs on the queue at
/// `url`, for the events the rule `rule_arn` delivers — EventBridge makes that call as its own
/// service principal and relies on the queue's resource-based policy, so a queue that has
/// granted it nothing receives nothing.
async fn grant_delivery(
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

/// Put the ENABLED rule `name` with `pattern` on the default bus and point it at the SQS
/// queue `queue_arn`; answers the rule's ARN.
async fn given_rule(
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
        json!({ "Rule": name, "Targets": [{ "Id": "queue-target", "Arn": queue_arn }] }),
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

/// Every message waiting on the queue at `url` right now, as parsed JSON event envelopes.
async fn drain(emulator: &Emulator, case: &str, url: &str) -> Vec<Value> {
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

/// Poll the queue at `url` until `collected` holds `wanted` events for the flow `flow_arn`,
/// or [`DELIVERY_DEADLINE`] passes.
///
/// A CONDITION is waited for, never a fixed span: the second status of a start or a stop is
/// owed by the clock, so the wait ends as soon as the delivery arrives. The events already
/// collected count towards `wanted`, because an SQS message is handed out once.
async fn collect_until(
    emulator: &Emulator,
    case: &str,
    url: &str,
    flow_arn: &str,
    collected: &mut Vec<Value>,
    wanted: usize,
) {
    let deadline = std::time::Instant::now() + DELIVERY_DEADLINE;
    loop {
        collected.extend(
            drain(emulator, case, url)
                .await
                .into_iter()
                .filter(|event| event["resources"] == json!([flow_arn])),
        );
        if collected.len() >= wanted || std::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Everything the queue at `url` delivers for the flow `flow_arn`, waited for up to `wanted`
/// events. See [`collect_until`].
async fn delivered_events(
    emulator: &Emulator,
    case: &str,
    url: &str,
    flow_arn: &str,
    wanted: usize,
) -> Vec<Value> {
    let mut collected = Vec::new();
    collect_until(emulator, case, url, flow_arn, &mut collected, wanted).await;
    collected
}

/// The `(previousStatus, currentStatus)` pair one event reports.
fn moved(event: &Value) -> (String, String) {
    (
        event["detail"]["previousStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        event["detail"]["currentStatus"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// One row of the transition table: the lifecycle calls a case drives, and the moves the rule
/// says they publish, in order.
struct Row {
    case: &'static str,
    /// The `/v1/flows/{segment}/{FlowArn}` segments to POST, in order. A flow is created
    /// `STANDBY`, so a stop needs a start before it.
    segments: &'static [&'static str],
    /// Whether the flow is DELETED after those calls.
    then_delete: bool,
    /// The `(previousStatus, currentStatus)` moves the rule says are published, in order.
    moves: &'static [(&'static str, &'static str)],
}

/// The table spans the moves a flow makes: the two a start produces (one accepted by the
/// call, one owed by the CLOCK), the two more a stop produces, and the one a delete reports.
const ROWS: &[Row] = &[
    Row {
        case: "a start publishes STARTING and then the ACTIVE the clock owes",
        segments: &["start"],
        then_delete: false,
        moves: &[("STANDBY", "STARTING"), ("STARTING", "ACTIVE")],
    },
    Row {
        case: "a stop of a running flow publishes STOPPING and then STANDBY",
        segments: &["start", "stop"],
        then_delete: false,
        moves: &[
            ("STANDBY", "STARTING"),
            ("STARTING", "ACTIVE"),
            ("ACTIVE", "STOPPING"),
            ("STOPPING", "STANDBY"),
        ],
    },
    Row {
        case: "a delete of a stopped flow publishes DELETING",
        segments: &[],
        then_delete: true,
        moves: &[("STANDBY", "DELETING")],
    },
    Row {
        case: "a started, stopped and then deleted flow publishes every move it made",
        segments: &["start", "stop"],
        then_delete: true,
        moves: &[
            ("STANDBY", "STARTING"),
            ("STARTING", "ACTIVE"),
            ("ACTIVE", "STOPPING"),
            ("STOPPING", "STANDBY"),
            ("STANDBY", "DELETING"),
        ],
    },
];

/// Rule, the whole table: every move a flow makes reaches the target of a rule that matches
/// it, as the event the rule's pattern selected.
#[tokio::test]
async fn every_flow_status_move_is_published_to_the_default_event_bus() {
    for row in ROWS {
        let case = row.case;
        let emulator = Emulator::with_event_bus();
        let (url, queue_arn) = given_queue(&emulator, case, "mediaconnect-status-changes").await;
        // The pattern a client keyed on this service's flow events writes: this service's
        // source and the flow status change's own detail-type.
        let rule = given_rule(
            &emulator,
            case,
            "mediaconnect-flow-status-change",
            json!({ "source": [EVENT_SOURCE], "detail-type": [FLOW_STATUS_CHANGE] }),
            &queue_arn,
        )
        .await;
        grant_delivery(&emulator, case, &url, &queue_arn, &rule).await;

        let flow_arn = created_flow_arn(
            &emulator,
            &format!("events-{}", row.moves.len()),
            json!({}),
        )
        .await;

        // Everything this row's target has delivered so far: a message is handed out once, so
        // the row accumulates its deliveries rather than re-reading them.
        let mut collected: Vec<Value> = Vec::new();
        for segment in row.segments {
            let (status, code, body) = emulator
                .send("POST", &format!("/v1/flows/{segment}/{flow_arn}"), None)
                .await;
            assert_eq!(
                (status, code.as_deref()),
                (202, None),
                "case {case:?}: the {segment} must be accepted, got {status} {code:?}: {body}"
            );
            // The next lifecycle call needs the status the CLOCK owes this flow, so the case
            // waits for the event that reports it rather than for a span of time: both moves
            // of this transition must have been published before the next call is made.
            let wanted = collected.len() + 2;
            collect_until(
                &emulator,
                case,
                &url,
                &flow_arn,
                &mut collected,
                wanted,
            )
            .await;
            assert!(
                collected.len() >= wanted,
                "case {case:?}: the {segment} must publish both of its moves, got {collected:?}"
            );
        }

        if row.then_delete {
            let (status, code, body) = emulator
                .send("DELETE", &format!("/v1/flows/{flow_arn}"), None)
                .await;
            assert_eq!(
                (status, code.as_deref(), body["status"].as_str()),
                (202, None, Some("DELETING")),
                "case {case:?}: the delete must be accepted, got {status} {code:?}: {body}"
            );
        }

        // The whole row's sequence, waited for by the number of moves the rule says it makes.
        collect_until(
            &emulator,
            case,
            &url,
            &flow_arn,
            &mut collected,
            row.moves.len(),
        )
        .await;
        let mut seen: Vec<(String, String)> = Vec::new();
        for event in &collected {
            assert_published_event(case, event, &flow_arn);
            seen.push(moved(event));
        }
        let expected: Vec<(String, String)> = row
            .moves
            .iter()
            .map(|(previous, current)| (previous.to_string(), current.to_string()))
            .collect();
        assert_eq!(
            seen, expected,
            "case {case:?}: expected the moves {expected:?} published in order, got {seen:?}"
        );
    }
}

/// What every published event must carry, whichever move it reports.
fn assert_published_event(case: &str, event: &Value, flow_arn: &str) {
    assert_eq!(
        event["source"],
        json!(EVENT_SOURCE),
        "case {case:?}: expected source {EVENT_SOURCE}, got {event}"
    );
    assert_eq!(
        event["detail-type"],
        json!(FLOW_STATUS_CHANGE),
        "case {case:?}: expected detail-type {FLOW_STATUS_CHANGE}, got {event}"
    );
    assert_eq!(
        event["resources"],
        json!([flow_arn]),
        "case {case:?}: expected the flow's ARN in resources, got {event}"
    );
    let (previous, current) = moved(event);
    assert!(
        !current.is_empty() && !previous.is_empty(),
        "case {case:?}: the event reports the move it is about, got {event}"
    );
}

/// One row of the delivery table: a rule's pattern, whether its target granted EventBridge
/// the delivery, and whether that target must end up holding the flow's event.
struct DeliveryRow {
    case: &'static str,
    rule: &'static str,
    queue: &'static str,
    /// Whether the target queue grants `events.amazonaws.com` `sqs:SendMessage`.
    granted: bool,
    /// Whether the flow's event must be delivered to this target.
    delivered: bool,
}

/// The table spans both halves of the delivery contract: which patterns select a flow status
/// change, and whether the target is allowed to receive it.
const DELIVERY_ROWS: &[DeliveryRow] = &[
    DeliveryRow {
        case: "source, detail-type and the flow's own resource, authorized",
        rule: "matches-everything-about-it",
        queue: "matching-authorized",
        granted: true,
        delivered: true,
    },
    DeliveryRow {
        case: "source alone, authorized",
        rule: "matches-the-source",
        queue: "source-authorized",
        granted: true,
        delivered: true,
    },
    DeliveryRow {
        case: "source and detail-type, but the queue granted no delivery",
        rule: "matches-but-unauthorized",
        queue: "matching-unauthorized",
        granted: false,
        delivered: false,
    },
    DeliveryRow {
        case: "a detail-type MediaConnect never emits for a status change",
        rule: "other-detail-type",
        queue: "other-detail-type",
        granted: true,
        delivered: false,
    },
    DeliveryRow {
        case: "another service's source",
        rule: "another-source",
        queue: "another-source",
        granted: true,
        delivered: false,
    },
];

/// The `EventPattern` of one delivery row, built once the flow's ARN is known.
fn delivery_pattern(rule: &str, flow_arn: &str) -> Value {
    match rule {
        "matches-everything-about-it" => json!({
            "source": [EVENT_SOURCE],
            "detail-type": [FLOW_STATUS_CHANGE],
            "resources": [flow_arn],
        }),
        "matches-the-source" => json!({ "source": [EVENT_SOURCE] }),
        "matches-but-unauthorized" => {
            json!({ "source": [EVENT_SOURCE], "detail-type": [FLOW_STATUS_CHANGE] })
        }
        "other-detail-type" => {
            json!({ "source": [EVENT_SOURCE], "detail-type": [OTHER_DETAIL_TYPE] })
        }
        _ => json!({ "source": ["aws.ecs"], "detail-type": ["ECS Task State Change"] }),
    }
}

/// Rule, the delivery half: ONE flow move, five rules on the same bus — the targets of the
/// rules whose patterns select it and whose queues authorized EventBridge receive it, and no
/// other target receives anything.
#[tokio::test]
async fn only_a_matching_rule_with_an_authorized_target_receives_the_event() {
    let case = "one start, five rules";
    let emulator = Emulator::with_event_bus();
    let flow_arn = created_flow_arn(&emulator, "delivery-contract", json!({})).await;

    let mut urls: Vec<(&DeliveryRow, String)> = Vec::new();
    for row in DELIVERY_ROWS {
        let (url, queue_arn) = given_queue(&emulator, row.case, row.queue).await;
        let rule_arn = given_rule(
            &emulator,
            row.case,
            row.rule,
            delivery_pattern(row.rule, &flow_arn),
            &queue_arn,
        )
        .await;
        if row.granted {
            grant_delivery(&emulator, row.case, &url, &queue_arn, &rule_arn).await;
        }
        urls.push((row, url));
    }

    let (status, code, body) = emulator
        .send("POST", &format!("/v1/flows/start/{flow_arn}"), None)
        .await;
    assert_eq!(
        (status, code.as_deref(), body["status"].as_str()),
        (202, None, Some("STARTING")),
        "case {case:?}: the start must be accepted, got {status} {code:?}: {body}"
    );

    // Both moves of the start, waited for on every target that must receive them: the second
    // one is the clock's, and no MediaConnect call is made for it.
    for (row, url) in &urls {
        let wanted = if row.delivered { 2 } else { 0 };
        let seen = delivered_events(&emulator, row.case, url, &flow_arn, wanted).await;
        if row.delivered {
            let moves: Vec<(String, String)> = seen.iter().map(moved).collect();
            assert_eq!(
                moves,
                vec![
                    ("STANDBY".to_string(), "STARTING".to_string()),
                    ("STARTING".to_string(), "ACTIVE".to_string()),
                ],
                "case {:?}: expected both moves of the start, got {moves:?}",
                row.case
            );
            for event in &seen {
                assert_published_event(row.case, event, &flow_arn);
            }
        } else {
            assert!(
                seen.is_empty(),
                "case {:?}: this target must receive nothing, got {seen:?}",
                row.case
            );
        }
    }
}

/// Rule, the once-only half: a move is published ONCE however many times the flow is read
/// afterwards — the event stream is the flow's MOVES, not the reads of it — and the status the
/// event reports is the status the control plane reports for the same flow.
#[tokio::test]
async fn a_move_is_published_once_however_often_the_flow_is_read() {
    let case = "repeated reads of one started flow";
    let emulator = Emulator::with_event_bus();
    let (url, queue_arn) = given_queue(&emulator, case, "once-only").await;
    let rule = given_rule(
        &emulator,
        case,
        "mediaconnect-flow-status-change",
        json!({ "source": [EVENT_SOURCE], "detail-type": [FLOW_STATUS_CHANGE] }),
        &queue_arn,
    )
    .await;
    grant_delivery(&emulator, case, &url, &queue_arn, &rule).await;

    let flow_arn = created_flow_arn(&emulator, "read-repeatedly", json!({})).await;
    let (status, _, body) = emulator
        .send("POST", &format!("/v1/flows/start/{flow_arn}"), None)
        .await;
    assert_eq!(
        status, 202,
        "case {case:?}: the start must be accepted, got {status}: {body}"
    );

    // Wait for the move the clock owes, then read the flow repeatedly through both reporting
    // doors: a read materialises nothing more, so it publishes nothing more.
    let first = delivered_events(&emulator, case, &url, &flow_arn, 2).await;
    assert_eq!(
        first.len(),
        2,
        "case {case:?}: expected both moves of the start, got {first:?}"
    );

    for _ in 0..3 {
        let (status, code, described) = emulator.describe_flow(&flow_arn).await;
        assert_eq!(
            (status, code.as_deref(), described["flow"]["status"].as_str()),
            (200, None, Some("ACTIVE")),
            "case {case:?}: DescribeFlow must report the settled status, got {status} \
             {code:?}: {described}"
        );
        let (status, code, listed) = emulator.list_flows("").await;
        assert_eq!(
            (status, code.as_deref()),
            (200, None),
            "case {case:?}: ListFlows answered {status} {code:?}: {listed}"
        );
    }

    // Nothing was merely undelivered either: the store holds no change waiting to be
    // published, which is read off the very queue the handlers write to.
    let queued = emulator
        .engine
        .state::<service_com_amazonaws_mediaconnect_mediaconnect::state::ComAmazonawsMediaconnectMediaconnectState>()
        .lock()
        .pending_flow_event_count();
    assert_eq!(
        queued, 0,
        "case {case:?}: a read must queue no event, got {queued}"
    );

    let again = drain(&emulator, case, &url).await;
    assert!(
        again.is_empty(),
        "case {case:?}: reading the flow must publish nothing more, got {again:?}"
    );
}
