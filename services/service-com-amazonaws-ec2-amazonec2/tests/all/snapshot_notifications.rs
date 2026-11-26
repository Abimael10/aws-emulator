// SPDX-License-Identifier: CC0-1.0

//! The event an EBS snapshot's OUTCOME is published as, end to end over the wire.
//!
//! THE RULE: a snapshot action is not finished when the API answers it — the answer reports a
//! `pending` snapshot — so Amazon EBS publishes the OUTCOME of that action, once per snapshot
//! it was asked for, onto the account's default EventBridge bus as `source` `aws.ec2` /
//! `detail-type` `EBS Snapshot Notification`, naming the snapshot in the envelope's
//! `resources` and carrying in the event `detail` the action it is the outcome of
//! (`createSnapshot`, `createSnapshots`, `copySnapshot`), the `state` the snapshot settled in,
//! the `result` saying whether that is the state the action set out to reach, the `cause` of a
//! result that failed, the snapshot and its source, and the id of the call itself.
//! A snapshot that holds its source's data reports `completed` / `succeeded` with no cause,
//! and one whose work could not be done at all reports `error` / `failed` with the reason — so
//! anything that reacts to a snapshot's fate learns of a failure instead of waiting for data
//! that will never arrive, and a copy whose source is not there is observable at all: EC2
//! accepts that call and the failure surfaces afterwards.
//!
//! The rows span what the rule generalises over: WHICH action started the snapshot (a single
//! create, a multi-volume create, a copy), what the snapshot was made FROM (a volume of the
//! account, a snapshot of this Region, a snapshot of another Region), and the OUTCOME (a
//! snapshot that completes, and a copy that fails) — each row's outcome following what the
//! store settled on rather than the shape of the request. Every event is read off the queue a
//! real rule on the real default bus delivered it to, and every snapshot is read back through
//! `DescribeSnapshots`, so the event and the API are held to the same answer.
//!
//! Nothing here reads this crate's own state: EventBridge and Amazon SQS are mounted BESIDE
//! Amazon EC2 on one engine, exactly as the assembled router mounts them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use service_com_amazonaws_eventbridge_awsevents as eventbridge;
use service_com_amazonaws_sqs_amazonsqs as sqs;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// A Region of the same partition that is NOT the one these calls are signed for: a copy
/// names the Region its source lives in, so one row reads that Region back off the event.
const ANOTHER_REGION: &str = "eu-west-1";

/// The zone every volume here is created in.
const ZONE: &str = "us-east-1a";

/// The `source`, `detail-type` and `detail.event` values the outcome of a snapshot action is
/// published under, spelled out here rather than imported, so a wrong constant in the
/// implementation cannot make a test pass.
const EC2_EVENT_SOURCE: &str = "aws.ec2";
const SNAPSHOT_NOTIFICATION: &str = "EBS Snapshot Notification";
const CREATE_SNAPSHOT_EVENT: &str = "createSnapshot";
const CREATE_SNAPSHOTS_EVENT: &str = "createSnapshots";
const COPY_SNAPSHOT_EVENT: &str = "copySnapshot";

/// The `state` each outcome settles in and the `result` it reports, spelled out for the same
/// reason.
const COMPLETED: &str = "completed";
const ERROR: &str = "error";
const SUCCEEDED: &str = "succeeded";
const FAILED: &str = "failed";

/// A snapshot id of the long form this service mints that no snapshot of the account carries:
/// the copy of a source that is not there is what makes a failure observable.
const MISSING_SNAPSHOT_ID: &str = "snap-0123456789abcdef0";

/// The sentence a copy whose source could not be resolved reports as the `cause` of its
/// failure, in AWS's own words: it names neither the id nor the Region.
const MISSING_SOURCE_CAUSE: &str = "Source snapshot is not found";

/// How long a poll keeps asking for an event, or for a snapshot to settle, before giving up.
/// Generous: the publication is made once the outcome falls due, so this is many times the
/// work a row does and still lets a broken case fail rather than hang.
const DELIVERY_BUDGET: Duration = Duration::from_secs(60);

