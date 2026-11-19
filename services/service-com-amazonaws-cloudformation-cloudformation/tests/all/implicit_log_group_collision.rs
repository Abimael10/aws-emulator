// SPDX-License-Identifier: CC0-1.0

//! The pre-deployment EXISTENCE check over a log-group name that is already taken OUTSIDE
//! CloudFormation — the trap a deployment walks into when it declares the group a function has
//! already opened for itself.
//!
//! RULE: CloudFormation's pre-deployment validation asks the service that OWNS a type whether
//! it already holds a resource under every physical name the template STATES for a resource the
//! operation would create, whatever brought that resource into being, and a deployment that
//! fails the check is refused before anything is provisioned — so no resource of the template
//! exists afterwards and the resource that was already there is unchanged. The failure is
//! reported as the validation it is: the stack settles `ROLLBACK_COMPLETE`, its ONE stack-level
//! `CREATE_FAILED` event carries `DetailedStatus` `VALIDATION_FAILED` with the summary every
//! pre-deployment validation reports on the stack (the count of errors and the pointer at
//! `DescribeEvents`), and `DescribeEvents` carries one `VALIDATION_ERROR` result naming
//! `NAME_CONFLICT_VALIDATION`, `FAILED`, at the element of the template the resource was
//! declared at, reading CloudFormation's sentence for the collision (fixed text `Resource of
//! type '…' with identifier '…' already exists.`).
//!
//! The rows differ along what the rule generalises over: WHO holds the name — the log group a
//! function's first invocation opened for itself, left behind when the stack that declared the
//! function was deleted, and a group a client created through CloudWatch Logs' own wire — and
//! the FORM of that name (the default `/aws/lambda/<function name>` and a group the function's
//! `LoggingConfig` sends its logs to instead), beside the control row, where nothing holds the
//! name and the whole template deploys.
//!
//! Every case drives both services through one router: the CloudFormation calls over the
//! awsQuery wire, the CloudWatch Logs calls over awsJson1_1 and the AWS Lambda calls over
//! restJson1 — so what is asserted is what a client that deploys the template and then reads
//! the resources back observes.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::settle_the_store;
use crate::declared_function_logs_on_first_invoke::event_messages;
use crate::hotswapped_function_code::lambda;
use crate::logs_group_children_resources::{group_exists, logs, stream_names};
use crate::stack_declared_function_identity::{
    get_function, inline_source, trust_policy, LOOKS, LOOK_AGAIN, MANAGED_POLICY_ARN, STATE_ACTIVE,
};
use crate::stack_lifecycle::{
    acknowledging_iam, call, create_stack, describe_stack, member, router_with_engine, xml_blocks,
    xml_text, ACCOUNT_ID, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The logical ids every template below declares its resources under.
const GROUP_LOGICAL_ID: &str = "LogGroup";
const ROLE_LOGICAL_ID: &str = "ExecutionRole";
const FUNCTION_LOGICAL_ID: &str = "Fn";

/// The resource type the collision is about.
const LOG_GROUP_TYPE: &str = "AWS::Logs::LogGroup";

/// The runtime and handler every declared function here states, and so the one source file the
/// inline package carries.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// The retention the colliding template asks for, which a group the deployment never created
/// must not be given.
const DECLARED_RETENTION: i64 = 3;

/// The validation CloudFormation names for the check that a resource an operation would create
/// does not already exist. Source: `state::early_validation::NAME_CONFLICT_VALIDATION`.
const NAME_CONFLICT_VALIDATION: &str = "NAME_CONFLICT_VALIDATION";

/// The `EventType` a validation result is reported under, from the model's `EventType` enum
/// (`smithy_query(query="shape", shape="EventType")`).
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `OperationEvent$ValidationStatus` for a validation that failed, from the model's
/// `ValidationStatus` enum (`[FAILED, SKIPPED]`).
const VALIDATION_STATUS_FAILED: &str = "FAILED";

/// `StackEvent$DetailedStatus` for an operation a pre-deployment validation failed, from the
/// model's `DetailedStatus` enum (`[CONFIGURATION_COMPLETE, VALIDATION_FAILED]`).
const DETAILED_STATUS_VALIDATION_FAILED: &str = "VALIDATION_FAILED";

/// How AWS Lambda refuses a read of a function it does not hold: the modelled
/// `ResourceNotFoundException` (httpError 404) and the fixed text of its message, whose one
/// variable part is the ARN of the function the request named.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const FUNCTION_NOT_FOUND_PREFIX: &str = "Function not found: ";

/// The fixed text of the line a function's runtime closes one invocation's log with. Source:
/// the `REPORT RequestId: <id>` line Lambda writes with the run's spend.
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// The members `DescribeLogGroups` reports about a group's identity and its retention, which a
/// refused deployment must leave exactly as they were.
const REPORTED_MEMBERS: [&str; 3] = ["arn", "creationTime", "retentionInDays"];

/// CloudFormation's own sentence for a resource it cannot create because one already exists
/// under the identifier the declaration asked for: fixed text around the declared type and that
/// identifier. Source: `state::early_validation::already_exists`.
fn already_exists(resource_type: &str, identifier: &str) -> String {
    format!("Resource of type '{resource_type}' with identifier '{identifier}' already exists.")
}

/// What a stack reports for a failure of ANY of CloudFormation's pre-deployment validations:
/// the count of errors the stage found and where their detail is read. Source:
/// `state::early_validation::validation_failure_summary`.
fn summary(errors: usize) -> String {
    format!(
        "Validation failed with {errors} error(s). Call DescribeEvents to retrieve the full \
         list of issues with resource and property details, resolve each error, then retry \
         the operation."
    )
}

/// How the group whose name the colliding template declares came to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Given {
    /// A function's own first invocation opened it, and the stack that declared that function
    /// was then deleted — so the group is held by nothing CloudFormation manages.
    AnInvocationsImplicitGroup,
    /// A client created it through CloudWatch Logs' own wire.
    AClientsCreateLogGroup,
    /// Nothing holds the name: the control row, whose template deploys.
    Nothing,
}

