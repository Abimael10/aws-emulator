// SPDX-License-Identifier: CC0-1.0

//! The `AWS::Logs::LogGroup` a stack declares, read back through Amazon CloudWatch Logs' own
//! `DescribeLogGroups` — and then written into by the `AWS::Lambda::Function` declared beside
//! it.
//!
//! RULE: a stack reaches a successful terminal status only once every resource it declares
//! really exists in the service that OWNS it, so a declared log group is one CloudWatch Logs
//! holds under the declared `LogGroupName` — reported by `DescribeLogGroups` filtered on that
//! name, carrying the declared retention and the tags the declaration and the stack gave it —
//! from the moment the stack reports the deployment complete, and it is the group a function
//! logging there writes into, whatever its execution role may do: a role short of
//! `logs:CreateLogGroup` has nothing left to create, and the deployment's retention is not
//! undone by the run that writes to the group.
//!
//! That is the shape a deployment takes to control a function's retention without granting
//! `logs:CreateLogGroup`: declare the group at the name the function logs to, grant the role
//! only the writes. A stack that reports the group created while CloudWatch Logs holds nothing
//! under that name leaves the client reading an empty `DescribeLogGroups`, the declared
//! retention unapplied, and a least-privilege role with nowhere to write.
//!
//! The rows differ along what the rule generalises over: whether the declared name is the
//! function's DEFAULT log group (`/aws/lambda/<function name>`) or the one its `LoggingConfig`
//! names, the retention the declaration states (the shortest the owning service takes, one
//! from the middle of its catalogue, the longest, and none at all), how many tags the
//! declaration carries beside the stack's own, the runtime family that writes the line, and
//! what the execution role may do on the group — the writes only, or the whole of AWS's own
//! managed basic-execution policy, which may create a group the stack has already made.
//!
//! Every expected value is computed here from what the row declares plus the identifiers the
//! stack publishes through its outputs; nothing is read out of the answer being checked.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_table_resources::failure_reasons;
// ONE spelling of a CloudWatch Logs call for this target, and the reads built on it, owned by
// the suites that first needed them: the group's own description and its tags, the streams it
// holds, and every message it holds.
use crate::custom_resource_provider_denied_rollback::logged_lines;
use crate::logs_and_iot_resources::{described_group, group_tags};
use crate::logs_group_children_resources::{logs, stream_names};
// The sibling suite over the log group a declared function's first invocation opens waits on
// the same function, invokes it the same way and waits for the same delivery, so waiting and
// invoking are ITS definitions rather than a second copy of them here.
use crate::declared_function_logs_on_first_invoke::{awaited_state, delivered_log, invoke};
use crate::stack_declared_function_identity::{
    settled_stack, trust_policy, MANAGED_POLICY_ARN, STATE_ACTIVE,
};
use crate::stack_lifecycle::{
    acknowledging_iam, create_stack, member, output_value, router, ACCOUNT_ID, REGION,
};
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The logical ids every template below declares its resources under.
const ROLE_LOGICAL_ID: &str = "ExecutionRole";
const GROUP_LOGICAL_ID: &str = "LogGroup";
const FUNCTION_LOGICAL_ID: &str = "Fn";

/// The output keys every template publishes the identifiers the stack decided under.
const FUNCTION_NAME_OUTPUT: &str = "FunctionName";
const GROUP_NAME_OUTPUT: &str = "GroupName";
const GROUP_ARN_OUTPUT: &str = "GroupArn";

/// The stack-level tag every row supplies, which CloudFormation propagates to the resources it
/// creates that are taggable — of these three, the log group and the role.
const STACK_TAG: (&str, &str) = ("Deployment", "declared-log-group-suite");

/// The IAM action namespace CloudWatch Logs' calls are named in, and the two calls a function's
/// runtime makes to deliver one invocation's log into a group that ALREADY exists. Source: the
/// actions AWS's own managed execution-role policy for a function
/// (`AWSLambdaBasicExecutionRole`) grants, less the `CreateLogGroup` a declared group leaves
/// nothing to do.
const LOGS_NAMESPACE: &str = "logs";
const WRITES_ONLY: [&str; 2] = ["CreateLogStream", "PutLogEvents"];

