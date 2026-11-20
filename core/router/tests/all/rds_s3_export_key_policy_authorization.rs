// SPDX-License-Identifier: CC0-1.0

//! Which KMS keys an export is accepted under, and which it is refused for.
//!
//! RULE: an export is refused for its key exactly when AWS KMS's own authorities ANSWER that
//! the two operations `KmsKeyId` documents — `kms:CreateGrant` and `kms:DescribeKey` — are
//! not available on it: KMS holds no such key, holds it unusable, or the KEY POLICY withholds
//! them from the export. That policy decision is IAM's: it is taken on the KEY — the resource
//! a key policy names by the wildcard or by the key's own ARN, whichever of the four
//! spellings `KmsKeyId` publishes the request used to select it — for the principals the
//! export acts as, and under the condition keys the export's own KMS calls carry (they are
//! made through Amazon RDS, on behalf of the caller's account, and the grant one of them
//! creates is created for an Amazon Web Services resource), so a statement scoped by one of
//! those keys DECIDES rather than staying undecided. The refusal is the modelled
//! `KMSKeyNotAccessibleFault` (HTTP 400), worded as the key's own absence with the reference
//! echoed exactly as the request spelled it; a key whose authorities DO admit the export
//! exports, and the task reaches `COMPLETE` with `PercentProgress` 100 and its objects under
//! the prefix.
//!
//! Only a cross-service suite can pin this: one key decision spans four services over ONE
//! engine (Amazon RDS for the export, AWS KMS for the key and its policy, IAM for the export
//! role and its policies, Amazon S3 for the destination), exactly as `core/router/src/lib.rs`
//! mounts them.
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
pub(crate) const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is signed for.
pub(crate) const REGION: &str = "us-west-2";

/// The export principal a role has to trust to be an export role, and the service the grant
/// an export takes on its key is created for ("Provide access to an Amazon S3 bucket using an
/// IAM role", Exporting DB snapshot data to Amazon S3).
pub(crate) const EXPORT_PRINCIPAL: &str = "export.rds.amazonaws.com";

/// A trust policy that admits the export principal.
pub(crate) const EXPORT_TRUST: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"Service":"export.rds.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#
);

/// The two KMS operations an export has to be authorized to run on its key, as `KmsKeyId`
/// lists them.
pub(crate) const EXPORT_KEY_ACTIONS: [&str; 2] = ["kms:CreateGrant", "kms:DescribeKey"];

/// How many times a wait looks again before it gives up. The transitions this control plane
/// owes are due on a sub-second clock, so this leaves generous room on a loaded machine while
/// never waiting on something that is not coming.
pub(crate) const POLL_LOOKS: usize = 300;

/// How long one look waits before the next. Inside the poll loop only: nothing here sleeps
/// for a state instead of looking for it.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// What one call answered.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: String,
}

impl Answer {
    /// Assert the call was served, and hand back its body.
    pub(crate) fn served(self, case: &str) -> String {
        assert_eq!(
            self.status, 200,
            "case {case:?}: the call must be served, got HTTP {} with {}",
            self.status, self.body
        );
        self.body
    }
}

/// One call of `method` to the service `signing_name` at `path`, with `body`.
pub(crate) fn dispatch(
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
pub(crate) fn rds(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(&str, String)],
) -> Answer {
    query(router, runtime, "rds", "2014-10-31", action, params)
}

/// One awsQuery call to IAM.
pub(crate) fn iam(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    action: &str,
    params: &[(&str, String)],
) -> Answer {
    query(router, runtime, "iam", "2010-05-08", action, params)
}

