// SPDX-License-Identifier: CC0-1.0

//! The DELIVERY GRANT a destination bucket has to carry before this service accepts a
//! report definition against it, driven over the awsJson1_1 wire with Amazon S3 beside this
//! service on ONE engine.
//!
//! The rule under test: an operation that accepts a `ReportDefinition` evaluates the
//! destination bucket's OWN policy for a grant, to the report-delivery service principal, of
//! every Amazon S3 action the delivery performs — reading the bucket's ACL and its policy,
//! and writing the report's files under the definition's prefix — and refuses the call with
//! HTTP 400 `ValidationException` when the policy grants any of them to nobody, persisting
//! nothing. Both operations that take a definition judge it, so a definition cannot be
//! created against, or moved onto, a bucket the report could never be delivered to.
//!
//! The precondition is judged PER CALL rather than once per bucket, so the byte-identical
//! request refused against an unpolicied bucket is served as soon as the policy is put on
//! that same bucket — which is the signal a client provisioning a bucket and a definition
//! together retries on.
//!
//! The rows differ along what the rule generalises over: which delivery action the policy
//! grants, to which principal, on which resource, under which condition and which condition
//! operator, whether a `Deny` sits beside the grant, which prefix the report is delivered
//! under, and which Region the definition states for its bucket — never along the identity
//! of a bucket or the name of a report.
//!
//! A bucket this emulator's object store does not hold carries no policy to ground a
//! refusal in, so it is not judged here and the suites that create no bucket at all are
//! unaffected by this gate.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_costandusagereportservice_awsorigamiservicegatewayservice as cur;

use super::report_definition_lifecycle::error_code;

/// The Region every call here is signed for: the Cost and Usage Report control plane is
/// addressed in it, and so is the Amazon S3 front door.
const REGION: &str = "us-east-1";

/// The service principal the published delivery grant names, read off this service's own
/// constant so the suite and the handler cannot drift apart about WHO the grant is for.
const DELIVERY_PRINCIPAL: &str = cur::state::DELIVERY_SERVICE_PRINCIPAL;

/// The prefix most rows deliver the report under.
const PREFIX: &str = "cur-delivery-prefix";

/// This service and Amazon S3 over ONE shared engine, so the policy this service reads is
/// the document Amazon S3's own `PutBucketPolicy` wrote on a bucket its own `CreateBucket`
/// made.
struct Destinations {
    router: MultiServiceRouter,
}

