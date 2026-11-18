// SPDX-License-Identifier: CC0-1.0

//! The deployment pattern a client ships a "configuration value maintained by an event-driven
//! function" as: ONE stack declaring the execution role, the function, the
//! `AWS::Lambda::Permission` that admits Amazon EventBridge, the bus, the `AWS::Events::Rule`
//! whose target is the function, and the `AWS::SSM::Parameter` the stack seeds — observed only
//! through the API surface, as the value Parameter Store comes to hold.
//!
//! The rule under test: **once such a stack is `CREATE_COMPLETE`, an event the declared rule's
//! pattern matches is delivered to the function that rule targets as one asynchronous
//! invocation, and the call the handler makes with the AWS SDK its managed runtime supplies is
//! served by the named service as the function's own execution role — so what the handler wrote
//! is what a later read reports, at a new version over the one the stack seeded, and a call that
//! service refuses comes back to the handler as that service's own modelled refusal rather than
//! failing inside the runtime.**
//!
//! That the SDK a managed runtime supplies carries a client for every service this emulator
//! serves is what makes the chain run at all: a runtime whose bundled catalogue omitted the
//! service the handler names could not even construct the client, so the stack would create,
//! the rule would exist and the invocation would answer, while the declared value stayed at
//! its seed forever and no caller saw an error anywhere.
//!
//! Six crates have to agree for the rule to hold — CloudFormation installs the whole wiring and
//! archives the inline handler into a package this emulator can run, IAM holds the role and its
//! inline document, STS mints the session the handler's calls are signed as, EventBridge owns
//! the buses, the rule and the delivery, Lambda holds the invoke grant and runs the handler, and
//! Systems Manager holds the parameters — so the suite lives here: a crate-local suite can only
//! ever see one side of it. `lambda_runtime_sdk_drives_parameter_store_and_event_bus` pins the
//! same SDK on a rule and a grant a CLIENT installed call by call, and the authorization verdict
//! each of its calls is decided by; what only this suite reaches is the whole pipeline DECLARED
//! in one template, seed included.
//!
//! The rows differ along what the rule generalises over: the bus the matched rule is declared on
//! (the account's default bus, and one the same stack declares) and so which of the two
//! functions — each on a `python3.x` runtime of its own — the delivery runs; whether the name the
//! handler writes is the one the stack seeded (so the write is an overwrite, answering the next
//! version) or one no resource declared (so it is a create, answering the first); the form of the
//! value written (a bare word, a JSON document carrying punctuation and non-ASCII text, a
//! comma-separated list) and the parameter type it is written as; both values of the `Overwrite`
//! the write asks for, including the one the service refuses over a name it already holds; and
//! whether the published event's source matches the rule's pattern at all.

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of};

/// The Region the whole stack is deployed into, and so the Region every ARN below carries.
const REGION: &str = "us-east-1";

/// The account this emulator serves.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The `StackStatus` the pipeline must reach before anything is published.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stack every resource below is declared in.
const STACK: &str = "cfn-evt-lambda-ssm-pipeline";

/// The hierarchy the role's one grant scopes itself to: every name the deployed pipeline
/// maintains lives under it.
const MAINTAINED_PATH: &str = "/cfn-evt-ssm";

/// The parameter the stack SEEDS, and the value it seeds it with — the value a consumer reads
/// until the first matched event is delivered — beside the value the pipeline moves it to.
const SEEDED_NAME: &str = "/cfn-evt-ssm/maintained";
const SEED: &str = "initial";
const REVISION: &str = "revision-two";

/// The `source` the rules' patterns match, and one they do not.
const MATCHED_SOURCE: &str = "cfn.evt.ssm.pipeline";
const UNMATCHED_SOURCE: &str = "cfn.evt.ssm.elsewhere";

/// The `detail-type` every published event carries.
const DETAIL_TYPE: &str = "Configuration Revision";

