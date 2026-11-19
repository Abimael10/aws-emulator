// SPDX-License-Identifier: CC0-1.0

//! What the identity-based policies in force on a state machine's EXECUTION ROLE decide about
//! the AWS SDK integration call its `Task` state makes — read over the whole path a client
//! automates: an event on the EventBridge bus, the rule's state-machine target, the execution
//! that target starts, and AWS Backup at the far end.
//!
//! RULE: Step Functions makes a `Task` state's integration call acting as the state machine's
//! `roleArn`, however that execution was started, and the call is judged by the access request
//! the integration itself forms — the IAM action the invoked operation publishes
//! (`backup:StartBackupJob`, "Actions defined by AWS Backup", IAM Service Authorization
//! Reference) over the ARN of the resource the request addresses (the vault the backup is
//! taken INTO, named by `StartBackupJobInput$BackupVaultName`, whose own ARN AWS Backup
//! reports when the vault is created). A call those policies do not allow is NOT made: the
//! task fails under the invoked service's refusal, the execution ends `FAILED`, and NOTHING
//! the call would have created exists afterwards — no backup job for the protected resource
//! and no recovery point in the vault. A call they DO allow is made, and the job it started is
//! AWS Backup's own.
//!
//! The error a failed `Task` reports for a refused AWS SDK integration call is the invoked
//! service followed by the name of the exception that service's SDK raises for the answer:
//! "the error names ... are prefixed with the service name and are based on the AWS SDK for
//! Java exception names" is the convention Step Functions documents for this family, and the
//! names it illustrates it with (`Lambda.AWSLambdaException`,
//! `DynamoDB.AmazonDynamoDBException`) are that SDK's exception CLASSES rather than wire error
//! codes — a class exists per error the service MODELS, and every other answer is raised as
//! the service's base exception. `StartBackupJob` models no authorization error of its own
//! (`smithy_query(query="operation", operation="StartBackupJob",
//! model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")`
//! lists `InvalidParameterValueException`, `InvalidRequestException`,
//! `LimitExceededException`, `MissingParameterValueException`, `ResourceNotFoundException` and
//! `ServiceUnavailableException` and nothing about authorization), so an access-denied answer
//! from it has no class of its own and is raised as AWS Backup's base exception. WHICH
//! exception that is per service is read once, in
//! `services/service-com-amazonaws-sfn-awsstepfunctions/src/state/integration.rs` (the `Api`
//! entry's `sdk_exception`), and is pinned for this very call by that crate's
//! `tests/all/task_target_authorization.rs`; this suite holds the chain's end to that one
//! reading rather than to a second one. The refusal CLASS underneath the name — the
//! access-denied answer AWS Backup's own front door gives a caller it does not grant, which
//! the cause's status and sentence carry — is pinned where that gate lives, in
//! `services/service-com-amazonaws-backup-cryocontrollerusermanager/tests/all/caller_authorization.rs`.
//!
//! The rows differ along what the rule generalises over: WHAT the policies in force on the
//! execution role say about that one access request — nothing about the service at all,
//! another action of it, the action over another vault's ARN, the action granted and explicitly
//! denied, the action over the ARN the request addresses, and every action of the service by
//! wildcard — which is the only thing that differs between two otherwise identical chains.
//! Each row drives its own rule, its own target, its own machine and its own vault, so the
//! permitted and refused chains stand side by side in one table.
//!
//! Five crates have to agree for that to hold — IAM holds the roles and the documents in force
//! on them, EventBridge matches the envelope and makes the handover as the target's role, Step
//! Functions runs the execution and decides the task's call, AWS Backup serves
//! `StartBackupJob` and holds (or does not hold) the job, and Amazon DynamoDB holds the table
//! the backup would protect — so the suite lives here and runs over the whole `build_router`
//! service set the binary serves.
//!
//! The chain's wiring is `crate::backup_event_rule_target_runs_its_backup_task`'s, borrowed
//! rather than restated; that suite pins the chain under a role that may make the call, and
//! `services/service-com-amazonaws-backup-cryocontrollerusermanager/tests/all/caller_authorization.rs`
//! pins AWS Backup's own gate for a call made directly against it.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The chain's own wiring, from the suite that owns it: the assembled router, the service
// callers, the roles, the protected table, the state machine and the readers that wait for an
// execution and for a job to settle.
use crate::backup_event_rule_target_runs_its_backup_task::{
    backup, backup_state_machine, events, executions_once_started, job_once_settled, member,
    once_terminal, protected_table, role, role_trusted_by, router, BACKUP_PRINCIPAL, COMPLETED,
    DYNAMODB_TYPE, EVENTS_PRINCIPAL, LOOK_INTERVAL, QUIET_WINDOW, START_BACKUP_JOB_ACTION,
    START_EXECUTION_ACTION, STATES_PRINCIPAL, TEST_SOURCE,
};

