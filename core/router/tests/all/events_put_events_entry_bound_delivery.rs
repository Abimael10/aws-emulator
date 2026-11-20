// SPDX-License-Identifier: CC0-1.0

//! The `Entries` bound of `PutEvents`, and the delivery a request inside it makes,
//! on the WHOLE assembled emulator with request signatures VERIFIED — the
//! configuration the emulator serves traffic in.
//!
//! The RULE under test: **`PutEvents.Entries` is `@required` and modelled
//! `@length(min: 1, max: 10)`, so a request carrying more than ten entries is
//! refused as a WHOLE — one `ValidationException` (HTTP 400) about the list member,
//! and not one of its entries is published — while a request of exactly ten is
//! accepted and every one of the ten reaches the SQS target of the rule that
//! matched it on the custom bus it was published to.** Sources: the modelled bound
//! (`smithy_query(query="operation", operation="PutEvents")`) and `PutTargets` for
//! the grant a queue target needs ("for Lambda functions, Amazon SNS topics and
//! Amazon SQS queues, EventBridge relies on resource-based policies").
//!
//! `service-com-amazonaws-eventbridge-awsevents`'s own suites pin both halves over
//! two services mounted alone with request authentication OFF
//! (`tests/all/put_events_entry_count_limit.rs`). What this suite adds is the two
//! axes those cannot see:
//!
//! * **The signatures are verified.** Every call below is signed with the keys this
//!   process publishes once authentication is on — and so is the `sqs:SendMessage`
//!   EventBridge makes THROUGH THE EMULATOR'S OWN CALLBACK for each delivery, which
//!   a router that authenticates every request has to accept before the queue ever
//!   sees a message.
//! * **Every service is mounted.** The publication is claimed, routed and
//!   authorized on the assembled router rather than on a pair of services.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built —
//! and the requests driven — on an explicitly sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is signed for and addressed in.
const REGION: &str = "us-west-2";

/// `PutEventsRequestEntryList`'s modelled `@length` maximum.
const MAX_ENTRIES: usize = 10;

/// The custom bus, the rule on it and the queue that rule targets.
const BUS: &str = "entry-bound-bus";
const RULE: &str = "entry-bound-rule";
const QUEUE: &str = "entry-bound-queue";

/// The source the rule matches and the detail type each entry carries.
const SOURCE: &str = "com.demo.entry-bound";
const DETAIL_TYPE: &str = "Entry Bound Probe";

/// The service principal EventBridge delivers to an SQS target as, which the
/// queue's own policy has to allow the send for.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// How long the accepted half waits for every delivery before giving up, and how
/// often it looks meanwhile. Generous: what is asserted is that the ten envelopes
/// arrive at all, never how fast the host runs.
const DELIVERY_WAIT: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long the refused half keeps looking at the queue before concluding that a
/// request refused as a whole published nothing. A bound on an ABSENCE, so it is
/// the one span a slow host may spend without changing the verdict: the accepted
/// half proves the same wiring delivers in a fraction of it.
const WITHHELD_WINDOW: Duration = Duration::from_secs(5);

/// The key pair this process publishes once request authentication is on, which
/// every call below is signed with.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    /// Sign `request` for `signing_name` in [`REGION`], in the `Authorization`
    /// header an SDK request carries.
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

/// One SIGNED `X-Amz-Target` RPC call on the regional endpoint of `signing_name`,
/// answering the HTTP status and the parsed body.
async fn rpc(
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
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One EventBridge call (`awsJson1_1`).
async fn events(
    router: &MultiServiceRouter,
    signer: &Signer,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    rpc(
        router,
        signer,
        "events",
        "application/x-amz-json-1.1",
        &format!("AWSEvents.{operation}"),
        body,
    )
    .await
}

/// One Amazon SQS call (`awsJson1_0`).
async fn sqs(
    router: &MultiServiceRouter,
    signer: &Signer,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    rpc(
        router,
        signer,
        "sqs",
        "application/x-amz-json-1.0",
        &format!("AmazonSQS.{operation}"),
        body,
    )
    .await
}

/// The custom bus, the queue with the policy EventBridge needs, and the rule whose
/// one target is that queue; answers the queue's url.
async fn given_wiring(router: &MultiServiceRouter, signer: &Signer) -> String {
    let (status, created) = events(router, signer, "CreateEventBus", &json!({ "Name": BUS })).await;
    assert_eq!(
        status, 200,
        "CreateEventBus answered {status} {created}"
    );
    let (status, queue) = sqs(router, signer, "CreateQueue", &json!({ "QueueName": QUEUE })).await;
    assert_eq!(status, 200, "CreateQueue answered {status} {queue}");
    let url = queue["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a QueueUrl in {queue}"))
        .to_string();
    let (status, attributes) = sqs(
        router,
        signer,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "GetQueueAttributes answered {status} {attributes}"
    );
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridgeDelivery",
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": &queue_arn,
        }],
    })
    .to_string();
    let (status, written) = sqs(
        router,
        signer,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "SetQueueAttributes answered {status} {written}"
    );
    let (status, ruled) = events(
        router,
        signer,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": BUS,
            "EventPattern": json!({ "source": [SOURCE] }).to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(status, 200, "PutRule answered {status} {ruled}");
    let (status, targeted) = events(
        router,
        signer,
        "PutTargets",
        &json!({
            "Rule": RULE,
            "EventBusName": BUS,
            "Targets": [{ "Id": "queue", "Arn": &queue_arn }],
        }),
    )
    .await;
    assert_eq!(status, 200, "PutTargets answered {status} {targeted}");
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "the queue target must be accepted, got {targeted}"
    );
    url
}

