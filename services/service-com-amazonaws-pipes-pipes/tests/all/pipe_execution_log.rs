// SPDX-License-Identifier: CC0-1.0

//! What a pipe carrying a `LogConfiguration` WRITES, end to end over the wire, with Amazon
//! SQS, IAM and CloudWatch Logs mounted beside pipes on ONE engine — so an execution log
//! record is asserted where a client reads it (`FilterLogEvents` on the configured group)
//! and the execution it is about where a client sees that (the record at the target queue).
//!
//! The RULE asserted over the table below: a RUNNING pipe writes, for every execution it
//! takes, the records its configured `Level` admits to the destination its
//! `LogConfiguration` names — `OFF` none, `ERROR` only the failures, `INFO` those plus the
//! execution's own start and verdict, `TRACE` those plus a record per stage the execution
//! reached — each naming the executing pipe in `resourceArn`, sharing one `executionId`
//! across the records of one execution and reporting its own `logLevel`, and carrying what
//! the stage was handed in its own `payload` field exactly when `IncludeExecutionData`
//! contains `ALL`. An execution that never happened leaves no record, and a destination the
//! pipe's execution role may not write to, or that the account does not hold, leaves the
//! group as it was while the execution itself runs unaffected. The write is authorized on the
//! STREAM the records go into, so the grant AWS publishes for `logs:CreateLogStream` and
//! `logs:PutLogEvents` — the group with everything under it, or the group's streams — lets a
//! pipe write whichever form its destination's `LogGroupArn` is in, and a grant over another
//! group's streams lets it write nothing.
//!
//! A record per STAGE is a record per stage the execution HANDS A BATCH TO — the enrichment
//! and the target; what the source poll took is the `ExecutionStarted` record's own payload,
//! and a stage that failed the execution is named by its failure record whichever stage it
//! was.
//!
//! The rows differ along what the rule generalises over — the configured level, whether the
//! execution data was asked for, whether the execution SUCCEEDS or its target stage fails,
//! which published form of a log group's ARN the destination is written in, what resource the
//! role is granted the two log actions on, and whether the destination exists at all. Each
//! row is asserted by its own test, so one row never waits out another row's pipe.
//!
//! Every wait here LOOKS AGAIN until the condition it needs holds, on a generous deadline,
//! and every assertion that something stays ABSENT is looked at repeatedly across its span:
//! nothing below is asserted about elapsed time, so a row reports what the pipe did and
//! never how fast the host it ran on happened to be.
//!
//! The second table ([`the_refusal_is_reported_for`]) is about the `ERROR` level over a stage
//! whose INVOCATION the service it called REFUSED, with Step Functions mounted beside the
//! rest: the refusal belongs to that service, so the record names which service refused the
//! pipe's call, with what status, under which class and request id, over which resource and
//! in whose words.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_pipes_pipes::state::execution_role::SERVICE_PRINCIPAL;
use smithy_server_runtime::MultiServiceRouter;

use crate::pipe_data_plane::{collect_at_least, given_queue, rpc, send, LOOK_AGAIN, QUIET};
use crate::pipe_lifecycle::{call, REGION};
use crate::pipe_metrics::given_role_stating;

/// How long a wait for something the pipe has to do — a delivery, or a log record appearing
/// — is given before the row is a failure.
///
/// Generous rather than tight: every wait looks again until its condition holds, so a row
/// whose work the pipe did spends none of this, and a host running this suite beside every
/// other one is slow at everything.
const BUDGET: Duration = Duration::from_secs(60);

/// How long a wait in the refused-invocation table is given before the row is a failure.
///
/// More generous than [`BUDGET`]: a row there drives a pipe through a stage that calls
/// another service of this emulator, so its work spans two services' own passes, and this
/// suite runs beside every other one in the workspace. Every wait below looks again until
/// its condition holds, so a row whose work the pipe did spends none of this.
const REFUSAL_BUDGET: Duration = Duration::from_secs(150);

/// The record kinds a pipe writes, named as the records themselves name them.
const EXECUTION_STARTED: &str = "ExecutionStarted";
const EXECUTION_SUCCEEDED: &str = "ExecutionSucceeded";
const EXECUTION_FAILED: &str = "ExecutionFailed";
const EXECUTION_STAGE_STARTED: &str = "ExecutionStageStarted";
const EXECUTION_STAGE_SUCCEEDED: &str = "ExecutionStageSucceeded";
const EXECUTION_STAGE_FAILED: &str = "ExecutionStageFailed";
/// The kinds that report the INVOCATION a stage made being refused by the service it was
/// made to, one per stage that makes one.
const ENRICHMENT_INVOCATION_FAILED: &str = "EnrichmentInvocationFailed";
const TARGET_INVOCATION_FAILED: &str = "TargetInvocationFailed";

/// Pipes, plus every service this rule is observed through, on ONE engine — the shape
/// `core/router/src/lib.rs` mounts them in: the source and target queues, the IAM role the
/// stages act as, the CloudWatch Logs group the records are read back from, and the Step
/// Functions state machine the rows about a REFUSED INVOCATION name as a pipe's enrichment
/// or target.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_pipes_pipes::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// One CloudWatch Logs call, over the same awsJson path every other RPC here takes.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "logs", "Logs_20140328", operation, body).await
}

