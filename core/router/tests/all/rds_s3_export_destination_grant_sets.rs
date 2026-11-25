// SPDX-License-Identifier: CC0-1.0

//! Which POLICY SET an export's destination grant is read from, which SPELLING of the
//! encryption key it is read for, and WHEN the destination's own condition is reported.
//!
//! RULE: Amazon RDS decides whether an export's role may make the calls the export makes on
//! its destination from the role's EFFECTIVE identity policies — every document IAM holds
//! for it, inline or attached — and from the destination's own policy, and it makes those
//! calls IN ORDER: the bucket-location read first, the object write after it. That first
//! call's ANSWER is also the only thing that tells the export whether the destination is a
//! bucket it can write at all, and an object store discloses neither a bucket's location nor
//! its existence to a caller it refuses that read. So a role granted the export's calls on
//! ANOTHER bucket is refused on the bucket the request names — `IamRoleMissingPermissions`
//! naming `s3:GetBucketLocation` — whether or not the Region holds a bucket of that name;
//! the destination's own `InvalidS3BucketFault` is the answer to a role that MAY read it (and
//! to a value that is no bucket name at all, which names no destination and is decided from
//! the request alone); and the same role is accepted on the bucket it IS granted on,
//! whichever of the spellings `KmsKeyId` publishes ("the key ARN, key ID, alias ARN, or alias
//! name") the request named the key by and whether or not it carried an `S3Prefix`.
//!
//! Only a cross-service suite can pin this: one export decision spans four services over ONE
//! engine — Amazon RDS for the control plane and the export, IAM for the role, its trust
//! document and the two kinds of identity policy it can carry, AWS KMS for the key and its
//! aliases, and Amazon S3 for the destinations — exactly as `core/router/src/lib.rs` mounts
//! them. What this suite adds beside `rds_s3_export_authorization_refusals.rs`, which pins
//! one refusal per defect over an INLINE policy and one key spelling, is the POLICY SET and
//! the KEY SPELLING as the axes: the same decision has to come out the same way for a role
//! whose grant lives in a managed policy attached to it as for one carrying it inline, and
//! for a key named by id, by ARN or by alias.
//!
//! Every call goes over the wire, on a router with request authentication off as every suite
//! in this crate runs it. Assembling the whole router costs stack (see `boot_serve.rs`), so
//! it is built — and the requests driven — on a sized thread.

use std::sync::Arc;
use std::time::Duration;

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{element, elements, form_encode, opt_element};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for.
const REGION: &str = "us-west-2";

/// The destination the roles below are granted the export's calls on.
const GRANTED_BUCKET: &str = "rds-export-grant-sets-granted";

/// A destination that EXISTS and that no role below is granted anything on.
const OTHER_BUCKET: &str = "rds-export-grant-sets-elsewhere";

/// A well-formed bucket name this Region's object store holds nothing under.
const ABSENT_BUCKET: &str = "rds-export-grant-sets-absent";

/// A value that is no general-purpose bucket name at all (uppercase letters, an underscore
/// and two adjacent periods), so Amazon S3 holds no bucket under it in any account.
const MALFORMED_BUCKET: &str = "Rds_Export..Sets";

/// The DB instance whose snapshot every export below reads.
const INSTANCE: &str = "rds-export-grant-sets-instance";

/// The snapshot of [`INSTANCE`] that is the exports' `SourceArn`.
const SNAPSHOT: &str = "rds-export-grant-sets-snapshot";

/// The alias one row names the export's key by: "the alias name for the KMS key"
/// (`StartExportTask$KmsKeyId`).
const KEY_ALIAS: &str = "alias/rds-export-grant-sets";

/// A trust policy that admits the export principal — the service that performs a snapshot
/// export ("Provide access to an Amazon S3 bucket using an IAM role", Exporting DB snapshot
/// data to Amazon S3).
const EXPORT_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"export.rds.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// How many times a wait looks again before it gives up. The transitions this control plane
/// owes are due on a sub-second clock, so this leaves generous room on a loaded machine while
/// never waiting on something that is not coming.
const POLL_LOOKS: usize = 300;

