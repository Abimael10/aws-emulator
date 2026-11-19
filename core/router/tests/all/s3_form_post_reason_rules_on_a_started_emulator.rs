// SPDX-License-Identifier: CC0-1.0

//! The two creation paths of one EventBridge-enabled bucket, routed by the reason their
//! envelopes carry, driven against the emulator a customer STARTS: the built binary, on its own
//! port, with request authentication on, reached over TLS.
//!
//! The rule under test: **a bucket whose notification configuration carries an
//! `EventBridgeConfiguration` ("Enables delivery of events to Amazon EventBridge",
//! `smithy_query(query="shape",shape="NotificationConfiguration")`) publishes every object
//! creation onto the default bus of the bucket's own Region as `source` `aws.s3` / `detail-type`
//! `Object Created`, naming the addressed bucket in `detail.bucket.name`, the RAW key in
//! `detail.object.key` and the API THAT CREATED THE OBJECT in `detail.reason` — `PutObject` for a
//! `PUT` and `POST Object` for a browser form POST (Amazon S3 User Guide, *Using EventBridge*;
//! Amazon S3 API Reference, *POST Object*). Because those reason literals differ, a rule whose
//! pattern pins one of them receives that creation path's events alone, and the rule pinning the
//! other stays empty.**
//!
//! This is a LEG of [`crate::cfn_deployed_pipeline_on_a_started_emulator`], run on the emulator
//! that suite starts rather than on a second one, because starting the binary costs what starting
//! the emulator costs. It needs a STARTED emulator and not an assembled router: the browser form
//! upload carries no `Authorization` header, no `X-Amz-Target` and a `Host` that names no service
//! — so which service claims it is decided on a request a listening process received, over its
//! own TLS connection, with the credentials verified, while the consumers of the target queues
//! are LONG-POLLING on other connections. The in-process sibling
//! `crate::s3_form_post_upload_routes_by_event_bridge_reason` pins the same rule on a router
//! assembled inside the test process, and the S3 crate's `object_event_dual_destinations` pins it
//! beside EventBridge and Amazon SQS alone.
//!
//! The rows are what the rule generalises over rather than one recorded upload: the two creation
//! paths of one bucket, the two ways a client addresses a bucket root for a form upload (the
//! regional endpoint host, and the authority the client was HANDED with no trailing slash after
//! the bucket), and the two grant forms a client writes on a rule's target queue (unconditioned,
//! and scoped by `aws:SourceArn` to the delivering rule). Every wait is on the MESSAGE — a long
//! poll, the wait a consumer of a notification queue makes — and never on the clock; every
//! assertion quotes the emulator's own log, which is where a delivery that could not be made says
//! so.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::{Started, REGION};

/// How long a delivery is waited for, and how often this leg looks while waiting. Every wait is
/// on a CONDITION; the spans are generous — a delivery this leg waits for is made over the
/// loopback interface — so a loaded host spends none of them.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How long the client waits on one `ReceiveMessage` while waiting for a delivery, in seconds —
/// "The duration (in seconds) for which the call waits for a message to arrive in the queue
/// before returning" (`ReceiveMessageRequest$WaitTimeSeconds`). A long poll is how a consumer
/// waits for a delivery it cannot otherwise be told about, so this waits on the MESSAGE rather
/// than on the clock.
const WAIT_TIME_SECONDS: u64 = 5;

/// The wait a look that is NOT waiting for anything makes: a read of what is on the queue at this
/// instant, which is the short poll `WaitTimeSeconds` 0 asks for. Every such look here is taken
/// after a delivery to the SIBLING queue has already been handed over, so there is nothing left
/// to wait for.
const NO_WAIT: u64 = 0;

/// The `source` and `detail-type` Amazon S3 publishes an object creation under, and the `reason`
/// each creating API stamps on it — spelled out here rather than imported, so a wrong constant in
/// the implementation cannot make this leg pass.
const S3_SOURCE: &str = "aws.s3";
const OBJECT_CREATED: &str = "Object Created";
const PUT_REASON: &str = "PutObject";
const POST_REASON: &str = "POST Object";

/// The bus an Amazon Web Services service's own events go to.
const DEFAULT_BUS: &str = "default";

