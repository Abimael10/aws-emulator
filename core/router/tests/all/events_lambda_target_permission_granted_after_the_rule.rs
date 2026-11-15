// SPDX-License-Identifier: CC0-1.0

//! A grant added to a Lambda function target AFTER the rule and its target already exist
//! changes what the NEXT matching delivery comes to — end to end over the real
//! `MultiServiceRouter` with CloudFormation, EventBridge, Lambda, Amazon SQS and IAM mounted
//! on one engine.
//!
//! The rule pinned here: **for a Lambda function target "EventBridge relies on resource-based
//! policies" (`PutTargets`), and that document is read AT EVERY DELIVERY — so the policy a
//! matching event's handover is decided against is the one the function carries at the instant
//! of that handover, not the one it carried when the rule or the target was written. A
//! statement a client adds afterwards admits the next matching delivery, with nothing about the
//! rule or the target changed, and a statement removed again stops admitting the delivery after
//! it.**
//!
//! Neither half depends on how the rule and its target came to exist, so both are read here
//! over the provenance a client cannot patch up by re-deploying: a CloudFormation stack that
//! declares the rule, its function target and no `AWS::Lambda::Permission` at all, whose
//! permission gap is then closed by a direct `lambda AddPermission` on the deployed function.
//! The observation is made where a client makes it — on the queue the deployed handler itself
//! writes to — so a handover recorded but never run fails here.
//!
//! Every row ends by granting the invocation unconditionally and publishing one more event,
//! and WAITS for that event to come out of the handler. That delivery is what makes the row's
//! "nothing ran" assertions mean something: the same rule, the same target and the same
//! handler are shown to deliver within the row, so an event that did not arrive was refused
//! rather than merely slow. The phases that must observe nothing are read only for their OWN
//! marker, since a received message becomes visible again.
//!
//! The rows differ along what the rule generalises over: WHAT the statement added after the
//! deployment says — scoped by `SourceArn` to the rule that holds the target, unscoped, or
//! scoped to another rule — and therefore whether the delivery that follows it is admitted.
//!
//! `crate::events_lambda_target_resource_policy` is the suite for which policy SHAPES admit the
//! invocation when the statement is already in place, and
//! `crate::cloudformation_events_rule_lambda_target_permission_boundary` for a grant the stack
//! itself declares; this one is the suite for the document being re-read per delivery.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, execution_role, lambda, stayed_quiet_about, wait_for, ACCOUNT,
    CREATE_COMPLETE, LOOK_INTERVAL, ROLE_NAME,
};

/// The Region every request here is signed for — the one `wait_for_stack_status` polls in.
const REGION: &str = "us-east-1";

/// The envelope members every event this suite publishes carries, and what each deployed rule's
/// `EventPattern` matches on: a `source`, a `detail-type` and one member nested inside `detail`.
const SOURCE: &str = "sim2real.events.regrant";
const DETAIL_TYPE: &str = "Target Permission Regrant Probe";
const EVENT_SOURCE: &str = "cloudfront.amazonaws.com";

/// The service principal EventBridge hands a resource-policy-governed target over as.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The IAM action an invocation of a function performs, which is what a grant for this service
/// has to name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The statement ids this suite writes under: the row's own grant, and the unconditional one
/// its closing control adds.
const STATEMENT_ID: &str = "AllowEventBridgeInvokeAfterFix";
const ANCHOR_STATEMENT_ID: &str = "AllowEventBridgeInvokeAnchor";

/// How many times the function's state is polled before the suite gives up on it.
const POLLS: usize = 600;

/// CloudFormation, EventBridge, Lambda, Amazon SQS and IAM over ONE engine, as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `events`, which is what routes the
/// request. The signature is never checked: this router runs with request authentication off.
fn events_authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/events/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("authorization", events_authorization())
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// Poll `GetFunctionConfiguration` until Lambda first reports `name` `Active` — "the function
/// is ready to be invoked" (`FunctionConfiguration$State`) — so a handover withheld later is
/// the policy's refusal and not the lifecycle's. The span waited for is that service's own, so
/// nothing here sleeps a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, name: &str) {
    for _ in 0..POLLS {
        let (_, reported) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        if reported.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("case {case:?}: Lambda never reported {name} Active");
}

