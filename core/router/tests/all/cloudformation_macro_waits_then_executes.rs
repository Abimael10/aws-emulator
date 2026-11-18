// SPDX-License-Identifier: CC0-1.0

//! A change set whose macro's function WAITS: the change set is held open for as long as the
//! macro runs, and what the macro's own run comes to decides whether the change set can be
//! executed afterwards.
//!
//! The rule under test: **a handler's own wait is part of the span its function's `Timeout`
//! allows it — "the amount of time that Lambda allows a function to run before stopping it" —
//! so a macro whose function waits INSIDE that span answers and its change set reaches
//! `CREATE_COMPLETE` (the identical `ExecuteChangeSet` that was refused while it ran then
//! succeeds and deploys the resources the macro's document declares), while one whose function
//! waits PAST that span is stopped by its own `Timeout` and settles the change set `FAILED`
//! with the transform's own failure, which states the `Timeout` the run was configured for.**
//!
//! Three crates have to agree for that to hold — CloudFormation registers the macro and
//! invokes it, AWS Lambda runs the function, Amazon SNS answers for the topics the expanded
//! template declares — so it is pinned here, against the emulator's own assembled service set.
//! The rows differ along what the rule generalises over: the span the macro's handler asks to
//! wait for against the `Timeout` its function is configured with, on both sides of that span
//! and on both sides of the span this emulator really spends on a handler's wait.
//!
//! The WINDOW itself — that `ExecuteChangeSet` is refused with `InvalidChangeSetStatus` while
//! the macro runs — is also asserted here for every row, because the refusal has to leave the
//! change set and its stack untouched for the execution that follows to be the identical call.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// How long to wait between polls of a status, and how long to keep looking before giving up.
/// Generous: the wait is on the CONDITION, never on a span of this suite's own.
const POLL: Duration = Duration::from_millis(25);
const DEADLINE: Duration = Duration::from_secs(120);

/// The emulator as it is ASSEMBLED, on one engine: the function CloudFormation invokes for a
/// macro is the one Lambda runs, and the topics the macro's document declares are created in
/// the service that owns them.
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

/// Call one awsQuery operation of `service`.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation operation.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// One Amazon SNS operation, for reading the topics the deployment really created back out of
/// the service that owns them.
async fn sns(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    query(router, "sns", "2010-03-31", action, members).await
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

/// Poll the stack until its `StackStatus` is one of `wanted`, and answer the status it reported
/// with the answer it was read from.
async fn wait_for_stack(
    router: &MultiServiceRouter,
    stack: &str,
    wanted: &[&str],
) -> (String, String) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (code, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {code} with body {body}"
        );
        let status = opt_element(&body, "StackStatus").unwrap_or_default();
        if wanted.contains(&status.as_str()) || Instant::now() >= giving_up_at {
            return (status, body);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The macro's function: it waits inside the HANDLER for the span the row names — which is the
/// macro being slow rather than this suite sleeping — and then answers the macro protocol with
/// a topic added to the fragment it was handed, which nothing but the function could state.
fn waiting_macro(asked_seconds: u32) -> String {
    format!(
        r#"
import time


def handler(event, context):
    time.sleep({asked_seconds})
    fragment = event["fragment"]
    fragment["Resources"]["AddedByTheMacro"] = {{"Type": "AWS::SNS::Topic"}}
    return {{
        "requestId": event["requestId"],
        "status": "success",
        "fragment": fragment,
    }}
"#
    )
}

/// The template that REGISTERS the macro: the function that performs it, the role that function
/// runs as, and the `AWS::CloudFormation::Macro` resource that names both.
fn registration(macro_name: &str, asked_seconds: u32, timeout_seconds: u32) -> String {
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
                    "Timeout": timeout_seconds,
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "Code": { "ZipFile": waiting_macro(asked_seconds) }
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

/// What the change set's creation has to come to once the macro's own run has ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// The macro's wait fitted inside its function's `Timeout`: it answered, so the plan is
    /// computed over the document it produced and the change set can be executed.
    Completes,
    /// The macro's wait was past its function's `Timeout`: the run was stopped by it, so the
    /// change set carries the transform's own failure and offers no plan.
    TimesOut,
}

/// One row: the span the macro's handler asks to wait for, the `Timeout` its function is
/// configured with, and what the change set's creation comes to.
struct Row {
    case: &'static str,
    asked_seconds: u32,
    timeout_seconds: u32,
    outcome: Outcome,
}

/// The rows, spanning the rule: a wait far past the span this emulator really spends on one but
/// well inside the function's own `Timeout`, a wait inside a `Timeout` smaller than that span,
/// and waits past the `Timeout` on either side of it.
const ROWS: &[Row] = &[
    Row {
        case: "a wait of minutes, inside a Timeout that allows it",
        asked_seconds: 90,
        timeout_seconds: 120,
        outcome: Outcome::Completes,
    },
    Row {
        case: "a short wait, inside a short Timeout",
        asked_seconds: 2,
        timeout_seconds: 10,
        outcome: Outcome::Completes,
    },
    Row {
        case: "a wait of minutes, past a Timeout of a few seconds",
        asked_seconds: 300,
        timeout_seconds: 4,
        outcome: Outcome::TimesOut,
    },
    Row {
        case: "a wait past a Timeout above the span one wait really takes",
        asked_seconds: 600,
        timeout_seconds: 45,
        outcome: Outcome::TimesOut,
    },
];

/// Deploy the macro's own stack and answer once CloudFormation reports it `CREATE_COMPLETE`.
async fn register_macro(router: &MultiServiceRouter, stack: &str, macro_name: &str, row: &Row) {
    let (code, body) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            (
                "TemplateBody",
                &registration(macro_name, row.asked_seconds, row.timeout_seconds),
            ),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {:?}: expected HTTP 200 from CreateStack for the macro's own stack, got {code} \
         with body {body}",
        row.case
    );
    let (reached, described) = wait_for_stack(router, stack, &["CREATE_COMPLETE"]).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {:?}: the macro's own stack must deploy, got {described}",
        row.case
    );
}

