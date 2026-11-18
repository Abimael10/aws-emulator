// SPDX-License-Identifier: CC0-1.0

//! A Lambda-backed CUSTOM RESOURCE whose provider function makes an AWS call its EXECUTION
//! ROLE is not granted, and what the stack behind it does — end to end over the wire.
//!
//! RULE under test, in two halves that only hold together:
//!
//! 1. **A running handler's SDK call is signed as the function's execution role, and the
//!    service that serves it decides the call against that role's identity-based policies: a
//!    read the role carries no grant for is REFUSED, so the exception leaves the handler and
//!    CloudFormation is never answered.** The complement is the positive control below: the
//!    same stack whose role DOES grant the read is served, the provider answers, and the value
//!    it read is what the stack publishes.
//! 2. **CloudFormation does not know what a provider did before it stopped answering, so a
//!    request it never answered fails the resource with CloudFormation's own sentence for it
//!    and the rollback asks that provider to DELETE the resource; a `Delete` the provider does
//!    not answer either is a rollback that could not be carried out, which leaves the stack
//!    `ROLLBACK_FAILED`** — the state an operator has to clean up from, never the clean
//!    `ROLLBACK_COMPLETE` a client reads as "the stack is back to a consistent state".
//!
//! Five services have to agree for that to hold, and this crate's harness mounts them all on
//! one engine: CloudFormation sends the provider request and holds the resource until it is
//! answered, AWS Lambda runs the provider as its role, IAM holds that role, AWS Systems
//! Manager decides the read the provider makes, and Amazon CloudWatch Logs holds what the run
//! printed — which is what says WHY the provider never answered.
//!
//! The rows differ along what the rule generalises over: whether the execution role grants the
//! read the provider makes (the refusal and its positive control) and how the custom
//! resource's type is spelled (the generic `AWS::CloudFormation::CustomResource` and an
//! author's own `Custom::<Name>`).
//!
//! Nothing here sleeps for a span the emulator owes: the wait CloudFormation gives a provider
//! is reached by moving that wait past its own window on the store, and every other wait is a
//! poll for the CONDITION with a generous deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::stabilising::CREATE_STABILISATION_WINDOW;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, create_stack, member, router_with_engine, store, xml_text, REGION};
use crate::stack_lifecycle::acknowledging_iam;

/// How long a condition the clock owes is waited for, and how often it is looked at.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// The logical ids the rows declare their custom resource and their parameter under.
const CUSTOM: &str = "Waiter";
const SETTING: &str = "Setting";

/// What the provider prints for each request it is sent, so the log reader can tell the
/// `Create` it never answered from the `Delete` the rollback owes.
const SAW_PREFIX: &str = "provider saw ";

/// What it prints when the read it makes is refused, which is what says WHY it never answered.
const REFUSED_PREFIX: &str = "provider could not read the parameter: ";

/// The sentence CloudFormation words for a provider that did not answer, as the resource's own
/// event carries it.
const NO_RESPONSE: &str = "CloudFormation did not receive a response from your Custom Resource";

/// The value the stack writes into the parameter its provider reads, which is what the
/// positive control reads back out of the stack's own output.
const HELD_VALUE: &str = "read by the provider";

/// The provider: it reads ONE parameter with the SDK its runtime supplies and answers
/// CloudFormation with the value it read. A read the role is not granted raises, which is the
/// answer that never comes — and the handler prints the refusal first, so the log says why.
const PROVIDER_SOURCE: &str = r#"
import boto3
import cfnresponse


def handler(event, context):
    print("provider saw %s" % event["RequestType"])
    name = event["ResourceProperties"]["Parameter"]
    try:
        read = boto3.client("ssm").get_parameter(Name=name)
    except Exception as error:
        print("provider could not read the parameter: %s" % error)
        raise
    cfnresponse.send(
        event,
        context,
        cfnresponse.SUCCESS,
        {"Value": read["Parameter"]["Value"]},
        physicalResourceId="waiter-instance",
    )
"#;

/// One Amazon CloudWatch Logs call over the awsJson1_1 wire its model declares, signed for
/// `logs` in the Region these stacks live in — so what a case reads back is what the
/// provider's own run really wrote.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/logs/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://logs.{REGION}.amazonaws.com/"))
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header("authorization", authorization)
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed CloudWatch Logs request");
    let response = router.dispatch(request).await;
    let status_code = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status_code, parsed)
}

/// Every message the log group `group` holds right now.
///
/// `pub(crate)` because more than one suite of this target reads a group's events through
/// `FilterLogEvents`, and there is one spelling of that read: the suite over the group a stack
/// declares at a function's own log-group name
/// (`declared_log_group_takes_the_functions_log.rs`) reads the run's delivered log through it.
pub(crate) async fn logged_lines(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status_code, body) = logs(router, "FilterLogEvents", &json!({"logGroupName": group})).await;
    if status_code != 200 {
        return Vec::new();
    }
    body.get("events")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event.get("message").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// What the stack reports now: the `StackStatus` it carries and the body it was read from.
