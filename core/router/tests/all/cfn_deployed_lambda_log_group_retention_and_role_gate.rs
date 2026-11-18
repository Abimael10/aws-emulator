// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed AWS Lambda function whose stack ALSO declares the log group the
//! function logs to by default, and what the function's execution role decides about the
//! records of its runs.
//!
//! RULE under test: **a stack that declares an `AWS::Logs::LogGroup` and an
//! `AWS::Lambda::Function` that logs into it owns that group's configuration — the declared
//! `RetentionInDays` is what `DescribeLogGroups` reports for it before AND after the
//! function's runs, because an invocation's log delivery writes into the group that is already
//! there instead of re-creating it, and a group whose declaration states no retention still
//! reports none — and WHETHER an invocation's records reach that group is decided by the
//! function's execution role: Lambda makes the `logs:CreateLogGroup`, `logs:CreateLogStream`
//! and `logs:PutLogEvents` calls of the delivery for the function, so a role short of one of
//! them does not make that call, while the invocation itself is answered the same either way
//! (HTTP 200, no `FunctionError`).**
//!
//! Both halves are the same rule stated over one resource: the log group is state the owning
//! service holds, so what the stack configured on it and what a run wrote into it are
//! independent of each other — the write never resets the configuration, and the
//! configuration never admits a write the role does not grant. A group whose retention a run
//! silently reset makes retention-as-code unverifiable; a run whose records arrive whatever
//! the role grants makes the least-privilege contrast — the granted role writes, the ungranted
//! one does not — unobservable.
//!
//! `RetentionInDays` is applied by that service's own `PutRetentionPolicy` ("Sets the
//! retention of the specified log group"), and a group with no retention policy never expires:
//! "To set a log group so that its log events do not expire, use `DeleteRetentionPolicy`"
//! (`smithy_query(query="doc",shape="Days")`). A function declaring no `LoggingConfig` logs to
//! "a default log group named `/aws/lambda/<function name>`"
//! (`smithy_query(query="doc",shape="LoggingConfig",member="LogGroup")`), which is the group
//! the first table below declares under that very name.
//!
//! The rows differ along what the rule generalises over rather than restating one template:
//!
//! * the RETENTION declared — the shortest and the longest span the owning service publishes,
//!   one in between, and no retention at all (the never-expiring group);
//! * the FORM the execution role's logs grant takes — the AWS-managed basic execution policy
//!   reached by `ManagedPolicyArns`, and an inline `Policies` document naming the three calls;
//! * what an UNGRANTED role carries — nothing but its trust document, a document granting an
//!   action that is none of the three, and a document granting the two group-level calls but
//!   not the write, which is the row that shows the write itself is what is gated.
//!
//! No expected value here is a literal from a recording: each row's group name is computed
//! from the function name the row declared and checked against the name the stack publishes,
//! the ends of the retention list are quoted from that service's own model, and the marker
//! each run logs is built by the row.
//!
//! Four crates have to agree for the rule to hold — CloudFormation provisions the group, the
//! role and the function, IAM answers for what the execution role may do, AWS Lambda runs the
//! function and delivers its log, Amazon CloudWatch Logs holds the group, its retention and
//! its events — so the suite lives here rather than in any one of them. The wire harness it
//! drives is [`crate::cloudformation_lambda_function_log_destination`]'s, which states the
//! neighbouring rule about WHERE a deployed function's records land.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudformation_lambda_function_log_destination::{
    cloudformation, emulator, filtered, groups_named, invoke, messages_at_least, messages_in,
    trust_policy, wait_until_active, CREATE_COMPLETE, DEFAULT_GROUP_PREFIX, FUNCTION_TIMEOUT,
    HANDLER_SOURCE, RECORDS_PER_RUN, REPORT_PREFIX,
};
use crate::query_wire::{outputs_of, physical_ids_of};

/// The shortest, a middle and the longest retention Amazon CloudWatch Logs publishes for a log
/// group: "Possible values are: 1, 3, 5, 7, 14, 30, 60, 90, 120, 150, 180, 365, 400, 545, 731,
/// 1096, 1827, 2192, 2557, 2922, 3288, and 3653" (that model's `Days` shape,
/// `smithy_query(query="doc",shape="Days")`).
const SHORTEST_RETENTION: i64 = 1;
const MIDDLE_RETENTION: i64 = 14;
const LONGEST_RETENTION: i64 = 3653;

