// SPDX-License-Identifier: CC0-1.0

//! The finished scan of an image pushed into an Amazon ECR repository through the registry's
//! LAYER-UPLOAD sequence reaches the EventBridge rule that matches it, over a queue whose
//! grant is SCOPED to the invoking rule — through the WHOLE emulator.
//!
//! The RULE under test: **an image that entered a repository scanned on push through
//! `InitiateLayerUpload` -> `UploadLayerPart` -> `CompleteLayerUpload` -> `PutImage` is scanned
//! like any other, and the scan that FINISHES is announced once on the account's default
//! EventBridge bus (`source` `aws.ecr`, `detail-type` `ECR Image Scan`) carrying the
//! repository, the image's digest and the terminal status the run ended in; EventBridge makes
//! that delivery as its own service principal, so the QUEUE's own document decides it — a
//! grant for that principal admits the send whether it is unconditioned, scoped to the
//! invoking rule by `aws:SourceArn` or scoped to that rule's account by `aws:SourceAccount`,
//! while a grant scoped to ANOTHER rule admits nothing, and a rule that holds no target at all
//! delivers nowhere.**
//!
//! Three services have to agree for that to hold (Amazon ECR scans and publishes, EventBridge
//! routes and decides the delivery against the queue's document, Amazon SQS holds what
//! arrived), so this suite lives here. The sibling
//! [`super::ecr_scan_on_push_event_reaches_its_queue_target`] pins the announcement member for
//! member for a manifest pushed on its own under an unconditioned grant; what only this suite
//! reaches is the push that goes through the raw layer-upload sequence — a single-layer image
//! that is no filesystem basic scanning can read, so the run ends at the terminal status that
//! says so — and the SCOPING of the grant the delivery is decided against.
//!
//! The rows differ along what the rule generalises over: the form the queue's grant takes —
//! unconditioned, scoped to the invoking rule's own ARN, scoped to a pattern covering it,
//! scoped to the account it belongs to, and scoped to a DIFFERENT rule, which is the boundary.
//!
//! The second test drives one of those rows again with every SIGNATURE VERIFIED, which no
//! suite over this announcement reaches: the registry publishes it from a task of its own that
//! outlives the push that armed it, through the emulator's own callback, so a router that
//! authenticates every request has to accept that publication — and EventBridge's own send to
//! the queue — before anything arrives.
//!
//! Nothing here sleeps for an event: it POLLS the queues, exactly as a client does, and every
//! "nothing was delivered" read is made at the instant a rule that IS granted has already
//! received the same announcement, rather than after a span of the clock.

use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::ecr_scan_on_push_event_reaches_its_queue_target::{
    authorization, member, router, DEFAULT_BUS, DELIVERY_DEADLINE, ECR_TARGET_PREFIX,
    POLL_INTERVAL, REGION, SOURCE,
};

/// The `detail-type` Amazon ECR announces a finished image scan under, spelled out here rather
/// than imported from the service crate, so a wrong constant in the implementation cannot make
/// this suite pass.
const IMAGE_SCAN: &str = "ECR Image Scan";

/// The service principal EventBridge makes a target's call as: "for Lambda functions, Amazon
/// SNS topics and Amazon SQS queues, EventBridge relies on resource-based policies"
/// (`PutTargets`), so the queue's own policy is the document that admits the send.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The terminal `ScanStatus` a basic scan ends in when it could not examine the image. A basic
/// scan reports the vulnerabilities of the operating-system packages in an image's filesystem,
/// so an image whose only layer is no archive to read a package database out of is one basic
/// scanning does not support, and the run ends here rather than at the status whose findings
/// are a statement about the image.
const FAILED: &str = "FAILED";

/// The media types a Docker image manifest V2 schema 2 and the blobs it references declare.
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";
const CONFIG_MEDIA_TYPE: &str = "application/vnd.docker.container.image.v1+json";
const LAYER_MEDIA_TYPE: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

/// How a call of this suite is authenticated.
enum Caller<'a> {
    /// A credential scope that only ROUTES the request, on a router that verifies no
    /// signature — which is what the table test drives, because a grant's scoping has nothing
    /// to do with signing.
    Routed,
    /// A real SigV4 signature over the request, which the authenticating router verifies.
    Signed(&'a Signer),
}

/// The key pair this process publishes once request authentication is on.
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