/// How long a poll waits between asks.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How many `ReceiveMessage` rounds one drain makes: a receive answers at most ten messages,
/// and this is well above the events the whole table publishes.
const MAX_RECEIVE_ROUNDS: usize = 8;

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// Amazon EC2, EventBridge and Amazon SQS over ONE shared engine, exactly as the assembled
/// router mounts them: the bus a snapshot's outcome is published onto and the queue a rule
/// delivers that event to are each that service's own resource.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(eventbridge::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(sqs::EmulatedService::new(engine));
    router
}

/// One `awsJson` RPC call against the service whose signing name is `signing_name`,
/// answering the status and the payload.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDSNAPEVENTS/20260101/{REGION}/\
                 {signing_name}/aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
            ),
        )
        .body(::bytes::Bytes::from(
            ::serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed JSON request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = if response.body().is_empty() {
        Value::Null
    } else {
        ::serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, payload)
}

/// One EventBridge call, which must be served.
async fn events_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, payload) = json_call(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EventBridge {operation} must answer 200, got {status} with {payload}"
    );
    payload
}

/// One Amazon SQS call, which must be served.
async fn sqs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let (status, payload) = json_call(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: Amazon SQS {operation} must answer 200, got {status} with {payload}"
    );
    payload
}

/// One EC2 call over the wire this service speaks, which must be served.
async fn ec2_ok(router: &MultiServiceRouter, case: &str, action: &str, params: &Params) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EC2 {action} must answer 200, got {status} with {body}"
    );
    body
}

/// A queue, created through Amazon SQS itself and carrying the grant a rule's target needs —
/// EventBridge makes the target's `sqs:SendMessage` as its own service principal and relies
/// on the queue's resource-based policy — answering its url and the ARN a rule names it by.
async fn queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs_ok(router, case, "CreateQueue", json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue must answer a QueueUrl, got {created}"))
        .to_string();
    let attributes = sqs_ok(
        router,
        case,
        "GetQueueAttributes",
        json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: GetQueueAttributes must answer a QueueArn, got {attributes}")
        })
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs_ok(
        router,
        case,
        "SetQueueAttributes",
        json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, arn)
}

/// A default-bus rule matching every `aws.ec2` snapshot notification, delivering to a queue
/// of its own, and that queue's url — the ONE wiring every test below watches events through.
async fn watched_queue(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (url, queue_arn) = queue(router, case, name).await;
    events_ok(
        router,
        case,
        "PutRule",
        json!({
            "Name": name,
            "State": "ENABLED",
            "EventPattern": json!({
                "source": [EC2_EVENT_SOURCE],
                "detail-type": [SNAPSHOT_NOTIFICATION],
            })
            .to_string(),
        }),
    )
    .await;
    events_ok(
        router,
        case,
        "PutTargets",
        json!({ "Rule": name, "Targets": [{ "Id": "t1", "Arn": queue_arn }] }),
    )
    .await;
    url
}

/// Every event waiting on `url` right now, taken off the queue so a later drain reports only
/// what arrived after this one.
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let mut events = Vec::new();
    for _ in 0..MAX_RECEIVE_ROUNDS {
        let received = sqs_ok(
            router,
            case,
            "ReceiveMessage",
            json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "WaitTimeSeconds": 0 }),
        )
        .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            break;
        }
        for message in messages {
            if let Some(handle) = message["ReceiptHandle"].as_str() {
                sqs_ok(
                    router,
                    case,
                    "DeleteMessage",
                    json!({ "QueueUrl": url, "ReceiptHandle": handle }),
                )
                .await;
            }
            let body = message["Body"].as_str().unwrap_or_default();
            events.push(::serde_json::from_str::<Value>(body).unwrap_or_else(|error| {
                panic!("case {case:?}: a delivered event is JSON ({error}): {body}")
            }));
        }
    }
    events
}

