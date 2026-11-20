// SPDX-License-Identifier: CC0-1.0

//! An event an EventBridge rule matches STARTS the execution of the rule's state-machine
//! target, and that execution's `Task` state really performs AWS Backup's own
//! `StartBackupJob` — so the backup the automation asked for exists in AWS Backup
//! afterwards.
//!
//! The RULE pinned here: **EventBridge routes an event by its CONTENT, and for a
//! state-machine target the handover is `states:StartExecution` made as the role the target
//! names, started with the document that target's own input member renders (the envelope
//! itself where the target sets none, `Target$Input`'s constant where it sets one, and
//! `Target$InputTransformer`'s template — whose `<token>`s are the event's own values and
//! whose other text is the rule author's constants — where it sets that); the execution then
//! runs its definition, and a `Task` state whose `Resource` is
//! `arn:aws:states:::aws-sdk:backup:startBackupJob` performs AWS Backup's
//! `StartBackupJob` with the document the state built, so the execution reaches `SUCCEEDED`,
//! its result is that operation's own response, and the job it created is readable through
//! AWS Backup's own API and settles to its terminal state there. A rule whose `EventPattern`
//! does not match the event starts nothing, however many of its siblings on the same bus
//! do.** The event that drives the chain is any event the bus carries: one a CLIENT
//! published, and one AWS Backup itself published for a change to a resource of its own.
//!
//! Sources: the state-machine target's role and the call it makes, "for … Step Functions
//! state machines … EventBridge relies on IAM roles that you specify in the `RoleARN`
//! argument in `PutTargets`" (`PutTargets`); the three input members of a target
//! (`Target$Input` — "valid JSON text passed to the target. In this case, nothing from the
//! event itself is passed to the target", `Target$InputPath`, `Target$InputTransformer`);
//! the AWS SDK integration family of a `Task` state's `Resource`
//! (`arn:aws:states:::aws-sdk:<sdk name>:<action>`), whose call is that service's own API
//! operation; and for the members of the request and the result, AWS Backup's own model
//! (`smithy_query(query="operation", operation="StartBackupJob")`:
//! `BackupVaultName`/`ResourceArn`/`IamRoleArn` in, `BackupJobId`/`RecoveryPointArn` out).
//! The `detail-type` AWS Backup publishes a plan's mutations under is the bus's own, which
//! the backup Smithy model does not describe, so it is spelled out here rather than imported
//! from the service crate.
//!
//! Five crates have to agree for that to hold — IAM holds the three roles and decides what
//! each may do, EventBridge matches the envelope and makes the start as the target's role,
//! Step Functions runs the execution and makes the task's call, AWS Backup serves
//! `StartBackupJob` and runs the job, and Amazon DynamoDB holds the table the job protects —
//! so the suite lives here rather than in any one crate, and it runs over the WHOLE
//! `build_router` service set the binary serves.
//!
//! The rows differ along exactly what the rule generalises over: WHICH input member the
//! target shapes the execution's document with (a transformer rendering the whole document
//! out of the event's detail, no member at all so the envelope is handed over whole, a
//! constant `Input` carrying nothing of the event, and a transformer mixing the rule
//! author's constants with one value taken from the event), and WHO published the event the
//! rule matched (a client's own `PutEvents`, and AWS Backup's own publication for a backup
//! plan a client updated).
//!
//! What is NOT asserted here, because it is asserted elsewhere:
//! `services/service-com-amazonaws-sfn-awsstepfunctions/tests/all/backup_task_integration.rs`
//! pins what each AWS Backup action a `Task` may name does when the execution is started by
//! a client, `crate::backup_job_state_change_invokes_its_lambda_target` and
//! `crate::cfn_deployed_backup_pipeline_delivers_job_events` pin the envelope AWS Backup
//! publishes for a job's own moves and its delivery to Lambda, SNS and Amazon SQS targets,
//! and `crate::backup_plan_mutations_publish_plan_state_change_events` pins the envelope a
//! plan's mutation publishes. What only this suite reaches is the whole path: an event on
//! the bus, a state machine started for it with the document the target rendered, and a real
//! AWS Backup job at the far end.
//!
//! Every row here runs under a role that MAY start the backup, so what this suite holds fixed
//! is the chain itself. What the policies in force on that role decide about the task's call
//! is the question of `crate::backup_event_rule_target_permission_decides_its_backup_task`,
//! which borrows the harness below rather than restating the wiring — which is why the
//! harness is `pub(crate)`.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery IAM caller and the XML reader this test crate writes ONCE rather than per
// suite. They sign for the Region below, which is why this suite signs for it too.
use crate::query_wire::element;
use crate::sns_lambda_subscription_permission_boundary::iam;

