// SPDX-License-Identifier: CC0-1.0

//! A SYNCHRONOUS Express run's own history reaches the log group its state machine's
//! `loggingConfiguration` names, and from there the Lambda function a subscription filter on that
//! group names — but only once that function permits the invocation, and only for the runs that
//! happened while the filter existed.
//!
//! RULE under test: **a state machine whose `loggingConfiguration` names a CloudWatch Logs log
//! group delivers the history events of EVERY run of it — a synchronous Express run included —
//! into that group as JSON log events identifying the event and the execution it belongs to,
//! selected by the configuration's `level` and carrying the execution's own payload only where
//! `includeExecutionData` asks for it; those are ordinary ingestions, so a subscription filter on
//! the group forwards the ones its `filterPattern` selects to its destination from the moment the
//! filter exists onwards and never the batches ingested before it, and a filter whose
//! `destinationArn` names a Lambda function is CONFIGURED only where that function's own
//! resource-based policy lets CloudWatch Logs invoke it for that group — a put naming a function
//! without that grant being refused with `InvalidParameterException` and storing no filter.**
//!
//! Where each half comes from:
//!
//! * `CreateStateMachine$loggingConfiguration` "defines what execution history events are logged
//!   and where they are logged", `LoggingConfiguration$level` states which events each level
//!   logs ("ALL – Log all events. ERROR – Log all events of type ERROR and FATAL. FATAL – Log all
//!   events of type FATAL. OFF – No logging.") and `includeExecutionData` "determines whether
//!   execution data is included in your log"; `LogDestination$cloudWatchLogsLogGroup` names the
//!   group, whose `logGroupArn` "must end with `:*`". Nothing in the model makes the delivery a
//!   property of HOW a run was started, so a run started by `StartSyncExecution` — the only way
//!   an Express workflow is run to completion in one call, and one whose history this service's
//!   own reporting APIs do not serve — is logged like any other.
//! * `PutSubscriptionFilter` subscribes "to a real-time stream of log events ingested through
//!   PutLogEvents" and has them "delivered to a specific destination", which is why a filter owes
//!   nothing for what was ingested before it; `destinationArn` names a Lambda function as one
//!   destination kind, and a function is the one kind needing no `roleArn` ("To perform a
//!   PutSubscriptionFilter operation for any destination except a Lambda function, you must also
//!   have the iam:PassRole permission") because it is INVOKED, so the document
//!   `lambda:AddPermission` writes is what admits the delivery.
//!
//! Three crates have to agree for the rule to hold — Step Functions runs the workflow and vends
//! its history, CloudWatch Logs holds the group, decides the put and makes the hand-over, AWS
//! Lambda holds the destination function and its policy and runs it — so the suite lives here
//! rather than in any one of them. `crate::logs_vended_group_lambda_subscriber_permission` states
//! the put's decision over the grant a function carries; this suite states what the SYNCHRONOUS
//! run delivers and what the subscriber is handed once the put is accepted, so its rows vary the
//! configuration instead: the `level` deciding which events are delivered, whether the execution
//! data is included, which event type the filter selects, and which ARN form the invoke grant is
//! scoped to.
//!
//! The deliverable is the EFFECT at both ends: the group is read back with `FilterLogEvents`, and
//! the destination function decodes the two layers its payload is framed in and prints the
//! envelope document, which is read back out of the function's own log group — so what is checked
//! is what the function was really handed. The topology's wiring (the services on one engine, the
//! destination function's handler, execution role and deployment, the calls each service is
//! addressed over, the reading of what the function printed) is
//! `crate::logs_subscription_filter_to_lambda`'s, taken from there rather than copied.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::logs_subscription_filter_to_lambda::{
    cloudformation, dispatch, group_arn, iam, lambda, logs, logs_ok, printed_at_least,
    published_function_arn, text_member, Answered, DATA_MESSAGE, DESTINATION_ID, EXECUTION_ROLE,
    FUNCTION, FUNCTION_TIMEOUT, HANDLER_SOURCE, LOCAL_POLLS, POLL, ROLE_POLICY, TRUST_POLICY,
};
use crate::query_wire::element;

