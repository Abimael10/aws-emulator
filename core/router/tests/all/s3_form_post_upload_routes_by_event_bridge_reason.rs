// SPDX-License-Identifier: CC0-1.0

//! A BROWSER FORM upload to a bucket that publishes to EventBridge drives the rule scoped to
//! the creation path it came of — over the WHOLE assembled router, with request
//! authentication ON, which is the configuration the emulator serves traffic in.
//!
//! The RULE under test: **a bucket whose notification configuration carries an
//! `EventBridgeConfiguration` ("enables delivery of events to Amazon EventBridge",
//! `smithy_query(query="shape",shape="NotificationConfiguration")` on
//! `EventBridgeConfiguration`) publishes every object creation onto the account's default bus
//! as `source` `aws.s3` / `detail-type` `Object Created`, naming the bucket in
//! `detail.bucket.name`, the RAW key in `detail.object.key` and the API THAT CREATED THE
//! OBJECT in `detail.reason` — `PutObject` for a `PUT` and `POST Object` for a browser form
//! POST (Amazon S3 User Guide, *Using EventBridge*; Amazon S3 API Reference, *POST Object*).
//! Because the two reasons are different literals, a rule whose pattern pins one of them
//! receives the creations of that path alone.**
//!
//! Only a cross-service suite on the ASSEMBLED router can state this: the form POST is not a
//! modelled operation (no `Authorization` header, no `X-Amz-Target`, a `multipart/form-data`
//! payload addressed to the bucket), so which service claims it is the whole router's
//! attribution; the publication is Amazon S3's, the match and the handover EventBridge's, and
//! what arrived is Amazon SQS's. The crate-local suite
//! `services/service-com-amazonaws-s3-amazons3/tests/all/object_event_dual_destinations.rs`
//! pins the same rule over the two creation paths and over the key spellings each
//! notification family uses.
//!
//! The rows are the two creation paths of one bucket — the `PUT` and the browser form POST,
//! whose `policy` carries the signature this process's published credential produces over it
//! — each asserting the envelope its own reason-scoped rule receives and the absence of it on
//! the other's, read at the instant the matching rule has already been delivered to.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on an explicitly sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

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

/// The `source` and `detail-type` Amazon S3 publishes an object creation under, and the
/// `reason` each creating API stamps on it — spelled out here rather than imported, so a
/// wrong constant in the implementation cannot make this suite pass.
const S3_SOURCE: &str = "aws.s3";
const OBJECT_CREATED: &str = "Object Created";
const PUT_REASON: &str = "PutObject";
const POST_REASON: &str = "POST Object";

/// The bus an Amazon Web Services service's own events go to.
const DEFAULT_BUS: &str = "default";

/// The service principal EventBridge hands a queue target over as, which is what the queue's
/// resource policy admits ("for Amazon SQS queues, EventBridge relies on resource-based
/// policies").
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The XML namespace a client puts on an S3 request payload.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The boundary the form below delimits its parts with, and the media type that announces it.
const BOUNDARY: &str = "----------emulator-router-form";

/// The bytes the form uploads, and the name the browser states for the attached file.
const FILE_BYTES: &[u8] = b"browser,form,upload\n1,2,3\n";
const FILE_NAME: &str = "quarterly.csv";

/// How long a wait for a delivery lasts before the suite gives up, and how often it looks.
/// GENEROUS: nothing here asserts on how long anything took.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The credential every signed call below uses: the pair this process publishes once request
/// authentication is on, which is also what a form's POST policy is signed with.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    /// Sign `request` for `signing_name` in [`REGION`], in the `Authorization` header an SDK
    /// request carries.
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

/// Dispatch one request through the router, answering the status and the body as text.
async fn dispatch(router: &MultiServiceRouter, request: http::Request<Vec<u8>>) -> (u16, String) {
    let (parts, payload) = request.into_parts();
    let request = http::Request::from_parts(parts, bytes::Bytes::from(payload));
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, body)
}

/// One SIGNED RPC call against the service whose signing name is `signing_name`, which must
/// answer 200, as its parsed JSON body.
async fn rpc(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: &Value,
) -> Value {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let built = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", content_type)
        .header("x-amz-target", target)
        .body(body.to_string().into_bytes())
        .expect("the test request is well formed");
    let (status, answered) = dispatch(router, signer.sign(built, signing_name)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {target}, got {status} with {answered}"
    );
    serde_json::from_str(&answered).unwrap_or(Value::Null)
}