/// One row: the function name both templates declare, where that function logs, and who
/// already holds the group at that name.
struct Row {
    case: &'static str,
    function: &'static str,
    /// The group the function's `LoggingConfig` names, or `None` to leave it to the default
    /// `/aws/lambda/<function name>`.
    logging_group: Option<&'static str>,
    given: Given,
}

const ROWS: &[Row] = &[
    Row {
        // The issue's own shape: the implicit group of a function an earlier stack declared,
        // left behind when that stack was deleted.
        case: "the default group an earlier stack's function opened on its first invocation",
        function: "implicit-collision-default",
        logging_group: None,
        given: Given::AnInvocationsImplicitGroup,
    },
    Row {
        // The same provenance at another name form: the group the function's own
        // `LoggingConfig` sends its logs to, which its first invocation opens just the same.
        case: "the vended group an earlier stack's function was configured to log to",
        function: "implicit-collision-vended",
        logging_group: Some("/aws/vendedlogs/implicit-collision-vended"),
        given: Given::AnInvocationsImplicitGroup,
    },
    Row {
        // The same name form, held by a client rather than by an invocation.
        case: "a group a client created at the function's default group name",
        function: "implicit-collision-client",
        logging_group: None,
        given: Given::AClientsCreateLogGroup,
    },
    Row {
        case: "the control: a default group name nothing holds",
        function: "implicit-collision-free",
        logging_group: None,
        given: Given::Nothing,
    },
];

/// The group the row's function logs to. Source: `LoggingConfig$LogGroup` — "The name of the
/// Amazon CloudWatch log group the function sends logs to. By default, Lambda functions send
/// logs to a default log group named /aws/lambda/<function name>".
fn group_of(row: &Row) -> String {
    row.logging_group
        .map_or_else(|| format!("/aws/lambda/{}", row.function), str::to_string)
}

/// One template body declaring `resources`, each under the logical id it is paired with.
fn declaring(resources: &[(&str, Value)]) -> String {
    let mut declared = Map::new();
    for (logical_id, declaration) in resources {
        declared.insert((*logical_id).to_string(), declaration.clone());
    }
    json!({"Resources": Value::Object(declared)}).to_string()
}

/// The execution role every template declares, carrying the AWS-managed basic-execution policy
/// — the grant that lets the function's runtime open its own log group.
fn role_declaration() -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "AssumeRolePolicyDocument": trust_policy(),
            "ManagedPolicyArns": [MANAGED_POLICY_ARN],
        },
    })
}

