// SPDX-License-Identifier: CC0-1.0

//! What Amazon S3 refuses a BUCKET CONFIGURATION for, and in which words.
//!
//! An error code, an HTTP status and an error message are part of the API contract: a client
//! branches on the code, SDK retry logic on the status, and an operator searches for the
//! message. This suite pins four refusals of the configuration planes whose wording and
//! classification a caller acts on, and the one envelope member a consumer parses:
//!
//! * `CreateBucket` for a name in AWS's OWN bucket namespace — the `sthree-` prefix, Amazon
//!   S3's internal one — is refused 403 `AccessDenied`: the name is spelled the way a bucket
//!   name is spelled and what the caller may not do is HOLD it, which is a different branch
//!   from "the name is malformed, fix it" (`InvalidBucketName`, 400) — the refusal every other
//!   published reserved prefix and suffix earns, each belonging to another spelling or another
//!   S3 resource kind;
//! * `PutBucketReplication` validates the rule's DESTINATION before the configuration is
//!   stored: a destination bucket that does not exist, or that exists without versioning
//!   enabled, could never receive a replica, so the configuration is refused rather than
//!   stored as valid;
//! * `PutBucketNotificationConfiguration` naming a FIFO queue as a destination is refused for
//!   that restriction by name, so the fix (use a standard queue) is readable from the error;
//! * `PutBucketLifecycleConfiguration` whose filter inverts the object-size range is refused
//!   with AWS's own sentence naming the two members to reconcile;
//! * and the `s3:TestEvent` a validated queue destination receives states its `Time` as an
//!   ISO-8601 UTC instant with exactly three fractional-second digits, which is what a
//!   consumer parsing the envelope with a strict millisecond format reads.
//!
//! Every row is driven as a real HTTP request against the router, so what is pinned is the wire
//! document. The notification rows need the destination's own service, so Amazon SQS is mounted
//! beside S3: what they assert is the message that queue really received.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for, and so the Region every destination lives in.
const REGION: &str = "us-east-1";

/// The XML namespace a client puts on an Amazon S3 request payload.
const XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The ARN of the IAM role a replication configuration names.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/s3-replication-role";

/// The code S3 answers a value that is not valid for the parameter carrying it, the code it
/// answers a request whose content it cannot carry out as given, and the HTTP status its
/// published error list carries for both.
const INVALID_ARGUMENT: &str = "InvalidArgument";
const INVALID_REQUEST: &str = "InvalidRequest";
const BAD_REQUEST: u16 = 400;

/// What one call answered.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// The text of the first `<name>…</name>` of the body, or the empty string.
    ///
    /// XML character references are resolved, so what an assertion compares is the text the
    /// element CARRIES rather than the document's escaping of it — an SDK's XML reader
    /// resolves them too.
    fn element(&self, name: &str) -> String {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let Some(start) = self.body.find(&open) else {
            return String::new();
        };
        let rest = &self.body[start + open.len()..];
        match rest.find(&close) {
            Some(end) => unescaped(&rest[..end]),
            None => String::new(),
        }
    }

    /// The `<Code>` of an error document, or the empty string for an answer that is not one.
    fn code(&self) -> String {
        self.element("Code")
    }

    /// The `<Message>` of an error document.
    fn message(&self) -> String {
        self.element("Message")
    }
}

/// `text` with the five XML character references resolved: an element's text content carries
/// them, and what every assertion here is about is the text itself.
fn unescaped(text: &str) -> String {
    text.replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The emulator for this suite: S3 and Amazon SQS mounted on ONE engine exactly as the router
/// mounts them, with request authentication off so each call carries a credential scope (which
/// names the Region and the service) without a computed signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// One call of `method` to the endpoint of `signing_name` at `path`, signed for [`REGION`].
async fn send(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body: String::from_utf8_lossy(&body).to_string(),
    }
}

/// One S3 call carrying an XML (or empty) payload.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answer {
    send(
        router,
        "s3",
        method,
        path,
        &[("content-type", "application/xml")],
        body.as_bytes().to_vec(),
    )
    .await
}

