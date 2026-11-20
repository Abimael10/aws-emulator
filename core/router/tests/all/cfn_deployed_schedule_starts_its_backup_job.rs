// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed EventBridge rule that fires on its own SCHEDULE really produces the
//! AWS Backup job its target asks for — and what the DEPLOYED roles may do decides whether that
//! happens at all.
//!
//! The RULE pinned here: **a rule carrying a `ScheduleExpression` self-triggers on that cadence
//! ("rules with `ScheduleExpressions` self-trigger based on the given schedule", `PutRule`),
//! with nothing published to the bus, and EventBridge invokes each of its targets ACTING AS the
//! role that target declares ("EventBridge relies on IAM roles that you specify in the `RoleARN`
//! argument in `PutTargets`") — so for a state-machine target the firing is a real
//! `states:StartExecution`, the execution runs its definition as the machine's own `roleArn`,
//! and a `Task` state whose `Resource` is `arn:aws:states:::aws-sdk:backup:startBackupJob`
//! performs AWS Backup's `StartBackupJob`: the job it creates is readable through AWS Backup's
//! own `ListBackupJobs`/`DescribeBackupJob` for the vault the request named and settles to its
//! terminal state there. The chain is made of permissions throughout: the identity-based
//! policies in force on the role a call is made as decide that call, so an execution role that
//! may not perform `backup:StartBackupJob` has its task refused, the execution ends `FAILED`,
//! and NOTHING the refused call would have created exists afterwards.** Whether the topology was
//! wired by a client's own API calls or PROVISIONED FROM A TEMPLATE changes none of it: the
//! `AWS::Events::Rule` provider installs the declared schedule, target ARN and target role, so
//! the deployed rule fires exactly as a hand-made one does.
//!
//! Sources: the schedule's self-triggering and the role a target is invoked as (`PutRule`,
//! `PutTargets`); the AWS SDK integration family of a `Task` state's `Resource`
//! (`arn:aws:states:::aws-sdk:<sdk name>:<action>`), whose call is that service's own API
//! operation; the members of that call and of its answer, from AWS Backup's own model
//! (`smithy_query(query="operation", operation="StartBackupJob",
//! model_path="services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json")`
//! — `BackupVaultName`/`ResourceArn`/`IamRoleArn` in, `BackupJobId`/`RecoveryPointArn` out, and
//! `ListBackupJobs`' own `ByBackupVaultName` filter for the read); the action an invoked
//! operation publishes ("Actions defined by AWS Backup", IAM Service Authorization Reference);
//! and the states a job can take (`smithy_query(query="shape",
//! shape="com.amazonaws.backup#BackupJobState")`).
//!
//! Six crates have to agree for that to hold — CloudFormation makes every create the template
//! declares, IAM holds the three roles and decides what each may do, EventBridge fires the rule
//! on its schedule and makes the handover as the target's role, Step Functions runs the
//! execution and makes the task's call, AWS Backup serves `StartBackupJob` and runs the job, and
//! Amazon DynamoDB holds the table the job protects — so the suite lives here rather than in any
//! one crate, and it runs over the WHOLE `build_router` service set the binary serves.
//!
//! The rows of the first table differ along exactly what the rule generalises over: the FORM the
//! `ScheduleExpression` states the cadence in (`rate` with a singular and with a plural unit, and
//! `cron` naming its own instants), each of which has its first firing fall due the same span
//! after the rule can reach a target. The second table differs along what the policies in force
//! on the EXECUTION role say about the task's one call — the action it needs, and another action
//! of the same service that grants nothing about it — with both chains deployed by ONE template
//! so the permitted chain is the positive control that says the stack's schedule really fired.
//!
//! What is NOT asserted here, because it is asserted elsewhere:
//! `crate::events_schedule_rule_starts_its_state_machine` pins the scheduled-event envelope and
//! the dead letter of a target whose role cannot make the handover,
//! `crate::backup_event_rule_target_runs_its_backup_task` pins the same backup chain driven by a
//! PUBLISHED event and the document each input member renders,
//! `crate::backup_event_rule_target_permission_decides_its_backup_task` pins the whole table of
//! what an execution role's policies decide about that call, and
//! `services/service-com-amazonaws-backup-cryocontrollerusermanager/tests/all/caller_authorization.rs`
//! pins the refusal class and sentence AWS Backup's own front door answers an unauthorized
//! caller with. What only this suite reaches is the path the issue describes: a TEMPLATE's vault,
//! table, roles, state machine and scheduled rule, nothing published, and a real AWS Backup job
//! at the far end.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The chain's own callers and waiters, from the suite that owns them: the assembled router, the
// service callers, the readers that wait for an execution and for a job to settle, and the
// principals and actions the chain is made of. `tests/all/main.rs` is one crate root, so this
// suite borrows that wiring rather than restating it.
use crate::backup_event_rule_target_runs_its_backup_task::{
    backup, events, executions_once_started, job_once_settled, member, once_terminal, router,
    BACKUP_PRINCIPAL, COMPLETED, DYNAMODB_TYPE, EVENTS_PRINCIPAL, LOOK_INTERVAL, RUN_BUDGET,
    START_BACKUP_JOB_ACTION, START_BACKUP_JOB_TASK, START_EXECUTION_ACTION, STATES_PRINCIPAL,
};
// The shared stack waiter, the awsQuery CloudFormation caller and the reader of a stack's
// outputs this test crate writes ONCE rather than per suite. They sign for the same Region the
// callers above do.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{cloudformation, CREATE_COMPLETE};

