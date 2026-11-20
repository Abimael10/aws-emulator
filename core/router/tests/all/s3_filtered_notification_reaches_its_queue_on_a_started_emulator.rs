// SPDX-License-Identifier: CC0-1.0

//! Key-name filtered bucket notifications reach their Amazon SQS queue on the emulator a client
//! STARTS: the built binary, on its own port, with request authentication on, reached over TLS
//! with SigV4-signed calls.
//!
//! The RULE under test: **a `QueueConfiguration` is published exactly one `Records` message per
//! object change its `Events` names whose key passes EVERY rule of its key-name `Filter` —
//! "the Amazon S3 API supports filtering based on the prefix and suffix of the object key name"
//! (`NotificationConfigurationFilter`) — and none at all for a change whose key fails one of
//! them, while every key written is a real object either way; the queue's own access policy,
//! narrowed to the publishing bucket by `aws:SourceArn`, is what admits those publications.**
//!
//! [`crate::s3_notification_converts_on_a_started_emulator`] pins the function destination of
//! the same emitter, and the S3 crate's own suites pin this rule over a router assembled inside
//! a test process with authentication off. The emulator a client runs is a different
//! configuration of the same code: the publication Amazon S3 makes is signed with the
//! credentials THAT process vends, the queue's resource policy decides it, and it is made on the
//! binary's own multi-thread runtime, on a task of its own, after the write was already
//! answered. This suite is the one that pins the chain THERE, so a notification that is
//! delivered in-process and not on the started emulator is a failure a test reports rather than
//! one only a client sees.
//!
//! The table spans what the rule generalises over: which rules the filter carries (a prefix and
//! a suffix together, a prefix alone, a suffix alone) and the form of their values (a one-segment
//! prefix, a deeper one, a plain extension, a two-part one), and each row carries a key every
//! rule admits beside keys that fail one of them in each way a key can — the prefix, the suffix,
//! both, and a key holding the suffix inside the name rather than at its end. Each row's rejected
//! keys are written FIRST, so the admitted key's record is the barrier that proves the delivery
//! path ran: their absence from the queue is then an absence rather than a silence. A second
//! admitted key follows, because a configuration that delivered once has to go on delivering.
//!
//! ONE emulator is started for the whole table: starting the binary costs what starting the
//! emulator costs, while each row's own bucket and queue cost a handful of calls.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::elements;

/// The configuration id every row installs, which each record it publishes reports.
const CONFIGURATION_ID: &str = "only-what-the-filter-admits";

/// The event class every row subscribes to — the wildcard form, which "stands for every event of
/// that type", so the row's filter is the only thing that can exclude a write.
const CREATED: &str = "s3:ObjectCreated:*";

/// The `eventName` a `PutObject` of an admitted key must deliver.
const CREATED_BY_PUT: &str = "ObjectCreated:Put";

/// The service principal Amazon S3 publishes a notification under, which the queue's own policy
/// has to allow.
const S3_PRINCIPAL: &str = "s3.amazonaws.com";

/// The handshake a validated queue destination receives when the configuration is installed.
const TEST_EVENT: &str = "s3:TestEvent";

/// The XML namespace a client puts on an S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// How long a wait for a delivery lasts, and how often it looks.
///
/// Generous on purpose: every wait is on the DELIVERY, looked for again until it is there, so a
/// fast machine never spends the span and a loaded one is given room.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How long one `ReceiveMessage` long-polls, which is what a consumer of such a queue does.
const WAIT_TIME_SECONDS: u32 = 5;

/// One row of the filter table: the rules the configuration's `Filter` carries, the keys they
/// admit, and the keys they do not.
struct Row {
    case: &'static str,
    /// The bucket and the queue this row wires together, kept apart from every other row's.
    name: &'static str,
    /// `(Name, Value)` per `FilterRule`, as a request spells them: the two values
    /// `FilterRuleName` publishes (`smithy_query(query="shape",shape="FilterRuleName")` reports
    /// `[prefix, suffix]`) are the lower-case vocabulary.
    filter_rules: &'static [(&'static str, &'static str)],
    /// Two keys every rule of the filter admits: the first proves the chain, the second that it
    /// kept working after the suppressed writes.
    admitted: [&'static str; 2],
    /// Keys at least one rule rejects, each of which must deliver nothing.
    rejected: &'static [&'static str],
}