/// The two runtimes the stack's functions declare: both are `python3.x` values of the `Runtime`
/// enum (source: `smithy_query(query="shape",shape="Runtime")`), so the SDK belongs to the
/// managed runtime family rather than to one point release. Each function reports its own, which
/// is how a row names WHICH function the delivery ran.
const DEFAULT_BUS_RUNTIME: &str = "python3.12";
const DECLARED_BUS_RUNTIME: &str = "python3.11";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the pipeline rather than how fast an interpreter
/// gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a created function reaching
/// `Active`, an asynchronous delivery landing in Parameter Store — and how many looks to take.
/// The spans are the owning services' own, so no fixed sleep names the instant one ends, and the
/// deadline is generous enough that a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 900;

/// The refusal a read of a name Parameter Store holds nothing for answers with, and its status.
const NOT_FOUND: &str = "ParameterNotFound";
const NOT_FOUND_STATUS: u16 = 400;

/// The code the serving service refuses a create over a name it already holds with: "the
/// parameter already exists. You can't create duplicate parameters"
/// (`smithy_query(query="operation",operation="PutParameter")`).
const ALREADY_EXISTS: &str = "ParameterAlreadyExists";

/// The handler both declared functions run: for the event it is handed it writes the value the
/// envelope carries into the parameter the envelope names, asking for the `Overwrite` the
/// envelope states, and then records what came of that write — the version it answered, or the
/// code the serving service refused it with — into the report parameter the envelope names.
/// Which function ran is part of that record, read out of the function's own environment.
///
/// This is the shape of the handler a "config value maintained by an event-driven function"
/// template ships: the client is built at module scope from the SDK the runtime supplies, with
/// no dependency of the deployment package's own.
const MAINTAINING_HANDLER: &str = r#"import json
import os

import boto3
from botocore.exceptions import ClientError

CLIENT = boto3.client("ssm")


def handler(event, context):
    detail = event["detail"]
    report = {"runtime": os.environ["DEPLOYED_RUNTIME"], "source": event["source"]}
    try:
        answered = CLIENT.put_parameter(
            Name=detail["record"],
            Value=detail["value"],
            Type=detail["type"],
            Overwrite=detail["overwrite"],
        )
    except ClientError as refused:
        report["error"] = refused.response["Error"]["Code"]
    else:
        report["version"] = answered["Version"]
    CLIENT.put_parameter(
        Name=detail["report"],
        Value=json.dumps(report, sort_keys=True),
        Type="String",
        Overwrite=True,
    )
    return report
"#;

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched back
/// into this same router, so the parameter it writes is the one this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. Its credential is the long-term key every emulator publishes, so these calls are
/// the account's own and not the pipeline's role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One CloudFormation call over its awsQuery wire, answered with the status and the XML.
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
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One awsJson1_1 call of `service`, under the `X-Amz-Target` prefix `target_prefix`.
async fn rpc(
    router: &MultiServiceRouter,
    service: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(service))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Systems Manager call (`sigv4Name: ssm`, service shape `AmazonSSM`), as the account's own
/// credential.
async fn ssm(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "ssm", "AmazonSSM", operation, body).await
}

/// One Amazon EventBridge call, as the account's own credential.
async fn events(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "events", "AWSEvents", operation, body).await
}