/// The `source` and `detail-type` EventBridge publishes a SCHEDULED firing under — what says the
/// document the execution was started with came from the rule's own schedule rather than from
/// anything a client published. Spelled out here rather than imported from the service crate, so
/// a wrong constant in the implementation cannot make this suite pass.
const SCHEDULED_SOURCE: &str = "aws.events";
const SCHEDULED_DETAIL_TYPE: &str = "Scheduled Event";

/// The action AWS Backup performs UNDER a job's own role to create the recovery point of a
/// DynamoDB table: of the DynamoDB grants AWS publishes for the backup role (AWS Backup *Managed
/// policies*, `AWSBackupServiceRolePolicyForBackup`), the one action that CREATES the recovery
/// point.
const CREATE_BACKUP_ACTION: &str = "dynamodb:CreateBackup";

/// Another action of AWS Backup, published under the same prefix ("Actions defined by AWS
/// Backup", IAM Service Authorization Reference): a READ of a job, which grants nothing about
/// starting one.
const ANOTHER_BACKUP_ACTION: &str = "backup:DescribeBackupJob";

/// The two terminal `ExecutionStatus` values a single-task execution reaches, and the rule state
/// a deployed schedule is put in force under.
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";
const ENABLED: &str = "ENABLED";

/// The placeholders the declared definition carries, each filled in by the template's own
/// `DefinitionSubstitutions` ("specify values for the placeholder variables in your state
/// machine definition") — how a template puts the ARN of a resource it ALSO declares into a
/// `Task` state's parameters.
const VAULT_PLACEHOLDER: &str = "VaultName";
const RESOURCE_PLACEHOLDER: &str = "ResourceArn";
const ROLE_PLACEHOLDER: &str = "BackupRoleArn";

/// A trust policy naming one service principal, which is what lets that service act as the role.
fn trust(service: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// An `AWS::IAM::Role` `Policies` list carrying one inline document that allows `action`.
fn policies(action: &str) -> Value {
    json!([{
        "PolicyName": "granted",
        "PolicyDocument": {
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Allow", "Action": action, "Resource": "*" }],
        },
    }])
}

/// A role declaration: trusted by `service`, allowed `action`, named `name`.
fn role(name: &str, service: &str, action: &str) -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "RoleName": name,
            "AssumeRolePolicyDocument": trust(service),
            "Policies": policies(action),
        },
    })
}

/// The Amazon States Language document every machine below runs: ONE `Task` state performing
/// `StartBackupJob` with the three members the template substitutes in.
fn definition() -> String {
    json!({
        "StartAt": "StartBackupJob",
        "States": {
            "StartBackupJob": {
                "Type": "Task",
                "Resource": START_BACKUP_JOB_TASK,
                "Parameters": {
                    "BackupVaultName": format!("${{{VAULT_PLACEHOLDER}}}"),
                    "ResourceArn": format!("${{{RESOURCE_PLACEHOLDER}}}"),
                    "IamRoleArn": format!("${{{ROLE_PLACEHOLDER}}}"),
                },
                "End": true,
            },
        },
    })
    .to_string()
}