/// The error code CloudWatch Logs refuses a put it will not take with, and the sentence it words
/// a destination function it may not invoke with — fixed text for the class, carrying no value of
/// the request.
const INVALID_PARAMETER: &str = "InvalidParameterException";
const NOT_PERMITTED_MESSAGE: &str =
    "Could not execute the lambda function. Make sure you have given CloudWatch Logs permission \
     to execute your function.";

/// The principal a client grants a log subscription's invocation to, and the action it performs.
const LOGS_PRINCIPAL: &str = "logs.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The role the state machines below run as. No IAM record is created for it: a role this account
/// does not hold is evidence of nothing about what it may do, so the creates are served, and what
/// this suite is about is the delivery rather than the role's documents.
const STATE_MACHINE_ROLE: &str = "arn:aws:iam::000000000000:role/express-observability";

/// The workflow every row runs: one `Choice` over the input, a `Succeed` for the run that is
/// asked to pass and a `Fail` for the run that is asked to fail. One definition serves the
/// warm-up run and the failing runs, so the only difference between them is the input.
const DEFINITION: &str = concat!(
    r#"{"StartAt":"Branch","States":{"#,
    r#""Branch":{"Type":"Choice","Choices":[{"Variable":"$.ok","BooleanEquals":true,"#,
    r#""Next":"Done"}],"Default":"Boom"},"#,
    r#""Done":{"Type":"Succeed"},"#,
    r#""Boom":{"Type":"Fail","Error":"Rejected","Cause":"the input asked this run to fail"}}}"#
);

/// The `Fail` state's name, its `Error` and its `Cause`, which is what the events of a failing
/// run report.
const FAIL_STATE: &str = "Boom";
const FAIL_ERROR: &str = "Rejected";
const FAIL_CAUSE: &str = "the input asked this run to fail";

/// The inputs the two kinds of run are started with.
const PASSING_INPUT: &str = r#"{"ok": true}"#;
const FAILING_INPUT: &str = r#"{"ok": false}"#;

/// The history event types this suite reasons about, as the vended record's `type` spells them.
const EXECUTION_STARTED: &str = "ExecutionStarted";
const FAIL_STATE_ENTERED: &str = "FailStateEntered";
const EXECUTION_FAILED: &str = "ExecutionFailed";

/// The `status` `StartSyncExecution` reports for a run that reached its `Fail` state, and for one
/// that succeeded (`SyncExecutionStatus`).
const FAILED_STATUS: &str = "FAILED";
const SUCCEEDED_STATUS: &str = "SUCCEEDED";

/// The ARN form an invoke grant is scoped to the row's log group by. Both name the same group:
/// the one ending in the `:*` that covers the group's streams, which is the form the documented
/// permission for this wiring is scoped to, and the one that does not.
#[derive(Debug, Clone, Copy)]
enum Scope {
    Streams,
    Group,
}

impl Scope {
    fn source_arn(self, account: &str, group: &str) -> String {
        let streams = group_arn(account, group);
        match self {
            Scope::Streams => streams.clone(),
            Scope::Group => streams
                .strip_suffix(":*")
                .unwrap_or(&streams)
                .to_string(),
        }
    }
}

/// One row: a logging configuration, the event type the row's filter selects out of what that
/// configuration delivers, and how the destination function's invoke grant is scoped.
struct Row {
    case: &'static str,
    /// Names this row's log group, state machine and subscription filter, so no two rows share
    /// one.
    name: &'static str,
    /// `LoggingConfiguration$level`.
    level: &'static str,
    /// `LoggingConfiguration$includeExecutionData`.
    include_execution_data: bool,
    /// The `type` this row's `filterPattern` selects.
    selected_type: &'static str,
    /// Members the selected record's `details` carries whatever the configuration says — the
    /// TRANSITION rather than the payload — with the value each must have.
    detail_members: &'static [(&'static str, &'static str)],
    /// The member of that record's `details` carrying the execution's own payload, which is
    /// delivered only where `includeExecutionData` asks for it, or `None` for a record whose
    /// detail has no payload member at all.
    payload_member: Option<&'static str>,
    scope: Scope,
    /// Event types a failing run must be reported in the group by at this `level`.
    holds: &'static [&'static str],
    /// Event types this `level` does not deliver.
    lacks: &'static [&'static str],
}

