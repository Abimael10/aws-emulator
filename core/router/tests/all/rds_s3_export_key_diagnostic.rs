// SPDX-License-Identifier: CC0-1.0

//! What an accepted export leaves behind for a caller to diagnose its KEY use: the grant the
//! export runs under on that key, and the encryption its delivered objects are recorded with.
//!
//! RULE: an export is performed WITH the KMS key the request named — "the ID of the Amazon
//! Web Services KMS key to use to encrypt the data exported to Amazon S3" (`KmsKeyId`) — so
//! two things exist afterwards, both naming that key by its own ARN whichever of the four
//! spellings the request selected it with: the `kms:CreateGrant` the operation documents as
//! required is really performed, so the key holds a grant for the service that performs the
//! export, and every object the export delivered is stored under SSE-KMS with that key, which
//! is what a later read of one reports.
//!
//! Only a cross-service suite can pin this: the key lives in AWS KMS, the objects in Amazon
//! S3 and the export in Amazon RDS, over ONE engine, exactly as `core/router/src/lib.rs`
//! mounts them. The harness is the one the key-authorization suite beside this file drives
//! the router with.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::Value;

use super::query_wire::{element, elements};
use super::rds_s3_export_key_policy_authorization::{
    create_key, dispatch, export_fixtures, kms, params, poll_until, rds, start_export,
    BOOT_STACK_SIZE, EXPORT_KEY_ACTIONS, EXPORT_PRINCIPAL, REGION,
};

/// The `ServerSideEncryption` value an object encrypted with a KMS key carries.
const KMS_SSE_ALGORITHM: &str = "aws:kms";

/// The response header `ServerSideEncryption` is bound to.
const SSE_HEADER: &str = "x-amz-server-side-encryption";

/// The response header `SSEKMSKeyId` is bound to.
const SSE_KEY_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

