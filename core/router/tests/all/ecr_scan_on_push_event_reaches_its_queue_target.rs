// SPDX-License-Identifier: CC0-1.0

//! An image push into an Amazon ECR repository that is SCANNED ON PUSH reaches the
//! EventBridge rule that matches the finished scan, through the WHOLE emulator.
//!
//! The RULE under test: **a repository whose `imageScanningConfiguration.scanOnPush` is
//! true is scanned on every push ("if set to `true`, images will be scanned after being
//! pushed", `ImageScanningConfiguration$scanOnPush`), and the scan that FINISHES is
//! announced once on the account's default EventBridge bus as `source` `aws.ecr` /
//! `detail-type` `ECR Image Scan`, naming the scanned repository in `resources` and
//! carrying in `detail` the status the run ended in, the repository, the image's digest, its
//! tags and the counts of its findings per severity; EventBridge then routes it by its
//! CONTENT, so a rule whose `EventPattern` matches delivers it to that rule's targets — a
//! rule with NO target delivers it nowhere, and a rule whose pattern names another
//! repository receives none of them.**
//!
//! Three services have to agree for that to hold (Amazon ECR scans and publishes,
//! EventBridge routes and delivers, Amazon SQS holds what arrived), so this suite lives
//! here: a crate-local test can only ever see one side of it. The crate-local suite
//! `services/service-com-amazonaws-ecr-amazonec2containerregistry-v20150921/tests/all/scan_on_push.rs`
//! pins the same rule over a TABLE of repository configurations and image filesystems, and
//! pins the published `detail` against what the scan the push started reads back as.
//!
//! Nothing here sleeps for an event: it POLLS the queue, exactly as a client does — and a
//! "nothing was delivered" read is made at the instant a rule that IS targeted has already
//! received the same announcement, rather than after a span of the clock.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
pub(crate) const REGION: &str = "us-west-2";

/// The `source` and `detail-type` Amazon ECR announces a finished image scan under, spelled
/// out here rather than imported from the service crate, so a wrong constant in the
/// implementation cannot make this suite pass.
pub(crate) const SOURCE: &str = "aws.ecr";
const IMAGE_SCAN: &str = "ECR Image Scan";

/// The bus an AWS service's own events go to.
pub(crate) const DEFAULT_BUS: &str = "default";

/// Amazon ECR's `X-Amz-Target` prefix, from that service's own model.
pub(crate) const ECR_TARGET_PREFIX: &str = "AmazonEC2ContainerRegistry_V20150921";

/// How long the suite waits for an announcement to arrive before giving up. A GENEROUS
/// ceiling, not an expectation: nothing here asserts on how long anything took.
pub(crate) const DELIVERY_DEADLINE: Duration = Duration::from_secs(30);

/// One poll of a queue inside [`DELIVERY_DEADLINE`].
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The whole emulator over one engine, exactly as `core/router/src/lib.rs` assembles it for
/// the binary.
pub(crate) fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: the router above runs with request
/// authentication off.
pub(crate) fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `X-Amz-Target` RPC call against the service whose signing name is `signing_name`,
/// which must answer 200.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
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

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`).
pub(crate) async fn events(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`).
pub(crate) async fn sqs(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// One Amazon ECR call (`awsJson1_1`).
pub(crate) async fn ecr(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        case,
        "ecr",
        ECR_TARGET_PREFIX,
        "1.1",
        operation,
        body,
    )
    .await
}

/// A string member of a response, or a panic naming the case and what came back.
pub(crate) fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// An SQS queue carrying the grant a rule's target needs — EventBridge makes the target's
/// `sqs:SendMessage` as its own service principal and "relies on resource-based policies"
/// for a queue (`PutTargets`), so a queue that has granted it nothing receives nothing —
/// answering its url and the ARN a rule names it by.
pub(crate) async fn queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({"QueueName": name})).await;
    let url = member(case, &created, "/QueueUrl");
    let attributes = sqs(
        router,
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
            "Principal": {"Service": "events.amazonaws.com"},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// An ENABLED rule on the default bus matching the finished scans of `repository`, with no
/// target yet.
async fn rule(router: &MultiServiceRouter, case: &str, name: &str, repository: &str) {
    events(
        router,
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
}

/// Give `rule_name` the queue `queue_arn` as its one target.
pub(crate) async fn target(
    router: &MultiServiceRouter,
    case: &str,
    rule_name: &str,
    queue_arn: &str,
) {
    let targeted = events(
        router,
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
async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
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

/// Collect envelopes off the queue at `url` until one naming the image `digest` has arrived
/// or [`DELIVERY_DEADLINE`] passes, and answer everything that arrived.
async fn collect_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    digest: &str,
) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + DELIVERY_DEADLINE;
    let mut arrived: Vec<Value> = Vec::new();
    loop {
        arrived.extend(drain(router, case, url).await);
        let found = arrived
            .iter()
            .any(|envelope| envelope.pointer("/detail/image-digest") == Some(&json!(digest)));
        if found || tokio::time::Instant::now() >= deadline {
            return arrived;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The one envelope of `arrived` that names the image `digest`, or a panic naming the case.
fn announcement<'a>(case: &str, arrived: &'a [Value], digest: &str) -> &'a Value {
    arrived
        .iter()
        .find(|envelope| envelope.pointer("/detail/image-digest") == Some(&json!(digest)))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an 'ECR Image Scan' announcement naming the pushed \
                 image {digest}, got {arrived:?}"
            )
        })
}