/// One Amazon SQS call (`awsJson1_0`, `POST /` carrying `X-Amz-Target`).
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let target = format!("AmazonSQS.{operation}");
    let answered = send(
        router,
        "sqs",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", target.as_str()),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "{operation}: expected HTTP 200, got {} with {:?}",
        answered.status, answered.body
    );
    ::serde_json::from_str(&answered.body).unwrap_or(Value::Null)
}

/// Create the bucket `name` and assert it came into being.
async fn bucket(router: &MultiServiceRouter, name: &str) {
    let created = s3(router, "PUT", &format!("/{name}"), "").await;
    assert_eq!(
        created.status, 200,
        "the bucket {name:?} must be created, got {} with {:?}",
        created.status, created.body
    );
}

/// Enable versioning on the bucket `name` — the state both ends of a replication need.
async fn enable_versioning(router: &MultiServiceRouter, name: &str) {
    let payload = format!(
        "<VersioningConfiguration xmlns=\"{XMLNS}\">\
         <Status>Enabled</Status></VersioningConfiguration>"
    );
    let versioned = s3(router, "PUT", &format!("/{name}?versioning"), &payload).await;
    assert_eq!(
        versioned.status, 200,
        "the bucket {name:?} must take versioning, got {} with {:?}",
        versioned.status, versioned.body
    );
}