/// The error a failed `Task` of the AWS SDK integration family reports an unmodelled refusal
/// of an AWS Backup call under: the service as that SDK's client names it, and the base
/// exception an answer no modelled error shape matches deserializes to — the tree's one
/// reading of both, cited in the module docs.
const REFUSAL_ERROR: &str = "Backup.AWSBackupException";

/// Another action of the invoked service, published under the same prefix: a read of a job
/// ("Actions defined by AWS Backup", IAM Service Authorization Reference), which grants
/// nothing about starting one.
const ANOTHER_BACKUP_ACTION: &str = "backup:DescribeBackupJob";

/// Every action of the invoked service, by the wildcard an IAM document may write a service's
/// actions with.
const EVERY_BACKUP_ACTION: &str = "backup:*";

/// WHAT the policies in force on the execution role say about the task's one access request —
/// the dimension the rows differ along.
#[derive(Debug, Clone, Copy)]
enum Granted {
    /// Nothing about the invoked service at all: the role may only have the execution started.
    NothingOfTheService,
    /// Another action of the invoked service, over every resource.
    AnotherActionOfTheService,
    /// The action the call needs, but over ANOTHER vault's ARN.
    TheActionOnAnotherVault,
    /// The action over every resource, and an explicit `Deny` of it beside the grant.
    TheActionAndAnExplicitDeny,
    /// The action over the ARN the request addresses.
    TheActionOnTheVaultAddressed,
    /// Every action of the invoked service, over every resource.
    EveryActionOfTheService,
}

impl Granted {
    /// The statements this row's execution-role policy carries BESIDE the one that lets
    /// EventBridge start the execution, where `vault` is the ARN of the vault the task's
    /// request addresses and `other` the ARN of a second vault it does not.
    fn statements(self, vault: &str, other: &str) -> Vec<Value> {
        let allow = |action: &str, resource: &str| {
            json!({ "Effect": "Allow", "Action": action, "Resource": resource })
        };
        match self {
            Self::NothingOfTheService => Vec::new(),
            Self::AnotherActionOfTheService => vec![allow(ANOTHER_BACKUP_ACTION, "*")],
            Self::TheActionOnAnotherVault => vec![allow(START_BACKUP_JOB_ACTION, other)],
            Self::TheActionAndAnExplicitDeny => vec![
                allow(START_BACKUP_JOB_ACTION, "*"),
                json!({
                    "Effect": "Deny",
                    "Action": START_BACKUP_JOB_ACTION,
                    "Resource": "*",
                }),
            ],
            Self::TheActionOnTheVaultAddressed => vec![allow(START_BACKUP_JOB_ACTION, vault)],
            Self::EveryActionOfTheService => vec![allow(EVERY_BACKUP_ACTION, "*")],
        }
    }
}

/// WHY the documents in force withhold the call, which is the clause IAM's own refusal
/// sentence ends with.
#[derive(Debug, Clone, Copy)]
enum Because {
    /// No readable statement allows the action: IAM's default deny, which names the action
    /// nothing granted.
    NoPolicyAllows,
    /// An explicit `Deny` in one of the role's own documents, which is a fact about that
    /// document rather than about the action.
    AnExplicitDeny,
}