/// The whole rule, one row at a time: the macro's stack, the change set over a template that
/// invokes it, and what the change set comes to once the macro's run has ended.
#[tokio::test]
async fn a_macro_whose_function_waits_inside_its_timeout_answers_and_its_change_set_executes() {
    let router = build_router();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let macro_name = format!("WaitingMacro{index}");
        register_macro(
            &router,
            &format!("macro-registration-{index}"),
            &macro_name,
            row,
        )
        .await;

        // A change set over a template that invokes the macro, for a stack that does not exist
        // yet: a CREATE change set leaves its stack in review until it is executed.
        let stack = format!("macro-change-set-{index}");
        let invoking = json!({
            "Transform": macro_name,
            "Resources": { "Declared": { "Type": "AWS::SNS::Topic" } }
        })
        .to_string();
        let (code, body) = cloudformation(
            &router,
            "CreateChangeSet",
            &[
                ("StackName", &stack),
                ("ChangeSetName", "plan"),
                ("ChangeSetType", "CREATE"),
                ("TemplateBody", &invoking),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected HTTP 200 from CreateChangeSet, got {code} with body {body}"
        );
        let change_set = opt_element(&body, "Id").unwrap_or_default();

        // While the macro's function runs the creation is reported IN PROGRESS, and in that
        // status the change set cannot be executed: there is no plan yet.
        let (status, described) =
            wait_for_change_set(&router, &change_set, &["CREATE_IN_PROGRESS"]).await;
        assert_eq!(
            status, "CREATE_IN_PROGRESS",
            "case {case:?}: expected the creation to report CREATE_IN_PROGRESS while the macro's \
             function runs, got {described}"
        );
        let (code, refused) =
            cloudformation(&router, "ExecuteChangeSet", &[("ChangeSetName", &change_set)]).await;
        assert_eq!(
            code, 400,
            "case {case:?}: expected ExecuteChangeSet to be refused while the macro runs, got \
             {code} with body {refused}"
        );
        assert_eq!(
            opt_element(&refused, "Code").as_deref(),
            Some("InvalidChangeSetStatus"),
            "case {case:?}: expected the modelled InvalidChangeSetStatus code, got {refused}"
        );
        let expected = format!(
            "ChangeSet [{change_set}] cannot be executed in its current status of \
             [CREATE_IN_PROGRESS]"
        );
        assert_eq!(
            opt_element(&refused, "Message").as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the refusal {expected:?}, got {refused}"
        );
        // ...and the refusal changed nothing: the change set is still being created and its
        // stack is still in review, which is what makes the execution below the same call.
        let (_, described) =
            cloudformation(&router, "DescribeChangeSet", &[("ChangeSetName", &change_set)]).await;
        assert_eq!(
            opt_element(&described, "Status").as_deref(),
            Some("CREATE_IN_PROGRESS"),
            "case {case:?}: the refused execution must leave the creation in progress, got \
             {described}"
        );
        let (_, stacks) = cloudformation(&router, "DescribeStacks", &[("StackName", &stack)]).await;
        assert_eq!(
            opt_element(&stacks, "StackStatus").as_deref(),
            Some("REVIEW_IN_PROGRESS"),
            "case {case:?}: the refused execution must leave the stack in review, got {stacks}"
        );

        let (status, settled) =
            wait_for_change_set(&router, &change_set, &["CREATE_COMPLETE", "FAILED"]).await;
        match row.outcome {
            Outcome::TimesOut => assert_timed_out(row, &status, &settled),
            Outcome::Completes => {
                assert_eq!(
                    status, "CREATE_COMPLETE",
                    "case {case:?}: the macro's wait is inside the {} s its function is \
                     configured for, so the creation must complete over the document it \
                     produced, got {settled}",
                    row.timeout_seconds
                );
                executes(&router, case, &stack, &change_set, &settled).await;
            }
        }
    }
}