async fn described(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let (http_status, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        http_status, 200,
        "expected DescribeStacks to answer for {stack:?}, got {http_status} {body}"
    );
    (xml_text(&body, "StackStatus").unwrap_or_default(), body)
}

/// Move every wait the stack is holding for a provider's answer past its own window, which is
/// how a case reaches the instant such a wait has run out without sleeping it out.
///
/// The window is the span CloudFormation gives whoever was asked
/// ([`CREATE_STABILISATION_WINDOW`]), and the wait runs from the instant the request was sent,
/// so moving that instant back past the window is exactly the state a wait that ran out is in.
fn age_the_provider_waits(engine: &Arc<engine::InMemoryEngine>, stack_name: &str) {
    let past_the_window = CREATE_STABILISATION_WINDOW + Duration::from_secs(1);
    let held = store(engine);
    let mut locked = held.lock();
    let Some(stack) = locked.resolve_mut(REGION, stack_name) else {
        return;
    };
    for barrier in &mut stack.stabilisations {
        if barrier.provider_request.is_some() && barrier.released_at.is_none() {
            barrier.asked_at -= past_the_window;
        }
    }
}

/// Poll the stack — which is what drives what its clock owes — until its provider's log holds
/// a line carrying `needle`, and answer every line the log then holds.
async fn driven_until_logged(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    group: &str,
    needle: &str,
) -> Vec<String> {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        described(router, stack).await;
        let held = logged_lines(router, group).await;
        if held.iter().any(|line| line.contains(needle)) {
            return held;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the provider's log {group:?} to carry {needle:?}, got \
             {held:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Poll `stack` until it reports a status that is not `*_IN_PROGRESS`, moving every wait it
/// holds for a provider's answer past its window on the way when `silent` — the shape of a
/// provider that answers nothing at all.
async fn settled(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack: &str,
    silent: bool,
) -> (String, String) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (reported, body) = described(router, stack).await;
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return (reported, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {stack:?} to settle, last was {reported:?} with {body}"
        );
        if silent {
            age_the_provider_waits(engine, stack);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The `(ResourceStatus, ResourceStatusReason)` pairs the stack's events carry for ONE
/// resource, read as the PARALLEL member lists `DescribeStackEvents` renders — every event
/// reports a logical id and a status, and the reason is read off the event block the status
/// sits in.
async fn resource_events(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> Vec<(String, String)> {
    let (http_status, body) = call(
        router,
        "DescribeStackEvents",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected DescribeStackEvents to answer, got {http_status} {body}"
    );
    crate::harness::xml_blocks(&body, "member")
        .into_iter()
        .filter(|block| xml_text(block, "LogicalResourceId").as_deref() == Some(logical_id))
        .map(|block| {
            (
                xml_text(&block, "ResourceStatus").unwrap_or_default(),
                xml_text(&block, "ResourceStatusReason").unwrap_or_default(),
            )
        })
        .collect()
}

/// One row: whether the provider's execution role grants the read it makes, and how the custom
/// resource's type is spelled.
struct Row {
    case: &'static str,
    stack: &'static str,
    function: &'static str,
    parameter: &'static str,
    resource_type: &'static str,
    /// Whether the role the function runs as grants `ssm:GetParameter`, which is what decides
    /// whether the provider can answer at all.
    grants_the_read: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "an execution role that does not grant the read, the generic type",
        stack: "denied-provider-generic",
        function: "denied-provider-generic-handler",
        parameter: "/denied-provider/generic",
        resource_type: "AWS::CloudFormation::CustomResource",
        grants_the_read: false,
    },
    Row {
        case: "an execution role that does not grant the read, the author's own namespace",
        stack: "denied-provider-authored",
        function: "denied-provider-authored-handler",
        parameter: "/denied-provider/authored",
        resource_type: "Custom::Provisioner",
        grants_the_read: false,
    },
    Row {
        case: "the positive control: the same stack whose role grants the read",
        stack: "granted-provider",
        function: "granted-provider-handler",
        parameter: "/granted-provider/setting",
        resource_type: "Custom::Provisioner",
        grants_the_read: true,
    },
];

/// The statements the execution role of one row carries: the log writes every execution role
/// needs, plus the parameter read where the row grants it.
fn role_statements(row: &Row) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": "logs:*",
        "Resource": "*",
    })];
    if row.grants_the_read {
        statements.push(json!({
            "Effect": "Allow",
            "Action": "ssm:GetParameter",
            "Resource": "*",
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements})
}

/// The template one row deploys: the parameter the provider reads, the execution role it runs
/// as, the provider function, and the custom resource that function backs.
fn template(row: &Row) -> String {
    json!({
        "Resources": {
            SETTING: {
                "Type": "AWS::SSM::Parameter",
                "Properties": {
                    "Name": row.parameter,
                    "Type": "String",
                    "Value": HELD_VALUE,
                },
            },
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "what-the-provider-may-do",
                        "PolicyDocument": role_statements(row),
                    }],
                },
            },
            "Provider": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": row.function,
                    "Code": {"ZipFile": PROVIDER_SOURCE},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": 60,
                },
            },
            CUSTOM: {
                "Type": row.resource_type,
                "DependsOn": [SETTING, "Provider"],
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": ["Provider", "Arn"]},
                    "Parameter": row.parameter,
                },
            },
        },
        "Outputs": {
            "ReadValue": {"Value": {"Fn::GetAtt": [CUSTOM, "Value"]}},
        },
    })
    .to_string()
}

