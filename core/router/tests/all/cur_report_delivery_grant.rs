// SPDX-License-Identifier: CC0-1.0

//! The Cost and Usage Report delivery grant, decided on the ASSEMBLED emulator.
//!
//! The rule under test: before the report service accepts a report definition it evaluates
//! the destination bucket's own policy for a grant of the report delivery to the delivery
//! service principal, and refuses the call with HTTP 400 `ValidationException` while that
//! grant is absent, persisting nothing — so the identical request is refused before the
//! policy is attached and served after it, and the definition exists only once the grant
//! does.
//!
//! The report service's own suite
//! (`services/service-com-amazonaws-costandusagereportservice-awsorigamiservicegatewayservice/tests/all/bucket_delivery_grant.rs`)
//! spans what the grant may say over a table of policies. What only this target can pin is
//! that the decision holds on the router `core/router/src/lib.rs` assembles: the policy is
//! written through the Amazon S3 front door of the whole emulator, the definition is
//! submitted to the report service's own front door beside it, and both resolve to the ONE
//! shared engine the production entrypoint mounts every service on.

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for: the Cost and Usage Report control plane is
/// addressed in it, and so is the Amazon S3 front door.
const REGION: &str = "us-east-1";

/// The bucket the report would be delivered into.
const BUCKET: &str = "cur-assembled-delivery-grant";

/// The report definition under test.
const REPORT: &str = "cur-assembled-delivery-grant-report";

/// The prefix the report is delivered under.
const PREFIX: &str = "cur";

/// The whole emulator on one engine, exactly as the production entrypoint assembles it.
fn emulator() -> MultiServiceRouter {
    let mut router = aws_emulator_router::build_router(aws_emulator_router::new_engine());
    // The signature is not what is under test: the stored policy and the decision taken on
    // it are.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header scoped to `service` in [`REGION`]: the credential scope is
/// how the assembled router picks the service a request is for.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261117/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One call to the report service on the awsJson1_1 wire.
async fn cur(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSOrigamiServiceGatewayService.{operation}"),
        )
        .header("authorization", authorization("cur"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One request to the emulator's Amazon S3 front door, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &[u8]) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/json")
        .body(Bytes::copy_from_slice(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
    )
}

/// The delivery grant AWS publishes for a report destination bucket: the bucket's ACL and
/// policy readable, and the report's objects writable, by the delivery principal, both
/// conditioned on this account and on its report definitions.
///
/// SOURCE: the published Cost and Usage Report bucket policy. Its action names and its
/// service principal are read off the report service's own constants, so this suite and the
/// handler cannot drift apart about what the grant is for.
fn published_grant(bucket: &str) -> Value {
    let principal =
        service_com_amazonaws_costandusagereportservice_awsorigamiservicegatewayservice::state::DELIVERY_SERVICE_PRINCIPAL;
    let bucket_actions =
        service_com_amazonaws_costandusagereportservice_awsorigamiservicegatewayservice::state::BUCKET_DELIVERY_ACTIONS;
    let object_action =
        service_com_amazonaws_costandusagereportservice_awsorigamiservicegatewayservice::state::OBJECT_DELIVERY_ACTION;
    let account = engine::emulator_account_id();
    let condition = json!({
        "StringEquals": {
            "aws:SourceAccount": account,
            "aws:SourceArn": format!("arn:aws:cur:{REGION}:{account}:definition/*"),
        }
    });
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Principal": { "Service": principal },
                "Action": bucket_actions,
                "Resource": format!("arn:aws:s3:::{bucket}"),
                "Condition": condition,
            },
            {
                "Effect": "Allow",
                "Principal": { "Service": principal },
                "Action": object_action,
                "Resource": format!("arn:aws:s3:::{bucket}/*"),
                "Condition": condition,
            },
        ],
    })
}

/// The report definition submitted on both attempts, byte for byte.
fn definition() -> Value {
    json!({
        "ReportDefinition": {
            "ReportName": REPORT,
            "TimeUnit": "DAILY",
            "Format": "textORcsv",
            "Compression": "GZIP",
            "AdditionalSchemaElements": ["RESOURCES"],
            "S3Bucket": BUCKET,
            "S3Prefix": PREFIX,
            "S3Region": REGION,
        },
    })
}

/// Whether the account holds a definition called `REPORT`, as the report service's own read
/// answers.
async fn holds_the_definition(router: &MultiServiceRouter, case: &str) -> bool {
    let (status, body) = cur(router, "DescribeReportDefinitions", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeReportDefinitions must answer 200, got {status} with {body:?}"
    );
    body["ReportDefinitions"]
        .as_array()
        .expect("ReportDefinitions is a list")
        .iter()
        .any(|listed| listed["ReportName"].as_str() == Some(REPORT))
}

/// RULE: on the assembled emulator, the definition is refused while its destination bucket
/// carries no delivery grant and served once that bucket's policy grants it — and no
/// definition exists until it does.
#[tokio::test]
async fn a_definition_is_served_only_once_its_bucket_grants_the_delivery() {
    let case = "the assembled emulator";
    let router = emulator();

    let (created, body) = s3(&router, "PUT", &format!("/{BUCKET}"), &[]).await;
    assert_eq!(
        created, 200,
        "case {case:?}: CreateBucket must answer 200, got {created} with {body:?}"
    );

    let (refused, refusal) = cur(&router, "PutReportDefinition", &definition()).await;
    assert_eq!(
        (refused, refusal["__type"].as_str().unwrap_or("<no __type>")),
        (400, "ValidationException"),
        "case {case:?}: an ungranted bucket must be refused, got {refused} with {refusal:?}"
    );
    assert!(
        !holds_the_definition(&router, case).await,
        "case {case:?}: a refused create must leave no definition behind"
    );

    let document = published_grant(BUCKET).to_string();
    let (attached, body) = s3(
        &router,
        "PUT",
        &format!("/{BUCKET}?policy"),
        document.as_bytes(),
    )
    .await;
    assert!(
        (200..300).contains(&attached),
        "case {case:?}: PutBucketPolicy must be accepted, got {attached} with {body:?}"
    );

    let (served, body) = cur(&router, "PutReportDefinition", &definition()).await;
    assert_eq!(
        served, 200,
        "case {case:?}: the identical call must be served once the grant is in place, got \
         {served} with {body:?}"
    );
    assert!(
        holds_the_definition(&router, case).await,
        "case {case:?}: the accepted definition must be reported by the service's own read"
    );
}