/// One row of the naming table: the case, the name the create carries, and the status and code
/// it earns — `None` for a name the published rules admit.
type NameRow = (String, String, Option<(u16, &'static str)>);

/// The prefix AWS keeps for its OWN buckets, and the other prefixes and suffixes a
/// general-purpose bucket name may not carry: one entry per sentence of the Amazon S3 User
/// Guide's *General purpose bucket naming rules* ("bucket names must not start with the prefix
/// `xn--`. … must not start with the prefix `sthree-`. … must not start with the prefix
/// `amzn-s3-demo-`"; "must not end with the suffix `-s3alias`. … must not end with the suffix
/// `--ol-s3`. … must not end with the suffix `.mrap`. … must not end with the suffix `--x-s3`.
/// … must not end with the suffix `--table-s3`"). The emulator reads the same catalogs from
/// `service_common::s3_bucket_name`, whose own suite pins them against those sentences; they
/// are written out here because this test target does not depend on that crate.
///
/// `sthree-` is Amazon S3's own internal namespace, which is the one that earns the
/// authorization refusal; the rest are spellings or other S3 resource kinds.
const AWS_RESERVED_PREFIX: &str = "sthree-";
const OTHER_RESERVED_PREFIXES: [&str; 2] = ["xn--", "amzn-s3-demo-"];
const RESERVED_SUFFIXES: [&str; 5] = ["-s3alias", "--ol-s3", ".mrap", "--x-s3", "--table-s3"];

/// RULE: a `CreateBucket` whose name is in AWS's OWN bucket namespace is refused 403
/// `AccessDenied` — the name is AWS's, not the caller's — while a name the rules refuse for its
/// SPELLING, the prefixes and suffixes reserved to other S3 resource kinds included, is the 400
/// `InvalidBucketName` a caller fixes by respelling it.
///
/// The rows span what the rule generalises over: AWS's own prefix, every other published
/// reserved prefix and suffix, names carrying that same text somewhere other than the end it is
/// reserved at, and the spelling rules the second refusal belongs to.
#[tokio::test]
async fn a_reserved_bucket_namespace_is_denied_and_a_misspelled_name_is_invalid() {
    let router = emulator();

    // (case, the name the create carries, the status and code it earns — `None` for a name the
    // rules admit)
    let mut rows: Vec<NameRow> = vec![
        (
            "a name the rules admit".to_string(),
            "reserved-namespace-neighbour".to_string(),
            None,
        ),
        (
            "a reserved prefix somewhere other than the start".to_string(),
            "logs-xn--archive".to_string(),
            None,
        ),
        (
            "a reserved suffix somewhere other than the end".to_string(),
            "bucket-s3alias-logs".to_string(),
            None,
        ),
        (
            "an upper-case letter".to_string(),
            "Reserved-Namespace".to_string(),
            Some((400, "InvalidBucketName")),
        ),
        (
            "an underscore".to_string(),
            "reserved_namespace".to_string(),
            Some((400, "InvalidBucketName")),
        ),
        (
            "two adjacent periods".to_string(),
            "reserved..namespace".to_string(),
            Some((400, "InvalidBucketName")),
        ),
    ];
    for prefix in OTHER_RESERVED_PREFIXES {
        rows.push((
            format!("the reserved prefix {prefix:?}"),
            format!("{prefix}namespace-probe"),
            Some((400, "InvalidBucketName")),
        ));
    }
    rows.push((
        format!("AWS's own prefix {AWS_RESERVED_PREFIX:?}"),
        format!("{AWS_RESERVED_PREFIX}namespace-probe"),
        Some((403, "AccessDenied")),
    ));
    for suffix in RESERVED_SUFFIXES {
        rows.push((
            format!("the reserved suffix {suffix:?}"),
            format!("namespace-probe{suffix}"),
            Some((400, "InvalidBucketName")),
        ));
    }

    for (case, name, refused) in rows {
        let created = s3(&router, "PUT", &format!("/{name}"), "").await;
        match refused {
            None => assert_eq!(
                created.status, 200,
                "case {case:?}: S3 holds a bucket under {name:?}, got {} with {:?}",
                created.status, created.body
            ),
            Some((status, code)) => {
                assert_eq!(
                    (created.status, created.code().as_str()),
                    (status, code),
                    "case {case:?}: expected {status} {code:?}, got {} {:?} with {:?}",
                    created.status,
                    created.code(),
                    created.body
                );
                // Refused BEFORE anything was written, so the account holds no bucket under a
                // name it may not have: the listing never names it.
                let listed = s3(&router, "GET", "/", "").await;
                assert!(
                    !listed.body.contains(&format!("<Name>{name}</Name>")),
                    "case {case:?}: a refused create must record nothing, got {:?}",
                    listed.body
                );
            }
        }
    }
}

/// The `ReplicationConfiguration` payload replicating the whole bucket into `destination`,
/// whose `Destination` carries the `extra` elements the row names and whose rule carries
/// `rule_extra`.
fn replication_payload_with(destination: &str, extra: &str, rule_extra: &str) -> String {
    format!(
        "<ReplicationConfiguration xmlns=\"{XMLNS}\"><Role>{ROLE_ARN}</Role>\
         <Rule><Priority>1</Priority><Filter></Filter><Status>Enabled</Status>{rule_extra}\
         <Destination><Bucket>arn:aws:s3:::{destination}</Bucket>{extra}</Destination></Rule>\
         </ReplicationConfiguration>"
    )
}

/// The same payload with a destination carrying nothing beyond its bucket.
fn replication_payload(destination: &str) -> String {
    replication_payload_with(destination, "", "")
}

/// RULE: `PutBucketReplication` stores no rule whose DESTINATION could never receive a replica
/// — the destination bucket has to exist and to have versioning enabled, the requirement both
/// ends of a replication satisfy — so such a configuration is refused `InvalidRequest` (400)
/// and the bucket keeps whatever configuration it had.
///
/// The rows span what the rule generalises over: a destination nothing holds, one that is there
/// with versioning never configured, one whose versioning was SUSPENDED (which keeps no object
/// versions either), and the destination that satisfies both — which is the row that shows the
/// refusals are about the destination and not about the configuration.
#[tokio::test]
async fn a_replication_destination_that_can_take_no_replica_is_refused() {
    let router = emulator();
    let source = "replication-destination-source";
    bucket(&router, source).await;
    enable_versioning(&router, source).await;

    // The destinations, each in the state its row is about.
    bucket(&router, "replication-dest-unversioned").await;
    bucket(&router, "replication-dest-suspended").await;
    let suspended = format!(
        "<VersioningConfiguration xmlns=\"{XMLNS}\">\
         <Status>Suspended</Status></VersioningConfiguration>"
    );
    let answered = s3(
        &router,
        "PUT",
        "/replication-dest-suspended?versioning",
        &suspended,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "the suspended destination must take its versioning status, got {} with {:?}",
        answered.status, answered.body
    );
    bucket(&router, "replication-dest-ready").await;
    enable_versioning(&router, "replication-dest-ready").await;

    // (case, the destination the rule names, whether the configuration is stored)
    let rows: [(&str, &str, bool); 5] = [
        ("a destination nothing holds", "replication-dest-absent", false),
        (
            "a destination whose versioning was never configured",
            "replication-dest-unversioned",
            false,
        ),
        (
            "a destination whose versioning is suspended",
            "replication-dest-suspended",
            false,
        ),
        ("a destination that is the source bucket", source, false),
        (
            "a destination that exists with versioning enabled",
            "replication-dest-ready",
            true,
        ),
    ];

    for (case, destination, stored) in rows {
        let answered = s3(
            &router,
            "PUT",
            &format!("/{source}?replication"),
            &replication_payload(destination),
        )
        .await;
        if !stored {
            assert_eq!(
                (answered.status, answered.code().as_str()),
                (BAD_REQUEST, INVALID_REQUEST),
                "case {case:?}: a rule that could never replicate must be refused, expected \
                 {BAD_REQUEST} {INVALID_REQUEST:?}, got {} {:?} with {:?}",
                answered.status,
                answered.code(),
                answered.body
            );
            assert!(
                answered.message().contains("Destination bucket must"),
                "case {case:?}: the refusal must state the requirement the destination broke, \
                 got {:?}",
                answered.message()
            );            // Nothing was stored, so the read still reports no configuration at all.
            let read = s3(&router, "GET", &format!("/{source}?replication"), "").await;
            assert_eq!(
                read.status, 404,
                "case {case:?}: a refused put records nothing, got {} with {:?}",
                read.status, read.body
            );
            continue;
        }
        assert_eq!(
            answered.status, 200,
            "case {case:?}: a destination that can take a replica is accepted, got {} with {:?}",
            answered.status, answered.body
        );
        // Round trip: the configuration the put named is the one the bucket holds.
        let read = s3(&router, "GET", &format!("/{source}?replication"), "").await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the stored configuration must read back, got {} with {:?}",
            read.status, read.body
        );
        assert!(
            read.body
                .contains(&format!("arn:aws:s3:::{destination}")),
            "case {case:?}: the read must report the destination the put named, got {:?}",
            read.body
        );
    }
}

/// RULE, the other half of the destination's validation: a `Destination` whose MEMBERS do not
/// describe a destination S3 can write is refused `InvalidRequest` (400) with the requirement
/// the container broke, and nothing is stored.
///
/// The rows span what the rule generalises over: each documented pairing a container can state
/// one half of (`ReplicationTime` without the `Metrics` that measure it, an
/// `AccessControlTranslation` without the `Account` whose the replicas become, a rule's
/// `SourceSelectionCriteria` without the `EncryptionConfiguration` the replicas are sealed
/// under), the one storage class no replica may be stored in, and — the rows that show the rule
/// is the PAIRING and not the member — a destination naming `Metrics` on its own and one naming
/// both halves, both of which are stored.
#[tokio::test]
async fn a_replication_destination_whose_members_do_not_pair_is_refused() {
    let router = emulator();
    let source = "replication-members-source";
    let destination = "replication-members-destination";
    for name in [source, destination] {
        bucket(&router, name).await;
        enable_versioning(&router, name).await;
    }

    /// The S3 Replication Time Control container and the metrics that measure it, each with
    /// the 15-minute threshold S3 RTC publishes.
    const REPLICATION_TIME: &str =
        "<ReplicationTime><Status>Enabled</Status><Time><Minutes>15</Minutes></Time>\
         </ReplicationTime>";
    const METRICS: &str =
        "<Metrics><Status>Enabled</Status><EventThreshold><Minutes>15</Minutes></EventThreshold>\
         </Metrics>";

    // (case, the elements the Destination carries beyond its bucket, the elements the rule
    // carries, the sentence the refusal states — empty for a row that is stored)
    let rows: [(&str, &str, &str, &str); 6] = [
        (
            "ReplicationTime with no Metrics beside it",
            REPLICATION_TIME,
            "",
            "Replication destination must contain both ReplicationTime and Metrics or neither.",
        ),
        (
            "AccessControlTranslation with no Account beside it",
            "<AccessControlTranslation><Owner>Destination</Owner></AccessControlTranslation>",
            "",
            "AccessControlTranslation cannot be specified without Account.",
        ),
        (
            "SourceSelectionCriteria with no EncryptionConfiguration beside it",
            "",
            "<SourceSelectionCriteria><SseKmsEncryptedObjects><Status>Enabled</Status>\
             </SseKmsEncryptedObjects></SourceSelectionCriteria>",
            "EncryptionConfiguration must be specified when SourceSelectionCriteria is \
             specified.",
        ),
        (
            "the storage class no replica may be stored in",
            "<StorageClass>FSX_OPENZFS</StorageClass>",
            "",
            "FSX_OPENZFS is not an accepted value when replicating objects.",
        ),
        ("Metrics on its own", METRICS, "", ""),
        (
            "both halves of the time control",
            &const_str_concat(REPLICATION_TIME, METRICS),
            "",
            "",
        ),
    ];

    for (case, extra, rule_extra, refusal) in rows {
        let answered = s3(
            &router,
            "PUT",
            &format!("/{source}?replication"),
            &replication_payload_with(destination, extra, rule_extra),
        )
        .await;
        if !refusal.is_empty() {
            assert_eq!(
                (answered.status, answered.code().as_str()),
                (BAD_REQUEST, INVALID_REQUEST),
                "case {case:?}: expected {BAD_REQUEST} {INVALID_REQUEST:?}, got {} {:?} with \
                 {:?}",
                answered.status,
                answered.code(),
                answered.body
            );
            assert_eq!(
                answered.message(),
                refusal,
                "case {case:?}: expected the sentence {refusal:?}, got {:?}",
                answered.message()
            );
            continue;
        }
        assert_eq!(
            answered.status, 200,
            "case {case:?}: a destination whose members pair is accepted, got {} with {:?}",
            answered.status, answered.body
        );
        // Round trip: the configuration the put named is the one the bucket holds.
        let read = s3(&router, "GET", &format!("/{source}?replication"), "").await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the stored configuration must read back, got {} with {:?}",
            read.status, read.body
        );
        assert!(
            read.body.contains("<Metrics>"),
            "case {case:?}: the read must report the members the put named, got {:?}",
            read.body
        );
    }
}