/// The service principal EventBridge hands a queue target over as, which is what the queue's own
/// resource policy admits ("for Amazon SQS queues, EventBridge relies on resource-based
/// policies").
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The XML namespace a client puts on an S3 request payload.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The bucket this leg routes the creations of, and the rules and queues it scopes to each reason.
const BUCKET: &str = "started-form-post-reason-routing";
const PUT_RULE: &str = "started-reason-put";
const POST_RULE: &str = "started-reason-post";

/// The boundary the form below delimits its parts with, and the file part's own name.
const BOUNDARY: &str = "----------emulator-started-form";
const FILE_NAME: &str = "quarterly.csv";

/// The bytes each creation path uploads.
const FILE_BYTES: &str = "browser,form,upload\n1,2,3\n";

/// `text` as JSON, or null for a body that is not JSON at all.
fn parsed(text: &str) -> Value {
    if text.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// One Amazon SQS call (`awsJson1_0`), which must answer 200.
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = emulator.rpc(
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    );
    assert_eq!(
        status, 200,
        "case {case:?}: Amazon SQS {operation} must answer 200, got {status} with {answered}; \
         the log:\n{}",
        emulator.log()
    );
    parsed(&answered)
}

/// One EventBridge call (`awsJson1_1`), which must answer 200.
fn events(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = emulator.rpc(
        "events",
        &format!("AWSEvents.{operation}"),
        "application/x-amz-json-1.1",
        body,
    );
    assert_eq!(
        status, 200,
        "case {case:?}: EventBridge {operation} must answer 200, got {status} with {answered}; \
         the log:\n{}",
        emulator.log()
    );
    parsed(&answered)
}

/// An SQS queue called `name` whose resource policy admits EventBridge's deliveries, answered as
/// its url and the ARN a rule names it by.
///
/// `source_arn` is the rule the grant is scoped to, or `None` for the unconditioned grant: both
/// are forms a client writes, and either admits the deliveries of the rule naming this queue.
fn queue(
    emulator: &Started,
    case: &str,
    name: &str,
    source_arn: Option<&str>,
) -> (String, String) {
    let created = sqs(emulator, case, "CreateQueue", &json!({"QueueName": name}));
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {name} reports no url, got {created}"))
        .to_string();
    let attributes = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: {name} reports no ARN, got {attributes}"))
        .to_string();
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": {"Service": EVENTS_PRINCIPAL},
        "Action": "sqs:SendMessage",
        "Resource": &arn,
    });
    if let Some(source_arn) = source_arn {
        statement["Condition"] = json!({"ArnEquals": {"aws:SourceArn": source_arn}});
    }
    let policy = json!({"Version": "2012-10-17", "Statement": [statement]}).to_string();
    sqs(
        emulator,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    );
    (url, arn)
}

/// An ENABLED rule on the default bus matching this bucket's `Object Created` events whose
/// `detail.reason` is `reason`, delivering to the queue `queue_arn`.
fn reason_scoped_rule(
    emulator: &Started,
    case: &str,
    name: &str,
    reason: &str,
    queue_arn: &str,
) {
    events(
        emulator,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": DEFAULT_BUS,
            "State": "ENABLED",
            "EventPattern": json!({
                "source": [S3_SOURCE],
                "detail-type": [OBJECT_CREATED],
                "detail": { "bucket": { "name": [BUCKET] }, "reason": [reason] },
            })
            .to_string(),
        }),
    );
    let targeted = events(
        emulator,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": DEFAULT_BUS,
            "Targets": [{ "Id": "the-queue", "Arn": queue_arn }],
        }),
    );
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: every target of {name:?} must be accepted, got {targeted}"
    );
}

/// Every envelope waiting on the queue at `url` right now, each parsed as the JSON it carries and
/// DELETED, so a later look finds only later ones. `wait` is the `WaitTimeSeconds` of the poll:
/// [`WAIT_TIME_SECONDS`] where a delivery is being waited for, [`NO_WAIT`] where the look is a
/// read of this instant.
fn drain(emulator: &Started, case: &str, url: &str, wait: u64) -> Vec<Value> {
    let received = sqs(
        emulator,
        case,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": wait,
        }),
    );
    let mut arrived = Vec::new();
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        sqs(
            emulator,
            case,
            "DeleteMessage",
            &json!({
                "QueueUrl": url,
                "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
        );
        arrived.push(parsed(message["Body"].as_str().unwrap_or_default()));
    }
    arrived
}