/// Push a manifest whose bytes are the row's own into `repository`, under `tag` when one is
/// named, and answer the digest the registry stored it under. An untagged push names none.
pub(crate) async fn push(
    router: &MultiServiceRouter,
    case: &str,
    repository: &str,
    marker: &str,
    tag: Option<&str>,
) -> String {
    let manifest = json!({"schemaVersion": 2, "marker": marker}).to_string();
    let mut body = json!({"repositoryName": repository, "imageManifest": manifest});
    if let Some(tag) = tag {
        body["imageTag"] = json!(tag);
    }
    let pushed = ecr(router, case, "PutImage", &body).await;
    member(case, &pushed, "/image/imageId/imageDigest")
}

/// RULE: the finished scan of a pushed image is announced on the default bus and delivered
/// to the rule whose pattern matches it — and to no rule that has no target, and to no rule
/// whose pattern names another repository.
///
/// Nothing here waits a fixed span for a delivery that must NOT happen: a third rule on the
/// same repository is targeted from the start, so the arrival of one push's announcement
/// THERE is the instant at which the announcement has been published and routed — and every
/// "received nothing" read is made at that instant.
#[tokio::test]
async fn a_scan_on_push_reaches_the_matching_rules_queue_target() {
    let case = "a push into a repository scanned on push";
    let router = router();
    let scanned = "sim-ecr-scan-on-push";
    let other = "sim-ecr-other-repository";

    // The queue a rule is given as its target only LATER, the queue that observes when an
    // announcement has been routed, and the queue only a rule that cannot match targets.
    let (late_url, late_arn) = queue(&router, case, "ecr-scan-late").await;
    let (probe_url, probe_arn) = queue(&router, case, "ecr-scan-probe").await;
    let (other_url, other_arn) = queue(&router, case, "ecr-scan-other").await;

    for repository in [scanned, other] {
        ecr(
            &router,
            case,
            "CreateRepository",
            &json!({
                "repositoryName": repository,
                "imageScanningConfiguration": {"scanOnPush": true},
            }),
        )
        .await;
    }

    // Three rules: the one under test, matching the scanned repository with NO target yet;
    // the probe, matching the same repository with a target from the start; and one naming
    // another repository, also targeted from the start.
    rule(&router, case, "ecr-scan-late", scanned).await;
    rule(&router, case, "ecr-scan-probe", scanned).await;
    target(&router, case, "ecr-scan-probe", &probe_arn).await;
    rule(&router, case, "ecr-scan-other", other).await;
    target(&router, case, "ecr-scan-other", &other_arn).await;

    // The first push: both rules on the scanned repository match its finished scan, and
    // only the probe has a target to deliver to.
    let first_digest = push(&router, case, scanned, "first", Some("v1")).await;
    let probed = collect_until(&router, case, &probe_url, &first_digest).await;
    announcement(case, &probed, &first_digest);

    // The announcement has been published and routed — so at this instant the rule with no
    // target has delivered nothing, and neither has the rule naming another repository.
    let untargeted = drain(&router, case, &late_url).await;
    assert!(
        untargeted.is_empty(),
        "case {case:?}: a rule with no target must deliver nothing, got {untargeted:?}"
    );
    let unmatched = drain(&router, case, &other_url).await;
    assert!(
        unmatched.is_empty(),
        "case {case:?}: a rule whose pattern names another repository must receive nothing, \
         got {unmatched:?}"
    );

    // The positive control on that same rule: it is given the queue as its target, and the
    // next push's finished scan arrives there.
    target(&router, case, "ecr-scan-late", &late_arn).await;
    let digest = push(&router, case, scanned, "second", Some("v2")).await;

    let arrived = collect_until(&router, case, &late_url, &digest).await;
    assert!(
        !arrived.iter().any(|envelope| envelope
            .pointer("/detail/image-digest")
            == Some(&json!(first_digest))),
        "case {case:?}: the first push's announcement was delivered to a rule that had no \
         target at the time, yet the queue carries the scan of {first_digest}: {arrived:?}"
    );
    let envelope = announcement(case, &arrived, &digest);

    // What the scan the push started reads back as, which is what the announcement reports.
    let read = ecr(
        &router,
        case,
        "DescribeImageScanFindings",
        &json!({"repositoryName": scanned, "imageId": {"imageDigest": &digest}}),
    )
    .await;
    let reported_status = member(case, &read, "/imageScanStatus/status");
    let expected_arn = format!(
        "arn:aws:ecr:{REGION}:{}:repository/{scanned}",
        engine::EMULATOR_ACCOUNT_ID
    );
    for (pointer, expected) in [
        ("/source", json!(SOURCE)),
        ("/detail-type", json!(IMAGE_SCAN)),
        ("/account", json!(engine::EMULATOR_ACCOUNT_ID)),
        ("/region", json!(REGION)),
        ("/resources", json!([expected_arn])),
        ("/detail/repository-name", json!(scanned)),
        ("/detail/image-digest", json!(digest)),
        ("/detail/image-tags", json!(["v2"])),
        ("/detail/scan-status", json!(reported_status)),
        ("/detail/finding-severity-counts", json!({})),
    ] {
        let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {pointer} of the announcement to be {expected}, got \
             {actual} in {envelope}"
        );
    }

    // The negative half: the rule naming another repository matched nothing either push
    // caused, so its own queue — which has had a target since before both pushes — is empty
    // at the instant the matching rule's own announcement has already been delivered.
    let withheld = drain(&router, case, &other_url).await;
    assert!(
        withheld.is_empty(),
        "case {case:?}: a rule whose pattern names another repository must receive nothing, \
         got {withheld:?}"
    );
}
