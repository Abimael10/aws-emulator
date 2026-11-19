// SPDX-License-Identifier: CC0-1.0

//! The QUEUE destination's own access policy as the gate on INSTALLING a bucket notification
//! configuration — the half of destination validation that is about PERMISSION rather than
//! existence.
//!
//! RULE under test: **`PutBucketNotificationConfiguration` validates every destination the
//! container names before it stores anything — "after Amazon S3 receives this request, it first
//! verifies that any Amazon Simple Notification Service (Amazon SNS) or Amazon Simple Queue
//! Service (Amazon SQS) destination exists, and that the bucket owner has permission to publish
//! to it by sending a test notification … If the message fails, the entire PUT action will fail,
//! and Amazon S3 will not add the configuration to your bucket" — so a container naming a queue
//! that EXISTS and whose own access policy does not admit `sqs:SendMessage` by the Amazon S3
//! service principal for this bucket is refused WHOLE with HTTP 400 `InvalidArgument` and the
//! sentence S3 states for that class, the configuration already in force survives untouched, and
//! the queue S3 may not publish to receives nothing. The opt-out the model binds to
//! `x-amz-skip-destination-validation` ("skips validation of Amazon SQS, Amazon SNS, and Lambda
//! destinations", `PutBucketNotificationConfigurationRequest$SkipDestinationValidation`) records
//! such a container as given, and the grant then decides the publication instead.**
//!
//! Two crates have to agree for that to hold — Amazon S3 judges the container and makes the
//! publication, and Amazon SQS holds the queue and the access policy the decision is read from —
//! so the suite lives here: a crate-local suite can only ever see one side of it. Its siblings
//! reach the OTHER destination kinds' install-time half
//! ([`crate::cloudformation_sns_topic_policy_admits_s3_notifications`] for a topic,
//! [`crate::s3_notification_lambda_invoke_grant_gates_delivery`] for a function) and the
//! publication-time half for a queue
//! ([`crate::s3_notification_delivery_follows_the_destination_grant`], where the grant moves
//! under a configuration that is already installed). What only this suite reaches is the QUEUE
//! destination being judged at INSTALL time: a queue that is there but grants Amazon S3 nothing.
//!
//! The rows differ along what the rule generalises over — which form the destination queue's own
//! access policy takes: no policy at all, a grant for another action of the queue's own service,
//! `sqs:SendMessage` granted to another service's principal, the grant narrowed by
//! `aws:SourceArn` to another bucket, an explicit deny beside the grant, the grant this bucket
//! needs, every action of the service granted unconditioned, and no grant at all with validation
//! skipped. Every row drives the IDENTICAL two-destination container, whose first destination is
//! a queue known to be granted, so each verdict is read against a chain seen to deliver.
//!
//! Every request is dispatched through a BOXED future, and the suite runs on a thread sized like
//! the emulator's own boot path ([`driven`]): the chain this drives — S3's validation and its
//! delivery into SQS, both dispatched back into the assembled router — nests more future than a
//! default test thread's stack holds.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, opt_element};
use crate::s3_notification_invokes_its_lambda_target::{
    authorization, build_router, create_bucket, driven, s3, Answer,
};

/// The Region every request here is signed for, and so the Region every queue below lives in.
/// It is the Region the shared S3 harness signs for too.
const REGION: &str = "us-east-1";

/// The XML namespace a client puts on an Amazon S3 request payload.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The service principal Amazon S3 publishes a bucket's notifications under — "for Amazon Web
/// Services services, the principal is a domain-style identifier that the service defines, such
/// as s3.amazonaws.com" (`AddPermission$Principal`) — and ANOTHER service's principal, which is
/// the row where the grant exists and is not Amazon S3's.
const S3_PRINCIPAL: &str = "s3.amazonaws.com";
const OTHER_PRINCIPAL: &str = "sns.amazonaws.com";