/// The function every template declares, named so that its default log group is the name the
/// collision is about.
fn function_declaration(row: &Row, depends_on_the_group: bool) -> Value {
    let mut properties = json!({
        "FunctionName": row.function,
        "Runtime": RUNTIME,
        "Handler": HANDLER,
        "Role": {"Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"]},
        "Code": {"ZipFile": inline_source(RUNTIME)},
    });
    if let Some(group) = row.logging_group {
        properties["LoggingConfig"] = json!({"LogGroup": group});
    }
    let mut declaration = json!({
        "Type": "AWS::Lambda::Function",
        "Properties": properties,
    });
    if depends_on_the_group {
        declaration["DependsOn"] = json!(GROUP_LOGICAL_ID);
    }
    declaration
}

/// The PRE-STEP template: the role and the function, with no log group declared at all — the
/// group is the first invocation's doing.
fn pre_step_template(row: &Row) -> String {
    declaring(&[
        (ROLE_LOGICAL_ID, role_declaration()),
        (FUNCTION_LOGICAL_ID, function_declaration(row, false)),
    ])
}

/// The COLLIDING template: the log group at the name the row's function logs to, the role, and
/// the function depending on that group — the shape a deployment that means to manage its
/// function's logs has.
fn colliding_template(row: &Row) -> String {
    declaring(&[
        (
            GROUP_LOGICAL_ID,
            json!({
                "Type": LOG_GROUP_TYPE,
                "Properties": {
                    "LogGroupName": group_of(row),
                    "RetentionInDays": DECLARED_RETENTION,
                },
            }),
        ),
        (ROLE_LOGICAL_ID, role_declaration()),
        (FUNCTION_LOGICAL_ID, function_declaration(row, true)),
    ])
}

/// What CloudWatch Logs holds for one group: the members it reports about its identity and its
/// retention, and every stream of it with the messages that stream carries.
#[derive(Debug, Default, PartialEq, Eq)]
struct GroupSnapshot {
    /// One entry per [`REPORTED_MEMBERS`] name, `None` where the group carries no such member —
    /// which is what a group with no retention reports for `retentionInDays`.
    members: Vec<(String, Option<String>)>,
    logs: Vec<(String, Vec<String>)>,
}

impl GroupSnapshot {
    /// Whether the group is there at all, which is what its identity members say.
    fn exists(&self) -> bool {
        self.members
            .iter()
            .any(|(name, value)| name == "arn" && value.is_some())
    }

    /// What the group reports for one member.
    fn member(&self, name: &str) -> Option<&String> {
        self.members
            .iter()
            .find(|(held, _)| held == name)
            .and_then(|(_, value)| value.as_ref())
    }

    /// Whether any stream of the group carries a run's closing report, which is how a delivered
    /// invocation log is recognised.
    fn carries_a_delivered_run(&self) -> bool {
        self.logs.iter().any(|(_, messages)| {
            messages
                .iter()
                .any(|line| line.starts_with(REPORT_PREFIX))
        })
    }
}

/// Read [`GroupSnapshot`] for `group` through CloudWatch Logs' own wire, which is the only
/// place the answer lives.
async fn group_snapshot(router: &MultiServiceRouter, group: &str) -> GroupSnapshot {
    let (status_code, document) = logs(
        router,
        "DescribeLogGroups",
        &json!({"logGroupNamePrefix": group}),
    )
    .await;
    let record = if status_code == 200 {
        document
            .get("logGroups")
            .and_then(Value::as_array)
            .and_then(|groups| {
                groups
                    .iter()
                    .find(|held| {
                        held.get("logGroupName").and_then(Value::as_str) == Some(group)
                    })
                    .cloned()
            })
    } else {
        None
    };
    let members = REPORTED_MEMBERS
        .iter()
        .map(|name| {
            (
                (*name).to_string(),
                record
                    .as_ref()
                    .and_then(|held| held.get(*name))
                    .map(Value::to_string),
            )
        })
        .collect();
    let mut held_logs = Vec::new();
    for stream in stream_names(router, group).await {
        let messages = event_messages(router, group, &stream).await;
        held_logs.push((stream, messages));
    }
    GroupSnapshot {
        members,
        logs: held_logs,
    }
}

