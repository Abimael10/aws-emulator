// SPDX-License-Identifier: CC0-1.0

//! What DataSync proves about the BUCKET before it stores an Amazon S3 transfer location.
//!
//! RULE: a location is a transfer endpoint on a bucket, so `CreateLocationS3` resolves the
//! bucket its `S3BucketArn` names in the object store and refuses the request with
//! `InvalidRequestException` (400) — storing nothing — when the object store answers that
//! it holds no such bucket. An answer the object store did not give establishes nothing: an
//! `S3BucketArn` that addresses an access point rather than a bucket is not refused on
//! these grounds.
//!
//! RULE: and a refusal that reports what the probe ANSWERED carries that answer as its
//! class, in both `errorCode` and `datasyncErrorCode` — `S3NoSuchBucket` for a bucket the
//! object store does not hold, `S3AccessDenied` for a listing it will not serve the bucket
//! access role, and the assume-role gate ahead of both reports its own
//! `StsAssumeRoleInvalidRequest`.
//!
//! Every case goes through the same front door a client uses — `POST /` with
//! `X-Amz-Target: FmrsService.<Operation>` and an `awsJson1_1` body — against a router that
//! mounts DataSync beside this emulator's own Amazon S3 and IAM on ONE engine, so the
//! buckets the pre-flight resolves are real buckets of the object store, created and
//! removed through its own API.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::{EmbeddedEmulator, MultiServiceRouter};

/// The Region every request in this suite is signed for, and the one its SDK clients
/// address.
const REGION: &str = "us-west-2";

/// The one bucket access role these cases name.
const ROLE_NAME: &str = "datasync-bucket-access";

/// A bucket the object store holds throughout.
const HELD_BUCKET: &str = "preflight-held";

/// A second one, so the rule is not keyed to a single name.
const SECOND_HELD_BUCKET: &str = "preflight-second";

/// A bucket that is created and then removed again.
const DELETED_BUCKET: &str = "preflight-deleted";

/// A bucket name nothing is ever created under.
const ABSENT_BUCKET: &str = "preflight-absent";

/// A bucket the object store holds that the bucket access role may NOT list, so the
/// listing half of the access test is what refuses it.
const UNGRANTED_BUCKET: &str = "preflight-ungranted";

/// A second one, so the listing half is not keyed to a single name either.
const SECOND_UNGRANTED_BUCKET: &str = "preflight-ungranted-two";

/// The service principal a bucket access role has to trust, as a trust policy spells it.
const SERVICE_PRINCIPAL: &str = "datasync.amazonaws.com";

/// The role name the cases that need a role DataSync cannot assume name.
const UNTRUSTED_ROLE_NAME: &str = "datasync-untrusted-access";

/// The access point an `S3BucketArn` may address instead of a bucket.
const ACCESS_POINT: &str = "preflight-access-point";

/// DataSync, Amazon S3 and IAM on one shared engine.
struct Aws {
    router: MultiServiceRouter,
}

