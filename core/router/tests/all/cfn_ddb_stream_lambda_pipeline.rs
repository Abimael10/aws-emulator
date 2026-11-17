// SPDX-License-Identifier: CC0-1.0

//! The two declarations a stream-processing estate carries BESIDE the table, role, function and
//! mapping that make it: a second DynamoDB table for the consumer's results, and a CloudWatch
//! dashboard over the consumer's own `AWS/Lambda` series.
//!
//! The RULE is the one `cloudformation_ddb_stream_lambda_trigger_estate.rs` states and
//! generalises over the stream's own dimensions — a create whose declared resources the owning
//! services accept reaches `CREATE_COMPLETE` with every declared resource `CREATE_COMPLETE`, and
//! each identifier the stack publishes is the one the owning service answers to. That suite is
//! where the estate itself is pinned, and this one does not repeat it: what is asserted HERE is
//! the rule over the two declarations that suite's templates do not carry, which the recorded
//! pipelines do — so a dashboard beside the trigger is deployed into CloudWatch and is readable
//! there, and a second table is deployed into DynamoDB and is readable there, without changing
//! whether the trigger's own resources stand up.
//!
//! Six crates have to agree for that to hold — CloudFormation drives the deployment, and
//! DynamoDB, DynamoDB Streams, IAM, Lambda and CloudWatch each own one of the declarations — so
//! only the assembled emulator can ask it. The failure side of the same deployment, where one
//! declaration is refused, is `cfn_ddb_stream_lambda_pipeline_diag.rs`.
//!
//! Every observation is one a client makes: `DescribeStacks`, `DescribeStackResources`, the
//! stack's own outputs, and the owning services' own reads of what the stack declared.

use serde_json::{json, Value};

use super::query_wire::{elements, outputs_of, pairs_of};
use crate::cloudformation_apigatewayv2_resources::{cloudformation, REGION};
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{cw, router as emulator};

/// The `StackStatus` a completed create reports, and the `ResourceStatus` every resource of one
/// reports — both from the model's own enums.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const RESOURCE_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The logical ids the template declares its resources under.
const TABLE: &str = "SourceTable";
const RESULTS: &str = "ResultsTable";
const ROLE: &str = "ConsumerRole";
const FUNCTION: &str = "Consumer";
const MAPPING: &str = "StreamTrigger";
const DASHBOARD: &str = "PipelineDashboard";

/// The names the template asks for, one per resource that takes one.
const TABLE_NAME: &str = "pipeline-source";
const RESULTS_NAME: &str = "pipeline-results";
const ROLE_NAME: &str = "pipeline-consumer-role";
const FUNCTION_NAME: &str = "pipeline-consumer";
const DASHBOARD_NAME: &str = "pipeline-dashboard";

/// The series the declared dashboard's one widget is over: the backlog age of the mapping the
/// same template declares, as the AWS Lambda metric reference names it.
const NAMESPACE: &str = "AWS/Lambda";
const ITERATOR_AGE: &str = "IteratorAge";