/// The stack's body once it reports `expected`: looked for again until it does or the budget
/// runs out, with the transitions this store owes pulled into the past each round, so nothing
/// here depends on how fast the host runs.
async fn awaited_stack(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
    expected: &str,
) -> String {
    let mut last = String::new();
    for _ in 0..LOOKS {
        settle_the_store(router, engine).await;
        let (code, body) = describe_stack(router, stack).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeStacks({stack:?}) must answer 200, got {code}: {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        last = body;
        if reported == expected {
            return last;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    last
}

/// The `State` AWS Lambda reports for `name`, looked for again until the function is out of its
/// provisioning window — what the `FunctionActiveV2` waiter does for a client.
async fn awaited_state(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let mut last = String::new();
    for _ in 0..LOOKS {
        let answered = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &[],
            &[],
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: GetFunctionConfiguration({name:?}) must answer 200 for the function \
             the stack reports as created, got {} with {}",
            answered.status, answered.body
        );
        last = answered
            .body
            .get("State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if last == STATE_ACTIVE {
            return last;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    last
}

/// Invoke `name` once, so the run opens the function's log group for itself.
async fn invoke(router: &MultiServiceRouter, case: &str, name: &str) {
    let answered = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/invocations"),
        json!({"probe": "implicit-group"}).to_string().as_bytes(),
        &[("x-amz-invocation-type", "RequestResponse")],
    )
    .await;
    assert_eq!(
        (answered.status, answered.function_error()),
        (200, None),
        "case {case:?}: the declared function {name:?} must run to completion, got {} with {}",
        answered.status,
        answered.body
    );
}

/// Look again until `group` holds a delivered run, and answer the snapshot taken then.
async fn awaited_delivery(router: &MultiServiceRouter, group: &str) -> GroupSnapshot {
    let mut last = GroupSnapshot::default();
    for _ in 0..LOOKS {
        let snapshot = group_snapshot(router, group).await;
        if snapshot.carries_a_delivered_run() {
            return snapshot;
        }
        last = snapshot;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    last
}

/// `(LogicalResourceId, ResourceStatus, DetailedStatus, ResourceStatusReason)` for every event
/// `DescribeStackEvents` reports for `stack`, newest first.
async fn stack_events(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String, String)> {
    let (status_code, body) =
        call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: the stack's events must be readable, got {status_code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .map(|event| {
            (
                xml_text(event, "LogicalResourceId").unwrap_or_default(),
                xml_text(event, "ResourceStatus").unwrap_or_default(),
                xml_text(event, "DetailedStatus").unwrap_or_default(),
                xml_text(event, "ResourceStatusReason").unwrap_or_default(),
            )
        })
        .collect()
}

/// One validation RESULT `DescribeEvents` reports: which validation was applied to which
/// resource, how it ended, where it was applied and what it says.
#[derive(Debug, PartialEq, Eq)]
struct ValidationResult {
    logical_id: String,
    resource_type: String,
    event_type: String,
    name: String,
    status: String,
    path: String,
    reason: String,
}

/// Every validation result `DescribeEvents` carries for `stack`.
async fn validation_results(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<ValidationResult> {
    let (status_code, body) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: the operation's events must be readable, got {status_code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "ValidationName").is_some())
        .map(|event| ValidationResult {
            logical_id: xml_text(event, "LogicalResourceId").unwrap_or_default(),
            resource_type: xml_text(event, "ResourceType").unwrap_or_default(),
            event_type: xml_text(event, "EventType").unwrap_or_default(),
            name: xml_text(event, "ValidationName").unwrap_or_default(),
            status: xml_text(event, "ValidationStatus").unwrap_or_default(),
            path: xml_text(event, "ValidationPath").unwrap_or_default(),
            reason: xml_text(event, "ValidationStatusReason").unwrap_or_default(),
        })
        .collect()
}

/// The wire error code and message of a refused answer, as a restJson1 error envelope carries
/// them.
fn refusal(body: &Value) -> (String, String) {
    let code = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let message = body
        .get("Message")
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (code, message)
}

/// Make the row's group exist the way the row says, and answer the snapshot of it taken just
/// before the colliding create — the state a refused deployment must leave untouched.
async fn given(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    row: &Row,
) -> GroupSnapshot {
    let case = row.case;
    let group = group_of(row);
    match row.given {
        Given::AnInvocationsImplicitGroup => {
            let pre_step = format!("{}-pre", row.function);
            let (status_code, created) = create_stack(
                router,
                &pre_step,
                &pre_step_template(row),
                &acknowledging_iam(&[]),
            )
            .await;
            assert_eq!(
                status_code, 200,
                "case {case:?}: the pre-step CreateStack must answer 200, got {status_code} \
                 {created}"
            );
            let stack_id = xml_text(&created, "StackId").unwrap_or_default();
            let described =
                awaited_stack(router, engine, case, &pre_step, status::CREATE_COMPLETE).await;
            assert_eq!(
                xml_text(&described, "StackStatus").unwrap_or_default(),
                status::CREATE_COMPLETE,
                "case {case:?}: the pre-step stack must reach CREATE_COMPLETE, got {described}"
            );

            // The group is the FIRST INVOCATION's doing, so the function has to run before it
            // exists at all.
            let state = awaited_state(router, case, row.function).await;
            assert_eq!(
                state, STATE_ACTIVE,
                "case {case:?}: AWS Lambda must report {:?} {STATE_ACTIVE} once the pre-step \
                 stack is complete, got {state:?}",
                row.function
            );
            invoke(router, case, row.function).await;
            let delivered = awaited_delivery(router, &group).await;
            assert!(
                delivered.carries_a_delivered_run(),
                "case {case:?}: the invocation must leave its log in {group:?}, got {delivered:?}"
            );

            // ...and the stack that declared the function is taken away, leaving the group
            // behind: a name held by nothing CloudFormation manages.
            let (status_code, deleted) =
                call(router, "DeleteStack", &[member("StackName", pre_step.as_str())]).await;
            assert_eq!(
                status_code, 200,
                "case {case:?}: the pre-step DeleteStack must answer 200, got {status_code} \
                 {deleted}"
            );
            let described =
                awaited_stack(router, engine, case, &stack_id, status::DELETE_COMPLETE).await;
            assert_eq!(
                xml_text(&described, "StackStatus").unwrap_or_default(),
                status::DELETE_COMPLETE,
                "case {case:?}: the pre-step stack must reach DELETE_COMPLETE, got {described}"
            );
            assert!(
                group_exists(router, &group).await,
                "case {case:?}: the implicit group {group:?} must outlive the stack that \
                 declared the function, got none"
            );
        }
        Given::AClientsCreateLogGroup => {
            let (status_code, body) =
                logs(router, "CreateLogGroup", &json!({"logGroupName": group})).await;
            assert_eq!(
                status_code, 200,
                "case {case:?}: the setup CreateLogGroup({group:?}) must answer 200, got \
                 {status_code} {body}"
            );
        }
        Given::Nothing => {}
    }
    group_snapshot(router, &group).await
}

#[tokio::test]
async fn a_log_group_name_already_taken_outside_cloudformation_refuses_the_whole_stack() {
    for row in ROWS {
        let case = row.case;
        let (router, engine) = router_with_engine();
        let group = group_of(row);

        let before = given(&router, &engine, row).await;
        let collides = row.given != Given::Nothing;
        assert_eq!(
            before.exists(),
            collides,
            "case {case:?}: before the create, {group:?} must be held only where the row says \
             so, got {before:?}"
        );
        assert_eq!(
            before.member("retentionInDays"),
            None,
            "case {case:?}: the group that is already there carries no retention, got {before:?}"
        );

        let stack = format!("{}-collide", row.function);
        let (status_code, created) = create_stack(
            &router,
            &stack,
            &colliding_template(row),
            &acknowledging_iam(&[]),
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: CreateStack must answer 200 — the request is valid and the stack \
             exists — got {status_code} {created}"
        );

        let expected_status = if collides {
            status::ROLLBACK_COMPLETE
        } else {
            status::CREATE_COMPLETE
        };
        let described = awaited_stack(&router, &engine, case, &stack, expected_status).await;
        let settled = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            settled, expected_status,
            "case {case:?}: expected the stack to settle {expected_status}, got {settled:?} \
             ({described})"
        );

        let reason = already_exists(LOG_GROUP_TYPE, &group);
        // ...and what the STACK reports for it: the summary over the stage's one error.
        let reported = summary(1);
        let events = stack_events(&router, case, &stack).await;

        if !collides {
            // The control row: the whole template deployed, so the group is the stack's and
            // carries the retention it declared, and the function is there to be read.
            let after = group_snapshot(&router, &group).await;
            assert!(
                after.exists(),
                "case {case:?}: the deployment must leave {group:?} in CloudWatch Logs, got \
                 {after:?}"
            );
            assert_eq!(
                after.member("retentionInDays").map(String::as_str),
                Some(DECLARED_RETENTION.to_string().as_str()),
                "case {case:?}: the group the deployment created must carry the declared \
                 retention, got {after:?}"
            );
            let (code, body) = get_function(&router, row.function).await;
            assert_eq!(
                code, 200,
                "case {case:?}: GetFunction({:?}) must answer 200 for a stack that deployed, \
                 got {code} {body}",
                row.function
            );
            continue;
        }

        // The stack's own failure event is the ONE report of the failure, and it carries the
        // detailed status the model gives a validation failure together with the sentence.
        let failed: Vec<&(String, String, String, String)> = events
            .iter()
            .filter(|(logical_id, event_status, _, _)| {
                logical_id == &stack && event_status == resource_status::CREATE_FAILED
            })
            .collect();
        assert_eq!(
            failed.len(),
            1,
            "case {case:?}: expected exactly one stack-level CREATE_FAILED, got {events:?}"
        );
        let (_, _, detailed_status, event_reason) = failed[0];
        assert_eq!(
            (detailed_status.as_str(), event_reason.as_str()),
            (DETAILED_STATUS_VALIDATION_FAILED, reported.as_str()),
            "case {case:?}: expected the failure event to report \
             {DETAILED_STATUS_VALIDATION_FAILED} and {reported:?}, got {detailed_status:?} and \
             {event_reason:?}"
        );

        // Nothing was asked of any owning service: no resource of the template reports an
        // event of its own.
        let about_the_resources: Vec<&(String, String, String, String)> = events
            .iter()
            .filter(|(logical_id, _, _, _)| {
                logical_id == GROUP_LOGICAL_ID
                    || logical_id == ROLE_LOGICAL_ID
                    || logical_id == FUNCTION_LOGICAL_ID
            })
            .collect();
        assert!(
            about_the_resources.is_empty(),
            "case {case:?}: a refused deployment provisions nothing, so no resource may report \
             an event, got {about_the_resources:?}"
        );

        // ...and the detail of the failure is the validation result, at the element of the
        // template the group was declared at.
        let results = validation_results(&router, case, &stack).await;
        let expected_results = vec![ValidationResult {
            logical_id: GROUP_LOGICAL_ID.to_string(),
            resource_type: LOG_GROUP_TYPE.to_string(),
            event_type: VALIDATION_ERROR.to_string(),
            name: NAME_CONFLICT_VALIDATION.to_string(),
            status: VALIDATION_STATUS_FAILED.to_string(),
            path: format!("/Resources/{GROUP_LOGICAL_ID}"),
            reason: reason.clone(),
        }];
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got {results:?}"
        );

        // The function the template declared does not exist, because nothing was provisioned.
        let (code, body) = get_function(&router, row.function).await;
        assert_eq!(
            code, 404,
            "case {case:?}: GetFunction({:?}) must be refused for a stack that provisioned \
             nothing, got {code} {body}",
            row.function
        );
        let expected_refusal = (
            RESOURCE_NOT_FOUND.to_string(),
            format!(
                "{FUNCTION_NOT_FOUND_PREFIX}arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:{}",
                row.function
            ),
        );
        let answered = refusal(&body);
        assert_eq!(
            answered, expected_refusal,
            "case {case:?}: expected {expected_refusal:?}, got {answered:?} ({body})"
        );

        // ...and the group that was already there is exactly as it was: same identity, no
        // retention applied, its stream and its events intact.
        let after = group_snapshot(&router, &group).await;
        assert_eq!(
            after, before,
            "case {case:?}: the group the failed stack never created must be untouched, \
             expected {before:?}, got {after:?}"
        );
    }
}