/// The action Amazon S3 performs on a queue destination to publish a notification to it, beside
/// ANOTHER action of that same service (the row where the grant does not name the publication)
/// and the wildcard that stands for every action of the service.
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";
const OTHER_QUEUE_ACTION: &str = "sqs:ReceiveMessage";
const EVERY_QUEUE_ACTION: &str = "sqs:*";

/// The condition keys such a grant is narrowed by: the ONE publishing resource, and the account
/// that owns it.
const SOURCE_ARN_KEY: &str = "aws:SourceArn";
const SOURCE_ACCOUNT_KEY: &str = "aws:SourceAccount";

/// The event class every configuration below is notified of: "you can enable notifications for
/// every event of a type by using the wildcard" (`Event`).
const OBJECT_CREATED: &str = "s3:ObjectCreated:*";

/// The key-name prefix each destination of the two-destination container is scoped to, so the
/// two configurations overlap in nothing and the ambiguous-filter refusal cannot be what fires.
const GRANTED_PREFIX: &str = "ord/";
const CANDIDATE_PREFIX: &str = "deny/";

/// The `Id` each configuration of that container carries.
const GRANTED_ID: &str = "granted-queue";
const CANDIDATE_ID: &str = "candidate-queue";

/// The `Event` of the test notification a validated destination receives when the configuration
/// is installed, which is drained before any object is written.
const TEST_EVENT: &str = "s3:TestEvent";

/// The refusal class S3 answers a container it could not validate with: the code, the status and
/// the FIXED sentence of the class ("the message, not the code alone, is what identifies this
/// class" — S3 reuses `InvalidArgument` for an ambiguous key-name filter as well).
const INVALID_ARGUMENT: &str = "InvalidArgument";
const INVALID_ARGUMENT_STATUS: u16 = 400;
const UNABLE_TO_VALIDATE: &str = "Unable to validate the following destination configurations";

/// The per-destination detail that refusal carries beside the fixed sentence for a destination
/// that IS there and whose policy withholds the publication — the `<ArgumentValue{n}>` of the
/// numbered series, whose `<ArgumentName{n}>` is the ARN the request named.
const UNPERMITTED_DETAIL: &str =
    "Permissions on the destination queue do not allow S3 to publish notifications from \
     this bucket";

/// The header the model binds `SkipDestinationValidation` to
/// (`smithy_query(query="operation",operation="PutBucketNotificationConfiguration")`).
const SKIP_VALIDATION_HEADER: &str = "x-amz-skip-destination-validation";

/// The bucket an `aws:SourceArn`-narrowed grant names in the row where that key names ANOTHER
/// bucket than the one publishing. Nothing creates it: the point is the ARN the condition holds.
const FOREIGN_BUCKET: &str = "s3-queue-grant-some-other-bucket";

/// How long a row waits for work the clock owes — the asynchronous delivery of an object event,
/// and the install-time test notification — and how often it looks while waiting. Deliberately
/// generous: this suite shares its host with the rest of the test target, and nothing here tests
/// how fast that host is.
const DELIVERY_BUDGET: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long a read that must see NOTHING keeps looking. It runs only after the same row has seen
/// the delivery the surviving configuration owes, so the absence is read against a chain known
/// to be live.
const QUIET_WINDOW: Duration = Duration::from_secs(5);

/// What the candidate destination queue's OWN access policy says — the only thing that differs
/// between the rows, and so the only thing each verdict can be about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuePolicy {
    /// The queue carries no access policy at all, so nothing grants the publication.
    Absent,
    /// The Amazon S3 principal granted ANOTHER action of the queue's own service.
    OtherAction,
    /// `sqs:SendMessage` granted, to another service's principal.
    OtherPrincipal,
    /// `sqs:SendMessage` granted to Amazon S3, narrowed by `aws:SourceArn` to another bucket.
    ForeignSourceArn,
    /// The grant this bucket needs, beside an explicit deny of the same call.
    DeniedBesideTheGrant,
    /// The grant this bucket needs: `sqs:SendMessage` for the Amazon S3 principal, narrowed to
    /// this bucket and the account that owns it.
    Granted,
    /// Every action of the queue's own service granted to Amazon S3, unconditioned.
    EveryQueueAction,
}

