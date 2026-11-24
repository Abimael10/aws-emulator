// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::Backup::BackupVault` with the
//! configurations AWS Backup applies through calls of its OWN really gets them applied —
//! on the create, and again in place when a deployment changes one.
//!
//! The rule under test: **a property of a resource type that the owning service applies
//! through a call of its own, separate from the create, is applied by that call once the
//! create has made the resource, is re-applied where a later deployment states it
//! differently and REMOVED through that configuration's own delete where a later deployment
//! no longer states it — and only a property no call of that service can change makes the
//! deployment a replacement.** For a backup vault those calls are
//! `PutBackupVaultAccessPolicy`, `PutBackupVaultNotifications`,
//! `PutBackupVaultLockConfiguration` and `TagResource`/`UntagResource`, while the vault's
//! name and the key it is encrypted under are members of `CreateBackupVault` alone. Two
//! crates have to agree for that to hold (CloudFormation makes the calls, AWS Backup serves
//! them), so the suite lives here rather than in either crate.
//!
//! The rows differ along what the rule generalises over: which of the configurations the
//! declaration states, the FORM the policy is written in (an inline document, or the text of
//! one), how many notification events it names, the retention bounds a declared lock carries,
//! whether the vault carries declared tags, the Region deployed in, and — for the change
//! table — whether a configuration is added, re-valued or dropped.
//!
//! Nothing here sleeps for a span and then asserts: every wait looks again for the CONDITION
//! it is waiting for until it holds or a generous deadline passes ([`poll_until`]).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`), so no suite re-derives the encoding its assertions depend on.
use crate::query_wire::{form_encode, opt_element, physical_ids_of};

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// How long a clock-owed transition is waited for before the wait is a failure: generously
/// more than the spans a stack owes, so a loaded host still reaches the condition.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The three tags CloudFormation records on every resource it provisions, by key.
const STACK_NAME_TAG: &str = "aws:cloudformation:stack-name";
const STACK_ID_TAG: &str = "aws:cloudformation:stack-id";
const LOGICAL_ID_TAG: &str = "aws:cloudformation:logical-id";

/// The logical id every template here gives its vault.
const VAULT_LOGICAL_ID: &str = "Vault";

