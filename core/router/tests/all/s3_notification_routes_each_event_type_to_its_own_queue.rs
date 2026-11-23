// SPDX-License-Identifier: CC0-1.0

//! One bucket fanning its object events out across SEVERAL Amazon SQS destinations, each
//! scoped to its own event class.
//!
//! The rule under test: **every entry of a bucket's notification configuration is matched
//! against an object mutation on its own `Events` list, independently of the other entries and
//! of the order the container lists them in, and the mutation publishes exactly one `Records`
//! message per matching entry — stamped with that entry's `Id` as `s3.configurationId` and
//! with the `eventName` of the mutation — and nothing at all to an entry whose `Events` do not
//! cover it.** A value covers the event itself and, in the `s3:<Type>:*` form, every event of
//! that type (`Event`), so two entries naming two different events of ONE type are notified of
//! no event in common.
//!
//! Two crates have to agree for that to hold — S3 evaluates the configuration and makes the
//! publications, Amazon SQS holds the queues they land on — so the suite lives here.
//! [`crate::s3_notification_fans_out_across_destination_kinds`] pins the same independence
//! across destination KINDS and
//! [`crate::s3_notification_delivery_follows_the_destination_grant`] pins the grant each
//! publication is decided by; what only this suite reaches is several entries of the SAME list
//! split by event class.
//!
//! The rows differ along what the rule generalises over: how many entries the container
//! carries (two, three), the form of each entry's `Events` (the `s3:<Type>:*` wildcard, a
//! literal event value), the order the container lists the entries in, which mutation happens
//! (a `PUT`, a `COPY`, a `DELETE`, a tag write, a tag removal) and whether any entry matches it
//! at all.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for, and so the Region every queue lives in.
const REGION: &str = "us-east-1";

/// The XML namespace a client puts on an Amazon S3 request payload.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The service principal Amazon S3 publishes a bucket's notifications under — "for Amazon
/// Web Services services, the principal is a domain-style identifier that the service
/// defines, such as s3.amazonaws.com" (`AddPermission$Principal`).
const S3_PRINCIPAL: &str = "s3.amazonaws.com";

/// The action Amazon S3 performs to publish a notification to a queue.
const SEND_MESSAGE_ACTION: &str = "sqs:SendMessage";

/// The condition key a grant narrowed to ONE publishing bucket names it by.
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The `Event` of the validation handshake a queue receives when it is attached as a
/// destination: it carries no `Records` and is not an object event.
const TEST_EVENT: &str = "s3:TestEvent";

/// How long a row waits for the asynchronous delivery of an object event, and how often it
/// looks while waiting. Deliberately generous: this suite shares its host with the rest of the
/// test target, and nothing here tests how fast that host is.
const DELIVERY_BUDGET: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// How long the hold at the end of a row keeps looking before concluding that nothing more
/// came. Every record the row owed has already arrived by then, so the window is read against a
/// pipeline known to be live.
const QUIET_WINDOW: Duration = Duration::from_secs(3);

/// The stack the emulator's own boot path gives the thread that assembles the router
/// (`core/router/src/main.rs`).
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// Run `body` to completion on a thread with [`BOOT_STACK_SIZE`], on the current-thread
/// runtime the deliveries are made on: a notification delivery is a task of its own, so it
/// runs on this same thread and needs the same depth.
fn driven<Body>(name: &'static str, body: Body)
where
    Body: FnOnce() -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = ()>>>
        + Send
        + 'static,
{
    ::std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let runtime = ::tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(body());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// The emulator as it is ASSEMBLED, on one engine: the publication S3 makes is dispatched
/// back into this same router, so the queues it reaches are the ones this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

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
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// Dispatch one request against S3's REST surface, path-style, and assert the status the
/// operation answers a success with.
async fn s3_ok(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
    expected: u16,
) -> String {
    let host = format!("s3.{REGION}.amazonaws.com");
    let mut request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, expected,
        "case {case:?}: expected HTTP {expected} from {method} {path}, got {status} with {text}"
    );
    text
}

/// A queue called `name` whose own access policy lets Amazon S3 publish `bucket`'s
/// notifications to it: its url and the ARN Amazon SQS reports for it.
async fn given_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
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
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": grant(&arn, bucket) } }),
    )
    .await;
    (url, arn)
}