/// The Region every request below is signed for — the one the shared IAM caller signs for.
pub(crate) const REGION: &str = "us-east-1";

/// The `source` a client's own events below carry, and the `source` AWS Backup publishes
/// under with the `detail-type` of a stored plan's mutation — spelled out here rather than
/// imported from the service crate, so a wrong constant in the implementation cannot make
/// this suite pass.
pub(crate) const TEST_SOURCE: &str = "sim2real.backup.automation";
const BACKUP_SOURCE: &str = "aws.backup";
const BACKUP_PLAN_STATE_CHANGE: &str = "Backup Plan State Change";

/// The service principals this chain acts as, and the two calls the roles grant: EventBridge
/// starts the execution, and the execution's task starts the backup job.
pub(crate) const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
pub(crate) const STATES_PRINCIPAL: &str = "states.amazonaws.com";
pub(crate) const BACKUP_PRINCIPAL: &str = "backup.amazonaws.com";
pub(crate) const START_EXECUTION_ACTION: &str = "states:StartExecution";
pub(crate) const START_BACKUP_JOB_ACTION: &str = "backup:StartBackupJob";

/// The `Task` `Resource` under test: the AWS SDK integration family's name for AWS Backup's
/// `StartBackupJob`.
pub(crate) const START_BACKUP_JOB_TASK: &str = "arn:aws:states:::aws-sdk:backup:startBackupJob";

/// The state a backup job of this emulator is stored in before the clock moves it, and the
/// terminal state a job performed under a permitted role reaches, as the model's
/// `BackupJobState` spells them
/// (`smithy_query(query="shape", shape="com.amazonaws.backup#BackupJobState")`).
const CREATED: &str = "CREATED";
pub(crate) const COMPLETED: &str = "COMPLETED";

/// The Backup resource type of the table every row protects, as the Backup API's own
/// `ByResourceType` catalog spells it (`ListBackupJobs`).
pub(crate) const DYNAMODB_TYPE: &str = "DynamoDB";