/// The rows. Each level's selection is the one `LogLevel` documents, so the event types a row
/// expects and the ones it rules out are read off that: `ALL` logs every event, `ERROR` the
/// ERROR and FATAL ones (entering a `Fail` state and the execution's own failure), `FATAL` only
/// the execution's own end.
const ROWS: &[Row] = &[
    Row {
        case: "level ALL with execution data, a filter selecting the fail-state entry, the grant \
               scoped to the group's streams",
        name: "express-all",
        level: "ALL",
        include_execution_data: true,
        selected_type: FAIL_STATE_ENTERED,
        detail_members: &[("name", FAIL_STATE)],
        payload_member: Some("input"),
        scope: Scope::Streams,
        holds: &[EXECUTION_STARTED, FAIL_STATE_ENTERED, EXECUTION_FAILED],
        lacks: &[],
    },
    Row {
        case: "level ERROR without execution data, a filter selecting the fail-state entry, the \
               grant scoped to the group itself",
        name: "express-error",
        level: "ERROR",
        include_execution_data: false,
        selected_type: FAIL_STATE_ENTERED,
        detail_members: &[("name", FAIL_STATE)],
        payload_member: Some("input"),
        scope: Scope::Group,
        holds: &[FAIL_STATE_ENTERED, EXECUTION_FAILED],
        lacks: &[EXECUTION_STARTED],
    },
    Row {
        case: "level FATAL with execution data, a filter selecting the execution's own failure, \
               the grant scoped to the group's streams",
        name: "express-fatal",
        level: "FATAL",
        include_execution_data: true,
        selected_type: EXECUTION_FAILED,
        detail_members: &[("error", FAIL_ERROR), ("cause", FAIL_CAUSE)],
        payload_member: None,
        scope: Scope::Streams,
        holds: &[EXECUTION_FAILED],
        lacks: &[EXECUTION_STARTED, FAIL_STATE_ENTERED],
    },
];

impl Row {
    /// The vended log group this row's state machine logs into, under the prefix a Step Functions
    /// vended log group carries.
    fn group(&self) -> String {
        format!("/aws/vendedlogs/states/{}", self.name)
    }

    /// This row's subscription filter, and the pattern it selects its event type with: a
    /// condition over the `type` member of the JSON log event the delivery writes.
    fn filter(&self) -> String {
        format!("{}-to-lambda", self.name)
    }

    fn pattern(&self) -> String {
        format!("{{$.type = \"{}\"}}", self.selected_type)
    }

    /// The statement id the invoke permission for this row's group is added under.
    fn statement_id(&self) -> String {
        format!("{}-logs-invoke", self.name)
    }
}

/// CloudWatch Logs, AWS Lambda, AWS CloudFormation, IAM and AWS Step Functions on ONE engine, the
/// shape `core/router/src/lib.rs` mounts them in. The sibling suite's wiring carries the first
/// four; this flow adds the service that vends the logs, so the router is assembled here.
///
/// Request authentication is off, as in the sibling suite: each call carries a credential scope so
/// the router can name the target service, and SigV4 itself is not under test.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        engine,
    ));
    router
}

