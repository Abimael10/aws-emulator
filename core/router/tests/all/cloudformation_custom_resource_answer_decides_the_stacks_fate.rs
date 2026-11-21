// SPDX-License-Identifier: CC0-1.0

//! ONE template, deployed twice: what decides the terminal status of a stack holding a
//! Lambda-backed custom resource — and the fate of the resources beside it — is the `Status`
//! its PROVIDER answers with, nothing else.
//!
//! RULE under test: **a custom resource whose `ServiceToken` names a function is provisioned
//! by invoking that function with the request and holding the resource until the function PUTs
//! its answer to the request's own `ResponseURL`; an answer of `SUCCESS` makes the resource
//! `CREATE_COMPLETE` under the `PhysicalResourceId` it reported, with the `Data` it reported as
//! what `Fn::GetAtt` on the resource resolves against, so the stack settles `CREATE_COMPLETE`
//! with its outputs published and every resource it created still held by the service that owns
//! it — while an answer of `FAILED` fails that resource and rolls the operation back, leaving
//! the stack `ROLLBACK_COMPLETE`, no outputs, and the resources beside it removed from their
//! owning service.**
//!
//! So the two answers are DISTINGUISHABLE: the same `TemplateBody`, deployed with only the
//! parameter that tells the provider which answer to send changed, reaches two different
//! terminal statuses and leaves two different sets of resources behind. A stack that fails
//! whichever answer its provider sent is an emulator a client cannot read a provider's health
//! off at all.
//!
//! Three crates have to agree for that to hold — CloudFormation sends the request and holds the
//! resource, AWS Lambda runs the provider the stack itself created from inline code, and Amazon
//! SNS owns the sibling resource whose survival is read back through its OWN API — so the suite
//! lives here. Every observation is a client's: `DescribeStacks`, `DescribeStackEvents` and
//! `sns:GetTopicAttributes`.
//!
//! The rows differ along what the rule generalises over: how the provider sends its answer (the
//! response module CloudFormation supplies to inline `ZipFile` code, and the Python standard
//! library's own HTTP client), the Python runtime the provider is declared for, the text the
//! answer's `Data` carries, and the `PhysicalResourceId` it reports. The parameter that selects
//! the answer is the only thing that differs between the two tests.
//!
//! Nothing here sleeps on a service's behalf: every wait polls for the CONDITION under a
//! generous deadline, and nothing asserts on how long a transition took.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_failed_custom_resource_rollback_deletes::resource_events;
use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries, which is what the sibling topic's own
/// ARN is read back by once the stack that made it is gone.
const ACCOUNT: &str = "000000000000";

/// The terminal statuses the two tests wait for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The resource status a custom resource whose provider answered `FAILED` takes.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// How long a status the clock owes is waited for, and how often it is looked at. The deadline
/// is far longer than the spans this emulator gives these transitions, so a loaded host cannot
/// decide a row.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(100);

/// The logical ids every template below declares its resources under.
const CUSTOM: &str = "MyResource";
const TOPIC: &str = "AlwaysTopic";

/// The template parameter that tells the provider which answer to send, and the two values it
/// is deployed with — the ONE difference between the two tests.
const SWITCH: &str = "ForceFail";
const ANSWER_FAILED: &str = "true";
const ANSWER_SUCCESS: &str = "false";

/// The provider's answer through the response module CloudFormation supplies to inline code —
/// the documented idiom, importing the runtime's own bundled SDK beside it as a provider that
/// calls other services does.
const ANSWERS_WITH_THE_RESPONSE_MODULE: &str = r#"
import json
import boto3
import cfnresponse


def handler(event, context):
    properties = event.get("ResourceProperties", {})
    physical = properties["AnswerId"]
    if event["RequestType"] == "Delete":
        cfnresponse.send(event, context, cfnresponse.SUCCESS, {}, physical)
        return
    if properties.get("ForceFail") == "true":
        cfnresponse.send(
            event,
            context,
            cfnresponse.FAILED,
            {},
            physical,
            reason=properties["FailureReason"],
        )
        return
    cfnresponse.send(
        event,
        context,
        cfnresponse.SUCCESS,
        {"RequestType": event["RequestType"], "NewValue": properties.get("Value", "")},
        physical,
    )
"#;

/// The same provider, sending the response document with the standard library's own HTTP
/// client: what decides the resource is the answer that reaches the `ResponseURL`, not which
/// client carried it.
const ANSWERS_WITH_URLLIB: &str = r#"
import json
import boto3
import urllib.request