/// The rule read off a row whose macro ran past its function's `Timeout`: the change set
/// carries the transform's own failure, which states the span the function was configured for
/// and not the span its handler asked to wait for, and offers no plan to execute.
fn assert_timed_out(row: &Row, status: &str, settled: &str) {
    let case = row.case;
    assert_eq!(
        status, "FAILED",
        "case {case:?}: the macro's wait is past the {} s its function is configured for, so the \
         creation must fail on the transform, got {settled}",
        row.timeout_seconds
    );
    let reason = opt_element(settled, "StatusReason").unwrap_or_default();
    let stated = format!(
        "Task timed out after {:.2} seconds",
        f64::from(row.timeout_seconds)
    );
    assert!(
        reason.contains("failed with:") && reason.contains(&stated),
        "case {case:?}: expected the transform's own failure stating {stated:?} — the function's \
         own Timeout, not the {} s its handler asked for — got {reason:?}",
        row.asked_seconds
    );
    assert!(
        elements(settled, "LogicalResourceId").is_empty(),
        "case {case:?}: a macro that did not answer offers no plan, got {settled}"
    );
}

/// The rule read off a row whose macro answered: the plan is the macro's own output, the
/// identical `ExecuteChangeSet` that was refused while the macro ran now succeeds, and the
/// topics the expanded document declares are really created in the service that owns them.
async fn executes(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    change_set: &str,
    settled: &str,
) {
    let planned = elements(settled, "LogicalResourceId");
    for logical_id in ["Declared", "AddedByTheMacro"] {
        assert!(
            planned.contains(&logical_id.to_string()),
            "case {case:?}: expected the plan to carry {logical_id:?} — the macro's own output — \
             got {planned:?}"
        );
    }
    let (code, body) =
        cloudformation(router, "ExecuteChangeSet", &[("ChangeSetName", change_set)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected the identical ExecuteChangeSet to succeed once the macro had \
         answered, got {code} with body {body}"
    );
    let (reached, described) = wait_for_stack(router, stack, &["CREATE_COMPLETE"]).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the executed change set to create the stack, got {described}"
    );
    let (_, resources) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    let deployed: Vec<(String, String)> = elements(&resources, "LogicalResourceId")
        .into_iter()
        .zip(elements(&resources, "PhysicalResourceId"))
        .collect();
    let (_, topics) = sns(router, "ListTopics", &[]).await;
    for logical_id in ["Declared", "AddedByTheMacro"] {
        let physical = deployed
            .iter()
            .find(|(declared, _)| declared == logical_id)
            .map(|(_, physical)| physical.clone())
            .unwrap_or_default();
        assert!(
            !physical.is_empty(),
            "case {case:?}: expected the stack to hold {logical_id:?} after the execution, got \
             {deployed:?}"
        );
        // The resource is one the OWNING service holds: a stack that reports a topic created is
        // only right if Amazon SNS answers for it.
        assert!(
            topics.contains(&physical),
            "case {case:?}: expected Amazon SNS to hold the topic {physical:?} the stack \
             published for {logical_id:?}, got {topics}"
        );
    }
}
