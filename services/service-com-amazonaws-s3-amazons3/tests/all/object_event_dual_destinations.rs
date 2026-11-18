// SPDX-License-Identifier: CC0-1.0

//! What ONE object write owes a bucket configured with BOTH notification families: the
//! native `Records` message of each matching destination configuration AND the EventBridge
//! envelope the bus routes.
//!
//! The rules under test:
//!
//! * one object creation is reported ONCE to each configured destination: the native record
//!   spells the object's key URL ENCODED (a space as `+`, every character outside the
//!   alphanumerics and `-`, `_`, `.` percent-escaped from its UTF-8 bytes, the key's own `/`
//!   left alone) while the EventBridge envelope carries the key RAW, and both carry the same
//!   `sequencer` — "a string representation of a hexadecimal value used to determine event
//!   sequence" for one key — because one write is one event;
//! * the two families SPELL the same write differently: the record names it
//!   `eventName` = `ObjectCreated:Put` under the `Id` of the configuration it is being
//!   delivered for (`s3.configurationId`) and carries `s3.object.eTag`, while the envelope
//!   names it `detail-type` = `Object Created` with `detail.reason` naming the API that
//!   created the object and carries `detail.object.etag`;
//! * `detail.reason` names the CREATING API, so the two creation paths of one object are
//!   told apart by it — `PutObject` for a PUT and `POST Object` for a browser form POST —
//!   and a rule pinning one reason receives nothing the other produced;
//! * a form upload whose `policy` is SIGNED travels the same path: the signature the
//!   process's own published credential produces over the base64 policy admits the upload,
//!   which then publishes that same `POST Object` creation, and a signature that is not the
//!   one that credential produces is refused with nothing stored;
//! * `PutBucketNotificationConfiguration` REPLACES the whole document, so a put that omits
//!   `EventBridgeConfiguration` makes `GetBucketNotificationConfiguration` stop reporting it
//!   and stops the envelopes, while the destination configurations the replacement carries
//!   keep being delivered under their own `Id`s;
//! * and the encoding a record's key carries is REVERSIBLE: decoding the published key the
//!   way a form value is decoded (`+` back to a space, then the percent escapes back to
//!   their UTF-8 bytes) answers the very key S3 stored, so a consumer that decodes it reads
//!   the object back with it.
//!
//! Every assertion drives real HTTP requests through a router carrying S3 beside the two
//! services a notification travels through ([`super::object_event_notifications::Emulator`]),
//! so what is pinned is the message a consumer receives.
//!
//! The first table's rows differ along what the key encoding generalises over: a plain name,
//! a key under a prefix, a space, a character RFC 3986 leaves unreserved, the reserved
//! characters a query string is built of, a non-ASCII character and the punctuation the
//! encoding leaves alone.

use serde_json::{json, Value};

use super::object_event_notifications::{request_key, queue_arn, Answer, Emulator, REGION, XMLNS};
// The plus-aware unquoting a consumer applies to a published key, which the listing-encoding
// suite already owns: one decoder, not a second copy of it.
use super::object_listing_encoding::decoded;
use super::post_object_form_upload::{form_document, form_media_type};

/// The `Id` the dual-destination configuration carries, and the one the replacement that
/// drops EventBridge carries — a record reports it as its `configurationId`, which is how a
/// consumer tells which configuration delivered it.
const DUAL_ID: &str = "dual-native";
const NATIVE_ONLY_ID: &str = "native-only";

/// The `Event` value every destination configuration below names: "you can enable
/// notifications for every event of a type by using the wildcard".
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// What the two families name one creation by: the `eventName` of a record, the
/// `detail-type` of an envelope, and the `detail.reason` of each creating API.
const RECORD_EVENT_NAME: &str = "ObjectCreated:Put";
const OBJECT_CREATED_DETAIL_TYPE: &str = "Object Created";
const PUT_REASON: &str = "PutObject";
const POST_REASON: &str = "POST Object";

/// How long a wait for a delivery lasts before the suite gives up on it, and how often it
/// looks while waiting. GENEROUS: no assertion here is about how long anything took.
const DELIVERY_BUDGET: ::std::time::Duration = ::std::time::Duration::from_secs(20);
const LOOK_INTERVAL: ::std::time::Duration = ::std::time::Duration::from_millis(25);

/// The bytes every write below stores.
const BODY: &[u8] = b"one write, two destinations\n";

