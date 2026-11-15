// SPDX-License-Identifier: CC0-1.0

//! A change set whose template invokes a REGISTERED macro cannot be executed while that
//! macro's function is still running.
//!
//! The rule under test: **a template's `Transform` section names macros CloudFormation
//! performs before the template is planned from, and a macro a client registered is performed
//! by INVOKING the function its `AWS::CloudFormation::Macro` resource names — so a change set
//! over such a template reports `CREATE_IN_PROGRESS` for as long as that function runs, cannot
//! be executed in that status (`InvalidChangeSetStatus`), and reaches a terminal status only
//! once the macro has answered.** Three crates have to agree for that to hold —
//! CloudFormation registers the macro and invokes it, AWS Lambda runs the function, IAM
//! answers for the role it runs as — so the window itself is pinned here, against the
//! emulator's own assembled service set.
//!
//! What the macro's ANSWER decides — the plan computed over the document it produced, and the
//! transform failure a macro that raised or answered a failure status settles the change set
//! with — is pinned in the crate that owns it, over a table spanning those outcomes:
//! `services/service-com-amazonaws-cloudformation-cloudformation/tests/all/private_macro_change_sets.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// How long to wait between polls of a status, and how long to keep looking before giving up.
/// Generous: the wait is on the CONDITION, and a macro takes as long as its function takes.
const POLL: Duration = Duration::from_millis(25);
const DEADLINE: Duration = Duration::from_secs(60);

/// The emulator as it is ASSEMBLED, on one engine: the function CloudFormation invokes for a
/// macro is the one Lambda runs.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
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
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Lambda `restJson1` read, for the wait on the macro's function coming up.
async fn lambda(router: &MultiServiceRouter, uri: &str) -> Value {
    let request = http::Request::builder()
        .method("GET")
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
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

/// Poll the change set until its `Status` is one of `wanted`, and answer the status it reported
/// with the answer it was read from.
async fn wait_for_change_set(
    router: &MultiServiceRouter,
    change_set: &str,
    wanted: &[&str],
) -> (String, String) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (code, body) =
            cloudformation(router, "DescribeChangeSet", &[("ChangeSetName", change_set)]).await;
        assert_eq!(
            code, 200,
            "DescribeChangeSet {change_set:?}: expected HTTP 200, got {code} with body {body}"
        );
        let status = opt_element(&body, "Status").unwrap_or_default();
        if wanted.contains(&status.as_str()) || Instant::now() >= giving_up_at {
            return (status, body);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The macro's function: it waits inside the HANDLER, which is the macro being slow rather
/// than this suite sleeping, and then answers the macro protocol with a resource added to the
/// fragment it was handed.
const SLOW_MACRO: &str = r#"
import time


def handler(event, context):
    time.sleep(5)
    fragment = event["fragment"]
    fragment["Resources"]["AddedByTheMacro"] = {"Type": "AWS::SNS::Topic"}
    return {
        "requestId": event["requestId"],
        "status": "success",
        "fragment": fragment,
    }
"#;

/// The template that REGISTERS the macro: the function that performs it, the role that function
/// runs as, and the `AWS::CloudFormation::Macro` resource that names both.
fn registration(macro_name: &str) -> String {
    json!({
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole"
                        }]
                    }
                }
            },
            "MacroFunction": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Timeout": 60,
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "Code": { "ZipFile": SLOW_MACRO }
                }
            },
            "Macro": {
                "Type": "AWS::CloudFormation::Macro",
                "Properties": {
                    "Name": macro_name,
                    "FunctionName": { "Fn::GetAtt": ["MacroFunction", "Arn"] }
                }
            }
        }
    })
    .to_string()
}