/// A state-machine declaration whose task backs up the declared table, as the declared backup
/// role, into the vault declared under the logical id `vault`.
fn state_machine(name: &str, execution_role: &str, vault: &str) -> Value {
    json!({
        "Type": "AWS::StepFunctions::StateMachine",
        "Properties": {
            "StateMachineName": name,
            "StateMachineType": "STANDARD",
            "RoleArn": { "Fn::GetAtt": [execution_role, "Arn"] },
            "DefinitionString": definition(),
            "DefinitionSubstitutions": {
                VAULT_PLACEHOLDER: { "Ref": vault },
                RESOURCE_PLACEHOLDER: { "Fn::GetAtt": ["Table", "Arn"] },
                ROLE_PLACEHOLDER: { "Fn::GetAtt": ["BackupRole", "Arn"] },
            },
        },
    })
}

/// A backup vault declaration under the name `name`.
fn vault(name: &str) -> Value {
    json!({
        "Type": "AWS::Backup::BackupVault",
        "Properties": { "BackupVaultName": name },
    })
}

/// The protected resource every template declares: a DynamoDB table an owning service really
/// holds.
fn table(name: &str) -> Value {
    json!({
        "Type": "AWS::DynamoDB::Table",
        "Properties": {
            "TableName": name,
            "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
        },
    })
}

/// A scheduled rule declaration: it fires on `schedule` and hands each of `targets` over as the
/// declared target role.
fn scheduled_rule(name: &str, schedule: &str, targets: Value) -> Value {
    json!({
        "Type": "AWS::Events::Rule",
        "Properties": {
            "Name": name,
            "State": ENABLED,
            "ScheduleExpression": schedule,
            "Targets": targets,
        },
    })
}

/// One target declaration: the id it is installed under, the machine it starts and the deployed
/// role EventBridge makes that start as.
fn target(id: &str, machine: &str) -> Value {
    json!({
        "Id": id,
        "Arn": { "Ref": machine },
        "RoleArn": { "Fn::GetAtt": ["TargetRole", "Arn"] },
    })
}

/// Deploy `template` as `stack` and answer the `DescribeStacks` answer it reported
/// `CREATE_COMPLETE` in.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    template: &str,
) -> String {
    cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            // "If your stack template contains IAM resources with custom names, you must specify
            // CAPABILITY_NAMED_IAM" — every stack below names its roles.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    let (status, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "case {case:?}: expected the stack to deploy, got {status} with {described}"
    );
    described
}

/// One output of a deployed stack, or a panic naming the case and what the stack published.
fn output(case: &str, described: &str, key: &str) -> String {
    outputs_of(described)
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the stack to publish {key}: {described}"))
}

/// Every backup job AWS Backup holds in `vault` right now, read through the filter a client
/// polling for a scheduled backup uses.
async fn jobs_in_vault(router: &MultiServiceRouter, case: &str, vault: &str) -> Vec<Value> {
    let listed = backup(
        router,
        case,
        "GET",
        &format!("/backup-jobs?backupVaultName={vault}"),
        None,
    )
    .await;
    listed["BackupJobs"].as_array().cloned().unwrap_or_default()
}