impl Aws {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_datasync_fmrsservice::EmulatedService::new(Arc::clone(&engine)),
        );
        // The bucket a location names is a bucket of THIS object store.
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        // Whether the bucket access role can be assumed, and what it allows, is IAM's.
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router }
    }

    fn callback(&self) -> EmbeddedEmulator {
        EmbeddedEmulator::from_router(self.router.clone())
    }

    fn s3(&self) -> ::aws_sdk_s3::Client {
        ::service_common::s3_client::s3_client(&self.callback(), REGION)
    }

    fn iam(&self) -> ::aws_sdk_iam::Client {
        ::service_common::iam_client::iam_client_through(&self.callback(), REGION)
    }

    /// One `awsJson1_1` DataSync call, returning the HTTP status and parsed body.
    async fn call(&self, operation: &str, body: Value) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("FmrsService.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDDATASYNCTEST/20260101/{REGION}\
                     /datasync/aws4_request, SignedHeaders=host;x-amz-target, Signature=00"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = response.into_body();
        let parsed = if payload.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&payload)
                .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
        };
        (status, parsed)
    }

    /// A DataSync call that must succeed.
    async fn ok(&self, operation: &str, body: Value, case: &str) -> Value {
        let (status, parsed) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected {operation} to answer 200, got {status} with {parsed}"
        );
        parsed
    }

    /// Create `bucket` in the object store.
    async fn create_bucket(&self, bucket: &str, case: &str) {
        let created = self
            .s3()
            .create_bucket()
            .bucket(bucket)
            .create_bucket_configuration(
                ::aws_sdk_s3::types::CreateBucketConfiguration::builder()
                    .location_constraint(::aws_sdk_s3::types::BucketLocationConstraint::from(
                        REGION,
                    ))
                    .build(),
            )
            .send()
            .await;
        assert!(
            created.is_ok(),
            "case {case:?}: Amazon S3 must create the bucket {bucket}, got {created:?}"
        );
    }

    /// Remove `bucket` from the object store again.
    async fn delete_bucket(&self, bucket: &str, case: &str) {
        let deleted = self.s3().delete_bucket().bucket(bucket).send().await;
        assert!(
            deleted.is_ok(),
            "case {case:?}: Amazon S3 must remove the bucket {bucket}, got {deleted:?}"
        );
    }

    /// Create the bucket access role this suite transfers under: a trust policy admitting
    /// DataSync, and an inline policy that allows the bucket listing over EVERY value the
    /// rows name — so the only thing any row can be refused for is the bucket itself.
    async fn create_role(&self, resources: &[String]) -> String {
        self.create_role_trusting(ROLE_NAME, SERVICE_PRINCIPAL, resources)
            .await
    }

    /// [`Self::create_role`] for a role held under `name` whose trust policy admits
    /// `trusted_service` and nothing else: a role naming another service is one DataSync
    /// cannot take on at all, whatever its inline policy then allows.
    async fn create_role_trusting(
        &self,
        name: &str,
        trusted_service: &str,
        resources: &[String],
    ) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": trusted_service },
                "Action": "sts:AssumeRole",
            }],
        });
        let created = self
            .iam()
            .create_role()
            .role_name(name)
            .assume_role_policy_document(trust.to_string())
            .send()
            .await
            .unwrap_or_else(|error| panic!("IAM creates the role {name}: {error:?}"));
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": ["s3:ListBucket", "s3:GetObject", "s3:PutObject"],
                "Resource": resources,
            }],
        });
        self.iam()
            .put_role_policy()
            .role_name(name)
            .policy_name("bucket-access")
            .policy_document(document.to_string())
            .send()
            .await
            .unwrap_or_else(|error| panic!("IAM stores the inline policy of {name}: {error:?}"));
        created
            .role()
            .expect("the created role is reported")
            .arn()
            .to_string()
    }

    /// The location ARNs this service now holds, in creation order.
    async fn held_locations(&self, case: &str) -> Vec<String> {
        let listed = self.ok("ListLocations", json!({}), case).await;
        listed["Locations"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry["LocationArn"].as_str().map(ToString::to_string))
            .collect()
    }
}

/// The ARN of a plain Amazon S3 bucket.
fn bucket_arn(bucket: &str) -> String {
    format!("arn:aws:s3:::{bucket}")
}

/// The ARN of an Amazon S3 access point of this account, the other form `S3BucketArn`
/// admits (`arn:<partition>:s3:<region>:<account>:accesspoint/<name>`).
fn access_point_arn(name: &str) -> String {
    format!(
        "arn:aws:s3:{REGION}:{account}:accesspoint/{name}",
        account = engine::emulator_account_id()
    )
}