/// The notification document naming `queue_arn` under `id` for object creations, with an
/// `EventBridgeConfiguration` when `event_bridge` — the exact subresource
/// `PutBucketNotificationConfiguration` records.
fn notification_document(id: &str, queue: &str, event_bridge: bool) -> String {
    let bridge = if event_bridge {
        "<EventBridgeConfiguration></EventBridgeConfiguration>"
    } else {
        ""
    };
    format!(
        "<NotificationConfiguration xmlns=\"{XMLNS}\">\
         <QueueConfiguration><Id>{id}</Id><Queue>{queue}</Queue>\
         <Event>{OBJECT_CREATED}</Event></QueueConfiguration>\
         {bridge}</NotificationConfiguration>"
    )
}

/// `PUT /<bucket>/<key>` — one object write, with the key encoded the way a request URI
/// carries it ([`request_key`]), so what S3 stores is the row's own key.
async fn put_object(emulator: &Emulator, case: &str, bucket: &str, key: &str) -> Answer {
    let answer = emulator
        .send(
            "s3",
            "PUT",
            &format!("/{bucket}/{}", request_key(key)),
            &[("content-type", "text/plain")],
            BODY.to_vec(),
        )
        .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: writing {key:?} must succeed, got {} with {}",
        answer.status,
        answer.text()
    );
    answer
}

/// The unquoted entity tag a write answered with: "the ETag reflects changes only to the
/// contents of an object", carried quoted on the wire and unquoted in a notification.
fn written_etag(case: &str, answer: &Answer) -> String {
    answer
        .header("etag")
        .unwrap_or_else(|| panic!("case {case:?}: the write must answer an ETag"))
        .trim_matches('"')
        .to_string()
}

/// One `ReceiveMessage` of up to ten messages off `url`, each parsed as the JSON document it
/// carries. The messages are left received — the queue's visibility timeout keeps a second
/// look from handing them over again — so a wait that collects across looks counts each
/// delivery once.
async fn drain(emulator: &Emulator, url: &str) -> Vec<Value> {
    let received = emulator
        .sqs(
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            let body = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str(body).unwrap_or(Value::Null)
        })
        .collect()
}

/// Every native `Records` message of `collected`: the test notification a validated queue
/// destination also receives is not one.
fn records(collected: &[Value]) -> Vec<&Value> {
    collected
        .iter()
        .filter(|message| message.get("Records").is_some())
        .collect()
}

/// Collect off `url` until `enough` holds over what has arrived or [`DELIVERY_BUDGET`]
/// passes, and answer everything that arrived. Waits for the CONDITION, not for a span.
async fn collect_until(
    emulator: &Emulator,
    url: &str,
    enough: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let deadline = ::std::time::Instant::now() + DELIVERY_BUDGET;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(emulator, url).await);
        if enough(&arrived) || ::std::time::Instant::now() >= deadline {
            return arrived;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The one record of `collected` whose object key is `published`, or a panic naming the case.
fn record_for<'a>(case: &str, collected: &'a [Value], published: &str) -> &'a Value {
    let found: Vec<&Value> = records(collected)
        .into_iter()
        .filter(|message| {
            message.pointer("/Records/0/s3/object/key") == Some(&json!(published))
        })
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: exactly one native record must name the key {published:?}, got \
         {collected:?}"
    );
    found[0]
}

/// The one envelope of `collected` whose `detail.object.key` is `key`, or a panic naming the
/// case.
fn envelope_for<'a>(case: &str, collected: &'a [Value], key: &str) -> &'a Value {
    let found: Vec<&Value> = collected
        .iter()
        .filter(|envelope| envelope.pointer("/detail/object/key") == Some(&json!(key)))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: exactly one envelope must name the key {key:?}, got {collected:?}"
    );
    found[0]
}

/// A member of a delivered document, or `Null` when it carries none.
fn at(document: &Value, pointer: &str) -> Value {
    document.pointer(pointer).cloned().unwrap_or(Value::Null)
}