impl Destinations {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The signature is not what is under test: the stored policy and the decision taken
        // on it are.
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(cur::EmulatedService::new(engine));
        Self { router }
    }

    /// One call to this service on the awsJson1_1 wire. The SigV4 credential scope names the
    /// `cur` signing name, which is how the router picks this service.
    async fn call(&self, operation: &str, body: &Value) -> (u16, Value) {
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
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One request to the Amazon S3 front door, path-style: the bucket and any subresource
    /// live in the URI and the body is the payload.
    async fn s3(&self, method: &str, path: &str, body: &[u8]) -> (u16, String) {
        let host = format!("s3.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("authorization", authorization("s3"))
            .header("content-type", "application/json")
            .body(Bytes::copy_from_slice(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (
            status,
            String::from_utf8_lossy(response.into_body().as_ref()).to_string(),
        )
    }

    /// `PUT /<bucket>` — Amazon S3's `CreateBucket`.
    async fn create_bucket(&self, case: &str, bucket: &str) {
        let (status, body) = self.s3("PUT", &format!("/{bucket}"), &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateBucket {bucket:?} must answer 200, got {status} with {body:?}"
        );
    }

    /// `PUT /<bucket>?policy` — Amazon S3's `PutBucketPolicy`.
    async fn put_bucket_policy(&self, case: &str, bucket: &str, document: &Value) {
        let (status, body) = self
            .s3(
                "PUT",
                &format!("/{bucket}?policy"),
                document.to_string().as_bytes(),
            )
            .await;
        assert!(
            (200..300).contains(&status),
            "case {case:?}: PutBucketPolicy on {bucket:?} must be accepted, got {status} with \
             {body:?}"
        );
    }

    /// A bucket of this account carrying `policy`, or carrying none for `None`.
    async fn bucket_with(&self, case: &str, bucket: &str, policy: Option<&Value>) {
        self.create_bucket(case, bucket).await;
        if let Some(policy) = policy {
            self.put_bucket_policy(case, bucket, policy).await;
        }
    }

    /// Every definition the account holds, over as many pages as it takes.
    async fn described(&self, case: &str) -> Vec<Value> {
        let mut page = json!({});
        let mut seen = Vec::new();
        loop {
            let (status, body) = self.call("DescribeReportDefinitions", &page).await;
            assert_eq!(
                status, 200,
                "case {case:?}: DescribeReportDefinitions must answer 200, got {status} with \
                 {body:?}"
            );
            seen.extend(
                body["ReportDefinitions"]
                    .as_array()
                    .expect("ReportDefinitions is a list")
                    .iter()
                    .cloned(),
            );
            match body["NextToken"].as_str() {
                Some(token) => page = json!({ "NextToken": token }),
                None => return seen,
            }
        }
    }

    /// The definition the account holds under `name`, if any.
    async fn described_as(&self, case: &str, name: &str) -> Option<Value> {
        self.described(case)
            .await
            .into_iter()
            .find(|listed| listed["ReportName"].as_str() == Some(name))
    }
}

/// A SigV4 `Authorization` header scoped to `service` in [`REGION`]: the credential scope is
/// how the router picks the service a request is for. Nothing here is verified.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261117/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The ARN of the bucket itself, as a bucket policy names it: a bucket name is global to the
/// partition, so the ARN carries neither Region nor account.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// The ARN of the bucket's objects.
fn objects_of(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}/*")
}

/// One statement of a bucket policy.
fn statement(
    effect: &str,
    principal: Value,
    actions: Value,
    resources: Value,
    condition: Option<Value>,
) -> Value {
    let mut statement = json!({
        "Effect": effect,
        "Principal": principal,
        "Action": actions,
        "Resource": resources,
    });
    if let Some(condition) = condition {
        statement
            .as_object_mut()
            .expect("a statement is a JSON object")
            .insert("Condition".to_string(), condition);
    }
    statement
}

/// A document carrying `statements`.
fn document(statements: Vec<Value>) -> Value {
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The condition the bucket policy AWS publishes for a report destination carries, under
/// `operator`: the account the report is delivered for, and that account's report
/// definitions.
///
/// SOURCE: the published Cost and Usage Report bucket policy, whose clause names
/// `aws:SourceAccount` and the `arn:aws:cur:us-east-1:<account>:definition/*`
/// `aws:SourceArn`. Spelled out here rather than read off this service's own helper, so a
/// change in what the verification presents is caught rather than mirrored.
fn source_condition(operator: &str, region: &str, account: &str) -> Value {
    json!({
        operator: {
            "aws:SourceAccount": account,
            "aws:SourceArn": format!("arn:aws:cur:{region}:{account}:definition/*"),
        }
    })
}

/// The delivery grant AWS publishes for a report destination bucket: the bucket's ACL and
/// policy readable, and the report's objects writable, by the delivery principal — both
/// statements conditioned as `condition` says.
fn published_grant(bucket: &str, condition: Option<Value>) -> Value {
    document(vec![
        statement(
            "Allow",
            json!({ "Service": DELIVERY_PRINCIPAL }),
            json!(cur::state::BUCKET_DELIVERY_ACTIONS),
            json!(bucket_arn(bucket)),
            condition.clone(),
        ),
        statement(
            "Allow",
            json!({ "Service": DELIVERY_PRINCIPAL }),
            json!(cur::state::OBJECT_DELIVERY_ACTION),
            json!(objects_of(bucket)),
            condition,
        ),
    ])
}

/// The published grant as this account's own policy spells it: conditioned on this account
/// and its report definitions, matched with IAM's string equality.
fn account_grant(bucket: &str) -> Value {
    published_grant(
        bucket,
        Some(source_condition(
            "StringEquals",
            REGION,
            engine::emulator_account_id(),
        )),
    )
}

/// The report definition a row submits.
fn definition(name: &str, bucket: &str, prefix: &str, region: &str) -> Value {
    json!({
        "ReportName": name,
        "TimeUnit": "DAILY",
        "Format": "textORcsv",
        "Compression": "GZIP",
        "AdditionalSchemaElements": ["RESOURCES"],
        "S3Bucket": bucket,
        "S3Prefix": prefix,
        "S3Region": region,
    })
}

/// The `Message` of an error body, under either casing the envelope may use.
fn message(body: &Value) -> &str {
    body["Message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or("<no Message>")
}

/// The refusal a destination whose policy does not grant the delivery is answered with: the
/// account the verification ran for, and the bucket and Region the REQUEST named.
fn verification_failed(bucket: &str, region: &str) -> String {
    format!(
        "Failed to verify customer bucket permission. accountId= {}, bucket name: {bucket}, \
         bucket region: {region}",
        engine::emulator_account_id()
    )
}

/// Assert this service reports `submitted` member for member as the request named it.
fn assert_reports_as_submitted(case: &str, submitted: &Value, listed: &Value) {
    for member in [
        "ReportName",
        "TimeUnit",
        "Format",
        "Compression",
        "AdditionalSchemaElements",
        "S3Bucket",
        "S3Prefix",
        "S3Region",
    ] {
        assert_eq!(
            &listed[member], &submitted[member],
            "case {case:?}: the stored {member} must be the one submitted, got {listed:?}"
        );
    }
}

/// One row: what the destination bucket's policy says, where the report is delivered, and
/// whether the definition is accepted.
struct Row {
    case: &'static str,
    /// The policy put on this row's bucket, built from its name, or `None` for a bucket
    /// created without one.
    policy: fn(&str) -> Option<Value>,
    /// The `S3Prefix` the report is delivered under.
    prefix: &'static str,
    /// The Region the definition states its destination bucket is in.
    region: &'static str,
    /// Whether the definition is accepted.
    served: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the delivery grant AWS publishes",
            policy: |bucket| Some(account_grant(bucket)),
            prefix: PREFIX,
            region: REGION,
            served: true,
        },
        // The same grant with its clause matched as a PATTERN rather than by equality: the
        // account's report definitions are named either way.
        Row {
            case: "the published grant whose source clause is a pattern",
            policy: |bucket| {
                Some(published_grant(
                    bucket,
                    Some(source_condition(
                        "StringLike",
                        REGION,
                        engine::emulator_account_id(),
                    )),
                ))
            },
            prefix: PREFIX,
            region: REGION,
            served: true,
        },
        // No condition at all: an unconditional grant covers every delivery.
        Row {
            case: "an unconditional grant of every delivery action",
            policy: |bucket| Some(published_grant(bucket, None)),
            prefix: PREFIX,
            region: REGION,
            served: true,
        },
        // A wildcard action on the bucket and on its objects: every S3 action includes the
        // ones the delivery performs.
        Row {
            case: "a wildcard action granted to the delivery principal",
            policy: |bucket| {
                Some(document(vec![statement(
                    "Allow",
                    json!({ "Service": [DELIVERY_PRINCIPAL] }),
                    json!("s3:*"),
                    json!([bucket_arn(bucket), objects_of(bucket)]),
                    None,
                )]))
            },
            prefix: PREFIX,
            region: REGION,
            served: true,
        },
        // The object write scoped to exactly the prefix the report is delivered under.
        Row {
            case: "the write granted under the delivery prefix alone",
            policy: |bucket| {
                Some(document(vec![
                    statement(
                        "Allow",
                        json!({ "Service": [DELIVERY_PRINCIPAL] }),
                        json!(cur::state::BUCKET_DELIVERY_ACTIONS),
                        json!(bucket_arn(bucket)),
                        None,
                    ),
                    statement(
                        "Allow",
                        json!({ "Service": [DELIVERY_PRINCIPAL] }),
                        json!(cur::state::OBJECT_DELIVERY_ACTION),
                        json!(format!("arn:aws:s3:::{bucket}/{PREFIX}/*")),
                        None,
                    ),
                ]))
            },
            prefix: PREFIX,
            region: REGION,
            served: true,
        },
        // The published grant for a report delivered at the ROOT of the bucket, stated for a
        // bucket in another Region: neither is what the grant is about.
        Row {
            case: "the published grant, an empty prefix and a bucket stated elsewhere",
            policy: |bucket| Some(account_grant(bucket)),
            prefix: "",
            region: "eu-west-1",
            served: true,
        },
        // A bucket created with NO policy: the recorded refusal.
        Row {
            case: "a bucket carrying no policy at all",
            policy: |_| None,
            prefix: PREFIX,
            region: REGION,
            served: false,
        },
        // Only the object write: the delivery also reads the bucket's own ACL and policy, so
        // a destination granting it neither is not deliverable.
        Row {
            case: "the write granted but not the bucket reads",
            policy: |bucket| {
                Some(document(vec![statement(
                    "Allow",
                    json!({ "Service": [DELIVERY_PRINCIPAL] }),
                    json!(cur::state::OBJECT_DELIVERY_ACTION),
                    json!(objects_of(bucket)),
                    None,
                )]))
            },
            prefix: PREFIX,
            region: REGION,
            served: false,
        },
        // Only the bucket reads: reading a bucket is not writing a report into it.
        Row {
            case: "the bucket reads granted but not the write",
            policy: |bucket| {
                Some(document(vec![statement(
                    "Allow",
                    json!({ "Service": [DELIVERY_PRINCIPAL] }),
                    json!(cur::state::BUCKET_DELIVERY_ACTIONS),
                    json!(bucket_arn(bucket)),
                    None,
                )]))
            },
            prefix: PREFIX,
            region: REGION,
            served: false,
        },
        // Every delivery action, granted to ANOTHER service.
        Row {
            case: "the delivery granted to another service principal",
            policy: |bucket| {
                Some(document(vec![statement(
                    "Allow",
                    json!({ "Service": ["bcm-data-exports.amazonaws.com"] }),
                    json!("s3:*"),
                    json!([bucket_arn(bucket), objects_of(bucket)]),
                    None,
                )]))
            },
            prefix: PREFIX,
            region: REGION,
            served: false,
        },
        // The published grant written for ANOTHER bucket's resources.
        Row {
            case: "the delivery granted on another bucket",
            policy: |_| Some(account_grant("cur-grant-somebody-elses-bucket")),
            prefix: PREFIX,
            region: REGION,
            served: false,
        },
        // The write granted under a prefix the report is not delivered under.
        Row {
            case: "the write granted under another prefix",
            policy: |bucket| {
                Some(document(vec![
                    statement(
                        "Allow",
                        json!({ "Service": [DELIVERY_PRINCIPAL] }),
                        json!(cur::state::BUCKET_DELIVERY_ACTIONS),
                        json!(bucket_arn(bucket)),
                        None,
                    ),
                    statement(
                        "Allow",
                        json!({ "Service": [DELIVERY_PRINCIPAL] }),
                        json!(cur::state::OBJECT_DELIVERY_ACTION),
                        json!(format!("arn:aws:s3:::{bucket}/elsewhere/*")),
                        None,
                    ),
                ]))
            },
            prefix: PREFIX,
            region: REGION,
            served: false,
        },
        // The published grant conditioned on ANOTHER account.
        Row {
            case: "the grant conditioned on another account",
            policy: |bucket| {
                Some(published_grant(
                    bucket,
                    Some(source_condition("StringEquals", REGION, "111122223333")),
                ))
            },
            prefix: PREFIX,
            region: REGION,
            served: false,
        },
        // The published grant conditioned on the report definitions of another Region: the
        // control plane holds this account's definitions in one Region, and not that one.
        Row {
            case: "the grant conditioned on another Region's definitions",
            policy: |bucket| {
                Some(published_grant(
                    bucket,
                    Some(source_condition(
                        "StringEquals",
                        "ap-south-1",
                        engine::emulator_account_id(),
                    )),
                ))
            },
            prefix: PREFIX,
            region: REGION,
            served: false,
        },
        // An explicit `Deny` of the write beside the published grant: the deny wins.
        Row {
            case: "an explicit deny of the write beside the published grant",
            policy: |bucket| {
                let mut denied = account_grant(bucket);
                denied["Statement"]
                    .as_array_mut()
                    .expect("Statement is a list")
                    .push(statement(
                        "Deny",
                        json!({ "Service": [DELIVERY_PRINCIPAL] }),
                        json!(cur::state::OBJECT_DELIVERY_ACTION),
                        json!(objects_of(bucket)),
                        None,
                    ));
                Some(denied)
            },
            prefix: PREFIX,
            region: REGION,
            served: false,
        },
    ]
}

/// RULE: a report definition is accepted exactly where the destination bucket's own policy
/// grants the report delivery to the delivery principal, and a refused create persists
/// nothing.
#[tokio::test]
async fn a_definition_is_accepted_only_where_its_bucket_grants_the_delivery() {
    for (index, row) in rows().into_iter().enumerate() {
        // One emulator per row, so no row is affected by what another stored (an account
        // holds a bounded number of definitions) and the rows need no ordering.
        let emulator = Destinations::new();
        let case = row.case;
        let bucket = format!("cur-delivery-grant-{index}");
        let name = format!("cur-delivery-grant-report-{index}");
        emulator
            .bucket_with(case, &bucket, (row.policy)(&bucket).as_ref())
            .await;

        let submitted = definition(&name, &bucket, row.prefix, row.region);
        let (status, body) = emulator
            .call(
                "PutReportDefinition",
                &json!({ "ReportDefinition": submitted }),
            )
            .await;

        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: the definition must be accepted, got {status} with {body:?}"
            );
            let listed = emulator
                .described_as(case, &name)
                .await
                .unwrap_or_else(|| panic!("case {case:?}: the accepted definition must read back"));
            assert_reports_as_submitted(case, &submitted, &listed);
        } else {
            assert_eq!(
                (status, error_code(&body)),
                (400, "ValidationException"),
                "case {case:?}: expected a 400 ValidationException, got {status} with {body:?}"
            );
            assert_eq!(
                message(&body),
                verification_failed(&bucket, row.region),
                "case {case:?}: expected the bucket-permission refusal, got {body:?}"
            );
            assert_eq!(
                emulator.described_as(case, &name).await,
                None,
                "case {case:?}: a refused create must leave no definition behind"
            );
        }
    }
}

/// RULE: the refusal states the account the verification ran for and the bucket and Region
/// the REQUEST named, so two refusals on the same call path are told apart by it.
#[tokio::test]
async fn the_refusal_names_the_bucket_and_region_the_request_stated() {
    let emulator = Destinations::new();
    // The rows differ in the two variable parts of the sentence: the bucket named, and the
    // Region the definition states for it.
    let cases: [(&str, &str); 3] = [
        ("cur-refusal-names-first", "us-east-1"),
        ("cur-refusal-names-second", "us-west-2"),
        ("cur-refusal-names-third", "ap-northeast-1"),
    ];
    for (bucket, region) in cases {
        emulator.bucket_with(bucket, bucket, None).await;
        let (status, body) = emulator
            .call(
                "PutReportDefinition",
                &json!({
                    "ReportDefinition": definition("cur-refusal-names", bucket, PREFIX, region),
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body)),
            (400, "ValidationException"),
            "case {bucket:?}: expected a 400 ValidationException, got {status} with {body:?}"
        );
        assert_eq!(
            message(&body),
            verification_failed(bucket, region),
            "case {bucket:?}: the refusal must name this bucket and Region, got {body:?}"
        );
    }
}

/// RULE: the precondition is judged per call, so the byte-identical request refused against
/// an unpolicied bucket is served once that bucket carries the grant — and nothing exists
/// under the requested name until it does.
#[tokio::test]
async fn the_same_request_is_served_once_the_bucket_carries_the_grant() {
    let case = "attach the grant, then repeat the identical request";
    let emulator = Destinations::new();
    let bucket = "cur-grant-attached-later";
    let name = "cur-grant-attached-later-report";
    let submitted = definition(name, bucket, PREFIX, REGION);
    let request = json!({ "ReportDefinition": submitted });
    emulator.bucket_with(case, bucket, None).await;

    let (refused, refusal) = emulator.call("PutReportDefinition", &request).await;
    assert_eq!(
        (refused, error_code(&refusal)),
        (400, "ValidationException"),
        "case {case:?}: the first call must be refused, got {refused} with {refusal:?}"
    );
    assert_eq!(
        emulator.described_as(case, name).await,
        None,
        "case {case:?}: no definition may exist before the grant does"
    );

    emulator
        .put_bucket_policy(case, bucket, &account_grant(bucket))
        .await;

    let (served, body) = emulator.call("PutReportDefinition", &request).await;
    assert_eq!(
        served, 200,
        "case {case:?}: the identical call must be served once the grant is in place, got \
         {served} with {body:?}"
    );
    let listed = emulator
        .described_as(case, name)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the accepted definition must read back"));
    assert_reports_as_submitted(case, &submitted, &listed);
}

/// RULE: the update path judges the same precondition the create does, so a definition
/// moved onto a bucket that does not grant the delivery is refused and the stored definition
/// is left exactly as it was.
#[tokio::test]
async fn an_update_onto_an_ungranted_bucket_is_refused_and_changes_nothing() {
    let case = "move a stored definition onto another bucket";
    let emulator = Destinations::new();
    let granted = "cur-update-granted";
    let ungranted = "cur-update-ungranted";
    let elsewhere = "cur-update-granted-elsewhere";
    let name = "cur-update-report";

    emulator
        .bucket_with(case, granted, Some(&account_grant(granted)))
        .await;
    emulator.bucket_with(case, ungranted, None).await;
    emulator
        .bucket_with(case, elsewhere, Some(&account_grant(elsewhere)))
        .await;

    let stored = definition(name, granted, PREFIX, REGION);
    let (created, body) = emulator
        .call("PutReportDefinition", &json!({ "ReportDefinition": stored }))
        .await;
    assert_eq!(
        created, 200,
        "case {case:?}: the definition must be created, got {created} with {body:?}"
    );

    let onto_ungranted = definition(name, ungranted, PREFIX, REGION);
    let (status, refusal) = emulator
        .call(
            "ModifyReportDefinition",
            &json!({ "ReportName": name, "ReportDefinition": onto_ungranted }),
        )
        .await;
    assert_eq!(
        (status, error_code(&refusal)),
        (400, "ValidationException"),
        "case {case:?}: the update must be refused, got {status} with {refusal:?}"
    );
    assert_eq!(
        message(&refusal),
        verification_failed(ungranted, REGION),
        "case {case:?}: the refusal must state the verification that failed, got {refusal:?}"
    );
    let unchanged = emulator
        .described_as(case, name)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the stored definition must survive"));
    assert_reports_as_submitted(case, &stored, &unchanged);

    // The same update onto a bucket that DOES carry the grant is applied, which is what
    // tells this refusal from a service that refuses every update.
    let onto_granted = definition(name, elsewhere, PREFIX, REGION);
    let (applied, body) = emulator
        .call(
            "ModifyReportDefinition",
            &json!({ "ReportName": name, "ReportDefinition": onto_granted }),
        )
        .await;
    assert_eq!(
        applied, 200,
        "case {case:?}: the update onto a granted bucket must be served, got {applied} with \
         {body:?}"
    );
    let moved = emulator
        .described_as(case, name)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the updated definition must read back"));
    assert_reports_as_submitted(case, &onto_granted, &moved);
}