/// One JSON-1.1 call to AWS KMS.
pub(crate) fn kms(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
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
            ("x-amz-target", &format!("TrentService.{operation}")),
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
pub(crate) use params;

/// The `Message` of an awsQuery error answer, with the serializer's entity escaping undone,
/// so the sentence compares as the service worded it.
fn message_of(body: &str) -> String {
    elements(body, "Message")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no <Message> element in {body}"))
}

/// Look again until `probe` answers, so a wait is for the CONDITION rather than for a span.
pub(crate) fn poll_until(case: &str, mut probe: impl FnMut() -> Option<String>) -> String {
    for _ in 0..POLL_LOOKS {
        if let Some(answer) = probe() {
            return answer;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    panic!("case {case:?}: the state never arrived over {POLL_LOOKS} looks");
}

/// A key KMS mints under `policy`, reported as `(key id, key ARN)`.
///
/// A document is attached with the policy-lockout safety check bypassed where it is one the
/// caller would not keep management of: these rows are about what the EXPORT may do under a
/// document, not about who may replace it.
pub(crate) fn create_key(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
    policy: Option<String>,
) -> (String, String) {
    let mut body = serde_json::json!({});
    if let Some(policy) = policy {
        body["Policy"] = Value::String(policy);
        body["BypassPolicyLockoutSafetyCheck"] = Value::Bool(true);
    }
    let minted = kms(router, runtime, "CreateKey", body).served(case);
    let minted: Value = serde_json::from_str(&minted).expect("KMS answers JSON");
    let metadata = &minted["KeyMetadata"];
    (
        metadata["KeyId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: KMS reports the key id: {minted}"))
            .to_string(),
        metadata["Arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: KMS reports the key ARN: {minted}"))
            .to_string(),
    )
}

/// One key-policy document: `actions` on `resource` for `principal` under `effect`, with the
/// `Condition` clause where the row carries one.
pub(crate) fn key_policy(
    effect: &str,
    principal: Value,
    actions: &[&str],
    resource: &str,
    condition: Option<Value>,
) -> String {
    let mut statement = serde_json::json!({
        "Effect": effect,
        "Principal": principal,
        "Action": actions,
        "Resource": resource,
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    serde_json::json!({ "Version": "2012-10-17", "Statement": [statement] }).to_string()
}

/// Start one export of `snapshot_arn` into `bucket` under the key reference `key`, below the
/// prefix `task` (one folder per row, so the rows cannot read each other's objects).
pub(crate) fn start_export(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    task: &str,
    snapshot_arn: &str,
    bucket: &str,
    role_arn: &str,
    key: &str,
) -> Answer {
    rds(
        router,
        runtime,
        "StartExportTask",
        &params![
            "ExportTaskIdentifier" => task,
            "SourceArn" => snapshot_arn,
            "S3BucketName" => bucket,
            "S3Prefix" => task,
            "IamRoleArn" => role_arn,
            "KmsKeyId" => key,
        ],
    )
}

/// The destination, the export role and the source snapshot every row of a key suite runs
/// against, as `(bucket, role ARN, snapshot ARN)`.
///
/// One instance and one snapshot for the whole suite: the rows differ in the KEY, so they
/// share everything else.
pub(crate) fn export_fixtures(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    stem: &str,
) -> (String, String, String) {
    let bucket = format!("{stem}-destination");
    dispatch(
        router,
        runtime,
        "s3",
        "PUT",
        &format!("/{bucket}"),
        &[],
        format!(
            "<CreateBucketConfiguration><LocationConstraint>{REGION}\
             </LocationConstraint></CreateBucketConfiguration>"
        ),
    )
    .served("create the destination bucket");

    let role = format!("{stem}-role");
    let created = iam(
        router,
        runtime,
        "CreateRole",
        &params![
            "RoleName" => role,
            "AssumeRolePolicyDocument" => EXPORT_TRUST,
        ],
    )
    .served("create the export role");
    let role_arn = element(&created, "Arn");
    let bucket_arn = format!("arn:aws:s3:::{bucket}");
    // The five actions the user guide's export role carries on the destination, and the two
    // KMS operations the export performs on its key.
    let role_policy = serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": [
                    "s3:PutObject*",
                    "s3:GetObject*",
                    "s3:ListBucket",
                    "s3:DeleteObject*",
                    "s3:GetBucketLocation",
                ],
                "Resource": [bucket_arn.clone(), format!("{bucket_arn}/*")],
            },
            {
                "Effect": "Allow",
                "Action": EXPORT_KEY_ACTIONS,
                "Resource": "*",
            },
        ],
    })
    .to_string();
    iam(
        router,
        runtime,
        "PutRolePolicy",
        &params![
            "RoleName" => role,
            "PolicyName" => "export-to-s3",
            "PolicyDocument" => role_policy,
        ],
    )
    .served("attach the export role's policy");

    let instance = format!("{stem}-instance");
    let snapshot = format!("{stem}-snapshot");
    rds(
        router,
        runtime,
        "CreateDBInstance",
        &params![
            "DBInstanceIdentifier" => instance,
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
            router,
            runtime,
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => instance],
        )
        .served("describe the source DB instance");
        (element(&described, "DBInstanceStatus") == "available").then_some(described)
    });
    rds(
        router,
        runtime,
        "CreateDBSnapshot",
        &params![
            "DBSnapshotIdentifier" => snapshot,
            "DBInstanceIdentifier" => instance,
        ],
    )
    .served("snapshot the source DB instance");
    let described = poll_until("the source snapshot becomes available", || {
        let described = rds(
            router,
            runtime,
            "DescribeDBSnapshots",
            &params!["DBSnapshotIdentifier" => snapshot],
        )
        .served("describe the source DB snapshot");
        (element(&described, "Status") == "available").then_some(described)
    });

    (bucket, role_arn, element(&described, "DBSnapshotArn"))
}