/// How long a row waits for the handover and the run it must see, how long the row that must
/// see NOTHING keeps looking for one, and how often either looks. Each is generous: nothing
/// here is a measurement of how fast the host runs. The wait covers a whole minute, because a
/// row whose rule carries a `cron(...)` schedule waits for an instant that expression NAMES —
/// at minute resolution, the finest that format has — and not for a span this emulator chooses.
pub(crate) const RUN_BUDGET: Duration = Duration::from_secs(180);
pub(crate) const QUIET_WINDOW: Duration = Duration::from_secs(3);
pub(crate) const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The whole emulator over one engine, exactly as `core/router/src/lib.rs` assembles it for
/// the binary: the `StartExecution` EventBridge makes and the `StartBackupJob` the task makes
/// are dispatched back into this same router, so the job this suite reads is the job that run
/// created.
pub(crate) fn router() -> MultiServiceRouter {
    let mut router = aws_emulator_router::build_router(aws_emulator_router::new_engine());
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: the router above runs with request authentication
/// off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// The status and parsed body of one response (`Value::Null` for an empty or unparseable
/// body).
fn answered(response: &http::Response<Bytes>) -> (u16, Value) {
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One `X-Amz-Target` RPC call against the service whose signing name is `signing_name`.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    json_version: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", target)
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    answered(&router.dispatch(request).await)
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`), which must answer 200.
pub(crate) async fn events(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        "events",
        &format!("AWSEvents.{operation}"),
        "1.1",
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Step Functions call (`awsJson1_0`, `AWSStepFunctions.<Operation>`), which must answer
/// 200.
pub(crate) async fn sfn(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        "states",
        &format!("AWSStepFunctions.{operation}"),
        "1.0",
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Step Functions {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon DynamoDB call (`awsJson1_0`, `DynamoDB_20120810.<Operation>`), which must
/// answer 200.
async fn dynamodb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(
        router,
        "dynamodb",
        &format!("DynamoDB_20120810.{operation}"),
        "1.0",
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One AWS Backup call (`restJson1`), which must answer 200.
pub(crate) async fn backup(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Value {
    let payload = body.map_or_else(Bytes::new, |body| Bytes::from(body.to_string()));
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://backup.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("backup.{REGION}.amazonaws.com"))
        .header("authorization", authorization("backup"));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(payload)
        .expect("the test request is well formed");
    let (status, parsed) = answered(&router.dispatch(request).await);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Backup {method} {uri}, got {status} with {parsed}"
    );
    parsed
}

/// A string member of a response, or a panic naming the case and what came back.
pub(crate) fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// The trust document of a role the services `services` may act as — a list, because one
/// role is handed to more than one service wherever the same role both takes the handover
/// and runs the work it starts.
fn trusting(services: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": services },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// An identity policy allowing `actions` on every resource.
fn allowing(actions: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": actions,
            "Resource": "*",
        }],
    })
    .to_string()
}

/// Create the role `name` where IAM holds it — trusted by `service`, granting `actions` — and
/// answer the ARN IAM ITSELF reports for it, so a value this suite hands another service is
/// the owning service's own answer rather than an ARN format written out here.
pub(crate) async fn role(
    router: &MultiServiceRouter,
    name: &str,
    service: &str,
    actions: &[&str],
) -> String {
    role_trusted_by(router, name, &[service], &allowing(actions)).await
}

/// The same, for a role whose identity-based policy is the WHOLE document `policy` and which
/// the services `services` may act as: what a role may do is the dimension a suite about
/// authorization varies, so the document is handed in rather than built from a list of
/// actions.
///
/// `pub(crate)` — like the rest of this harness — because
/// [`crate::backup_event_rule_target_permission_decides_its_backup_task`] asks what the
/// policies in force on that role decide about the same chain, and `tests/all/main.rs` is one
/// crate root: one wiring, reached from both.
pub(crate) async fn role_trusted_by(
    router: &MultiServiceRouter,
    name: &str,
    services: &[&str],
    policy: &str,
) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trusting(services)),
        ],
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "granted"),
            ("PolicyDocument", policy),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// A state machine whose single `Task` state performs `StartBackupJob` with the three members
/// read out of the document the execution was started with, at `vault_path`, `resource_path`
/// and `role_path`; answers its ARN.
pub(crate) async fn backup_state_machine(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    paths: (&str, &str, &str),
) -> String {
    let (vault_path, resource_path, role_path) = paths;
    let definition = json!({
        "StartAt": "StartBackupJob",
        "States": {
            "StartBackupJob": {
                "Type": "Task",
                "Resource": START_BACKUP_JOB_TASK,
                "Parameters": {
                    "BackupVaultName.$": vault_path,
                    "ResourceArn.$": resource_path,
                    "IamRoleArn.$": role_path,
                },
                "End": true,
            },
        },
    });
    let created = sfn(
        router,
        case,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": definition.to_string(),
            "roleArn": role_arn,
            "type": "STANDARD",
        }),
    )
    .await;
    member(case, &created, "/stateMachineArn")
}

/// The executions the state machine at `arn` holds right now.
pub(crate) async fn executions(router: &MultiServiceRouter, case: &str, arn: &str) -> Vec<Value> {
    let listed = sfn(
        router,
        case,
        "ListExecutions",
        &json!({ "stateMachineArn": arn }),
    )
    .await;
    listed["executions"].as_array().cloned().unwrap_or_default()
}

/// The executions of the machine at `arn` once it holds at least one, looking again until it
/// does or [`RUN_BUDGET`] has passed with it not doing so.
pub(crate) async fn executions_once_started(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + RUN_BUDGET;
    loop {
        let held = executions(router, case, arn).await;
        if !held.is_empty() {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the matched event to start an execution of {arn}, got \
             {held:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The execution at `execution_arn` once it has left `RUNNING`.
pub(crate) async fn once_terminal(
    router: &MultiServiceRouter,
    case: &str,
    execution_arn: &str,
) -> Value {
    let deadline = Instant::now() + RUN_BUDGET;
    loop {
        let described = sfn(
            router,
            case,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        if described["status"].as_str() != Some("RUNNING") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to reach a terminal status, got {described}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The backup job `job_id` as AWS Backup reports it once the clock has moved it out of
/// [`CREATED`], looking again until it has or [`RUN_BUDGET`] has passed.
pub(crate) async fn job_once_settled(
    router: &MultiServiceRouter,
    case: &str,
    job_id: &str,
) -> Value {
    let deadline = Instant::now() + RUN_BUDGET;
    loop {
        let read = backup(router, case, "GET", &format!("/backup-jobs/{job_id}"), None).await;
        if read["State"].as_str() != Some(CREATED) {
            return read;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the job the task started to leave {CREATED:?}, got {read}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// A protected resource an owning service really holds: a DynamoDB table, answered as the ARN
/// DynamoDB itself reports for it.
pub(crate) async fn protected_table(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> String {
    let created = dynamodb(
        router,
        case,
        "CreateTable",
        &json!({
            "TableName": name,
            "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "S" }],
            "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    member(case, &created, "/TableDescription/TableArn")
}

/// HOW the target shapes the document the execution is started with — the dimension the rows
/// differ along, and the one thing that decides where the task reads its three members from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// `Target$InputTransformer`, rendering the task's whole document out of the event's own
    /// detail.
    TransformedFromDetail,
    /// No input member at all, so the envelope itself is handed over and the task reads the
    /// detail inside it.
    WholeEnvelope,
    /// `Target$Input`, a constant document carrying nothing of the event.
    ConstantInput,
    /// `Target$InputTransformer` mixing the rule author's own constants — the vault the
    /// backup is taken into among them — with ONE value taken from the event.
    TransformedWithConstants,
}

impl Shape {
    /// Where the task reads the vault, the resource and the role from, inside the document
    /// the execution was started with.
    fn task_paths(self) -> (&'static str, &'static str, &'static str) {
        match self {
            // The envelope is handed over whole, so the task's document is inside its detail.
            Self::WholeEnvelope => ("$.detail.vault", "$.detail.resource", "$.detail.role"),
            _ => ("$.vault", "$.resource", "$.role"),
        }
    }

    /// The input member this shape adds to the target, for a chain backing `resource` up into
    /// `vault` as `role`.
    fn target_members(self, vault: &str, resource: &str, role: &str) -> Value {
        match self {
            Self::TransformedFromDetail => json!({
                "InputTransformer": {
                    "InputPathsMap": {
                        "vault": "$.detail.vault",
                        "resource": "$.detail.resource",
                        "role": "$.detail.role",
                    },
                    "InputTemplate":
                        "{\"vault\": <vault>, \"resource\": <resource>, \"role\": <role>}",
                },
            }),
            Self::WholeEnvelope => json!({}),
            Self::ConstantInput => json!({
                "Input": json!({ "vault": vault, "resource": resource, "role": role })
                    .to_string(),
            }),
            Self::TransformedWithConstants => json!({
                "InputTransformer": {
                    "InputPathsMap": { "plan": "$.detail.backupPlanId" },
                    "InputTemplate": format!(
                        "{{\"vault\": \"{vault}\", \"resource\": \"{resource}\", \
                         \"role\": \"{role}\", \"plan\": <plan>}}"
                    ),
                },
            }),
        }
    }
}

/// WHO published the event the rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// A client's own `PutEvents`, whose detail carries the chain's three values.
    Client,
    /// AWS Backup itself, publishing a stored backup plan's mutation.
    PlanMutation,
}

/// One row: which input member the target shapes the execution's document with, and who
/// published the event that starts it.
struct Row {
    case: &'static str,
    /// The `detail-type` the rule's pattern names and the event carries.
    detail_type: &'static str,
    trigger: Trigger,
    shape: Shape,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a transformer rendering the task's document out of the event's detail",
            detail_type: "VaultBackupRequested",
            trigger: Trigger::Client,
            shape: Shape::TransformedFromDetail,
        },
        Row {
            case: "the envelope handed over whole, read by the task through its detail",
            detail_type: "VaultBackupRequestedWhole",
            trigger: Trigger::Client,
            shape: Shape::WholeEnvelope,
        },
        Row {
            case: "the target's own constant input, carrying nothing of the event",
            detail_type: "VaultBackupRequestedConstant",
            trigger: Trigger::Client,
            shape: Shape::ConstantInput,
        },
        Row {
            case: "AWS Backup's own plan state change, with the vault supplied by the \
                   transformer",
            detail_type: BACKUP_PLAN_STATE_CHANGE,
            trigger: Trigger::PlanMutation,
            shape: Shape::TransformedWithConstants,
        },
    ]
}

/// The body of a backup plan named `name` whose one rule targets `vault`: the smallest plan
/// `CreateBackupPlan` stores, since what the row needs of it is the MUTATION it publishes
/// rather than anything the plan schedules.
fn plan_body(name: &str, vault: &str) -> Value {
    json!({
        "BackupPlanName": name,
        "Rules": [{ "RuleName": "daily", "TargetBackupVaultName": vault }],
    })
}

/// RULE: a matched event starts the rule's state machine with the document the target
/// rendered, and the execution's `aws-sdk:backup:startBackupJob` task leaves a real AWS
/// Backup job behind.
#[tokio::test]
async fn a_matched_event_runs_the_targets_backup_task() {
    // ONE emulator for the whole table: every row names its own vault, table, roles, rule and
    // state machine, so no row can see another's resources, and each row's rule names a
    // `detail-type` only that row's own event carries.
    let router = router();
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let vault = format!("event-driven-vault-{index}");
        let resource = protected_table(&router, case, &format!("event-driven-table-{index}")).await;
        backup(
            &router,
            case,
            "PUT",
            &format!("/backup-vaults/{vault}"),
            Some(&json!({})),
        )
        .await;
        // The three roles this chain acts as, each granting exactly the call made as it.
        let job_role = role(
            &router,
            &format!("event-driven-backup-{index}"),
            BACKUP_PRINCIPAL,
            &["*"],
        )
        .await;
        let machine_role = role(
            &router,
            &format!("event-driven-states-{index}"),
            STATES_PRINCIPAL,
            &[START_BACKUP_JOB_ACTION],
        )
        .await;
        let target_role = role(
            &router,
            &format!("event-driven-events-{index}"),
            EVENTS_PRINCIPAL,
            &[START_EXECUTION_ACTION],
        )
        .await;

        let machine = backup_state_machine(
            &router,
            case,
            &format!("event-driven-backup-{index}"),
            &machine_role,
            row.shape.task_paths(),
        )
        .await;

        // The rule, on the default bus, matching this row's own event alone.
        let source = match row.trigger {
            Trigger::Client => TEST_SOURCE,
            Trigger::PlanMutation => BACKUP_SOURCE,
        };
        let rule_name = format!("event-driven-rule-{index}");
        events(
            &router,
            case,
            "PutRule",
            &json!({
                "Name": &rule_name,
                "EventPattern": json!({
                    "source": [source],
                    "detail-type": [row.detail_type],
                })
                .to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let mut target = json!({
            "Id": "to-state-machine",
            "Arn": &machine,
            // EventBridge starts a state machine AS the role the target names, so a target of
            // this kind carries one.
            "RoleArn": &target_role,
        });
        for (name, value) in row
            .shape
            .target_members(&vault, &resource, &job_role)
            .as_object()
            .cloned()
            .unwrap_or_default()
        {
            target[name] = value;
        }
        let wired = events(
            &router,
            case,
            "PutTargets",
            &json!({ "Rule": &rule_name, "Targets": [target] }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {case:?}: the state-machine target must be accepted, got {wired}"
        );

        // The event itself: a client's own, or the one AWS Backup publishes for a plan a
        // client updated.
        let detail = json!({ "vault": &vault, "resource": &resource, "role": &job_role });
        let plan_id = match row.trigger {
            Trigger::Client => {
                let published = events(
                    &router,
                    case,
                    "PutEvents",
                    &json!({
                        "Entries": [{
                            "Source": TEST_SOURCE,
                            "DetailType": row.detail_type,
                            "Detail": detail.to_string(),
                        }],
                    }),
                )
                .await;
                assert_eq!(
                    published["FailedEntryCount"],
                    json!(0),
                    "case {case:?}: the event must be accepted, got {published}"
                );
                None
            }
            Trigger::PlanMutation => {
                let name = format!("event-driven-plan-{index}");
                let created = backup(
                    &router,
                    case,
                    "PUT",
                    "/backup/plans",
                    Some(&json!({ "BackupPlan": plan_body(&name, &vault) })),
                )
                .await;
                let plan_id = member(case, &created, "/BackupPlanId");
                // The MUTATION is what the bus carries: a create is what its own response
                // reports, so the plan is updated and that update's envelope is the one this
                // row's rule matches.
                backup(
                    &router,
                    case,
                    "POST",
                    &format!("/backup/plans/{plan_id}"),
                    Some(&json!({
                        "BackupPlan": plan_body(&format!("{name}-renamed"), &vault),
                    })),
                )
                .await;
                Some(plan_id)
            }
        };

        // ONE event the rule matched is ONE handover, so one execution of its target.
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
            json!("SUCCEEDED"),
            "case {case:?}: expected the execution to succeed, got {described}"
        );

        // The document the execution was started with is the one this row's input member
        // renders, and nothing else.
        let input: Value = serde_json::from_str(&member(case, &described, "/input"))
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the execution's input must be JSON ({error}): {described}")
            });
        match row.shape {
            Shape::WholeEnvelope => {
                for (pointer, expect) in [
                    ("/source", json!(TEST_SOURCE)),
                    ("/detail-type", json!(row.detail_type)),
                    ("/detail", detail.clone()),
                ] {
                    let actual = input.pointer(pointer).cloned().unwrap_or(Value::Null);
                    assert_eq!(
                        actual, expect,
                        "case {case:?}: expected the envelope's {pointer} to be {expect}, got \
                         {actual} in {input}"
                    );
                }
            }
            Shape::TransformedWithConstants => {
                let plan_id = plan_id
                    .clone()
                    .expect("such a row mutates a plan, which is what the event is about");
                assert_eq!(
                    input,
                    json!({
                        "vault": &vault,
                        "resource": &resource,
                        "role": &job_role,
                        "plan": plan_id,
                    }),
                    "case {case:?}: expected the transformer's constants and the event's own \
                     plan id, got {input}"
                );
            }
            Shape::TransformedFromDetail | Shape::ConstantInput => {
                assert_eq!(
                    input,
                    json!({ "vault": &vault, "resource": &resource, "role": &job_role }),
                    "case {case:?}: expected the rendered document the task reads, got {input}"
                );
            }
        }

        // The task's result is `StartBackupJob`'s own response, and the job it names is AWS
        // Backup's own stored job.
        let output: Value = serde_json::from_str(&member(case, &described, "/output"))
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the execution's output must be JSON ({error}): {described}")
            });
        let job_id = member(case, &output, "/BackupJobId");
        let recovery_point = member(case, &output, "/RecoveryPointArn");
        let read = job_once_settled(&router, case, &job_id).await;
        for (pointer, expect) in [
            ("/BackupVaultName", json!(&vault)),
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

        // The far end a client reads: the recovery point the backup wrote is held by the
        // vault the automation named.
        let held = backup(
            &router,
            case,
            "GET",
            &format!("/backup-vaults/{vault}/recovery-points"),
            None,
        )
        .await;
        let arns: Vec<String> = held["RecoveryPoints"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|point| point["RecoveryPointArn"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(
            arns.contains(&recovery_point),
            "case {case:?}: expected the vault to hold the recovery point the task reported \
             ({recovery_point:?}), got {arns:?}"
        );
    }
}

/// RULE, the other half: the handover is decided by the PATTERN, so a rule whose pattern the
/// event does not match starts nothing — while a sibling rule on the same bus, matched by
/// that same event, runs its own state machine and so proves the publication and the dispatch
/// happened at all.
#[tokio::test]
async fn an_event_a_rules_pattern_misses_starts_nothing() {
    const CASE: &str = "a rule scoped to another detail-type, beside one the event matches";
    const MATCHED: &str = "VaultBackupRequested";
    const UNMATCHED: &str = "VaultRestoreRequested";

    let router = router();
    let vault = "scoped-vault";
    let resource = protected_table(&router, CASE, "scoped-table").await;
    backup(
        &router,
        CASE,
        "PUT",
        &format!("/backup-vaults/{vault}"),
        Some(&json!({})),
    )
    .await;
    let job_role = role(&router, "scoped-backup", BACKUP_PRINCIPAL, &["*"]).await;
    let machine_role = role(
        &router,
        "scoped-states",
        STATES_PRINCIPAL,
        &[START_BACKUP_JOB_ACTION],
    )
    .await;
    let target_role = role(
        &router,
        "scoped-events",
        EVENTS_PRINCIPAL,
        &[START_EXECUTION_ACTION],
    )
    .await;

    // Two rules on the one bus, each with its own state machine: one scoped to the
    // detail-type the event carries, one scoped to another.
    let mut machines: Vec<(&str, String)> = Vec::new();
    for detail_type in [MATCHED, UNMATCHED] {
        let machine = backup_state_machine(
            &router,
            CASE,
            &format!("scoped-machine-{detail_type}"),
            &machine_role,
            Shape::TransformedFromDetail.task_paths(),
        )
        .await;
        events(
            &router,
            CASE,
            "PutRule",
            &json!({
                "Name": format!("scoped-rule-{detail_type}"),
                "EventPattern": json!({
                    "source": [TEST_SOURCE],
                    "detail-type": [detail_type],
                })
                .to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let mut target = json!({
            "Id": "to-state-machine",
            "Arn": &machine,
            "RoleArn": &target_role,
        });
        for (name, value) in Shape::TransformedFromDetail
            .target_members(vault, &resource, &job_role)
            .as_object()
            .cloned()
            .unwrap_or_default()
        {
            target[name] = value;
        }
        events(
            &router,
            CASE,
            "PutTargets",
            &json!({
                "Rule": format!("scoped-rule-{detail_type}"),
                "Targets": [target],
            }),
        )
        .await;
        machines.push((detail_type, machine));
    }

    events(
        &router,
        CASE,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": TEST_SOURCE,
                "DetailType": MATCHED,
                "Detail": json!({
                    "vault": vault,
                    "resource": &resource,
                    "role": &job_role,
                })
                .to_string(),
            }],
        }),
    )
    .await;

    let matched = machines
        .iter()
        .find(|(detail_type, _)| *detail_type == MATCHED)
        .map(|(_, machine)| machine.clone())
        .expect("the matched machine was created above");
    let unmatched = machines
        .iter()
        .find(|(detail_type, _)| *detail_type == UNMATCHED)
        .map(|(_, machine)| machine.clone())
        .expect("the unmatched machine was created above");

    // The control: the sibling rule's own machine really ran for this event.
    let started = executions_once_started(&router, CASE, &matched).await;
    let execution_arn = member(CASE, &started[0], "/executionArn");
    let described = once_terminal(&router, CASE, &execution_arn).await;
    assert_eq!(
        described["status"],
        json!("SUCCEEDED"),
        "case {CASE:?}: the matching rule's own execution must succeed, got {described}"
    );

    // And the rule the event does not match started nothing, looked for over a window longer
    // than the span between two dispatches of one publication.
    let deadline = Instant::now() + QUIET_WINDOW;
    while Instant::now() < deadline {
        let held = executions(&router, CASE, &unmatched).await;
        assert!(
            held.is_empty(),
            "case {CASE:?}: nothing may start the unmatched rule's machine, got {held:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}