impl QueuePolicy {
    /// The document to set on the queue, or `None` for the queue that carries none.
    fn document(self, queue_arn: &str, bucket: &str) -> Option<String> {
        let statements = match self {
            Self::Absent => return None,
            Self::OtherAction => vec![statement(
                "Allow",
                S3_PRINCIPAL,
                OTHER_QUEUE_ACTION,
                queue_arn,
                Some(bucket),
            )],
            Self::OtherPrincipal => vec![statement(
                "Allow",
                OTHER_PRINCIPAL,
                SEND_MESSAGE_ACTION,
                queue_arn,
                Some(bucket),
            )],
            Self::ForeignSourceArn => vec![statement(
                "Allow",
                S3_PRINCIPAL,
                SEND_MESSAGE_ACTION,
                queue_arn,
                Some(FOREIGN_BUCKET),
            )],
            Self::DeniedBesideTheGrant => vec![
                statement(
                    "Allow",
                    S3_PRINCIPAL,
                    SEND_MESSAGE_ACTION,
                    queue_arn,
                    Some(bucket),
                ),
                statement(
                    "Deny",
                    S3_PRINCIPAL,
                    SEND_MESSAGE_ACTION,
                    queue_arn,
                    Some(bucket),
                ),
            ],
            Self::Granted => vec![statement(
                "Allow",
                S3_PRINCIPAL,
                SEND_MESSAGE_ACTION,
                queue_arn,
                Some(bucket),
            )],
            Self::EveryQueueAction => vec![statement(
                "Allow",
                S3_PRINCIPAL,
                EVERY_QUEUE_ACTION,
                queue_arn,
                None,
            )],
        };
        Some(
            json!({ "Version": "2012-10-17", "Statement": statements })
                .to_string(),
        )
    }
}

/// One statement of a queue's access policy: `effect` on `action` for `principal` on `resource`,
/// narrowed to the notifications of `bucket` where one is named.
///
/// An S3 bucket's ARN carries no Region and no account, because a bucket name is globally
/// unique; the account the conditions name is the one this emulator serves
/// ([`engine::emulator_account_id`]), which is every bucket's reported owner.
fn statement(
    effect: &str,
    principal: &str,
    action: &str,
    resource: &str,
    bucket: Option<&str>,
) -> Value {
    let mut written = json!({
        "Sid": format!("{effect}{}", action.replace([':', '*'], "")),
        "Effect": effect,
        "Principal": { "Service": principal },
        "Action": action,
        "Resource": resource,
    });
    if let Some(bucket) = bucket {
        written["Condition"] = json!({
            "ArnEquals": { SOURCE_ARN_KEY: format!("arn:aws:s3:::{bucket}") },
            "StringEquals": { SOURCE_ACCOUNT_KEY: engine::emulator_account_id() },
        });
    }
    written
}

/// One row of the table: which policy the candidate queue carries, whether the put asks for the
/// documented opt-out, and whether the container is RECORDED.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    policy: QueuePolicy,
    skip_validation: bool,
    recorded: bool,
}

