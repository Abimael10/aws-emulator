// SPDX-License-Identifier: CC0-1.0

//! What `StartExportTask` answers for each way an export's destination or its IAM role can
//! stop it, and what a permitted export leaves in the bucket.
//!
//! RULE: Amazon RDS resolves an export's role and then makes the calls an export makes on the
//! destination IN ORDER — it reads the bucket's location before it writes an object — and
//! reports the FIRST answer that stops the export, as the fault that answer carries: a
//! destination the Region holds nothing under is the bucket fault; a role this account holds
//! nothing under is a principal the export's KMS grant cannot be created for, so it is the
//! key fault — worded as the generic "specified KMS key" sentence naming the KEY the request's
//! reference resolves to, since the grant would have been created on that key — ; a role it
//! HOLDS that the export principal cannot assume is the role-not-found
//! fault; and a role that can be assumed but cannot make one of those calls — because nothing
//! grants it, or because the bucket's own policy denies it — is the missing-permissions
//! fault, whose sentence names the call that failed rather than a fixed action. A permitted
//! export walks to a terminal status and leaves both of the files an export writes under its
//! own folder, the summary one naming the source it read.
//!
//! Only a cross-service suite can pin this: one export decision spans four services over ONE
//! engine (Amazon RDS for the control plane and the export, IAM for the role and its
//! policies, AWS KMS for the key the data is encrypted under, and Amazon S3 for the
//! destination and its policy), exactly as `core/router/src/lib.rs` mounts them.
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

/// The destination bucket every accepted export writes into.
const BUCKET: &str = "rds-export-probe-destination";

/// A bucket name this Region's object store holds nothing under.
const ABSENT_BUCKET: &str = "rds-export-probe-no-such-bucket";

/// The prefix the permitted export writes under.
const PREFIX: &str = "probe";

/// The DB instance whose snapshot every export below reads.
const INSTANCE: &str = "rds-export-probe-instance";

/// The snapshot of [`INSTANCE`] that is the exports' `SourceArn`.
const SNAPSHOT: &str = "rds-export-probe-snapshot";

/// The export principal a role has to trust to be an export role: the service that performs
/// a snapshot export ("Provide access to an Amazon S3 bucket using an IAM role", Exporting DB
/// snapshot data to Amazon S3).
const EXPORT_PRINCIPAL: &str = "export.rds.amazonaws.com";

/// A trust policy that admits the export principal.
const EXPORT_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"export.rds.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// A trust policy that admits a DIFFERENT service principal, so the export cannot assume the
/// role at all.
const OTHER_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// How many times a wait looks again before it gives up. The transitions this control plane
/// owes are due on a sub-second clock, so this leaves generous room on a loaded machine while
/// never waiting on something that is not coming.
const POLL_LOOKS: usize = 200;

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

    /// Assert the call was accepted, for an operation that answers with no content (Amazon
    /// S3's `PutBucketPolicy` answers `204`).
    fn accepted(self, case: &str) {
        assert!(
            (200..300).contains(&self.status),
            "case {case:?}: the call must be accepted, got HTTP {} with {}",
            self.status,
            self.body
        );
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

/// `[(name, value.to_string()), ..]` — the parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The `Message` of an awsQuery error answer, with the serializer's entity escaping undone,
/// so the sentence compares as the service worded it.
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

/// One export this suite starts, and what it is answered with.
struct Attempt<'a> {
    /// What the row is about, quoted by every assertion of it.
    case: &'a str,
    /// The export task identifier the attempt uses, which is also its folder in the bucket.
    task: &'a str,
    /// The `IamRoleArn` the attempt passes.
    role_arn: &'a str,
    /// The `S3BucketName` the attempt passes.
    bucket: &'a str,
}

/// Start one export of the suite's snapshot, with the suite's key.
fn start_export(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    attempt: &Attempt<'_>,
    snapshot_arn: &str,
    kms_key_id: &str,
) -> Answer {
    rds(
        router,
        runtime,
        "StartExportTask",
        &params![
            "ExportTaskIdentifier" => attempt.task,
            "SourceArn" => snapshot_arn,
            "S3BucketName" => attempt.bucket,
            "S3Prefix" => PREFIX,
            "IamRoleArn" => attempt.role_arn,
            "KmsKeyId" => kms_key_id,
        ],
    )
}