/// The fixed text of the framing a function's runtime writes around one run's own output.
/// Source: the lines Lambda writes for one invocation — `START RequestId: <id> Version:
/// <version>`, `END RequestId: <id>` and `REPORT RequestId: <id>` with the run's spend.
const START_PREFIX: &str = "START RequestId: ";
const REPORT_PREFIX: &str = "REPORT RequestId: ";

/// Where the row's template declares its log group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    /// At the function's DEFAULT log group, `/aws/lambda/<function name>` — the shape the
    /// reported case has, and the one that needs no `LoggingConfig` at all.
    ///
    /// Source: `LoggingConfig$LogGroup` — "By default, Lambda functions send logs to a default
    /// log group named /aws/lambda/<function name>".
    Default,
    /// At a group of the deployment's own, which the function's `LoggingConfig` then names.
    Vended(&'static str),
}

/// What the declared execution role may do on the declared group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// An inline document stating the two WRITES on the group's own ARN and nothing else: a
    /// least-privilege role that could not create the group if it had to.
    WritesOnly,
    /// `ManagedPolicyArns` naming AWS's managed basic-execution policy, which grants
    /// `logs:CreateLogGroup` as well — so the run may re-create a group the stack has already
    /// made, and the declared retention must survive that.
    AwsManaged,
}

/// One row of the table.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `FunctionName` the declaration states, which is also what the default group's name
    /// is built from.
    function: &'static str,
    group: Group,
    /// The `RetentionInDays` the declaration states, or `None` for a group declared with none.
    ///
    /// Source: the `Days` shape's own documentation — "Possible values are: 1, 3, 5, 7, 14, 30,
    /// 60, 90, 120, 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557, 2922, 3288, and
    /// 3653".
    retention: Option<i64>,
    /// The tags the declaration carries, beside [`STACK_TAG`].
    tags: &'static [(&'static str, &'static str)],
    runtime: &'static str,
    grant: Grant,
    /// The one line the handler writes, which has to be readable from the declared group.
    marker: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        // The reported case's own shape: the function's default group, declared with the
        // shortest retention the owning service takes, on a role that may not create a group.
        case: "the function's default group at the shortest retention, on a role that may not \
               create one",
        stack: "declared-group-default-one-day",
        function: "declared-group-one-day-fn",
        group: Group::Default,
        retention: Some(1),
        tags: &[("Owner", "platform")],
        runtime: "python3.12",
        grant: Grant::WritesOnly,
        marker: "declared-group-one-day-ran",
    },
    Row {
        // The same shape with no retention declared at all: the group exists, and reports no
        // retention rather than one of this suite's choosing.
        case: "the function's default group with no retention declared",
        stack: "declared-group-default-no-retention",
        function: "declared-group-kept-fn",
        group: Group::Default,
        retention: None,
        tags: &[],
        runtime: "nodejs20.x",
        grant: Grant::WritesOnly,
        marker: "declared-group-kept-ran",
    },
    Row {
        // A role carrying the managed basic-execution policy, which MAY create the group: the
        // retention the deployment applied is not undone by the run.
        case: "the function's default group at a mid-catalogue retention, on the managed \
               basic-execution policy",
        stack: "declared-group-default-fourteen",
        function: "declared-group-fourteen-fn",
        group: Group::Default,
        retention: Some(14),
        tags: &[("Tier", "audit")],
        runtime: "python3.13",
        grant: Grant::AwsManaged,
        marker: "declared-group-fourteen-ran",
    },
    Row {
        // A group of the deployment's own at the longest published retention, with two declared
        // tags beside the stack's: the rule holds for the group the `LoggingConfig` names too.
        case: "a vended group at the longest published retention, named by the function's \
               LoggingConfig",
        stack: "declared-group-vended-longest",
        function: "declared-group-vended-fn",
        group: Group::Vended("/aws/vendedlogs/declared-group-vended"),
        retention: Some(3653),
        tags: &[("Owner", "platform"), ("Tier", "audit")],
        runtime: "nodejs22.x",
        grant: Grant::WritesOnly,
        marker: "declared-group-vended-ran",
    },
];