/// One Lambda `restJson1` GET, as its status and parsed body.
async fn lambda_get(router: &MultiServiceRouter, uri: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = Box::pin(router.dispatch(request)).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The ARN of the parameter `name` names in this Region: a hierarchical name keeps its whole path
/// after the `parameter/` prefix, and the leading separator is not part of it.
fn parameter_arn(name: &str) -> String {
    format!(
        "arn:aws:ssm:{REGION}:{ACCOUNT}:parameter/{}",
        name.strip_prefix('/').unwrap_or(name)
    )
}

/// The one resource the role's grant names: every parameter under the maintained hierarchy.
fn granted_hierarchy() -> String {
    parameter_arn(&format!("{MAINTAINED_PATH}/*"))
}

/// The declaration of one maintaining function: the runtime it declares, the name it is created
/// under, and the inline handler CloudFormation archives into its deployment package.
fn function(name: &str, runtime: &str) -> Value {
    json!({
        "Type": "AWS::Lambda::Function",
        "Properties": {
            "FunctionName": name,
            "Runtime": runtime,
            "Handler": "index.handler",
            "Role": {"Fn::GetAtt": ["MaintainerRole", "Arn"]},
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": {"Variables": {"DEPLOYED_RUNTIME": runtime}},
            "Code": {"ZipFile": MAINTAINING_HANDLER},
        },
    })
}

/// The declaration of one rule and the grant that admits its deliveries: the rule matches the
/// one source, targets `function_id`'s function, and the permission is scoped by `aws:SourceArn`
/// to this rule, which is how a deployed template admits one rule's deliveries and no others.
fn rule_and_permission(name: &str, bus: Option<&str>, function_id: &str, rule_id: &str) -> Value {
    let mut properties = json!({
        "Name": name,
        "State": "ENABLED",
        "EventPattern": {"source": [MATCHED_SOURCE]},
        "Targets": [{
            "Id": "maintainer",
            "Arn": {"Fn::GetAtt": [function_id, "Arn"]},
        }],
    });
    if let Some(bus) = bus {
        properties["EventBusName"] = json!({"Ref": bus});
    }
    json!({
        "Rule": {"Type": "AWS::Events::Rule", "Properties": properties},
        "Permission": {
            "Type": "AWS::Lambda::Permission",
            "Properties": {
                "FunctionName": {"Fn::GetAtt": [function_id, "Arn"]},
                "Action": "lambda:InvokeFunction",
                "Principal": "events.amazonaws.com",
                "SourceArn": {"Fn::GetAtt": [rule_id, "Arn"]},
            },
        },
    })
}

/// The whole pipeline, as one template: the execution role with its single scoped grant, one
/// function per bus, the bus the stack declares, a rule on each bus targeting its function, the
/// grant that admits each rule's deliveries, and the parameter the stack seeds.
fn template(
    default_function: &str,
    declared_function: &str,
    bus: &str,
    default_rule: &str,
    declared_rule: &str,
) -> String {
    let on_default = rule_and_permission(
        default_rule,
        None,
        "DefaultBusMaintainer",
        "DefaultBusRule",
    );
    let on_declared = rule_and_permission(
        declared_rule,
        Some("ConfigBus"),
        "DeclaredBusMaintainer",
        "DeclaredBusRule",
    );
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "MaintainerRole": {
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
                    "Policies": [{
                        "PolicyName": "maintain-the-configuration",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [
                                {
                                    "Effect": "Allow",
                                    "Action": "ssm:PutParameter",
                                    "Resource": granted_hierarchy(),
                                },
                                {
                                    "Effect": "Allow",
                                    "Action": [
                                        "logs:CreateLogGroup",
                                        "logs:CreateLogStream",
                                        "logs:PutLogEvents",
                                    ],
                                    "Resource": "*",
                                },
                            ],
                        },
                    }],
                },
            },
            "DefaultBusMaintainer": function(default_function, DEFAULT_BUS_RUNTIME),
            "DeclaredBusMaintainer": function(declared_function, DECLARED_BUS_RUNTIME),
            "ConfigBus": {
                "Type": "AWS::Events::EventBus",
                "Properties": {"Name": bus},
            },
            "DefaultBusRule": on_default["Rule"],
            "DefaultBusInvokePermission": on_default["Permission"],
            "DeclaredBusRule": on_declared["Rule"],
            "DeclaredBusInvokePermission": on_declared["Permission"],
            "MaintainedParameter": {
                "Type": "AWS::SSM::Parameter",
                "Properties": {
                    "Name": SEEDED_NAME,
                    "Type": "String",
                    "Value": SEED,
                },
            },
        },
        "Outputs": {
            "DefaultBusFunction": {"Value": {"Ref": "DefaultBusMaintainer"}},
            "DeclaredBusFunction": {"Value": {"Ref": "DeclaredBusMaintainer"}},
            "BusName": {"Value": {"Ref": "ConfigBus"}},
            "MaintainedParameter": {"Value": {"Ref": "MaintainedParameter"}},
        },
    })
    .to_string()
}