/// The AWS-managed policy that grants a function's own log delivery, named as a template
/// reaches it.
const MANAGED_BASIC_EXECUTION: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The three CloudWatch Logs calls an invocation's log delivery makes, under the IAM action
/// names that service publishes for them.
const CREATE_LOG_GROUP: &str = "logs:CreateLogGroup";
const CREATE_LOG_STREAM: &str = "logs:CreateLogStream";
const PUT_LOG_EVENTS: &str = "logs:PutLogEvents";

/// An action that is none of the three, carried by the row whose ungranted role holds a
/// document that grants something else entirely.
const UNRELATED_ACTION: &str = "s3:GetObject";

/// The error shape `FilterLogEvents` answers for a group CloudWatch Logs holds nothing under
/// (`smithy_query(query="operation",operation="FilterLogEvents")`), which is what a group no
/// run was allowed to create resolves to.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The logical ids the templates below declare.
const ROLE_ID: &str = "ExecutionRole";
const GROUP_ID: &str = "LogGroup";
const FUNCTION_ID: &str = "Logger";
const GRANTED_ROLE_ID: &str = "GrantedRole";
const UNGRANTED_ROLE_ID: &str = "UngrantedRole";
const GRANTED_FUNCTION_ID: &str = "GrantedFunction";
const UNGRANTED_FUNCTION_ID: &str = "UngrantedFunction";

/// The ONE group under `name`, or a panic naming what was resolved instead.
async fn group_named(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let resolved = groups_named(router, case, name).await;
    assert_eq!(
        resolved.len(),
        1,
        "case {case:?}: expected DescribeLogGroups for the prefix {name:?} to resolve exactly 1 \
         group with that exact name, got {resolved:?}"
    );
    resolved[0].clone()
}

/// The retention the group `name` reports right now, as `retentionInDays`, or `None` for a
/// group that never expires.
async fn retention_of(router: &MultiServiceRouter, case: &str, name: &str) -> Option<i64> {
    group_named(router, case, name)
        .await
        .get("retentionInDays")
        .and_then(Value::as_i64)
}

/// What an execution role is given, which is what decides whether the delivery of a run's log
/// is made at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// The AWS-managed basic execution policy, attached by `ManagedPolicyArns`.
    ManagedBasicExecution,
    /// An inline `Policies` document naming the three calls.
    InlineLogsPolicy,
    /// Nothing but the trust document: no `ManagedPolicyArns` and no `Policies` at all.
    NoPolicies,
    /// An inline document granting an action that is none of the three.
    UnrelatedPolicy,
    /// An inline document granting the two group-level calls but not the write.
    WithoutPutLogEvents,
}

impl Grant {
    /// Whether it admits `logs:CreateLogGroup`, which is what creates a destination group
    /// nothing declared.
    fn creates_the_group(self) -> bool {
        matches!(
            self,
            Self::ManagedBasicExecution | Self::InlineLogsPolicy | Self::WithoutPutLogEvents
        )
    }

    /// The `AWS::IAM::Role` a row declares for it.
    fn role(self) -> Value {
        let mut properties = Map::new();
        properties.insert("AssumeRolePolicyDocument".to_string(), trust_policy());
        let inline = |actions: Value| {
            json!([{
                "PolicyName": "delivery",
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }]
                }
            }])
        };
        match self {
            Self::ManagedBasicExecution => {
                properties.insert(
                    "ManagedPolicyArns".to_string(),
                    json!([MANAGED_BASIC_EXECUTION]),
                );
            }
            Self::InlineLogsPolicy => {
                properties.insert(
                    "Policies".to_string(),
                    inline(json!([CREATE_LOG_GROUP, CREATE_LOG_STREAM, PUT_LOG_EVENTS])),
                );
            }
            Self::NoPolicies => {}
            Self::UnrelatedPolicy => {
                properties.insert("Policies".to_string(), inline(json!([UNRELATED_ACTION])));
            }
            Self::WithoutPutLogEvents => {
                properties.insert(
                    "Policies".to_string(),
                    inline(json!([CREATE_LOG_GROUP, CREATE_LOG_STREAM])),
                );
            }
        }
        json!({ "Type": "AWS::IAM::Role", "Properties": Value::Object(properties) })
    }
}