/// How long one look waits before the next. Inside the poll loop only: nothing here sleeps
/// for a state instead of looking for it.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// What one call answered.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// Assert the call was served, and hand back its body.
    fn served(self, case: &str) -> String {
        assert_eq!(
            self.status, 200,
            "case {case:?}: the call must be served, got HTTP {} with {}",
            self.status, self.body
        );
        self.body
    }
}

/// One call of `method` to the service `signing_name` at `path`, with `body`.
fn dispatch(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: String,
) -> Answer {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header(
            // The signature is never checked: this router runs with authentication off.
            // What the scope carries is the Region and the service a handler reads.
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        );
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body: String::from_utf8_lossy(&body).to_string(),
    }
}

/// One awsQuery call to the service `signing_name`, as an SDK client sends one.
fn query(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> Answer {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        runtime,
        signing_name,
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body,
    )
}

/// One awsQuery call to Amazon RDS.
fn rds(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(&str, String)],
) -> Answer {
    query(router, runtime, "rds", "2014-10-31", action, params)
}

/// One awsQuery call to IAM.
fn iam(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(&str, String)],
) -> Answer {
    query(router, runtime, "iam", "2010-05-08", action, params)
}

/// One JSON call to AWS KMS.
fn kms(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    target: &str,
    body: Value,
) -> Answer {
    dispatch(
        router,
        runtime,
        "kms",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &format!("TrentService.{target}")),
        ],
        body.to_string(),
    )
}

/// `[(name, value.to_string()), ..]` — the parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The `Message` of an awsQuery error answer.
fn message_of(body: &str) -> String {
    elements(body, "Message")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no <Message> element in {body}"))
}

/// Look again until `probe` answers, so a wait is for the CONDITION rather than for a span.
fn poll_until(case: &str, mut probe: impl FnMut() -> Option<String>) -> String {
    for _ in 0..POLL_LOOKS {
        if let Some(answer) = probe() {
            return answer;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    panic!("case {case:?}: the state never arrived over {POLL_LOOKS} looks");
}

/// An identity policy granting the five calls the user guide's export role carries
/// ("s3:PutObject* s3:GetObject* s3:ListBucket s3:DeleteObject* s3:GetBucketLocation",
/// `StartExportTask$IamRoleArn`) on `bucket` and on the objects in it.
fn export_policy(bucket: &str) -> String {
    serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "s3:PutObject*",
                "s3:GetObject*",
                "s3:ListBucket",
                "s3:DeleteObject*",
                "s3:GetBucketLocation",
            ],
            "Resource": [
                format!("arn:aws:s3:::{bucket}"),
                format!("arn:aws:s3:::{bucket}/*"),
            ],
        }],
    })
    .to_string()
}

/// What one export of the table below is expected to be answered with.
enum Outcome {
    /// The export is refused before anything is written, for the call `s3:GetBucketLocation`
    /// — the first one it makes on the destination.
    RefusedOnBucketLocation,
    /// The export is refused for the DESTINATION itself: a value no bucket is named by, or a
    /// name this Region holds no bucket under, reported to a role that may read it.
    RefusedOnDestination,
    /// The export is accepted, and the task it started is reported on its way.
    Accepted,
}

/// One export this suite starts.
struct Row<'a> {
    /// What the row is about, quoted by every assertion of it.
    case: &'a str,
    /// The export task identifier, which is also the task's own folder in the bucket.
    task: &'a str,
    /// The `IamRoleArn` the row passes.
    role_arn: &'a str,
    /// The `S3BucketName` the row passes.
    bucket: &'a str,
    /// The `KmsKeyId` the row passes — one of the spellings that name the suite's one key.
    key: &'a str,
    /// The `S3Prefix` the row passes, where it passes one.
    prefix: Option<&'a str>,
    /// What it must be answered with.
    outcome: Outcome,
}