/// A log group called `name`, answered with the ARN CloudWatch Logs reports for it — read
/// back off `DescribeLogGroups` the way a client that is about to point a service at a group
/// reads it, with any trailing `:*` stripped.
async fn given_log_group(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = logs(router, "CreateLogGroup", &json!({ "logGroupName": name })).await;
    assert_eq!(
        status, 200,
        "case {name}: CreateLogGroup answered {status} {created}"
    );
    let (status, described) = logs(
        router,
        "DescribeLogGroups",
        &json!({ "logGroupNamePrefix": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {name}: DescribeLogGroups answered {status} {described}"
    );
    let arn = described["logGroups"]
        .as_array()
        .and_then(|groups| groups.first())
        .and_then(|group| group["arn"].as_str())
        .unwrap_or_else(|| panic!("case {name}: no log group arn in {described}"))
        .to_string();
    arn.strip_suffix(":*").unwrap_or(&arn).to_string()
}

/// Every event `FilterLogEvents` reports for `group`, optionally through `pattern`, and the
/// status it answered with — a group the account does not hold answers a refusal rather than
/// an empty page, which is "no records" just as plainly.
async fn filtered(
    router: &MultiServiceRouter,
    group: &str,
    pattern: Option<&str>,
) -> (u16, Vec<Value>) {
    let mut body = json!({ "logGroupName": group });
    if let Some(pattern) = pattern {
        body["filterPattern"] = json!(pattern);
    }
    let (status, answer) = logs(router, "FilterLogEvents", &body).await;
    if status != 200 {
        return (status, Vec::new());
    }
    (
        status,
        answer["events"].as_array().cloned().unwrap_or_default(),
    )
}

/// Every event of `group` as the JSON record its `message` spells, in the order reported.
async fn records_in(router: &MultiServiceRouter, group: &str, pattern: Option<&str>) -> Vec<Value> {
    let (_, events) = filtered(router, group, pattern).await;
    events
        .iter()
        .map(|event| {
            let message = event["message"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(message).unwrap_or_else(|error| {
                panic!("an execution log record is not JSON ({error}): {message:?}")
            })
        })
        .collect()
}

/// Wait until the records of `group` carry every message type in `wanted`, answering what
/// the group held when the wait ended.
async fn records_carrying(
    router: &MultiServiceRouter,
    group: &str,
    wanted: &[&str],
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let held = records_in(router, group, None).await;
        let kinds: BTreeSet<&str> = held
            .iter()
            .filter_map(|record| record["messageType"].as_str())
            .collect();
        if wanted.iter().all(|kind| kinds.contains(kind)) || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Watch `group` for the whole of `watch`, failing the row at the first look that finds any
/// record in it.
///
/// A group that stays empty is an invariant rather than something to wait for, so it is
/// LOOKED AT repeatedly across the span instead of once at the end of a sleep.
async fn stays_empty(router: &MultiServiceRouter, case: &str, group: &str, watch: Duration) {
    let deadline = Instant::now() + watch;
    loop {
        let (status, events) = filtered(router, group, None).await;
        assert!(
            events.is_empty(),
            "case {case:?}: expected no execution log record in {group} (FilterLogEvents \
             answered {status}), got {events:?}"
        );
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Which published form of a log group's ARN a value is written in.
///
/// A destination's `LogGroupArn` may be either, and both name the same group: the group on
/// its own, and the group together with everything under it — the form `DescribeLogGroups`
/// reports a group's `arn` in.
#[derive(Clone, Copy, Debug)]
enum GroupArn {
    /// `arn:aws:logs:<region>:<account>:log-group:<name>`.
    OnItsOwn,
    /// That ARN followed by `:*`.
    WithEverythingUnderIt,
}

/// What the pipe's execution role is granted `logs:CreateLogStream` and `logs:PutLogEvents`
/// on, as the resource element of its one statement for them.
#[derive(Clone, Copy, Debug)]
enum LogGrant {
    /// Nothing: the role carries no statement for those actions at all.
    Nothing,
    /// The configured group with everything under it (`<the group's ARN>:*`).
    EverythingUnderTheGroup,
    /// Every stream of the configured group (`<the group's ARN>:log-stream:*`).
    TheGroupsStreams,
    /// Every stream of ANOTHER group of the same account: a grant that does not reach the
    /// destination this pipe names.
    AnotherGroupsStreams,
}

/// One row of the table: how the pipe is configured to log, and what the group then holds.
struct Row {
    case: &'static str,
    /// `LogConfiguration.Level`.
    level: &'static str,
    /// Whether `IncludeExecutionData` carries `ALL`.
    execution_data: bool,
    /// Whether the account holds the log group the configuration names.
    group_held: bool,
    /// The form the destination's `LogGroupArn` is written in.
    destination: GroupArn,
    /// What the pipe's execution role is granted the two log actions on.
    log_grant: LogGrant,
    /// Whether that role may deliver to the target: a row that withholds it is an execution
    /// whose TARGET STAGE fails.
    may_deliver: bool,
    /// The message types the group has to carry.
    expect: &'static [&'static str],
    /// The message types it must not carry.
    forbid: &'static [&'static str],
}

/// RULE: a RUNNING pipe writes the execution log records its configured `Level` admits to
/// the CloudWatch Logs destination its `LogConfiguration` names — each naming the pipe, the
/// one execution they belong to and their own level, and carrying the stage's data in
/// `payload` exactly when `IncludeExecutionData` contains `ALL` — and writes nothing for an
/// execution that did not happen, to a destination it may not write to, or to one the
/// account does not hold. The write is authorized on the STREAM the records go into, so a
/// role granted those actions over the group's streams may write whichever published form
/// the destination's `LogGroupArn` is in, and one granted them only over another group's
/// streams may not.
///
/// One row of the table below is asserted per test, so the rows run beside each other and no
/// one of them waits out another's pipe.
async fn the_rule_holds_for(row: Row) {
    {
        let router = emulator();
        let (source_url, source_arn) = given_queue(&router, &format!("{}-source", row.case)).await;
        let (target_url, target_arn) = given_queue(&router, &format!("{}-target", row.case)).await;
        let group = format!("/aws/vendedlogs/pipes/{}", row.case);
        let group_arn = if row.group_held {
            given_log_group(&router, &group).await
        } else {
            // A well-formed destination naming a group nothing created.
            format!(
                "arn:aws:logs:{REGION}:{}:log-group:{group}",
                engine::emulator_account_id()
            )
        };
        // The destination as the pipe configures it, in the form this row names.
        let configured_arn = match row.destination {
            GroupArn::OnItsOwn => group_arn.clone(),
            GroupArn::WithEverythingUnderIt => format!("{group_arn}:*"),
        };
        // What the role is granted the two log actions on, written independently of the form
        // the destination is in: a grant and a destination that name the same group do so in
        // whichever forms their own writers chose.
        let log_grant = match row.log_grant {
            LogGrant::Nothing => None,
            LogGrant::EverythingUnderTheGroup => Some(format!("{group_arn}:*")),
            LogGrant::TheGroupsStreams => Some(format!("{group_arn}:log-stream:*")),
            LogGrant::AnotherGroupsStreams => Some(format!(
                "arn:aws:logs:{REGION}:{}:log-group:{group}-elsewhere:log-stream:*",
                engine::emulator_account_id()
            )),
        };
        // The role every stage acts as: the source poll always (a pipe whose role may not
        // take its source's records is never brought up at all, which is
        // `tests/all/pipe_bring_up.rs`), and the delivery and the log write as the row says.
        let mut statements = vec![json!({
            "Effect": "Allow",
            "Action": ["sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:GetQueueAttributes"],
            "Resource": source_arn,
        })];
        if row.may_deliver {
            statements.push(json!({
                "Effect": "Allow",
                "Action": "sqs:SendMessage",
                "Resource": target_arn,
            }));
        }
        if let Some(resource) = log_grant {
            statements.push(json!({
                "Effect": "Allow",
                "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": resource,
            }));
        }
        let role_arn = given_role_stating(
            &router,
            &format!("{}-role", row.case),
            SERVICE_PRINCIPAL,
            Value::Array(statements),
        )
        .await;
        let mut log_configuration = json!({
            "CloudwatchLogsLogDestination": { "LogGroupArn": configured_arn },
            "Level": row.level,
        });
        if row.execution_data {
            log_configuration["IncludeExecutionData"] = json!(["ALL"]);
        }
        let created = call(
            &router,
            "POST",
            &format!("/v1/pipes/{}", row.case),
            Some(json!({
                "Source": source_arn,
                "Target": target_arn,
                "RoleArn": role_arn,
                "SourceParameters": { "SqsQueueParameters": { "BatchSize": 1 } },
                "LogConfiguration": log_configuration,
                "DesiredState": "RUNNING",
            })),
            REGION,
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {:?}: CreatePipe answered {} {}",
            row.case, created.status, created.body
        );
        let pipe_arn = created.string("Arn");
        assert!(
            !pipe_arn.is_empty(),
            "case {:?}: CreatePipe reported no Arn: {}",
            row.case,
            created.body
        );

        // The record the execution is about, carrying a marker no other row sends.
        let marker = format!("{}-marker", row.case);
        send(&router, &source_url, std::slice::from_ref(&marker)).await;

        if row.may_deliver {
            // The delivery is the EVIDENCE that the execution happened, so every read of
            // the group below is about a pipe that has already run rather than about one the
            // host has not got round to yet.
            let delivered = collect_at_least(&router, &target_url, 1, BUDGET).await;
            assert_eq!(
                delivered.len(),
                1,
                "case {:?}: expected the target to hold the record the pipe moved, got \
                 {delivered:?}",
                row.case
            );
        }

        if row.expect.is_empty() {
            stays_empty(&router, row.case, &group, QUIET).await;
            return;
        }

        let held = records_carrying(&router, &group, row.expect).await;
        let kinds: BTreeSet<&str> = held
            .iter()
            .filter_map(|record| record["messageType"].as_str())
            .collect();
        for wanted in row.expect {
            assert!(
                kinds.contains(wanted),
                "case {:?}: expected a {wanted} record in {group}, got {kinds:?} from {held:?}",
                row.case
            );
        }
        for forbidden in row.forbid {
            assert!(
                !kinds.contains(forbidden),
                "case {:?}: expected no {forbidden} record in {group}, got {kinds:?}",
                row.case
            );
        }

        // Every record names the pipe that wrote it, belongs to an execution and reports its
        // own level.
        let levels: BTreeSet<&str> = held
            .iter()
            .filter_map(|record| record["logLevel"].as_str())
            .collect();
        let executions: BTreeSet<&str> = held
            .iter()
            .filter_map(|record| record["executionId"].as_str())
            .collect();
        for record in &held {
            assert_eq!(
                record["resourceArn"],
                json!(pipe_arn),
                "case {:?}: expected every record to name the executing pipe, got {record}",
                row.case
            );
            assert!(
                record["executionId"].as_str().is_some_and(|id| !id.is_empty()),
                "case {:?}: expected every record to carry an executionId, got {record}",
                row.case
            );
            assert!(
                record["timestamp"]
                    .as_str()
                    .is_some_and(|at| !at.is_empty()),
                "case {:?}: expected every record to be stamped, got {record}",
                row.case
            );
        }
        assert!(
            !executions.is_empty(),
            "case {:?}: expected the records to name the execution they belong to, got {held:?}",
            row.case
        );
        if row.may_deliver {
            // One execution moved the one record sent, so every record of this group
            // belongs to it: the id is what a reader asks the execution's records by.
            assert_eq!(
                executions.len(),
                1,
                "case {:?}: expected the records of ONE execution to share one id, got \
                 {executions:?}",
                row.case
            );
        }
        // The finest level the configuration admits is one the records actually carry: a
        // TRACE pipe writes TRACE records, and an ERROR pipe writes ERROR ones.
        let finest = match row.level {
            "TRACE" => "TRACE",
            "INFO" => "INFO",
            _ => "ERROR",
        };
        assert!(
            levels.contains(finest),
            "case {:?}: expected a record at the configured level {finest}, got {levels:?}",
            row.case
        );

        // The execution DATA: the record the pipe took travels in the record's own payload
        // field exactly when the configuration asked for it.
        let carrying_payload: Vec<&Value> = held
            .iter()
            .filter(|record| record.get("payload").is_some())
            .collect();
        if row.execution_data && row.may_deliver {
            assert!(
                carrying_payload.iter().any(|record| record["payload"]
                    .as_str()
                    .is_some_and(|payload| payload.contains(&marker))),
                "case {:?}: expected a record whose own payload field carries the source \
                 record's payload {marker:?}, got {held:?}",
                row.case
            );
        }
        if !row.execution_data {
            assert!(
                carrying_payload.is_empty(),
                "case {:?}: expected no record to carry the execution data, got \
                 {carrying_payload:?}",
                row.case
            );
        }

        // And the group answers ABOUT that execution: filtering on its id reports its
        // records only, while a marker never sent through the pipe matches nothing.
        let execution = executions
            .first()
            .expect("the records name an execution")
            .to_string();
        let of_execution = records_in(
            &router,
            &group,
            Some(&format!("\"{execution}\"")),
        )
        .await;
        assert!(
            !of_execution.is_empty(),
            "case {:?}: expected the records of execution {execution} to be readable by its \
             id, got none",
            row.case
        );
        for record in &of_execution {
            assert_eq!(
                record["executionId"],
                json!(execution),
                "case {:?}: expected only the records of execution {execution}, got {record}",
                row.case
            );
            assert_eq!(
                record["resourceArn"],
                json!(pipe_arn),
                "case {:?}: expected every record of that execution to name the pipe, got \
                 {record}",
                row.case
            );
        }
        let (status, never_sent) = filtered(
            &router,
            &group,
            Some(&format!("\"{}-never-sent\"", row.case)),
        )
        .await;
        assert!(
            never_sent.is_empty(),
            "case {:?}: expected a marker the pipe never moved to match no record \
             (FilterLogEvents answered {status}), got {never_sent:?}",
            row.case
        );
    }
}

/// The row of the table the behaviour the issue was filed about is one of: every record the
/// finest level admits, with the execution data asked for, over the grant AWS publishes for
/// these two actions — the group with everything under it — and a destination naming the
/// group on its own.
#[tokio::test]
async fn a_trace_pipe_asking_for_the_execution_data_logs_every_stage_with_the_payload() {
    the_rule_holds_for(Row {
        case: "trace-with-execution-data",
        level: "TRACE",
        execution_data: true,
        group_held: true,
        destination: GroupArn::OnItsOwn,
        log_grant: LogGrant::EverythingUnderTheGroup,
        may_deliver: true,
        expect: &[
            EXECUTION_STARTED,
            EXECUTION_STAGE_STARTED,
            EXECUTION_STAGE_SUCCEEDED,
            EXECUTION_SUCCEEDED,
        ],
        forbid: &[EXECUTION_STAGE_FAILED, EXECUTION_FAILED],
    })
    .await;
}

/// One level coarser: the execution's own records, and none of the per-stage ones.
#[tokio::test]
async fn an_info_pipe_logs_the_executions_own_records_and_no_stage_record() {
    the_rule_holds_for(Row {
        case: "info-with-execution-data",
        level: "INFO",
        execution_data: true,
        group_held: true,
        destination: GroupArn::OnItsOwn,
        log_grant: LogGrant::EverythingUnderTheGroup,
        may_deliver: true,
        expect: &[EXECUTION_STARTED, EXECUTION_SUCCEEDED],
        forbid: &[
            EXECUTION_STAGE_STARTED,
            EXECUTION_STAGE_SUCCEEDED,
            EXECUTION_STAGE_FAILED,
            EXECUTION_FAILED,
        ],
    })
    .await;
}

/// The same finest level with the execution data NOT asked for: every record, carrying none
/// of the data — "by default, execution data is not included".
#[tokio::test]
async fn a_trace_pipe_asking_for_no_execution_data_logs_every_stage_without_the_payload() {
    the_rule_holds_for(Row {
        case: "trace-without-execution-data",
        level: "TRACE",
        execution_data: false,
        group_held: true,
        destination: GroupArn::WithEverythingUnderIt,
        log_grant: LogGrant::TheGroupsStreams,
        may_deliver: true,
        expect: &[
            EXECUTION_STARTED,
            EXECUTION_STAGE_STARTED,
            EXECUTION_STAGE_SUCCEEDED,
            EXECUTION_SUCCEEDED,
        ],
        forbid: &[EXECUTION_STAGE_FAILED, EXECUTION_FAILED],
    })
    .await;
}

/// The level that carries only failures, over an execution that has none.
#[tokio::test]
async fn an_error_pipe_logs_nothing_for_an_execution_that_succeeds() {
    the_rule_holds_for(Row {
        case: "error-level-over-a-pipe-that-succeeds",
        level: "ERROR",
        execution_data: true,
        group_held: true,
        destination: GroupArn::OnItsOwn,
        log_grant: LogGrant::EverythingUnderTheGroup,
        may_deliver: true,
        expect: &[],
        forbid: &[
            EXECUTION_STARTED,
            EXECUTION_SUCCEEDED,
            EXECUTION_STAGE_STARTED,
            EXECUTION_STAGE_SUCCEEDED,
            EXECUTION_STAGE_FAILED,
            EXECUTION_FAILED,
        ],
    })
    .await;
}

/// The same level over an execution whose TARGET stage is refused: the failure is exactly
/// what this level carries, and the stage that failed is named in it.
#[tokio::test]
async fn an_error_pipe_logs_the_stage_that_failed_its_execution() {
    the_rule_holds_for(Row {
        case: "error-level-over-a-failing-target",
        level: "ERROR",
        execution_data: true,
        group_held: true,
        destination: GroupArn::OnItsOwn,
        log_grant: LogGrant::TheGroupsStreams,
        may_deliver: false,
        expect: &[EXECUTION_STAGE_FAILED, EXECUTION_FAILED],
        forbid: &[
            EXECUTION_STARTED,
            EXECUTION_SUCCEEDED,
            EXECUTION_STAGE_STARTED,
            EXECUTION_STAGE_SUCCEEDED,
        ],
    })
    .await;
}

/// Logging turned off: the destination stays as it was however much the pipe runs.
#[tokio::test]
async fn a_pipe_whose_logging_is_off_writes_nothing() {
    the_rule_holds_for(Row {
        case: "logging-turned-off",
        level: "OFF",
        execution_data: true,
        group_held: true,
        destination: GroupArn::WithEverythingUnderIt,
        log_grant: LogGrant::EverythingUnderTheGroup,
        may_deliver: true,
        expect: &[],
        forbid: &[
            EXECUTION_STARTED,
            EXECUTION_SUCCEEDED,
            EXECUTION_STAGE_STARTED,
            EXECUTION_STAGE_SUCCEEDED,
            EXECUTION_STAGE_FAILED,
            EXECUTION_FAILED,
        ],
    })
    .await;
}

/// The write is made as the pipe's execution role, so a role that may not make it writes
/// nothing — while the execution itself still moves the record.
#[tokio::test]
async fn a_role_that_may_not_write_the_log_writes_nothing_and_still_delivers() {
    the_rule_holds_for(Row {
        case: "a-role-that-may-not-write-the-log",
        level: "TRACE",
        execution_data: true,
        group_held: true,
        destination: GroupArn::OnItsOwn,
        log_grant: LogGrant::Nothing,
        may_deliver: true,
        expect: &[],
        forbid: &[EXECUTION_STARTED, EXECUTION_SUCCEEDED],
    })
    .await;
}

/// A destination the account does not hold: the records are lost and no group is created,
/// which is what makes a misconfigured destination observable.
#[tokio::test]
async fn a_destination_the_account_does_not_hold_holds_nothing() {
    the_rule_holds_for(Row {
        case: "a-destination-the-account-does-not-hold",
        level: "TRACE",
        execution_data: true,
        group_held: false,
        destination: GroupArn::OnItsOwn,
        log_grant: LogGrant::EverythingUnderTheGroup,
        may_deliver: true,
        expect: &[],
        forbid: &[EXECUTION_STARTED, EXECUTION_SUCCEEDED],
    })
    .await;
}

/// The same pipe as the row above with its destination named in the OTHER published form of
/// a group's ARN — the group with everything under it, which is what a client reads off
/// `DescribeLogGroups` — against a role granted the group's streams: the records go into the
/// one group either ARN names.
#[tokio::test]
async fn a_destination_naming_the_group_with_everything_under_it_is_written_to() {
    the_rule_holds_for(Row {
        case: "a-destination-naming-everything-under-the-group",
        level: "TRACE",
        execution_data: true,
        group_held: true,
        destination: GroupArn::WithEverythingUnderIt,
        log_grant: LogGrant::TheGroupsStreams,
        may_deliver: true,
        expect: &[
            EXECUTION_STARTED,
            EXECUTION_STAGE_STARTED,
            EXECUTION_STAGE_SUCCEEDED,
            EXECUTION_SUCCEEDED,
        ],
        forbid: &[EXECUTION_STAGE_FAILED, EXECUTION_FAILED],
    })
    .await;
}

/// A role granted the two actions over the streams of the group the destination names, with
/// that destination naming the group on its own: the grant reaches the stream the records go
/// into, so they are written.
#[tokio::test]
async fn a_role_granted_the_groups_streams_writes_the_log() {
    the_rule_holds_for(Row {
        case: "a-role-granted-the-groups-streams",
        level: "TRACE",
        execution_data: true,
        group_held: true,
        destination: GroupArn::OnItsOwn,
        log_grant: LogGrant::TheGroupsStreams,
        may_deliver: true,
        expect: &[
            EXECUTION_STARTED,
            EXECUTION_STAGE_STARTED,
            EXECUTION_STAGE_SUCCEEDED,
            EXECUTION_SUCCEEDED,
        ],
        forbid: &[EXECUTION_STAGE_FAILED, EXECUTION_FAILED],
    })
    .await;
}

/// And a role granted those actions over ANOTHER group's streams writes nothing into this
/// one: the resource the write is authorized on is the stream of the group the destination
/// names, so a grant that does not reach it is no grant at all — while the execution still
/// moves the record.
#[tokio::test]
async fn a_role_granted_another_groups_streams_writes_nothing_and_still_delivers() {
    the_rule_holds_for(Row {
        case: "a-role-granted-another-groups-streams",
        level: "TRACE",
        execution_data: true,
        group_held: true,
        destination: GroupArn::OnItsOwn,
        log_grant: LogGrant::AnotherGroupsStreams,
        may_deliver: true,
        expect: &[],
        forbid: &[EXECUTION_STARTED, EXECUTION_SUCCEEDED],
    })
    .await;
}

/// A workflow that hands its input straight back, so an enrichment run of it answers the
/// events it was handed. It is the smallest definition that runs at all.
const ECHO_DEFINITION: &str = r#"{"StartAt":"Echo","States":{"Echo":{"Type":"Pass","End":true}}}"#;

/// The workflow types a state machine below is created as, as Step Functions' own
/// `StateMachineType` spells them.
const STANDARD: &str = "STANDARD";
/// See [`STANDARD`].
const EXPRESS: &str = "EXPRESS";

/// One Step Functions call (`awsJson1_0`, `AWSStepFunctions.<Operation>`).
async fn states(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "states", "AWSStepFunctions", operation, body).await
}

/// A state machine called `name` of the workflow type `sm_type`; answers the ARN a pipe
/// names it by.
async fn given_state_machine(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    sm_type: &str,
) -> String {
    let (status, created) = states(
        router,
        "CreateStateMachine",
        &json!({
            "name": name,
            "definition": ECHO_DEFINITION,
            "roleArn": format!("arn:aws:iam::{}:role/{name}-workflow", engine::emulator_account_id()),
            "type": sm_type,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStateMachine answered {status} {created}"
    );
    created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no stateMachineArn in {created}"))
        .to_string()
}

/// Which stage of the pipe names the state machine, and so whose invocation the called
/// service refuses.
#[derive(Clone, Copy, Debug)]
enum Stage {
    /// The pipe's `Enrichment`, with a queue as its target.
    Enrichment,
    /// The pipe's `Target`, started synchronously (`REQUEST_RESPONSE`).
    Target,
}

/// What the state machine that stage names IS.
#[derive(Clone, Copy, Debug)]
enum Machine {
    /// A `STANDARD` workflow: the type `StartSyncExecution` is "not available for", so Step
    /// Functions refuses the synchronous start of it.
    Standard,
    /// An `EXPRESS` workflow: the type that serves the synchronous start, so nothing refuses
    /// the call.
    Express,
    /// A well-formed state machine ARN of this account that nothing created — another
    /// refusal of the same service, worded about the resource.
    Absent,
}

/// One row of the refused-invocation table: which stage calls Step Functions, what it names,
/// and what the group then holds about it.
struct Refused {
    /// Also the pipe's, the queues', the group's and the state machine's name, so every row
    /// is independent.
    case: &'static str,
    stage: Stage,
    machine: Machine,
    /// The record the refusal is reported by, or `None` for a call nothing refused.
    message_type: Option<&'static str>,
    /// How that record's message opens, before the service that refused.
    opening: &'static str,
    /// The sentence the called service itself worded, where it is that service's own fixed
    /// text for the error class; `None` where the sentence quotes the resource, and the row
    /// asserts the structure around it instead of Step Functions' own wording.
    reason: Option<&'static str>,
}

/// The first record of `group` whose message carries `marker` — read the way the client in
/// the report reads it, with `FilterLogEvents` through a pattern for that marker — once one
/// is there.
async fn record_carrying(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    marker: &str,
) -> Value {
    let pattern = format!("\"{marker}\"");
    let deadline = Instant::now() + REFUSAL_BUDGET;
    loop {
        let held = records_in(router, group, Some(&pattern)).await;
        if let Some(first) = held.first() {
            return first.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a record carrying {marker:?} in {group}, got none"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Watch `group` for the whole of `watch`, failing the row at the first look that finds a
/// record reporting a refused invocation.
async fn no_invocation_failure_in(router: &MultiServiceRouter, case: &str, group: &str) {
    let deadline = Instant::now() + QUIET;
    loop {
        let kinds: BTreeSet<String> = records_in(router, group, None)
            .await
            .iter()
            .filter_map(|record| record["messageType"].as_str().map(ToString::to_string))
            .collect();
        for forbidden in [ENRICHMENT_INVOCATION_FAILED, TARGET_INVOCATION_FAILED] {
            assert!(
                !kinds.contains(forbidden),
                "case {case:?}: expected no {forbidden} record for a call nothing refused, \
                 got {kinds:?}"
            );
        }
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: when the service a pipe's enrichment or target stage CALLS refuses that call, the
/// pipe writes one `ERROR`-level record for the stage whose invocation failed —
/// `EnrichmentInvocationFailed` or `TargetInvocationFailed` — whose `error` attributes the
/// refusal to the service that answered it: `awsService` the namespace that service signs
/// under, `httpStatusCode` the status it answered, `exceptionType` that status's class,
/// `resourceArn` the resource the call named, `requestId` the identifier of that answer, and
/// `message` the stage's own opening, the service that refused, that service's own sentence
/// and the clause naming the call. A call nothing refused leaves no such record.
///
/// The rows differ along what the rule generalises over: which stage made the call, what the
/// resource it named is, and which refusal of the called service that produces.
async fn the_refusal_is_reported_for(row: Refused) {
    let router = emulator();
    let (source_url, source_arn) = given_queue(&router, &format!("{}-source", row.case)).await;
    let (target_url, target_arn) = given_queue(&router, &format!("{}-target", row.case)).await;
    let group = format!("/aws/vendedlogs/pipes/{}", row.case);
    let group_arn = given_log_group(&router, &group).await;
    let machine = format!("{}-machine", row.case);
    let machine_arn = match row.machine {
        Machine::Standard => given_state_machine(&router, row.case, &machine, STANDARD).await,
        Machine::Express => given_state_machine(&router, row.case, &machine, EXPRESS).await,
        // The one form of an ARN that names a state machine of this account: nothing
        // created it, which is the other refusal this service answers the start with.
        Machine::Absent => format!(
            "arn:aws:states:{REGION}:{}:stateMachine:{machine}",
            engine::emulator_account_id()
        ),
    };
    // The role every stage acts as: it may take the source's records, deliver to the queue,
    // start the workflow either way round, and write the pipe's own log.
    let role_arn = given_role_stating(
        &router,
        &format!("{}-role", row.case),
        SERVICE_PRINCIPAL,
        json!([
            {
                "Effect": "Allow",
                "Action": ["sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:GetQueueAttributes"],
                "Resource": source_arn,
            },
            { "Effect": "Allow", "Action": "sqs:SendMessage", "Resource": target_arn },
            {
                "Effect": "Allow",
                "Action": ["states:StartSyncExecution", "states:StartExecution"],
                "Resource": machine_arn,
            },
            {
                "Effect": "Allow",
                "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": format!("{group_arn}:log-stream:*"),
            },
        ]),
    )
    .await;
    let mut body = json!({
        "Source": source_arn,
        "RoleArn": role_arn,
        "SourceParameters": { "SqsQueueParameters": { "BatchSize": 1 } },
        "LogConfiguration": {
            "CloudwatchLogsLogDestination": { "LogGroupArn": group_arn },
            "Level": "ERROR",
            "IncludeExecutionData": ["ALL"],
        },
        "DesiredState": "RUNNING",
    });
    match row.stage {
        Stage::Enrichment => {
            body["Target"] = json!(target_arn);
            body["Enrichment"] = json!(machine_arn);
        }
        Stage::Target => {
            body["Target"] = json!(machine_arn);
            body["TargetParameters"] = json!({
                "StepFunctionStateMachineParameters": { "InvocationType": "REQUEST_RESPONSE" },
            });
        }
    }
    let created = call(
        &router,
        "POST",
        &format!("/v1/pipes/{}", row.case),
        Some(body),
        REGION,
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {:?}: CreatePipe answered {} {}",
        row.case, created.status, created.body
    );
    let pipe_arn = created.string("Arn");

    // The record the execution is about, carrying a marker no other row sends.
    let marker = format!("{}-marker", row.case);
    send(&router, &source_url, std::slice::from_ref(&marker)).await;

    let Some(message_type) = row.message_type else {
        // The control: the workflow type serves the synchronous start, so the enrichment
        // answers the events and the record reaches the target — and nothing reports a
        // refused invocation.
        let delivered = collect_at_least(&router, &target_url, 1, REFUSAL_BUDGET).await;
        assert!(
            delivered.iter().any(|body| body.contains(&marker)),
            "case {:?}: expected the target to hold the record the enrichment answered, got \
             {delivered:?}",
            row.case
        );
        no_invocation_failure_in(&router, row.case, &group).await;
        return;
    };

    let case = row.case;
    let record = record_carrying(&router, case, &group, &marker).await;
    assert_eq!(
        record["messageType"],
        json!(message_type),
        "case {case:?}: expected the record carrying the sent record to be a \
         {message_type}, got {record}"
    );
    assert_eq!(
        record["logLevel"],
        json!("ERROR"),
        "case {case:?}: expected a failure to be reported at ERROR, got {record}"
    );
    assert_eq!(
        record["resourceArn"],
        json!(pipe_arn),
        "case {case:?}: expected the record to name the executing pipe, got {record}"
    );
    assert!(
        !record["executionId"].as_str().unwrap_or_default().is_empty(),
        "case {case:?}: expected the record to belong to an execution, got {record}"
    );
    assert!(
        record["payload"]
            .as_str()
            .unwrap_or_default()
            .contains(&marker),
        "case {case:?}: expected the record the stage was handed in the record's payload, \
         got {record}"
    );

    let error = &record["error"];
    assert_eq!(
        error["awsService"],
        json!("states"),
        "case {case:?}: expected the refusal attributed to Step Functions, got {error}"
    );
    assert_eq!(
        error["httpStatusCode"],
        json!(400),
        "case {case:?}: expected the status Step Functions answered, got {error}"
    );
    assert_eq!(
        error["exceptionType"],
        json!("BadRequest"),
        "case {case:?}: expected the class of a 400 answer, got {error}"
    );
    assert_eq!(
        error["resourceArn"],
        json!(machine_arn),
        "case {case:?}: expected the resource the refused call named, got {error}"
    );
    let request_id = error["requestId"].as_str().unwrap_or_default().to_string();
    assert!(
        !request_id.is_empty(),
        "case {case:?}: expected the identifier of the answer that refused, got {error}"
    );
    let message = error["message"].as_str().unwrap_or_default();
    let head = format!("{} with error from States: ", row.opening);
    let tail = format!(
        " (Service: Sfn, Status Code: 400, Request ID: {request_id}) (SDK Attempt Count: 1)"
    );
    assert!(
        message.starts_with(&head),
        "case {case:?}: expected the message to open {head:?}, got {message:?}"
    );
    assert!(
        message.ends_with(&tail),
        "case {case:?}: expected the message to name the call it came from as {tail:?}, got \
         {message:?}"
    );
    let reason = message
        .strip_prefix(&head)
        .and_then(|rest| rest.strip_suffix(&tail))
        .unwrap_or_default();
    match row.reason {
        Some(expected) => assert_eq!(
            reason, expected,
            "case {case:?}: expected the called service's own sentence {expected:?}, got \
             {reason:?}"
        ),
        None => assert!(
            !reason.is_empty() && reason.contains(&machine_arn),
            "case {case:?}: expected the called service's own sentence about \
             {machine_arn:?}, got {reason:?}"
        ),
    }
}

/// What Step Functions answers the SYNCHRONOUS start of a `STANDARD` workflow with — the
/// refusal a pipe enriching through such a state machine fails every record on. Its text is
/// the service's own for the class and carries no value of the request.
const TYPE_NOT_SUPPORTED: &str = "This operation is not supported by this type of state machine";

/// The row the report is about: a `STANDARD` state machine as the pipe's `Enrichment`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_standard_state_machine_as_enrichment_is_reported_as_a_refused_invocation() {
    the_refusal_is_reported_for(Refused {
        case: "a-standard-enrichment",
        stage: Stage::Enrichment,
        machine: Machine::Standard,
        message_type: Some(ENRICHMENT_INVOCATION_FAILED),
        opening: "Enrichment invocation failed",
        reason: Some(TYPE_NOT_SUPPORTED),
    })
    .await;
}

/// The same refusal of the same service at the OTHER stage that calls it: a `STANDARD` state
/// machine as the pipe's `Target`, started synchronously.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_standard_state_machine_as_target_is_reported_as_a_refused_invocation() {
    the_refusal_is_reported_for(Refused {
        case: "a-standard-target",
        stage: Stage::Target,
        machine: Machine::Standard,
        message_type: Some(TARGET_INVOCATION_FAILED),
        opening: "Target invocation failed",
        reason: Some(TYPE_NOT_SUPPORTED),
    })
    .await;
}

/// ANOTHER refusal of the same service, worded about the resource: an enrichment ARN that
/// names a state machine nothing created.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_enrichment_the_called_service_does_not_hold_is_reported_as_it_answered() {
    the_refusal_is_reported_for(Refused {
        case: "an-absent-enrichment",
        stage: Stage::Enrichment,
        machine: Machine::Absent,
        message_type: Some(ENRICHMENT_INVOCATION_FAILED),
        opening: "Enrichment invocation failed",
        reason: None,
    })
    .await;
}

/// The control: the identical wiring against an `EXPRESS` twin, which serves the synchronous
/// start — the records reach the target and no record reports a refused invocation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_express_enrichment_is_refused_by_nothing_and_reports_no_failure() {
    the_refusal_is_reported_for(Refused {
        case: "an-express-enrichment",
        stage: Stage::Enrichment,
        machine: Machine::Express,
        message_type: None,
        opening: "",
        reason: None,
    })
    .await;
}