/// The first job AWS Backup holds in `vault` once it holds one, looking again until it does or
/// [`RUN_BUDGET`] has passed with it holding none.
async fn job_in_vault_once_listed(
    router: &MultiServiceRouter,
    case: &str,
    vault: &str,
) -> Value {
    let deadline = Instant::now() + RUN_BUDGET;
    loop {
        let held = jobs_in_vault(router, case, vault).await;
        if let Some(job) = held.first() {
            return job.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the deployed schedule to create a backup job in {vault}, got \
             {held:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The one execution of the machine at `machine_arn`, waited for and then read in its terminal
/// status.
async fn terminal_execution(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
) -> Value {
    let started = executions_once_started(router, case, machine_arn).await;
    let execution_arn = member(case, &started[0], "/executionArn");
    once_terminal(router, case, &execution_arn).await
}

/// Check that the document `input` is the scheduled-event envelope EventBridge publishes itself —
/// what says NOTHING a client published started this execution.
fn assert_started_by_the_schedule(case: &str, input: &str) {
    let envelope: Value = serde_json::from_str(input).unwrap_or_else(|error| {
        panic!("case {case:?}: expected the execution's input to be JSON ({error}), got {input:?}")
    });
    assert_eq!(
        envelope.get("source").and_then(Value::as_str),
        Some(SCHEDULED_SOURCE),
        "case {case:?}: expected the scheduled source, got {envelope}"
    );
    assert_eq!(
        envelope.get("detail-type").and_then(Value::as_str),
        Some(SCHEDULED_DETAIL_TYPE),
        "case {case:?}: expected the scheduled detail-type, got {envelope}"
    );
}

/// One row of the first table: the FORM the schedule states its cadence in, and the slug the
/// row's own resources are named after.
struct Row {
    case: &'static str,
    slug: &'static str,
    schedule: &'static str,
}

/// Every form a `ScheduleExpression` states a cadence in. Which INSTANTS each form names is the
/// EventBridge service crate's own reading — a `rate(...)` states an interval and no instant, so
/// its first firing falls inside the first period, while a `cron(...)` NAMES its instants and
/// fires at the first one ahead of the clock — so the cron row names an instant in every minute,
/// the finest resolution that format has, rather than one a row would have to wait out the rest
/// of an hour for. What a row here pins is the chain that first firing drives.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a rate schedule stated in a singular unit",
            slug: "rate-hour",
            schedule: "rate(1 hour)",
        },
        Row {
            case: "a rate schedule stated in a plural unit",
            slug: "rate-minutes",
            schedule: "rate(5 minutes)",
        },
        Row {
            case: "a cron schedule naming its own instants",
            slug: "cron-minutely",
            // The six-field, minute-resolution form `ScheduleExpression` documents ("Minutes
            // Hours Day-of-month Month Day-of-week Year"), with `?` for the day of the week:
            // its instants fall at the top of every minute.
            schedule: "cron(* * * * ? *)",
        },
    ]
}

/// The whole chain in ONE template: the vault the backup is taken into, the table it protects,
/// the role it is performed under, the state machine that performs it, the role that machine
/// runs as, the role EventBridge makes the handover as, and the scheduled rule itself with its
/// target declared inline.
fn chain_template(row: &Row) -> String {
    let names = |kind: &str| format!("cfn-sched-backup-{}-{kind}", row.slug);
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a scheduled EventBridge rule that starts an AWS Backup job",
        "Resources": {
            "Vault": vault(&names("vault")),
            "Table": table(&names("table")),
            "BackupRole": role(&names("backup-role"), BACKUP_PRINCIPAL, CREATE_BACKUP_ACTION),
            "ExecutionRole": role(
                &names("exec-role"),
                STATES_PRINCIPAL,
                START_BACKUP_JOB_ACTION,
            ),
            "TargetRole": role(&names("target-role"), EVENTS_PRINCIPAL, START_EXECUTION_ACTION),
            "Machine": state_machine(&names("machine"), "ExecutionRole", "Vault"),
            "Rule": scheduled_rule(
                &names("rule"),
                row.schedule,
                json!([target("BackupAutomation", "Machine")]),
            ),
        },
        "Outputs": {
            "VaultName": { "Value": { "Ref": "Vault" } },
            "TableArn": { "Value": { "Fn::GetAtt": ["Table", "Arn"] } },
            "BackupRoleArn": { "Value": { "Fn::GetAtt": ["BackupRole", "Arn"] } },
            "TargetRoleArn": { "Value": { "Fn::GetAtt": ["TargetRole", "Arn"] } },
            "MachineArn": { "Value": { "Ref": "Machine" } },
            "RuleName": { "Value": { "Ref": "Rule" } },
        },
    })
    .to_string()
}