/// A change set whose macro's function is still RUNNING holds the creation open: the change set
/// reports `CREATE_IN_PROGRESS`, which is not a status it can be executed in, and once the
/// macro answers the creation completes over the document it produced.
#[tokio::test]
async fn a_change_set_whose_macro_is_still_running_cannot_be_executed() {
    let router = build_router();
    let macro_name = "SlowMacroUnderTest";

    // The macro's own stack: the function and the registration that names it.
    let (code, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", "macro-registration"),
            ("TemplateBody", &registration(macro_name)),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "expected HTTP 200 from CreateStack for the macro's own stack, got {code} with body \
         {body}"
    );
    let giving_up_at = Instant::now() + DEADLINE;
    let mut described = String::new();
    let mut reached = String::new();
    while reached != "CREATE_COMPLETE" && Instant::now() < giving_up_at {
        let (_, body) = cloudformation(
            &router,
            "DescribeStacks",
            &[("StackName", "macro-registration")],
        )
        .await;
        reached = opt_element(&body, "StackStatus").unwrap_or_default();
        described = body;
        if reached == "CREATE_COMPLETE" {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "the macro's own stack must deploy, got {described}"
    );
    let (_, resources) = cloudformation(
        &router,
        "DescribeStackResources",
        &[("StackName", "macro-registration")],
    )
    .await;
    let function = elements(&resources, "LogicalResourceId")
        .into_iter()
        .zip(elements(&resources, "PhysicalResourceId"))
        .find(|(logical_id, _)| logical_id == "MacroFunction")
        .map(|(_, physical)| physical)
        .unwrap_or_default();
    let state = wait_until_active(&router, &function).await;
    assert_eq!(
        state, "Active",
        "expected Lambda to report the macro's function {function:?} Active, got {state:?}"
    );

    // A change set over a template that invokes it.
    let invoking = json!({
        "Transform": macro_name,
        "Resources": { "Queue": { "Type": "AWS::SQS::Queue" } }
    })
    .to_string();
    let (code, body) = cloudformation(
        &router,
        "CreateChangeSet",
        &[
            ("StackName", "macro-change-set"),
            ("ChangeSetName", "plan"),
            ("ChangeSetType", "CREATE"),
            ("TemplateBody", &invoking),
            ("Capabilities.member.1", "CAPABILITY_AUTO_EXPAND"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "expected HTTP 200 from CreateChangeSet, got {code} with body {body}"
    );
    let change_set = opt_element(&body, "Id").unwrap_or_default();

    // The creation is reported IN PROGRESS while the function runs — the modelled status a
    // creation that has not concluded is in.
    let (status, described) =
        wait_for_change_set(&router, &change_set, &["CREATE_IN_PROGRESS"]).await;
    assert_eq!(
        status, "CREATE_IN_PROGRESS",
        "expected the creation to report CREATE_IN_PROGRESS while the macro runs, got {described}"
    );
    assert_eq!(
        opt_element(&described, "ExecutionStatus").as_deref(),
        Some("UNAVAILABLE"),
        "expected a change set with no plan yet to be unavailable for execution, got {described}"
    );

    // ...and in that status it cannot be executed: the plan does not exist yet.
    let (code, body) = cloudformation(
        &router,
        "ExecuteChangeSet",
        &[("ChangeSetName", &change_set)],
    )
    .await;
    assert_eq!(
        code, 400,
        "expected ExecuteChangeSet to be refused while the macro runs, got {code} with body \
         {body}"
    );
    assert_eq!(
        opt_element(&body, "Code").as_deref(),
        Some("InvalidChangeSetStatus"),
        "expected the modelled InvalidChangeSetStatus code, got {body}"
    );
    assert!(
        body.contains("CREATE_IN_PROGRESS"),
        "expected the refusal to name the status that refuses the call, got {body}"
    );

    // Once the macro has answered, the creation completes over the document it produced — the
    // plan carries the resource the macro added, which nothing but the function could state.
    let (status, described) =
        wait_for_change_set(&router, &change_set, &["CREATE_COMPLETE", "FAILED"]).await;
    assert_eq!(
        status, "CREATE_COMPLETE",
        "expected the creation to complete once the macro answered, got {described}"
    );
    let planned = elements(&described, "LogicalResourceId");
    assert!(
        planned.contains(&"AddedByTheMacro".to_string()),
        "expected the plan to carry the resource the macro added, got {planned:?}"
    );
}