/// The two element strings joined, so a row can state both halves of a pairing without
/// spelling either twice.
fn const_str_concat(one: &str, other: &str) -> String {
    format!("{one}{other}")
}

/// The queue ARN Amazon SQS keys the queue `name` by: `arn:aws:sqs:<region>:<account>:<name>`.
fn queue_arn(name: &str) -> String {
    format!(
        "arn:aws:sqs:{REGION}:{}:{name}",
        ::engine::emulator_account_id()
    )
}

/// The `NotificationConfiguration` publishing every object creation to the destination `arn`,
/// carried on the member that destination's kind arrives on.
fn destination_notification(element: &str, arn: &str) -> String {
    let container = match element {
        "Topic" => "TopicConfiguration",
        _ => "QueueConfiguration",
    };
    format!(
        "<NotificationConfiguration xmlns=\"{XMLNS}\"><{container}>\
         <{element}>{arn}</{element}><Event>s3:ObjectCreated:*</Event>\
         </{container}></NotificationConfiguration>"
    )
}

/// RULE: a notification destination that names a FIFO resource — a queue or a topic, both of
/// which end in the `.fifo` suffix — is refused for that restriction by name: Amazon S3
/// publishes to neither, which it states in a sentence of its own rather than in the generic
/// destination-validation text. A STANDARD queue that grants S3 the publication is accepted and
/// receives the `s3:TestEvent` probe.
///
/// And that probe's `Time` is an ISO-8601 UTC instant with exactly three fractional-second
/// digits: S3 publishes the instant to the millisecond, so a consumer parsing the envelope with
/// a strict millisecond format reads every event this emulator publishes.
#[tokio::test]
async fn a_fifo_queue_destination_is_refused_and_the_test_event_states_milliseconds() {
    let router = emulator();
    let name = "notification-wording-bucket";
    bucket(&router, name).await;

    // The FIFO queue, created as one, and a standard queue beside it — each granting Amazon S3
    // the publication, so what tells the two rows apart is the KIND of queue and nothing else.
    let mut urls = Vec::new();
    for (queue, attributes) in [
        ("ordered-queue.fifo", json!({ "FifoQueue": "true" })),
        ("standard-queue", json!({})),
    ] {
        let created = sqs(
            &router,
            "CreateQueue",
            &json!({ "QueueName": queue, "Attributes": attributes }),
        )
        .await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("creating {queue:?} must answer a QueueUrl, got {created}"))
            .to_string();
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "s3-publish",
                "Effect": "Allow",
                "Principal": {"Service": "s3.amazonaws.com"},
                "Action": "sqs:SendMessage",
                "Resource": queue_arn(queue),
            }],
        })
        .to_string();
        sqs(
            &router,
            "SetQueueAttributes",
            &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
        )
        .await;
        urls.push(url);
    }

    // The FIFO destinations, one per destination kind that has a FIFO form: the refusal names
    // that kind's own restriction and echoes the ARN beside the member it arrived on, and
    // nothing is recorded.
    //
    // (case, the member the ARN arrives on, the ARN, the sentence S3 states)
    let fifo: [(&str, &str, String, &str); 2] = [
        (
            "a FIFO queue destination",
            "Queue",
            queue_arn("ordered-queue.fifo"),
            "FIFO SQS queues are not supported.",
        ),
        (
            "a FIFO topic destination",
            "Topic",
            format!(
                "arn:aws:sns:{REGION}:{}:ordered-topic.fifo",
                ::engine::emulator_account_id()
            ),
            "FIFO SNS topics are not supported.",
        ),
    ];
    for (case, element, arn, sentence) in fifo {
        let refused = s3(
            &router,
            "PUT",
            &format!("/{name}?notification"),
            &destination_notification(element, &arn),
        )
        .await;
        assert_eq!(
            (refused.status, refused.code().as_str()),
            (BAD_REQUEST, INVALID_ARGUMENT),
            "case {case:?}: expected {BAD_REQUEST} {INVALID_ARGUMENT:?}, got {} {:?} with {:?}",
            refused.status,
            refused.code(),
            refused.body
        );
        assert_eq!(
            refused.message(),
            sentence,
            "case {case:?}: the refusal must state the restriction itself, got {:?}",
            refused.message()
        );
        assert_eq!(
            (refused.element("ArgumentName"), refused.element("ArgumentValue")),
            (element.to_string(), arn.clone()),
            "case {case:?}: the refusal must name the member and echo the ARN, got {:?}",
            refused.body
        );
        let read = s3(&router, "GET", &format!("/{name}?notification"), "").await;
        assert!(
            !read.body.contains(&arn),
            "case {case:?}: a refused put records nothing, got {:?}",
            read.body
        );
    }

    // The STANDARD queue: accepted, and the probe S3 publishes to a validated destination is
    // waiting on it.
    let accepted = s3(
        &router,
        "PUT",
        &format!("/{name}?notification"),
        &destination_notification("Queue", &queue_arn("standard-queue")),
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "case \"a standard queue destination\": expected 200, got {} with {:?}",
        accepted.status, accepted.body
    );

    let received = sqs(
        &router,
        "ReceiveMessage",
        &json!({ "QueueUrl": urls[1].clone(), "MaxNumberOfMessages": 10 }),
    )
    .await;
    let bodies: Vec<Value> = received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|message| message["Body"].as_str())
        .filter_map(|body| ::serde_json::from_str::<Value>(body).ok())
        .collect();
    let probe = bodies
        .iter()
        .find(|body| body["Event"].as_str() == Some("s3:TestEvent"))
        .unwrap_or_else(|| {
            panic!(
                "case \"a standard queue destination\": the validated queue must carry the test \
                 notification, got {bodies:?}"
            )
        });
    let stated = probe["Time"]
        .as_str()
        .unwrap_or_else(|| panic!("the test notification must state a Time, got {probe}"));
    assert!(
        states_milliseconds(stated),
        "case \"the test notification's Time\": expected an ISO-8601 millisecond UTC instant \
         (YYYY-MM-DDTHH:MM:SS.sssZ), got {stated:?}"
    );
}

