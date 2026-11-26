// SPDX-License-Identifier: CC0-1.0

//! A change set over a template that invokes a REGISTERED macro: CloudFormation performs the
//! macro before it plans anything, and the statuses a client polls follow that work.
//!
//! The rule under test: **a template's `Transform` section names macros CloudFormation
//! performs before the template is planned from, and a macro a client registered is performed
//! by INVOKING the function its `AWS::CloudFormation::Macro` resource names — so a change set
//! over such a template reports `CREATE_IN_PROGRESS` while that function runs, cannot be
//! executed while it does (`InvalidChangeSetStatus`), and reaches a terminal status decided by
//! what the macro answered: `CREATE_COMPLETE` with a plan over the document the macro
//! produced, or `FAILED` carrying the transform's own failure.**
//!
//! The rows of the table below differ along what the rule generalises over: what the macro
//! DOES to the fragment (adds a resource, rewrites a property, reads the parameters its
//! declaration carried) and how it ENDS (answering `success`, answering a failure status,
//! raising) — so the plan a caller reviews is in every case the macro's own output, and a
//! macro that did not succeed offers none.
//!
//! The WINDOW itself — that the creation reports `CREATE_IN_PROGRESS` for as long as the
//! function runs and that `ExecuteChangeSet` is refused in that status — is pinned against the
//! emulator's own assembled service set, in
//! `core/router/tests/all/cloudformation_private_macro_change_sets.rs`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, member, router_with_engine, xml_text, xml_texts, REGION};

/// How long to wait between polls of a status, and how long to keep looking before giving up.
/// Generous: the wait is on the CONDITION, and a macro takes as long as its function takes.
const POLL: Duration = Duration::from_millis(25);
const DEADLINE: Duration = Duration::from_secs(60);