/// Whether `event` names `snapshot_arn` among its `resources`.
fn names_snapshot(event: &Value, snapshot_arn: &str) -> bool {
    event["resources"].as_array().is_some_and(|resources| {
        resources
            .iter()
            .any(|resource| resource.as_str() == Some(snapshot_arn))
    })
}

/// The events delivered to `url` that name `snapshot_arn` in their `resources`, looked for
/// until the deadline passes rather than waited out, so the answer is the one the service
/// published however fast the host runs. Every other event drained on the way is answered
/// too, so nothing a row published is lost to another row's poll.
async fn delivered_for(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    snapshot_arn: &str,
) -> (Vec<Value>, Vec<Value>) {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    let mut mine: Vec<Value> = Vec::new();
    let mut others: Vec<Value> = Vec::new();
    loop {
        for event in drain(router, case, url).await {
            if names_snapshot(&event, snapshot_arn) {
                mine.push(event);
            } else {
                others.push(event);
            }
        }
        if !mine.is_empty() {
            return (mine, others);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: an EBS snapshot notification for {snapshot_arn} must be delivered, \
             drained {others:?} instead"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `status` `DescribeSnapshots` reports for `snapshot_id` once it has settled out of
/// `pending`, polled until the deadline passes.
async fn settled_state(router: &MultiServiceRouter, case: &str, snapshot_id: &str) -> String {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    loop {
        let body = ec2_ok(
            router,
            case,
            "DescribeSnapshots",
            &vec![p("SnapshotId.1", snapshot_id)],
        )
        .await;
        let reported = element(&body, "status").unwrap_or_default();
        if !reported.is_empty() && reported != "pending" {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {snapshot_id} must settle out of pending, got {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One volume of `size` GiB in [`ZONE`], answering its id and the ARN the account knows it
/// by — the ARN a snapshot of it names as its source, read off EC2's own answer.
async fn create_volume(router: &MultiServiceRouter, case: &str, size: i32) -> (String, String) {
    let body = ec2_ok(
        router,
        case,
        "CreateVolume",
        &vec![
            p("AvailabilityZone", ZONE),
            p("Size", size.to_string()),
        ],
    )
    .await;
    let volume_id = element(&body, "volumeId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVolume reports a volumeId, got {body}"));
    let volume_arn = element(&body, "volumeArn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVolume reports a volumeArn, got {body}"));
    (volume_id, volume_arn)
}

/// The ARN the account knows the volume `volume_id` by, read back through `DescribeVolumes`.
async fn described_volume_arn(router: &MultiServiceRouter, case: &str, volume_id: &str) -> String {
    let body = ec2_ok(
        router,
        case,
        "DescribeVolumes",
        &vec![p("VolumeId.1", volume_id)],
    )
    .await;
    element(&body, "volumeArn")
        .unwrap_or_else(|| panic!("case {case:?}: DescribeVolumes reports a volumeArn, got {body}"))
}

/// The ARN the account knows the snapshot `snapshot_id` of `region` by, as EC2 addresses a
/// snapshot: `arn:aws:ec2:<region>:<account>:snapshot/<id>`.
fn snapshot_arn(region: &str, snapshot_id: &str) -> String {
    format!(
        "arn:aws:ec2:{region}:{account}:snapshot/{snapshot_id}",
        account = engine::emulator_account_id()
    )
}

/// What a row requires of the event's `cause`: a failed outcome states WHY, and one that did
/// not fail carries the empty string.
#[derive(Debug)]
enum Cause {
    /// Exactly this sentence.
    Exactly(String),
    /// Nothing, there being nothing that caused it.
    None,
}

/// One outcome a row asserts: the snapshot it is about, what it was made from, and what the
/// event must say of it.
#[derive(Debug)]
struct Expected {
    snapshot_id: String,
    source_arn: String,
    event: &'static str,
    state: &'static str,
    result: &'static str,
    cause: Cause,
    /// The Region the snapshot itself belongs to, which is the Region the call was made in.
    region: &'static str,
}

/// THE RULE over the actions that start a snapshot and the outcomes they can reach.
#[tokio::test]
async fn every_snapshot_action_publishes_the_outcome_of_each_snapshot_it_started() {
    let router = emulator();
    let url = watched_queue(&router, "the snapshot notifications", "ebs-snapshot-events").await;

    let mut expected: Vec<Expected> = Vec::new();

    // A single create of one volume of the account: the snapshot completes, and its source is
    // the volume it is a snapshot of.
    let case = "a create of one volume";
    let (volume_id, volume_arn) = create_volume(&router, case, 4).await;
    let body = ec2_ok(
        &router,
        case,
        "CreateSnapshot",
        &vec![p("VolumeId", volume_id.clone())],
    )
    .await;
    let created = element(&body, "snapshotId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSnapshot reports its snapshot, got {body}"));
    expected.push(Expected {
        snapshot_id: created.clone(),
        source_arn: volume_arn,
        event: CREATE_SNAPSHOT_EVENT,
        state: COMPLETED,
        result: SUCCEEDED,
        cause: Cause::None,
        region: REGION,
    });

    // The multi-volume create: one snapshot per volume the instance holds an attachment of —
    // the root volume its launch gave it — and one outcome published per snapshot.
    let case = "a create of an instance's volumes";
    let body = ec2_ok(
        &router,
        case,
        "RunInstances",
        &vec![
            p("ImageId", "ami-0123456789abcdef0"),
            p("MinCount", "1"),
            p("MaxCount", "1"),
        ],
    )
    .await;
    let instance_id = element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reports its instance, got {body}"));
    let body = ec2_ok(
        &router,
        case,
        "CreateSnapshots",
        &vec![p("InstanceSpecification.InstanceId", instance_id)],
    )
    .await;
    let set: Vec<String> = elements(&body, "snapshotId");
    assert!(
        !set.is_empty(),
        "case {case:?}: an instance holds the root volume its launch created, so the set is \
         not empty, got {body}"
    );
    let sources: Vec<String> = elements(&body, "volumeId");
    assert_eq!(
        sources.len(),
        set.len(),
        "case {case:?}: every snapshot of the set names the volume it is of, got {body}"
    );
    for (snapshot_id, source_volume) in set.iter().zip(&sources) {
        expected.push(Expected {
            snapshot_id: snapshot_id.clone(),
            source_arn: described_volume_arn(&router, case, source_volume).await,
            event: CREATE_SNAPSHOTS_EVENT,
            state: COMPLETED,
            result: SUCCEEDED,
            cause: Cause::None,
            region: REGION,
        });
    }

    // A copy of a snapshot that IS there: it completes, and its source is the snapshot it was
    // copied from in the Region the request named.
    let case = "a copy of a stored snapshot";
    let body = ec2_ok(
        &router,
        case,
        "CopySnapshot",
        &vec![
            p("SourceSnapshotId", created.clone()),
            p("SourceRegion", REGION),
        ],
    )
    .await;
    let copy = element(&body, "snapshotId")
        .unwrap_or_else(|| panic!("case {case:?}: CopySnapshot reports its snapshot, got {body}"));
    expected.push(Expected {
        snapshot_id: copy,
        source_arn: snapshot_arn(REGION, &created),
        event: COPY_SNAPSHOT_EVENT,
        state: COMPLETED,
        result: SUCCEEDED,
        cause: Cause::None,
        region: REGION,
    });

    // And a copy of a snapshot that is NOT there, from each of the two Regions a source can
    // live in: the call is accepted, and the failure is the copy's own outcome.
    for (case, source_region) in [
        ("a copy of a missing snapshot of this Region", REGION),
        ("a copy of a missing snapshot of another Region", ANOTHER_REGION),
    ] {
        let body = ec2_ok(
            &router,
            case,
            "CopySnapshot",
            &vec![
                p("SourceSnapshotId", MISSING_SNAPSHOT_ID),
                p("SourceRegion", source_region),
            ],
        )
        .await;
        let failing = element(&body, "snapshotId").unwrap_or_else(|| {
            panic!("case {case:?}: a copy of a missing source is still accepted, got {body}")
        });
        expected.push(Expected {
            snapshot_id: failing,
            source_arn: snapshot_arn(source_region, MISSING_SNAPSHOT_ID),
            event: COPY_SNAPSHOT_EVENT,
            state: ERROR,
            result: FAILED,
            cause: Cause::Exactly(MISSING_SOURCE_CAUSE.to_string()),
            region: REGION,
        });
    }

    // Every event delivered while another row was being waited for, so a row's own event is
    // never missed because an earlier poll drained it.
    let mut spare: Vec<Value> = Vec::new();
    // The id each row's own event carried: the ids tell two outcomes apart, so no two rows
    // may report the same one.
    let mut request_ids: Vec<String> = Vec::new();
    for row in &expected {
        let case = row.event;
        let arn = snapshot_arn(row.region, &row.snapshot_id);
        let mut mine: Vec<Value> = spare
            .iter()
            .filter(|event| names_snapshot(event, &arn))
            .cloned()
            .collect();
        if mine.is_empty() {
            let (delivered, others) = delivered_for(&router, case, &url, &arn).await;
            mine = delivered;
            spare.extend(others);
        }
        assert_eq!(
            mine.len(),
            1,
            "case {case:?}: exactly one notification is published for {arn}, got {mine:?}"
        );
        let event = &mine[0];
        assert_eq!(
            event["source"].as_str(),
            Some(EC2_EVENT_SOURCE),
            "case {case:?}: expected the source {EC2_EVENT_SOURCE}, got {event}"
        );
        assert_eq!(
            event["detail-type"].as_str(),
            Some(SNAPSHOT_NOTIFICATION),
            "case {case:?}: expected the detail-type {SNAPSHOT_NOTIFICATION}, got {event}"
        );
        assert_eq!(
            event["resources"].as_array().map(Vec::len),
            Some(1),
            "case {case:?}: the event names the one snapshot it is about, got {event}"
        );
        let detail = &event["detail"];
        let checks: [(&str, &str); 5] = [
            ("event", row.event),
            ("state", row.state),
            ("result", row.result),
            ("snapshot_id", arn.as_str()),
            ("source", row.source_arn.as_str()),
        ];
        for (member, expected_text) in checks {
            assert_eq!(
                detail[member].as_str(),
                Some(expected_text),
                "case {case:?}: expected the detail {member} {expected_text:?}, got {event}"
            );
        }
        let cause = detail["cause"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: the detail carries a cause member, got {event}")
        });
        match &row.cause {
            Cause::Exactly(sentence) => assert_eq!(
                cause, sentence,
                "case {case:?}: expected the cause {sentence:?}, got {cause:?}"
            ),
            Cause::None => assert!(
                cause.is_empty(),
                "case {case:?}: an outcome that did not fail has no cause, got {cause:?}"
            ),
        }

        // The interval the action ran over: the snapshot was initiated before its outcome
        // fell due, and both are stamped as the one kind of timestamp.
        let start = detail["startTime"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: the detail carries a startTime member, got {event}")
        });
        let end = detail["endTime"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: the detail carries an endTime member, got {event}")
        });
        assert!(
            start <= end,
            "case {case:?}: a snapshot cannot end before it started, got {start:?} and {end:?}"
        );
        for (member, text) in [("startTime", start), ("endTime", end)] {
            assert!(
                text.len() == "2026-01-01T00:00:00Z".len() && text.ends_with('Z'),
                "case {case:?}: {member} is an RFC3339 instant at second precision, got {text:?}"
            );
        }

        // The event names the CALL it is the outcome of, in the form a request id takes —
        // asserted through the ONE reading of that form this tree has
        // (`service_common::query_error_envelope::is_request_id`) — and no two outcomes carry
        // the same id.
        let request_id = detail["request-id"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: the detail carries a request-id member, got {event}")
        });
        assert!(
            ::service_common::query_error_envelope::is_request_id(request_id),
            "case {case:?}: expected a request id of the 8-4-4-4-12 form, got {request_id:?}"
        );
        assert!(
            !request_ids.contains(&request_id.to_string()),
            "case {case:?}: {request_id:?} was already reported for another outcome, got \
             {request_ids:?}"
        );
        request_ids.push(request_id.to_string());

        // The event and the API agree: what the outcome reported is the state a later read of
        // the snapshot reports, so nothing is published that the service does not stand by.
        let read_back = settled_state(&router, case, &row.snapshot_id).await;
        assert_eq!(
            read_back, row.state,
            "case {case:?}: DescribeSnapshots must report {:?} for {}, got {read_back:?}",
            row.state, row.snapshot_id
        );
    }
}