/// Deploy the pipeline and answer the outputs it published, once it reports `CREATE_COMPLETE`.
async fn deploy(router: &MultiServiceRouter, body: &str) -> std::collections::BTreeMap<String, String> {
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", body),
            // The template declares an IAM role, which CloudFormation will not create without
            // the caller acknowledging it ("Requires capabilities : [CAPABILITY_IAM]").
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant its resources' owning services report them
    // created, which no fixed sleep can name: this looks until it reports it.
    let (reported, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_status, raised) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
        elements(&raised, "ResourceStatusReason")
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: the pipeline must reach {CREATE_COMPLETE}, got {reported} with resource failures \
         {failures:?}"
    );
    outputs_of(&described)
}

/// Wait until Lambda reports `name` `Active`, which is the state a delivery needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_status, configuration) =
            lambda_get(router, &format!("/2015-03-31/functions/{name}/configuration")).await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} Active, last reported {last}");
}

/// Look for the parameter `name` until Parameter Store holds it, and answer the `Parameter`
/// structure it reports. The write is owed from the instant the publication was accepted — the
/// delivery is asynchronous — so the wait is on that CONDITION rather than on a fixed span.
async fn wait_for_parameter(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (status, read) = ssm(router, "GetParameter", &json!({"Name": name})).await;
        if status == 200 {
            return read["Parameter"].clone();
        }
        last = read;
        tokio::time::sleep(LOOK).await;
    }
    panic!(
        "case {case:?}: Parameter Store must come to hold {name:?}, the last read answered {last}"
    );
}

/// Assert Parameter Store holds nothing under `name`, which is what says no handler wrote it.
async fn assert_absent(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, read) = ssm(router, "GetParameter", &json!({"Name": name})).await;
    let rendered = read.to_string();
    assert!(
        status == NOT_FOUND_STATUS && rendered.contains(NOT_FOUND),
        "case {case:?}: nothing must have written {name:?}, so the read of it must answer \
         {NOT_FOUND_STATUS} {NOT_FOUND}, got {status} with {rendered}"
    );
}

/// Which bus a row publishes onto — and so which declared rule matches it, and which of the two
/// functions that rule's target names.
#[derive(Debug, Clone, Copy)]
enum Bus {
    /// The account's default bus, which the rule declaring no `EventBusName` is bound to.
    Default,
    /// The bus the same stack declares.
    Declared,
}

