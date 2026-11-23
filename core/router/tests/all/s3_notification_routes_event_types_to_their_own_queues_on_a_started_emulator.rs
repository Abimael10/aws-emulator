// SPDX-License-Identifier: CC0-1.0

//! Per-event-class queue routing on the emulator a client STARTS: the built binary, on its own
//! port, with request authentication on, reached over TLS with SigV4-signed calls and drained
//! by LONG-POLLING consumers.
//!
//! The RULE under test: **every entry of a bucket's notification configuration is matched
//! against an object mutation on its own `Events`, independently of the other entries, and the
//! mutation publishes exactly one `Records` message per matching entry — stamped with that
//! entry's `Id` as `s3.configurationId` and with the `eventName` of the mutation — and nothing
//! at all to an entry whose `Events` do not cover it.**
//!
//! [`crate::s3_notification_routes_each_event_type_to_its_own_queue`] pins the same rule over a
//! table of containers on a router assembled inside the test process, with authentication off.
//! The emulator a client RUNS is a different configuration of the same code: each publication
//! Amazon S3 makes is signed with the credentials that process vends, the queue's own resource
//! policy decides it, and it is made on the binary's own multi-thread runtime, on a task that
//! outlives the write that produced it. This leg is the one that pins the per-class split
//! THERE, so a record delivered in-process and not on the started emulator is a failure a test
//! reports rather than one only a client sees.
//!
//! It runs as a LEG of [`crate::cfn_deployed_pipeline_on_a_started_emulator`], on the emulator
//! that suite already starts, rather than starting a second binary.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;

/// The XML namespace a client puts on an Amazon S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The service principal Amazon S3 publishes a bucket's notifications under, which each
/// queue's own policy has to allow.
const S3_PRINCIPAL: &str = "s3.amazonaws.com";

/// The validation handshake a queue receives when it is attached as a destination: it carries
/// no `Records` and is not an object event.
const TEST_EVENT: &str = "s3:TestEvent";

/// How long a wait for a delivery lasts, and how often it looks. Generous on purpose: every
/// wait is on the DELIVERY, looked for again until it is there, so a fast machine never spends
/// the span and a loaded one is given room.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How long one `ReceiveMessage` long-polls, which is what a consumer of such a queue does.
const WAIT_TIME_SECONDS: u32 = 5;

/// The bucket this leg wires, and the two queues it fans out to.
const BUCKET: &str = "started-per-event-class-routing";
const CREATED_QUEUE: &str = "started-per-event-class-created";
const REMOVED_QUEUE: &str = "started-per-event-class-removed";

/// The `Id` each entry is recorded under, which is what the records it is published report as
/// `s3.configurationId`.
const CREATED_ID: &str = "created";
const REMOVED_ID: &str = "removed";

/// One Amazon SQS call that must answer 200.
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    let (status, text) = emulator.rpc(
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    );
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}; the \
         log:\n{}",
        emulator.log()
    );
    parsed
}

/// One S3 call that must answer `expected`.
fn s3_ok(emulator: &Started, case: &str, expected: u16, method: &str, path: &str, body: &str) {
    let (status, text) = emulator.s3(method, path, body);
    assert_eq!(
        status, expected,
        "case {case:?}: expected HTTP {expected} from {method} {path}, got {status} with \
         {text}; the log:\n{}",
        emulator.log()
    );
}

/// Create a queue whose own access policy lets Amazon S3 publish this bucket's notifications
/// to it, and answer `(url, arn)`.
fn queue_for(emulator: &Started, case: &str, name: &str) -> (String, String) {
    let created = sqs(emulator, case, "CreateQueue", &json!({ "QueueName": name }));
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    // The publication is a call Amazon S3 makes as its own service principal acting for the
    // publishing bucket, so the grant names that principal and is narrowed to this bucket by
    // `aws:SourceArn`; an S3 bucket's ARN carries no Region and no account.
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowS3SendMessage",
            "Effect": "Allow",
            "Principal": { "Service": S3_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": &arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": format!("arn:aws:s3:::{BUCKET}") } },
        }],
    })
    .to_string();
    sqs(
        emulator,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    );
    (url, arn)
}

/// What one queue has been handed so far: every record, and how many validation handshakes
/// arrived.
#[derive(Default)]
struct Held {
    records: Vec<(String, String)>,
    handshakes: usize,
}