/// The `AWS::Lambda::Function` a row declares: named, running as `role_id`, and coming up only
/// after `depends_on` where the row declares its log group in the same stack.
fn function(name: &str, role_id: &str, depends_on: Option<&str>) -> Value {
    let mut declared = Map::new();
    declared.insert("Type".to_string(), json!("AWS::Lambda::Function"));
    declared.insert(
        "Properties".to_string(),
        json!({
            "FunctionName": name,
            "Role": { "Fn::GetAtt": [role_id, "Arn"] },
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Timeout": FUNCTION_TIMEOUT,
            "Code": { "ZipFile": HANDLER_SOURCE }
        }),
    );
    if let Some(group_id) = depends_on {
        declared.insert("DependsOn".to_string(), json!(group_id));
    }
    Value::Object(declared)
}

/// The `AWS::Logs::LogGroup` a row declares under the default log path of its function, with
/// the retention it states (or none at all).
fn log_group(name: &str, retention: Option<i64>) -> Value {
    let mut properties = Map::new();
    properties.insert("LogGroupName".to_string(), json!(name));
    if let Some(days) = retention {
        properties.insert("RetentionInDays".to_string(), json!(days));
    }
    json!({ "Type": "AWS::Logs::LogGroup", "Properties": Value::Object(properties) })
}

/// Deploy one template and wait for the stack to report `CREATE_COMPLETE`, answering the
/// `DescribeStacks` body it was read from.
async fn deployed(router: &MultiServiceRouter, case: &str, stack: &str, body: &str) -> String {
    let created = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", body),
            // The templates declare IAM roles, which is acknowledged with CAPABILITY_IAM.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {} with {:?}",
        created.status,
        created.text()
    );
    let (reached, described) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {described}"
    );
    described
}

/// The value the completed stack published under `output`.
fn published(case: &str, described: &str, output: &str) -> String {
    outputs_of(described)
        .get(output)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no {output} in {described}"))
}

/// One row of the retention table: what the declared group's retention is, and in what form
/// the execution role is granted the delivery.
struct Retained {
    case: &'static str,
    /// The `FunctionName` the template states, which is also what names its default log group.
    function_name: &'static str,
    /// The `RetentionInDays` the template states, or `None` for a group that never expires.
    retention: Option<i64>,
    /// How the role is granted the delivery. Every row of this table is granted it: what an
    /// ungranted role leaves behind is the second table's rule.
    grant: Grant,
}

const RETAINED: &[Retained] = &[
    Retained {
        case: "the shortest published retention, on a role carrying the AWS-managed policy",
        function_name: "cfn-retention-shortest",
        retention: Some(SHORTEST_RETENTION),
        grant: Grant::ManagedBasicExecution,
    },
    Retained {
        case: "a retention in the middle of the published list, on a role carrying an inline \
               document",
        function_name: "cfn-retention-middle",
        retention: Some(MIDDLE_RETENTION),
        grant: Grant::InlineLogsPolicy,
    },
    Retained {
        case: "the longest published retention, on a role carrying the AWS-managed policy",
        function_name: "cfn-retention-longest",
        retention: Some(LONGEST_RETENTION),
        grant: Grant::ManagedBasicExecution,
    },
    Retained {
        case: "a group declaring no retention at all, on a role carrying an inline document",
        function_name: "cfn-retention-never-expires",
        retention: None,
        grant: Grant::InlineLogsPolicy,
    },
];

impl Retained {
    /// The group this row declares: the default log path of the function it declares.
    fn group(&self) -> String {
        format!("{DEFAULT_GROUP_PREFIX}{}", self.function_name)
    }

    /// The template: the execution role, the declared group, and the function that comes up
    /// after it.
    fn template(&self) -> String {
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                ROLE_ID: self.grant.role(),
                GROUP_ID: log_group(&self.group(), self.retention),
                FUNCTION_ID: function(self.function_name, ROLE_ID, Some(GROUP_ID))
            },
            "Outputs": {
                "FunctionName": { "Value": { "Ref": FUNCTION_ID } },
                "GroupName": { "Value": { "Ref": GROUP_ID } }
            }
        })
        .to_string()
    }
}