/// `count` entries onto [`BUS`], each carrying the three properties an event needs
/// and its position in the batch.
fn entries(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| {
            json!({
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "index": index }).to_string(),
                "EventBusName": BUS,
            })
        })
        .collect()
}

/// Receive everything waiting on the queue at `url` and delete it; answers the
/// `index` each delivered envelope's detail carried.
async fn drain(router: &MultiServiceRouter, signer: &Signer, url: &str) -> Vec<i64> {
    let (status, received) = sqs(
        router,
        signer,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ReceiveMessage answered {status} {received}"
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut indexes = Vec::new();
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            let (status, deleted) = sqs(
                router,
                signer,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
            assert_eq!(status, 200, "DeleteMessage answered {status} {deleted}");
        }
        let envelope: Value = message["Body"]
            .as_str()
            .and_then(|body| serde_json::from_str(body).ok())
            .unwrap_or(Value::Null);
        if let Some(index) = envelope.pointer("/detail/index").and_then(Value::as_i64) {
            indexes.push(index);
        }
    }
    indexes
}

/// RULE: on a router that verifies request signatures, a `PutEvents` of more than
/// ten entries is refused as a whole and publishes nothing, while one of exactly
/// ten is accepted and every entry reaches the queue target of the rule that
/// matched it on the custom bus.
#[test]
fn the_entry_bound_refuses_a_whole_batch_and_exactly_ten_are_delivered() {
    std::thread::Builder::new()
        .name("put-events-entry-bound".to_string())
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

/// Both halves, against ONE assembled router.
async fn drive() {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    // The whole suite runs with request authentication ON, as the emulator serves
    // traffic.
    router.set_request_authentication(true);
    let published = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning authentication on publishes this process's keys");
    let signer = Signer {
        key_id: published.access_key_id.clone(),
        secret: published.secret_access_key.clone(),
    };

    let url = given_wiring(&router, &signer).await;

    // One past the bound: the whole request is refused and nothing is published.
    let over = entries(MAX_ENTRIES + 1);
    let (status, refused) = events(&router, &signer, "PutEvents", &json!({ "Entries": &over })).await;
    assert_eq!(
        status, 400,
        "a batch past the modelled bound must be refused, got {status} {refused}"
    );
    let code = refused["__type"]
        .as_str()
        .map(|name| name.rsplit('#').next().unwrap_or(name).to_string())
        .unwrap_or_default();
    assert_eq!(
        code, "ValidationException",
        "the refusal names the constraint that failed, got {refused}"
    );
    let message = refused["message"].as_str().unwrap_or_default();
    assert!(
        message.ends_with(
            "' at 'entries' failed to satisfy constraint: Member must have length less than \
             or equal to 10"
        ),
        "the clause is about the entries list and its bound, got {message:?}"
    );
    let watching = Instant::now();
    while watching.elapsed() < WITHHELD_WINDOW {
        let delivered = drain(&router, &signer, &url).await;
        assert!(
            delivered.is_empty(),
            "a refused batch publishes nothing, got {delivered:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    // Exactly the bound: accepted, and every entry reaches the rule's target. The
    // wait is on the CONDITION, so a slow host delays the suite instead of
    // breaking it.
    let (status, answer) = events(
        &router,
        &signer,
        "PutEvents",
        &json!({ "Entries": entries(MAX_ENTRIES) }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a batch of exactly ten must be accepted, got {status} {answer}"
    );
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "every one of the ten is accepted, got {answer}"
    );
    let started = Instant::now();
    let mut delivered: Vec<i64> = Vec::new();
    while started.elapsed() < DELIVERY_WAIT && delivered.len() < MAX_ENTRIES {
        delivered.extend(drain(&router, &signer, &url).await);
        if delivered.len() < MAX_ENTRIES {
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    }
    delivered.sort_unstable();
    let expected: Vec<i64> = (0..MAX_ENTRIES).map(|index| index as i64).collect();
    assert_eq!(
        delivered, expected,
        "expected every entry of the accepted batch on {url}, got {delivered:?}"
    );
}
