// SPDX-License-Identifier: CC0-1.0

//! Which rule a PERFORMED backup job's `Backup Job State Change` envelope reaches, when the
//! rule scopes itself by a member of the event `detail`.
//!
//! RULE under test: a backup job's move is published on the account's default EventBridge bus
//! as `source` `aws.backup` / `detail-type` `Backup Job State Change`, carrying the job's own
//! stored members as the event `detail`, and EventBridge routes that envelope by its CONTENT —
//! so a rule whose `EventPattern` constrains a member of `detail` receives the envelopes whose
//! detail carries the value it names and NONE of an otherwise identical job's whose detail
//! carries another. That is how a client drives per-vault, per-resource automation
//! (notification, compliance, downstream copy) off one shared bus: the scoping has to hold at
//! the `detail` level, not only on `source` and `detail-type`.
//!
//! The rows differ along what the rule generalises over: WHICH `detail` member the pattern
//! names — the vault the job wrote into by ARN, the same vault by NAME, and the protected
//! resource the job captured — each value read off the resource's own create rather than
//! spelled here, with a second vault holding a second table whose job must reach the rule's
//! target not at all.
//!
//! Every job here runs under the role AWS's own instructions have a client provision: trusted
//! by `backup.amazonaws.com` with the published service-role policy ATTACHED, so what is
//! routed is a backup that was really performed and a `COMPLETED` move rather than a failure.
//! `tests/all/backup_job_under_the_published_service_role.rs` owns WHAT such a job comes to
//! and the table over the documents in force on its role;
//! `tests/all/job_state_change_events.rs` owns the table over `source`/`detail-type` scoping
//! and the published `detail` of every job kind. What this suite adds is the routing decision
//! taken INSIDE `detail`, with two jobs that differ only in the member the pattern names.
//!
//! Sources. The envelope is in no Smithy model — AWS Backup's EventBridge notifications carry
//! `"source": "aws.backup"` with `"detail-type": "Backup Job State Change"` — so every
//! `detail` member asserted here is compared against what `DescribeBackupJob` reports for the
//! same job (`smithy_query(query="operation", operation="DescribeBackupJob",
//! model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")`,
//! whose `State` is the enum `BackupJobState`). That a queue target's delivery is decided by
//! the queue's own resource policy is what `PutTargets` documents ("for Lambda functions,
//! Amazon SNS topics and Amazon SQS queues, EventBridge relies on resource-based policies"),
//! which is why the queue grants `events.amazonaws.com` the send under an `aws:SourceArn`
//! condition naming the delivering rule and nothing else.
//!
//! Every case drives real HTTP through the router: the Backup wire is the shared harness in
//! `vault_and_backup_job.rs`, and Amazon DynamoDB (which holds the protected tables), IAM
//! (which holds the role and the documents that decide what it may do), EventBridge and Amazon
//! SQS are mounted beside Backup on ONE shared engine, because each of those decisions is put
//! over the emulator's own in-process callback.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_state_change_events::{
    delivered, delivered_of_type, events_in, given_unpolicied_queue, set_queue_policy,
    BACKUP_JOB_STATE_CHANGE, COMPLETED, EVENT_SOURCE,
};
use crate::plan_and_selection_refusals::{create_role, form_value};
use crate::restore_job::source_table;
use crate::vault_and_backup_job::{
    create_vault, settled_backup_job, start_job, string_at, ACCOUNT, REGION,
};

/// The policy AWS publishes for the service role a backup is performed under, which is what
/// AWS's own instructions have a client attach to that role.
const PUBLISHED_BACKUP_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSBackupServiceRolePolicyForBackup";

/// The service principal a backup role trusts, as AWS Backup assumes it to perform the job.
const BACKUP_PRINCIPAL: &str = "backup.amazonaws.com";