/// RULE: a deployed rule carrying a `ScheduleExpression` fires on its own, starts its
/// state-machine target as the DEPLOYED target role, and the execution's task really performs
/// `StartBackupJob` — so the job is there in AWS Backup for the deployed vault and settles to its
/// terminal state, whichever form the schedule was stated in.
#[tokio::test]
async fn a_cfn_deployed_schedule_starts_the_backup_job_its_target_asks_for() {
    // ONE emulator for the whole table: every row names its own stack and its own resources
    // inside it, so no row can see another's.
    let router = router();
    for row in rows() {
        let case = row.case;
        let stack = format!("cfn-sched-backup-{}", row.slug);
        let described = deployed(&router, case, &stack, &chain_template(&row)).await;

        let vault_name = output(case, &described, "VaultName");
        let table_arn = output(case, &described, "TableArn");
        let backup_role_arn = output(case, &described, "BackupRoleArn");
        let target_role_arn = output(case, &described, "TargetRoleArn");
        let machine_arn = output(case, &described, "MachineArn");
        let rule_name = output(case, &described, "RuleName");

        // ---- The rule EventBridge holds is the declared one: the schedule it fires on, the
        // state it is in force in, and the deployed role its target is invoked as.
        let held = events(&router, case, "DescribeRule", &json!({ "Name": rule_name })).await;
        assert_eq!(
            held.get("ScheduleExpression").and_then(Value::as_str),
            Some(row.schedule),
            "case {case:?}: expected the declared schedule to be in force, got {held}"
        );
        assert_eq!(
            held.get("State").and_then(Value::as_str),
            Some(ENABLED),
            "case {case:?}: expected the rule to be enabled, got {held}"
        );
        let installed = events(
            &router,
            case,
            "ListTargetsByRule",
            &json!({ "Rule": rule_name }),
        )
        .await;
        let targets = installed["Targets"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            targets.len(),
            1,
            "case {case:?}: expected the declared target to be installed, got {installed}"
        );
        assert_eq!(
            member(case, &targets[0], "/Arn"),
            machine_arn,
            "case {case:?}: expected the target to name the deployed machine, got {installed}"
        );
        assert_eq!(
            member(case, &targets[0], "/RoleArn"),
            target_role_arn,
            "case {case:?}: expected the target to carry the deployed role, got {installed}"
        );

        // ---- The schedule fires with nothing published, and what it started is an execution of
        // the deployed machine carrying EventBridge's own scheduled-event document.
        let terminal = terminal_execution(&router, case, &machine_arn).await;
        assert_started_by_the_schedule(case, &member(case, &terminal, "/input"));
        assert_eq!(
            terminal.get("status").and_then(Value::as_str),
            Some(SUCCEEDED),
            "case {case:?}: expected the scheduled execution to succeed, got {terminal}"
        );

        // ---- And the job the task started is AWS Backup's own: in the deployed vault, of the
        // deployed table, performed under the deployed backup role, settling COMPLETED.
        let job = job_in_vault_once_listed(&router, case, &vault_name).await;
        for (field, expected) in [
            ("BackupVaultName", vault_name.clone()),
            ("ResourceArn", table_arn.clone()),
            ("IamRoleArn", backup_role_arn.clone()),
            ("ResourceType", DYNAMODB_TYPE.to_string()),
        ] {
            assert_eq!(
                job.get(field).and_then(Value::as_str),
                Some(expected.as_str()),
                "case {case:?}: expected {field} {expected:?} on the scheduled job, got {job}"
            );
        }
        let job_id = member(case, &job, "/BackupJobId");
        let settled = job_once_settled(&router, case, &job_id).await;
        assert_eq!(
            settled.get("State").and_then(Value::as_str),
            Some(COMPLETED),
            "case {case:?}: expected the scheduled job to complete, got {settled}"
        );
    }
}