/// One AWS Step Functions call over its `awsJson1_0` wire. The `sync-` host prefix the model
/// declares on `StartSyncExecution` is a DNS detail of the real endpoint; what routes a request
/// here is its credential scope, so one caller serves every operation.
async fn states(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answered {
    dispatch(
        router,
        "states",
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", &format!("AWSStepFunctions.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// One Step Functions call that must succeed.
async fn states_ok(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let answered = states(router, operation, body).await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from {operation}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// The error code an awsJson answer carries, as its envelope states it (`__type`, which may be
/// qualified by the shape's namespace).
fn error_code(answered: &Value) -> String {
    let stated = answered
        .get("__type")
        .and_then(Value::as_str)
        .or_else(|| answered.get("code").and_then(Value::as_str))
        .unwrap_or_default();
    stated.rsplit('#').next().unwrap_or(stated).to_string()
}

/// The filter names CloudWatch Logs reports for `group`.
async fn filters_of(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<Value> {
    let page = logs_ok(
        router,
        case,
        "DescribeSubscriptionFilters",
        json!({ "logGroupName": group }),
    )
    .await;
    page.get("subscriptionFilters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Every log event the group `group` holds, parsed as the JSON document a vended Step Functions
/// record is. A group nothing has written to is reported as holding nothing.
async fn vended_records(router: &MultiServiceRouter, group: &str) -> Vec<Value> {
    let answered = logs(router, "FilterLogEvents", json!({ "logGroupName": group })).await;
    if answered.status != 200 {
        return Vec::new();
    }
    answered
        .json()
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|event| event.get("message").and_then(Value::as_str).map(str::to_string))
        .filter_map(|message| serde_json::from_str::<Value>(&message).ok())
        .collect()
}

/// The string at `member` of `record`, or `None` where it carries none.
fn member(record: &Value, name: &str) -> Option<String> {
    record
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The records of ONE execution, in the order the group reports them.
fn of_execution(records: &[Value], execution_arn: &str) -> Vec<Value> {
    records
        .iter()
        .filter(|record| member(record, "execution_arn").as_deref() == Some(execution_arn))
        .cloned()
        .collect()
}

/// The `type` of each of those records.
fn types_of(records: &[Value]) -> Vec<String> {
    records
        .iter()
        .filter_map(|record| member(record, "type"))
        .collect()
}

/// Wait until the group holds a record of `execution_arn` for every type in `wanted`, and
/// answer every record of that execution the group then holds. The delivery is owed by the
/// service that ran the workflow, so this waits for the CONDITION; the budget is far longer than
/// the span either service chooses, so a loaded host is slower without the rule being any
/// different.
async fn awaited_records(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    execution_arn: &str,
    wanted: &[&str],
) -> Vec<Value> {
    let mut records = vended_records(router, group).await;
    for _ in 0..LOCAL_POLLS {
        let mine = of_execution(&records, execution_arn);
        let held = types_of(&mine);
        if wanted
            .iter()
            .all(|type_name| held.iter().any(|found| found == type_name))
        {
            return mine;
        }
        tokio::time::sleep(POLL).await;
        records = vended_records(router, group).await;
    }
    panic!(
        "case {case:?}: expected the vended group {group:?} to hold {wanted:?} records of \
         {execution_arn:?}, it holds {:?}",
        types_of(&records)
    );
}

/// Deploy the destination function every row's filter points at, and answer its ARN once AWS
/// Lambda reports it invocable. A CloudFormation template is what archives inline source into a
/// deployment package this emulator can run, so the function comes from a one-resource stack; the
/// handler, the role documents and the two waits the deployment owes are the sibling suite's.
async fn deploy_destination(router: &MultiServiceRouter) -> String {
    let created = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    let role_arn = element(&created, "Arn");
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", EXECUTION_ROLE),
            ("PolicyName", "basic-execution"),
            ("PolicyDocument", ROLE_POLICY),
        ],
    )
    .await;

    let stack = "sfn-express-sync-subscriber";
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            DESTINATION_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Role": role_arn,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
        "Outputs": {
            "FunctionArn": { "Value": json!({ "Fn::GetAtt": [DESTINATION_ID, "Arn"] }) },
        },
    })
    .to_string();
    cloudformation(
        router,
        "setup",
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    published_function_arn(router, "setup", stack).await
}

/// Grant `logs.amazonaws.com` the invocation of the destination function for THIS row's log
/// group, scoped in the row's own ARN form — the document `lambda:AddPermission` writes, which is
/// what admits a Lambda-destination hand-over.
async fn grant_invocation(router: &MultiServiceRouter, row: &Row, group: &str, account: &str) {
    let case = row.case;
    let answered = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": row.statement_id(),
            "Action": INVOKE_ACTION,
            "Principal": LOGS_PRINCIPAL,
            "SourceArn": row.scope.source_arn(account, group),
            "SourceAccount": account,
        })),
    )
    .await;
    assert_eq!(
        answered.status,
        201,
        "case {case:?}: expected HTTP 201 from AddPermission on {FUNCTION}, got {} with {:?}",
        answered.status,
        answered.text()
    );
}