impl Because {
    /// The clause itself, for the action the task's call asks for.
    fn clause(self) -> String {
        match self {
            Self::NoPolicyAllows => format!(
                "because no identity-based policy allows the {START_BACKUP_JOB_ACTION} action"
            ),
            Self::AnExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        }
    }
}

/// What the chain comes to: the call is refused for a reason, or it is made.
#[derive(Debug, Clone, Copy)]
enum Outcome {
    Refused(Because),
    Performed,
}

/// One row: the case, the `detail-type` only this row's own rule and event carry, what the
/// execution role is granted, and what the chain must come to.
struct Row {
    case: &'static str,
    detail_type: &'static str,
    granted: Granted,
    outcome: Outcome,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role granted nothing of the invoked service",
            detail_type: "BackupRequestedNoGrant",
            granted: Granted::NothingOfTheService,
            outcome: Outcome::Refused(Because::NoPolicyAllows),
        },
        Row {
            case: "a role granted another action of the invoked service",
            detail_type: "BackupRequestedOtherAction",
            granted: Granted::AnotherActionOfTheService,
            outcome: Outcome::Refused(Because::NoPolicyAllows),
        },
        Row {
            case: "a role granted the action over another vault's ARN",
            detail_type: "BackupRequestedOtherVault",
            granted: Granted::TheActionOnAnotherVault,
            outcome: Outcome::Refused(Because::NoPolicyAllows),
        },
        Row {
            case: "a role granted the action and explicitly denied it",
            detail_type: "BackupRequestedExplicitDeny",
            granted: Granted::TheActionAndAnExplicitDeny,
            outcome: Outcome::Refused(Because::AnExplicitDeny),
        },
        Row {
            case: "a role granted the action over the vault the request addresses",
            detail_type: "BackupRequestedVaultGrant",
            granted: Granted::TheActionOnTheVaultAddressed,
            outcome: Outcome::Performed,
        },
        Row {
            case: "a role granted every action of the invoked service",
            detail_type: "BackupRequestedWildcardGrant",
            granted: Granted::EveryActionOfTheService,
            outcome: Outcome::Performed,
        },
    ]
}

/// The identity-based document the row's execution role carries: the handover EventBridge
/// makes as it, plus whatever this row grants about the task's own call.
fn execution_role_policy(granted: Granted, vault: &str, other: &str) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": START_EXECUTION_ACTION,
        "Resource": "*",
    })];
    statements.extend(granted.statements(vault, other));
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// Create the vault `name` and answer the ARN AWS BACKUP ITSELF reports for it, so the
/// resource the access request is read over is the owning service's own value rather than an
/// ARN format written out here.
async fn vault(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = backup(
        router,
        case,
        "PUT",
        &format!("/backup-vaults/{name}"),
        Some(&json!({})),
    )
    .await;
    member(case, &created, "/BackupVaultArn")
}

/// The backup jobs AWS Backup holds for the resource `resource_arn`, through its own listing.
async fn jobs_for(
    router: &MultiServiceRouter,
    case: &str,
    resource_arn: &str,
) -> Vec<Value> {
    let listed = backup(
        router,
        case,
        "GET",
        &format!("/backup-jobs?resourceArn={resource_arn}"),
        None,
    )
    .await;
    listed["BackupJobs"].as_array().cloned().unwrap_or_default()
}

