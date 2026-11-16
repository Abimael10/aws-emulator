// SPDX-License-Identifier: CC0-1.0

//! A BULK fan-out from a custom EventBridge bus to SQS queue targets, driven
//! against the WHOLE assembled router with request authentication on — the
//! configuration the emulator serves traffic in.
//!
//! The RULE under test: **`PutEvents` on a custom bus accepts entries in batches
//! of up to ten and answers `FailedEntryCount` 0 with an `EventId` per accepted
//! entry; every ENABLED rule on that bus whose `EventPattern` matches an accepted
//! event delivers that event to each of its targets at least once, independent of
//! order, and a bulk workload converges — so the four pattern forms over one
//! workload partition it exactly as their patterns say.** Sources: `PutEvents`'s
//! own documentation for the batch and the per-entry `EventId`, `PutRule` /
//! `PutTargets` for the rules and their targets, and the EventBridge event-pattern
//! reference for `prefix`, `numeric`, `anything-but` and a plain value list.
//!
//! What this suite adds to the service crate's own
//! (`services/service-com-amazonaws-eventbridge-awsevents/tests/all/bulk_fan_out.rs`,
//! which pins the same partition over the two services alone) is the axis that
//! crate cannot see: the assembled router mounts EVERY service and verifies every
//! signature, so each delivery EventBridge makes through the in-process callback
//! is a whole signed request through the same front door a client's is. A bulk
//! fan-out whose deliveries were made one call per (event, rule, target) does not
//! converge there, which is what
//! `crate::ops::put_events::queue_sends` batching is for.
//!
//! The rows are the four pattern forms the workload is partitioned by, and each
//! asserts the exact set of published ids its queue received: no expected event
//! missing, no foreign event delivered, the complementary `prefix` and
//! `anything-but` sets disjoint and together exactly what the match-everything
//! rule received.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built —
//! and the requests driven — on an explicitly sized thread.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_emulator_router::{build_router, InMemoryEngine};
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is signed for and addressed in.
const REGION: &str = "us-east-1";

/// The service principal EventBridge delivers to an SQS target as, which is what
/// the queue's resource policy admits ("for Amazon SQS queues, EventBridge relies
/// on resource-based policies").
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The `Source` and `DetailType` every entry of the workload carries.
const SOURCE: &str = "com.example.bulk-fan-out";
const DETAIL_TYPE: &str = "Bulk Event";

/// How many events the workload publishes, and how many entries one request
/// carries ("you can batch multiple event entries into one request", bounded by
/// `Entries`' modelled maximum of 10).
const EVENTS: usize = 200;
const BATCH: usize = 10;

/// How many tenants the workload cycles through, so the `prefix` rule selects one
/// in [`TENANTS`] of the events and the complementary `anything-but` rule the
/// rest.
const TENANTS: usize = 20;

/// The credential every call below signs with: the pair this process publishes
/// once request authentication is on.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    fn sign(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
    ) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }
}

/// One signed RPC call on the regional endpoint of `signing_name`.
async fn call(
    router: &MultiServiceRouter,
    signer: &Signer,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let built = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .body(body.to_string().into_bytes())
        .expect("the test request is well formed");
    let signed = signer.sign(built, signing_name);
    let (parts, payload) = signed.into_parts();
    let request = http::Request::from_parts(parts, bytes::Bytes::from(payload));
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`), which must answer
/// 200.
async fn events(
    router: &MultiServiceRouter,
    signer: &Signer,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = call(
        router,
        signer,
        "events",
        "application/x-amz-json-1.1",
        &format!("AWSEvents.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`), which must answer
/// 200.
async fn sqs(router: &MultiServiceRouter, signer: &Signer, operation: &str, body: &Value) -> Value {
    let (status, parsed) = call(
        router,
        signer,
        "sqs",
        "application/x-amz-json-1.0",
        &format!("AmazonSQS.{operation}"),
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A queue called `name` whose policy admits this service's deliveries for any
/// rule of `bus`, scoped the way a fan-out subscriber's policy is written;
/// answers its url and the ARN a rule names it by.
async fn queue(
    router: &MultiServiceRouter,
    signer: &Signer,
    name: &str,
    bus: &str,
) -> (String, String) {
    let created = sqs(router, signer, "CreateQueue", &json!({"QueueName": name})).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        signer,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name}: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridge",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
            "Condition": {"ArnLike": {"aws:SourceArn": format!(
                "arn:aws:events:{REGION}:{}:rule/{bus}/*",
                engine::EMULATOR_ACCOUNT_ID
            )}},
        }],
    })
    .to_string();
    sqs(
        router,
        signer,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// Every published id the queue at `url` received, draining it until it reports
/// nothing left.
async fn drained(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    url: &str,
) -> BTreeSet<i64> {
    let mut ids = BTreeSet::new();
    loop {
        let received = sqs(
            router,
            signer,
            "ReceiveMessage",
            &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
        )
        .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            return ids;
        }
        for message in messages {
            let text = message["Body"].as_str().unwrap_or_default();
            let envelope: Value = serde_json::from_str(text).unwrap_or_else(|error| {
                panic!("case {case}: a delivered body is not JSON ({error}): {text}")
            });
            let id = envelope["detail"]["id"].as_i64().unwrap_or_else(|| {
                panic!("case {case}: a delivered envelope names no published id: {envelope}")
            });
            ids.insert(id);
            let receipt = message["ReceiptHandle"].as_str().unwrap_or_default();
            sqs(
                router,
                signer,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": receipt}),
            )
            .await;
        }
    }
}