def answer(event, status, data, physical, reason):
    document = json.dumps(
        {
            "Status": status,
            "Reason": reason,
            "PhysicalResourceId": physical,
            "StackId": event["StackId"],
            "RequestId": event["RequestId"],
            "LogicalResourceId": event["LogicalResourceId"],
            "Data": data,
        }
    ).encode("utf-8")
    request = urllib.request.Request(
        event["ResponseURL"],
        data=document,
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()


def handler(event, context):
    properties = event.get("ResourceProperties", {})
    physical = properties["AnswerId"]
    if event["RequestType"] == "Delete":
        answer(event, "SUCCESS", {}, physical, "deleted")
        return
    if properties.get("ForceFail") == "true":
        answer(event, "FAILED", {}, physical, properties["FailureReason"])
        return
    answer(
        event,
        "SUCCESS",
        {"RequestType": event["RequestType"], "NewValue": properties.get("Value", "")},
        physical,
        "created",
    )
"#;

/// One row: the provider's source and the runtime it is declared for, the names its stack's
/// resources are created under, and what its answer carries.
struct Row {
    case: &'static str,
    stack: &'static str,
    source: &'static str,
    runtime: &'static str,
    /// The `PhysicalResourceId` the provider answers with, which is what `Ref` on the custom
    /// resource must then resolve to.
    answer_id: &'static str,
    /// The text the declaration hands the provider, which comes back as one `Data` entry.
    value: &'static str,
    /// The `Reason` the provider states when the switch tells it to fail.
    reason: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a provider answering through the supplied response module",
        stack: "cr-answer-module",
        source: ANSWERS_WITH_THE_RESPONSE_MODULE,
        runtime: "python3.12",
        answer_id: "answered-by-the-module",
        value: "v1",
        reason: "the provider was told to fail this create",
    },
    // The same rule where the provider carries the document itself, on another Python release
    // line, with text an XML envelope and a form body both have to convey unharmed.
    Row {
        case: "a provider answering with the standard library's HTTP client",
        stack: "cr-answer-urllib",
        source: ANSWERS_WITH_URLLIB,
        runtime: "python3.11",
        answer_id: "answered/by-the-client-2",
        value: "a value, with spaces & a ✓",
        reason: "Error: the provider refused (ForceFail). See the log stream.",
    },
];

impl Row {
    /// The name the sibling topic is declared under, so the resource the rollback removes can be
    /// asked for by ARN after the stack that made it is gone.
    fn topic_name(&self) -> String {
        format!("{}-topic", self.stack)
    }

    /// The ARN Amazon SNS holds that topic under.
    fn topic_arn(&self) -> String {
        format!("arn:aws:sns:{REGION}:{ACCOUNT}:{}", self.topic_name())
    }
}

/// The ONE template both tests deploy: the execution role the provider assumes, the provider
/// itself from inline code, the custom resource that provider backs with the switch handed to it
/// as a property, an independent topic beside it, and outputs publishing what the provider
/// answered and what the topic is.
fn template(row: &Row) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Parameters": {
            SWITCH: {
                "Type": "String",
                "Default": ANSWER_SUCCESS,
                "AllowedValues": [ANSWER_SUCCESS, ANSWER_FAILED],
            },
        },
        "Resources": {
            "ProviderRole": {
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
                    "ManagedPolicyArns": [
                        "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole",
                    ],
                },
            },
            "ProviderFunction": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": format!("{}-provider", row.stack),
                    "Code": {"ZipFile": row.source},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["ProviderRole", "Arn"]},
                    "Runtime": row.runtime,
                    "Timeout": 30,
                },
            },
            CUSTOM: {
                "Type": "Custom::Sim2RealTest",
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": ["ProviderFunction", "Arn"]},
                    "AnswerId": row.answer_id,
                    "Value": row.value,
                    "FailureReason": row.reason,
                    SWITCH: {"Ref": SWITCH},
                },
            },
            TOPIC: {
                "Type": "AWS::SNS::Topic",
                "Properties": {"TopicName": row.topic_name()},
            },
        },
        "Outputs": {
            "RequestType": {"Value": {"Fn::GetAtt": [CUSTOM, "RequestType"]}},
            "NewValue": {"Value": {"Fn::GetAtt": [CUSTOM, "NewValue"]}},
            "ResourceRef": {"Value": {"Ref": CUSTOM}},
            "TopicArn": {"Value": {"Ref": TOPIC}},
        },
    })
    .to_string()
}

/// The emulator as it is ASSEMBLED, on one engine: the provider CloudFormation invokes is the
/// function the stack itself created, and the topic beside it is Amazon SNS's own.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: every call carries a credential scope so the
    // router can name the target service, and the signature itself is not under test.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call, as the status it answered and its body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
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
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// `GetTopicAttributes` for one topic ARN, as the status Amazon SNS answered and its body: this
/// is where the fate of the resource BESIDE the custom resource is read, in the service that
/// owns it.
async fn topic_attributes(router: &MultiServiceRouter, topic_arn: &str) -> (u16, String) {
    query(
        router,
        "sns",
        "2010-03-31",
        "GetTopicAttributes",
        &[("TopicArn", topic_arn)],
    )
    .await
}

/// Deploy `row`'s template with the switch set to `switch`.
async fn deploy(router: &MultiServiceRouter, row: &Row, switch: &str) {
    let (status, accepted) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", &template(row)),
            ("Parameters.member.1.ParameterKey", SWITCH),
            ("Parameters.member.1.ParameterValue", switch),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: expected CreateStack({}={switch}) to be accepted, got {status} with \
         {accepted}",
        row.case, SWITCH
    );
}