/// Whether `stated` is an ISO-8601 UTC instant with exactly three fractional-second digits —
/// `YYYY-MM-DDTHH:MM:SS.sssZ`, the form S3's own event records state a time in.
///
/// Read by SHAPE rather than against any one instant: the instant is per-run, the format is the
/// requirement.
fn states_milliseconds(stated: &str) -> bool {
    let bytes = stated.as_bytes();
    if bytes.len() != 24 {
        return false;
    }
    let digits_at = |positions: &[usize]| {
        positions
            .iter()
            .all(|at| bytes[*at].is_ascii_digit())
    };
    digits_at(&[0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18, 20, 21, 22])
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'.'
        && bytes[23] == b'Z'
}

/// RULE: a lifecycle rule whose filter states an object-size range no object size is inside —
/// the minimum at or above the maximum — is refused `InvalidRequest` with AWS's own sentence
/// naming the two members the caller has to reconcile, and nothing is stored.
///
/// The rows span what the rule generalises over: the range inverted and the two bounds EQUAL,
/// stated directly on the filter and inside its `And` conjunction, and the range that is not
/// empty — which is the row that shows the refusals are about the ordering of the bounds.
#[tokio::test]
async fn an_inverted_object_size_range_is_refused_in_aws_words() {
    let router = emulator();
    let name = "lifecycle-size-range-bucket";
    bucket(&router, name).await;

    /// The sentence AWS states for an object-size range whose minimum is not below its
    /// maximum: fixed text naming the two members, with nothing of the request in it.
    const INVERTED: &str =
        "'ObjectSizeLessThan' has to be a value greater than 'ObjectSizeGreaterThan'.";

    // (case, the Filter element the rule carries, whether the configuration is stored)
    let rows: [(&str, &str, bool); 5] = [
        (
            "the bounds inverted on the filter itself",
            "<Filter><And><Prefix>a/</Prefix>\
             <ObjectSizeGreaterThan>1000</ObjectSizeGreaterThan>\
             <ObjectSizeLessThan>100</ObjectSizeLessThan></And></Filter>",
            false,
        ),
        (
            "the bounds equal",
            "<Filter><And><Prefix>a/</Prefix>\
             <ObjectSizeGreaterThan>1024</ObjectSizeGreaterThan>\
             <ObjectSizeLessThan>1024</ObjectSizeLessThan></And></Filter>",
            false,
        ),
        (
            "the bounds inverted by one byte",
            "<Filter><And><Prefix>a/</Prefix>\
             <ObjectSizeGreaterThan>1025</ObjectSizeGreaterThan>\
             <ObjectSizeLessThan>1024</ObjectSizeLessThan></And></Filter>",
            false,
        ),
        (
            "the bounds inverted with no prefix beside them",
            "<Filter><And>\
             <ObjectSizeGreaterThan>2048</ObjectSizeGreaterThan>\
             <ObjectSizeLessThan>1024</ObjectSizeLessThan></And></Filter>",
            false,
        ),
        (
            "a range an object size is inside",
            "<Filter><And><Prefix>a/</Prefix>\
             <ObjectSizeGreaterThan>1024</ObjectSizeGreaterThan>\
             <ObjectSizeLessThan>1025</ObjectSizeLessThan></And></Filter>",
            true,
        ),
    ];

    for (case, filter, stored) in rows {
        let payload = format!(
            "<LifecycleConfiguration xmlns=\"{XMLNS}\"><Rule><ID>sized</ID>{filter}\
             <Status>Enabled</Status><Expiration><Days>1</Days></Expiration></Rule>\
             </LifecycleConfiguration>"
        );
        let answered = s3(&router, "PUT", &format!("/{name}?lifecycle"), &payload).await;
        if stored {
            assert_eq!(
                answered.status, 200,
                "case {case:?}: a range an object size is inside is accepted, got {} with {:?}",
                answered.status, answered.body
            );
            let read = s3(&router, "GET", &format!("/{name}?lifecycle"), "").await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the stored configuration must read back, got {} with {:?}",
                read.status, read.body
            );
            continue;
        }
        assert_eq!(
            (answered.status, answered.code().as_str()),
            (BAD_REQUEST, INVALID_REQUEST),
            "case {case:?}: expected {BAD_REQUEST} {INVALID_REQUEST:?}, got {} \
             {:?} with {:?}",
            answered.status,
            answered.code(),
            answered.body
        );
        assert_eq!(
            answered.message(),
            INVERTED,
            "case {case:?}: expected the message {INVERTED:?}, got {:?}",
            answered.message()
        );
        // Nothing was stored: the bucket still carries no lifecycle configuration.
        let read = s3(&router, "GET", &format!("/{name}?lifecycle"), "").await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: a refused put records nothing, got {} with {:?}",
            read.status, read.body
        );
    }
}
