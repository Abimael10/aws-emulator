// SPDX-License-Identifier: CC0-1.0

//! The least-privilege contract for `backup:StartBackupJob` read the way a CLIENT watches it:
//! the same role's documents decide a call the client makes itself with that role's session
//! credential AND the call a Step Functions `Task` makes acting as that role after an
//! EventBridge rule handed the execution over, and each outcome is readable through the owning
//! service's own listing.
//!
//! RULE: a `backup:StartBackupJob` call is decided by the identity-based policies in force on
//! the principal that MAKES it — the assumed-role session a client signs with, or the state
//! machine's `roleArn` when Step Functions makes the call — read over the ARN of the resource
//! the request addresses, which for this operation is the vault the backup is taken INTO
//! (`StartBackupJobInput$BackupVaultName`, whose ARN AWS Backup reports when the vault is
//! created). A call those policies do not allow is NOT made: the client's own call is refused
//! with IAM's identity-based denial sentence, the task's call fails its execution, and in
//! neither case does AWS Backup hold a job for the protected resource. A call they DO allow is
//! made, and the job it started is AWS Backup's own.
//!
//! What this suite adds to the two beside it
//! (`crate::backup_event_rule_target_runs_its_backup_task`, which pins the chain under a role
//! that may make the call, and
//! `crate::backup_event_rule_target_permission_decides_its_backup_task`, which pins what the
//! execution role's documents decide about the task's call over a wider row set) is the
//! OBSERVABILITY the contract is checked through, in two tests:
//!
//! 1. the call a CLIENT makes itself, holding a credential `sts:AssumeRole` minted through the
//!    router rather than one seeded into a store — so the principal the refusal names is the
//!    ARN STS ITSELF reported for that mint — over the table of what the documents in force on
//!    that one role say about the single access request the call forms;
//! 2. the execution an EventBridge rule started, looked up the way a client polls for it:
//!    through `ListExecutions` SCOPED BY `statusFilter`, which must report the execution under
//!    the terminal status it reached and under no other (`ExecutionStatus`,
//!    `smithy_query(query="shape", shape="com.amazonaws.sfn#ExecutionStatus",
//!    model_path="services/service-com-amazonaws-sfn-awsstepfunctions/smithy-model/com-amazonaws-sfn-awsstepfunctions.json")`),
//!    beside AWS Backup's holding read through `ListBackupJobs` scoped `ByResourceArn` — empty
//!    for a withheld call, exactly the one job for a permitted one. That test carries the two
//!    rows the two terminal statuses need and no more, because the row set over the task's own
//!    decision is the sibling suite's.
//!
//! The rows differ along what the rule generalises over: WHAT the documents in force on the one
//! role say about that single access request — nothing about the service at all, another action
//! of it, the action over the ARN the request addresses, and every action of the service by
//! wildcard. Each row drives its own vault, its own protected table, its own role, its own
//! state machine and its own rule, matched on a `detail-type` only that row's event carries, so
//! no row can see another's resources or another's handover.
//!
//! The access-denied CLASS and status AWS Backup's front door answers an unauthorized caller
//! with are that service's own, pinned where that gate lives
//! (`services/service-com-amazonaws-backup-cryocontrollerusermanager/tests/all/caller_authorization.rs`);
//! this suite holds the chain's two ends to that one reading rather than to a second one. The
//! error name a failed `Task` reports for a refused AWS SDK integration call is pinned by
//! `crate::backup_event_rule_target_permission_decides_its_backup_task`, so it is not restated
//! here.
//!
//! Six crates have to agree for this to hold — IAM holds the role and the documents, STS mints
//! the session and names its principal, EventBridge matches the envelope and makes the
//! handover, Step Functions runs the execution and decides the task's call, AWS Backup serves
//! `StartBackupJob` and holds (or does not hold) the job, and Amazon DynamoDB holds the tables
//! the backups would protect — so the suite lives here and runs over the whole `build_router`
//! service set the binary serves. The chain's wiring is
//! `crate::backup_event_rule_target_runs_its_backup_task`'s, borrowed rather than restated.