/// One `X-Amz-Target` RPC call that has to answer 200, and the parsed body.
#[allow(clippy::too_many_arguments)]
async fn rpc(
    router: &MultiServiceRouter,
    caller: &Caller<'_>,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let built = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(body.to_string().into_bytes())
        .expect("the test request is well formed");
    let request = match caller {
        Caller::Routed => {
            let (mut parts, payload) = built.into_parts();
            parts.headers.insert(
                "authorization",
                authorization(signing_name)
                    .parse()
                    .expect("the credential scope is a header value"),
            );
            http::Request::from_parts(parts, Bytes::from(payload))
        }
        Caller::Signed(signer) => {
            let (parts, payload) = signer.sign(built, signing_name).into_parts();
            http::Request::from_parts(parts, Bytes::from(payload))
        }
    };
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon ECR call (`awsJson1_1`).
async fn ecr(
    router: &MultiServiceRouter,
    caller: &Caller<'_>,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        caller,
        case,
        "ecr",
        ECR_TARGET_PREFIX,
        "1.1",
        operation,
        body,
    )
    .await
}

/// One EventBridge call (`awsJson1_1`).
async fn events(
    router: &MultiServiceRouter,
    caller: &Caller<'_>,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        caller,
        case,
        "events",
        "AWSEvents",
        "1.1",
        operation,
        body,
    )
    .await
}

/// One Amazon SQS call (`awsJson1_0`).
async fn sqs(
    router: &MultiServiceRouter,
    caller: &Caller<'_>,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        caller,
        case,
        "sqs",
        "AmazonSQS",
        "1.0",
        operation,
        body,
    )
    .await
}

/// The content digest of `content`: the `sha256:<hex>` form a registry addresses a blob by,
/// computed here independently of the emulator's own helper.
fn content_digest(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex::encode(Sha256::digest(content)))
}

/// `len` bytes that depend only on `len` and `marker`: content spread over the whole byte
/// range, as a compressed layer's bytes are, and no archive any scanner can read.
fn blob_bytes(marker: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| u8::try_from((index * 31 + usize::from(marker) * 7 + 13) % 256).unwrap_or(0))
        .collect()
}

/// Register one blob in `repository` through the sequence a registry client uses, and answer
/// the digest the registry stored it under.
async fn register_blob(
    router: &MultiServiceRouter,
    caller: &Caller<'_>,
    case: &str,
    repository: &str,
    bytes: &[u8],
) -> String {
    let opened = ecr(
        router,
        caller,
        case,
        "InitiateLayerUpload",
        &json!({"repositoryName": repository}),
    )
    .await;
    let upload_id = member(case, &opened, "/uploadId");
    let last = i64::try_from(bytes.len()).expect("a test blob fits in an i64") - 1;
    let uploaded = ecr(
        router,
        caller,
        case,
        "UploadLayerPart",
        &json!({
            "repositoryName": repository,
            "uploadId": &upload_id,
            "partFirstByte": 0,
            "partLastByte": last,
            "layerPartBlob": aws_smithy_types::base64::encode(bytes),
        }),
    )
    .await;
    assert_eq!(
        uploaded["lastByteReceived"].as_i64(),
        Some(last),
        "case {case:?}: the upload must report the last byte it holds, got {uploaded}"
    );
    let digest = content_digest(bytes);
    let completed = ecr(
        router,
        caller,
        case,
        "CompleteLayerUpload",
        &json!({
            "repositoryName": repository,
            "uploadId": &upload_id,
            "layerDigests": [&digest],
        }),
    )
    .await;
    assert_eq!(
        completed["layerDigest"].as_str(),
        Some(digest.as_str()),
        "case {case:?}: the completed upload must register the blob under the digest of its own \
         bytes ({digest}), got {completed}"
    );
    digest
}

/// Push a single-layer image into `repository` through the raw layer-upload sequence, and
/// answer the digest the registry stored its manifest under.
///
/// `marker` makes one push's content differ from another's, so every push below is a distinct
/// image with a scan of its own.
async fn push_layered_image(
    router: &MultiServiceRouter,
    caller: &Caller<'_>,
    case: &str,
    repository: &str,
    marker: u8,
    tag: &str,
) -> String {
    let config = blob_bytes(marker, 96);
    let layer = blob_bytes(marker.wrapping_add(101), 512);
    let config_digest = register_blob(router, caller, case, repository, &config).await;
    let layer_digest = register_blob(router, caller, case, repository, &layer).await;
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "config": {
            "mediaType": CONFIG_MEDIA_TYPE,
            "digest": &config_digest,
            "size": config.len(),
        },
        "layers": [{
            "mediaType": LAYER_MEDIA_TYPE,
            "digest": &layer_digest,
            "size": layer.len(),
        }],
    })
    .to_string();
    let pushed = ecr(
        router,
        caller,
        case,
        "PutImage",
        &json!({
            "repositoryName": repository,
            "imageManifest": &manifest,
            "imageManifestMediaType": MANIFEST_MEDIA_TYPE,
            "imageTag": tag,
        }),
    )
    .await;
    let digest = member(case, &pushed, "/image/imageId/imageDigest");
    assert_eq!(
        digest,
        content_digest(manifest.as_bytes()),
        "case {case:?}: the manifest must be stored under the digest of its own bytes, got \
         {pushed}"
    );
    digest
}