/// Assert that the queue at `url` holds, hides and delays NOTHING: every one of the three depth
/// attributes a client reads a drained queue's emptiness off (`ApproximateNumberOfMessages`,
/// `ApproximateNumberOfMessagesNotVisible` and `ApproximateNumberOfMessagesDelayed`) reports zero.
fn assert_empty_depth(emulator: &Started, case: &str, url: &str) {
    const DEPTH_ATTRIBUTES: [&str; 3] = [
        "ApproximateNumberOfMessages",
        "ApproximateNumberOfMessagesNotVisible",
        "ApproximateNumberOfMessagesDelayed",
    ];
    let read = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": DEPTH_ATTRIBUTES}),
    );
    for attribute in DEPTH_ATTRIBUTES {
        let actual = read.pointer(&format!("/Attributes/{attribute}")).cloned();
        assert_eq!(
            actual,
            Some(json!("0")),
            "case {case:?}: a drained queue must report {attribute} as 0 on {url}, got {actual:?} \
             in {read}"
        );
    }
}

/// Long-poll `url` until an envelope names the object `key`, and answer everything that arrived.
/// Waits for the CONDITION, not for a span of the clock.
fn awaited(emulator: &Started, case: &str, url: &str, key: &str) -> Vec<Value> {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(emulator, case, url, WAIT_TIME_SECONDS));
        if arrived
            .iter()
            .any(|envelope| envelope.pointer("/detail/object/key") == Some(&json!(key)))
        {
            return arrived;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected an {OBJECT_CREATED:?} envelope naming {key:?} on {url}, got \
             {arrived:?}; the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// The one envelope of `arrived` that names the object `key`, or a panic naming the case.
fn envelope_for<'a>(case: &str, arrived: &'a [Value], key: &str) -> &'a Value {
    arrived
        .iter()
        .find(|envelope| envelope.pointer("/detail/object/key") == Some(&json!(key)))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected an envelope naming {key:?}, got {arrived:?}")
        })
}

/// Assert that `envelope` is the `Object Created` envelope of the creation path whose `reason` is
/// `reason`, published for this leg's bucket.
fn assert_envelope(case: &str, envelope: &Value, reason: &str) {
    for (pointer, expected) in [
        ("/source", json!(S3_SOURCE)),
        ("/detail-type", json!(OBJECT_CREATED)),
        ("/detail/reason", json!(reason)),
        ("/detail/bucket/name", json!(BUCKET)),
    ] {
        let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {pointer} of the {reason:?} envelope to be {expected}, got \
             {actual} in {envelope}"
        );
    }
}

/// The `multipart/form-data` document a browser submits: each field as its own part, and the file
/// as the one part that also states a `filename`.
fn form_document(fields: &[(&str, &str)]) -> Vec<u8> {
    let mut document = Vec::new();
    for (name, value) in fields {
        document.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        document.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        document.extend_from_slice(value.as_bytes());
        document.extend_from_slice(b"\r\n");
    }
    document.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    document.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{FILE_NAME}\"\r\n")
            .as_bytes(),
    );
    document.extend_from_slice(b"Content-Type: text/csv\r\n\r\n");
    document.extend_from_slice(FILE_BYTES.as_bytes());
    document.extend_from_slice(b"\r\n");
    document.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    document
}