use std::time::Instant;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::backup_event_rule_target_runs_its_backup_task::{
    backup, backup_state_machine, events, executions_once_started, member, once_terminal,
    protected_table, role, router, sfn, BACKUP_PRINCIPAL, EVENTS_PRINCIPAL, LOOK_INTERVAL,
    QUIET_WINDOW, REGION, START_BACKUP_JOB_ACTION, START_EXECUTION_ACTION, STATES_PRINCIPAL,
    TEST_SOURCE,
};
use crate::query_wire::element;
use crate::sns_lambda_subscription_permission_boundary::iam;

/// The account every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`), which is the
/// account the role and the vault belong to.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The long-term key every emulator publishes: the account's own credential, which holds no IAM
/// policies, so the setup calls and the readings made with it are not what any row is about.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The API version an `sts:AssumeRole` form body names.
const STS_API_VERSION: &str = "2011-06-15";

/// The `RoleSessionName` every mint here asks for — the last segment of the assumed-role ARN a
/// refusal names.
const SESSION_NAME: &str = "least-privilege-probe";

/// The access-denied answer AWS Backup's front door gives a caller it does not grant, as the
/// suite that owns that gate pins it (cited in the module docs).
const DENIED_STATUS: u16 = 403;
const DENIED_CODE: &str = "AccessDeniedException";

/// Another action of the invoked service, published under the same prefix: a read of a job
/// ("Actions defined by AWS Backup", IAM Service Authorization Reference), which grants nothing
/// about starting one.
const ANOTHER_BACKUP_ACTION: &str = "backup:DescribeBackupJob";

/// Every action of the invoked service, by the wildcard an IAM document may write a service's
/// actions with.
const EVERY_BACKUP_ACTION: &str = "backup:*";

/// The terminal execution statuses the chain's rows expect, as the model's `ExecutionStatus`
/// spells them (cited in the module docs).
const FAILED: &str = "FAILED";
const SUCCEEDED: &str = "SUCCEEDED";

/// A SigV4 `Authorization` whose credential is `access_key_id` — the key a service reads the
/// caller's identity from — and whose scope names `signing_name`, which routes the request. The
/// signature is never checked: the router runs with request authentication off.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One AWS Backup request (`restJson1`) signed with `access_key_id`, with whatever it answered:
/// the rows read a refusal out of it, so it asserts no status of its own. The harness's own
/// `backup` helper signs as the account and asserts 200, which is what the readings below use;
/// WHO signs is the dimension this suite varies, so the request is built here.
async fn backup_as(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |body| Bytes::from(body.to_string()));
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://backup.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("backup.{REGION}.amazonaws.com"))
        .header("authorization", authorization(access_key_id, "backup"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Assume `role_arn` through STS and answer (the access key id of the session it minted, the
/// assumed-role ARN STS reports for it) — the principal every refusal of a request signed with
/// that key must name.
async fn assumed_session(
    router: &MultiServiceRouter,
    case: &str,
    role_arn: &str,
) -> (String, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "sts.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(AMBIENT_KEY, "sts"))
        .body(Bytes::from(format!(
            "Action=AssumeRole&Version={STS_API_VERSION}&RoleArn={role_arn}\
             &RoleSessionName={SESSION_NAME}"
        )))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from STS AssumeRole, got {status} with {answered}"
    );
    (element(&answered, "AccessKeyId"), element(&answered, "Arn"))
}

/// The trust document of the chain's one role: the two services that act as it, plus the
/// account root, which delegates to the whole account and is what lets a client of this account
/// assume it.
fn trust_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Principal": { "Service": [EVENTS_PRINCIPAL, STATES_PRINCIPAL] },
                "Action": "sts:AssumeRole",
            },
            {
                "Effect": "Allow",
                "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
                "Action": "sts:AssumeRole",
            },
        ],
    })
    .to_string()
}