/// RULE: `CreateLocationS3` stores a location only for a bucket the object store reports
/// holding; a bucket it answers is not there is refused with
/// `InvalidRequestException` (400) naming that bucket, and nothing is stored. An
/// `S3BucketArn` that addresses no bucket at all is not refused on these grounds.
///
/// The rows differ along what the rule generalises over: a bucket that is held, a second
/// one so the rule is not keyed to one name, a name nothing was ever created under, a
/// bucket that was created and then removed, and an access point, which is the other form
/// the member admits.
#[tokio::test]
async fn create_location_s3_resolves_the_bucket_in_the_object_store() {
    let aws = Aws::new();
    for bucket in [HELD_BUCKET, SECOND_HELD_BUCKET, DELETED_BUCKET] {
        aws.create_bucket(bucket, "setup").await;
    }
    let role_arn = aws
        .create_role(&[
            bucket_arn(HELD_BUCKET),
            bucket_arn(SECOND_HELD_BUCKET),
            bucket_arn(DELETED_BUCKET),
            bucket_arn(ABSENT_BUCKET),
            access_point_arn(ACCESS_POINT),
        ])
        .await;
    aws.delete_bucket(DELETED_BUCKET, "setup").await;

    // (case, the S3BucketArn the request carries, the bucket a refusal must name)
    let rows = [
        (
            "a bucket the object store holds",
            bucket_arn(HELD_BUCKET),
            None,
        ),
        (
            "a second bucket the object store holds",
            bucket_arn(SECOND_HELD_BUCKET),
            None,
        ),
        (
            "a bucket name nothing was created under",
            bucket_arn(ABSENT_BUCKET),
            Some(ABSENT_BUCKET),
        ),
        (
            "a bucket that was created and then removed",
            bucket_arn(DELETED_BUCKET),
            Some(DELETED_BUCKET),
        ),
        (
            // An access point is a resource of its own: the object store is not asked
            // about a bucket here, so nothing is established about one.
            "an access point, which addresses no bucket",
            access_point_arn(ACCESS_POINT),
            None,
        ),
    ];

    let mut accepted: Vec<String> = Vec::new();
    for (case, s3_bucket_arn, absent_bucket) in rows {
        let request = json!({
            "S3BucketArn": s3_bucket_arn,
            "S3Config": { "BucketAccessRoleArn": role_arn },
        });
        match absent_bucket {
            None => {
                let created = aws.ok("CreateLocationS3", request, case).await;
                let location_arn = created["LocationArn"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected a LocationArn, got {created}")
                    })
                    .to_string();
                accepted.push(location_arn);
            }
            Some(bucket) => {
                let (status, parsed) = aws.call("CreateLocationS3", request).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected CreateLocationS3 to answer 400, got {status} \
                     with {parsed}"
                );
                let wire_type = parsed["__type"].as_str().unwrap_or_default();
                assert!(
                    wire_type.contains("InvalidRequestException"),
                    "case {case:?}: expected an InvalidRequestException, got {parsed}"
                );
                assert_eq!(
                    parsed["message"].as_str(),
                    Some(
                        format!(
                            "Datasync bucket access test failed: no such bucket {bucket} exists."
                        )
                        .as_str()
                    ),
                    "case {case:?}: the refusal must name the bucket it could not find, \
                     got {parsed}"
                );
            }
        }
    }

    // A refused create stored nothing: the listing holds exactly the accepted locations.
    let held = aws.held_locations("listing").await;
    assert_eq!(
        held, accepted,
        "case \"listing\": expected only the accepted locations, got {held:?}"
    );
}