/// What an accepted export leaves on its key and on its objects, for each spelling the
/// request may name that key by.
#[test]
fn an_accepted_export_holds_a_grant_and_seals_its_objects_with_the_key() {
    std::thread::Builder::new()
        .name("rds-s3-export-key-delivery".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            let (bucket, role_arn, snapshot_arn) =
                export_fixtures(&router, &runtime, "rds-export-key-delivery");
            let root = format!(
                "arn:aws:iam::{}:root",
                role_arn
                    .split(':')
                    .nth(4)
                    .expect("the role ARN carries an account")
            );
            // One key the export role is granted the two export operations on, beside the
            // account's own access the default key policy carries — the document the user
            // guide's own example for an export key is — named below by each spelling
            // `KmsKeyId` publishes: one key, so what the rows compare is the SPELLING and not
            // the key.
            let (key_id, key_arn) = create_key(
                &router,
                &runtime,
                "the key the export runs under",
                Some(
                    serde_json::json!({
                        "Version": "2012-10-17",
                        "Statement": [
                            {
                                "Sid": "Enable IAM User Permissions",
                                "Effect": "Allow",
                                "Principal": {"AWS": root},
                                "Action": "kms:*",
                                "Resource": "*",
                            },
                            {
                                "Sid": "Allow the export role to use the key",
                                "Effect": "Allow",
                                "Principal": {"AWS": role_arn.clone()},
                                "Action": EXPORT_KEY_ACTIONS,
                                "Resource": "*",
                            },
                        ],
                    })
                    .to_string(),
                ),
            );
            kms(
                &router,
                &runtime,
                "CreateAlias",
                serde_json::json!({
                    "AliasName": "alias/rds-export-key-delivery",
                    "TargetKeyId": key_id.clone(),
                }),
            )
            .served("alias the export's key");

            let rows: [(&str, &str, &str); 3] = [
                ("the key named by its id", "delivery-by-id", key_id.as_str()),
                (
                    "the key named by its ARN",
                    "delivery-by-arn",
                    key_arn.as_str(),
                ),
                (
                    "the key named by an alias",
                    "delivery-by-alias",
                    "alias/rds-export-key-delivery",
                ),
            ];
            for (case, task, reference) in rows {
                start_export(
                    &router,
                    &runtime,
                    task,
                    &snapshot_arn,
                    &bucket,
                    &role_arn,
                    reference,
                )
                .served(case);
                poll_until(case, || {
                    let described = rds(
                        &router,
                        &runtime,
                        "DescribeExportTasks",
                        &params!["ExportTaskIdentifier" => task],
                    )
                    .served("describe the export task");
                    (element(&described, "Status") == "COMPLETE").then_some(described)
                });

                // The GRANT the export runs under: KMS holds one on the key — named by the
                // key's own ARN, which is what a grant is taken on whichever spelling the
                // export was asked with — for the service that performs the export and
                // constrained to the source it was performed for.
                let listed = kms(
                    &router,
                    &runtime,
                    "ListGrants",
                    serde_json::json!({"KeyId": key_arn.clone()}),
                )
                .served("list the key's grants");
                let listed: Value = serde_json::from_str(&listed).expect("KMS answers JSON");
                let grants = listed["Grants"]
                    .as_array()
                    .unwrap_or_else(|| panic!("case {case:?}: KMS reports a grant list: {listed}"));
                let held = grants.iter().find(|grant| {
                    grant["GranteePrincipal"].as_str() == Some(EXPORT_PRINCIPAL)
                        && grant["Constraints"]["SourceArn"].as_str()
                            == Some(snapshot_arn.as_str())
                });
                let held = held.unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: the key holds the export's grant for \
                         {EXPORT_PRINCIPAL} on {snapshot_arn}, got {listed}"
                    )
                });
                assert_eq!(
                    held["KeyId"].as_str(),
                    Some(key_arn.as_str()),
                    "case {case:?}: the grant names the key by its own ARN, got {held}"
                );
                let operations: Vec<&str> = held["Operations"]
                    .as_array()
                    .map(|values| values.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                for operation in ["GenerateDataKey", "Encrypt", "Decrypt", "DescribeKey"] {
                    assert!(
                        operations.contains(&operation),
                        "case {case:?}: the export's grant permits {operation}, got \
                         {operations:?}"
                    );
                }

                // And the OBJECTS: each one the export delivered is recorded under SSE-KMS
                // with that same key, so a read of one reports the algorithm and the key.
                let objects = dispatch(
                    &router,
                    &runtime,
                    "s3",
                    "GET",
                    &format!("/{bucket}?list-type=2&prefix={task}"),
                    &[],
                    String::new(),
                )
                .served("list the export's objects");
                let keys = elements(&objects, "Key");
                assert!(
                    !keys.is_empty(),
                    "case {case:?}: the export delivered objects under its prefix, got \
                     {objects}"
                );
                for object in &keys {
                    let read = head_object(&router, &runtime, &bucket, object);
                    assert_eq!(
                        read.status, 200,
                        "case {case:?}: {object} must be readable, got HTTP {}",
                        read.status
                    );
                    assert_eq!(
                        read.header(SSE_HEADER).as_deref(),
                        Some(KMS_SSE_ALGORITHM),
                        "case {case:?}: {object} is stored under SSE-KMS, got {:?}",
                        read.header(SSE_HEADER)
                    );
                    assert_eq!(
                        read.header(SSE_KEY_HEADER).as_deref(),
                        Some(key_arn.as_str()),
                        "case {case:?}: {object} is sealed with the export's key, got {:?}",
                        read.header(SSE_KEY_HEADER)
                    );
                }
            }
        })
        .expect("the sized thread starts")
        .join()
        .expect("the suite runs to its assertions");
}

/// One `HeadObject`, whose answer is entirely in its HEADERS — which is where the encryption
/// members of a read are bound, so this is the one call of this suite that reads them.
fn head_object(
    router: &smithy_server_runtime::MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    bucket: &str,
    key: &str,
) -> Read {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("HEAD")
        .uri(format!("https://{host}/{bucket}/{key}"))
        .header("host", host)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/s3/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let (parts, _) = response.into_parts();
    Read {
        status: parts.status.as_u16(),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    String::from_utf8_lossy(value.as_bytes()).to_string(),
                )
            })
            .collect(),
    }
}

/// What one `HeadObject` answered: its status and the headers the read's members are bound to.
struct Read {
    status: u16,
    headers: Vec<(String, String)>,
}

impl Read {
    /// The value of the response header `name`, `None` where the answer carries none.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.clone())
    }
}