/// One EventBridge call (`awsJson1_1`).
async fn events(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        signer,
        case,
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
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        signer,
        case,
        "sqs",
        "application/x-amz-json-1.0",
        &format!("AmazonSQS.{operation}"),
        body,
    )
    .await
}

/// One SIGNED S3 request on the regional endpoint, answering the status and the body.
async fn s3(
    router: &MultiServiceRouter,
    signer: &Signer,
    method: &str,
    path: &str,
    body: &[u8],
) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let built = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/octet-stream")
        .body(body.to_vec())
        .expect("the test request is well formed");
    dispatch(router, signer.sign(built, "s3")).await
}

/// A string member of a response, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// An ENABLED rule on the default bus matching this bucket's `Object Created` events whose
/// `detail.reason` is `reason`, delivering to the queue `queue_arn`.
async fn reason_scoped_rule(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    name: &str,
    bucket: &str,
    reason: &str,
    queue_arn: &str,
) {
    events(
        router,
        signer,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": DEFAULT_BUS,
            "State": "ENABLED",
            "EventPattern": json!({
                "source": [S3_SOURCE],
                "detail-type": [OBJECT_CREATED],
                "detail": { "bucket": { "name": [bucket] }, "reason": [reason] },
            })
            .to_string(),
        }),
    )
    .await;
    let targeted = events(
        router,
        signer,
        case,
        "PutTargets",
        &json!({
            "Rule": name,
            "EventBusName": DEFAULT_BUS,
            "Targets": [{ "Id": "the-queue", "Arn": queue_arn }],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: every target of {name:?} must be accepted, got {targeted}"
    );
}

/// An SQS queue called `name` whose resource policy admits the deliveries of the rule
/// `rule_arn` and of no other; answers its url and the ARN a rule names it by.
async fn queue(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    name: &str,
    rule_arn: &str,
) -> (String, String) {
    let created = sqs(
        router,
        signer,
        case,
        "CreateQueue",
        &json!({"QueueName": name}),
    )
    .await;
    let url = member(case, &created, "/QueueUrl");
    let attributes = sqs(
        router,
        signer,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = member(case, &attributes, "/Attributes/QueueArn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
            "Condition": {"ArnEquals": {"aws:SourceArn": rule_arn}},
        }],
    })
    .to_string();
    sqs(
        router,
        signer,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// Every envelope waiting on the queue at `url` right now, each parsed as the JSON it
/// carries. The messages are left received, so a wait that looks again counts each delivery
/// once.
async fn drain(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    url: &str,
) -> Vec<Value> {
    let received = sqs(
        router,
        signer,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
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

/// Collect off the queue at `url` until an envelope names the object `key` or
/// [`DELIVERY_DEADLINE`] passes, and answer everything that arrived. Waits for the
/// CONDITION, not for a span of the clock.
async fn collect_until(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    url: &str,
    key: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(router, signer, case, url).await);
        let found = arrived
            .iter()
            .any(|envelope| envelope.pointer("/detail/object/key") == Some(&json!(key)));
        if found || Instant::now() >= deadline {
            return arrived;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The one envelope of `arrived` that names the object `key`, or a panic naming the case.
fn envelope_for<'a>(case: &str, arrived: &'a [Value], key: &str) -> &'a Value {
    arrived
        .iter()
        .find(|envelope| envelope.pointer("/detail/object/key") == Some(&json!(key)))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an 'Object Created' envelope naming {key:?}, got \
                 {arrived:?}"
            )
        })
}

/// The `multipart/form-data` document a browser submits: each field as its own part, and the
/// file as the one part that also states a `filename`.
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
    document.extend_from_slice(FILE_BYTES);
    document.extend_from_slice(b"\r\n");
    document.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    document
}