/// A queue carrying no policy at all yet, answering its url and the ARN a rule names it by.
async fn bare_queue(
    router: &MultiServiceRouter,
    caller: &Caller<'_>,
    case: &str,
    name: &str,
) -> (String, String) {
    let created = sqs(
        router,
        caller,
        case,
        "CreateQueue",
        &json!({"QueueName": name}),
    )
    .await;
    let url = member(case, &created, "/QueueUrl");
    let attributes = sqs(
        router,
        caller,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = member(case, &attributes, "/Attributes/QueueArn");
    (url, arn)
}

/// The policy document a row grants on its queue: `sqs:SendMessage` for EventBridge's own
/// service principal on that queue, under the row's `Condition`.
fn policy(queue_arn: &str, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Sid": "AllowEventBridgeDelivery",
        "Effect": "Allow",
        "Principal": {"Service": EVENTS_PRINCIPAL},
        "Action": "sqs:SendMessage",
        "Resource": queue_arn,
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({"Version": "2012-10-17", "Statement": [statement]})
}

/// Put `document` on the queue at `url` as its `Policy`, exactly as a client does.
async fn grant(
    router: &MultiServiceRouter,
    caller: &Caller<'_>,
    case: &str,
    url: &str,
    document: &Value,
) {
    sqs(
        router,
        caller,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": url, "Attributes": {"Policy": document.to_string()}}),
    )
    .await;
}

/// An ENABLED rule on the default bus matching the finished scans of `repository`, with no
/// target yet, answering the ARN EventBridge minted for it.
async fn rule(
    router: &MultiServiceRouter,
    caller: &Caller<'_>,
    case: &str,
    name: &str,
    repository: &str,
) -> String {
    let created = events(
        router,
        caller,
        case,
        "PutRule",
        &json!({
            "Name": name,
            "EventBusName": DEFAULT_BUS,
            "EventPattern": json!({
                "source": [SOURCE],
                "detail-type": [IMAGE_SCAN],
                "detail": {"repository-name": [repository]},
            })
            .to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    member(case, &created, "/RuleArn")
}

/// Give the rule `rule_name` the queue `queue_arn` as its one target.
async fn target(
    router: &MultiServiceRouter,
    caller: &Caller<'_>,
    case: &str,
    rule_name: &str,
    queue_arn: &str,
) {
    let targeted = events(
        router,
        caller,
        case,
        "PutTargets",
        &json!({
            "Rule": rule_name,
            "EventBusName": DEFAULT_BUS,
            "Targets": [{"Id": "t1", "Arn": queue_arn}],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"],
        json!(0),
        "case {case:?}: every target must be accepted, got {targeted}"
    );
}

/// Every envelope now waiting on the queue at `url`.
async fn drain(
    router: &MultiServiceRouter,
    caller: &Caller<'_>,
    case: &str,
    url: &str,
) -> Vec<Value> {
    let received = sqs(
        router,
        caller,
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
            let raw = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(raw).unwrap_or(Value::Null)
        })
        .collect()
}

/// Collect envelopes off the queue at `url` until one names the image `digest` or
/// [`DELIVERY_DEADLINE`] passes, and answer everything that arrived.
async fn collect_until(
    router: &MultiServiceRouter,
    caller: &Caller<'_>,
    case: &str,
    url: &str,
    digest: &str,
) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + DELIVERY_DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(router, caller, case, url).await);
        let found = arrived
            .iter()
            .any(|envelope| envelope.pointer("/detail/image-digest") == Some(&json!(digest)));
        if found || tokio::time::Instant::now() >= deadline {
            return arrived;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The one envelope of `arrived` naming the image `digest`, or a panic naming the case.
fn announcement<'a>(case: &str, arrived: &'a [Value], digest: &str) -> &'a Value {
    arrived
        .iter()
        .find(|envelope| envelope.pointer("/detail/image-digest") == Some(&json!(digest)))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an {IMAGE_SCAN:?} announcement naming the pushed image \
                 {digest}, got {arrived:?}"
            )
        })
}