/// RULE: a bulk publication onto a custom bus reaches the queue target of every
/// matching rule, on the assembled, authenticating router.
#[test]
fn a_bulk_custom_bus_fan_out_reaches_every_matching_rules_queue() {
    std::thread::Builder::new()
        .name("bulk-custom-bus-fan-out".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(drive());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

async fn drive() {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    // The whole suite runs with request authentication ON, as the emulator serves
    // traffic: every call below carries the signature this process's keys produce,
    // and so does every delivery EventBridge makes through the callback.
    router.set_request_authentication(true);
    let published = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning authentication on publishes this process's keys");
    let signer = Signer {
        key_id: published.access_key_id.clone(),
        secret: published.secret_access_key.clone(),
    };

    let bus = "bulk-fan-out-bus";
    events(&router, &signer, "CreateEventBus", &json!({"Name": bus})).await;

    // Each row: the rule, the pattern it selects events with, and which published
    // index that pattern selects.
    type Row = (&'static str, Value, fn(usize) -> bool);
    /// One rule's queue, with the predicate deciding which events it must hold.
    type Subscriber = (&'static str, String, fn(usize) -> bool);
    let rows: [Row; 4] = [
        (
            "prefix",
            json!({"source": [SOURCE], "detail": {"tenant": [{"prefix": "t-000"}]}}),
            |index| index % TENANTS == 0,
        ),
        (
            "numeric",
            json!({"source": [SOURCE], "detail": {"n": [{"numeric": [">=", EVENTS / 2]}]}}),
            |index| index >= EVENTS / 2,
        ),
        (
            "everything",
            json!({"source": [SOURCE], "detail-type": [DETAIL_TYPE]}),
            |_| true,
        ),
        (
            "anything-but",
            json!({"source": [SOURCE], "detail": {"tenant": [{"anything-but": ["t-000"]}]}}),
            |index| index % TENANTS != 0,
        ),
    ];

    let mut queues: Vec<Subscriber> = Vec::new();
    for (rule, pattern, selects) in &rows {
        let (url, arn) = queue(&router, &signer, &format!("bulk-{rule}"), bus).await;
        events(
            &router,
            &signer,
            "PutRule",
            &json!({
                "Name": rule,
                "EventBusName": bus,
                "EventPattern": pattern.to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let targeted = events(
            &router,
            &signer,
            "PutTargets",
            &json!({
                "Rule": rule,
                "EventBusName": bus,
                "Targets": [{"Id": "q", "Arn": arn}],
            }),
        )
        .await;
        assert_eq!(
            targeted["FailedEntryCount"],
            json!(0),
            "case {rule}: PutTargets reported a failed entry: {targeted}"
        );
        queues.push((rule, url, *selects));
    }

    // The workload, published in requests of `BATCH` entries: every batch answers
    // `FailedEntryCount` 0 with an `EventId` per entry.
    let mut minted: BTreeSet<String> = BTreeSet::new();
    for chunk in 0..EVENTS / BATCH {
        let entries: Vec<Value> = (0..BATCH)
            .map(|offset| {
                let index = chunk * BATCH + offset;
                json!({
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "EventBusName": bus,
                    "Detail": json!({
                        "tenant": format!("t-{:03}", index % TENANTS),
                        "n": index,
                        "id": index,
                    }).to_string(),
                })
            })
            .collect();
        let answered = events(&router, &signer, "PutEvents", &json!({"Entries": entries})).await;
        assert_eq!(
            answered["FailedEntryCount"],
            json!(0),
            "case batch {chunk}: PutEvents reported a failed entry: {answered}"
        );
        for entry in answered["Entries"].as_array().cloned().unwrap_or_default() {
            let id = entry["EventId"]
                .as_str()
                .unwrap_or_else(|| panic!("case batch {chunk}: no EventId in {entry}"))
                .to_string();
            assert!(
                minted.insert(id.clone()),
                "case batch {chunk}: the EventId {id} was minted for another entry too"
            );
        }
    }
    assert_eq!(
        minted.len(),
        EVENTS,
        "expected one EventId per accepted entry, got {}",
        minted.len()
    );

    // What each rule's queue received: exactly the published ids its pattern
    // selects.
    let mut received: Vec<(&str, BTreeSet<i64>)> = Vec::new();
    for (rule, url, selects) in queues {
        let ids = drained(&router, &signer, rule, &url).await;
        let expected: BTreeSet<i64> = (0..EVENTS)
            .filter(|index| selects(*index))
            .map(|index| i64::try_from(index).unwrap_or_default())
            .collect();
        assert_eq!(
            ids, expected,
            "case {rule}: expected the {} published ids this rule's pattern selects, got {}",
            expected.len(),
            ids.len()
        );
        received.push((rule, ids));
    }

    // The complementary pair: the `prefix` and `anything-but` sets share nothing
    // and together are exactly what the match-everything rule received.
    let set = |name: &str| -> BTreeSet<i64> {
        received
            .iter()
            .find(|(rule, _)| *rule == name)
            .map(|(_, ids)| ids.clone())
            .unwrap_or_else(|| panic!("the suite drained a queue for {name}"))
    };
    let (prefix, anything_but, everything) = (set("prefix"), set("anything-but"), set("everything"));
    assert!(
        prefix.is_disjoint(&anything_but),
        "expected the prefix and anything-but sets to share no event, they share {:?}",
        prefix.intersection(&anything_but).collect::<Vec<_>>()
    );
    let together: BTreeSet<i64> = prefix.union(&anything_but).copied().collect();
    assert_eq!(
        together, everything,
        "expected the prefix and anything-but sets together to cover exactly what the \
         match-everything rule received ({} ids), got {}",
        everything.len(),
        together.len()
    );
}