/// The template the stack is deployed from: the trigger's four resources, plus the two this
/// suite is about.
fn template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            TABLE: {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "TableName": TABLE_NAME,
                    "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                    "BillingMode": "PAY_PER_REQUEST",
                    "StreamSpecification": {"StreamViewType": "NEW_AND_OLD_IMAGES"},
                },
            },
            RESULTS: {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "TableName": RESULTS_NAME,
                    "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                    "BillingMode": "PAY_PER_REQUEST",
                },
            },
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": ROLE_NAME,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    // What AWS Lambda requires of a mapping onto a table's stream, plus the
                    // writes a consumer that records its results makes.
                    "Policies": [{
                        "PolicyName": "read-the-stream",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": [
                                    "dynamodb:DescribeStream",
                                    "dynamodb:GetRecords",
                                    "dynamodb:GetShardIterator",
                                    "dynamodb:ListStreams",
                                    "dynamodb:PutItem",
                                    "logs:CreateLogGroup",
                                    "logs:CreateLogStream",
                                    "logs:PutLogEvents",
                                ],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION_NAME,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "Code": {"ZipFile": "def handler(event, context):\n    return event\n"},
                },
            },
            MAPPING: {
                "Type": "AWS::Lambda::EventSourceMapping",
                "Properties": {
                    "EventSourceArn": {"Fn::GetAtt": [TABLE, "StreamArn"]},
                    "FunctionName": {"Ref": FUNCTION},
                    "StartingPosition": "TRIM_HORIZON",
                    "BatchSize": 1,
                },
            },
            DASHBOARD: {
                "Type": "AWS::CloudWatch::Dashboard",
                "Properties": {
                    "DashboardName": DASHBOARD_NAME,
                    "DashboardBody": json!({
                        "widgets": [{
                            "type": "metric",
                            "properties": {
                                "metrics": [[
                                    NAMESPACE, ITERATOR_AGE, "FunctionName", FUNCTION_NAME,
                                ]],
                                "region": REGION,
                            },
                        }],
                    })
                    .to_string(),
                },
            },
        },
        "Outputs": {
            "DashboardName": {"Value": {"Ref": DASHBOARD}},
            "ResultsTable": {"Value": {"Ref": RESULTS}},
        },
    })
    .to_string()
}

/// RULE: the results table and the dashboard a stream-processing estate declares beside its
/// trigger are created in the services that own them, and the stack reaches `CREATE_COMPLETE`
/// with every declared resource created.
#[tokio::test]
async fn a_dashboard_and_a_results_table_declared_beside_the_trigger_are_created() {
    let case = "the recorded pipeline's results table and dashboard";
    let stack = "pipeline-with-a-dashboard";
    let router = emulator();
    let body = template();
    let (status, answered) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", &body),
            // The template names its own role, which is what these acknowledgements are for.
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer HTTP 200, got {status} with {answered}"
    );

    let (reached, described) = wait_for_stack_status(&router, stack, CREATE_COMPLETE).await;
    let (_, events) =
        cloudformation(&router, "DescribeStackEvents", &[("StackName", stack)]).await;
    assert_eq!(
        reached,
        CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {reached} with \
         resource failures {:?} in {described}",
        elements(&events, "ResourceStatusReason")
    );

    // Every declared resource is one the stack reports as created.
    let (status, resources) =
        cloudformation(&router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStackResources must answer HTTP 200, got {status} with \
         {resources}"
    );
    let reported = pairs_of(&resources, "LogicalResourceId", "ResourceStatus");
    let expected: std::collections::BTreeMap<String, String> =
        [TABLE, RESULTS, ROLE, FUNCTION, MAPPING, DASHBOARD]
            .into_iter()
            .map(|logical_id| {
                (
                    logical_id.to_string(),
                    RESOURCE_CREATE_COMPLETE.to_string(),
                )
            })
            .collect();
    assert_eq!(
        reported, expected,
        "case {case:?}: expected every declared resource {RESOURCE_CREATE_COMPLETE}, got \
         {reported:?} in {resources}"
    );

    // The two declarations this suite is about exist in the services that own them, under the
    // names the stack published for them.
    let outputs = outputs_of(&described);
    assert_eq!(
        outputs.get("DashboardName").map(String::as_str),
        Some(DASHBOARD_NAME),
        "case {case:?}: expected the stack to publish the declared dashboard name, got \
         {outputs:?}"
    );
    assert_eq!(
        outputs.get("ResultsTable").map(String::as_str),
        Some(RESULTS_NAME),
        "case {case:?}: expected the stack to publish the declared results table, got \
         {outputs:?}"
    );

    let held = cw(
        &router,
        case,
        REGION,
        "GetDashboard",
        &json!({ "DashboardName": DASHBOARD_NAME }),
    )
    .await;
    let body: Value = serde_json::from_str(
        held.get("DashboardBody")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .unwrap_or(Value::Null);
    assert_eq!(
        body.pointer("/widgets/0/properties/metrics/0/1")
            .and_then(Value::as_str),
        Some(ITERATOR_AGE),
        "case {case:?}: expected CloudWatch to hold the declared widget over \
         {NAMESPACE}/{ITERATOR_AGE}, got {held}"
    );
}