/// Add one statement to the deployed function's resource-based policy through `AddPermission`,
/// which answers "201" with the created `Statement`.
async fn add_permission(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    statement_id: &str,
    source_arn: Option<&str>,
) {
    let mut body = json!({
        "StatementId": statement_id,
        "Action": INVOKE_ACTION,
        "Principal": EVENTS_PRINCIPAL,
    });
    if let Some(source_arn) = source_arn {
        body["SourceArn"] = json!(source_arn);
    }
    let (status, answered) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(body),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from Lambda AddPermission({statement_id}), got \
         {status} with {answered}"
    );
    assert!(
        answered["Statement"]
            .as_str()
            .is_some_and(|statement| !statement.is_empty()),
        "case {case:?}: expected AddPermission to answer the created statement, got {answered}"
    );
}

/// Withdraw one statement again through `RemovePermission`, whose answer is "204".
async fn remove_permission(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    statement_id: &str,
) {
    let (status, answered) = lambda(
        router,
        "DELETE",
        &format!("/2015-03-31/functions/{function}/policy/{statement_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "case {case:?}: expected HTTP 204 from Lambda RemovePermission({statement_id}), got \
         {status} with {answered}"
    );
}

/// The handler the stack's `AWS::Lambda::Function` declares inline: it writes the whole
/// envelope it was handed to the queue the same stack declares, which is where the invocation
/// is observed. The queue is addressed by NAME through the service's own `GetQueueUrl`, so the
/// source carries nothing the deployment had to compute.
const FORWARDING_SOURCE: &str = r#"
import json
import boto3


def lambda_handler(event, context):
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName="SINK_NAME")["QueueUrl"]
    client.send_message(QueueUrl=url, MessageBody=json.dumps(event))
    return {"forwarded": 1}
"#;

/// The template every row deploys: a queue, the forwarding function, and the rule whose single
/// target is that function — and DELIBERATELY no `AWS::Lambda::Permission`, so the only grants
/// the function ever carries are the ones a client adds afterwards.
fn template(stack: &str) -> String {
    let sink = format!("{stack}-sink");
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Sink": {
                "Type": "AWS::SQS::Queue",
                "Properties": { "QueueName": sink },
            },
            "Forwarder": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{stack}-forwarder"),
                    "Role": format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Timeout": 10,
                    "Code": { "ZipFile": FORWARDING_SOURCE.replace("SINK_NAME", &sink) },
                },
            },
            "Rule": {
                "Type": "AWS::Events::Rule",
                "Properties": {
                    "Name": format!("{stack}-rule"),
                    "State": "ENABLED",
                    "EventPattern": {
                        "source": [SOURCE],
                        "detail-type": [DETAIL_TYPE],
                        "detail": { "eventSource": [EVENT_SOURCE] },
                    },
                    "Targets": [{
                        "Id": "forwarder",
                        "Arn": { "Fn::GetAtt": ["Forwarder", "Arn"] },
                    }],
                },
            },
        },
        "Outputs": {
            "QueueUrl": { "Value": { "Ref": "Sink" } },
            "FunctionName": { "Value": { "Ref": "Forwarder" } },
            "RuleName": { "Value": { "Ref": "Rule" } },
            "RuleArn": { "Value": { "Fn::GetAtt": ["Rule", "Arn"] } },
        },
    })
    .to_string()
}