/// One row: the form the queue's grant takes, and whether a send made for the invoking rule is
/// admitted by it.
struct Row {
    case: &'static str,
    /// Suffix naming this row's repository, queues and rules.
    slug: &'static str,
    /// The `Condition` block the row's grant carries, built from the ARN of the rule the
    /// delivery is made for. `None` is an unconditioned grant.
    condition: fn(&str) -> Option<Value>,
    /// Whether the announcement reaches the row's queue once that rule holds it as a target.
    delivered: bool,
}

/// The ARN of a rule of this account that is NOT the one being invoked — the boundary row's
/// scope.
fn another_rule_arn() -> String {
    format!(
        "arn:aws:events:{REGION}:{}:rule/ecr-scan-some-other-rule",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// The rows: the grant scoping forms a client writes for an EventBridge target, and the one
/// that scopes the grant away from the invoking rule.
const ROWS: &[Row] = &[
    Row {
        case: "a grant for the service principal with no condition at all",
        slug: "plain",
        condition: |_rule_arn| None,
        delivered: true,
    },
    Row {
        case: "a grant scoped to the invoking rule's own ARN with ArnEquals",
        slug: "arn-equals",
        condition: |rule_arn| Some(json!({"ArnEquals": {"aws:SourceArn": rule_arn}})),
        delivered: true,
    },
    Row {
        case: "a grant scoped by a pattern covering every rule of this account",
        slug: "arn-like",
        condition: |rule_arn| {
            let prefix = rule_arn.rsplit_once('/').map_or(rule_arn, |(head, _)| head);
            Some(json!({"ArnLike": {"aws:SourceArn": format!("{prefix}/*")}}))
        },
        delivered: true,
    },
    Row {
        case: "a grant scoped to the account the invoking rule belongs to",
        slug: "source-account",
        condition: |_rule_arn| {
            Some(json!({"StringEquals": {"aws:SourceAccount": engine::EMULATOR_ACCOUNT_ID}}))
        },
        delivered: true,
    },
    Row {
        case: "a grant scoped to a DIFFERENT rule's ARN",
        slug: "other-rule",
        condition: |_rule_arn| Some(json!({"ArnEquals": {"aws:SourceArn": another_rule_arn()}})),
        delivered: false,
    },
];

/// Drive one row end to end: the absence leg on a rule with no target, then the row's grant,
/// the target, and the next push's announcement.
async fn drive(router: &MultiServiceRouter, caller: &Caller<'_>, row: &Row) {
    let case = row.case;
    let repository = format!("sim-ecr-scoped-{}", row.slug);
    ecr(
        router,
        caller,
        case,
        "CreateRepository",
        &json!({
            "repositoryName": &repository,
            "imageScanningConfiguration": {"scanOnPush": true},
        }),
    )
    .await;

    // The row's own queue and rule, and a probe rule whose queue grants this service the send
    // with nothing to scope it: the probe's arrival is the instant at which the announcement
    // has been published and routed, which is when every "nothing arrived" read below is made.
    let (row_url, row_arn) = bare_queue(router, caller, case, &format!("ecr-scan-{}", row.slug))
        .await;
    let (probe_url, probe_arn) =
        bare_queue(router, caller, case, &format!("ecr-scan-{}-probe", row.slug)).await;
    grant(router, caller, case, &probe_url, &policy(&probe_arn, None)).await;

    let rule_name = format!("ecr-scan-{}", row.slug);
    let rule_arn = rule(router, caller, case, &rule_name, &repository).await;
    let probe_rule = format!("ecr-scan-{}-probe", row.slug);
    rule(router, caller, case, &probe_rule, &repository).await;
    target(router, caller, case, &probe_rule, &probe_arn).await;

    // The rule under test holds no target yet, which is what the registry's first announcement
    // is published against.
    let targets = events(
        router,
        caller,
        case,
        "ListTargetsByRule",
        &json!({"Rule": &rule_name, "EventBusName": DEFAULT_BUS}),
    )
    .await;
    assert_eq!(
        targets["Targets"].as_array().map_or(0, Vec::len),
        0,
        "case {case:?}: the rule under test must start with no target, got {targets}"
    );

    let first = push_layered_image(router, caller, case, &repository, 1, "v1").await;
    let probed = collect_until(router, caller, case, &probe_url, &first).await;
    announcement(case, &probed, &first);
    let untargeted = drain(router, caller, case, &row_url).await;
    assert!(
        untargeted.is_empty(),
        "case {case:?}: a rule with no target must deliver nothing, got {untargeted:?}"
    );

    // The row's grant, then the target: the next push's finished scan is decided against the
    // document the queue now carries.
    grant(
        router,
        caller,
        case,
        &row_url,
        &policy(&row_arn, (row.condition)(&rule_arn)),
    )
    .await;
    target(router, caller, case, &rule_name, &row_arn).await;

    let second = push_layered_image(router, caller, case, &repository, 2, "v2").await;
    let probed = collect_until(router, caller, case, &probe_url, &second).await;
    let probe_envelope = announcement(case, &probed, &second).clone();

    // What the scan the push started reads back as, which is what the announcement reports: an
    // image whose single layer is no filesystem basic scanning can read cannot be examined, so
    // the run ends at the terminal status that says so.
    let read = ecr(
        router,
        caller,
        case,
        "DescribeImageScanFindings",
        &json!({"repositoryName": &repository, "imageId": {"imageDigest": &second}}),
    )
    .await;
    let reported = member(case, &read, "/imageScanStatus/status");
    assert_eq!(
        reported, FAILED,
        "case {case:?}: a scan that cannot examine the image it was started on must end \
         {FAILED:?}, got {reported:?} in {read}"
    );
    for (pointer, expected) in [
        ("/source", json!(SOURCE)),
        ("/detail-type", json!(IMAGE_SCAN)),
        ("/detail/repository-name", json!(repository)),
        ("/detail/scan-status", json!(reported)),
    ] {
        let actual = probe_envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {pointer} of the announcement to be {expected}, got \
             {actual} in {probe_envelope}"
        );
    }

    if row.delivered {
        let arrived = collect_until(router, caller, case, &row_url, &second).await;
        let envelope = announcement(case, &arrived, &second);
        assert_eq!(
            envelope.pointer("/detail/scan-status"),
            Some(&json!(reported)),
            "case {case:?}: expected the delivered announcement to report {reported:?}, got \
             {envelope}"
        );
        // One scan that finished is ONE announcement: a second copy would make a consumer act
        // twice on one run.
        let copies = arrived
            .iter()
            .filter(|envelope| envelope.pointer("/detail/image-digest") == Some(&json!(second)))
            .count();
        assert_eq!(
            copies, 1,
            "case {case:?}: the finished scan must be announced once, got {copies} copies in \
             {arrived:?}"
        );
        assert!(
            !arrived
                .iter()
                .any(|envelope| envelope.pointer("/detail/image-digest") == Some(&json!(first))),
            "case {case:?}: the first push's announcement was delivered to a rule that had no \
             target at the time: {arrived:?}"
        );
    } else {
        let withheld = drain(router, caller, case, &row_url).await;
        assert!(
            withheld.is_empty(),
            "case {case:?}: a queue whose grant is scoped to another rule must receive nothing, \
             got {withheld:?}"
        );
    }
}

/// RULE: the finished scan of an image pushed through the layer-upload sequence is announced
/// and delivered to the matching rule's queue target exactly where the queue's own document
/// admits this service's send for THAT rule — and a rule with no target delivers nowhere.
#[tokio::test]
async fn a_scan_on_push_reaches_a_queue_whose_grant_is_scoped_to_the_invoking_rule() {
    let router = router();
    for row in ROWS {
        drive(&router, &Caller::Routed, row).await;
    }
}

/// The SAME rule with every signature verified: the announcement is published from a task of
/// the registry's that outlives the push it was armed by, through the emulator's own callback,
/// so an authenticating router has to accept that publication and EventBridge's own send
/// before the queue holds anything.
#[tokio::test]
async fn a_scan_on_push_announcement_is_published_through_an_authenticating_router() {
    let mut router = router();
    router.set_request_authentication(true);
    let published = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning authentication on publishes this process's keys");
    let signer = Signer {
        key_id: published.access_key_id.clone(),
        secret: published.secret_access_key.clone(),
    };
    let row = Row {
        case: "a grant scoped to the invoking rule's own ARN, with signatures verified",
        slug: "signed",
        condition: |rule_arn| Some(json!({"ArnEquals": {"aws:SourceArn": rule_arn}})),
        delivered: true,
    };
    drive(&router, &Caller::Signed(&signer), &row).await;
}