#[tokio::test]
async fn a_declared_log_group_keeps_its_retention_across_the_functions_own_runs() {
    // ONE emulator for every row: each row deploys a stack of its own, so the group and the
    // function a row reads are still that row's alone.
    let router = emulator();

    for (index, row) in RETAINED.iter().enumerate() {
        let case = row.case;
        let stack = format!("cfn-loggroup-retention-{index}");
        let described = deployed(&router, case, &stack, &row.template()).await;

        // 1. What the stack published for the group, which is the name a client goes looking
        // by: the group's own physical id, and the default log path of the declared function.
        let group = published(case, &described, "GroupName");
        let function = published(case, &described, "FunctionName");
        assert_eq!(
            function, row.function_name,
            "case {case:?}: expected the published name to be the declared one"
        );
        assert_eq!(
            group,
            row.group(),
            "case {case:?}: expected the published group to be the function's default log path"
        );
        let resources =
            cloudformation(&router, "DescribeStackResources", &[("StackName", &stack)]).await;
        let physical = physical_ids_of(&resources.text());
        assert_eq!(
            physical.get(GROUP_ID).map(String::as_str),
            Some(group.as_str()),
            "case {case:?}: expected the published name to be the group's physical id, got \
             {physical:?}"
        );

        // 2. Before anything has run: the declared group exists in CloudWatch Logs, carrying
        // exactly the retention the declaration asked for — and none where it asked for none,
        // which is the group whose events never expire.
        let before = retention_of(&router, case, &group).await;
        assert_eq!(
            before, row.retention,
            "case {case:?}: expected the group {group:?} to report the declared retention {:?} \
             before the first run, got {before:?}",
            row.retention
        );

        // 3. One run of the function whose default log path that group is.
        let settled = wait_until_active(&router, case, &function).await;
        assert_eq!(
            settled
                .get("LoggingConfig")
                .and_then(|logging| logging.get("LogGroup"))
                .and_then(Value::as_str),
            Some(group.as_str()),
            "case {case:?}: expected AWS Lambda to report {group:?} as the function's log group, \
             got {settled}"
        );
        let marker = format!("marker-{index}");
        invoke(&router, case, &function, &marker).await;

        // 4. The run's records are in that group — the delivery wrote into the group the stack
        // had already made, so there is still exactly one group under the name.
        let printed = format!("logged {marker}");
        let held = messages_at_least(&router, &group, RECORDS_PER_RUN).await;
        assert!(
            held.iter().any(|message| message.contains(&printed)),
            "case {case:?}: expected the group {group:?} to hold the printed line {printed:?}, \
             got {held:?}"
        );
        assert!(
            held.iter().any(|message| message.contains(REPORT_PREFIX)),
            "case {case:?}: expected the group {group:?} to hold a {REPORT_PREFIX:?} record for \
             the run, got {held:?}"
        );

        // 5. ... and the retention the stack declared is STILL what the group reports: the
        // write is not a re-create, so nothing the run did reset what the template configured.
        let after = retention_of(&router, case, &group).await;
        assert_eq!(
            after, row.retention,
            "case {case:?}: expected the group {group:?} to still report the declared retention \
             {:?} after the run, got {after:?}",
            row.retention
        );
    }
}

/// One row of the gate table: how the granted role is granted, and what the ungranted role
/// carries instead.
struct Gated {
    case: &'static str,
    /// The `FunctionName` of the function whose role grants the delivery.
    granted_function: &'static str,
    /// The `FunctionName` of its twin, which differs in nothing but its role.
    ungranted_function: &'static str,
    granted: Grant,
    ungranted: Grant,
}

const GATED: &[Gated] = &[
    Gated {
        case: "the AWS-managed policy against a role carrying nothing but its trust document",
        granted_function: "cfn-gate-managed-good",
        ungranted_function: "cfn-gate-managed-bad",
        granted: Grant::ManagedBasicExecution,
        ungranted: Grant::NoPolicies,
    },
    Gated {
        case: "an inline document naming the three calls against one granting another action",
        granted_function: "cfn-gate-inline-good",
        ungranted_function: "cfn-gate-inline-bad",
        granted: Grant::InlineLogsPolicy,
        ungranted: Grant::UnrelatedPolicy,
    },
    Gated {
        case: "the AWS-managed policy against a role granted the group calls but not the write",
        granted_function: "cfn-gate-partial-good",
        ungranted_function: "cfn-gate-partial-bad",
        granted: Grant::ManagedBasicExecution,
        ungranted: Grant::WithoutPutLogEvents,
    },
];

impl Gated {
    /// The default log path of one of the two functions.
    fn group(name: &str) -> String {
        format!("{DEFAULT_GROUP_PREFIX}{name}")
    }

    /// The template: two roles, and two otherwise identical functions on them. Neither log
    /// group is declared, so the only thing that can create one is a run's own delivery.
    fn template(&self) -> String {
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                GRANTED_ROLE_ID: self.granted.role(),
                UNGRANTED_ROLE_ID: self.ungranted.role(),
                GRANTED_FUNCTION_ID: function(self.granted_function, GRANTED_ROLE_ID, None),
                UNGRANTED_FUNCTION_ID: function(
                    self.ungranted_function,
                    UNGRANTED_ROLE_ID,
                    None
                )
            },
            "Outputs": {
                "GrantedFunctionName": { "Value": { "Ref": GRANTED_FUNCTION_ID } },
                "UngrantedFunctionName": { "Value": { "Ref": UNGRANTED_FUNCTION_ID } }
            }
        })
        .to_string()
    }
}