/// The recovery points the vault `name` holds, through AWS Backup's own listing.
async fn recovery_points(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Vec<Value> {
    let held = backup(
        router,
        case,
        "GET",
        &format!("/backup-vaults/{name}/recovery-points"),
        None,
    )
    .await;
    held["RecoveryPoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// RULE: the policies in force on the execution role decide whether the `Task` state's AWS
/// Backup call is made at all, and a call they withhold leaves the execution `FAILED` and AWS
/// Backup holding nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_execution_roles_policies_decide_the_targets_backup_task() {
    // ONE emulator for the whole table: every row names its own vault, table, roles, rule and
    // state machine, and its rule matches a `detail-type` only its own event carries, so no
    // row can see another's resources or another's handover.
    let router = router();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let vault_name = format!("permission-vault-{index}");
        let vault_arn = vault(&router, case, &vault_name).await;
        // A second vault, which no row's task addresses: it is what the row granted the action
        // over ANOTHER resource is granted it over.
        let other_vault_arn = vault(&router, case, &format!("permission-other-{index}")).await;
        let resource =
            protected_table(&router, case, &format!("permission-table-{index}")).await;

        // The role the BACKUP would be performed under, which every row grants in full: what
        // the rows vary is the role the CALL is made as, below.
        let job_role = role(
            &router,
            &format!("permission-backup-{index}"),
            BACKUP_PRINCIPAL,
            &["*"],
        )
        .await;
        // The one role of the chain: EventBridge starts the execution as it, and Step
        // Functions makes the task's call as it, so it is trusted by both services and its
        // document is what this row varies.
        let chain_role = role_trusted_by(
            &router,
            &format!("permission-chain-{index}"),
            &[EVENTS_PRINCIPAL, STATES_PRINCIPAL],
            &execution_role_policy(row.granted, &vault_arn, &other_vault_arn),
        )
        .await;

        let machine = backup_state_machine(
            &router,
            case,
            &format!("permission-machine-{index}"),
            &chain_role,
            ("$.vault", "$.resource", "$.role"),
        )
        .await;

        let rule_name = format!("permission-rule-{index}");
        events(
            &router,
            case,
            "PutRule",
            &json!({
                "Name": &rule_name,
                "EventPattern": json!({
                    "source": [TEST_SOURCE],
                    "detail-type": [row.detail_type],
                })
                .to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let wired = events(
            &router,
            case,
            "PutTargets",
            &json!({
                "Rule": &rule_name,
                "Targets": [{
                    "Id": "to-state-machine",
                    "Arn": &machine,
                    // EventBridge starts a state machine AS the role the target names.
                    "RoleArn": &chain_role,
                    // The document the execution is started with, which the task reads its
                    // three members out of.
                    "Input": json!({
                        "vault": &vault_name,
                        "resource": &resource,
                        "role": &job_role,
                    })
                    .to_string(),
                }],
            }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {case:?}: the state-machine target must be accepted, got {wired}"
        );

        let published = events(
            &router,
            case,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": TEST_SOURCE,
                    "DetailType": row.detail_type,
                    "Detail": json!({}).to_string(),
                }],
            }),
        )
        .await;
        assert_eq!(
            published["FailedEntryCount"],
            json!(0),
            "case {case:?}: the event must be accepted, got {published}"
        );

        // The handover happens whatever the task's call comes to: what the role may do decides
        // the CALL, not whether the execution is started.
        let started = executions_once_started(&router, case, &machine).await;
        assert_eq!(
            started.len(),
            1,
            "case {case:?}: one matched event starts one execution, got {started:?}"
        );
        let execution_arn = member(case, &started[0], "/executionArn");
        let described = once_terminal(&router, case, &execution_arn).await;

        match row.outcome {
            Outcome::Refused(because) => {
                assert_eq!(
                    described["status"],
                    json!("FAILED"),
                    "case {case:?}: expected the refused task to fail the execution, got \
                     {described}"
                );
                assert_eq!(
                    described["error"],
                    json!(REFUSAL_ERROR),
                    "case {case:?}: expected the task to fail under {REFUSAL_ERROR}, got \
                     {described}"
                );
                // The cause is IAM's own sentence: who was refused, the action the invoked
                // operation publishes, the ARN the request addressed, and which of IAM's rules
                // withheld it. The session the call was made as is minted per execution, so
                // what is held here is the ROLE it is a session of.
                let cause = member(case, &described, "/cause");
                let role_name = chain_role
                    .rsplit('/')
                    .next()
                    .expect("a role ARN ends with the role's name")
                    .to_string();
                let sentence = format!(
                    "is not authorized to perform: {START_BACKUP_JOB_ACTION} on resource: \
                     {vault_arn} {}",
                    because.clause()
                );
                assert!(
                    cause.contains(&sentence),
                    "case {case:?}: expected the cause to state {sentence:?}, got {cause:?}"
                );
                assert!(
                    cause.contains(&format!("assumed-role/{role_name}/")),
                    "case {case:?}: expected the cause to blame a session of the execution role \
                     {role_name:?}, got {cause:?}"
                );

                // And the far end: a call that was not made created nothing. Looked for over a
                // window longer than the span a job of this emulator takes to appear, so a job
                // created late would still be caught.
                let deadline = Instant::now() + QUIET_WINDOW;
                loop {
                    let jobs = jobs_for(&router, case, &resource).await;
                    assert!(
                        jobs.is_empty(),
                        "case {case:?}: a refused call must leave no backup job for \
                         {resource:?}, got {jobs:?}"
                    );
                    let points = recovery_points(&router, case, &vault_name).await;
                    assert!(
                        points.is_empty(),
                        "case {case:?}: a refused call must leave the vault {vault_name:?} \
                         empty, got {points:?}"
                    );
                    if Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(LOOK_INTERVAL).await;
                }
            }
            Outcome::Performed => {
                assert_eq!(
                    described["status"],
                    json!("SUCCEEDED"),
                    "case {case:?}: expected the permitted task to succeed, got {described}"
                );
                let output: Value = serde_json::from_str(&member(case, &described, "/output"))
                    .unwrap_or_else(|error| {
                        panic!(
                            "case {case:?}: the execution's output must be JSON ({error}): \
                             {described}"
                        )
                    });
                let job_id = member(case, &output, "/BackupJobId");
                let recovery_point = member(case, &output, "/RecoveryPointArn");
                // The job the call started is AWS Backup's own, readable through its API and
                // settling to its terminal state there.
                let read = job_once_settled(&router, case, &job_id).await;
                for (pointer, expect) in [
                    ("/BackupVaultName", json!(&vault_name)),
                    ("/ResourceArn", json!(&resource)),
                    ("/ResourceType", json!(DYNAMODB_TYPE)),
                    ("/IamRoleArn", json!(&job_role)),
                    ("/RecoveryPointArn", json!(&recovery_point)),
                    ("/State", json!(COMPLETED)),
                ] {
                    let actual = read.pointer(pointer).cloned().unwrap_or(Value::Null);
                    assert_eq!(
                        actual, expect,
                        "case {case:?}: expected the stored job's {pointer} to be {expect}, got \
                         {actual} in {read}"
                    );
                }
                // The same job through the listing a client reads, keyed by the resource the
                // automation protected.
                let listed: Vec<String> = jobs_for(&router, case, &resource)
                    .await
                    .iter()
                    .map(|job| job["BackupJobId"].as_str().unwrap_or_default().to_string())
                    .collect();
                assert!(
                    listed.contains(&job_id),
                    "case {case:?}: expected AWS Backup to list the job the task started \
                     ({job_id:?}) for {resource:?}, got {listed:?}"
                );
                let held: Vec<String> = recovery_points(&router, case, &vault_name)
                    .await
                    .iter()
                    .map(|point| {
                        point["RecoveryPointArn"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect();
                assert!(
                    held.contains(&recovery_point),
                    "case {case:?}: expected the vault to hold the recovery point the task \
                     reported ({recovery_point:?}), got {held:?}"
                );
            }
        }
    }
}

/// RULE, the other half of what it means for the POLICIES to decide: two chains that differ in
/// nothing but the document in force on their execution role come to different ends, and the
/// refused one leaves the permitted one's work untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_chains_differing_only_in_their_roles_policy_end_differently() {
    const CASE: &str = "one bus, one vault, one table, two roles";
    const PERMITTED: &str = "BackupPairPermitted";
    const REFUSED: &str = "BackupPairRefused";

    let router = router();
    let vault_name = "pair-vault";
    let vault_arn = vault(&router, CASE, vault_name).await;
    let other_vault_arn = vault(&router, CASE, "pair-other").await;
    let resource = protected_table(&router, CASE, "pair-table").await;
    let job_role = role(&router, "pair-backup", BACKUP_PRINCIPAL, &["*"]).await;

    // The pair: identical definitions, identical targets, identical events — one role granted
    // the task's call over the vault addressed, one granted nothing of the service.
    let mut chains: Vec<(&str, String)> = Vec::new();
    for (detail_type, granted) in [
        (PERMITTED, Granted::TheActionOnTheVaultAddressed),
        (REFUSED, Granted::NothingOfTheService),
    ] {
        let chain_role = role_trusted_by(
            &router,
            &format!("pair-chain-{detail_type}"),
            &[EVENTS_PRINCIPAL, STATES_PRINCIPAL],
            &execution_role_policy(granted, &vault_arn, &other_vault_arn),
        )
        .await;
        let machine = backup_state_machine(
            &router,
            CASE,
            &format!("pair-machine-{detail_type}"),
            &chain_role,
            ("$.vault", "$.resource", "$.role"),
        )
        .await;
        events(
            &router,
            CASE,
            "PutRule",
            &json!({
                "Name": format!("pair-rule-{detail_type}"),
                "EventPattern": json!({
                    "source": [TEST_SOURCE],
                    "detail-type": [detail_type],
                })
                .to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        events(
            &router,
            CASE,
            "PutTargets",
            &json!({
                "Rule": format!("pair-rule-{detail_type}"),
                "Targets": [{
                    "Id": "to-state-machine",
                    "Arn": &machine,
                    "RoleArn": &chain_role,
                    "Input": json!({
                        "vault": vault_name,
                        "resource": &resource,
                        "role": &job_role,
                    })
                    .to_string(),
                }],
            }),
        )
        .await;
        // Both chains are wired before either event is published, so the two handovers are
        // in flight together and neither waits on the other's end.
        events(
            &router,
            CASE,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": TEST_SOURCE,
                    "DetailType": detail_type,
                    "Detail": json!({}).to_string(),
                }],
            }),
        )
        .await;
        chains.push((detail_type, machine));
    }

    let mut ends: Vec<(&str, Value)> = Vec::new();
    for (detail_type, machine) in &chains {
        let started = executions_once_started(&router, CASE, machine).await;
        assert_eq!(
            started.len(),
            1,
            "case {CASE:?}: one matched event starts one execution of the {detail_type} \
             chain, got {started:?}"
        );
        let execution_arn = member(CASE, &started[0], "/executionArn");
        ends.push((
            detail_type,
            once_terminal(&router, CASE, &execution_arn).await,
        ));
    }
    for (detail_type, described) in &ends {
        let expected = if *detail_type == PERMITTED {
            "SUCCEEDED"
        } else {
            "FAILED"
        };
        assert_eq!(
            described["status"],
            json!(expected),
            "case {CASE:?}: expected the {detail_type} chain to end {expected}, got {described}"
        );
    }

    // ONE job for the table — the permitted chain's — so the refused chain added nothing and
    // took nothing away.
    let jobs = jobs_for(&router, CASE, &resource).await;
    assert_eq!(
        jobs.len(),
        1,
        "case {CASE:?}: expected exactly the permitted chain's job for {resource:?}, got {jobs:?}"
    );
    let points = recovery_points(&router, CASE, vault_name).await;
    assert_eq!(
        points.len(),
        1,
        "case {CASE:?}: expected exactly the permitted chain's recovery point in \
         {vault_name:?}, got {points:?}"
    );
}