/// An ENABLED rule on the default bus matching every `aws.s3` "Object Created" event about
/// `bucket`, delivering to `queue`. A rule pinning `detail.reason` names it in `reason`.
async fn rule(emulator: &Emulator, name: &str, bucket: &str, reason: Option<&str>, queue: &str) {
    let mut detail = json!({ "bucket": { "name": [bucket] } });
    if let Some(reason) = reason {
        detail["reason"] = json!([reason]);
    }
    emulator
        .events(
            "PutRule",
            &json!({
                "Name": name,
                "State": "ENABLED",
                "EventPattern": json!({
                    "source": ["aws.s3"],
                    "detail-type": [OBJECT_CREATED_DETAIL_TYPE],
                    "detail": detail,
                })
                .to_string(),
            }),
        )
        .await;
    let targeted = emulator
        .events(
            "PutTargets",
            &json!({
                "Rule": name,
                "Targets": [{ "Id": "the-queue", "Arn": queue }],
            }),
        )
        .await;
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "every target of {name:?} must be accepted, got {targeted}"
    );
}

/// RULE: one PutObject on a bucket carrying both families is reported once to each — the
/// native record spelling the key as a form value escapes it and the envelope carrying it
/// raw, both under the same sequencer, each with its own family's field spelling.
///
/// The rows differ along the form of the key, which is what the encoding generalises over.
#[tokio::test]
async fn one_write_reaches_each_family_with_its_own_key_spelling() {
    // (case, key, the key the native record publishes)
    let rows: &[(&str, &str, &str)] = &[
        ("a plain key", "report.txt", "report.txt"),
        ("a prefixed key", "input/2026/report.txt", "input/2026/report.txt"),
        ("a key with a space", "dual path/report.txt", "dual+path/report.txt"),
        // Every character outside the encoding's own set is escaped, among them the `~`
        // RFC 3986 leaves unreserved.
        ("a key with a tilde", "backup~1.txt", "backup%7E1.txt"),
        (
            "a key built of reserved characters",
            "report 2026+q3=final.txt",
            "report+2026%2Bq3%3Dfinal.txt",
        ),
        ("a non-ASCII key", "rapport-café.txt", "rapport-caf%C3%A9.txt"),
        // `-`, `_` and `.` are the punctuation the encoding leaves alone.
        (
            "a key of the characters the encoding leaves alone",
            "wild-card-1_v.2",
            "wild-card-1_v.2",
        ),
    ];

    let emulator = Emulator::new();
    let bucket = "dual-destination-writes";
    emulator.create_bucket(bucket).await;
    let native_url = emulator.create_queue("dual-native-queue").await;
    let bus_url = emulator.create_queue("dual-bus-queue").await;
    rule(
        &emulator,
        "dual-destination-rule",
        bucket,
        None,
        &queue_arn("dual-bus-queue"),
    )
    .await;
    let configured = emulator
        .s3_xml(
            "PUT",
            &format!("/{bucket}?notification"),
            &notification_document(DUAL_ID, &queue_arn("dual-native-queue"), true),
        )
        .await;
    assert_eq!(
        configured.status,
        200,
        "configuring both families must succeed, got {} with {}",
        configured.status,
        configured.text()
    );

    for (case, key, published) in rows {
        let written = put_object(&emulator, case, bucket, key).await;
        let unquoted = written_etag(case, &written);

        let native = collect_until(&emulator, &native_url, |arrived| {
            records(arrived)
                .iter()
                .any(|record| at(record, "/Records/0/s3/object/key") == json!(published))
        })
        .await;
        let record = record_for(case, &native, published);
        let delivered = collect_until(&emulator, &bus_url, |arrived| {
            arrived
                .iter()
                .any(|envelope| at(envelope, "/detail/object/key") == json!(*key))
        })
        .await;
        let envelope = envelope_for(case, &delivered, key);

        let size = json!(i64::try_from(BODY.len()).expect("the body's size fits"));
        for (pointer, expected) in [
            ("/Records/0/eventName", json!(RECORD_EVENT_NAME)),
            ("/Records/0/s3/configurationId", json!(DUAL_ID)),
            ("/Records/0/s3/bucket/name", json!(bucket)),
            ("/Records/0/s3/object/key", json!(published)),
            ("/Records/0/s3/object/eTag", json!(unquoted)),
            ("/Records/0/s3/object/size", size.clone()),
        ] {
            let actual = at(record, pointer);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the record's {pointer} to be {expected}, got \
                 {actual} in {record}"
            );
        }
        for (pointer, expected) in [
            ("/source", json!("aws.s3")),
            ("/detail-type", json!(OBJECT_CREATED_DETAIL_TYPE)),
            ("/detail/reason", json!(PUT_REASON)),
            ("/detail/bucket/name", json!(bucket)),
            ("/detail/object/key", json!(*key)),
            ("/detail/object/etag", json!(unquoted)),
            ("/detail/object/size", size),
        ] {
            let actual = at(envelope, pointer);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the envelope's {pointer} to be {expected}, got \
                 {actual} in {envelope}"
            );
        }
        // One write is one event, so the two families report the SAME sequencer for it.
        let record_sequencer = at(record, "/Records/0/s3/object/sequencer");
        let envelope_sequencer = at(envelope, "/detail/object/sequencer");
        assert_eq!(
            record_sequencer, envelope_sequencer,
            "case {case:?}: expected one sequencer for one write, got {record_sequencer} on \
             the record and {envelope_sequencer} on the envelope"
        );
        assert!(
            record_sequencer.is_string(),
            "case {case:?}: expected the record to carry a sequencer, got \
             {record_sequencer} in {record}"
        );
    }
}