#[tokio::test]
async fn only_an_execution_role_granting_the_delivery_leaves_a_runs_records_behind() {
    let router = emulator();

    for (index, row) in GATED.iter().enumerate() {
        let case = row.case;
        let stack = format!("cfn-loggroup-role-gate-{index}");
        let described = deployed(&router, case, &stack, &row.template()).await;

        let granted = published(case, &described, "GrantedFunctionName");
        let ungranted = published(case, &described, "UngrantedFunctionName");
        assert_eq!(
            (granted.as_str(), ungranted.as_str()),
            (row.granted_function, row.ungranted_function),
            "case {case:?}: expected the published names to be the declared ones"
        );
        wait_until_active(&router, case, &granted).await;
        wait_until_active(&router, case, &ungranted).await;

        // 1. BOTH invocations are clean: what the role may do to CloudWatch Logs is not part of
        // answering the call, so the ungranted twin answers exactly as the granted one does —
        // HTTP 200, no `FunctionError`, the handler's own result.
        let withheld_marker = format!("withheld-{index}");
        let delivered_marker = format!("delivered-{index}");
        invoke(&router, case, &ungranted, &withheld_marker).await;
        invoke(&router, case, &granted, &delivered_marker).await;

        // 2. The granted run's records are there, which is what says both deliveries have been
        // attempted by now: the ungranted run was invoked first and each invocation makes its
        // own delivery before it answers.
        let granted_group = Gated::group(&granted);
        let printed = format!("logged {delivered_marker}");
        let held = messages_at_least(&router, &granted_group, RECORDS_PER_RUN).await;
        assert!(
            held.iter().any(|message| message.contains(&printed)),
            "case {case:?}: expected the group {granted_group:?} to hold the printed line \
             {printed:?}, got {held:?}"
        );

        // 3. The ungranted run left NOTHING to read. WHICH answer says so depends on the one
        // call the row's role is short of: a role short of `logs:CreateLogGroup` never created
        // the destination at all, so the read of it is that service's own refusal, while a role
        // granted the group-level calls has a group that exists and holds no events — so the
        // gate is per call rather than all-or-nothing.
        let ungranted_group = Gated::group(&ungranted);
        let answered = filtered(&router, &ungranted_group).await;
        let resolved = groups_named(&router, case, &ungranted_group).await;
        if row.ungranted.creates_the_group() {
            assert_eq!(
                resolved.len(),
                1,
                "case {case:?}: expected DescribeLogGroups for {ungranted_group:?} to resolve \
                 exactly 1 group for the role {:?}, got {resolved:?}",
                row.ungranted
            );
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected HTTP 200 from FilterLogEvents for the existing group \
                 {ungranted_group:?}, got {} with {:?}",
                answered.status,
                answered.text()
            );
            let events = answered
                .json()
                .get("events")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert!(
                events.is_empty(),
                "case {case:?}: expected the group {ungranted_group:?} of a function whose role \
                 does not grant the write to hold no events, got {events:?}"
            );
        } else {
            assert!(
                resolved.is_empty(),
                "case {case:?}: expected DescribeLogGroups for {ungranted_group:?} to resolve no \
                 group for the role {:?}, got {resolved:?}",
                row.ungranted
            );
            assert_ne!(
                answered.status, 200,
                "case {case:?}: expected FilterLogEvents to refuse the group \
                 {ungranted_group:?}, which nothing created, got {:?}",
                answered.text()
            );
            assert!(
                answered.text().contains(RESOURCE_NOT_FOUND),
                "case {case:?}: expected FilterLogEvents for {ungranted_group:?} to answer \
                 {RESOURCE_NOT_FOUND}, got {:?}",
                answered.text()
            );
        }

        // 4. ... and the granted twin's records went to ITS own group rather than anywhere the
        // ungranted one could be read from: the two groups are the two functions' own.
        let delivered = messages_in(&router, &granted_group).await;
        assert!(
            delivered
                .iter()
                .all(|message| !message.contains(&withheld_marker)),
            "case {case:?}: expected no record of the ungranted run in {granted_group:?}, got \
             {delivered:?}"
        );
    }
}