/// The rows, spanning the forms a destination queue's own policy takes. Every one drives the
/// same two-destination container; only the second destination's policy (and, in the last row,
/// the opt-out header) differs.
const ROWS: &[Row] = &[
    Row {
        case: "a destination queue carrying no access policy at all",
        policy: QueuePolicy::Absent,
        skip_validation: false,
        recorded: false,
    },
    Row {
        case: "a grant for another action of the queue's own service",
        policy: QueuePolicy::OtherAction,
        skip_validation: false,
        recorded: false,
    },
    Row {
        case: "sqs:SendMessage granted to another service's principal",
        policy: QueuePolicy::OtherPrincipal,
        skip_validation: false,
        recorded: false,
    },
    Row {
        case: "a grant whose aws:SourceArn names another bucket",
        policy: QueuePolicy::ForeignSourceArn,
        skip_validation: false,
        recorded: false,
    },
    Row {
        case: "an explicit deny beside the grant",
        policy: QueuePolicy::DeniedBesideTheGrant,
        skip_validation: false,
        recorded: false,
    },
    Row {
        case: "the grant this bucket's notifications need",
        policy: QueuePolicy::Granted,
        skip_validation: false,
        recorded: true,
    },
    Row {
        case: "every action of the queue's own service, granted unconditioned",
        policy: QueuePolicy::EveryQueueAction,
        skip_validation: false,
        recorded: true,
    },
    Row {
        case: "no grant at all, with destination validation skipped",
        policy: QueuePolicy::Absent,
        skip_validation: true,
        recorded: true,
    },
];

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must be answered.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A queue called `name` carrying `policy`: the url and the ARN Amazon SQS reports for it.
async fn queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    policy: QueuePolicy,
    bucket: &str,
) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let reported = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = reported["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn, got {reported}"))
        .to_string();
    if let Some(document) = policy.document(&arn, bucket) {
        sqs(
            router,
            case,
            "SetQueueAttributes",
            &json!({ "QueueUrl": url, "Attributes": { "Policy": document } }),
        )
        .await;
    }
    (url, arn)
}

/// One `QueueConfiguration` of the container, as the request's XML carries it. The elements are
/// the ones S3's own model binds the list to
/// (`smithy_query(query="shape",shape="QueueConfiguration")`: the ARN member is bound as
/// `Queue`, the event list as a flattened `Event`).
fn configuration(id: &str, queue_arn: &str, prefix: &str) -> String {
    format!(
        "<QueueConfiguration><Id>{id}</Id><Queue>{queue_arn}</Queue>\
         <Event>{OBJECT_CREATED}</Event><Filter><S3Key><FilterRule><Name>prefix</Name>\
         <Value>{prefix}</Value></FilterRule></S3Key></Filter></QueueConfiguration>"
    )
}

/// `PUT /<bucket>?notification` with `configured` as the whole container, asking for the
/// documented opt-out when `skip_validation` — which is the only thing the header changes.
async fn put_notification(
    router: &MultiServiceRouter,
    bucket: &str,
    configured: &[String],
    skip_validation: bool,
) -> Answer {
    let body = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\">{}</NotificationConfiguration>",
        configured.concat()
    );
    let host = format!("s3.{REGION}.amazonaws.com");
    let mut building = http::Request::builder()
        .method("PUT")
        .uri(format!("https://{host}/{bucket}?notification"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream");
    if skip_validation {
        building = building.header(SKIP_VALIDATION_HEADER, "true");
    }
    let request = building
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// The queue ARNs `GetBucketNotificationConfiguration` reports for `bucket`, in the order the
/// document carries them.
async fn reported_queues(router: &MultiServiceRouter, case: &str, bucket: &str) -> Vec<String> {
    let answer = s3(router, "GET", &format!("/{bucket}?notification"), "").await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetBucketNotificationConfiguration({bucket:?}) must answer 200, got {} \
         with {}",
        answer.status, answer.body
    );
    elements(&answer.body, "Queue")
}

/// `PUT /<bucket>/<key>` — the object write whose event the configuration in force publishes.
async fn put_object(router: &MultiServiceRouter, case: &str, bucket: &str, key: &str) {
    let answer = s3(
        router,
        "PUT",
        &format!("/{bucket}/{key}"),
        "the object the notification is published for",
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutObject({bucket:?}, {key:?}) must succeed, got {} with {}",
        answer.status, answer.body
    );
}

/// Take every message waiting on the queue at `url` off it, as the text each body carries.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let mut found = Vec::new();
    loop {
        let answered = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let messages = answered["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            return found;
        }
        for message in messages {
            found.push(message["Body"].as_str().unwrap_or_default().to_string());
            if let Some(receipt) = message["ReceiptHandle"].as_str() {
                sqs(
                    router,
                    case,
                    "DeleteMessage",
                    &json!({ "QueueUrl": url, "ReceiptHandle": receipt }),
                )
                .await;
            }
        }
    }
}