/// The service principal EventBridge delivers a rule's target as, and the condition key a
/// grant written for it is scoped by: the ARN of the RULE the delivery was made for.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// Backup, DynamoDB, IAM, EventBridge and Amazon SQS over ONE shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_backup_cryocontrollerusermanager::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// Attach the AWS-managed policy `policy_arn` to the role `role_name`, through IAM's own
/// awsQuery wire — the call AWS's own instructions have a client make.
async fn attach_role_policy(router: &MultiServiceRouter, role_name: &str, policy_arn: &str) {
    let body = format!(
        "Action=AttachRolePolicy&Version=2010-05-08&RoleName={}&PolicyArn={}",
        form_value(role_name),
        form_value(policy_arn)
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let xml = String::from_utf8(response.into_body().to_vec()).expect("a UTF-8 response body");
    assert_eq!(
        status, 200,
        "AttachRolePolicy {policy_arn:?} on {role_name:?} answered {status} {xml}"
    );
}

/// The role every job of this suite runs under: trusted by AWS Backup and carrying the
/// published service-role policy, so the backup is really performed.
async fn published_backup_role(router: &MultiServiceRouter, role_name: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": BACKUP_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let arn = create_role(router, "/service-role/", role_name, &trust).await;
    attach_role_policy(router, role_name, PUBLISHED_BACKUP_POLICY).await;
    arn
}

/// What a case knows about the job the rule is scoped to.
struct Target {
    vault: String,
    vault_arn: String,
    table_arn: String,
}

/// One row of the table: which member of the event `detail` the rule scopes itself by, and the
/// value it names for the TARGET job.
struct Row {
    case: &'static str,
    member: &'static str,
    /// The value the pattern names, read off the target job's own identity.
    value: fn(&Target) -> String,
}

/// The `detail` members a client scopes such a rule by: the vault it watches (by ARN and by
/// name) and the protected resource it watches.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "scoped to one vault's ARN",
            member: "backupVaultArn",
            value: |target| target.vault_arn.clone(),
        },
        Row {
            case: "scoped to one vault's name",
            member: "backupVaultName",
            value: |target| target.vault.clone(),
        },
        Row {
            case: "scoped to one protected resource",
            member: "resourceArn",
            value: |target| target.table_arn.clone(),
        },
    ]
}