/// THE RULE's other half: the event is the outcome of a snapshot, not of a call — so a call
/// that is REFUSED starts no snapshot and publishes nothing.
#[tokio::test]
async fn a_refused_snapshot_action_publishes_nothing() {
    let case = "a refused snapshot action";
    let router = emulator();
    let url = watched_queue(&router, case, "ebs-snapshot-refusals").await;

    // One create that IS served, so the queue is known to be wired: its own outcome is the
    // only thing that may arrive.
    let (volume_id, volume_arn) = create_volume(&router, case, 1).await;
    let body = ec2_ok(
        &router,
        case,
        "CreateSnapshot",
        &vec![p("VolumeId", volume_id.clone())],
    )
    .await;
    let created = element(&body, "snapshotId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSnapshot reports its snapshot, got {body}"));

    // The refusals: a volume the account does not hold, a value that is no snapshot id at all
    // for a copy, and a probe that reports what the call WOULD have done — each with the
    // status EC2 answers it with, a probe being `412` rather than a `400`.
    let refused: [(&str, &str, u16, Params); 3] = [
        (
            "a volume the account does not hold",
            "CreateSnapshot",
            400,
            vec![p("VolumeId", "vol-0123456789abcdef0")],
        ),
        (
            "a copy whose source is not a snapshot id",
            "CopySnapshot",
            400,
            vec![
                p("SourceSnapshotId", "not-a-snapshot"),
                p("SourceRegion", REGION),
            ],
        ),
        (
            "a create that only probes",
            "CreateSnapshot",
            412,
            vec![p("VolumeId", volume_id.clone()), p("DryRun", "true")],
        ),
    ];
    for (refusal, action, expected_status, params) in refused {
        let (status, body) = call(&router, REGION, action, &params).await;
        assert_eq!(
            status, expected_status,
            "case {refusal:?}: expected HTTP {expected_status} from {action}, got {status} with \
             {body}"
        );
    }

    // The served create's own outcome arrives, which is what proves the wiring, and nothing
    // else does.
    let arn = snapshot_arn(REGION, &created);
    let (mine, others) = delivered_for(&router, case, &url, &arn).await;
    assert_eq!(
        mine.len(),
        1,
        "case {case:?}: one snapshot was created, so one notification is published, got {mine:?}"
    );
    assert!(
        others.is_empty(),
        "case {case:?}: a refused action starts no snapshot, so it publishes nothing, got \
         {others:?}"
    );
    assert_eq!(
        mine[0]["detail"]["source"].as_str(),
        Some(volume_arn.as_str()),
        "case {case:?}: the one event published is the create's own, got {:?}",
        mine[0]
    );

    // Both the refusals and the create have been answered, so anything they published is
    // already on the queue: a second drain reports it if there is one.
    let after = drain(&router, case, &url).await;
    assert!(
        after.is_empty(),
        "case {case:?}: nothing beyond the one create's outcome is published, got {after:?}"
    );
}