impl Held {
    /// Long-poll the queue once, deleting each message as it is handed over so one delivery is
    /// read once, and classify everything it carried.
    fn collect(&mut self, emulator: &Started, case: &str, url: &str) {
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
        for message in &messages {
            let text = message["Body"].as_str().unwrap_or_default();
            let body: Value = serde_json::from_str(text).unwrap_or_else(|error| {
                panic!("case {case:?}: a delivered body is not JSON ({error}): {text}")
            });
            if body["Event"].as_str() == Some(TEST_EVENT) && body.get("Records").is_none() {
                self.handshakes += 1;
            }
            for record in body["Records"].as_array().cloned().unwrap_or_default() {
                self.records.push((
                    record["eventName"].as_str().unwrap_or_default().to_string(),
                    record["s3"]["configurationId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                ));
            }
            if let Some(handle) = message["ReceiptHandle"].as_str() {
                sqs(
                    emulator,
                    case,
                    "DeleteMessage",
                    &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
                );
            }
        }
    }

    /// Look again until this queue holds `wanted` records, failing the case at the deadline.
    fn awaited(&mut self, emulator: &Started, case: &str, url: &str, wanted: usize) {
        let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
        loop {
            self.collect(emulator, case, url);
            // Read AFTER the look, so a look that finally delivers everything owed is not
            // reported as a missing delivery because the clock passed the deadline during it.
            if self.records.len() >= wanted {
                return;
            }
            assert!(
                Instant::now() < giving_up_at,
                "case {case:?}: expected {wanted} record(s) on {url:?} within \
                 {DELIVERY_DEADLINE:?}, got {:?}; the log:\n{}",
                self.records,
                emulator.log()
            );
            std::thread::sleep(LOOK_INTERVAL);
        }
    }
}

/// RULE: on the emulator a client starts, a container carrying one queue entry per event class
/// publishes each mutation to exactly the entry whose `Events` cover it, and to no other.
///
/// `pub(crate)` because it is a LEG of the suite that starts the binary: it is handed that
/// emulator rather than starting a second one.
pub(crate) fn per_event_class_entries_route_independently(emulator: &Started) {
    let case = "one queue entry per event class, on a started emulator";
    s3_ok(emulator, case, 200, "PUT", &format!("/{BUCKET}"), "");
    let (created_url, created_arn) = queue_for(emulator, case, CREATED_QUEUE);
    let (removed_url, removed_arn) = queue_for(emulator, case, REMOVED_QUEUE);

    // The container: two independent destinations, one per event class. The elements are the
    // ones S3's own model binds the list to
    // (`smithy_query(query="shape",shape="NotificationConfiguration")`: `QueueConfigurations`
    // -> `QueueConfiguration`, whose `QueueArn` is `Queue` and whose `Events` is `Event`).
    let configuration = format!(
        "<NotificationConfiguration xmlns=\"{XMLNS}\">\
         <QueueConfiguration><Id>{CREATED_ID}</Id><Queue>{created_arn}</Queue>\
         <Event>s3:ObjectCreated:*</Event></QueueConfiguration>\
         <QueueConfiguration><Id>{REMOVED_ID}</Id><Queue>{removed_arn}</Queue>\
         <Event>s3:ObjectRemoved:*</Event></QueueConfiguration>\
         </NotificationConfiguration>"
    );
    s3_ok(
        emulator,
        case,
        200,
        "PUT",
        &format!("/{BUCKET}?notification"),
        &configuration,
    );

    // The handshake each newly attached destination receives, which is what a consumer finds
    // waiting on a validated destination before any object changes.
    let mut created = Held::default();
    let mut removed = Held::default();
    for (held, url, id) in [
        (&mut created, &created_url, CREATED_ID),
        (&mut removed, &removed_url, REMOVED_ID),
    ] {
        let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
        loop {
            held.collect(emulator, case, url);
            if held.handshakes > 0 {
                break;
            }
            assert!(
                Instant::now() < giving_up_at,
                "case {case:?}: attaching the queue of entry {id:?} must publish one \
                 {TEST_EVENT} to it, got nothing inside {DELIVERY_DEADLINE:?}; the log:\n{}",
                emulator.log()
            );
            std::thread::sleep(LOOK_INTERVAL);
        }
        assert!(
            held.records.is_empty(),
            "case {case:?}: the first message the queue of entry {id:?} receives must be the \
             handshake, which carries no Records, got {:?}",
            held.records
        );
    }

    // The two mutations, each owing its one record to the entry whose class covers it. Each is
    // read back on its own queue before the next is made, and the hold at the end reads both
    // queues again past both barriers.
    let key = "route-me.txt";
    s3_ok(emulator, case, 200, "PUT", &format!("/{BUCKET}/{key}"), "x");
    created.awaited(emulator, case, &created_url, 1);
    s3_ok(emulator, case, 204, "DELETE", &format!("/{BUCKET}/{key}"), "");
    removed.awaited(emulator, case, &removed_url, 1);

    // One more look at each, past both barriers: nothing of the other class leaked and no
    // record was published twice. Each look is itself a long poll, so the window a late
    // delivery would have to arrive in is the poll's own.
    created.collect(emulator, case, &created_url);
    removed.collect(emulator, case, &removed_url);

    assert_eq!(
        created.records,
        vec![("ObjectCreated:Put".to_string(), CREATED_ID.to_string())],
        "case {case:?}: the queue of entry {CREATED_ID:?} owes exactly the created-class \
         record; the log:\n{}",
        emulator.log()
    );
    assert_eq!(
        removed.records,
        vec![("ObjectRemoved:Delete".to_string(), REMOVED_ID.to_string())],
        "case {case:?}: the queue of entry {REMOVED_ID:?} owes exactly the removed-class \
         record; the log:\n{}",
        emulator.log()
    );
    for (held, id) in [(&created, CREATED_ID), (&removed, REMOVED_ID)] {
        assert_eq!(
            held.handshakes, 1,
            "case {case:?}: the queue of entry {id:?} must receive exactly one {TEST_EVENT} \
             for the one time it was attached, got {}",
            held.handshakes
        );
    }
}