/// Create this row's EXPRESS state machine, logging into its own vended group, and answer its
/// ARN.
async fn created_machine(router: &MultiServiceRouter, row: &Row, account: &str) -> String {
    let created = states_ok(
        router,
        row.case,
        "CreateStateMachine",
        &json!({
            "name": row.name,
            "definition": DEFINITION,
            "roleArn": STATE_MACHINE_ROLE,
            "type": "EXPRESS",
            "loggingConfiguration": {
                "level": row.level,
                "includeExecutionData": row.include_execution_data,
                // `logGroupArn` "must end with :*", which is the form the group reports.
                "destinations": [{
                    "cloudWatchLogsLogGroup": {
                        "logGroupArn": group_arn(account, &row.group()),
                    },
                }],
            },
        }),
    )
    .await;
    created
        .get("stateMachineArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: a created state machine carries an ARN, got {created}",
                row.case
            )
        })
        .to_string()
}

/// Run one synchronous Express execution and answer `(executionArn, status)`.
async fn ran(
    router: &MultiServiceRouter,
    case: &str,
    machine: &str,
    name: &str,
    input: &str,
) -> (String, String) {
    let answered = states_ok(
        router,
        case,
        "StartSyncExecution",
        &json!({ "stateMachineArn": machine, "name": name, "input": input }),
    )
    .await;
    let arn = answered
        .get("executionArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: a sync run reports its ARN, got {answered}"))
        .to_string();
    let status = answered
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: a sync run reports its status, got {answered}"))
        .to_string();
    (arn, status)
}

/// The envelopes the destination function printed that name `group`, out of every envelope it has
/// printed so far.
fn envelopes_naming(envelopes: &[Value], group: &str) -> Vec<Value> {
    envelopes
        .iter()
        .filter(|envelope| envelope.get("logGroup").and_then(Value::as_str) == Some(group))
        .cloned()
        .collect()
}

/// The messages one envelope carries, parsed as the JSON documents the vended records are.
fn carried_records(case: &str, envelope: &Value) -> Vec<Value> {
    envelope
        .get("logEvents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|event| {
            let message = event
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: an envelope's event carries a message, got {event}")
                });
            serde_json::from_str::<Value>(message).unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: a vended record is a JSON document, got {error} in {message:?}"
                )
            })
        })
        .collect()
}