/// The table.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a prefix and a suffix, both of which must hold",
            name: "started-notification-filter-prefix-and-suffix",
            filter_rules: &[("prefix", "incoming/"), ("suffix", ".csv")],
            admitted: ["incoming/report.csv", "incoming/second.csv"],
            rejected: &[
                "other/report.csv",
                "incoming/report.json",
                "other/report.json",
                "report.csv",
                "incoming/nested/report.csv.bak",
            ],
        },
        Row {
            case: "a deeper prefix and a two-part suffix",
            name: "started-notification-filter-deep-prefix",
            filter_rules: &[("prefix", "logs/2026/"), ("suffix", ".json.gz")],
            admitted: ["logs/2026/day-1.json.gz", "logs/2026/april/day-2.json.gz"],
            rejected: &[
                "logs/2025/day-1.json.gz",
                "logs/2026/day-1.json",
                "logs/2026/day-1.json.gz.tmp",
                "day-1.json.gz",
            ],
        },
        Row {
            case: "a prefix alone, which admits every suffix under it",
            name: "started-notification-filter-prefix-only",
            filter_rules: &[("prefix", "staging/")],
            admitted: ["staging/anything.bin", "staging/deeper/again.csv"],
            rejected: &["production/anything.bin", "anything.bin"],
        },
        Row {
            case: "a suffix alone, which admits the key wherever it sits",
            name: "started-notification-filter-suffix-only",
            filter_rules: &[("suffix", ".parquet")],
            admitted: ["warehouse/part-0.parquet", "part-1.parquet"],
            rejected: &[
                "warehouse/part-0.parquet.crc",
                "warehouse/part-0.orc",
                "parquet",
            ],
        },
    ]
}

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

/// One S3 call that must answer 200, answered with its body.
fn s3_ok(emulator: &Started, case: &str, method: &str, path: &str, body: &str) -> String {
    let (status, text) = emulator.s3(method, path, body);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {method} {path}, got {status} with {text}; the \
         log:\n{}",
        emulator.log()
    );
    text
}

/// The notification configuration a row installs: one queue destination, subscribed to the
/// created class, filtered by the row's own rules.
fn configuration(queue_arn: &str, filter_rules: &[(&str, &str)]) -> String {
    let mut body = format!(
        "<NotificationConfiguration xmlns=\"{XMLNS}\"><QueueConfiguration>\
         <Id>{CONFIGURATION_ID}</Id><Queue>{queue_arn}</Queue><Event>{CREATED}</Event>"
    );
    body.push_str("<Filter><S3Key>");
    for (name, value) in filter_rules {
        body.push_str(&format!(
            "<FilterRule><Name>{name}</Name><Value>{value}</Value></FilterRule>"
        ));
    }
    body.push_str("</S3Key></Filter></QueueConfiguration></NotificationConfiguration>");
    body
}

/// Every message waiting on the queue at `url` right now, parsed as the JSON each carries and
/// deleted as it is handed over, so one delivery is read once.
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
    let mut bodies = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        bodies.push(serde_json::from_str::<Value>(body).unwrap_or_else(|error| {
            panic!("case {case:?}: a delivered body is not JSON ({error}): {body}")
        }));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            );
        }
    }
    bodies
}

/// What one row has read off its queue so far: every record, and how many installation
/// handshakes arrived — "when you configure an event notification on a bucket, Amazon S3 sends
/// the following test message" (Amazon S3 User Guide, *Event notification types and
/// destinations*), ONCE, so a second one would be a notification no change produced.
#[derive(Default)]
struct Read {
    records: Vec<(String, String, String)>,
    handshakes: usize,
}

/// Drain the queue once, classify everything it handed over into `read`, and answer the records
/// of the first body that carried any.
fn collect(
    emulator: &Started,
    case: &str,
    bucket: &str,
    url: &str,
    read: &mut Read,
) -> Vec<(String, String, String)> {
    let mut answered = Vec::new();
    for body in drain(emulator, case, url) {
        if body["Event"].as_str() == Some(TEST_EVENT) {
            read.handshakes += 1;
            assert_eq!(
                body["Bucket"].as_str(),
                Some(bucket),
                "case {case:?}: the test notification must name the bucket it was configured \
                 on, got {body}"
            );
        }
        let records = records_of(&body);
        read.records.extend(records.iter().cloned());
        if answered.is_empty() && !records.is_empty() {
            answered = records;
        }
    }
    answered
}

