// SPDX-License-Identifier: CC0-1.0

//! A CDK-shaped deployment — a parent stack whose `AWS::Lambda::Function` is configured out of
//! a nested stack's output — and the two things that have to hold of the function afterwards:
//! every configuration read AWS Lambda serves REPORTS the declared variables, and the handler
//! RUNS with them.
//!
//! The rule under test: **a function's declared `Environment.Variables` — including a value
//! substituted from another resource's `Fn::GetAtt` attribute, an
//! `AWS::CloudFormation::Stack`'s `Outputs.<key>` among them — are the variables every
//! configuration read reports on `Environment.Variables` (`GetFunctionConfiguration`, and
//! `GetFunction`'s `Configuration`) and the variables the handler's own process environment
//! carries, so a handler that resolves a resource out of `os.environ` reaches that resource.**
//!
//! Four crates have to agree for that to hold — CloudFormation creates the child stack and
//! then the parent's function, Amazon S3 holds the child template the parent points at, AWS
//! Lambda holds the function and runs it, and Amazon DynamoDB owns the table the child created
//! and the handler writes — so the suite lives here rather than in any one of them.
//!
//! The rows differ along what the rule generalises over: HOW the deployment is made (one
//! `CreateStack`, and the `CreateChangeSet` + `ExecuteChangeSet` pair `cdk deploy` makes),
//! WHICH spelling reads the child's output (the two-part `Fn::GetAtt` array, the dotted short
//! form, and the `${Name.Attribute}` variable of an `Fn::Sub` composed into text), HOW MANY
//! variables the map carries and how many of them are literals, and WHICH of the child's
//! outputs is read (a name a `Ref` published, an ARN an `Fn::GetAtt` published).
//!
//! Both halves are asserted per row from the SAME expected map, computed here from what the
//! child stack itself published: a configuration that reports variables the handler does not
//! see, or a handler that sees variables the configuration does not report, fails here.
//!
//! Nothing here sleeps for a span another service owes: every wait polls for the CONDITION with
//! a deadline far longer than the spans this emulator gives these transitions.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode, opt_element, outputs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region every resource belongs to.
/// The shared stack waiter polls this Region, so the stacks live in it.
const REGION: &str = "us-east-1";

/// The `StackStatus` a deployment that worked reaches.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `Status` and `ExecutionStatus` a change set a client may execute reports.
const CHANGE_SET_CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const CHANGE_SET_AVAILABLE: &str = "AVAILABLE";

/// How long a condition another service owes is waited for, and how often it is looked at.
/// Generous on purpose: nothing here is asserted about how long any of it takes.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// The bucket the child templates are staged in — a nested stack's `TemplateURL` points at one.
const STAGING_BUCKET: &str = "cdk-nested-output-runtime";

/// The `Timeout` every function declares: well past what one run of the handler needs, because
/// what is under test is the environment it runs with and not how fast an interpreter gets
/// through it.
const FUNCTION_TIMEOUT: u64 = 30;

/// The logical ids the parent gives its nested stack and its function.
const CHILD_LOGICAL_ID: &str = "Data";
const FUNCTION_LOGICAL_ID: &str = "Writer";
const ROLE_LOGICAL_ID: &str = "Role";

/// The handler every row deploys: it reports the value of each variable the EVENT names, as its
/// own process environment carries it, and writes the item the event names into the table
/// `TABLE_NAME` resolves to — so the variables the configuration reports are checked against
/// the ones the running function has, and the one that names a resource is checked by using it.
const HANDLER_SOURCE: &str = r#"
import os

import boto3


def handler(event, context):
    seen = {name: os.environ.get(name) for name in event["names"]}
    boto3.client("dynamodb").put_item(
        TableName=os.environ["TABLE_NAME"],
        Item={"pk": {"S": event["pk"]}, "marker": {"S": event["marker"]}},
    )
    return {"written": event["pk"], "seen": seen}
"#;