/// RULE: a put that omits `EventBridgeConfiguration` replaces the document — the read-back
/// stops reporting it and the envelopes stop — while the destination configurations the
/// replacement carries keep delivering, under their own `Id`.
#[tokio::test]
async fn dropping_the_event_bridge_configuration_stops_envelopes_and_not_records() {
    let emulator = Emulator::new();
    let bucket = "dual-destination-removal";
    emulator.create_bucket(bucket).await;
    let native_url = emulator.create_queue("removal-native-queue").await;
    let bus_url = emulator.create_queue("removal-bus-queue").await;
    rule(
        &emulator,
        "removal-rule",
        bucket,
        None,
        &queue_arn("removal-bus-queue"),
    )
    .await;

    // (case, the document to install, whether EventBridge is enabled by it, the
    // configuration id writes under it are delivered for, the key written under it)
    let rows: &[(&str, &str, bool, &str, &str)] = &[
        (
            "both families configured",
            DUAL_ID,
            true,
            DUAL_ID,
            "dual path/report 2026+q3=é~.txt",
        ),
        (
            "the replacement that drops EventBridge",
            NATIVE_ONLY_ID,
            false,
            NATIVE_ONLY_ID,
            "native only/report.txt",
        ),
    ];

    for (case, id, event_bridge, configuration_id, key) in rows {
        let configured = emulator
            .s3_xml(
                "PUT",
                &format!("/{bucket}?notification"),
                &notification_document(id, &queue_arn("removal-native-queue"), *event_bridge),
            )
            .await;
        assert_eq!(
            configured.status,
            200,
            "case {case:?}: installing the configuration must succeed, got {} with {}",
            configured.status,
            configured.text()
        );

        // What the stored subresource reports: the `Id` of the one destination
        // configuration, and the EventBridge container only while the document carries it.
        let read = emulator
            .s3_xml("GET", &format!("/{bucket}?notification"), "")
            .await;
        let document = read.text();
        assert_eq!(
            read.status, 200,
            "case {case:?}: reading the configuration back must succeed, got {} with \
             {document}",
            read.status
        );
        assert!(
            document.contains(&format!("<Id>{id}</Id>")),
            "case {case:?}: expected the read-back to list exactly [{id}], got {document}"
        );
        assert_eq!(
            document.contains("<EventBridgeConfiguration"),
            *event_bridge,
            "case {case:?}: expected the read-back to report EventBridgeConfiguration = \
             {event_bridge}, got {document}"
        );

        let written = put_object(&emulator, case, bucket, key).await;
        let unquoted = written_etag(case, &written);

        // The native record always arrives, under the `Id` of the configuration in force.
        let native = collect_until(&emulator, &native_url, |arrived| {
            !records(arrived).is_empty()
        })
        .await;
        let delivered = records(&native);
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: one write owes exactly one native record, got {native:?}"
        );
        for (pointer, expected) in [
            ("/Records/0/s3/configurationId", json!(*configuration_id)),
            ("/Records/0/eventName", json!(RECORD_EVENT_NAME)),
            ("/Records/0/s3/object/eTag", json!(unquoted)),
        ] {
            let actual = at(delivered[0], pointer);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the record's {pointer} to be {expected}, got \
                 {actual} in {}",
                delivered[0]
            );
        }

        // The envelope arrives only while the bucket carries the EventBridge container. The
        // "nothing arrived" read is made at the instant the NATIVE record of the same write
        // has already been delivered, so it is the absence of an envelope and not a race.
        if *event_bridge {
            let envelopes = collect_until(&emulator, &bus_url, |arrived| {
                arrived
                    .iter()
                    .any(|envelope| at(envelope, "/detail/object/key") == json!(*key))
            })
            .await;
            let envelope = envelope_for(case, &envelopes, key);
            assert_eq!(
                at(envelope, "/detail/reason"),
                json!(PUT_REASON),
                "case {case:?}: expected the envelope to name the creating API, got \
                 {envelope}"
            );
        } else {
            let envelopes = drain(&emulator, &bus_url).await;
            assert!(
                envelopes.is_empty(),
                "case {case:?}: a bucket whose configuration no longer carries \
                 EventBridgeConfiguration must publish no envelope, got {envelopes:?}"
            );
        }
    }
}