/// What a refusal of the bucket access test is CLASSIFIED as.
///
/// RULE: a refusal that reports what another service answered a pre-flight probe carries
/// that answer as its class, in BOTH code members of the one modelled client error and in
/// the `<namespace><answer>` spelling this service names such a failure by — so the object
/// store reporting no such bucket is `S3NoSuchBucket`, a listing it will not serve the
/// bucket access role is `S3AccessDenied`, and the assume-role gate ahead of both reports
/// the STS call it could not make, `StsAssumeRoleInvalidRequest`. The gate runs first, so a
/// role DataSync cannot take on is reported as such whatever the bucket then turns out to
/// be.
///
/// The rows differ along what the rule generalises over: which probe answered, which
/// bucket it was asked about (absent, removed, held-but-unlistable, and a second of each
/// kind so no row is keyed to one name), and whether the role the request names is one
/// DataSync can assume at all.
#[tokio::test]
async fn create_location_s3_classifies_what_the_bucket_access_test_found() {
    let aws = Aws::new();
    for bucket in [
        HELD_BUCKET,
        DELETED_BUCKET,
        UNGRANTED_BUCKET,
        SECOND_UNGRANTED_BUCKET,
    ] {
        aws.create_bucket(bucket, "setup").await;
    }
    // Listable: the held bucket only — so the two ungranted buckets are refused by the
    // listing half of the test and nothing else.
    let role_arn = aws.create_role(&[bucket_arn(HELD_BUCKET)]).await;
    // A role whose trust policy admits another service: DataSync cannot assume it.
    let untrusted_role_arn = aws
        .create_role_trusting(
            UNTRUSTED_ROLE_NAME,
            "ec2.amazonaws.com",
            &[
                bucket_arn(HELD_BUCKET),
                bucket_arn(UNGRANTED_BUCKET),
                bucket_arn(ABSENT_BUCKET),
            ],
        )
        .await;
    aws.delete_bucket(DELETED_BUCKET, "setup").await;

    /// What the service must answer one request.
    enum Expected {
        /// The request is accepted and a location is stored.
        Accepted,
        /// The request is refused, under this class discriminator and this message.
        Refused {
            code: &'static str,
            message: String,
        },
    }

    let rows = [
        (
            "a held bucket the role may list",
            bucket_arn(HELD_BUCKET),
            &role_arn,
            Expected::Accepted,
        ),
        (
            "a bucket name nothing was created under",
            bucket_arn(ABSENT_BUCKET),
            &role_arn,
            Expected::Refused {
                code: "S3NoSuchBucket",
                message: absent_bucket_message(ABSENT_BUCKET),
            },
        ),
        (
            "a bucket that was created and then removed",
            bucket_arn(DELETED_BUCKET),
            &role_arn,
            Expected::Refused {
                code: "S3NoSuchBucket",
                message: absent_bucket_message(DELETED_BUCKET),
            },
        ),
        (
            "a held bucket the role's policy does not cover",
            bucket_arn(UNGRANTED_BUCKET),
            &role_arn,
            Expected::Refused {
                code: "S3AccessDenied",
                message: listing_denied_message(UNGRANTED_BUCKET),
            },
        ),
        (
            "a second held bucket the role's policy does not cover",
            bucket_arn(SECOND_UNGRANTED_BUCKET),
            &role_arn,
            Expected::Refused {
                code: "S3AccessDenied",
                message: listing_denied_message(SECOND_UNGRANTED_BUCKET),
            },
        ),
        (
            // The gate ahead of the bucket access test: a role that cannot be taken on
            // says nothing about what it could read, so the bucket is never probed.
            "an absent bucket named with a role DataSync cannot assume",
            bucket_arn(ABSENT_BUCKET),
            &untrusted_role_arn,
            Expected::Refused {
                code: "StsAssumeRoleInvalidRequest",
                message: unassumable_role_message(&untrusted_role_arn),
            },
        ),
        (
            "an unlistable bucket named with a role DataSync cannot assume",
            bucket_arn(UNGRANTED_BUCKET),
            &untrusted_role_arn,
            Expected::Refused {
                code: "StsAssumeRoleInvalidRequest",
                message: unassumable_role_message(&untrusted_role_arn),
            },
        ),
    ];

    let mut accepted: Vec<String> = Vec::new();
    for (case, s3_bucket_arn, role, expected) in rows {
        let request = json!({
            "S3BucketArn": s3_bucket_arn,
            "S3Config": { "BucketAccessRoleArn": role },
        });
        match expected {
            Expected::Accepted => {
                let created = aws.ok("CreateLocationS3", request, case).await;
                let location_arn = created["LocationArn"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected a LocationArn, got {created}")
                    })
                    .to_string();
                accepted.push(location_arn);
            }
            Expected::Refused { code, message } => {
                let (status, parsed) = aws.call("CreateLocationS3", request).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected CreateLocationS3 to answer 400, got {status} \
                     with {parsed}"
                );
                let wire_type = parsed["__type"].as_str().unwrap_or_default();
                assert!(
                    wire_type.contains("InvalidRequestException"),
                    "case {case:?}: expected an InvalidRequestException, got {parsed}"
                );
                for member in ["errorCode", "datasyncErrorCode"] {
                    assert_eq!(
                        parsed[member].as_str(),
                        Some(code),
                        "case {case:?}: expected {member} to carry {code:?}, got {parsed}"
                    );
                }
                assert_eq!(
                    parsed["message"].as_str(),
                    Some(message.as_str()),
                    "case {case:?}: wrong refusal wording, got {parsed}"
                );
            }
        }
    }

    // Every refused create stored nothing.
    let held = aws.held_locations("listing").await;
    assert_eq!(
        held, accepted,
        "case \"listing\": expected only the accepted locations, got {held:?}"
    );
}

/// How the bucket access test words a bucket the object store reports is not there.
fn absent_bucket_message(bucket: &str) -> String {
    format!("Datasync bucket access test failed: no such bucket {bucket} exists.")
}

/// How it words a bucket it could not list under the role the request named.
fn listing_denied_message(bucket: &str) -> String {
    format!(
        "DataSync location access test failed: could not perform s3:ListObjectsV2 on bucket \
         {bucket}. Access denied. Ensure bucket access role has s3:ListBucket permission."
    )
}

/// How the gate ahead of the bucket access test words a role DataSync cannot take on.
fn unassumable_role_message(role_arn: &str) -> String {
    format!("Invalid IAM role, {role_arn}. DataSync service was unable to perform sts:AssumeRole.")
}