/// One statement allowing the Amazon S3 service principal to send `bucket`'s notifications to
/// the queue `resource` names, narrowed by `aws:SourceArn` to that one publishing bucket. An
/// S3 bucket's ARN carries no Region and no account, because a bucket name is globally unique.
fn grant(resource: &str, bucket: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowS3SendMessage",
            "Effect": "Allow",
            "Principal": { "Service": S3_PRINCIPAL },
            "Action": SEND_MESSAGE_ACTION,
            "Resource": resource,
            "Condition": { "ArnEquals": { SOURCE_ARN_KEY: format!("arn:aws:s3:::{bucket}") } },
        }],
    })
    .to_string()
}

/// One `QueueConfiguration` of a row's container: the `Id` it is recorded and reports records
/// under, and the `Events` it is notified of.
struct Entry {
    id: &'static str,
    events: &'static [&'static str],
}

/// `PUT /<bucket>?notification` with one `QueueConfiguration` per entry, each naming the queue
/// of the same position. The elements are the ones S3's own model binds the list to
/// (`smithy_query(query="shape",shape="NotificationConfiguration")`: `QueueConfigurations` ->
/// `QueueConfiguration`, whose `QueueArn` is `Queue` and whose `Events` is `Event`).
async fn put_notification(
    router: &MultiServiceRouter,
    case: &str,
    bucket: &str,
    entries: &[Entry],
    arns: &[String],
) {
    let mut body = format!("<NotificationConfiguration xmlns=\"{S3_XMLNS}\">");
    for (entry, arn) in entries.iter().zip(arns) {
        body.push_str("<QueueConfiguration>");
        body.push_str(&format!("<Id>{}</Id>", entry.id));
        body.push_str(&format!("<Queue>{arn}</Queue>"));
        for event in entry.events {
            body.push_str(&format!("<Event>{event}</Event>"));
        }
        body.push_str("</QueueConfiguration>");
    }
    body.push_str("</NotificationConfiguration>");
    s3_ok(
        router,
        case,
        "PUT",
        &format!("/{bucket}?notification"),
        &[],
        &body,
        200,
    )
    .await;
}