/// RULE: `detail.reason` names the API that created the object, so a rule pinning one
/// creation path receives nothing the other produced — `PutObject` for a PUT and
/// `POST Object` for a browser form POST.
///
/// The rows are the two creation paths of one object, each with the reason it must carry and
/// the one it must not reach.
#[tokio::test]
async fn an_object_creation_is_routed_by_the_api_that_created_it() {
    let emulator = Emulator::new();
    let bucket = "creation-reason-routing";
    emulator.create_bucket(bucket).await;
    let put_url = emulator.create_queue("reason-put-queue").await;
    let post_url = emulator.create_queue("reason-post-queue").await;
    rule(
        &emulator,
        "reason-put-rule",
        bucket,
        Some(PUT_REASON),
        &queue_arn("reason-put-queue"),
    )
    .await;
    rule(
        &emulator,
        "reason-post-rule",
        bucket,
        Some(POST_REASON),
        &queue_arn("reason-post-queue"),
    )
    .await;
    let configured = emulator
        .s3_xml(
            "PUT",
            &format!("/{bucket}?notification"),
            &format!(
                "<NotificationConfiguration xmlns=\"{XMLNS}\">\
                 <EventBridgeConfiguration></EventBridgeConfiguration>\
                 </NotificationConfiguration>"
            ),
        )
        .await;
    assert_eq!(
        configured.status,
        200,
        "enabling EventBridge delivery must succeed, got {} with {}",
        configured.status,
        configured.text()
    );

    // The PUT leg: the object is written by `PutObject`, so only the rule pinning that
    // reason receives it.
    let put_key = "paths/put.txt";
    put_object(&emulator, "a PUT of an object", bucket, put_key).await;
    let on_put = collect_until(&emulator, &put_url, |arrived| {
        arrived
            .iter()
            .any(|envelope| at(envelope, "/detail/object/key") == json!(put_key))
    })
    .await;
    let envelope = envelope_for("a PUT of an object", &on_put, put_key);
    assert_eq!(
        at(envelope, "/detail/reason"),
        json!(PUT_REASON),
        "case \"a PUT of an object\": expected the envelope to name PutObject, got {envelope}"
    );
    // The envelope for the PUT has been published and routed, so at this instant the rule
    // pinning the POST's own reason has received nothing.
    let withheld = drain(&emulator, &post_url).await;
    assert!(
        withheld.is_empty(),
        "case \"a PUT of an object\": a rule pinning the form POST's reason must receive \
         nothing a PUT produced, got {withheld:?}"
    );

    // The form-POST leg: the same bucket, written by the browser-based upload, which
    // answers the status the form asked for.
    let post_key = "paths/posted.txt";
    let media_type = form_media_type();
    let answer = emulator
        .send(
            "s3",
            "POST",
            &format!("/{bucket}/"),
            &[("content-type", media_type.as_str())],
            form_document(
                &[("key", post_key), ("success_action_status", "201")],
                Some(("quarterly.csv", BODY, Some("text/csv"))),
            ),
        )
        .await;
    assert_eq!(
        answer.status,
        201,
        "case \"a form POST of an object\": the upload must answer the status the form \
         asked for, got {} with {}",
        answer.status,
        answer.text()
    );
    // The object the upload really stored, which is what makes the envelope below more than
    // a claim: `HEAD` reports the posted bytes' length.
    let head = emulator
        .send(
            "s3",
            "HEAD",
            &format!("/{bucket}/{}", request_key(post_key)),
            &[],
            Vec::new(),
        )
        .await;
    assert_eq!(
        head.status, 200,
        "case \"a form POST of an object\": the posted object must be readable, got {}",
        head.status
    );
    assert_eq!(
        head.header("content-length"),
        Some(BODY.len().to_string()),
        "case \"a form POST of an object\": expected the posted length, got {:?}",
        head.header("content-length")
    );

    let on_post = collect_until(&emulator, &post_url, |arrived| {
        arrived
            .iter()
            .any(|envelope| at(envelope, "/detail/object/key") == json!(post_key))
    })
    .await;
    let posted = envelope_for("a form POST of an object", &on_post, post_key);
    for (pointer, expected) in [
        ("/detail/reason", json!(POST_REASON)),
        ("/detail/bucket/name", json!(bucket)),
        ("/region", json!(REGION)),
    ] {
        let actual = at(posted, pointer);
        assert_eq!(
            actual, expected,
            "case \"a form POST of an object\": expected {pointer} to be {expected}, got \
             {actual} in {posted}"
        );
    }
    // The envelope for the form POST has been published and routed, so at this instant the
    // rule pinning the PUT's reason has received nothing of it.
    let unmatched = drain(&emulator, &put_url).await;
    assert!(
        unmatched.is_empty(),
        "case \"a form POST of an object\": a rule pinning PutObject must receive nothing a \
         form POST produced, got {unmatched:?}"
    );
    // And the PutObject-scoped queue holds nothing: its own approximate depth is zero.
    let attributes = emulator
        .sqs(
            "GetQueueAttributes",
            &json!({
                "QueueUrl": &put_url,
                "AttributeNames": ["ApproximateNumberOfMessages"],
            }),
        )
        .await;
    assert_eq!(
        at(&attributes, "/Attributes/ApproximateNumberOfMessages"),
        json!("0"),
        "case \"a form POST of an object\": expected the PutObject-scoped queue to report no \
         messages, got {attributes}"
    );

    // A later PUT still reaches the PutObject-scoped rule: dropping nothing is what the
    // reason filter does.
    let later_key = "paths/later.txt";
    put_object(&emulator, "a later PUT of an object", bucket, later_key).await;
    let later = collect_until(&emulator, &put_url, |arrived| {
        arrived
            .iter()
            .any(|envelope| at(envelope, "/detail/object/key") == json!(later_key))
    })
    .await;
    let envelope = envelope_for("a later PUT of an object", &later, later_key);
    assert_eq!(
        at(envelope, "/detail/reason"),
        json!(PUT_REASON),
        "case \"a later PUT of an object\": expected the envelope to name PutObject, got \
         {envelope}"
    );
}