/// One browser form upload of `post_key`, submitted at `host``path` with NO `Authorization`
/// header: it must answer the status the form asked for, and the object it stored must read back
/// as the bytes the form attached.
///
/// The `policy` carries the conditions the upload is authorized under and `x-amz-signature` the
/// SIGNATURE the emulator's published credential produces over it:
/// `HMAC-SHA256(derived signing key, base64-encoded policy)`, hex encoded, with the key derived
/// from the date, the Region and the service the form's own `x-amz-credential` names (Amazon S3
/// API Reference, *Authenticating requests: browser-based uploads using POST*). The derivation is
/// the SDK signer's own, so this leg does not re-derive the key schedule.
fn form_post_leg(emulator: &Started, case: &str, host: &str, path: &str, post_key: &str) {
    let (key_id, secret) = emulator.credentials();
    let signed_at = SystemTime::now();
    let date = ::aws_smithy_types::DateTime::from(signed_at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("the signing instant renders as an ISO8601 date-time");
    // `YYYYMMDD` and `YYYYMMDDTHHMMSSZ`, the two forms a credential scope and an `x-amz-date` are
    // written in.
    let stamp: String = date.chars().filter(char::is_ascii_alphanumeric).collect();
    assert!(
        stamp.len() >= 15,
        "case {case:?}: the signing instant must render as a date and a time, got {date:?}"
    );
    let (day, instant) = (&stamp[..8], format!("{}Z", &stamp[..15]));
    let credential = format!("{key_id}/{day}/{REGION}/s3/aws4_request");
    let policy = json!({
        // Far enough ahead that no run of this leg reaches it.
        "expiration": "2999-01-01T00:00:00Z",
        "conditions": [
            { "bucket": BUCKET },
            ["starts-with", "$key", "paths/"],
            { "success_action_status": "201" },
            { "x-amz-algorithm": "AWS4-HMAC-SHA256" },
            { "x-amz-credential": &credential },
            { "x-amz-date": &instant },
            ["content-length-range", 0, 1_048_576],
        ],
    });
    let encoded = ::aws_smithy_types::base64::encode(policy.to_string());
    let signing_key = aws_sigv4::sign::v4::generate_signing_key(secret, signed_at, REGION, "s3");
    let signature = aws_sigv4::sign::v4::calculate_signature(signing_key, encoded.as_bytes());
    let (status, body) = emulator.unsigned(
        host,
        "POST",
        path,
        &[(
            "content-type",
            &format!("multipart/form-data; boundary={BOUNDARY}"),
        )],
        // The field order a browser submits: the policy fields, then the key, then the file.
        form_document(&[
            ("policy", encoded.as_str()),
            ("x-amz-algorithm", "AWS4-HMAC-SHA256"),
            ("x-amz-credential", credential.as_str()),
            ("x-amz-date", instant.as_str()),
            ("x-amz-signature", signature.as_str()),
            ("success_action_status", "201"),
            ("key", post_key),
        ]),
    );
    assert_eq!(
        status, 201,
        "case {case:?}: the form upload must answer the status the form asked for, got {status} \
         with {body}; the log:\n{}",
        emulator.log()
    );
    let (status, stored) = emulator.s3("GET", &format!("/{BUCKET}/{post_key}"), "");
    assert_eq!(
        status, 200,
        "case {case:?}: the posted object must be readable, got {status} with {stored}"
    );
    assert_eq!(
        stored, FILE_BYTES,
        "case {case:?}: the posted object must carry the bytes the form attached, got {stored:?}"
    );
}

/// RULE: on the emulator a client starts, a `PUT` and a browser form POST into one
/// EventBridge-enabled bucket each publish an `Object Created` envelope whose `detail.reason`
/// names the API that created the object, so the rule pinning one reason receives that path's
/// creations and none of the other's.
///
/// A LEG of [`crate::cfn_deployed_pipeline_on_a_started_emulator`], run on the emulator that
/// suite starts.
pub(crate) fn reason_scoped_rules_split_the_creation_paths(emulator: &Started) {
    let case = "the two creation paths of one bucket on a started emulator";

    let (status, body) = emulator.s3("PUT", &format!("/{BUCKET}"), "");
    assert_eq!(
        status, 200,
        "case {case:?}: creating {BUCKET} must succeed, got {status} with {body}; the log:\n{}",
        emulator.log()
    );

    // EventBridge delivery enabled on the bucket, and read back: the container's presence IS the
    // state, so the read-back has to report it.
    let document = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\">\
         <EventBridgeConfiguration></EventBridgeConfiguration>\
         </NotificationConfiguration>"
    );
    let (status, body) = emulator.s3("PUT", &format!("/{BUCKET}?notification"), &document);
    assert_eq!(
        status, 200,
        "case {case:?}: enabling EventBridge delivery must succeed, got {status} with {body}"
    );
    let (status, reported) = emulator.s3("GET", &format!("/{BUCKET}?notification"), "");
    assert_eq!(
        status, 200,
        "case {case:?}: reading the configuration back must succeed, got {status} with {reported}"
    );
    assert!(
        reported.contains("<EventBridgeConfiguration"),
        "case {case:?}: the read-back must report EventBridgeConfiguration, got {reported}"
    );

    // The two rules, each scoped to one of the two creation reasons, and the queue each delivers
    // to. The `PutObject` queue's grant is the unconditioned one and the form POST queue's is
    // scoped to its own rule: both are forms a client writes, and both admit the deliveries of
    // the rule that names the queue.
    let account = engine::EMULATOR_ACCOUNT_ID;
    let post_rule_arn = format!("arn:aws:events:{REGION}:{account}:rule/{POST_RULE}");
    let (put_url, put_queue_arn) = queue(emulator, case, PUT_RULE, None);
    let (post_url, post_queue_arn) = queue(emulator, case, POST_RULE, Some(&post_rule_arn));
    reason_scoped_rule(emulator, case, PUT_RULE, PUT_REASON, &put_queue_arn);
    reason_scoped_rule(emulator, case, POST_RULE, POST_REASON, &post_queue_arn);

    // The PUT leg: the object is created by `PutObject`, so only the rule pinning that reason
    // receives it.
    let put_key = "paths/put.txt";
    let (status, body) = emulator.s3("PUT", &format!("/{BUCKET}/{put_key}"), FILE_BYTES);
    assert_eq!(
        status, 200,
        "case {case:?}: writing {put_key:?} must succeed, got {status} with {body}"
    );
    let on_put = awaited(emulator, case, &put_url, put_key);
    assert_envelope(case, envelope_for(case, &on_put, put_key), PUT_REASON);
    // That envelope has been published and routed, so at this instant the rule pinning the form
    // POST's reason has received nothing.
    let withheld = drain(emulator, case, &post_url, NO_WAIT);
    assert!(
        withheld.is_empty(),
        "case {case:?}: a rule pinning the form POST's reason must receive nothing a PUT \
         produced, got {withheld:?}; the log:\n{}",
        emulator.log()
    );

    // The form-POST rows: submitted as a browser submits it — to the BUCKET, with NO
    // `Authorization` header, as `multipart/form-data` — once at the regional endpoint host a
    // browser reaches a real bucket under, and once at the authority the client was HANDED, which
    // is what a form built from a published endpoint url carries, with no trailing slash after
    // the bucket. Neither names a service in a credential scope, a target or a host label, so the
    // attribution of both rests on the request itself.
    let addressed: [(&str, String, String, &str); 2] = [
        (
            "the regional endpoint host",
            format!("s3.{REGION}.amazonaws.com"),
            format!("/{BUCKET}/"),
            "paths/posted.txt",
        ),
        (
            "the authority the client was handed",
            emulator.authority(),
            format!("/{BUCKET}"),
            "paths/posted-at-the-endpoint.txt",
        ),
    ];
    for (addressing, host, path, post_key) in &addressed {
        let case = &format!("{case}, form POSTed at {addressing}");
        form_post_leg(emulator, case, host, path, post_key);
        let on_post = awaited(emulator, case, &post_url, post_key);
        assert_envelope(case, envelope_for(case, &on_post, post_key), POST_REASON);
    }

    // And the rule pinning the PUT's own reason received nothing of the form uploads, read at the
    // instant their envelopes have already been delivered — and each queue holds, hides and
    // delays nothing at all once drained.
    let unmatched = drain(emulator, case, &put_url, NO_WAIT);
    assert!(
        unmatched.is_empty(),
        "case {case:?}: a rule pinning {PUT_REASON} must receive nothing a form POST produced, \
         got {unmatched:?}; the log:\n{}",
        emulator.log()
    );
    assert_empty_depth(emulator, case, &put_url);
    assert_empty_depth(emulator, case, &post_url);

    // And the rule pinning the PUT's reason is still live: a second write reaches it, carrying
    // only the new key.
    let again = "paths/put-again.txt";
    let (status, body) = emulator.s3("PUT", &format!("/{BUCKET}/{again}"), FILE_BYTES);
    assert_eq!(
        status, 200,
        "case {case:?}: writing {again:?} must succeed, got {status} with {body}"
    );
    let on_again = awaited(emulator, case, &put_url, again);
    assert_envelope(case, envelope_for(case, &on_again, again), PUT_REASON);
    let keys: Vec<Value> = on_again
        .iter()
        .filter_map(|envelope| envelope.pointer("/detail/object/key").cloned())
        .collect();
    assert_eq!(
        keys,
        vec![json!(again)],
        "case {case:?}: the {PUT_REASON} queue must carry only the new key, got {keys:?}"
    );
}