/// The group the row's function logs to: the one its `LoggingConfig` names, else the default
/// built from the name the declaration states.
fn log_group_of(row: &Row) -> String {
    match row.group {
        Group::Default => format!("/aws/lambda/{}", row.function),
        Group::Vended(name) => name.to_string(),
    }
}

/// The handler the declaration names, which decides the file the inline source is archived
/// under: `index` plus the extension the declared runtime loads by default.
const HANDLER: &str = "index.handler";

/// The inline source the row's function is declared with, which writes its one line through the
/// runtime's own console — so the line read back out of the declared group is the handler's,
/// not this suite's.
fn inline_source(row: &Row) -> String {
    if row.runtime.starts_with("python") {
        format!(
            "def handler(event, context):\n    print('{}')\n    return {{'ok': True}}\n",
            row.marker
        )
    } else {
        format!(
            "exports.handler = async () => {{\n    console.log('{}');\n    return {{ ok: true \
             }};\n}};\n",
            row.marker
        )
    }
}

/// The template a row deploys: the log group with the retention and tags it declares, the
/// execution role with the row's grant on it, and the function that logs there — with the
/// outputs that publish the identifiers the stack decided.
fn template(row: &Row) -> String {
    let group = log_group_of(row);

    let mut group_properties = json!({"LogGroupName": group});
    if let Some(days) = row.retention {
        group_properties["RetentionInDays"] = json!(days);
    }
    if !row.tags.is_empty() {
        group_properties["Tags"] = Value::Array(
            row.tags
                .iter()
                .map(|(key, value)| json!({"Key": key, "Value": value}))
                .collect(),
        );
    }

    let mut role_properties = json!({"AssumeRolePolicyDocument": trust_policy()});
    match row.grant {
        Grant::WritesOnly => {
            let actions: Vec<String> = WRITES_ONLY
                .iter()
                .map(|action| format!("{LOGS_NAMESPACE}:{action}"))
                .collect();
            role_properties["Policies"] = json!([{
                "PolicyName": "write-the-declared-group",
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": actions,
                        // The group's own ARN, as the type publishes it: the grant is scoped to
                        // the group the stack made and carries no create of its own.
                        "Resource": {"Fn::GetAtt": [GROUP_LOGICAL_ID, "Arn"]},
                    }],
                },
            }]);
        }
        Grant::AwsManaged => {
            role_properties["ManagedPolicyArns"] = json!([MANAGED_POLICY_ARN]);
        }
    }

    let mut function_properties = json!({
        "FunctionName": row.function,
        "Runtime": row.runtime,
        "Handler": HANDLER,
        "Role": {"Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"]},
        "Code": {"ZipFile": inline_source(row)},
    });
    if let Group::Vended(name) = row.group {
        function_properties["LoggingConfig"] = json!({"LogGroup": name});
    }

    let mut resources = Map::new();
    resources.insert(
        GROUP_LOGICAL_ID.to_string(),
        json!({"Type": "AWS::Logs::LogGroup", "Properties": group_properties}),
    );
    resources.insert(
        ROLE_LOGICAL_ID.to_string(),
        json!({"Type": "AWS::IAM::Role", "Properties": role_properties}),
    );
    resources.insert(
        FUNCTION_LOGICAL_ID.to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "DependsOn": GROUP_LOGICAL_ID,
            "Properties": function_properties,
        }),
    );

    json!({
        "Resources": Value::Object(resources),
        "Outputs": {
            FUNCTION_NAME_OUTPUT: {"Value": {"Ref": FUNCTION_LOGICAL_ID}},
            GROUP_NAME_OUTPUT: {"Value": {"Ref": GROUP_LOGICAL_ID}},
            GROUP_ARN_OUTPUT: {"Value": {"Fn::GetAtt": [GROUP_LOGICAL_ID, "Arn"]}},
        },
    })
    .to_string()
}