/// Every way a key's own authorities refuse an export, and every way they admit one.
#[test]
fn an_export_is_judged_against_the_keys_own_authorities() {
    std::thread::Builder::new()
        .name("rds-s3-export-key-policy".to_string())
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

            let (bucket, role_arn, snapshot_arn) =
                export_fixtures(&router, &runtime, "rds-export-key-probe");
            let account = role_arn
                .split(':')
                .nth(4)
                .expect("the role ARN carries an account")
                .to_string();
            let root = format!("arn:aws:iam::{account}:root");

            // ---- the keys each row is about ----
            // A key whose policy keeps the account's own access and explicitly DENIES the two
            // export operations to every principal.
            let (denied_key, denied_key_arn) = create_key(
                &router,
                &runtime,
                "a key whose policy denies the export operations",
                Some(
                    serde_json::json!({
                        "Version": "2012-10-17",
                        "Statement": [
                            {
                                "Effect": "Allow",
                                "Principal": {"AWS": root.clone()},
                                "Action": "kms:*",
                                "Resource": "*",
                            },
                            {
                                "Effect": "Deny",
                                "Principal": {"AWS": "*"},
                                "Action": EXPORT_KEY_ACTIONS,
                                "Resource": "*",
                            },
                        ],
                    })
                    .to_string(),
                ),
            );
            // A key whose policy grants the two operations to ANOTHER role and to nobody
            // else: it neither names this export's role nor delegates to the account.
            let (other_role_key, _) = create_key(
                &router,
                &runtime,
                "a key granting another role only",
                Some(key_policy(
                    "Allow",
                    serde_json::json!({"AWS": format!("arn:aws:iam::{account}:role/somebody-else")}),
                    &EXPORT_KEY_ACTIONS,
                    "*",
                    None,
                )),
            );
            // A key KMS holds, disabled: it exists and cannot encrypt anything.
            let (disabled_key, _) = create_key(&router, &runtime, "a key KMS holds disabled", None);
            kms(
                &router,
                &runtime,
                "DisableKey",
                serde_json::json!({"KeyId": disabled_key.clone()}),
            )
            .served("disable the key");
            // A well-formed key id KMS holds nothing under.
            let absent_key = "d1a6c8fe-0000-4000-8000-ab0123456789".to_string();

            // A key the account's own default key policy governs, which delegates to the
            // account's identity policies.
            let (default_key, default_key_arn) =
                create_key(&router, &runtime, "a key with the default key policy", None);
            // A policy granting the export role the two operations on the wildcard resource a
            // key policy names its own key by.
            let (wildcard_key, _) = create_key(
                &router,
                &runtime,
                "a key granting the role on the wildcard resource",
                Some(key_policy(
                    "Allow",
                    serde_json::json!({"AWS": role_arn.clone()}),
                    &EXPORT_KEY_ACTIONS,
                    "*",
                    None,
                )),
            );
            // The same grant written against the key's OWN ARN, which is the other spelling a
            // key policy states its resource with.
            let (arn_resource_key, arn_resource_key_arn) =
                create_key(&router, &runtime, "a key granting the role on its own ARN", None);
            kms(
                &router,
                &runtime,
                "PutKeyPolicy",
                serde_json::json!({
                    "KeyId": arn_resource_key.clone(),
                    "PolicyName": "default",
                    "BypassPolicyLockoutSafetyCheck": true,
                    "Policy": key_policy(
                        "Allow",
                        serde_json::json!({"AWS": role_arn.clone()}),
                        &EXPORT_KEY_ACTIONS,
                        &arn_resource_key_arn,
                        None,
                    ),
                }),
            )
            .served("write the key policy naming the key's own ARN");
            // A grant conditioned on the service the KMS call arrives THROUGH, which for an
            // export is Amazon RDS at this Region's endpoint.
            let (via_service_key, _) = create_key(
                &router,
                &runtime,
                "a key granting the role through Amazon RDS only",
                Some(key_policy(
                    "Allow",
                    serde_json::json!({"AWS": role_arn.clone()}),
                    &EXPORT_KEY_ACTIONS,
                    "*",
                    Some(serde_json::json!({
                        "StringEquals": {"kms:ViaService": format!("rds.{REGION}.amazonaws.com")},
                    })),
                )),
            );
            // A grant conditioned on the grant being created for an Amazon Web Services
            // resource, which is what an export's grant is. The condition key is one AWS KMS
            // publishes for the GRANT operations alone, so it scopes that statement and the
            // key's read stands unconditioned beside it.
            let (aws_resource_key, _) = create_key(
                &router,
                &runtime,
                "a key granting the role a grant for an AWS resource only",
                Some(
                    serde_json::json!({
                        "Version": "2012-10-17",
                        "Statement": [
                            {
                                "Effect": "Allow",
                                "Principal": {"AWS": role_arn.clone()},
                                "Action": "kms:CreateGrant",
                                "Resource": "*",
                                "Condition": {
                                    "Bool": {"kms:GrantIsForAWSResource": "true"},
                                },
                            },
                            {
                                "Effect": "Allow",
                                "Principal": {"AWS": role_arn.clone()},
                                "Action": "kms:DescribeKey",
                                "Resource": "*",
                            },
                        ],
                    })
                    .to_string(),
                ),
            );
            // And an ALIAS of the key the default policy governs: the fourth spelling
            // `KmsKeyId` publishes.
            kms(
                &router,
                &runtime,
                "CreateAlias",
                serde_json::json!({
                    "AliasName": "alias/rds-export-key-probe",
                    "TargetKeyId": default_key.clone(),
                }),
            )
            .served("alias the key with the default policy");

            // ---- the refusals: every condition that leaves the key unusable ----
            // Each row differs in WHY the key is unusable, or in the spelling the request
            // named it by, and every one of them is the same fault and the same sentence with
            // the reference echoed as passed.
            let refusals: [(&str, &str, &str); 5] = [
                (
                    "a key whose policy denies the export operations, by key id",
                    "key-denied-by-id",
                    denied_key.as_str(),
                ),
                (
                    "the same key named by its ARN",
                    "key-denied-by-arn",
                    denied_key_arn.as_str(),
                ),
                (
                    "a key granting another role only",
                    "key-other-role",
                    other_role_key.as_str(),
                ),
                (
                    "a key KMS holds disabled",
                    "key-disabled",
                    disabled_key.as_str(),
                ),
                (
                    "a key id KMS holds nothing under",
                    "key-absent",
                    absent_key.as_str(),
                ),
            ];
            for (case, task, reference) in refusals {
                let answered = start_export(
                    &router,
                    &runtime,
                    task,
                    &snapshot_arn,
                    &bucket,
                    &role_arn,
                    reference,
                );
                assert_eq!(
                    answered.status, 400,
                    "case {case:?}: expected HTTP 400, got {} with {}",
                    answered.status, answered.body
                );
                let code = element(&answered.body, "Code");
                assert_eq!(
                    code, "KMSKeyNotAccessibleFault",
                    "case {case:?}: expected KMSKeyNotAccessibleFault, got {code:?} in {}",
                    answered.body
                );
                let expected = format!("The KMS key {reference} doesn't exist or is disabled.");
                let message = message_of(&answered.body);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected message {expected:?}, got {message:?}"
                );
                // A refused export records no task and writes nothing: the key is judged
                // before anything is stored.
                let described = rds(
                    &router,
                    &runtime,
                    "DescribeExportTasks",
                    &params!["ExportTaskIdentifier" => task],
                )
                .served("describe the refused export task");
                assert_eq!(
                    opt_element(&described, "ExportTaskIdentifier"),
                    None,
                    "case {case:?}: a refused export records no task, got {described}"
                );
                let listed = dispatch(
                    &router,
                    &runtime,
                    "s3",
                    "GET",
                    &format!("/{bucket}?list-type=2&prefix={task}"),
                    &[],
                    String::new(),
                )
                .served("list the refused export's prefix");
                assert_eq!(
                    elements(&listed, "Key"),
                    Vec::<String>::new(),
                    "case {case:?}: a refused export writes nothing, got {listed}"
                );
            }

            // ---- and every key whose authorities DO admit the export ----
            let accepted: [(&str, &str, &str); 7] = [
                (
                    "a key the account's own default policy governs",
                    "key-ok-default",
                    default_key.as_str(),
                ),
                (
                    "the same key named by its ARN",
                    "key-ok-default-arn",
                    default_key_arn.as_str(),
                ),
                (
                    "the same key named by an alias",
                    "key-ok-default-alias",
                    "alias/rds-export-key-probe",
                ),
                (
                    "a key granting the role on the wildcard resource",
                    "key-ok-wildcard",
                    wildcard_key.as_str(),
                ),
                (
                    "a key granting the role on the key's own ARN",
                    "key-ok-arn-resource",
                    arn_resource_key.as_str(),
                ),
                (
                    "a key granting the role through Amazon RDS only",
                    "key-ok-via-service",
                    via_service_key.as_str(),
                ),
                (
                    "a key granting the role a grant for an AWS resource only",
                    "key-ok-aws-resource",
                    aws_resource_key.as_str(),
                ),
            ];
            for (case, task, reference) in accepted {
                let started = start_export(
                    &router,
                    &runtime,
                    task,
                    &snapshot_arn,
                    &bucket,
                    &role_arn,
                    reference,
                )
                .served(case);
                assert_eq!(
                    element(&started, "Status"),
                    "STARTING",
                    "case {case:?}: an export just accepted is STARTING, got {started}"
                );
                let described = poll_until(case, || {
                    let described = rds(
                        &router,
                        &runtime,
                        "DescribeExportTasks",
                        &params!["ExportTaskIdentifier" => task],
                    )
                    .served("describe the export task");
                    ["COMPLETE", "FAILED", "CANCELED"]
                        .contains(&element(&described, "Status").as_str())
                        .then_some(described)
                });
                assert_eq!(
                    element(&described, "Status"),
                    "COMPLETE",
                    "case {case:?}: the export must settle to COMPLETE, got {described}"
                );
                assert_eq!(
                    element(&described, "PercentProgress"),
                    "100",
                    "case {case:?}: a completed export exported all of its source, got \
                     {described}"
                );
                assert_eq!(
                    opt_element(&described, "FailureCause"),
                    None,
                    "case {case:?}: an export that completed carries no failure cause, got \
                     {described}"
                );
                let listed = dispatch(
                    &router,
                    &runtime,
                    "s3",
                    "GET",
                    &format!("/{bucket}?list-type=2&prefix={task}"),
                    &[],
                    String::new(),
                )
                .served("list the export's prefix");
                let keys = elements(&listed, "Key");
                assert!(
                    !keys.is_empty(),
                    "case {case:?}: a completed export leaves its objects under its own \
                     prefix, got {listed}"
                );
            }
        })
        .expect("the sized thread starts")
        .join()
        .expect("the suite runs to its assertions");
}