/// A rule whose pattern constrains a member of the event `detail` receives the matching job's
/// envelope and none of the other job's.
#[tokio::test]
async fn a_detail_scoped_rule_receives_only_the_matching_jobs_envelope() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let role_arn = published_backup_role(&router, "detail-scope-role").await;

        // Two vaults holding two tables: the one the rule is scoped to, and the one whose
        // otherwise identical job must reach the rule's target not at all.
        let mut targets = Vec::new();
        for which in ["target", "other"] {
            let vault = format!("detail-scope-{which}");
            let (status, created) = create_vault(&router, &vault, "{}").await;
            assert_eq!(
                status, 200,
                "case {case:?}: CreateBackupVault {vault:?} answered {status} {created}"
            );
            let table_arn =
                source_table(&router, &format!("detail-scope-{which}-table"), false).await;
            targets.push(Target {
                vault_arn: string_at(&created, "BackupVaultArn"),
                vault,
                table_arn,
            });
        }
        let scoped = (row.value)(&targets[0]);

        // The rule, and the queue that admits its deliveries and nothing else's.
        let rule = format!("detail-scope-rule-{index}");
        let mut scoped_detail = serde_json::Map::new();
        scoped_detail.insert(row.member.to_string(), json!([&scoped]));
        let pattern = json!({
            "source": [EVENT_SOURCE],
            "detail-type": [BACKUP_JOB_STATE_CHANGE],
            "detail": Value::Object(scoped_detail),
        });
        let (status, put) = events_in(
            &router,
            REGION,
            "PutRule",
            json!({ "Name": &rule, "State": "ENABLED", "EventPattern": pattern.to_string() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutRule answered {status} {put}"
        );
        let rule_arn = string_at(&put, "RuleArn");
        assert_eq!(
            rule_arn,
            format!("arn:aws:events:{REGION}:{ACCOUNT}:rule/{rule}"),
            "case {case:?}: the rule ARN a grant is written against, got {put}"
        );
        let (url, queue_arn) =
            given_unpolicied_queue(&router, case, &format!("detail-scope-queue-{index}")).await;
        let mut condition = serde_json::Map::new();
        condition.insert(SOURCE_ARN_KEY.to_string(), json!(&rule_arn));
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": EVENTS_PRINCIPAL },
                "Action": "sqs:SendMessage",
                "Resource": &queue_arn,
                "Condition": { "ArnEquals": condition },
            }],
        })
        .to_string();
        set_queue_policy(&router, case, &url, &policy).await;
        let (status, put) = events_in(
            &router,
            REGION,
            "PutTargets",
            json!({ "Rule": &rule, "Targets": [{ "Id": "sqs-target", "Arn": &queue_arn }] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutTargets answered {status} {put}"
        );

        // The other vault's job is started FIRST, so an envelope that leaked would already be
        // waiting by the time the target job's own envelope arrives.
        let mut job_ids = Vec::new();
        for target in [&targets[1], &targets[0]] {
            let (status, started) = start_job(
                &router,
                &json!({
                    "BackupVaultName": &target.vault,
                    "ResourceArn": &target.table_arn,
                    "IamRoleArn": &role_arn,
                })
                .to_string(),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: StartBackupJob in {:?} answered {status} {started}",
                target.vault
            );
            job_ids.push(string_at(&started, "BackupJobId"));
        }
        let (other_job, target_job) = (&job_ids[0], &job_ids[1]);

        // What the target received, looked for again until the transition the clock owes has
        // been published rather than after a fixed span.
        let mut envelopes =
            delivered_of_type(&router, case, &url, BACKUP_JOB_STATE_CHANGE, 1).await;
        // Both jobs have settled by now (the target job's envelope is here), so a leak of the
        // other job's envelope would be on the queue too: one further drain collects it.
        envelopes.extend(delivered(&router, case, &url).await);
        let ids: Vec<&str> = envelopes
            .iter()
            .filter_map(|event| event["detail"]["backupJobId"].as_str())
            .collect();
        assert!(
            ids.contains(&target_job.as_str()),
            "case {case:?}: expected the envelope of the scoped job {target_job:?}, \
             got {envelopes:?}"
        );
        assert!(
            !ids.contains(&other_job.as_str()),
            "case {case:?}: expected no envelope for the other vault's job {other_job:?}, \
             got {envelopes:?}"
        );
        for event in &envelopes {
            assert_eq!(
                event["source"],
                json!(EVENT_SOURCE),
                "case {case:?}: every envelope names AWS Backup as its source, got {event}"
            );
            assert_eq!(
                event["detail"][row.member],
                json!(&scoped),
                "case {case:?}: every envelope carries the scoped {} value, got {event}",
                row.member
            );
            assert_eq!(
                event["detail"]["state"],
                json!(COMPLETED),
                "case {case:?}: a performed backup publishes its COMPLETED move, got {event}"
            );
        }
        // The published detail is the job's own stored state, not a second copy of it.
        let described = settled_backup_job(&router, case, target_job).await;
        let event = envelopes
            .iter()
            .find(|event| event["detail"]["backupJobId"] == json!(target_job))
            .unwrap_or_else(|| panic!("case {case:?}: no envelope for {target_job:?}"));
        for (member, reported) in [
            ("backupVaultArn", "BackupVaultArn"),
            ("resourceArn", "ResourceArn"),
            ("recoveryPointArn", "RecoveryPointArn"),
            ("iamRoleArn", "IamRoleArn"),
        ] {
            assert_eq!(
                event["detail"][member], described[reported],
                "case {case:?}: detail.{member} must be what DescribeBackupJob reports for \
                 {reported}, got {event} against {described}"
            );
        }
    }
}