/// The signature a POST policy carries: `HMAC-SHA256(derived signing key, base64-encoded
/// policy)`, hex encoded, with the signing key derived from the date, the Region and the
/// service of the credential the form names (Amazon S3 API Reference, *Authenticating
/// requests: browser-based uploads using POST*).
///
/// The key schedule is the SDK signer's own (`aws_sigv4::sign::v4`), so this suite does not
/// carry a second derivation of it.
fn policy_signature(secret: &str, signed_at: ::std::time::SystemTime, encoded_policy: &str) -> String {
    let key = ::aws_sigv4::sign::v4::generate_signing_key(secret, signed_at, REGION, "s3");
    ::aws_sigv4::sign::v4::calculate_signature(key, encoded_policy.as_bytes())
}

/// The two forms the date of a signature is written in: the `YYYYMMDD` of a credential
/// scope and the `YYYYMMDDTHHMMSSZ` of an `x-amz-date`.
fn signing_dates(signed_at: ::std::time::SystemTime) -> (String, String) {
    let rendered = ::aws_smithy_types::DateTime::from(signed_at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("the signing instant renders as an ISO8601 date-time");
    let stamp: String = rendered.chars().filter(char::is_ascii_alphanumeric).collect();
    (stamp[..8].to_string(), format!("{}Z", &stamp[..15]))
}

/// RULE: the `policy` a form carries is enforced against the signature of the credential it
/// names — the upload is admitted only when the signature is the one that credential's
/// secret produces over the base64 policy — and an admitted form POST publishes the same
/// `POST Object` creation a form carrying no signature does.
///
/// The rows differ along what the rule generalises over: whether the signature is the
/// credential's own, and whether it is another credential's.
#[tokio::test]
async fn a_signed_form_policy_is_admitted_only_under_its_own_signature() {
    use ::base64::Engine;

    // The key pair this process publishes, which is what a client of this emulator signs
    // with: minted here rather than assumed, so the derivation below is the real one.
    let published = ::emulator_server::request_authentication::SigV4Authenticator::for_this_process();
    let signed_at = ::std::time::SystemTime::now();
    let (day, instant) = signing_dates(signed_at);

    let emulator = Emulator::new();
    let bucket = "signed-form-uploads";
    emulator.create_bucket(bucket).await;
    let post_url = emulator.create_queue("signed-post-queue").await;
    rule(
        &emulator,
        "signed-post-rule",
        bucket,
        Some(POST_REASON),
        &queue_arn("signed-post-queue"),
    )
    .await;
    let configured = emulator
        .s3_xml(
            "PUT",
            &format!("/{bucket}?notification"),
            &format!(
                "<NotificationConfiguration xmlns=\"{XMLNS}\">\
                 <EventBridgeConfiguration></EventBridgeConfiguration>\
                 </NotificationConfiguration>"
            ),
        )
        .await;
    assert_eq!(
        configured.status,
        200,
        "enabling EventBridge delivery must succeed, got {} with {}",
        configured.status,
        configured.text()
    );

    // (case, the key the form uploads, whether the form signs with the published secret,
    // the status the upload must be answered with)
    let rows: &[(&str, &str, bool, u16)] = &[
        ("the credential's own signature", "signed/accepted.txt", true, 201),
        ("another credential's signature", "signed/refused.txt", false, 403),
    ];

    for (case, key, signed_correctly, status) in rows {
        let credential = format!("{}/{day}/{REGION}/s3/aws4_request", published.access_key_id);
        let policy = json!({
            // Far enough ahead that no run of this suite reaches it.
            "expiration": "2999-01-01T00:00:00Z",
            "conditions": [
                { "bucket": bucket },
                ["starts-with", "$key", "signed/"],
                { "success_action_status": "201" },
                { "x-amz-algorithm": "AWS4-HMAC-SHA256" },
                { "x-amz-credential": &credential },
                { "x-amz-date": &instant },
                ["content-length-range", 0, 1_048_576],
            ],
        });
        let encoded = ::base64::engine::general_purpose::STANDARD.encode(policy.to_string());
        let signature = if *signed_correctly {
            policy_signature(&published.secret_access_key, signed_at, &encoded)
        } else {
            policy_signature("another-credentials-secret", signed_at, &encoded)
        };
        let media_type = form_media_type();
        let answer = emulator
            .send(
                "s3",
                "POST",
                &format!("/{bucket}/"),
                &[("content-type", media_type.as_str())],
                // The field order a browser submits: the policy fields, then the key, then
                // the file.
                form_document(
                    &[
                        ("policy", encoded.as_str()),
                        ("x-amz-algorithm", "AWS4-HMAC-SHA256"),
                        ("x-amz-credential", credential.as_str()),
                        ("x-amz-date", instant.as_str()),
                        ("x-amz-signature", signature.as_str()),
                        ("success_action_status", "201"),
                        ("key", key),
                    ],
                    Some(("quarterly.csv", BODY, Some("text/csv"))),
                ),
            )
            .await;
        assert_eq!(
            answer.status,
            *status,
            "case {case:?}: expected HTTP {status}, got {} with {}",
            answer.status,
            answer.text()
        );

        let head = emulator
            .send(
                "s3",
                "HEAD",
                &format!("/{bucket}/{}", request_key(key)),
                &[],
                Vec::new(),
            )
            .await;
        if *signed_correctly {
            assert_eq!(
                head.status, 200,
                "case {case:?}: the admitted upload must have stored the object, got {}",
                head.status
            );
            // And the admitted upload publishes the creation its own API made.
            let arrived = collect_until(&emulator, &post_url, |collected| {
                collected
                    .iter()
                    .any(|envelope| at(envelope, "/detail/object/key") == json!(*key))
            })
            .await;
            let envelope = envelope_for(case, &arrived, key);
            assert_eq!(
                at(envelope, "/detail/reason"),
                json!(POST_REASON),
                "case {case:?}: expected the envelope to name the POST of an object, got \
                 {envelope}"
            );
        } else {
            assert_eq!(
                head.status, 404,
                "case {case:?}: a refused upload must store nothing, got {}",
                head.status
            );
            let nothing = drain(&emulator, &post_url).await;
            assert!(
                nothing.is_empty(),
                "case {case:?}: a refused upload must publish nothing, got {nothing:?}"
            );
        }
    }
}

/// RULE: a native record's `s3.object.key` is the stored key URL ENCODED — a space as `+`
/// and every other character outside the alphanumerics and `-`, `_`, `.` percent-escaped
/// from its UTF-8 bytes, the key's own `/` left alone — and that encoding is reversible, so
/// decoding it as a form value answers the stored key and a read addressed by that decoded
/// key serves the object the record was published for.
///
/// The rows differ along the characters the encoding has to escape: the reserved characters
/// a query string is built of, the `~` RFC 3986 leaves unreserved but URL encoding does not,
/// a percent sign, multi-byte characters inside and outside the Basic Multilingual Plane,
/// several of them at once in one key, and a key made only of the characters the encoding
/// leaves alone.
#[tokio::test]
async fn a_published_record_key_is_the_reversible_encoding_of_the_stored_key() {
    // (case, the key stored, the key the native record publishes)
    let rows: &[(&str, &str, &str)] = &[
        (
            "a key carrying reserved characters, a multi-byte character and a tilde",
            "dual path/report 2026+q3=é~.txt",
            "dual+path/report+2026%2Bq3%3D%C3%A9%7E.txt",
        ),
        ("a key of tildes", "~backup~", "%7Ebackup%7E"),
        (
            "a tilde beside the characters a query string separates with",
            "a~b&c?d",
            "a%7Eb%26c%3Fd",
        ),
        ("a key carrying a percent sign", "100% done.txt", "100%25+done.txt"),
        (
            "a key of several multi-byte characters",
            "rapport café ñ.txt",
            "rapport+caf%C3%A9+%C3%B1.txt",
        ),
        (
            "a key carrying a character outside the Basic Multilingual Plane",
            "note 🙂.txt",
            "note+%F0%9F%99%82.txt",
        ),
        (
            "a key of only the characters the encoding leaves alone",
            "logs/2026-q3_report.v2",
            "logs/2026-q3_report.v2",
        ),
    ];

    let emulator = Emulator::new();
    let bucket = "record-key-encoding";
    emulator.create_bucket(bucket).await;
    let native_url = emulator.create_queue("record-key-encoding-queue").await;
    let configured = emulator
        .s3_xml(
            "PUT",
            &format!("/{bucket}?notification"),
            &notification_document(
                NATIVE_ONLY_ID,
                &queue_arn("record-key-encoding-queue"),
                false,
            ),
        )
        .await;
    assert_eq!(
        configured.status,
        200,
        "configuring the queue destination must succeed, got {} with {}",
        configured.status,
        configured.text()
    );

    for (case, key, published) in rows {
        put_object(&emulator, case, bucket, key).await;

        // The wait is for a record ABOUT this write to arrive — NOT for one spelling its key
        // a particular way — so a key published wrongly fails the assertion below with the
        // spelling it really carried instead of burning the deadline. The previous row took
        // its own record off the queue before this one wrote, so the one record this write
        // publishes is the one that arrives here.
        let arrived =
            collect_until(&emulator, &native_url, |collected| !records(collected).is_empty()).await;
        let delivered = records(&arrived);
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: exactly one record must report the write of {key:?}, got {arrived:?}"
        );
        let record = delivered[0];
        assert_eq!(
            at(record, "/Records/0/s3/configurationId"),
            json!(NATIVE_ONLY_ID),
            "case {case:?}: expected the record to name the configuration that delivered it, \
             got {record}"
        );
        let reported = at(record, "/Records/0/s3/object/key");
        assert_eq!(
            reported,
            json!(published),
            "case {case:?}: expected the record to publish the key as {published:?}, got \
             {reported}"
        );

        // The encoding is reversible: decoding what the emulator REPORTED — not the row's
        // own literal — answers the key S3 stored …
        let decoded_key = decoded(reported.as_str().unwrap_or_else(|| {
            panic!("case {case:?}: a record's key must be a string, got {reported}")
        }));
        assert_eq!(
            decoded_key, *key,
            "case {case:?}: expected decoding {reported} to answer the stored key {key:?}, \
             got {decoded_key:?}"
        );
        // … and a read addressed by that decoded key serves the object the record reported.
        let served = emulator
            .send(
                "s3",
                "GET",
                &format!("/{bucket}/{}", request_key(&decoded_key)),
                &[],
                Vec::new(),
            )
            .await;
        assert_eq!(
            served.status,
            200,
            "case {case:?}: reading the decoded key {decoded_key:?} must serve the object, got \
             {} with {}",
            served.status,
            served.text()
        );
        assert_eq!(
            served.bytes(),
            BODY,
            "case {case:?}: expected the decoded key to serve the written bytes, got {:?}",
            served.text()
        );
    }
}