/// Every object key the `Records` envelopes in `bodies` name. A body that is not such an
/// envelope — the `s3:TestEvent` probe a validated destination receives on install — names none.
fn keys_of(bodies: &[String]) -> Vec<String> {
    bodies
        .iter()
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .flat_map(|parsed| parsed["Records"].as_array().cloned().unwrap_or_default())
        .map(|record| {
            record["s3"]["object"]["key"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Drain the queue at `url` until the `s3:TestEvent` probe a validated destination receives on
/// install has been taken off it, and answer every body seen while waiting.
///
/// Bounded by [`DELIVERY_BUDGET`], which is the CONDITION's deadline and not a measurement of
/// it: the probe is what says the container was installed against a queue this suite reads, and
/// draining it is what lets each object write be observed on its own.
async fn awaited_test_event(router: &MultiServiceRouter, case: &str, url: &str) {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    let mut seen = Vec::new();
    loop {
        seen.extend(drained(router, case, url).await);
        if seen.iter().any(|text| text.contains(TEST_EVENT)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: installing the container must send the validated queue the \
             {TEST_EVENT} probe, saw {seen:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Look at the queue at `url` until it has taken a record naming `key`, or [`DELIVERY_BUDGET`]
/// passes — the CONDITION's deadline, not a measurement of it. Answers every key seen.
async fn awaited_record(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    key: &str,
) -> Vec<String> {
    let deadline = Instant::now() + DELIVERY_BUDGET;
    let mut bodies = Vec::new();
    loop {
        bodies.extend(drained(router, case, url).await);
        let keys = keys_of(&bodies);
        if keys.iter().any(|named| named == key) || Instant::now() >= deadline {
            return keys;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Everything the queue at `url` takes over [`QUIET_WINDOW`] — the read a destination that must
/// receive NOTHING is judged by, made only after the same row has seen the delivery its
/// surviving configuration owes.
async fn quiet(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let until = Instant::now() + QUIET_WINDOW;
    let mut seen = Vec::new();
    while Instant::now() < until {
        seen.extend(drained(router, case, url).await);
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    seen
}

/// Drive `rows` against ONE assembled emulator: every call S3 makes to validate a destination
/// and to publish to it is dispatched back into this same router, so the queues this reads are
/// the queues it reached.
async fn drive(rows: &[(usize, Row)]) {
    let router = build_router();
    for (index, row) in rows {
        let case = row.case;
        let bucket = format!("s3-queue-grant-{index}");
        create_bucket(&router, case, &bucket).await;
        let (granted_url, granted_arn) = queue(
            &router,
            case,
            &format!("s3-queue-grant-granted-{index}"),
            QueuePolicy::Granted,
            &bucket,
        )
        .await;
        let (candidate_url, candidate_arn) = queue(
            &router,
            case,
            &format!("s3-queue-grant-candidate-{index}"),
            row.policy,
            &bucket,
        )
        .await;

        // 1. The configuration already in force: the granted queue alone, which the install's
        //    own test notification says was validated.
        let granted_only = [configuration(GRANTED_ID, &granted_arn, GRANTED_PREFIX)];
        let installed = put_notification(&router, &bucket, &granted_only, false).await;
        assert_eq!(
            installed.status, 200,
            "case {case:?}: a container naming only the granted queue must be recorded, got {} \
             with {}",
            installed.status, installed.body
        );
        awaited_test_event(&router, case, &granted_url).await;

        // 2. The same container with the row's queue added, on a disjoint key-name prefix.
        let both = [
            configuration(GRANTED_ID, &granted_arn, GRANTED_PREFIX),
            configuration(CANDIDATE_ID, &candidate_arn, CANDIDATE_PREFIX),
        ];
        let answered = put_notification(&router, &bucket, &both, row.skip_validation).await;
        if row.recorded {
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected the container to be recorded with HTTP 200, got {} with \
                 {}",
                answered.status, answered.body
            );
        } else {
            assert_eq!(
                answered.status, INVALID_ARGUMENT_STATUS,
                "case {case:?}: expected the container to be refused HTTP \
                 {INVALID_ARGUMENT_STATUS}, got {} with {}",
                answered.status, answered.body
            );
            assert_eq!(
                element(&answered.body, "Code"),
                INVALID_ARGUMENT,
                "case {case:?}: expected the error code {INVALID_ARGUMENT:?}, got {}",
                answered.body
            );
            assert_eq!(
                element(&answered.body, "Message"),
                UNABLE_TO_VALIDATE,
                "case {case:?}: expected the message {UNABLE_TO_VALIDATE:?}, got {}",
                answered.body
            );
            assert_eq!(
                opt_element(&answered.body, "ArgumentName1").as_deref(),
                Some(candidate_arn.as_str()),
                "case {case:?}: expected the refusal to name the destination that failed, got {}",
                answered.body
            );
            assert_eq!(
                opt_element(&answered.body, "ArgumentValue1").as_deref(),
                Some(UNPERMITTED_DETAIL),
                "case {case:?}: expected the detail {UNPERMITTED_DETAIL:?}, got {}",
                answered.body
            );
        }

        // What a READ reports: the whole container where it was recorded, and exactly the
        // configuration that was already in force where it was refused.
        let reported = reported_queues(&router, case, &bucket).await;
        let expected: Vec<String> = if row.recorded {
            vec![granted_arn.clone(), candidate_arn.clone()]
        } else {
            vec![granted_arn.clone()]
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the bucket to report {expected:?}, got {reported:?}"
        );

        // 3. What the configuration in force DOES: one object under each prefix.
        let granted_key = format!("{GRANTED_PREFIX}{index}.txt");
        let candidate_key = format!("{CANDIDATE_PREFIX}{index}.txt");
        put_object(&router, case, &bucket, &granted_key).await;
        put_object(&router, case, &bucket, &candidate_key).await;

        let took = awaited_record(&router, case, &granted_url, &granted_key).await;
        assert!(
            took.contains(&granted_key),
            "case {case:?}: the granted queue must take the record for {granted_key:?}, took \
             {took:?}"
        );

        if row.recorded && !row.skip_validation {
            let delivered = awaited_record(&router, case, &candidate_url, &candidate_key).await;
            assert!(
                delivered.contains(&candidate_key),
                "case {case:?}: the queue whose policy admits the publication must take the \
                 record for {candidate_key:?}, took {delivered:?}"
            );
        } else {
            let seen = quiet(&router, case, &candidate_url).await;
            assert!(
                seen.is_empty(),
                "case {case:?}: a queue Amazon S3 may not publish this bucket's notifications \
                 to must receive nothing, got {seen:?}"
            );
        }
    }
}

/// The rows of [`ROWS`] whose container is refused, carrying the index each row names its
/// resources by.
fn refused_rows() -> Vec<(usize, Row)> {
    ROWS.iter()
        .copied()
        .enumerate()
        .filter(|(_, row)| !row.recorded)
        .collect()
}

/// The rows of [`ROWS`] whose container is recorded.
fn recorded_rows() -> Vec<(usize, Row)> {
    ROWS.iter()
        .copied()
        .enumerate()
        .filter(|(_, row)| row.recorded)
        .collect()
}

#[test]
fn an_unpermitted_queue_destination_is_refused_and_the_installed_configuration_survives() {
    driven("s3-queue-grant-refusals", || {
        Box::pin(async {
            drive(&refused_rows()).await;
        })
    });
}

#[test]
fn a_permitted_queue_destination_is_recorded_and_the_opt_out_records_either() {
    driven("s3-queue-grant-admissions", || {
        Box::pin(async {
            drive(&recorded_rows()).await;
        })
    });
}