/// Create the chain's role where IAM holds it, carrying `policy` as its inline document, and
/// answer the ARN IAM ITSELF reports for it.
async fn chain_role(router: &MultiServiceRouter, name: &str, policy: &Value) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_document()),
        ],
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "granted"),
            ("PolicyDocument", &policy.to_string()),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// Create the vault `name` and answer the ARN AWS BACKUP ITSELF reports for it, so the resource
/// the access request is read over is the owning service's own value rather than an ARN format
/// written out here.
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

/// The backup jobs AWS Backup holds for `resource_arn`, through its own listing scoped
/// `ByResourceArn`.
async fn jobs_for(router: &MultiServiceRouter, case: &str, resource_arn: &str) -> Vec<Value> {
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

/// The executions of the machine at `machine_arn` that Step Functions reports under
/// `statusFilter` — how a client polling for a terminal run selects it.
async fn executions_with_status(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    status: &str,
) -> Vec<Value> {
    let listed = sfn(
        router,
        case,
        "ListExecutions",
        &json!({ "stateMachineArn": machine_arn, "statusFilter": status }),
    )
    .await;
    listed["executions"].as_array().cloned().unwrap_or_default()
}

/// WHAT the documents in force on the one role say about the single access request the call
/// forms — the dimension the rows differ along.
#[derive(Debug, Clone, Copy)]
enum Granted {
    /// Nothing about the invoked service at all: the role may only have the execution started.
    NothingOfTheService,
    /// Another action of the invoked service, over every resource.
    AnotherActionOfTheService,
    /// The action the call needs, over the ARN the request addresses.
    TheActionOnTheVaultAddressed,
    /// Every action of the invoked service, over every resource.
    EveryActionOfTheService,
}

impl Granted {
    /// The statements this row's role carries BESIDE the one that lets EventBridge start the
    /// execution, where `vault` is the ARN of the vault the backup is taken into.
    fn statements(self, vault: &str) -> Vec<Value> {
        let allow = |action: &str, resource: &str| {
            json!({ "Effect": "Allow", "Action": action, "Resource": resource })
        };
        match self {
            Self::NothingOfTheService => Vec::new(),
            Self::AnotherActionOfTheService => vec![allow(ANOTHER_BACKUP_ACTION, "*")],
            Self::TheActionOnTheVaultAddressed => vec![allow(START_BACKUP_JOB_ACTION, vault)],
            Self::EveryActionOfTheService => vec![allow(EVERY_BACKUP_ACTION, "*")],
        }
    }

    /// Whether those statements withhold the call.
    fn withholds(self) -> bool {
        matches!(
            self,
            Self::NothingOfTheService | Self::AnotherActionOfTheService
        )
    }
}

/// The whole document a role carries: the handover EventBridge makes as it, plus whatever the
/// row grants about the backup call itself.
fn role_policy(granted: Granted, vault: &str) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": START_EXECUTION_ACTION,
        "Resource": "*",
    })];
    statements.extend(granted.statements(vault));
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The sentence IAM states an identity-based denial with, for a request no readable statement
/// allows: who was refused, the action asked for, the resource addressed, and the rule that
/// withheld the grant.
fn no_policy_allows(principal: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {START_BACKUP_JOB_ACTION} on \
         resource: {resource} because no identity-based policy allows the \
         {START_BACKUP_JOB_ACTION} action"
    )
}

/// The request `StartBackupJob` takes, for a backup of `resource` into `vault_name` performed
/// under `job_role`.
fn start_backup_job(vault_name: &str, resource: &str, job_role: &str) -> Value {
    json!({
        "BackupVaultName": vault_name,
        "ResourceArn": resource,
        "IamRoleArn": job_role,
    })
}

/// Everything one row needs: the vault the call addresses (and the ARN AWS Backup reports for
/// it), the protected resource, the role the backup is performed under, and the role the CALL
/// is made as, carrying this row's document.
struct Wiring {
    vault_name: String,
    vault_arn: String,
    resource: String,
    job_role: String,
    role_arn: String,
}