/// One policy document: a single statement of `effect` over `actions` and `resources`, for
/// `principal` where the document is a resource-based one (a bucket policy names the identity
/// it is about; an identity policy does not).
fn policy_document(
    effect: &str,
    principal: Option<&str>,
    actions: &[&str],
    resources: &[String],
) -> String {
    let mut statement = serde_json::json!({
        "Effect": effect,
        "Action": actions,
        "Resource": resources,
    });
    if let Some(principal) = principal {
        statement["Principal"] = serde_json::json!({ "AWS": principal });
    }
    serde_json::json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string()
}

/// Every refusal of the export surface, and the export that is permitted, over ONE source
/// snapshot, ONE key and one destination.
#[test]
fn an_export_is_judged_role_and_destination_call_by_call() {
    std::thread::Builder::new()
        .name("rds-s3-export-probe".to_string())
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

            // ---- the destination, the key and the roles ----
            dispatch(
                &router,
                &runtime,
                "s3",
                "PUT",
                &format!("/{BUCKET}"),
                &[],
                format!(
                    "<CreateBucketConfiguration><LocationConstraint>{REGION}\
                     </LocationConstraint></CreateBucketConfiguration>"
                ),
            )
            .served("create the destination bucket");
            let key = dispatch(
                &router,
                &runtime,
                "kms",
                "POST",
                "/",
                &[
                    ("content-type", "application/x-amz-json-1.1"),
                    ("x-amz-target", "TrentService.CreateKey"),
                ],
                "{}".to_string(),
            )
            .served("create the export's KMS key");
            let key: Value = serde_json::from_str(&key).expect("KMS answers JSON");
            let kms_key_id = key["KeyMetadata"]["KeyId"]
                .as_str()
                .unwrap_or_else(|| panic!("KMS reports the key it minted: {key}"))
                .to_string();
            // The same key as KMS NAMES it: the refusal that reports a grant the export could
            // not create is about the KEY that grant would have been created on, so it names
            // that key rather than the reference the request spelled it with.
            let kms_key_arn = key["KeyMetadata"]["Arn"]
                .as_str()
                .unwrap_or_else(|| panic!("KMS reports the ARN of the key it minted: {key}"))
                .to_string();

            let bucket_arn = format!("arn:aws:s3:::{BUCKET}");
            let object_arns = vec![bucket_arn.clone(), format!("{bucket_arn}/*")];
            // The five actions the user guide's export role carries.
            let every_action = policy_document(
                "Allow",
                None,
                &[
                    "s3:PutObject*",
                    "s3:GetObject*",
                    "s3:ListBucket",
                    "s3:DeleteObject*",
                    "s3:GetBucketLocation",
                ],
                &object_arns,
            );
            // A role that may READ the destination but may not write into it.
            let read_only = policy_document(
                "Allow",
                None,
                &["s3:GetBucketLocation", "s3:ListBucket"],
                std::slice::from_ref(&bucket_arn),
            );

            // (the role's name, its trust document, the policy it carries if any)
            let roles: [(&str, &str, Option<&str>); 4] = [
                ("probe-export-granted", EXPORT_TRUST, Some(&every_action)),
                ("probe-export-read-only", EXPORT_TRUST, Some(&read_only)),
                ("probe-export-no-policy", EXPORT_TRUST, None),
                ("probe-export-other-trust", OTHER_TRUST, Some(&every_action)),
            ];
            let mut role_arns: Vec<String> = Vec::new();
            for (name, trust, policy) in roles {
                let created = iam(
                    &router,
                    &runtime,
                    "CreateRole",
                    &params![
                        "RoleName" => name,
                        "AssumeRolePolicyDocument" => trust,
                    ],
                )
                .served(&format!("create the role {name}"));
                role_arns.push(element(&created, "Arn"));
                if let Some(policy) = policy {
                    iam(
                        &router,
                        &runtime,
                        "PutRolePolicy",
                        &params![
                            "RoleName" => name,
                            "PolicyName" => "export-to-s3",
                            "PolicyDocument" => policy,
                        ],
                    )
                    .served(&format!("attach the policy of {name}"));
                }
            }
            let granted = role_arns[0].clone();
            let read_only_role = role_arns[1].clone();
            let no_policy_role = role_arns[2].clone();
            let other_trust_role = role_arns[3].clone();
            // A well-formed ARN of this account naming a role IAM holds nothing under: the
            // granted role's own ARN with another name in it.
            let absent_role = format!(
                "{}/probe-export-absent",
                granted
                    .rsplit_once('/')
                    .unwrap_or_else(|| panic!("the role ARN carries a name: {granted}"))
                    .0
            );

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
                    "MasterUsername" => "probeadmin",
                    "MasterUserPassword" => "probe-Secret-99",
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

            // ---- the refusals ----
            // Each row differs in the ONE thing that stops the export, and carries the fault
            // and the sentence that answer is reported as.
            let refusals: [(Attempt<'_>, u16, &str, String); 5] = [
                (
                    Attempt {
                        case: "a role granted nothing on the destination",
                        task: "probe-no-policy",
                        role_arn: &no_policy_role,
                        bucket: BUCKET,
                    },
                    400,
                    "IamRoleMissingPermissions",
                    format!(
                        "The IAM role {no_policy_role} isn't authorized to call \
                         s3:GetBucketLocation on the S3 bucket {BUCKET}."
                    ),
                ),
                (
                    Attempt {
                        case: "a role granted the bucket read but not the object write",
                        task: "probe-read-only",
                        role_arn: &read_only_role,
                        bucket: BUCKET,
                    },
                    400,
                    "IamRoleMissingPermissions",
                    format!(
                        "The IAM role {read_only_role} isn't authorized to call s3:PutObject \
                         on the S3 bucket {BUCKET}."
                    ),
                ),
                (
                    Attempt {
                        case: "a role whose trust policy omits the export principal",
                        task: "probe-other-trust",
                        role_arn: &other_trust_role,
                        bucket: BUCKET,
                    },
                    404,
                    "IamRoleNotFound",
                    format!(
                        "The principal {EXPORT_PRINCIPAL} isn't allowed to assume the IAM \
                         role {other_trust_role} or the IAM role {other_trust_role} doesn't \
                         exist."
                    ),
                ),
                (
                    Attempt {
                        case: "a role this account holds nothing under",
                        task: "probe-absent-role",
                        role_arn: &absent_role,
                        bucket: BUCKET,
                    },
                    400,
                    "KMSKeyNotAccessibleFault",
                    format!(
                        "The specified KMS key [{kms_key_arn}] does not exist, is not enabled \
                         or you do not have permissions to access it."
                    ),
                ),
                (
                    Attempt {
                        case: "a destination this Region holds nothing under",
                        task: "probe-absent-bucket",
                        role_arn: &granted,
                        bucket: ABSENT_BUCKET,
                    },
                    400,
                    "InvalidS3BucketFault",
                    format!(
                        "The S3 bucket {ABSENT_BUCKET} isn't valid. Either it isn't located \
                         in the current AWS Region or it doesn't exist."
                    ),
                ),
            ];
            for (attempt, status, code, message) in &refusals {
                let answered = start_export(
                    &router,
                    &runtime,
                    attempt,
                    &snapshot_arn,
                    &kms_key_id,
                );
                let case = attempt.case;
                assert_eq!(
                    answered.status, *status,
                    "case {case:?}: expected HTTP {status}, got {} with {}",
                    answered.status, answered.body
                );
                let answered_code = element(&answered.body, "Code");
                assert_eq!(
                    &answered_code, code,
                    "case {case:?}: expected error code {code:?}, got {answered_code:?} in {}",
                    answered.body
                );
                let answered_message = message_of(&answered.body);
                assert_eq!(
                    &answered_message, message,
                    "case {case:?}: expected message {message:?}, got {answered_message:?}"
                );
                // Nothing a refused export named exists: no task to poll and no object in the
                // destination.
                let described = rds(
                    &router,
                    &runtime,
                    "DescribeExportTasks",
                    &params!["ExportTaskIdentifier" => attempt.task],
                )
                .served("describe the refused export task");
                assert_eq!(
                    opt_element(&described, "ExportTaskIdentifier"),
                    None,
                    "case {case:?}: a refused export records no task, got {described}"
                );
            }

            // ---- the export that is permitted ----
            let task = "probe-permitted";
            let started = start_export(
                &router,
                &runtime,
                &Attempt {
                    case: "a role trusted and granted every call the export makes",
                    task,
                    role_arn: &granted,
                    bucket: BUCKET,
                },
                &snapshot_arn,
                &kms_key_id,
            )
            .served("start the permitted export");
            assert_eq!(
                element(&started, "Status"),
                "STARTING",
                "case \"a permitted export\": an export just accepted is STARTING, got {started}"
            );
            let described = poll_until("the permitted export reaches a terminal status", || {
                let described = rds(
                    &router,
                    &runtime,
                    "DescribeExportTasks",
                    &params!["ExportTaskIdentifier" => task],
                )
                .served("describe the permitted export task");
                ["COMPLETE", "FAILED", "CANCELED"]
                    .contains(&element(&described, "Status").as_str())
                    .then_some(described)
            });
            assert_eq!(
                element(&described, "Status"),
                "COMPLETE",
                "case \"a permitted export\": the export must settle to COMPLETE, got \
                 {described}"
            );
            assert_eq!(
                element(&described, "PercentProgress"),
                "100",
                "case \"a permitted export\": a completed export exported all of its source, \
                 got {described}"
            );
            assert_eq!(
                opt_element(&described, "FailureCause"),
                None,
                "case \"a permitted export\": an export that completed carries no failure \
                 cause, got {described}"
            );

            // Both of the files an export writes are under the export's own folder, each is
            // ONE JSON object describing the export, and each names the source the data was
            // read out of.
            let listed = dispatch(
                &router,
                &runtime,
                "s3",
                "GET",
                &format!("/{BUCKET}?list-type=2&prefix={PREFIX}%2F{task}%2F"),
                &[],
                String::new(),
            )
            .served("list the export's folder");
            let keys = elements(&listed, "Key");
            let summary_key = format!("{PREFIX}/{task}/export_info_{task}.json");
            let tables_key = format!("{PREFIX}/{task}/export_tables_info_{task}_");
            for (case, expected) in [
                ("the export's summary file", summary_key.as_str()),
                ("the per-table file", tables_key.as_str()),
            ] {
                assert!(
                    keys.iter().any(|key| key.starts_with(expected)),
                    "case {case:?}: the export writes {expected:?}, got {keys:?}"
                );
            }
            let tables_key = keys
                .iter()
                .find(|key| key.starts_with(&tables_key))
                .unwrap_or_else(|| panic!("the per-table file is listed, got {keys:?}"))
                .clone();
            for (case, key, table_statistics) in [
                ("the export's summary file", summary_key.clone(), "tables"),
                ("the per-table file", tables_key, "perTableStatus"),
            ] {
                let document = dispatch(
                    &router,
                    &runtime,
                    "s3",
                    "GET",
                    &format!("/{BUCKET}/{key}"),
                    &[],
                    String::new(),
                )
                .served(&format!("read {case}"));
                let document: Value = serde_json::from_str(&document)
                    .unwrap_or_else(|error| panic!("case {case:?}: it is JSON: {error}"));
                assert_eq!(
                    document["sourceArn"].as_str(),
                    Some(snapshot_arn.as_str()),
                    "case {case:?}: it names the source the export read, got {document}"
                );
                assert_eq!(
                    document["exportTaskIdentifier"].as_str(),
                    Some(task),
                    "case {case:?}: it names the export that wrote it, got {document}"
                );
                assert!(
                    document[table_statistics].is_array(),
                    "case {case:?}: its {table_statistics} is the list of what was exported, \
                     got {document}"
                );
            }

            // ---- and the destination's OWN policy is the other half of each call ----
            // The role is granted everything; the bucket denies it the object write, so the
            // export is refused — and named against the write rather than against the read it
            // was allowed to make.
            dispatch(
                &router,
                &runtime,
                "s3",
                "PUT",
                &format!("/{BUCKET}?policy"),
                &[],
                policy_document(
                    "Deny",
                    Some(&granted),
                    &["s3:PutObject"],
                    &[format!("{bucket_arn}/*")],
                ),
            )
            .accepted("deny the role the object write on the destination");
            let answered = start_export(
                &router,
                &runtime,
                &Attempt {
                    case: "a destination whose own policy denies the object write",
                    task: "probe-bucket-deny",
                    role_arn: &granted,
                    bucket: BUCKET,
                },
                &snapshot_arn,
                &kms_key_id,
            );
            let case = "a destination whose own policy denies the object write";
            assert_eq!(
                answered.status, 400,
                "case {case:?}: expected HTTP 400, got {} with {}",
                answered.status, answered.body
            );
            let code = element(&answered.body, "Code");
            assert_eq!(
                code, "IamRoleMissingPermissions",
                "case {case:?}: expected IamRoleMissingPermissions, got {code:?} in {}",
                answered.body
            );
            let expected = format!(
                "The IAM role {granted} isn't authorized to call s3:PutObject on the S3 \
                 bucket {BUCKET}."
            );
            let message = message_of(&answered.body);
            assert_eq!(
                message, expected,
                "case {case:?}: expected message {expected:?}, got {message:?}"
            );
        })
        .expect("the sized thread starts")
        .join()
        .expect("the suite runs to its assertions");
}