#[tokio::test]
async fn a_sync_express_runs_vended_failures_reach_its_permitted_lambda_subscriber() {
    let router = emulator();
    let account = engine::emulator_account_id();
    let function_arn = deploy_destination(&router).await;

    // How many envelopes the destination function owes once the rows processed so far have each
    // had their one forwarded batch: one per row, so a row's own hand-over is waited for rather
    // than assumed.
    let mut owed = 0usize;

    for row in ROWS {
        let case = row.case;
        let group = row.group();
        let filter = row.filter();
        let pattern = row.pattern();

        logs_ok(
            &router,
            case,
            "CreateLogGroup",
            json!({ "logGroupName": group }),
        )
        .await;

        // The put the destination function does not permit yet. A Lambda destination carries no
        // `roleArn`: the function is invoked, so its own resource policy is what admits the
        // delivery, and this one grants nothing.
        let refused = logs(
            &router,
            "PutSubscriptionFilter",
            json!({
                "logGroupName": group,
                "filterName": filter,
                "filterPattern": pattern,
                "destinationArn": function_arn,
            }),
        )
        .await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: expected the put to be refused with HTTP 400, got {} with {:?}",
            refused.status,
            refused.text()
        );
        let answered = refused.json();
        assert_eq!(
            error_code(&answered),
            INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {answered}"
        );
        assert_eq!(
            answered.get("message").and_then(Value::as_str),
            Some(NOT_PERMITTED_MESSAGE),
            "case {case:?}: expected the refusal {NOT_PERMITTED_MESSAGE:?}, got {answered}"
        );
        assert!(
            filters_of(&router, case, &group).await.is_empty(),
            "case {case:?}: a refused put stores no subscription filter"
        );

        let machine = created_machine(&router, row, account).await;

        // The warm-up run, which succeeds: it proves the group really takes this state machine's
        // vended events, and it is the run no failure surface may ever report.
        let (warm_up, warm_up_status) =
            ran(&router, case, &machine, "warm-up", PASSING_INPUT).await;
        assert_eq!(
            warm_up_status, SUCCEEDED_STATUS,
            "case {case:?}: the warm-up run is asked to succeed, got {warm_up_status}"
        );

        // The first failing run, while nothing is subscribed.
        let (unwired, unwired_status) =
            ran(&router, case, &machine, "fail-unwired", FAILING_INPUT).await;
        assert_eq!(
            unwired_status, FAILED_STATUS,
            "case {case:?}: the run is asked to fail, got {unwired_status}"
        );
        let unwired_records = awaited_records(&router, case, &group, &unwired, row.holds).await;
        let unwired_types = types_of(&unwired_records);
        for wanted in row.holds {
            assert!(
                unwired_types.iter().any(|found| found == wanted),
                "case {case:?}: level {:?} delivers {wanted:?} for a failing run, the group holds \
                 {unwired_types:?}",
                row.level
            );
        }
        for unwanted in row.lacks {
            assert!(
                !unwired_types.iter().any(|found| found == unwanted),
                "case {case:?}: level {:?} delivers no {unwanted:?}, the group holds \
                 {unwired_types:?}",
                row.level
            );
        }

        // The warm-up run's own records, read off the same group: a run that succeeded entered no
        // `Fail` state and did not fail, so neither failure event may be reported for it — at
        // every level, including the one that delivers everything.
        let warm_up_types = types_of(&of_execution(
            &vended_records(&router, &group).await,
            &warm_up,
        ));
        for failure in [FAIL_STATE_ENTERED, EXECUTION_FAILED] {
            assert!(
                !warm_up_types.iter().any(|found| found == failure),
                "case {case:?}: the run that succeeded owes no {failure:?} record, the group \
                 holds {warm_up_types:?} for it"
            );
        }
        if row.level == "ALL" {
            assert!(
                warm_up_types.iter().any(|found| found == EXECUTION_STARTED),
                "case {case:?}: level ALL delivers the warm-up run's {EXECUTION_STARTED}, the \
                 group holds {warm_up_types:?} for it"
            );
        }

        // Nothing was subscribed while either run was ingested, so the function has been handed
        // no envelope naming this group. The reading is of what it has printed for every row so
        // far, and a hand-over still in flight could only name an EARLIER row's group.
        let (printed, other_lines) = printed_at_least(&router, owed).await;
        assert!(
            envelopes_naming(&printed, &group).is_empty(),
            "case {case:?}: no filter existed while those runs were ingested, so the function was \
             handed nothing for {group:?}, got {printed:?} beside {other_lines:?}"
        );

        // The grant the put needs, scoped to this row's group in this row's ARN form, and the
        // same put again — now accepted and reported back.
        grant_invocation(&router, row, &group, account).await;
        logs_ok(
            &router,
            case,
            "PutSubscriptionFilter",
            json!({
                "logGroupName": group,
                "filterName": filter,
                "filterPattern": pattern,
                "destinationArn": function_arn,
            }),
        )
        .await;
        let configured = filters_of(&router, case, &group).await;
        assert_eq!(
            configured.len(),
            1,
            "case {case:?}: the accepted put stores exactly one filter, got {configured:?}"
        );
        assert_eq!(
            member(&configured[0], "filterName").as_deref(),
            Some(filter.as_str()),
            "case {case:?}: the stored filter is the one that was put, got {configured:?}"
        );
        assert_eq!(
            member(&configured[0], "filterPattern").as_deref(),
            Some(pattern.as_str()),
            "case {case:?}: the stored filter carries the pattern it was put with, got \
             {configured:?}"
        );
        assert_eq!(
            member(&configured[0], "destinationArn").as_deref(),
            Some(function_arn.as_str()),
            "case {case:?}: the stored filter names the destination it was put with, got \
             {configured:?}"
        );

        // The second failing run, now that the filter exists.
        let (wired, wired_status) = ran(&router, case, &machine, "fail-wired", FAILING_INPUT).await;
        assert_eq!(
            wired_status, FAILED_STATUS,
            "case {case:?}: the run is asked to fail, got {wired_status}"
        );
        let wired_records = awaited_records(&router, case, &group, &wired, row.holds).await;
        assert_eq!(
            types_of(&wired_records)
                .iter()
                .filter(|found| *found == row.selected_type)
                .count(),
            1,
            "case {case:?}: the run reports its {:?} once, the group holds {:?} for it",
            row.selected_type,
            types_of(&wired_records)
        );

        // The hand-over: this row's group owes exactly one envelope, carrying exactly the records
        // the filter's pattern selects out of that run's batch — and none of the records ingested
        // before the filter existed.
        owed += 1;
        let (printed, other_lines) = printed_at_least(&router, owed).await;
        let mine = envelopes_naming(&printed, &group);
        assert_eq!(
            mine.len(),
            1,
            "case {case:?}: expected exactly one envelope naming {group:?}, got {printed:?} \
             beside {other_lines:?}"
        );
        let envelope = &mine[0];
        assert_eq!(
            text_member(case, envelope, "messageType"),
            DATA_MESSAGE,
            "case {case:?}: an envelope carrying ingested log events is a {DATA_MESSAGE}, got \
             {envelope}"
        );
        assert_eq!(
            text_member(case, envelope, "owner"),
            account,
            "case {case:?}: the envelope's owner is the account holding the log group, got \
             {envelope}"
        );
        let selecting: Vec<String> = envelope
            .get("subscriptionFilters")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            selecting,
            vec![filter.clone()],
            "case {case:?}: the envelope names exactly the filter that selected the events, got \
             {envelope}"
        );

        let carried = carried_records(case, envelope);
        assert_eq!(
            types_of(&carried),
            vec![row.selected_type.to_string()],
            "case {case:?}: expected the envelope to carry exactly what the pattern {pattern:?} \
             selects, got {carried:?}"
        );
        let belongs: Vec<Option<String>> = carried
            .iter()
            .map(|record| member(record, "execution_arn"))
            .collect();
        assert_eq!(
            belongs,
            vec![Some(wired.clone())],
            "case {case:?}: the forwarded record belongs to the run started while the filter \
             existed, got {carried:?}"
        );
        for earlier in [&warm_up, &unwired] {
            assert!(
                !belongs.iter().any(|arn| arn.as_deref() == Some(earlier.as_str())),
                "case {case:?}: a filter forwards nothing ingested before it existed, so no \
                 record of {earlier:?} may be in {carried:?}"
            );
        }

        // The record itself: the transition members are delivered whatever the configuration
        // says, and the execution's own payload only where `includeExecutionData` asks for it.
        let record = &carried[0];
        let details = record
            .get("details")
            .and_then(Value::as_object)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the selected record carries its details, got {record}")
            });
        for (name, expected) in row.detail_members {
            assert_eq!(
                details.get(*name).and_then(Value::as_str),
                Some(*expected),
                "case {case:?}: the record's details carry {name:?} == {expected:?}, got {record}"
            );
        }
        if let Some(payload) = row.payload_member {
            assert_eq!(
                details.contains_key(payload),
                row.include_execution_data,
                "case {case:?}: includeExecutionData is {}, so the details {} carry {payload:?}, \
                 got {record}",
                row.include_execution_data,
                if row.include_execution_data {
                    "must"
                } else {
                    "must not"
                }
            );
        }
        assert_eq!(
            member(record, "type").as_deref(),
            Some(row.selected_type),
            "case {case:?}: the forwarded record is the type the pattern selects, got {record}"
        );
        assert!(
            member(record, "id").is_some_and(|id| !id.is_empty()),
            "case {case:?}: a vended record identifies the history event it mirrors, got {record}"
        );
    }
}