/// One export's destination grant, read for every policy set the role carries and every
/// spelling of the key.
#[test]
fn an_exports_destination_grant_is_read_from_every_policy_set_the_role_carries() {
    std::thread::Builder::new()
        .name("rds-s3-export-grant-sets".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            // `enable_all`, which is what the emulator's own boot path builds: a DB instance
            // create opens the instance's engine listener through the core, and binding a
            // socket needs the runtime's I/O driver.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            // ---- the two destinations ----
            for bucket in [GRANTED_BUCKET, OTHER_BUCKET] {
                dispatch(
                    &router,
                    &runtime,
                    "s3",
                    "PUT",
                    &format!("/{bucket}"),
                    &[],
                    format!(
                        "<CreateBucketConfiguration><LocationConstraint>{REGION}\
                         </LocationConstraint></CreateBucketConfiguration>"
                    ),
                )
                .served(&format!("create the bucket {bucket}"));
            }

            // ---- the key, under every spelling a request may name it by ----
            let key = kms(&router, &runtime, "CreateKey", serde_json::json!({}))
                .served("create the export's KMS key");
            let key: Value = serde_json::from_str(&key).expect("KMS answers JSON");
            let key_id = key["KeyMetadata"]["KeyId"]
                .as_str()
                .unwrap_or_else(|| panic!("KMS reports the key it minted: {key}"))
                .to_string();
            let key_arn = key["KeyMetadata"]["Arn"]
                .as_str()
                .unwrap_or_else(|| panic!("KMS reports the ARN of the key it minted: {key}"))
                .to_string();
            kms(
                &router,
                &runtime,
                "CreateAlias",
                serde_json::json!({ "AliasName": KEY_ALIAS, "TargetKeyId": key_id }),
            )
            .served("give the export's key an alias");

            // ---- the roles: one granted INLINE, one granted through an ATTACHED policy ----
            let inline_role = {
                let created = iam(
                    &router,
                    &runtime,
                    "CreateRole",
                    &params![
                        "RoleName" => "rds-export-grant-sets-inline",
                        "AssumeRolePolicyDocument" => EXPORT_TRUST,
                    ],
                )
                .served("create the role carrying its grant inline");
                let arn = element(&created, "Arn");
                iam(
                    &router,
                    &runtime,
                    "PutRolePolicy",
                    &params![
                        "RoleName" => "rds-export-grant-sets-inline",
                        "PolicyName" => "export-to-s3",
                        "PolicyDocument" => export_policy(GRANTED_BUCKET),
                    ],
                )
                .served("put the inline policy on it");
                arn
            };
            let managed_role = {
                let created = iam(
                    &router,
                    &runtime,
                    "CreateRole",
                    &params![
                        "RoleName" => "rds-export-grant-sets-managed",
                        "AssumeRolePolicyDocument" => EXPORT_TRUST,
                    ],
                )
                .served("create the role carrying an attached policy");
                let arn = element(&created, "Arn");
                let policy = iam(
                    &router,
                    &runtime,
                    "CreatePolicy",
                    &params![
                        "PolicyName" => "rds-export-grant-sets-export-to-s3",
                        "PolicyDocument" => export_policy(GRANTED_BUCKET),
                    ],
                )
                .served("create the managed policy the role is granted through");
                let policy_arn = element(&policy, "Arn");
                iam(
                    &router,
                    &runtime,
                    "AttachRolePolicy",
                    &params![
                        "RoleName" => "rds-export-grant-sets-managed",
                        "PolicyArn" => policy_arn,
                    ],
                )
                .served("attach the managed policy to the role");
                arn
            };

            // And a role granted those calls on EVERY bucket: the destination's own condition
            // is what the location read ANSWERS a role that may make it.
            let every_bucket_role = {
                let created = iam(
                    &router,
                    &runtime,
                    "CreateRole",
                    &params![
                        "RoleName" => "rds-export-grant-sets-every-bucket",
                        "AssumeRolePolicyDocument" => EXPORT_TRUST,
                    ],
                )
                .served("create the role granted every bucket");
                let arn = element(&created, "Arn");
                iam(
                    &router,
                    &runtime,
                    "PutRolePolicy",
                    &params![
                        "RoleName" => "rds-export-grant-sets-every-bucket",
                        "PolicyName" => "export-to-s3",
                        "PolicyDocument" => export_policy("*"),
                    ],
                )
                .served("put the inline policy on it");
                arn
            };

            // ---- the source: a DB instance and a snapshot of it ----
            rds(
                &router,
                &runtime,
                "CreateDBInstance",
                &params![
                    "DBInstanceIdentifier" => INSTANCE,
                    "Engine" => "mysql",
                    "DBInstanceClass" => "db.t3.micro",
                    "AllocatedStorage" => "20",
                    "MasterUsername" => "grantsadmin",
                    "MasterUserPassword" => "grants-Secret-99",
                ],
            )
            .served("create the source DB instance");
            poll_until("the source instance becomes available", || {
                let described = rds(
                    &router,
                    &runtime,
                    "DescribeDBInstances",
                    &params!["DBInstanceIdentifier" => INSTANCE],
                )
                .served("describe the source DB instance");
                (element(&described, "DBInstanceStatus") == "available").then_some(described)
            });
            rds(
                &router,
                &runtime,
                "CreateDBSnapshot",
                &params![
                    "DBSnapshotIdentifier" => SNAPSHOT,
                    "DBInstanceIdentifier" => INSTANCE,
                ],
            )
            .served("snapshot the source DB instance");
            let snapshot = poll_until("the source snapshot becomes available", || {
                let described = rds(
                    &router,
                    &runtime,
                    "DescribeDBSnapshots",
                    &params!["DBSnapshotIdentifier" => SNAPSHOT],
                )
                .served("describe the source DB snapshot");
                (element(&described, "Status") == "available").then_some(described)
            });
            let snapshot_arn = element(&snapshot, "DBSnapshotArn");

            // The rows differ along what the rule generalises over: WHERE the role's grant
            // lives (inline, attached), WHICH destination the request names (the granted one,
            // another one that exists), the SPELLING of the key (id, ARN, alias) and whether
            // the request carries an `S3Prefix`.
            let rows = [
                Row {
                    case: "an inline grant on another bucket, the key by id",
                    task: "grant-sets-inline-elsewhere",
                    role_arn: &inline_role,
                    bucket: OTHER_BUCKET,
                    key: &key_id,
                    prefix: Some("sets"),
                    outcome: Outcome::RefusedOnBucketLocation,
                },
                Row {
                    case: "an inline grant elsewhere, naming a bucket the Region does not hold",
                    task: "grant-sets-inline-absent",
                    role_arn: &inline_role,
                    bucket: ABSENT_BUCKET,
                    key: &key_id,
                    prefix: None,
                    outcome: Outcome::RefusedOnBucketLocation,
                },
                Row {
                    case: "a grant on every bucket, naming a bucket the Region does not hold",
                    task: "grant-sets-every-absent",
                    role_arn: &every_bucket_role,
                    bucket: ABSENT_BUCKET,
                    key: &key_arn,
                    prefix: Some("sets"),
                    outcome: Outcome::RefusedOnDestination,
                },
                Row {
                    case: "an inline grant elsewhere, naming no bucket name at all",
                    task: "grant-sets-inline-malformed",
                    role_arn: &inline_role,
                    bucket: MALFORMED_BUCKET,
                    key: &key_id,
                    prefix: None,
                    outcome: Outcome::RefusedOnDestination,
                },
                Row {
                    case: "an attached grant on another bucket, the key by ARN",
                    task: "grant-sets-managed-elsewhere",
                    role_arn: &managed_role,
                    bucket: OTHER_BUCKET,
                    key: &key_arn,
                    prefix: None,
                    outcome: Outcome::RefusedOnBucketLocation,
                },
                Row {
                    case: "an attached grant on the bucket named, the key by alias",
                    task: "grant-sets-managed-granted",
                    role_arn: &managed_role,
                    bucket: GRANTED_BUCKET,
                    key: KEY_ALIAS,
                    prefix: Some("sets"),
                    outcome: Outcome::Accepted,
                },
                Row {
                    case: "an inline grant on the bucket named, the key by ARN, no prefix",
                    task: "grant-sets-inline-granted",
                    role_arn: &inline_role,
                    bucket: GRANTED_BUCKET,
                    key: &key_arn,
                    prefix: None,
                    outcome: Outcome::Accepted,
                },
            ];

            for row in &rows {
                let case = row.case;
                let mut request = vec![
                    ("ExportTaskIdentifier", row.task.to_string()),
                    ("SourceArn", snapshot_arn.clone()),
                    ("S3BucketName", row.bucket.to_string()),
                    ("IamRoleArn", row.role_arn.to_string()),
                    ("KmsKeyId", row.key.to_string()),
                ];
                if let Some(prefix) = row.prefix {
                    request.push(("S3Prefix", prefix.to_string()));
                }
                let answered = rds(&router, &runtime, "StartExportTask", &request);
                match row.outcome {
                    Outcome::RefusedOnBucketLocation => {
                        assert_eq!(
                            answered.status, 400,
                            "case {case:?}: expected HTTP 400, got {} with {}",
                            answered.status, answered.body
                        );
                        let code = element(&answered.body, "Code");
                        assert_eq!(
                            code, "IamRoleMissingPermissions",
                            "case {case:?}: expected IamRoleMissingPermissions, got {code:?} \
                             in {}",
                            answered.body
                        );
                        let expected = format!(
                            "The IAM role {} isn't authorized to call s3:GetBucketLocation \
                             on the S3 bucket {}.",
                            row.role_arn, row.bucket
                        );
                        let message = message_of(&answered.body);
                        assert_eq!(
                            message, expected,
                            "case {case:?}: expected message {expected:?}, got {message:?}"
                        );
                        // A refused export records no task and writes nothing.
                        let described = rds(
                            &router,
                            &runtime,
                            "DescribeExportTasks",
                            &params!["ExportTaskIdentifier" => row.task],
                        )
                        .served(&format!("describe the refused export of {case:?}"));
                        assert_eq!(
                            opt_element(&described, "ExportTaskIdentifier"),
                            None,
                            "case {case:?}: a refused export records no task, got {described}"
                        );
                    }
                    Outcome::RefusedOnDestination => {
                        assert_eq!(
                            answered.status, 400,
                            "case {case:?}: expected HTTP 400, got {} with {}",
                            answered.status, answered.body
                        );
                        let code = element(&answered.body, "Code");
                        assert_eq!(
                            code, "InvalidS3BucketFault",
                            "case {case:?}: expected InvalidS3BucketFault, got {code:?} in {}",
                            answered.body
                        );
                        let expected = format!(
                            "The S3 bucket {} isn't valid. Either it isn't located in the \
                             current AWS Region or it doesn't exist.",
                            row.bucket
                        );
                        let message = message_of(&answered.body);
                        assert_eq!(
                            message, expected,
                            "case {case:?}: expected message {expected:?}, got {message:?}"
                        );
                        let described = rds(
                            &router,
                            &runtime,
                            "DescribeExportTasks",
                            &params!["ExportTaskIdentifier" => row.task],
                        )
                        .served(&format!("describe the refused export of {case:?}"));
                        assert_eq!(
                            opt_element(&described, "ExportTaskIdentifier"),
                            None,
                            "case {case:?}: a refused export records no task, got {described}"
                        );
                    }
                    Outcome::Accepted => {
                        let started =
                            answered.served(&format!("start the export of {case:?}"));
                        assert_eq!(
                            element(&started, "Status"),
                            "STARTING",
                            "case {case:?}: an export just accepted is STARTING, got {started}"
                        );
                        // And it is the task `DescribeExportTasks` then reports, which is
                        // what a client polls — read back through the reporting call rather
                        // than off the answer to the start.
                        let described = poll_until(case, || {
                            let described = rds(
                                &router,
                                &runtime,
                                "DescribeExportTasks",
                                &params!["ExportTaskIdentifier" => row.task],
                            )
                            .served(&format!("describe the export of {case:?}"));
                            (element(&described, "Status") == "COMPLETE").then_some(described)
                        });
                        assert_eq!(
                            element(&described, "S3Bucket"),
                            row.bucket,
                            "case {case:?}: the task names the destination it was started \
                             for, got {described}"
                        );
                        assert_eq!(
                            opt_element(&described, "FailureCause"),
                            None,
                            "case {case:?}: an export that completed carries no failure \
                             cause, got {described}"
                        );
                    }
                }
            }
        })
        .expect("the sized thread starts")
        .join()
        .expect("the suite runs to completion");
}