/// Wire one row up, under names only that row uses.
async fn wiring(
    router: &MultiServiceRouter,
    case: &str,
    prefix: &str,
    granted: Granted,
) -> Wiring {
    let vault_name = format!("{prefix}-vault");
    let vault_arn = vault(router, case, &vault_name).await;
    let resource = protected_table(router, case, &format!("{prefix}-table")).await;
    // The role the BACKUP would be performed under, which every row grants in full: what the
    // rows vary is the role the CALL is made as.
    let job_role = role(router, &format!("{prefix}-backup-role"), BACKUP_PRINCIPAL, &["*"]).await;
    let role_arn = chain_role(
        router,
        &format!("{prefix}-caller-role"),
        &role_policy(granted, &vault_arn),
    )
    .await;
    Wiring {
        vault_name,
        vault_arn,
        resource,
        job_role,
        role_arn,
    }
}

/// One row of the table over the CLIENT's own call.
struct Row {
    case: &'static str,
    granted: Granted,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role granted nothing of the invoked service",
            granted: Granted::NothingOfTheService,
        },
        Row {
            case: "a role granted another action of the invoked service",
            granted: Granted::AnotherActionOfTheService,
        },
        Row {
            case: "a role granted the action over the vault the request addresses",
            granted: Granted::TheActionOnTheVaultAddressed,
        },
        Row {
            case: "a role granted every action of the invoked service",
            granted: Granted::EveryActionOfTheService,
        },
    ]
}

/// RULE: a client's own `StartBackupJob` is decided by the identity-based policies in force on
/// the role whose session credential it signs with, read over the ARN of the vault it addresses
/// — and a refusal names the principal STS reported for that very mint.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_clients_own_call_is_decided_by_the_policies_in_force_on_its_role() {
    // ONE emulator for the whole table: every row names its own vault, table and roles.
    let router = router();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let wired = wiring(&router, case, &format!("client-{index}"), row.granted).await;
        let (session_key, session_arn) = assumed_session(&router, case, &wired.role_arn).await;

        let (status, answered) = backup_as(
            &router,
            &session_key,
            "PUT",
            "/backup-jobs",
            Some(&start_backup_job(
                &wired.vault_name,
                &wired.resource,
                &wired.job_role,
            )),
        )
        .await;

        if row.granted.withholds() {
            assert_eq!(
                status, DENIED_STATUS,
                "case {case:?}: expected the call to be refused with {DENIED_STATUS}, got \
                 {status} {answered}"
            );
            assert_eq!(
                answered["__type"],
                json!(DENIED_CODE),
                "case {case:?}: expected {DENIED_CODE}, got {answered}"
            );
            assert_eq!(
                answered["Message"],
                json!(no_policy_allows(&session_arn, &wired.vault_arn)),
                "case {case:?}: expected IAM's own sentence naming the session STS minted, \
                 got {answered}"
            );
            let held = jobs_for(&router, case, &wired.resource).await;
            assert!(
                held.is_empty(),
                "case {case:?}: expected the refused call to store no job, got {held:?}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the operation's own answer, got {status} {answered}"
            );
            // The call was MADE: AWS Backup holds the job it started, in the vault addressed.
            let held = jobs_for(&router, case, &wired.resource).await;
            assert_eq!(
                held.len(),
                1,
                "case {case:?}: expected exactly one job for the resource addressed, got \
                 {held:?}"
            );
            assert_eq!(
                held[0]["BackupVaultName"],
                json!(wired.vault_name),
                "case {case:?}: expected the job to be held in the vault addressed, got \
                 {held:?}"
            );
            assert_eq!(
                held[0]["BackupJobId"],
                answered["BackupJobId"],
                "case {case:?}: expected the listing to report the job the call answered \
                 with, got {held:?}"
            );
        }
    }
}

/// One row of the table over the chain: the `detail-type` only this row's own rule and event
/// carry, what its role is granted, the status its execution must reach and the status it must
/// NOT be reported under.
struct ChainRow {
    case: &'static str,
    detail_type: &'static str,
    granted: Granted,
    reached: &'static str,
    not_reached: &'static str,
}