/// Deploy one row's stack, which must be accepted.
async fn deploy(router: &MultiServiceRouter, row: &Row) {
    let (http_status, body) =
        create_stack(router, row.stack, &template(row), &acknowledging_iam(&[])).await;
    assert_eq!(
        http_status, 200,
        "case {:?}: expected CreateStack to be accepted, got {http_status} {body}",
        row.case
    );
}

#[tokio::test]
async fn a_provider_refused_the_call_it_makes_never_answers_and_the_rollback_fails() {
    for row in ROWS.iter().filter(|row| !row.grants_the_read) {
        let case = row.case;
        let (router, engine) = router_with_engine();
        deploy(&router, row).await;
        let group = format!("/aws/lambda/{}", row.function);

        // 1. The provider RAN and Parameter Store refused the read its role carries no grant
        // for, so the handler raised before it could answer: the log says both.
        let logged = driven_until_logged(&router, case, row.stack, &group, REFUSED_PREFIX).await;
        assert!(
            logged
                .iter()
                .any(|line| line.contains(&format!("{SAW_PREFIX}Create"))),
            "case {case:?}: expected the log to carry the Create request the provider was \
             sent, got {logged:?}"
        );
        let refusal = logged
            .iter()
            .find(|line| line.contains(REFUSED_PREFIX))
            .cloned()
            .unwrap_or_default();
        assert!(
            refusal.contains("ssm:GetParameter"),
            "case {case:?}: expected the refusal the provider printed to be the one for the \
             read it is not granted, got {refusal:?}"
        );

        // 2. The wait for the answer that never came runs out, which fails the create — with
        // CloudFormation's own sentence for a provider that did not answer — and the rollback
        // sends that same provider the `Delete` it owes, whose run is refused the same read.
        age_the_provider_waits(&engine, row.stack);
        let logged = driven_until_logged(
            &router,
            case,
            row.stack,
            &group,
            &format!("{SAW_PREFIX}Delete"),
        )
        .await;
        assert!(
            logged
                .iter()
                .filter(|line| line.contains(REFUSED_PREFIX))
                .count()
                >= 2,
            "case {case:?}: expected the run for the rollback's Delete to be refused the same \
             read, got {logged:?}"
        );
        let events = resource_events(&router, case, row.stack, CUSTOM).await;
        let failed: Vec<&(String, String)> = events
            .iter()
            .filter(|(reported, _)| reported == resource_status::CREATE_FAILED)
            .collect();
        assert!(
            failed.iter().any(|(_, reason)| reason.contains(NO_RESPONSE)),
            "case {case:?}: expected the failed create to report {NO_RESPONSE:?}, got \
             {events:?}"
        );

        // 3. ...and that `Delete` goes unanswered too, so the rollback could not be carried
        // out: the stack rests in the FAILED rollback status, naming the resource it could
        // not remove.
        let (reported, body) = settled(&router, &engine, case, row.stack, true).await;
        assert_eq!(
            reported,
            status::ROLLBACK_FAILED,
            "case {case:?}: expected the stack to settle at {}, got {reported:?} with {body}",
            status::ROLLBACK_FAILED
        );
        let reason = xml_text(&body, "StackStatusReason").unwrap_or_default();
        assert!(
            reason.contains(CUSTOM),
            "case {case:?}: expected the stack's reason to name {CUSTOM} as the resource the \
             rollback could not delete, got {reason:?}"
        );
    }
}

#[tokio::test]
async fn a_provider_whose_role_grants_the_call_answers_and_the_stack_completes() {
    // The positive control of the same chain: nothing about the provider changes but the
    // statement its execution role carries, and with the grant in force the read is served,
    // the provider answers, and the value it read is what the stack publishes.
    let row = ROWS
        .iter()
        .find(|row| row.grants_the_read)
        .expect("the positive control row");
    let case = row.case;
    let (router, engine) = router_with_engine();
    deploy(&router, row).await;

    let (reported, body) = settled(&router, &engine, case, row.stack, false).await;
    assert_eq!(
        reported,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {}, got {reported:?} with {body}",
        status::CREATE_COMPLETE
    );
    let published = xml_text(&body, "OutputValue").unwrap_or_default();
    assert_eq!(
        published, HELD_VALUE,
        "case {case:?}: expected the stack to publish the value the provider read, got \
         {published:?} with {body}"
    );
    let logged = logged_lines(&router, &format!("/aws/lambda/{}", row.function)).await;
    assert!(
        !logged.iter().any(|line| line.contains(REFUSED_PREFIX)),
        "case {case:?}: expected the granted read to be served, got {logged:?}"
    );
}