/// Which object mutation a beat performs, named by the `Event` value the change produces.
#[derive(Debug, Clone, Copy)]
enum Mutation {
    /// `PutObject` — `s3:ObjectCreated:Put`.
    Put(&'static str),
    /// `CopyObject` — `s3:ObjectCreated:Copy`, from the first key to the second.
    Copy(&'static str, &'static str),
    /// `DeleteObject` of an unversioned object — `s3:ObjectRemoved:Delete`.
    Delete(&'static str),
    /// `PutObjectTagging` — `s3:ObjectTagging:Put`.
    TagPut(&'static str),
    /// `DeleteObjectTagging` — `s3:ObjectTagging:Delete`.
    TagDelete(&'static str),
}

impl Mutation {
    /// The key the change is made on, which is the key the record it owes names.
    fn key(self) -> &'static str {
        match self {
            Self::Put(key)
            | Self::Copy(_, key)
            | Self::Delete(key)
            | Self::TagPut(key)
            | Self::TagDelete(key) => key,
        }
    }

    /// Make the change against `bucket`, asserting the status the operation answers.
    async fn perform(self, router: &MultiServiceRouter, case: &str, bucket: &str) {
        // The copy source travels in a HEADER rather than in the body, which is what makes a
        // `PUT` of an object a `CopyObject` (`CopyObjectRequest$CopySource`).
        let source = match self {
            Self::Copy(from, _) => format!("/{bucket}/{from}"),
            _ => String::new(),
        };
        let (method, path, body, status): (_, _, String, u16) = match self {
            Self::Put(key) => (
                "PUT",
                format!("/{bucket}/{key}"),
                "the object the notification is published for".to_string(),
                200,
            ),
            Self::Copy(_, to) => ("PUT", format!("/{bucket}/{to}"), String::new(), 200),
            Self::Delete(key) => ("DELETE", format!("/{bucket}/{key}"), String::new(), 204),
            Self::TagPut(key) => (
                "PUT",
                format!("/{bucket}/{key}?tagging"),
                format!(
                    "<Tagging xmlns=\"{S3_XMLNS}\"><TagSet><Tag><Key>stage</Key>\
                     <Value>routed</Value></Tag></TagSet></Tagging>"
                ),
                200,
            ),
            Self::TagDelete(key) => (
                "DELETE",
                format!("/{bucket}/{key}?tagging"),
                String::new(),
                204,
            ),
        };
        let headers: Vec<(&str, &str)> = if source.is_empty() {
            Vec::new()
        } else {
            vec![("x-amz-copy-source", source.as_str())]
        };
        s3_ok(router, case, method, &path, &headers, &body, status).await;
    }
}

/// One beat of a row: the mutation, and the entry it owes its one record to — `None` for a
/// mutation no entry of the container is notified of.
struct Beat {
    mutation: Mutation,
    /// The position of the entry the record is owed to, and the `eventName` it carries.
    expected: Option<(usize, &'static str)>,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The bucket and the queue names this row works in, kept apart from every other row's.
    name: &'static str,
    /// The container's entries, in the order the document lists them.
    entries: &'static [Entry],
    /// The mutations the row makes, in order.
    beats: &'static [Beat],
}

/// The table: one row per shape of per-event-class fan-out the rule has to hold over.
const ROWS: &[Row] = &[
    Row {
        case: "two entries, the created and the removed class by wildcard",
        name: "per-class-two-entries",
        entries: &[
            Entry {
                id: "created",
                events: &["s3:ObjectCreated:*"],
            },
            Entry {
                id: "removed",
                events: &["s3:ObjectRemoved:*"],
            },
        ],
        beats: &[
            Beat {
                mutation: Mutation::Put("route-me.txt"),
                expected: Some((0, "ObjectCreated:Put")),
            },
            Beat {
                mutation: Mutation::Delete("route-me.txt"),
                expected: Some((1, "ObjectRemoved:Delete")),
            },
        ],
    },
    Row {
        case: "the same two entries, listed in the opposite order",
        name: "per-class-reversed",
        entries: &[
            Entry {
                id: "removed",
                events: &["s3:ObjectRemoved:*"],
            },
            Entry {
                id: "created",
                events: &["s3:ObjectCreated:*"],
            },
        ],
        beats: &[
            Beat {
                mutation: Mutation::Put("route-me.txt"),
                expected: Some((1, "ObjectCreated:Put")),
            },
            Beat {
                mutation: Mutation::Delete("route-me.txt"),
                expected: Some((0, "ObjectRemoved:Delete")),
            },
        ],
    },
    Row {
        case: "three entries on literal values, two of them details of one type",
        name: "per-class-literal-values",
        entries: &[
            Entry {
                id: "put-only",
                events: &["s3:ObjectCreated:Put"],
            },
            Entry {
                id: "copy-only",
                events: &["s3:ObjectCreated:Copy"],
            },
            Entry {
                id: "delete-only",
                events: &["s3:ObjectRemoved:Delete"],
            },
        ],
        beats: &[
            Beat {
                mutation: Mutation::Put("source.bin"),
                expected: Some((0, "ObjectCreated:Put")),
            },
            Beat {
                mutation: Mutation::Copy("source.bin", "copy.bin"),
                expected: Some((1, "ObjectCreated:Copy")),
            },
            Beat {
                mutation: Mutation::Delete("copy.bin"),
                expected: Some((2, "ObjectRemoved:Delete")),
            },
        ],
    },
    Row {
        case: "an entry scoped to a class no mutation of the row performs",
        name: "per-class-unreached-entry",
        entries: &[
            Entry {
                id: "created",
                events: &["s3:ObjectCreated:*"],
            },
            Entry {
                id: "tagged",
                events: &["s3:ObjectTagging:*"],
            },
            Entry {
                id: "acl",
                events: &["s3:ObjectAcl:Put"],
            },
        ],
        beats: &[
            Beat {
                mutation: Mutation::Put("tagged.txt"),
                expected: Some((0, "ObjectCreated:Put")),
            },
            Beat {
                mutation: Mutation::TagPut("tagged.txt"),
                expected: Some((1, "ObjectTagging:Put")),
            },
            Beat {
                mutation: Mutation::TagDelete("tagged.txt"),
                expected: Some((1, "ObjectTagging:Delete")),
            },
        ],
    },
    Row {
        case: "a mutation no entry of the container is notified of",
        name: "per-class-unmatched-mutation",
        entries: &[
            Entry {
                id: "removed",
                events: &["s3:ObjectRemoved:*"],
            },
            Entry {
                id: "tagged",
                events: &["s3:ObjectTagging:*"],
            },
        ],
        beats: &[
            Beat {
                mutation: Mutation::Put("unwatched.txt"),
                expected: None,
            },
            Beat {
                mutation: Mutation::Delete("unwatched.txt"),
                expected: Some((0, "ObjectRemoved:Delete")),
            },
        ],
    },
];

/// Whether one message body is the validation handshake rather than an object event: it names
/// `s3:TestEvent` as its `Event` and carries no `Records` at all.
fn is_handshake(body: &Value) -> bool {
    body["Event"].as_str() == Some(TEST_EVENT) && body.get("Records").is_none()
}

/// The `(eventName, configurationId, key)` of every record the bodies carry.
fn records_of(bodies: &[Value]) -> Vec<(String, String, String)> {
    bodies
        .iter()
        .flat_map(|body| body["Records"].as_array().cloned().unwrap_or_default())
        .map(|record| {
            (
                record["eventName"].as_str().unwrap_or_default().to_string(),
                record["s3"]["configurationId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                record["s3"]["object"]["key"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// Take every message waiting on the queue at `url` off it, parsed as the JSON each body
/// carries, deleting each as it is handed over so one delivery is read once.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
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
            let body = message["Body"].as_str().unwrap_or_default();
            found.push(serde_json::from_str::<Value>(body).unwrap_or_else(|error| {
                panic!("case {case:?}: a delivered body is not JSON ({error}): {body}")
            }));
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

/// What one queue of a row has been handed so far: the records, and how many validation
/// handshakes arrived.
#[derive(Default)]
struct Held {
    records: Vec<(String, String, String)>,
    handshakes: usize,
}

impl Held {
    /// Drain the queue once and classify everything it handed over.
    async fn collect(&mut self, router: &MultiServiceRouter, case: &str, url: &str) {
        let bodies = drained(router, case, url).await;
        self.handshakes += bodies.iter().filter(|body| is_handshake(body)).count();
        self.records.extend(records_of(&bodies));
    }
}

/// RULE: every entry of a bucket's notification configuration is matched against an object
/// mutation on its own `Events`, and the mutation publishes exactly one record to each matching
/// entry and nothing to the others.
#[test]
fn each_entry_of_one_container_is_matched_against_a_mutation_on_its_own_events() {
    driven("s3-per-event-class-queue-routing", || {
        Box::pin(async {
            let router = build_router();
            for row in ROWS {
                let case = row.case;
                let bucket = row.name;
                s3_ok(&router, case, "PUT", &format!("/{bucket}"), &[], "", 200).await;

                // One queue per entry, each granting Amazon S3 the send for this bucket.
                let mut urls = Vec::new();
                let mut arns = Vec::new();
                for entry in row.entries {
                    let name = format!("{}-{}", row.name, entry.id);
                    let (url, arn) = given_queue(&router, case, &name, bucket).await;
                    urls.push(url);
                    arns.push(arn);
                }
                put_notification(&router, case, bucket, row.entries, &arns).await;

                // The handshake each newly attached destination receives, which is what a
                // consumer of a validated destination finds waiting on it before any object
                // changes: exactly one, carrying no object record.
                let mut held: Vec<Held> = row.entries.iter().map(|_| Held::default()).collect();
                for (position, url) in urls.iter().enumerate() {
                    let deadline = Instant::now() + DELIVERY_BUDGET;
                    loop {
                        held[position].collect(&router, case, url).await;
                        // Read AFTER the look, so a look that finally finds the handshake is
                        // not reported as a missing one because the clock passed the deadline
                        // during it.
                        if held[position].handshakes > 0 {
                            break;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "case {case:?}: attaching the queue of entry {:?} must publish one \
                             {TEST_EVENT} to it, got nothing inside {DELIVERY_BUDGET:?}",
                            row.entries[position].id
                        );
                        tokio::time::sleep(LOOK_INTERVAL).await;
                    }
                    assert!(
                        held[position].records.is_empty(),
                        "case {case:?}: the first message the queue of entry {:?} receives must \
                         be the handshake, which carries no Records, got {:?}",
                        row.entries[position].id,
                        held[position].records
                    );
                }

                // What each entry's queue owes, in the order the row's beats produce it.
                let mut owed: Vec<Vec<(String, String, String)>> =
                    row.entries.iter().map(|_| Vec::new()).collect();
                for beat in row.beats {
                    beat.mutation.perform(&router, case, bucket).await;
                    if let Some((position, event_name)) = beat.expected {
                        owed[position].push((
                            event_name.to_string(),
                            row.entries[position].id.to_string(),
                            beat.mutation.key().to_string(),
                        ));
                    }
                }

                // Every record owed, looked for until it is there: the barrier the negative
                // leg below is read against.
                for (position, url) in urls.iter().enumerate() {
                    let deadline = Instant::now() + DELIVERY_BUDGET;
                    loop {
                        held[position].collect(&router, case, url).await;
                        // Read AFTER the look, for the reason the handshake wait gives.
                        if held[position].records.len() >= owed[position].len() {
                            break;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "case {case:?}: the queue of entry {:?} owes {:?}, got {:?} inside \
                             {DELIVERY_BUDGET:?}",
                            row.entries[position].id,
                            owed[position],
                            held[position].records
                        );
                        tokio::time::sleep(LOOK_INTERVAL).await;
                    }
                }

                // And NOTHING else, over a hold read against those barriers: an entry no
                // mutation reached stays empty, and a matching entry is published one record
                // per matching mutation and no second copy of any of them.
                let deadline = Instant::now() + QUIET_WINDOW;
                while Instant::now() < deadline {
                    for (position, url) in urls.iter().enumerate() {
                        held[position].collect(&router, case, url).await;
                    }
                    tokio::time::sleep(LOOK_INTERVAL).await;
                }
                for (position, entry) in row.entries.iter().enumerate() {
                    assert_eq!(
                        held[position].records, owed[position],
                        "case {case:?}: the queue of entry {:?} must be published exactly the \
                         records its own Events cover, expected {:?}, got {:?}",
                        entry.id, owed[position], held[position].records
                    );
                    assert_eq!(
                        held[position].handshakes, 1,
                        "case {case:?}: the queue of entry {:?} must receive exactly one \
                         {TEST_EVENT} for the one time it was attached, got {}",
                        entry.id, held[position].handshakes
                    );
                }
            }
        })
    });
}