/// Publish one matching entry carrying `marker`, and assert EventBridge accepted it whatever
/// the delivery then comes to: "FailedEntryCount — the number of failed entries", which a
/// matched-but-undeliverable target is not one of.
async fn publish(router: &MultiServiceRouter, case: &str, marker: &str) {
    let answered = events(
        router,
        case,
        "PutEvents",
        &json!({
            "Entries": [{
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "eventSource": EVENT_SOURCE, "marker": marker }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        answered["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the entry to be accepted, got {answered}"
    );
    let entries = answered["Entries"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        entries.len(),
        1,
        "case {case:?}: expected one answered entry, got {answered}"
    );
    assert!(
        entries[0]["EventId"].as_str().is_some_and(|id| !id.is_empty()),
        "case {case:?}: expected the accepted entry to carry an EventId, got {answered}"
    );
}

/// Wait for the envelope carrying `marker` to come out of the handler, and assert the one that
/// arrived is the one that was published: the handler writes the envelope it was handed, so
/// `detail.marker` and `source` are the submitted ones.
async fn forwarded_envelope(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    marker: &str,
) -> Vec<String> {
    let seen = wait_for(router, url, marker).await;
    let carrying: Vec<&String> = seen.iter().filter(|body| body.contains(marker)).collect();
    assert!(
        !carrying.is_empty(),
        "case {case:?}: expected the handler to run and write the envelope carrying \
         {marker:?} to the queue, got {seen:?}"
    );
    let envelope: Value = serde_json::from_str(carrying[0]).unwrap_or(Value::Null);
    assert_eq!(
        envelope["detail"]["marker"].as_str(),
        Some(marker),
        "case {case:?}: expected the forwarded envelope's detail.marker to be {marker:?}, got \
         {envelope}"
    );
    assert_eq!(
        envelope["source"].as_str(),
        Some(SOURCE),
        "case {case:?}: expected the forwarded envelope to carry the published source \
         {SOURCE:?}, got {envelope}"
    );
    seen
}

/// Where the statement added after the deployment is scoped, which is the one thing the rows
/// differ in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Scoped by `SourceArn` to the rule the stack provisioned, which holds the target.
    ItsOwnRule,
    /// Declared with no `SourceArn` at all, so it is not scoped to a rule.
    Unscoped,
    /// Scoped by `SourceArn` to a rule that is not the one invoking the function.
    AnotherRule,
}

/// One row: how the afterwards-added statement is scoped, and whether the delivery it governs
/// is admitted.
struct Row {
    case: &'static str,
    stack: &'static str,
    scope: Scope,
    /// Whether the delivery made AFTER that statement was added runs the function.
    admitted: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the statement added afterwards is scoped by SourceArn to the rule that \
                   holds the target",
            stack: "events-regrant-own-rule",
            scope: Scope::ItsOwnRule,
            admitted: true,
        },
        Row {
            case: "the statement added afterwards names no SourceArn at all",
            stack: "events-regrant-unscoped",
            scope: Scope::Unscoped,
            admitted: true,
        },
        Row {
            case: "the statement added afterwards is scoped by SourceArn to another rule",
            stack: "events-regrant-other-rule",
            scope: Scope::AnotherRule,
            admitted: false,
        },
    ]
}

/// Every row: the stack deploys a rule whose Lambda target no statement admits, the delivery
/// before any grant runs nothing, the grant added afterwards decides the delivery after it,
/// and once every statement is withdrawn the delivery after that runs nothing again — each
/// negative read against a delivery the same row proves.
#[tokio::test]
async fn a_grant_added_after_the_rule_decides_the_next_delivery_to_its_lambda_target() {
    for row in rows() {
        let case = row.case;
        let router = build_router();
        execution_role(&router).await;

        cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &template(row.stack)),
            ],
        )
        .await;
        let (status, described) = wait_for_stack_status(&router, row.stack, CREATE_COMPLETE).await;
        assert_eq!(
            status, CREATE_COMPLETE,
            "case {case:?}: expected the stack to deploy, got {status} with {described}"
        );
        let outputs = outputs_of(&described);
        let output = |name: &str| {
            outputs.get(name).cloned().unwrap_or_else(|| {
                panic!("case {case:?}: expected the stack to publish {name}, got {described}")
            })
        };
        let queue_url = output("QueueUrl");
        let function = output("FunctionName");
        let rule_name = output("RuleName");
        let rule_arn = output("RuleArn");

        // Both resources the stack reports exist where their own services hold them, which is
        // what a client checks before reading anything into a delivery that did not happen.
        wait_until_active(&router, case, &function).await;
        let described_rule =
            events(&router, case, "DescribeRule", &json!({ "Name": rule_name })).await;
        assert_eq!(
            described_rule["Arn"].as_str(),
            Some(rule_arn.as_str()),
            "case {case:?}: expected DescribeRule to report the ARN the stack published \
             ({rule_arn}), got {described_rule}"
        );

        // Phase one: no statement exists, so the matched handover cannot be made and the
        // handler never runs.
        let before = format!("{}-before", row.stack);
        publish(&router, case, &before).await;
        let leaked = stayed_quiet_about(&router, &queue_url, Some(&before)).await;
        assert!(
            leaked.is_empty(),
            "case {case:?}: expected the event published before any statement to run nothing, \
             got {leaked:?}"
        );

        // Phase two: the row's statement is added to the DEPLOYED function, with nothing about
        // the rule or its target touched.
        let source_arn = match row.scope {
            Scope::ItsOwnRule => Some(rule_arn.clone()),
            Scope::Unscoped => None,
            Scope::AnotherRule => Some(format!(
                "arn:aws:events:{REGION}:{ACCOUNT}:rule/some-other-rule"
            )),
        };
        add_permission(
            &router,
            case,
            &function,
            STATEMENT_ID,
            source_arn.as_deref(),
        )
        .await;

        let after = format!("{}-after", row.stack);
        publish(&router, case, &after).await;
        let admitted_delivery = if row.admitted {
            forwarded_envelope(&router, case, &queue_url, &after).await
        } else {
            let withheld = stayed_quiet_about(&router, &queue_url, Some(&after)).await;
            assert!(
                withheld.is_empty(),
                "case {case:?}: expected a statement scoped to another rule to admit nothing, \
                 got {withheld:?}"
            );
            Vec::new()
        };

        // Phase three: every statement is withdrawn, so the delivery after that is decided
        // against the document the function carries NOW and runs nothing.
        remove_permission(&router, case, &function, STATEMENT_ID).await;
        let withdrawn = format!("{}-withdrawn", row.stack);
        publish(&router, case, &withdrawn).await;
        let after_removal = stayed_quiet_about(&router, &queue_url, Some(&withdrawn)).await;
        assert!(
            after_removal.is_empty(),
            "case {case:?}: expected the withdrawn statement to stop admitting the delivery \
             after it, got {after_removal:?}"
        );

        // The row's closing control, and what makes its three negatives mean something: the
        // invocation is granted unconditionally and one more matching event is published, and
        // THAT one has to come out of the handler. The rule, the target and the handler are
        // the ones every phase above used, so an event that did not arrive was refused rather
        // than merely slower than the looks above.
        add_permission(&router, case, &function, ANCHOR_STATEMENT_ID, None).await;
        let anchor = format!("{}-anchor", row.stack);
        publish(&router, case, &anchor).await;
        let delivered = forwarded_envelope(&router, case, &queue_url, &anchor).await;

        // And none of the three refused events is among anything the queue ever held, read
        // across every look this row made.
        let everything: Vec<&String> = admitted_delivery.iter().chain(delivered.iter()).collect();
        for refused in [&before, &withdrawn] {
            assert!(
                !everything.iter().any(|body| body.contains(refused)),
                "case {case:?}: expected {refused:?} never to be delivered, but the queue held \
                 it: {everything:?}"
            );
        }
        if !row.admitted {
            assert!(
                !everything.iter().any(|body| body.contains(&after)),
                "case {case:?}: expected {after:?} never to be delivered, but the queue held \
                 it: {everything:?}"
            );
        }
    }
}