/// The two rows the two terminal statuses need: the wider row set over what the execution
/// role's documents decide about the task's call is the sibling suite's (module docs).
fn chain_rows() -> Vec<ChainRow> {
    vec![
        ChainRow {
            case: "a role granted nothing of the invoked service",
            detail_type: "LeastPrivilegeNoGrant",
            granted: Granted::NothingOfTheService,
            reached: FAILED,
            not_reached: SUCCEEDED,
        },
        ChainRow {
            case: "a role granted the action over the vault the request addresses",
            detail_type: "LeastPrivilegeVaultGrant",
            granted: Granted::TheActionOnTheVaultAddressed,
            reached: SUCCEEDED,
            not_reached: FAILED,
        },
    ]
}

/// RULE: the execution an EventBridge rule started is reported by `ListExecutions` under the
/// terminal status its task's call brought it to and under no other, and AWS Backup holds a job
/// for the protected resource only when that call was actually made.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_rule_started_execution_is_reported_under_the_status_it_reached() {
    let router = router();
    for (index, row) in chain_rows().iter().enumerate() {
        let case = row.case;
        let wired = wiring(&router, case, &format!("chain-{index}"), row.granted).await;

        let machine = backup_state_machine(
            &router,
            case,
            &format!("chain-{index}-machine"),
            &wired.role_arn,
            ("$.vault", "$.resource", "$.role"),
        )
        .await;
        let rule_name = format!("chain-{index}-rule");
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
        let target = events(
            &router,
            case,
            "PutTargets",
            &json!({
                "Rule": &rule_name,
                "Targets": [{
                    "Id": "to-state-machine",
                    "Arn": &machine,
                    // EventBridge starts a state machine AS the role the target names.
                    "RoleArn": &wired.role_arn,
                    "Input": json!({
                        "vault": &wired.vault_name,
                        "resource": &wired.resource,
                        "role": &wired.job_role,
                    })
                    .to_string(),
                }],
            }),
        )
        .await;
        assert_eq!(
            target["FailedEntryCount"],
            json!(0),
            "case {case:?}: the state-machine target must be accepted, got {target}"
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
        assert_eq!(
            described["status"],
            json!(row.reached),
            "case {case:?}: expected the execution to reach {}, got {described}",
            row.reached
        );

        // The way a client finds that run: the listing SCOPED to the status it polls for
        // reports it, and the listing scoped to the other terminal status does not.
        let under_reached = executions_with_status(&router, case, &machine, row.reached).await;
        let arns: Vec<String> = under_reached
            .iter()
            .map(|listed| member(case, listed, "/executionArn"))
            .collect();
        assert!(
            arns.contains(&execution_arn),
            "case {case:?}: expected ListExecutions(statusFilter={}) to report \
             {execution_arn}, got {arns:?}",
            row.reached
        );
        let under_other = executions_with_status(&router, case, &machine, row.not_reached).await;
        assert!(
            under_other.is_empty(),
            "case {case:?}: expected ListExecutions(statusFilter={}) to report nothing, got \
             {under_other:?}",
            row.not_reached
        );

        if row.granted.withholds() {
            // Nothing the withheld call would have created exists, and nothing appears later
            // either: the listing is looked at again until the window closes.
            let deadline = Instant::now() + QUIET_WINDOW;
            loop {
                let held = jobs_for(&router, case, &wired.resource).await;
                assert!(
                    held.is_empty(),
                    "case {case:?}: expected the withheld task to create no backup job, got \
                     {held:?}"
                );
                if Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(LOOK_INTERVAL).await;
            }
        } else {
            let held = jobs_for(&router, case, &wired.resource).await;
            assert_eq!(
                held.len(),
                1,
                "case {case:?}: expected the permitted task to create exactly one backup job \
                 for the resource it addressed, got {held:?}"
            );
            assert_eq!(
                held[0]["BackupVaultName"],
                json!(wired.vault_name),
                "case {case:?}: expected the job to be held in the vault the task addressed, \
                 got {held:?}"
            );
        }
    }
}