/// The emulator as it is ASSEMBLED, on one engine: the child stack, the function it configures,
/// the table the child declares and the write the handler makes are all served by this router.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One CloudFormation call over its awsQuery wire (API version 2010-05-15).
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("cloudformation.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One request against Amazon S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// One AWS Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// One Amazon DynamoDB call over its `awsJson1_0` wire.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("dynamodb.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("authorization", authorization("dynamodb"))
        .header("x-amz-target", format!("DynamoDB_20120810.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// How one row declares one environment variable's value.
#[derive(Debug, Clone, Copy)]
enum Declared {
    /// A literal, carried through as written.
    Literal(&'static str),
    /// `{"Fn::GetAtt": ["Data", "Outputs.<output>"]}` — the two-part array form.
    GetAttArray(&'static str),
    /// `{"Fn::GetAtt": "Data.Outputs.<output>"}` — the dotted short form.
    GetAttDotted(&'static str),
    /// `{"Fn::Sub": "<prefix>${Data.Outputs.<output>}"}` — the same read spelled inside a
    /// string, so the value is one PART of a composed string rather than the whole of it.
    SubbedInto(&'static str, &'static str),
}

/// The declaration a [`Declared`] is written as in the template.
fn declaration(declared: &Declared) -> Value {
    match declared {
        Declared::Literal(text) => json!(text),
        Declared::GetAttArray(output) => {
            json!({"Fn::GetAtt": [CHILD_LOGICAL_ID, format!("Outputs.{output}")]})
        }
        Declared::GetAttDotted(output) => {
            json!({"Fn::GetAtt": format!("{CHILD_LOGICAL_ID}.Outputs.{output}")})
        }
        Declared::SubbedInto(prefix, output) => {
            json!({"Fn::Sub": format!("{prefix}${{{CHILD_LOGICAL_ID}.Outputs.{output}}}")})
        }
    }
}

/// What a [`Declared`] must end up carrying, given the outputs the CHILD stack published —
/// computed from the child's own answer, never read out of the configuration under test.
fn expected_value(declared: &Declared, published: &BTreeMap<String, String>) -> String {
    let read = |output: &str| {
        published
            .get(output)
            .cloned()
            .unwrap_or_else(|| panic!("the child published no {output:?} output, got {published:?}"))
    };
    match declared {
        Declared::Literal(text) => (*text).to_string(),
        Declared::GetAttArray(output) | Declared::GetAttDotted(output) => read(output),
        Declared::SubbedInto(prefix, output) => format!("{prefix}{}", read(output)),
    }
}

/// How a row's deployment is made.
#[derive(Debug, Clone, Copy)]
enum Deploy {
    /// One `CreateStack`, the call a client makes directly.
    CreateStack,
    /// `CreateChangeSet` with `ChangeSetType=CREATE`, a wait for the plan, then
    /// `ExecuteChangeSet` — the three calls `cdk deploy` makes.
    ChangeSet(&'static str),
}

/// One row: the app it deploys and how, and the environment its function declares.
struct Row {
    case: &'static str,
    /// The parent stack's name.
    stack: &'static str,
    /// The key the row's child template is staged under.
    key: &'static str,
    deploy: Deploy,
    /// Every variable the function declares, in declaration order.
    variables: &'static [(&'static str, Declared)],
}

/// The variable whose value NAMES the table the handler writes: every row declares it, because
/// the half of the rule about the running function is that the handler reaches the resource its
/// environment names.
const TABLE_VARIABLE: &str = "TABLE_NAME";

const ROWS: &[Row] = &[
    Row {
        case: "one CreateStack, the two-part Fn::GetAtt array, the only variable",
        stack: "cdk-nested-env-create-stack",
        key: "env-create-stack.json",
        deploy: Deploy::CreateStack,
        variables: &[(TABLE_VARIABLE, Declared::GetAttArray("TableName"))],
    },
    Row {
        case: "the change-set pair cdk deploy makes, the dotted Fn::GetAtt short form, beside \
               two literals",
        stack: "cdk-nested-env-change-set",
        key: "env-change-set.json",
        deploy: Deploy::ChangeSet("cdk-deploy-dotted"),
        variables: &[
            (TABLE_VARIABLE, Declared::GetAttDotted("TableName")),
            ("STAGE", Declared::Literal("prod")),
            ("LOG_LEVEL", Declared::Literal("info")),
        ],
    },
    Row {
        case: "the change-set pair, with a second variable composing the child's ARN output \
               into text",
        stack: "cdk-nested-env-subbed",
        key: "env-subbed.json",
        deploy: Deploy::ChangeSet("cdk-deploy-subbed"),
        variables: &[
            (TABLE_VARIABLE, Declared::GetAttArray("TableName")),
            ("TABLE_TARGET", Declared::SubbedInto("target=", "TableArn")),
            ("STAGE", Declared::Literal("beta")),
        ],
    },
];

/// The CHILD template every row stages: one Amazon DynamoDB table whose name is
/// CloudFormation's to mint, with that name and its ARN published as outputs — the two kinds of
/// published value a parent reads back.
fn child_template() -> String {
    json!({
        "Resources": {
            "Table": {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                    "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
                    "BillingMode": "PAY_PER_REQUEST",
                },
            },
        },
        "Outputs": {
            "TableName": {"Value": {"Ref": "Table"}},
            "TableArn": {"Value": {"Fn::GetAtt": ["Table", "Arn"]}},
        },
    })
    .to_string()
}

/// The PARENT template: the function's execution role, the nested stack, and the function whose
/// environment reads the child's outputs.
fn parent_template(row: &Row) -> String {
    let mut variables = Map::new();
    for (name, declared) in row.variables {
        variables.insert((*name).to_string(), declaration(declared));
    }
    json!({
        "Resources": {
            ROLE_LOGICAL_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    // The write the handler makes, which the function performs AS this role.
                    "Policies": [{
                        "PolicyName": "write-the-table",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["dynamodb:PutItem"],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            CHILD_LOGICAL_ID: {
                "Type": "AWS::CloudFormation::Stack",
                "Properties": {"TemplateURL": staged_url(row.key)},
            },
            FUNCTION_LOGICAL_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Code": {"ZipFile": HANDLER_SOURCE},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {"Variables": Value::Object(variables)},
                },
            },
        },
        "Outputs": {
            "FunctionName": {"Value": {"Ref": FUNCTION_LOGICAL_ID}},
        },
    })
    .to_string()
}

/// The URL a staged child template is addressed by, path-style.
fn staged_url(key: &str) -> String {
    format!("https://s3.{REGION}.amazonaws.com/{STAGING_BUCKET}/{key}")
}

/// Stage one child template in Amazon S3, which is where a nested stack's `TemplateURL` points.
async fn stage(router: &MultiServiceRouter, case: &str, key: &str, body: &str) {
    let (status, answered) = s3(router, "PUT", &format!("/{STAGING_BUCKET}/{key}"), body).await;
    assert!(
        (200..300).contains(&status),
        "case {case:?}: staging {key:?} must be accepted, got {status} with {answered}"
    );
}

/// Make the row's deployment and answer the `DescribeStacks` answer the settled parent was read
/// from, having asserted it reached [`CREATE_COMPLETE`].
async fn deploy(router: &MultiServiceRouter, row: &Row) -> String {
    let case = row.case;
    let body = parent_template(row);
    match row.deploy {
        Deploy::CreateStack => {
            let (status, answered) = cloudformation(
                router,
                "CreateStack",
                &[
                    ("StackName", row.stack),
                    ("TemplateBody", &body),
                    ("Capabilities.member.1", "CAPABILITY_IAM"),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateStack to be accepted, got {status} with {answered}"
            );
        }
        Deploy::ChangeSet(name) => {
            let (status, created) = cloudformation(
                router,
                "CreateChangeSet",
                &[
                    ("StackName", row.stack),
                    ("ChangeSetName", name),
                    ("ChangeSetType", "CREATE"),
                    ("TemplateBody", &body),
                    ("Capabilities.member.1", "CAPABILITY_IAM"),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateChangeSet to be accepted, got {status} with \
                 {created}"
            );
            // A client addresses the change set by the `Id` the create answered with, which is
            // what its own waiter and its `ExecuteChangeSet` carry.
            let id = element(&created, "Id");
            let (planned, described) = settled_change_set(router, &id, row.stack).await;
            assert_eq!(
                planned,
                (
                    CHANGE_SET_CREATE_COMPLETE.to_string(),
                    CHANGE_SET_AVAILABLE.to_string()
                ),
                "case {case:?}: expected the change set to be created and executable, got \
                 {described}"
            );
            let (status, executed) = cloudformation(
                router,
                "ExecuteChangeSet",
                &[("ChangeSetName", &id), ("StackName", row.stack)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected ExecuteChangeSet to be accepted, got {status} with \
                 {executed}"
            );
        }
    }

    let (reached, described) = wait_for_stack_status(router, row.stack, CREATE_COMPLETE).await;
    let failures = if reached == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", row.stack)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reached, CREATE_COMPLETE,
        "case {case:?}: expected the parent to reach {CREATE_COMPLETE}, got {described} with \
         resource failures {failures:?}"
    );
    described
}

/// Poll `DescribeChangeSet` until the change set is created and executable, and answer the
/// `(Status, ExecutionStatus)` pair it reported last with the answer they were read from.
///
/// The wait is on that CONDITION — the plan is owed from the instant the create was accepted,
/// which no fixed sleep names — and the LAST pair seen is answered, so the caller's own
/// assertion is what reports a change set that never became executable.
async fn settled_change_set(
    router: &MultiServiceRouter,
    id: &str,
    stack: &str,
) -> ((String, String), String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (_, described) = cloudformation(
            router,
            "DescribeChangeSet",
            &[("ChangeSetName", id), ("StackName", stack)],
        )
        .await;
        let reported = (
            opt_element(&described, "Status").unwrap_or_default(),
            opt_element(&described, "ExecutionStatus").unwrap_or_default(),
        );
        let ready =
            reported.0 == CHANGE_SET_CREATE_COMPLETE && reported.1 == CHANGE_SET_AVAILABLE;
        if ready || Instant::now() >= giving_up_at {
            return (reported, described);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Poll `GetFunctionConfiguration` until AWS Lambda first reports the function `Active`, and
/// answer the configuration it reported then.
///
/// "During this time, you can't invoke or modify the function" is what that service reports
/// `Pending` for, and the span is its own, so no fixed sleep names the instant it ends.
async fn active_configuration(router: &MultiServiceRouter, case: &str, function: &str) -> Value {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected AWS Lambda to report {function:?}, got {status} with \
             {configuration}"
        );
        let reported = configuration.get("State").and_then(Value::as_str);
        if reported == Some("Active") || Instant::now() >= giving_up_at {
            return configuration;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Poll `DescribeTable` until Amazon DynamoDB first reports the table `ACTIVE`, and answer the
/// status it reported then — "you can perform read and write operations only on an ACTIVE
/// table", so the handler is invoked once the table it writes serves writes.
async fn active_table(router: &MultiServiceRouter, table: &str) -> String {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (_, described) = dynamodb(router, "DescribeTable", &json!({"TableName": table})).await;
        let status = described
            .get("Table")
            .and_then(|table| table.get("TableStatus"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if status == "ACTIVE" || Instant::now() >= giving_up_at {
            return status;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// `Environment.Variables` of one reported configuration, as the map a client reads it as, or
/// an empty map where the answer carries no `Environment` member at all — which is exactly the
/// absence this suite is about, reported by the assertion that compares it.
fn reported_variables(configuration: &Value) -> BTreeMap<String, String> {
    configuration
        .get("Environment")
        .and_then(|environment| environment.get("Variables"))
        .and_then(Value::as_object)
        .map(|variables| {
            variables
                .iter()
                .map(|(name, value)| (name.clone(), value.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_nested_stacks_output_is_reported_and_run_with_by_its_parents_function() {
    let router = build_router();
    let (status, answered) = s3(&router, "PUT", &format!("/{STAGING_BUCKET}"), "").await;
    assert!(
        (200..300).contains(&status),
        "setup: the staging bucket must be created, got {status} with {answered}"
    );

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        stage(&router, case, row.key, &child_template()).await;
        let described = deploy(&router, row).await;

        // The CHILD, addressed by the physical id the parent's nested-stack resource carries:
        // what it published is the value the function had to be configured with, read from the
        // stack that published it rather than from the configuration under test.
        let (status, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the parent's resources to be reported, got {status} \
             {resources}"
        );
        let physical_ids = physical_ids_of(&resources);
        let child_stack = physical_ids
            .get(CHILD_LOGICAL_ID)
            .cloned()
            .unwrap_or_default();
        assert!(
            !child_stack.is_empty(),
            "case {case:?}: expected the nested-stack resource to carry its child's id, got \
             {physical_ids:?}"
        );
        let (status, child_body) =
            cloudformation(&router, "DescribeStacks", &[("StackName", &child_stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the child stack {child_stack:?} to be described, got \
             {status} {child_body}"
        );
        // The child is the parent's: `ParentId` and `RootId` both name the stack that declared
        // it, which is what says the id above addresses THIS deployment's child.
        let parent_id = element(&described, "StackId");
        let lineage = (
            opt_element(&child_body, "ParentId"),
            opt_element(&child_body, "RootId"),
        );
        assert_eq!(
            lineage,
            (Some(parent_id.clone()), Some(parent_id.clone())),
            "case {case:?}: expected the child to report {parent_id:?} as both its parent and \
             its root, got {lineage:?} in {child_body}"
        );
        let published = outputs_of(&child_body);

        // What EVERY variable has to carry, computed from the child's own answer.
        let expected: BTreeMap<String, String> = row
            .variables
            .iter()
            .map(|(name, declared)| {
                ((*name).to_string(), expected_value(declared, &published))
            })
            .collect();

        let function = outputs_of(&described)
            .get("FunctionName")
            .cloned()
            .unwrap_or_default();
        assert!(
            !function.is_empty(),
            "case {case:?}: expected the parent to publish its function's name, got {described}"
        );

        // Half one: every configuration read AWS Lambda serves reports those variables.
        let configuration = active_configuration(&router, case, &function).await;
        assert_eq!(
            configuration.get("State").and_then(Value::as_str),
            Some("Active"),
            "case {case:?}: expected AWS Lambda to report {function:?} Active, got \
             {configuration}"
        );
        assert_eq!(
            reported_variables(&configuration),
            expected,
            "case {case:?}: expected GetFunctionConfiguration to report {expected:?}, got \
             {configuration}"
        );
        let (status, _, whole) =
            lambda(&router, "GET", &format!("/2015-03-31/functions/{function}"), &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetFunction to answer for {function:?}, got {status} \
             {whole}"
        );
        let in_get_function = whole
            .get("Configuration")
            .map(reported_variables)
            .unwrap_or_default();
        assert_eq!(
            in_get_function, expected,
            "case {case:?}: expected GetFunction's Configuration to report {expected:?}, got \
             {whole}"
        );

        // Half two: the handler RUNS with them, and the one that names a table reaches it.
        let table = expected
            .get(TABLE_VARIABLE)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the row declares no {TABLE_VARIABLE}"));
        let table_status = active_table(&router, &table).await;
        assert_eq!(
            table_status, "ACTIVE",
            "case {case:?}: expected Amazon DynamoDB to report {table:?} ACTIVE, got \
             {table_status:?}"
        );
        let pk = format!("row-{index}-key");
        let marker = format!("row-{index}-marker");
        let names: Vec<&str> = row.variables.iter().map(|(name, _)| *name).collect();
        let event = json!({"pk": pk, "marker": marker, "names": names});
        let (status, headers, payload) = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{function}/invocations"),
            event.to_string().as_bytes(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from Invoke, got {status} with {payload}"
        );
        let signal = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        assert_eq!(
            signal, None,
            "case {case:?}: expected no function error, got {signal:?} with {payload}"
        );
        let expected_payload = json!({
            "written": pk,
            "seen": expected
                .iter()
                .map(|(name, value)| (name.clone(), json!(value)))
                .collect::<Map<String, Value>>(),
        });
        assert_eq!(
            payload, expected_payload,
            "case {case:?}: expected the handler to report {expected_payload}, got {payload}"
        );

        // ...and the write it reported is a write Amazon DynamoDB really holds.
        let (status, read) = dynamodb(
            &router,
            "GetItem",
            &json!({
                "TableName": table,
                "Key": {"pk": {"S": pk}},
                "ConsistentRead": true,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetItem to answer for {table:?}, got {status} {read}"
        );
        let held = read.get("Item").cloned().unwrap_or(Value::Null);
        let wanted = json!({"pk": {"S": pk}, "marker": {"S": marker}});
        assert_eq!(
            held, wanted,
            "case {case:?}: expected the child's table to hold {wanted}, got {held}"
        );
    }
}