/// What a row's delivery must come to.
#[derive(Debug, Clone, Copy)]
enum Outcome {
    /// The handler's write took effect, answering `version` of the name it wrote.
    Written { version: u64 },
    /// The write was refused by the serving service, which the handler reports — leaving the
    /// name it addressed holding `still_holds`, or nothing at all.
    Refused {
        code: &'static str,
        still_holds: Option<(&'static str, u64)>,
    },
    /// The rule's pattern does not match the event, so nothing ran at all.
    NotDelivered,
}

/// One published event: the bus it goes onto, the source it carries, the parameter the handler is
/// asked to write (its name, value, type and whether to overwrite), the name the handler records
/// the outcome under, and what the delivery must come to.
struct Row {
    case: &'static str,
    bus: Bus,
    source: &'static str,
    record: &'static str,
    value: &'static str,
    parameter_type: &'static str,
    overwrite: bool,
    report: &'static str,
    outcome: Outcome,
}

const ROWS: &[Row] = &[
    Row {
        case: "a matched event on the default bus overwriting the value the stack seeded",
        bus: Bus::Default,
        source: MATCHED_SOURCE,
        record: SEEDED_NAME,
        value: REVISION,
        parameter_type: "String",
        overwrite: true,
        report: "/cfn-evt-ssm/reports/seeded",
        // The stack's own seeding is version 1, so the handler's overwrite is the second.
        outcome: Outcome::Written { version: 2 },
    },
    Row {
        case: "a matched event on the declared bus creating a name no resource declared",
        bus: Bus::Declared,
        source: MATCHED_SOURCE,
        record: "/cfn-evt-ssm/rendered",
        value: r#"{"endpoint":"db.internal.example:5432","note":"a documented valüe ✓"}"#,
        parameter_type: "String",
        overwrite: true,
        report: "/cfn-evt-ssm/reports/rendered",
        outcome: Outcome::Written { version: 1 },
    },
    Row {
        case: "a matched event on the declared bus writing a list-typed value",
        bus: Bus::Declared,
        source: MATCHED_SOURCE,
        record: "/cfn-evt-ssm/members",
        value: "alpha,beta,gamma",
        parameter_type: "StringList",
        overwrite: true,
        report: "/cfn-evt-ssm/reports/members",
        outcome: Outcome::Written { version: 1 },
    },
    Row {
        case: "a matched event on the default bus creating a name, asking for no overwrite",
        bus: Bus::Default,
        source: MATCHED_SOURCE,
        record: "/cfn-evt-ssm/created",
        value: "a first revision\nover two lines",
        parameter_type: "String",
        overwrite: false,
        report: "/cfn-evt-ssm/reports/created",
        outcome: Outcome::Written { version: 1 },
    },
    Row {
        case: "a matched event asking for no overwrite of the name the stack already seeded",
        bus: Bus::Default,
        source: MATCHED_SOURCE,
        record: SEEDED_NAME,
        value: "never written",
        parameter_type: "String",
        overwrite: false,
        report: "/cfn-evt-ssm/reports/refused",
        outcome: Outcome::Refused {
            code: ALREADY_EXISTS,
            // The refused write leaves the value the overwriting row put there.
            still_holds: Some((REVISION, 2)),
        },
    },
    Row {
        case: "an event whose source neither rule's pattern names",
        bus: Bus::Default,
        source: UNMATCHED_SOURCE,
        record: "/cfn-evt-ssm/unmatched",
        value: "never written",
        parameter_type: "String",
        overwrite: true,
        report: "/cfn-evt-ssm/reports/unmatched",
        outcome: Outcome::NotDelivered,
    },
];

/// Every row: the stack a client deploys installs a pipeline that really runs, and the value the
/// event-driven function writes with the SDK its managed runtime supplies is the one Parameter
/// Store then reports.
#[tokio::test]
async fn a_deployed_event_driven_pipeline_maintains_the_parameter_it_declares() {
    let default_function = format!("{STACK}-default-bus");
    let declared_function = format!("{STACK}-declared-bus");
    let bus = format!("{STACK}-bus");
    let default_rule = format!("{STACK}-default-rule");
    let declared_rule = format!("{STACK}-declared-rule");

    let router = build_router();
    let body = template(
        &default_function,
        &declared_function,
        &bus,
        &default_rule,
        &declared_rule,
    );
    let outputs = deploy(&router, &body).await;

    for (key, expected) in [
        ("DefaultBusFunction", &default_function),
        ("DeclaredBusFunction", &declared_function),
        ("BusName", &bus),
        ("MaintainedParameter", &SEEDED_NAME.to_string()),
    ] {
        assert_eq!(
            outputs.get(key).map(String::as_str),
            Some(expected.as_str()),
            "setup: the stack must publish {key} as {expected:?}, got {outputs:?}"
        );
    }

    // The seed is the stack's own: before any event is published, a consumer reads the declared
    // value at the first version. This is the state the pipeline is supposed to move.
    let seeded = wait_for_parameter(&router, "the stack's own seeding", SEEDED_NAME).await;
    assert_eq!(
        (
            seeded["Value"].as_str(),
            seeded["Type"].as_str(),
            seeded["Version"].as_u64(),
            seeded["ARN"].as_str(),
        ),
        (
            Some(SEED),
            Some("String"),
            Some(1),
            Some(parameter_arn(SEEDED_NAME).as_str()),
        ),
        "setup: the stack must seed {SEEDED_NAME:?}, got {seeded}"
    );

    for name in [&default_function, &declared_function] {
        wait_until_active(&router, name).await;
    }

    // The rows that must lead to no write at all are published FIRST, so that by the time the
    // delivered ones have been observed there has been at least as long for a write that must
    // not have happened to appear.
    let mut published: Vec<&Row> = ROWS
        .iter()
        .filter(|row| matches!(row.outcome, Outcome::NotDelivered))
        .collect();
    published.extend(
        ROWS.iter()
            .filter(|row| !matches!(row.outcome, Outcome::NotDelivered)),
    );

    for row in &published {
        let case = row.case;
        let bus_name = match row.bus {
            Bus::Default => "default",
            Bus::Declared => bus.as_str(),
        };
        let (status, accepted) = events(
            &router,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": row.source,
                    "DetailType": DETAIL_TYPE,
                    "EventBusName": bus_name,
                    "Detail": json!({
                        "record": row.record,
                        "value": row.value,
                        "type": row.parameter_type,
                        "overwrite": row.overwrite,
                        "report": row.report,
                    })
                    .to_string(),
                }],
            }),
        )
        .await;
        assert_eq!(
            (status, accepted["FailedEntryCount"].as_u64()),
            (200, Some(0)),
            "case {case:?}: the publication must be accepted, got {status} with {accepted}"
        );
    }

    for row in ROWS {
        let case = row.case;
        let expected_runtime = match row.bus {
            Bus::Default => DEFAULT_BUS_RUNTIME,
            Bus::Declared => DECLARED_BUS_RUNTIME,
        };
        match row.outcome {
            Outcome::Written { version } => {
                let report = wait_for_parameter(&router, case, row.report).await;
                let reported: Value = serde_json::from_str(
                    report["Value"].as_str().unwrap_or_default(),
                )
                .unwrap_or(Value::Null);
                assert_eq!(
                    reported,
                    json!({
                        "runtime": expected_runtime,
                        "source": row.source,
                        "version": version,
                    }),
                    "case {case:?}: the function the rule on that bus targets must report the \
                     version its write answered, got {reported}"
                );
                let held = wait_for_parameter(&router, case, row.record).await;
                assert_eq!(
                    (
                        held["Value"].as_str(),
                        held["Type"].as_str(),
                        held["Version"].as_u64(),
                        held["ARN"].as_str(),
                    ),
                    (
                        Some(row.value),
                        Some(row.parameter_type),
                        Some(version),
                        Some(parameter_arn(row.record).as_str()),
                    ),
                    "case {case:?}: Parameter Store must report what the handler wrote under \
                     {:?}, got {held}",
                    row.record
                );
            }
            Outcome::Refused { code, still_holds } => {
                let report = wait_for_parameter(&router, case, row.report).await;
                let reported: Value = serde_json::from_str(
                    report["Value"].as_str().unwrap_or_default(),
                )
                .unwrap_or(Value::Null);
                assert_eq!(
                    reported,
                    json!({
                        "runtime": expected_runtime,
                        "source": row.source,
                        "error": code,
                    }),
                    "case {case:?}: the handler's call must have reached the serving service and \
                     been refused by it, got {reported}"
                );
                match still_holds {
                    Some((value, version)) => {
                        let held = wait_for_parameter(&router, case, row.record).await;
                        assert_eq!(
                            (held["Value"].as_str(), held["Version"].as_u64()),
                            (Some(value), Some(version)),
                            "case {case:?}: the refused write must have left {:?} as it was, got \
                             {held}",
                            row.record
                        );
                    }
                    None => assert_absent(&router, case, row.record).await,
                }
            }
            Outcome::NotDelivered => {
                assert_absent(&router, case, row.report).await;
                assert_absent(&router, case, row.record).await;
            }
        }
    }
}