/// The names `DescribeLogGroups` reports for the prefix `prefix` — the call a client that
/// declared a group makes to find it.
async fn groups_under(router: &MultiServiceRouter, case: &str, prefix: &str) -> Vec<String> {
    let (status, document) = logs(
        router,
        "DescribeLogGroups",
        &json!({"logGroupNamePrefix": prefix}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeLogGroups(logGroupNamePrefix={prefix:?}) must answer 200, got \
         {status} with {document}"
    );
    document
        .get("logGroups")
        .and_then(Value::as_array)
        .map(|groups| {
            groups
                .iter()
                .filter_map(|group| {
                    group
                        .get("logGroupName")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The retention CloudWatch Logs reports for the group `group`, read through its own
/// `DescribeLogGroups`.
async fn reported_retention(router: &MultiServiceRouter, group: &str) -> Option<i64> {
    described_group(router, group)
        .await
        .and_then(|held| held.get("retentionInDays").and_then(Value::as_i64))
}

#[tokio::test]
async fn a_stack_declared_log_group_exists_and_takes_the_declared_functions_log() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let group = log_group_of(row);

        let (code, created) = create_stack(
            &router,
            row.stack,
            &template(row),
            &acknowledging_iam(&[
                member("Tags.member.1.Key", STACK_TAG.0),
                member("Tags.member.1.Value", STACK_TAG.1),
            ]),
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200, got {code}: {created}"
        );

        let (reported_status, described) = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            reported_status,
            status::CREATE_COMPLETE,
            "case {case:?}: the stack must reach CREATE_COMPLETE, got {reported_status:?} because \
             {:?}",
            failure_reasons(&router, row.stack).await
        );

        // The identifiers the stack published, against what the row declared rather than
        // against the answers being checked.
        let published_group = output_value(&described, GROUP_NAME_OUTPUT).unwrap_or_else(|| {
            panic!("case {case:?}: the stack must publish the group's name, got {described}")
        });
        assert_eq!(
            published_group, group,
            "case {case:?}: Ref of the declared group must answer the declared LogGroupName, got \
             {published_group:?}"
        );
        let published_arn = output_value(&described, GROUP_ARN_OUTPUT).unwrap_or_else(|| {
            panic!("case {case:?}: the stack must publish the group's ARN, got {described}")
        });
        assert_eq!(
            published_arn,
            format!("arn:aws:logs:{REGION}:{ACCOUNT_ID}:log-group:{group}:*"),
            "case {case:?}: the published log-group ARN must be the documented form for \
             {group:?}, got {published_arn:?}"
        );
        let function = output_value(&described, FUNCTION_NAME_OUTPUT).unwrap_or_else(|| {
            panic!("case {case:?}: the stack must publish the function's name, got {described}")
        });
        assert_eq!(
            function, row.function,
            "case {case:?}: the stack must publish the declared function name, got {function:?}"
        );

        // The call the reported case made: the group is there, under exactly that name, before
        // anything has been invoked — the deployment made it, not a later run.
        let under_prefix = groups_under(&router, case, &group).await;
        assert_eq!(
            under_prefix,
            vec![group.clone()],
            "case {case:?}: DescribeLogGroups(logGroupNamePrefix={group:?}) must report exactly \
             the declared group once the stack is complete, got {under_prefix:?}"
        );
        let held = described_group(&router, &group).await.unwrap_or_else(|| {
            panic!("case {case:?}: CloudWatch Logs must hold the declared group {group:?}")
        });
        assert_eq!(
            held.get("arn").and_then(Value::as_str),
            Some(published_arn.as_str()),
            "case {case:?}: the ARN the stack published must be the one CloudWatch Logs reports, \
             got {held}"
        );
        assert_eq!(
            held.get("retentionInDays").and_then(Value::as_i64),
            row.retention,
            "case {case:?}: CloudWatch Logs must report the declared retention ({:?}) for \
             {group:?}, got {held}",
            row.retention
        );

        // The tags the declaration asked for plus the stack's own, read through the group's own
        // ARN. The type's published `Arn` carries the trailing `:*` the `LogGroup$arn` member
        // does, which the tagging member's modelled pattern does not admit, so the read uses
        // the plain form of the same ARN.
        let tag_arn = format!("arn:aws:logs:{REGION}:{ACCOUNT_ID}:log-group:{group}");
        let carried = group_tags(&router, case, &tag_arn).await;
        let mut expected: Vec<(String, String)> = row
            .tags
            .iter()
            .chain(std::iter::once(&STACK_TAG))
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        expected.sort();
        assert_eq!(
            carried, expected,
            "case {case:?}: the declared group must carry {expected:?} through {tag_arn:?}, got \
             {carried:?}"
        );

        // Nothing has written to it yet: the deployment made the group, and only the group.
        let streams_before = stream_names(&router, &group).await;
        assert!(
            streams_before.is_empty(),
            "case {case:?}: before any invocation the declared group {group:?} must hold no \
             stream, got {streams_before:?}"
        );

        // The step a deployment pipeline takes next: the function out of its provisioning
        // window, then one invocation.
        let state = awaited_state(&router, case, &function).await;
        assert_eq!(
            state, STATE_ACTIVE,
            "case {case:?}: AWS Lambda must report {function:?} {STATE_ACTIVE} once the stack is \
             complete, got {state:?}"
        );
        let answered = invoke(&router, case, &function).await;
        assert_eq!(
            answered.get("ok"),
            Some(&json!(true)),
            "case {case:?}: expected the declared handler's own answer, got {answered}"
        );

        // The declared group is the one the function logs into: the run's framing and the
        // handler's own line are readable from it, through the group-level read a client that
        // declared the group makes (`FilterLogEvents` over the whole group) as well as through
        // the stream the run opened.
        let delivered = delivered_log(&router, &group).await;
        assert!(
            delivered.iter().any(|line| line.starts_with(START_PREFIX))
                && delivered.iter().any(|line| line.starts_with(REPORT_PREFIX)),
            "case {case:?}: the run's log must be readable from the stream the run opened in the \
             declared group {group:?}, got {delivered:?}"
        );
        let filtered = logged_lines(&router, &group).await;
        assert!(
            filtered.iter().any(|line| line.starts_with(START_PREFIX))
                && filtered.iter().any(|line| line.starts_with(REPORT_PREFIX)),
            "case {case:?}: the run's log must be readable from the declared group {group:?} \
             itself, got {filtered:?}"
        );
        assert!(
            filtered.iter().any(|line| line.contains(row.marker)),
            "case {case:?}: the handler's own line ({:?}) must be readable from the declared \
             group {group:?}, got {filtered:?}",
            row.marker
        );
        let streams_after = stream_names(&router, &group).await;
        assert_eq!(
            streams_after.len(),
            1,
            "case {case:?}: the one invocation must open one stream in the declared group \
             {group:?}, got {streams_after:?}"
        );

        // The deployment's retention is still in force: a run that writes to the group — and a
        // role that may re-create it — does not undo what the declaration applied.
        let after = reported_retention(&router, &group).await;
        assert_eq!(
            after, row.retention,
            "case {case:?}: the declared retention ({:?}) must survive the invocation, got \
             {after:?}",
            row.retention
        );

        // A function whose `LoggingConfig` names another group leaves the DEFAULT one
        // uncreated: the log went where the declaration said, not to both.
        if let Group::Vended(_) = row.group {
            let default_group = format!("/aws/lambda/{function}");
            let held = groups_under(&router, case, &default_group).await;
            assert!(
                held.is_empty(),
                "case {case:?}: a function logging to {group:?} must leave {default_group:?} \
                 uncreated, got {held:?}"
            );
        }
    }
}