/// The SNS topics the notification declarations name. Literal ARNs: the topic itself is no
/// part of what this suite is about, and `PutBackupVaultNotifications` takes the ARN the
/// declaration states.
const TOPIC_ARN: &str = "arn:aws:sns:us-west-2:000000000000:backup-vault-events";
const OTHER_TOPIC_ARN: &str = "arn:aws:sns:us-west-2:000000000000:backup-vault-alarms";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way
/// (the credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_backup_cryocontrollerusermanager::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one AWS Backup operation over its restJson1 wire, signed for `region`: the method
/// and URI its model binds the operation to.
async fn backup(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("backup.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("backup", region))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// A backup vault's ARN, as that service's own `backup_vault_arn` spells it.
fn vault_arn(region: &str, name: &str) -> String {
    format!("arn:aws:backup:{region}:{ACCOUNT}:backup-vault:{name}")
}

/// The error code a refusal carries, read off the `__type` its restJson1 error document
/// names the shape in.
fn error_code(answer: &Value) -> &str {
    answer["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
}

/// Look again until `look` reports the condition held, or [`SETTLE_DEADLINE`] passes —
/// panicking with `case` and whatever the last look saw.
///
/// Every wait in this suite goes through here: what is waited for is a CONDITION the clock
/// makes true, never an elapsed time.
async fn poll_until<F, Fut>(case: &str, waiting_for: &str, mut look: F) -> String
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = (bool, String)>,
{
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (held, seen) = look().await;
        if held {
            return seen;
        }
        if Instant::now() >= deadline {
            panic!("case {case:?}: timed out waiting for {waiting_for}; last saw {seen}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether a stack status is one a deployment STOPS in rather than one it passes through:
/// CloudFormation's own statuses end in `_IN_PROGRESS` while work is outstanding.
fn is_terminal(stack_status: &str) -> bool {
    stack_status.ends_with("_COMPLETE") || stack_status.ends_with("_FAILED")
}

/// Poll `DescribeStacks` until the stack rests in a terminal status, and answer that body.
async fn settled_stack(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack_name: &str,
) -> String {
    poll_until(case, "the stack to rest in a terminal status", || async {
        let (status, body) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
        if status != 200 {
            return (false, format!("HTTP {status} from DescribeStacks: {body}"));
        }
        let settled = opt_element(&body, "StackStatus").is_some_and(|it| is_terminal(&it));
        (settled, body)
    })
    .await
}

/// Deploy `template` as `stack_name` and hold the suite to the stack reaching `wanted`.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    action: &str,
    stack_name: &str,
    template: &str,
    wanted: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        region,
        action,
        &[("StackName", stack_name), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    let settled = settled_stack(router, case, region, stack_name).await;
    assert_eq!(
        opt_element(&settled, "StackStatus").as_deref(),
        Some(wanted),
        "case {case:?}: expected the stack to reach {wanted}, got {settled}"
    );
    settled
}

/// The physical id the stack publishes for the vault.
async fn vault_physical_id(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack_name: &str,
) -> String {
    let (status, resources) = cloudformation(
        router,
        region,
        "DescribeStackResources",
        &[("StackName", stack_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {resources}"
    );
    physical_ids_of(&resources)
        .get(VAULT_LOGICAL_ID)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: no physical id for {VAULT_LOGICAL_ID:?} in {resources}")
        })
}

/// What AWS Backup must report for one vault's resource policy: the document, parsed, or
/// nothing at all.
///
/// `GetBackupVaultAccessPolicy` reports the document in the canonical layout that service's
/// policy engine writes, so the assertion is on the document it PARSES to rather than on its
/// bytes; a vault carrying none is the modelled not-found.
async fn assert_access_policy(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    vault: &str,
    expected: Option<&Value>,
) {
    let (status, answered) = backup(
        router,
        region,
        "GET",
        &format!("/backup-vaults/{vault}/access-policy"),
    )
    .await;
    match expected {
        Some(document) => {
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetBackupVaultAccessPolicy on {vault:?}, \
                 got {status} with body {answered}"
            );
            let reported: Value = answered
                .get("Policy")
                .and_then(Value::as_str)
                .map(|text| serde_json::from_str(text).unwrap_or(Value::Null))
                .unwrap_or(Value::Null);
            assert_eq!(
                &reported, document,
                "case {case:?}: expected the vault to carry the declared policy {document}, got \
                 {answered}"
            );
        }
        None => {
            assert_eq!(
                status, 400,
                "case {case:?}: expected GetBackupVaultAccessPolicy to report no policy on \
                 {vault:?}, got {status} with body {answered}"
            );
            assert_eq!(
                error_code(&answered),
                "ResourceNotFoundException",
                "case {case:?}: expected the modelled not-found for a vault with no policy, got \
                 {answered}"
            );
        }
    }
}

/// What AWS Backup must report for one vault's notification configuration: the topic and the
/// events, or nothing at all.
async fn assert_notifications(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    vault: &str,
    expected: Option<(&str, &[&str])>,
) {
    let (status, answered) = backup(
        router,
        region,
        "GET",
        &format!("/backup-vaults/{vault}/notification-configuration"),
    )
    .await;
    match expected {
        Some((topic, events)) => {
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetBackupVaultNotifications on {vault:?}, \
                 got {status} with body {answered}"
            );
            assert_eq!(
                answered.get("SNSTopicArn").and_then(Value::as_str),
                Some(topic),
                "case {case:?}: expected the declared topic {topic:?}, got {answered}"
            );
            let reported: Vec<&str> = answered
                .get("BackupVaultEvents")
                .and_then(Value::as_array)
                .map(|events| events.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            assert_eq!(
                reported, events,
                "case {case:?}: expected the declared events {events:?}, got {answered}"
            );
        }
        None => {
            assert_eq!(
                status, 400,
                "case {case:?}: expected GetBackupVaultNotifications to report none on \
                 {vault:?}, got {status} with body {answered}"
            );
            assert_eq!(
                error_code(&answered),
                "ResourceNotFoundException",
                "case {case:?}: expected the modelled not-found for a vault with no \
                 notifications, got {answered}"
            );
        }
    }
}

/// What AWS Backup must report for one vault's LOCK: the two retention bounds the
/// declaration states, or an unlocked vault.
///
/// `DescribeBackupVault` is the read that answers for a vault's lock — it reports `Locked`,
/// `MinRetentionDays` and `MaxRetentionDays` — so that is where a declared
/// `LockConfiguration` has to show up.
async fn assert_lock(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    vault: &str,
    expected: Option<(i64, i64)>,
) {
    let (status, described) =
        backup(router, region, "GET", &format!("/backup-vaults/{vault}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeBackupVault on {vault:?}, got {status} \
         with body {described}"
    );
    let locked = described.get("Locked").and_then(Value::as_bool);
    match expected {
        Some((minimum, maximum)) => {
            assert_eq!(
                locked,
                Some(true),
                "case {case:?}: expected the declared lock to leave the vault locked, got \
                 {described}"
            );
            assert_eq!(
                described.get("MinRetentionDays").and_then(Value::as_i64),
                Some(minimum),
                "case {case:?}: expected MinRetentionDays {minimum}, got {described}"
            );
            assert_eq!(
                described.get("MaxRetentionDays").and_then(Value::as_i64),
                Some(maximum),
                "case {case:?}: expected MaxRetentionDays {maximum}, got {described}"
            );
        }
        None => {
            assert_eq!(
                locked,
                Some(false),
                "case {case:?}: expected an unlocked vault, got {described}"
            );
            assert_eq!(
                described.get("MinRetentionDays"),
                None,
                "case {case:?}: expected no MinRetentionDays on an unlocked vault, got \
                 {described}"
            );
        }
    }
}

/// The tags AWS Backup holds for the vault, read through `ListTags` on the vault's ARN.
async fn held_tags(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    vault: &str,
) -> BTreeMap<String, String> {
    let arn = vault_arn(region, vault);
    let (status, answered) = backup(
        router,
        region,
        "GET",
        &format!("/tags/{}", form_encode(&arn)),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListTags on {arn:?}, got {status} with body \
         {answered}"
    );
    answered
        .get("Tags")
        .and_then(Value::as_object)
        .map(|tags| {
            tags.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Hold the vault's tag set to exactly the declared tags beside the three CloudFormation
/// records on every resource it provisions.
async fn assert_tags(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stack_name: &str,
    vault: &str,
    declared: &[(&str, &str)],
) {
    let held = held_tags(router, case, region, vault).await;
    for (key, value) in declared {
        assert_eq!(
            held.get(*key).map(String::as_str),
            Some(*value),
            "case {case:?}: expected the vault to carry the declared tag {key:?}={value:?}, got \
             {held:?}"
        );
    }
    assert_eq!(
        held.get(STACK_NAME_TAG).map(String::as_str),
        Some(stack_name),
        "case {case:?}: expected the vault to carry {STACK_NAME_TAG}={stack_name:?}, got {held:?}"
    );
    assert_eq!(
        held.get(LOGICAL_ID_TAG).map(String::as_str),
        Some(VAULT_LOGICAL_ID),
        "case {case:?}: expected the vault to carry {LOGICAL_ID_TAG}={VAULT_LOGICAL_ID:?}, got \
         {held:?}"
    );
    assert!(
        held.contains_key(STACK_ID_TAG),
        "case {case:?}: expected the vault to carry {STACK_ID_TAG}, got {held:?}"
    );
    // The set is EXACTLY those: a key no declaration states and CloudFormation does not
    // record is one this deployment no longer gives the vault.
    let allowed: Vec<&str> = declared
        .iter()
        .map(|(key, _)| *key)
        .chain([STACK_NAME_TAG, STACK_ID_TAG, LOGICAL_ID_TAG])
        .collect();
    for key in held.keys() {
        assert!(
            allowed.contains(&key.as_str()),
            "case {case:?}: expected no tag {key:?} on the vault, got {held:?}"
        );
    }
}

/// One statement of a vault resource policy, naming its action and principal as the STRINGS
/// the IAM policy language admits for them ("a string or an array of strings").
fn policy_for(action: &str, region: &str, vault: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowBackupOperator",
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
            "Action": action,
            "Resource": vault_arn(region, vault)
        }]
    })
}

/// One row of the create table: a template, and what the rule says AWS Backup must then hold
/// for the vault it declares.
struct Row {
    case: &'static str,
    /// The Region the row deploys in: the vault is created there, and it is the Region its
    /// ARN carries.
    region: &'static str,
    /// The name the declaration gives the vault.
    vault_name: &'static str,
    /// The template body, in the form the row writes it.
    template: fn() -> String,
    /// The policy document the read must report, or `None` where the declaration states none.
    policy: Option<fn() -> Value>,
    /// The topic and the events the notification read must report, or `None` where the
    /// declaration states no notifications.
    notifications: Option<(&'static str, &'static [&'static str])>,
    /// The two retention bounds the vault's lock must report, or `None` where the
    /// declaration states no lock.
    lock: Option<(i64, i64)>,
    /// The tags the declaration asks for, beside the three CloudFormation records.
    tags: &'static [(&'static str, &'static str)],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a vault declaring nothing but its name",
            region: "us-west-2",
            vault_name: "cfn-vault-bare",
            template: || {
                json!({
                    "Resources": {
                        VAULT_LOGICAL_ID: {
                            "Type": "AWS::Backup::BackupVault",
                            "Properties": {"BackupVaultName": "cfn-vault-bare"}
                        }
                    }
                })
                .to_string()
            },
            policy: None,
            notifications: None,
            lock: None,
            tags: &[],
        },
        Row {
            case: "a vault declaring a resource policy as an inline document",
            region: "us-west-2",
            vault_name: "cfn-vault-policy",
            template: || {
                json!({
                    "Resources": {
                        VAULT_LOGICAL_ID: {
                            "Type": "AWS::Backup::BackupVault",
                            "Properties": {
                                "BackupVaultName": "cfn-vault-policy",
                                "AccessPolicy": policy_for(
                                    "backup:DescribeBackupVault",
                                    "us-west-2",
                                    "cfn-vault-policy",
                                )
                            }
                        }
                    }
                })
                .to_string()
            },
            policy: Some(|| {
                policy_for(
                    "backup:DescribeBackupVault",
                    "us-west-2",
                    "cfn-vault-policy",
                )
            }),
            notifications: None,
            lock: None,
            tags: &[],
        },
        Row {
            case: "a vault declaring the policy as the TEXT of one, and a single \
                   notification event",
            region: "us-east-1",
            vault_name: "cfn-vault-text-policy",
            template: || {
                json!({
                    "Resources": {
                        VAULT_LOGICAL_ID: {
                            "Type": "AWS::Backup::BackupVault",
                            "Properties": {
                                "BackupVaultName": "cfn-vault-text-policy",
                                "AccessPolicy": policy_for(
                                    "backup:ListRecoveryPointsByBackupVault",
                                    "us-east-1",
                                    "cfn-vault-text-policy",
                                )
                                .to_string(),
                                "Notifications": {
                                    "SNSTopicArn": TOPIC_ARN,
                                    "BackupVaultEvents": ["BACKUP_JOB_STARTED"]
                                }
                            }
                        }
                    }
                })
                .to_string()
            },
            policy: Some(|| {
                policy_for(
                    "backup:ListRecoveryPointsByBackupVault",
                    "us-east-1",
                    "cfn-vault-text-policy",
                )
            }),
            notifications: Some((TOPIC_ARN, &["BACKUP_JOB_STARTED"])),
            lock: None,
            tags: &[],
        },
        Row {
            case: "a vault declaring a policy, three notification events and its own tags",
            region: "eu-west-1",
            vault_name: "cfn-vault-everything",
            template: || {
                json!({
                    "Resources": {
                        VAULT_LOGICAL_ID: {
                            "Type": "AWS::Backup::BackupVault",
                            "Properties": {
                                "BackupVaultName": "cfn-vault-everything",
                                "BackupVaultTags": {"Owner": "platform", "Tier": "gold"},
                                "AccessPolicy": policy_for(
                                    "backup:CopyIntoBackupVault",
                                    "eu-west-1",
                                    "cfn-vault-everything",
                                ),
                                "Notifications": {
                                    "SNSTopicArn": OTHER_TOPIC_ARN,
                                    "BackupVaultEvents": [
                                        "BACKUP_JOB_STARTED",
                                        "BACKUP_JOB_FAILED",
                                        "RESTORE_JOB_COMPLETED"
                                    ]
                                }
                            }
                        }
                    }
                })
                .to_string()
            },
            policy: Some(|| {
                policy_for(
                    "backup:CopyIntoBackupVault",
                    "eu-west-1",
                    "cfn-vault-everything",
                )
            }),
            notifications: Some((
                OTHER_TOPIC_ARN,
                &[
                    "BACKUP_JOB_STARTED",
                    "BACKUP_JOB_FAILED",
                    "RESTORE_JOB_COMPLETED",
                ],
            )),
            lock: None,
            tags: &[("Owner", "platform"), ("Tier", "gold")],
        },
        Row {
            case: "the YAML spelling of a vault declaring notifications and tags and no \
                   policy",
            region: "eu-west-1",
            vault_name: "cfn-vault-yaml",
            template: || {
                [
                    "Resources:",
                    "  Vault:",
                    "    Type: AWS::Backup::BackupVault",
                    "    Properties:",
                    "      BackupVaultName: cfn-vault-yaml",
                    "      BackupVaultTags:",
                    "        Owner: storage",
                    "      Notifications:",
                    "        SNSTopicArn: arn:aws:sns:us-west-2:000000000000:backup-vault-events",
                    "        BackupVaultEvents:",
                    "          - COPY_JOB_FAILED",
                    "          - S3_BACKUP_OBJECT_FAILED",
                    "",
                ]
                .join("\n")
            },
            policy: None,
            notifications: Some((TOPIC_ARN, &["COPY_JOB_FAILED", "S3_BACKUP_OBJECT_FAILED"])),
            lock: None,
            tags: &[("Owner", "storage")],
        },
        Row {
            // The two retention bounds and NO `ChangeableForDays`, which is the mutable
            // (governance) lock the model documents: "if this parameter is not specified,
            // you can delete Vault Lock from the vault [...] at any time".
            case: "a vault declaring a vault LOCK beside a policy",
            region: "us-east-1",
            vault_name: "cfn-vault-locked",
            template: || {
                json!({
                    "Resources": {
                        VAULT_LOGICAL_ID: {
                            "Type": "AWS::Backup::BackupVault",
                            "Properties": {
                                "BackupVaultName": "cfn-vault-locked",
                                "LockConfiguration": {
                                    "MinRetentionDays": 7,
                                    "MaxRetentionDays": 365
                                },
                                "AccessPolicy": policy_for(
                                    "backup:DescribeBackupVault",
                                    "us-east-1",
                                    "cfn-vault-locked",
                                )
                            }
                        }
                    }
                })
                .to_string()
            },
            policy: Some(|| {
                policy_for(
                    "backup:DescribeBackupVault",
                    "us-east-1",
                    "cfn-vault-locked",
                )
            }),
            notifications: None,
            lock: Some((7, 365)),
            tags: &[],
        },
    ]
}

/// Every row: the configurations the declaration states reach AWS Backup, the ones it does
/// not state are not there, and deleting the stack takes the vault with it.
#[tokio::test]
async fn a_declared_vault_configuration_reaches_aws_backup() {
    let router = build_router();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("backup-vault-{index}");
        let template = (row.template)();

        deploy(
            &router,
            case,
            region,
            "CreateStack",
            &stack_name,
            &template,
            "CREATE_COMPLETE",
        )
        .await;

        // "Ref returns the backup vault name": the vault's physical id is the name the
        // declaration asked for, and AWS Backup answers to it.
        let vault = vault_physical_id(&router, case, region, &stack_name).await;
        assert_eq!(
            vault, row.vault_name,
            "case {case:?}: expected the vault's physical id to be the declared name {:?}, got \
             {vault:?}",
            row.vault_name
        );
        let (status, described) =
            backup(&router, region, "GET", &format!("/backup-vaults/{vault}")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeBackupVault on the name the stack \
             published, got {status} with body {described}"
        );
        assert_eq!(
            described.get("BackupVaultArn").and_then(Value::as_str),
            Some(vault_arn(region, &vault).as_str()),
            "case {case:?}: expected the read to report the vault's ARN, got {described}"
        );

        let expected_policy = row.policy.map(|policy| policy());
        assert_access_policy(&router, case, region, &vault, expected_policy.as_ref()).await;
        assert_notifications(&router, case, region, &vault, row.notifications).await;
        assert_lock(&router, case, region, &vault, row.lock).await;
        assert_tags(&router, case, region, &stack_name, &vault, row.tags).await;

        // Deleting the stack deletes the vault there, which is what makes every read above
        // evidence that the stack created it rather than that the reads answer anything.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        poll_until(case, "AWS Backup to stop holding the vault", || async {
            let (status, body) =
                backup(&router, region, "GET", &format!("/backup-vaults/{vault}")).await;
            (status != 200, format!("HTTP {status}: {body}"))
        })
        .await;
    }
}

/// One step of the change table: the properties the deployment declares beside the vault's
/// name, and what AWS Backup must then hold for the vault.
struct Step {
    case: &'static str,
    /// The properties the deployment declares beside `BackupVaultName`.
    properties: fn() -> Value,
    /// The policy the read must report after it, or `None` for a step that states none.
    policy: Option<fn() -> Value>,
    /// The topic and events the notification read must report after it, or `None`.
    notifications: Option<(&'static str, &'static [&'static str])>,
    /// The two retention bounds the vault's lock must report after it, or `None` for a step
    /// that states no lock.
    lock: Option<(i64, i64)>,
    /// The tags the vault must carry after it, beside the three CloudFormation records.
    tags: &'static [(&'static str, &'static str)],
}

/// The Region and vault the change table works in.
const CHANGE_REGION: &str = "us-west-2";
const CHANGED_VAULT: &str = "cfn-vault-changed";

fn steps() -> Vec<Step> {
    vec![
        Step {
            case: "the create, which states a policy, one event, a lock and one tag",
            properties: || {
                json!({
                    "BackupVaultTags": {"Owner": "platform"},
                    "LockConfiguration": {"MinRetentionDays": 7, "MaxRetentionDays": 365},
                    "AccessPolicy": policy_for(
                        "backup:DescribeBackupVault",
                        CHANGE_REGION,
                        CHANGED_VAULT,
                    ),
                    "Notifications": {
                        "SNSTopicArn": TOPIC_ARN,
                        "BackupVaultEvents": ["BACKUP_JOB_STARTED"]
                    }
                })
            },
            policy: Some(|| {
                policy_for(
                    "backup:DescribeBackupVault",
                    CHANGE_REGION,
                    CHANGED_VAULT,
                )
            }),
            notifications: Some((TOPIC_ARN, &["BACKUP_JOB_STARTED"])),
            lock: Some((7, 365)),
            tags: &[("Owner", "platform")],
        },
        Step {
            case: "a deployment that RE-VALUES all four: another action, another topic with \
                   two events, wider retention bounds, and a second tag",
            properties: || {
                json!({
                    "BackupVaultTags": {"Owner": "platform", "Tier": "gold"},
                    "LockConfiguration": {"MinRetentionDays": 14, "MaxRetentionDays": 730},
                    "AccessPolicy": policy_for(
                        "backup:CopyIntoBackupVault",
                        CHANGE_REGION,
                        CHANGED_VAULT,
                    ),
                    "Notifications": {
                        "SNSTopicArn": OTHER_TOPIC_ARN,
                        "BackupVaultEvents": ["BACKUP_JOB_FAILED", "COPY_JOB_FAILED"]
                    }
                })
            },
            policy: Some(|| {
                policy_for(
                    "backup:CopyIntoBackupVault",
                    CHANGE_REGION,
                    CHANGED_VAULT,
                )
            }),
            notifications: Some((OTHER_TOPIC_ARN, &["BACKUP_JOB_FAILED", "COPY_JOB_FAILED"])),
            lock: Some((14, 730)),
            tags: &[("Owner", "platform"), ("Tier", "gold")],
        },
        Step {
            case: "a deployment that DROPS the policy, the notifications and the lock, and \
                   re-values the tag set",
            properties: || json!({"BackupVaultTags": {"Tier": "silver"}}),
            policy: None,
            notifications: None,
            lock: None,
            tags: &[("Tier", "silver")],
        },
        Step {
            case: "a deployment that states a policy again on a vault carrying none",
            properties: || {
                json!({
                    "BackupVaultTags": {"Tier": "silver"},
                    "AccessPolicy": policy_for(
                        "backup:ListRecoveryPointsByBackupVault",
                        CHANGE_REGION,
                        CHANGED_VAULT,
                    )
                })
            },
            policy: Some(|| {
                policy_for(
                    "backup:ListRecoveryPointsByBackupVault",
                    CHANGE_REGION,
                    CHANGED_VAULT,
                )
            }),
            notifications: None,
            lock: None,
            tags: &[("Tier", "silver")],
        },
    ]
}

/// The template one step of the change table deploys: the vault under the one name every
/// step gives it, with that step's properties.
fn changed_template(properties: &Value) -> String {
    let mut declared = serde_json::Map::new();
    declared.insert(
        "BackupVaultName".to_string(),
        Value::String(CHANGED_VAULT.to_string()),
    );
    if let Some(members) = properties.as_object() {
        for (key, value) in members {
            declared.insert(key.clone(), value.clone());
        }
    }
    json!({
        "Resources": {
            VAULT_LOGICAL_ID: {
                "Type": "AWS::Backup::BackupVault",
                "Properties": Value::Object(declared)
            }
        }
    })
    .to_string()
}

/// Every step: a changed declaration reaches the vault AWS Backup ALREADY holds — the same
/// physical resource throughout — and a configuration the deployment no longer states is
/// removed from it rather than left behind.
#[tokio::test]
async fn a_changed_vault_configuration_reaches_the_vault_in_place() {
    let router = build_router();
    let region = CHANGE_REGION;
    let stack_name = "backup-vault-changes";

    let mut physical: Option<String> = None;
    for (index, step) in steps().into_iter().enumerate() {
        let case = step.case;
        let template = changed_template(&(step.properties)());
        let (action, wanted) = if index == 0 {
            ("CreateStack", "CREATE_COMPLETE")
        } else {
            ("UpdateStack", "UPDATE_COMPLETE")
        };
        deploy(
            &router, case, region, action, stack_name, &template, wanted,
        )
        .await;

        // An in-place change keeps the resource: the id the stack publishes is the one the
        // create published.
        let vault = vault_physical_id(&router, case, region, stack_name).await;
        assert_eq!(
            vault, CHANGED_VAULT,
            "case {case:?}: expected the vault's physical id to be {CHANGED_VAULT:?}, got \
             {vault:?}"
        );
        if let Some(created) = &physical {
            assert_eq!(
                &vault, created,
                "case {case:?}: expected the change to keep the physical id {created:?}, got \
                 {vault:?}"
            );
        }
        physical = Some(vault.clone());

        let expected_policy = step.policy.map(|policy| policy());
        assert_access_policy(&router, case, region, &vault, expected_policy.as_ref()).await;
        assert_notifications(&router, case, region, &vault, step.notifications).await;
        assert_lock(&router, case, region, &vault, step.lock).await;
        assert_tags(&router, case, region, stack_name, &vault, step.tags).await;
    }
}

/// The vault's NAME is no configuration of it: AWS Backup publishes no call that renames a
/// vault, so a deployment that states another name REPLACES the resource — and the vault it
/// replaces is kept where the declaration's `UpdateReplacePolicy` says to keep it.
#[tokio::test]
async fn a_renamed_vault_is_replaced_and_the_retained_one_survives() {
    let router = build_router();
    let region = "us-east-2";
    let stack_name = "backup-vault-renamed";
    let first = "cfn-vault-before";
    let second = "cfn-vault-after";

    let template = |name: &str| {
        json!({
            "Resources": {
                VAULT_LOGICAL_ID: {
                    "Type": "AWS::Backup::BackupVault",
                    "UpdateReplacePolicy": "Retain",
                    "Properties": {
                        "BackupVaultName": name,
                        "AccessPolicy": policy_for(
                            "backup:DescribeBackupVault",
                            region,
                            name,
                        )
                    }
                }
            }
        })
        .to_string()
    };

    let case = "the create of the vault that will be replaced";
    deploy(
        &router,
        case,
        region,
        "CreateStack",
        stack_name,
        &template(first),
        "CREATE_COMPLETE",
    )
    .await;
    assert_eq!(
        vault_physical_id(&router, case, region, stack_name).await,
        first,
        "case {case:?}: expected the vault to be created under {first:?}"
    );

    let case = "the deployment that states another name";
    deploy(
        &router,
        case,
        region,
        "UpdateStack",
        stack_name,
        &template(second),
        "UPDATE_COMPLETE",
    )
    .await;
    assert_eq!(
        vault_physical_id(&router, case, region, stack_name).await,
        second,
        "case {case:?}: expected the stack to publish the replacement vault {second:?}"
    );

    // The replacement is a vault of its own, carrying the policy the declaration states for
    // it...
    let (status, described) =
        backup(&router, region, "GET", &format!("/backup-vaults/{second}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeBackupVault on {second:?}, got {status} \
         with body {described}"
    );
    assert_access_policy(
        &router,
        case,
        region,
        second,
        Some(&policy_for("backup:DescribeBackupVault", region, second)),
    )
    .await;

    // ...and the vault it replaced is still there, because the declaration said to keep it.
    let (status, retained) =
        backup(&router, region, "GET", &format!("/backup-vaults/{first}")).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the replaced vault {first:?} to be RETAINED, the read answered \
         {status} with body {retained}"
    );
}