/// RULE: on the whole assembled router with request authentication on, a `PUT` and a browser
/// form POST into one EventBridge-enabled bucket each publish an `Object Created` envelope
/// whose `detail.reason` names the API that created the object, so the rule pinning one
/// reason receives that path's creations and none of the other's.
#[test]
fn a_form_post_upload_reaches_the_rule_scoped_to_its_own_reason() {
    std::thread::Builder::new()
        .name("s3-form-post-reason-routing".to_string())
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

/// The whole chain, on one assembled router.
async fn drive() {
    let case = "a bucket whose creations are routed by their reason";
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    // Every call below runs with request authentication ON, as the emulator serves traffic;
    // the form POST carries no `Authorization` header, because a browser form has none —
    // its credential is the policy it carries.
    router.set_request_authentication(true);
    let published = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning authentication on publishes this process's keys");
    let signer = Signer {
        key_id: published.access_key_id.clone(),
        secret: published.secret_access_key.clone(),
    };

    let bucket = "form-post-reason-routing";
    let (created, answered) = s3(&router, &signer, "PUT", &format!("/{bucket}"), b"").await;
    assert_eq!(
        created, 200,
        "case {case:?}: creating {bucket} must succeed, got {created} with {answered}"
    );

    // The two rules, each scoped to one of the two creation reasons, and the queue each
    // delivers to — whose policy admits that rule's deliveries alone.
    let account = engine::EMULATOR_ACCOUNT_ID;
    let put_rule_arn = format!("arn:aws:events:{REGION}:{account}:rule/reason-put");
    let post_rule_arn = format!("arn:aws:events:{REGION}:{account}:rule/reason-post");
    let (put_url, put_queue_arn) = queue(&router, &signer, case, "reason-put", &put_rule_arn).await;
    let (post_url, post_queue_arn) =
        queue(&router, &signer, case, "reason-post", &post_rule_arn).await;
    reason_scoped_rule(
        &router,
        &signer,
        case,
        "reason-put",
        bucket,
        PUT_REASON,
        &put_queue_arn,
    )
    .await;
    reason_scoped_rule(
        &router,
        &signer,
        case,
        "reason-post",
        bucket,
        POST_REASON,
        &post_queue_arn,
    )
    .await;

    // EventBridge delivery enabled on the bucket, and read back: the container's presence
    // IS the state, so the read-back has to report it.
    let document = format!(
        "<NotificationConfiguration xmlns=\"{S3_XMLNS}\">\
         <EventBridgeConfiguration></EventBridgeConfiguration>\
         </NotificationConfiguration>"
    );
    let (configured, answered) = s3(
        &router,
        &signer,
        "PUT",
        &format!("/{bucket}?notification"),
        document.as_bytes(),
    )
    .await;
    assert_eq!(
        configured, 200,
        "case {case:?}: enabling EventBridge delivery must succeed, got {configured} with \
         {answered}"
    );
    let (read, reported) = s3(
        &router,
        &signer,
        "GET",
        &format!("/{bucket}?notification"),
        b"",
    )
    .await;
    assert_eq!(
        read, 200,
        "case {case:?}: reading the configuration back must succeed, got {read} with {reported}"
    );
    assert!(
        reported.contains("<EventBridgeConfiguration"),
        "case {case:?}: the read-back must report EventBridgeConfiguration, got {reported}"
    );

    // The PUT leg: the object is created by `PutObject`, so only the rule pinning that
    // reason receives it.
    let put_key = "paths/put.txt";
    let (written, answered) = s3(
        &router,
        &signer,
        "PUT",
        &format!("/{bucket}/{put_key}"),
        FILE_BYTES,
    )
    .await;
    assert_eq!(
        written, 200,
        "case {case:?}: writing {put_key:?} must succeed, got {written} with {answered}"
    );
    let on_put = collect_until(&router, &signer, case, &put_url, put_key).await;
    let envelope = envelope_for(case, &on_put, put_key);
    for (pointer, expected) in [
        ("/source", json!(S3_SOURCE)),
        ("/detail-type", json!(OBJECT_CREATED)),
        ("/detail/reason", json!(PUT_REASON)),
        ("/detail/bucket/name", json!(bucket)),
    ] {
        let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {pointer} of the PUT's envelope to be {expected}, got \
             {actual} in {envelope}"
        );
    }
    // That envelope has been published and routed, so at this instant the rule pinning the
    // form POST's reason has received nothing.
    let withheld = drain(&router, &signer, case, &post_url).await;
    assert!(
        withheld.is_empty(),
        "case {case:?}: a rule pinning the form POST's reason must receive nothing a PUT \
         produced, got {withheld:?}"
    );

    // The form-POST leg, submitted as a browser submits it — to the BUCKET, with NO
    // `Authorization` header, as `multipart/form-data`, carrying the `policy` that
    // authorizes it and the SIGNATURE this process's published credential produces over
    // that policy: `HMAC-SHA256(derived signing key, base64-encoded policy)`, hex encoded,
    // with the key derived from the date, the Region and the service of the credential the
    // form names (Amazon S3 API Reference, *Authenticating requests: browser-based uploads
    // using POST*). The derivation is the SDK signer's own
    // ([`aws_sigv4::sign::v4`]), so this suite does not re-derive the key schedule.
    let post_key = "paths/posted.txt";
    let signed_at = SystemTime::now();
    let date = ::aws_smithy_types::DateTime::from(signed_at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("the signing instant renders as an ISO8601 date-time");
    // `YYYYMMDD` and `YYYYMMDDTHHMMSSZ`, the two forms a credential scope and an
    // `x-amz-date` are written in.
    let stamp: String = date.chars().filter(char::is_ascii_alphanumeric).collect();
    let (day, instant) = (&stamp[..8], format!("{}Z", &stamp[..15]));
    let credential = format!("{}/{day}/{REGION}/s3/aws4_request", signer.key_id);
    let policy = json!({
        // Far enough ahead that no run of this suite reaches it.
        "expiration": "2999-01-01T00:00:00Z",
        "conditions": [
            { "bucket": bucket },
            ["starts-with", "$key", "paths/"],
            { "success_action_status": "201" },
            { "x-amz-algorithm": "AWS4-HMAC-SHA256" },
            { "x-amz-credential": &credential },
            { "x-amz-date": &instant },
            ["content-length-range", 0, 1_048_576],
        ],
    });
    let encoded = ::aws_smithy_types::base64::encode(policy.to_string());
    let signing_key =
        aws_sigv4::sign::v4::generate_signing_key(&signer.secret, signed_at, REGION, "s3");
    let signature = aws_sigv4::sign::v4::calculate_signature(signing_key, encoded.as_bytes());
    let host = format!("s3.{REGION}.amazonaws.com");
    let form = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/{bucket}/"))
        .header("host", &host)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        // The field order a browser submits: the policy fields, then the key, then the file.
        .body(form_document(&[
            ("policy", encoded.as_str()),
            ("x-amz-algorithm", "AWS4-HMAC-SHA256"),
            ("x-amz-credential", credential.as_str()),
            ("x-amz-date", instant.as_str()),
            ("x-amz-signature", signature.as_str()),
            ("success_action_status", "201"),
            ("key", post_key),
        ]))
        .expect("the test request is well formed");
    let (posted, answered) = dispatch(&router, form).await;
    assert_eq!(
        posted, 201,
        "case {case:?}: the form upload must answer the status the form asked for, got \
         {posted} with {answered}"
    );
    // The object the upload really stored, read back through `HEAD`.
    let (head, _) = s3(
        &router,
        &signer,
        "HEAD",
        &format!("/{bucket}/{post_key}"),
        b"",
    )
    .await;
    assert_eq!(
        head, 200,
        "case {case:?}: the posted object must be readable, got {head}"
    );

    let on_post = collect_until(&router, &signer, case, &post_url, post_key).await;
    let posted_envelope = envelope_for(case, &on_post, post_key);
    for (pointer, expected) in [
        ("/source", json!(S3_SOURCE)),
        ("/detail-type", json!(OBJECT_CREATED)),
        ("/detail/reason", json!(POST_REASON)),
        ("/detail/bucket/name", json!(bucket)),
    ] {
        let actual = posted_envelope
            .pointer(pointer)
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {pointer} of the form POST's envelope to be {expected}, \
             got {actual} in {posted_envelope}"
        );
    }
    // And the rule pinning the PUT's own reason received nothing of the form upload, read at
    // the instant the form upload's envelope has already been delivered.
    let unmatched = drain(&router, &signer, case, &put_url).await;
    assert!(
        unmatched.is_empty(),
        "case {case:?}: a rule pinning PutObject must receive nothing a form POST produced, \
         got {unmatched:?}"
    );
}