/// One Lambda `restJson1` read, for the wait on the macro's function coming up.
async fn lambda(router: &MultiServiceRouter, uri: &str) -> Value {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/lambda/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    serde_json::from_slice(response.body()).unwrap_or(Value::Null)
}

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`: an
/// invocation of a function that service is still bringing up is no evidence about a macro.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = String::new();
    while Instant::now() < giving_up_at {
        let configuration =
            lambda(router, &format!("/2015-03-31/functions/{name}/configuration")).await;
        last = configuration
            .get("State")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if last == "Active" {
            return last;
        }
        tokio::time::sleep(POLL).await;
    }
    last
}

/// The `StackStatus` of `stack` right now, with the answer it was read from.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (code, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "DescribeStacks {stack:?}: expected HTTP 200, got {code} with body {body}"
    );
    (xml_text(&body, "StackStatus").unwrap_or_default(), body)
}

/// Poll the change set until its `Status` is one of `wanted`, and answer the status it
/// reported with the answer it was read from.
async fn wait_for_change_set(
    router: &MultiServiceRouter,
    change_set: &str,
    wanted: &[&str],
) -> (String, String) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (code, body) = call(
            router,
            "DescribeChangeSet",
            &[member("ChangeSetName", change_set)],
        )
        .await;
        assert_eq!(
            code, 200,
            "DescribeChangeSet {change_set:?}: expected HTTP 200, got {code} with body {body}"
        );
        let status = xml_text(&body, "Status").unwrap_or_default();
        if wanted.contains(&status.as_str()) || Instant::now() >= giving_up_at {
            return (status, body);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The trust policy a function's execution role carries.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole"
        }]
    })
}

/// The template that REGISTERS a macro: the function that performs it, the role that function
/// runs as, and the `AWS::CloudFormation::Macro` resource that names the macro and its
/// function.
pub(crate) fn registration(macro_name: &str, handler: &str) -> String {
    json!({
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": { "AssumeRolePolicyDocument": trust_policy() }
            },
            "MacroFunction": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": 60,
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "Code": { "ZipFile": handler }
                }
            },
            "Macro": {
                "Type": "AWS::CloudFormation::Macro",
                "Properties": {
                    "Name": macro_name,
                    "FunctionName": { "Fn::GetAtt": ["MacroFunction", "Arn"] }
                }
            }
        },
        "Outputs": { "FunctionName": { "Value": { "Ref": "MacroFunction" } } }
    })
    .to_string()
}

/// Deploy the registration stack and wait until the function it declares is one Lambda reports
/// `Active`, so the macro's invocation reaches a function that can run.
///
/// Shared with the suite that deploys a stack whose `Transform` names a registered macro
/// (`macro_transform_deployment.rs`): registering a macro is the same work whichever operation
/// then performs it, so it is spelled once.
pub(crate) async fn register_macro(
    router: &MultiServiceRouter,
    stack: &str,
    macro_name: &str,
    handler: &str,
) {
    let template = registration(macro_name, handler);
    let (code, body) = call(
        router,
        "CreateStack",
        &[
            member("StackName", stack),
            member("TemplateBody", &template),
            member("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "CreateStack {stack:?}: expected HTTP 200, got {code} with body {body}"
    );
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = stack_status(router, stack).await;
    while last.0 != "CREATE_COMPLETE" && Instant::now() < giving_up_at {
        tokio::time::sleep(POLL).await;
        last = stack_status(router, stack).await;
    }
    if last.0 != "CREATE_COMPLETE" {
        let (_, events) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
        panic!(
            "the macro's own stack must deploy, got {} with resource failures {:?}",
            last.1,
            xml_texts(&events, "ResourceStatusReason")
        );
    }
    let (_, resources) =
        call(router, "DescribeStackResources", &[member("StackName", stack)]).await;
    let function = xml_texts(&resources, "LogicalResourceId")
        .into_iter()
        .zip(xml_texts(&resources, "PhysicalResourceId"))
        .find(|(logical_id, _)| logical_id == "MacroFunction")
        .map(|(_, physical)| physical)
        .unwrap_or_default();
    let state = wait_until_active(router, &function).await;
    assert_eq!(
        state, "Active",
        "expected Lambda to report the macro's function {function:?} Active, got {state:?}"
    );
}

/// Create a CREATE change set over `template` for `stack` and answer the change set id the
/// call minted.
async fn create_change_set(
    router: &MultiServiceRouter,
    stack: &str,
    change_set: &str,
    template: &str,
) -> String {
    let (code, body) = call(
        router,
        "CreateChangeSet",
        &[
            member("StackName", stack),
            member("ChangeSetName", change_set),
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", template),
            member("Capabilities.member.1", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "CreateChangeSet {change_set:?}: expected HTTP 200, got {code} with body {body}"
    );
    xml_text(&body, "Id").unwrap_or_default()
}

/// A macro handler that answers the macro protocol: the `requestId` it was given, the status
/// `success` and the fragment `body` computes from the one it was handed.
pub(crate) fn handler(body: &str) -> String {
    format!(
        r#"
def handler(event, context):
    fragment = event["fragment"]
    params = event.get("params") or {{}}
    parameters = event.get("templateParameterValues") or {{}}
{body}
    return {{
        "requestId": event["requestId"],
        "status": "success",
        "fragment": fragment,
    }}
"#
    )
}

/// A macro that ADDS a resource to the fragment it was handed.
fn adds_a_resource() -> String {
    handler(r#"    fragment["Resources"]["AddedByTheMacro"] = {"Type": "AWS::SNS::Topic"}"#)
}

/// A macro that REWRITES a property of the fragment it was handed.
fn rewrites_a_property() -> String {
    handler(
        r#"    fragment["Resources"]["Queue"]["Properties"] = {"QueueName": "renamed-by-the-macro"}"#,
    )
}

/// A macro that reads the `params` its own `Transform` declaration carried, and the template
/// parameter values in force, into the fragment.
fn reads_its_parameters() -> String {
    handler(
        r#"    fragment["Resources"]["Queue"]["Properties"] = {
        "QueueName": params["QueueName"] + "-" + parameters["Suffix"],
    }"#,
    )
}

/// A macro that answers a FAILURE status rather than `success`.
fn answers_a_failure() -> String {
    r#"
def handler(event, context):
    return {
        "requestId": event["requestId"],
        "status": "failure",
        "errorMessage": "the macro refused this fragment",
    }
"#
    .to_string()
}

/// A macro whose handler RAISES.
fn raises() -> String {
    r#"
def handler(event, context):
    raise RuntimeError("the macro blew up")
"#
    .to_string()
}

/// The template a row invokes `macro_name` from: one resource of its own, which the macro then
/// adds to or rewrites.
fn invoking(macro_name: &str) -> String {
    json!({
        "Transform": macro_name,
        "Resources": { "Queue": { "Type": "AWS::SQS::Queue" } }
    })
    .to_string()
}

/// The same template, invoking the macro WITH parameters and declaring one of its own.
fn invoking_with_parameters(macro_name: &str) -> String {
    json!({
        "Transform": { "Name": macro_name, "Parameters": { "QueueName": "from-the-params" } },
        "Parameters": { "Suffix": { "Type": "String", "Default": "default-suffix" } },
        "Resources": { "Queue": { "Type": "AWS::SQS::Queue" } }
    })
    .to_string()
}

/// One row of the table: the macro's handler, the template that invokes it, and what the plan
/// the change set offers has to say once the macro has answered.
struct Row {
    case: &'static str,
    /// The macro's Python handler.
    handler: fn() -> String,
    /// The template the change set is created over, built from the macro's name.
    template: fn(&str) -> String,
    /// The `ChangeSetStatus` the creation has to reach.
    status: &'static str,
    /// The logical ids the plan has to carry, which are the resources the macro's OUTPUT
    /// declares.
    planned: &'static [&'static str],
    /// What the change set's `StatusReason` has to contain, for a macro that did not succeed.
    reason: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a macro that adds a resource to the fragment",
        handler: adds_a_resource,
        template: invoking,
        status: "CREATE_COMPLETE",
        planned: &["Queue", "AddedByTheMacro"],
        reason: None,
    },
    Row {
        case: "a macro that rewrites a property of the fragment",
        handler: rewrites_a_property,
        template: invoking,
        status: "CREATE_COMPLETE",
        planned: &["Queue"],
        reason: None,
    },
    Row {
        case: "a macro that reads its own params and the template's parameter values",
        handler: reads_its_parameters,
        template: invoking_with_parameters,
        status: "CREATE_COMPLETE",
        planned: &["Queue"],
        reason: None,
    },
    Row {
        case: "a macro that answers a failure status",
        handler: answers_a_failure,
        template: invoking,
        status: "FAILED",
        planned: &[],
        reason: Some("the macro refused this fragment"),
    },
    Row {
        case: "a macro whose handler raises",
        handler: raises,
        template: invoking,
        status: "FAILED",
        planned: &[],
        reason: Some("the macro blew up"),
    },
];

#[tokio::test]
async fn a_change_set_is_planned_from_what_the_macro_answered() {
    let (router, _engine) = router_with_engine();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let macro_name = format!("MacroUnderTest{index}");
        register_macro(
            &router,
            &format!("macro-registration-{index}"),
            &macro_name,
            &(row.handler)(),
        )
        .await;

        let stack = format!("macro-change-set-{index}");
        let change_set = create_change_set(
            &router,
            &stack,
            &format!("plan-{index}"),
            &(row.template)(&macro_name),
        )
        .await;

        // The creation reaches the status the macro's own answer decides, and nothing before
        // that is terminal: this waits for the CONDITION rather than for a span.
        let (status, described) =
            wait_for_change_set(&router, &change_set, &["CREATE_COMPLETE", "FAILED"]).await;
        assert_eq!(
            status, row.status,
            "case {case:?}: expected the change set to reach {}, got {described}",
            row.status
        );
        if let Some(reason) = row.reason {
            let reported = xml_text(&described, "StatusReason").unwrap_or_default();
            assert!(
                reported.contains("failed with:") && reported.contains(reason),
                "case {case:?}: expected the transform's own failure naming {reason:?}, got \
                 {reported:?}"
            );
        }
        let planned = xml_texts(&described, "LogicalResourceId");
        for logical_id in row.planned {
            assert!(
                planned.contains(&(*logical_id).to_string()),
                "case {case:?}: expected the plan to carry {logical_id:?} — the macro's own \
                 output — got {planned:?}"
            );
        }
        if row.planned.is_empty() {
            assert!(
                planned.is_empty(),
                "case {case:?}: expected a macro that did not succeed to offer no plan, got \
                 {planned:?}"
            );
        }
    }
}