/// The `(key, eventName, configurationId)` of each record one delivered body carries, which is
/// empty for the installation handshake — it carries no `Records` at all.
fn records_of(body: &Value) -> Vec<(String, String, String)> {
    body["Records"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|record| {
            (
                record["s3"]["object"]["key"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                record["eventName"].as_str().unwrap_or_default().to_string(),
                record["s3"]["configurationId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// Poll the queue until a body carrying `Records` arrives, failing the case at the deadline.
///
/// Everything seen on the way is kept in `read`, so a record no leg waited for — one of a key the
/// filter rejects — is read by the assertions at the end of the row rather than dropped here.
fn awaited_records(
    emulator: &Started,
    case: &str,
    bucket: &str,
    url: &str,
    read: &mut Read,
) -> Vec<(String, String, String)> {
    let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let answered = collect(emulator, case, bucket, url, read);
        if !answered.is_empty() {
            return answered;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected a Records notification on {url:?} within \
             {DELIVERY_DEADLINE:?}; the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Assert that `records` is exactly the one record `key` owes.
fn assert_one_record(case: &str, records: &[(String, String, String)], key: &str) {
    assert_eq!(
        records.len(),
        1,
        "case {case:?}: expected exactly one record for {key:?}, got {records:?}"
    );
    let (delivered_key, event_name, configuration_id) = &records[0];
    assert_eq!(
        delivered_key, key,
        "case {case:?}: expected s3.object.key {key:?}, got {delivered_key:?}"
    );
    assert_eq!(
        event_name, CREATED_BY_PUT,
        "case {case:?}: expected eventName {CREATED_BY_PUT:?}, got {event_name:?}"
    );
    assert_eq!(
        configuration_id, CONFIGURATION_ID,
        "case {case:?}: expected s3.configurationId {CONFIGURATION_ID:?}, got \
         {configuration_id:?}"
    );
}

/// Create the row's queue and answer `(url, arn)`, with the policy a client wiring S3 to a queue
/// writes: the publication is a call Amazon S3 makes as its own service principal acting for the
/// publishing bucket, so the grant names that principal and is narrowed to this bucket by
/// `aws:SourceArn`.
fn queue_for(emulator: &Started, case: &str, name: &str, bucket: &str) -> (String, String) {
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
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowS3SendMessage",
            "Effect": "Allow",
            "Principal": { "Service": S3_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": &arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": format!("arn:aws:s3:::{bucket}") } },
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

/// Take the installation handshake off the queue, which is the observable half of the
/// destination validation: "Amazon S3 ... verifies that any ... destination exists, and that the
/// bucket owner has permission to publish to it by sending a test notification"
/// (`PutBucketNotificationConfiguration`'s own documentation).
fn awaited_handshake(emulator: &Started, case: &str, bucket: &str, url: &str, read: &mut Read) {
    let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let records = collect(emulator, case, bucket, url, read);
        if read.handshakes > 0 {
            assert!(
                records.is_empty(),
                "case {case:?}: the first message a validated queue destination receives must be \
                 the test notification, which carries no Records, got {records:?}"
            );
            return;
        }
        assert!(
            records.is_empty(),
            "case {case:?}: installing a queue destination must publish its {TEST_EVENT} before \
             any object record, got {records:?}"
        );
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: installing a queue destination must publish one {TEST_EVENT} to the \
             queue it validated, got nothing inside {DELIVERY_DEADLINE:?}; the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// RULE: on the emulator a client starts, a key-name filtered queue configuration is published
/// one record per admitted write and nothing for a write it rejects, while every key written is
/// a real object.
#[test]
fn a_filtered_queue_configuration_delivers_only_the_keys_it_admits_on_a_started_emulator() {
    let emulator = Started::start();

    for row in rows() {
        let case = row.case;
        let bucket = row.name;
        s3_ok(&emulator, case, "PUT", &format!("/{bucket}"), "");
        let (queue_url, queue_arn) = queue_for(&emulator, case, row.name, bucket);
        s3_ok(
            &emulator,
            case,
            "PUT",
            &format!("/{bucket}?notification"),
            &configuration(&queue_arn, row.filter_rules),
        );

        // Everything this row ever reads off its queue, whichever leg read it: what the keys the
        // filter rejects must be absent from.
        let mut read = Read::default();
        awaited_handshake(&emulator, case, bucket, &queue_url, &mut read);

        // The keys the filter REJECTS, written before the admitted ones: each is a real object,
        // and none of them owes a record.
        for key in row.rejected {
            s3_ok(
                &emulator,
                case,
                "PUT",
                &format!("/{bucket}/{key}"),
                "a,b\n1,2\n",
            );
        }

        // The positive controls: the keys every rule of the filter admits, the second of them
        // written after the suppressed ones, because a configuration that delivered once has to
        // go on delivering.
        for key in row.admitted {
            s3_ok(
                &emulator,
                case,
                "PUT",
                &format!("/{bucket}/{key}"),
                "a,b\n3,4\n",
            );
            let records = awaited_records(&emulator, case, bucket, &queue_url, &mut read);
            assert_one_record(case, &records, key);
        }

        // The negative leg, read against the barriers above: nothing of the rejected keys was
        // ever delivered, while both admitted keys were.
        let leaked: Vec<&(String, String, String)> = read
            .records
            .iter()
            .filter(|(key, ..)| row.rejected.contains(&key.as_str()))
            .collect();
        assert!(
            leaked.is_empty(),
            "case {case:?}: expected the keys the filter rejects to deliver nothing, got \
             {leaked:?}; every record read: {:?}",
            read.records
        );
        // And the installation handshake happened exactly once, for the one destination this
        // configuration attached: a second one would be a message no change produced.
        assert_eq!(
            read.handshakes, 1,
            "case {case:?}: expected exactly one {TEST_EVENT} for the destination this \
             configuration attached, got {}",
            read.handshakes
        );

        // Suppression is a routing decision, not a write failure: every key written is listed.
        let listed = s3_ok(
            &emulator,
            case,
            "GET",
            &format!("/{bucket}?list-type=2"),
            "",
        );
        let keys = elements(&listed, "Key");
        for key in row.rejected.iter().copied().chain(row.admitted) {
            assert!(
                keys.iter().any(|listed| listed == key),
                "case {case:?}: {key:?} is an object whatever the filter withheld, got {keys:?}"
            );
        }
    }
}