/// Poll `stack` until it reports a status no operation is still in flight in, and answer that
/// status with the `DescribeStacks` answer it was read from.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (_, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return (reported, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {stack:?} to settle, last reported {reported:?} with {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test]
async fn a_provider_answering_success_completes_the_stack_and_keeps_the_resources_beside_it() {
    for row in ROWS {
        let case = row.case;
        let router = emulator();
        deploy(&router, row, ANSWER_SUCCESS).await;

        let (reported, described) = settled(&router, case, row.stack).await;
        if reported != CREATE_COMPLETE {
            let events = resource_events(&router, row.stack, CUSTOM).await;
            panic!(
                "case {case:?}: expected the stack to settle at {CREATE_COMPLETE} on its \
                 provider's {ANSWER_SUCCESS:?} answer, got {reported:?}; the custom resource \
                 recorded {events:?}; last answer {described}"
            );
        }

        // The provider's answer IS the resource: the id it reported is what `Ref` resolves to and
        // the `Data` it published is what `Fn::GetAtt` resolves against — including the request
        // type it was sent, which only a run of the provider could have reported.
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get("ResourceRef").map(String::as_str),
            Some(row.answer_id),
            "case {case:?}: expected Ref on {CUSTOM} to resolve to the answered \
             PhysicalResourceId, got {outputs:?}"
        );
        assert_eq!(
            outputs.get("RequestType").map(String::as_str),
            Some("Create"),
            "case {case:?}: expected Fn::GetAtt to resolve to the request type the provider was \
             sent, got {outputs:?}"
        );
        assert_eq!(
            outputs.get("NewValue").map(String::as_str),
            Some(row.value),
            "case {case:?}: expected Fn::GetAtt to resolve to the answered Data entry, got \
             {outputs:?}"
        );

        // ...and the resource beside it is still held by the service that owns it, at the ARN the
        // stack published.
        let published = outputs.get("TopicArn").cloned().unwrap_or_default();
        assert_eq!(
            published,
            row.topic_arn(),
            "case {case:?}: expected the stack to publish the sibling topic's ARN, got \
             {outputs:?}"
        );
        let (status, attributes) = topic_attributes(&router, &published).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Amazon SNS to still hold {published:?} after the stack \
             completed, got {status} with {attributes}"
        );
        assert!(
            attributes.contains(&published),
            "case {case:?}: expected the reported attributes of {published:?} to name it, got \
             {attributes}"
        );
    }
}

/// The error code and the message Amazon SNS refuses a topic it does not hold with, which is
/// what the rolled-back sibling answers once it is gone.
const NOT_FOUND: &str = "NotFound";
const TOPIC_DOES_NOT_EXIST: &str = "Topic does not exist";

#[tokio::test]
async fn the_same_template_whose_provider_answers_failed_rolls_back_and_removes_them() {
    for row in ROWS {
        let case = row.case;
        let router = emulator();
        deploy(&router, row, ANSWER_FAILED).await;

        let (reported, described) = settled(&router, case, row.stack).await;
        assert_eq!(
            reported, ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to settle at {ROLLBACK_COMPLETE} on its \
             provider's {ANSWER_FAILED:?} answer, got {reported:?} with {described}"
        );
        // A rolled-back stack published nothing: the outputs are what a completed operation
        // leaves, and this operation did not complete.
        let outputs = outputs_of(&described);
        assert!(
            outputs.is_empty(),
            "case {case:?}: expected a rolled-back stack to publish no outputs, got {outputs:?}"
        );

        // What failed is the custom resource, with the provider's own reason on its event.
        let events = resource_events(&router, row.stack, CUSTOM).await;
        assert!(
            events
                .iter()
                .any(|(status, reason)| status == CREATE_FAILED && reason.contains(row.reason)),
            "case {case:?}: expected a {CREATE_FAILED} event for {CUSTOM} carrying the \
             provider's own reason {:?}, got {events:?}",
            row.reason
        );
        // The resource beside it WAS created — the rollback is what removed it...
        let sibling = resource_events(&router, row.stack, TOPIC).await;
        assert!(
            sibling.iter().any(|(status, _)| status == CREATE_COMPLETE),
            "case {case:?}: expected the sibling {TOPIC} to have been created before the \
             rollback, got {sibling:?}"
        );
        // ...so the service that owns it no longer holds it, and says so in its own words.
        let topic_arn = row.topic_arn();
        let (status, refused) = topic_attributes(&router, &topic_arn).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected Amazon SNS to refuse {topic_arn:?} after the rollback \
             removed it, got {status} with {refused}"
        );
        assert!(
            refused.contains(NOT_FOUND) && refused.contains(TOPIC_DOES_NOT_EXIST),
            "case {case:?}: expected the refusal of {topic_arn:?} to be {NOT_FOUND} / \
             {TOPIC_DOES_NOT_EXIST:?}, got {refused}"
        );
    }
}