/// The same chain TWICE in one template, differing only in what the execution role of each may
/// do: one may perform the task's call, the other may only read a job.
fn permission_template() -> String {
    let names = |kind: &str| format!("cfn-sched-backup-grant-{kind}");
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "one schedule, two backup chains, one of them under-permissioned",
        "Resources": {
            "Vault": vault(&names("granted-vault")),
            "DeniedVault": vault(&names("denied-vault")),
            "Table": table(&names("table")),
            "BackupRole": role(&names("backup-role"), BACKUP_PRINCIPAL, CREATE_BACKUP_ACTION),
            "ExecutionRole": role(
                &names("exec-role"),
                STATES_PRINCIPAL,
                START_BACKUP_JOB_ACTION,
            ),
            "DeniedExecutionRole": role(
                &names("denied-exec-role"),
                STATES_PRINCIPAL,
                ANOTHER_BACKUP_ACTION,
            ),
            "TargetRole": role(&names("target-role"), EVENTS_PRINCIPAL, START_EXECUTION_ACTION),
            "Machine": state_machine(&names("machine"), "ExecutionRole", "Vault"),
            "DeniedMachine": state_machine(
                &names("denied-machine"),
                "DeniedExecutionRole",
                "DeniedVault",
            ),
            "Rule": scheduled_rule(
                &names("rule"),
                // One firing each, far enough apart that the row reads the first.
                "rate(1 hour)",
                json!([
                    target("GrantedAutomation", "Machine"),
                    target("DeniedAutomation", "DeniedMachine"),
                ]),
            ),
        },
        "Outputs": {
            "VaultName": { "Value": { "Ref": "Vault" } },
            "DeniedVaultName": { "Value": { "Ref": "DeniedVault" } },
            "MachineArn": { "Value": { "Ref": "Machine" } },
            "DeniedMachineArn": { "Value": { "Ref": "DeniedMachine" } },
        },
    })
    .to_string()
}

/// RULE: the task's call is made as the state machine's own role and judged on the policies in
/// force on it, so a deployed chain whose execution role may not perform the AWS Backup action
/// has the call refused: the scheduled execution ends `FAILED` and its vault holds no job — while
/// the permitted chain of the SAME stack, fired by the SAME rule, holds its own.
#[tokio::test]
async fn a_deployed_target_whose_role_may_not_start_the_backup_fails_its_execution() {
    let case = "one deployed schedule driving a permitted and an under-permissioned chain";
    let router = router();
    let stack = "cfn-sched-backup-grants";
    let described = deployed(&router, case, stack, &permission_template()).await;

    let vault_name = output(case, &described, "VaultName");
    let denied_vault = output(case, &described, "DeniedVaultName");
    let machine_arn = output(case, &described, "MachineArn");
    let denied_machine_arn = output(case, &described, "DeniedMachineArn");

    // The permitted chain is this stack's positive control: it says the deployed schedule really
    // fired, so the refused chain holding nothing is a refusal rather than a look taken early.
    let granted = terminal_execution(&router, case, &machine_arn).await;
    assert_eq!(
        granted.get("status").and_then(Value::as_str),
        Some(SUCCEEDED),
        "case {case:?}: expected the permitted chain's execution to succeed, got {granted}"
    );
    let job = job_in_vault_once_listed(&router, case, &vault_name).await;
    let settled = job_once_settled(&router, case, &member(case, &job, "/BackupJobId")).await;
    assert_eq!(
        settled.get("State").and_then(Value::as_str),
        Some(COMPLETED),
        "case {case:?}: expected the permitted chain's job to complete, got {settled}"
    );

    // The under-permissioned chain was started by the same firing, and its task's call was
    // refused: the execution ends FAILED carrying the error the refusal raised.
    let denied = terminal_execution(&router, case, &denied_machine_arn).await;
    assert_eq!(
        denied.get("status").and_then(Value::as_str),
        Some(FAILED),
        "case {case:?}: expected the under-permissioned execution to fail, got {denied}"
    );
    let reported = denied.get("error").and_then(Value::as_str).unwrap_or_default();
    assert!(
        !reported.is_empty(),
        "case {case:?}: expected the failed execution to name the error it failed under, got \
         {denied}"
    );

    // And nothing the refused call would have created exists: no job in the vault it addressed.
    let held = jobs_in_vault(&router, case, &denied_vault).await;
    assert!(
        held.is_empty(),
        "case {case:?}: expected the refused call to create no backup job in {denied_vault}, got \
         {held:?}"
    );
}
