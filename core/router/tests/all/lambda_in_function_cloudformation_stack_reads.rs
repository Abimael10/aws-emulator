// SPDX-License-Identifier: CC0-1.0

//! A RUNNING function reading the CloudFormation stack it was deployed by, through the whole
//! assembled router: the AWS SDK the managed Python runtime ships, the calls it makes to
//! CloudFormation, and the IAM decision taken on each of them.
//!
//! > **RULE: the AWS SDK a managed runtime ships reaches a service whose model declares
//! > `awsQuery` exactly as it reaches one bound by a JSON protocol — the runtime builds, signs
//! > and dispatches the request that service's own protocol describes into this emulator, and
//! > what the handler reads back is that service's own answer. Each such call is signed as the
//! > session of the function's execution role and decided by IAM before the service acts, as
//! > the action `<the service's namespace>:<the operation's own name>` on the resource the
//! > request names; a call no readable document allows, and one a readable document denies, is
//! > refused with the code that service publishes for an unauthorized call, at HTTP 403, and
//! > performs nothing.**
//!
//! AWS CloudFormation is one of those services: its operations travel as the `Action` of a
//! form-encoded body and answer an XML document, so a handler's `boto3.client("cloudformation")`
//! is served by the client the Lambda crate holds for that protocol rather than by the JSON
//! path most reachable services share. What the rule generalises over is the OPERATION: the
//! stack's resources, its template, its own record, the events it progressed through, the
//! stacks the account holds and a template validated outside any stack are each one call, and
//! each is judged as its own action.
//!
//! Reading its own stack is the shape most functions that reach this service take: a handler
//! declared in a template is handed its stack's name and asks CloudFormation what that stack
//! holds — which is how it finds the queue, the table or the topic its own stack created
//! without being told their names.
//!
//! Four crates have to agree for this to hold, which is why this is a router suite: IAM holds
//! the execution role and the documents in force on it, the token service mints the session the
//! handler's calls are signed as, AWS Lambda runs the handler and makes the call on its behalf,
//! and CloudFormation serves it and takes the decision. Everything is driven the way a client
//! reaches it — `iam:CreateRole`, `iam:AttachRolePolicy`, `iam:PutRolePolicy`,
//! `cloudformation:CreateStack`, then `lambda:Invoke` — so what the handler reads is what a
//! client reading the same stack reads, and the principal a refusal blames is the session the
//! function really runs as.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element, outputs_of};

/// The Region every request below is signed for, and so the Region every ARN below names.
const REGION: &str = "us-east-1";

/// An access key id neither the token service nor IAM holds a record for: the position the
/// account's own published credential is in, which is what the setup calls are signed with.
const ACCOUNT_KEY_ID: &str = "AKIDEXAMPLE";

/// The awsQuery API versions the services below are called at.
const IAM_VERSION: &str = "2010-05-08";
const CFN_VERSION: &str = "2010-05-15";

/// The wire error code CloudFormation publishes for an unauthorized call, and the status it
/// answers one with. The decision is IAM's, taken ahead of the service, so the code is in no
/// operation's modelled error set.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The AWS-managed policy a Lambda execution role is created with: it grants the log writes a
/// function makes and nothing else, so a role carrying only it has no grant on CloudFormation.
const BASIC_EXECUTION: &str = "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The principal an execution role has to trust for Lambda to run a function as it.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// Where Lambda binds the collection a function is read and invoked under.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime the probe function carries, how long one invocation is given, and the `State` a
/// function reports before it may be invoked. The timeout is well past what a run needs on any
/// host: what is under test is what the call answered, not how fast an interpreter gets through
/// the handler.
const RUNTIME: &str = "python3.12";
const FUNCTION_TIMEOUT: u64 = 120;
const ACTIVE: &str = "Active";

/// The `StackStatus` the function's stack has to reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long an observation of work the CLOCK owes — a stack completing, a function going
/// `Active` — waits before calling it a hang, and how long between two looks. Generous, so a
/// loaded host delays the setup instead of deciding the outcome; each wait is on its own
/// CONDITION rather than on a span.
const DEADLINE: Duration = Duration::from_secs(180);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The status Lambda answers an invocation that found no run capacity with: "Lambda returns
/// `TooManyRequestsException` if running the function would cause you to exceed a concurrency
/// limit" (`smithy_query(query="operation", operation="Invoke")`).
const THROTTLED: u16 = 429;

/// One answered request.
struct Answered {
    status: u16,
    headers: http::HeaderMap,
    text: String,
}

impl Answered {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or(Value::Null)
    }
}

/// A SigV4 `Authorization` whose credential names `access_key_id` and whose scope names
/// `signing_name` and [`REGION`], which is what routes the request and what Region the
/// decision is taken in. The signature itself is never checked here.
fn authorization(access_key_id: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Answered {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let response = router
        .dispatch(
            http::Request::builder()
                .method(method)
                .uri(format!("https://{host}{path}"))
                .header("host", &host)
                .header("authorization", authorization(ACCOUNT_KEY_ID, signing_name))
                .header("content-type", content_type)
                .body(bytes::Bytes::from(body))
                .expect("the test request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    Answered {
        status,
        headers,
        text: String::from_utf8_lossy(response.body().as_ref()).to_string(),
    }
}

/// One awsQuery call, signed with the account's own published credential, answered with its
/// status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut form = format!("Action={action}&Version={version}");
    for (name, value) in parameters {
        form.push('&');
        form.push_str(&form_encode(name));
        form.push('=');
        form.push_str(&form_encode(value));
    }
    let answered = dispatch(
        router,
        signing_name,
        "POST",
        "/",
        "application/x-www-form-urlencoded",
        form.into_bytes(),
    )
    .await;
    (answered.status, answered.text)
}

/// One awsQuery call that must be served, answered with its XML body.
async fn served_query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, signing_name, version, action, parameters).await;
    assert_eq!(
        status, 200,
        "setup: {signing_name} {action} must answer 200, got {status} with {body}"
    );
    body
}

/// The sentence IAM refuses an unauthorized call with: who was refused, which action, on which
/// resource, and which of IAM's rules refused it.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// The clause closing a refusal no readable document allowed — IAM's default deny.
fn no_grant(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The clause closing a refusal a readable document DENIED.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The IAM action a CloudFormation call performs: this service publishes one action per API
/// operation, named by that operation.
fn action_for(operation: &str) -> String {
    format!("cloudformation:{operation}")
}

/// An identity-based policy with one `Allow` of `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
}

/// An identity-based policy allowing EVERYTHING and denying `action` on every resource: the
/// explicit deny no grant lifts, which a single-statement document cannot express.
fn allow_everything_but(action: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": "*", "Resource": "*" },
            { "Effect": "Deny", "Action": action, "Resource": "*" },
        ],
    })
}

/// The trust policy of a role one AWS service assumes.
fn service_trust_policy(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The one name an inline document is put on a role under.
const INLINE_POLICY: &str = "grants";

/// Hold one role named `role_name` trusting Lambda, with the AWS-managed execution-role policy
/// attached to it, through IAM's own calls. Answers the role's ARN.
async fn execution_role(router: &MultiServiceRouter, role_name: &str) -> String {
    let created = served_query(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", role_name),
            (
                "AssumeRolePolicyDocument",
                &service_trust_policy(LAMBDA_PRINCIPAL),
            ),
        ],
    )
    .await;
    served_query(
        router,
        "iam",
        IAM_VERSION,
        "AttachRolePolicy",
        &[("RoleName", role_name), ("PolicyArn", BASIC_EXECUTION)],
    )
    .await;
    element(&created, "Arn")
}

/// Put `document` on `role_name` as its one inline policy — what a client grants an existing
/// role a missing permission with.
async fn put_role_policy(router: &MultiServiceRouter, role_name: &str, document: &Value) {
    served_query(
        router,
        "iam",
        IAM_VERSION,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &document.to_string()),
        ],
    )
    .await;
}

/// The handler the probe function runs: `import boto3` and the CloudFormation client at MODULE
/// scope — so a runtime shipping no SDK fails initialization rather than this call — and then
/// the ONE call the event names, reporting either what CloudFormation answered or the refusal it
/// answered with. The invocation's payload IS what the SDK gave the guest.
const HANDLER_SOURCE: &str = r#"
import json

import boto3
import botocore.exceptions

CFN = boto3.client("cloudformation")


def _template(read):
    """The template body as text, whichever form the SDK handed it over in."""
    body = read["TemplateBody"]
    if isinstance(body, str):
        return body
    return json.dumps(body)


def handler(event, context):
    call = event["call"]
    stack = event.get("stack")
    try:
        if call == "list_stack_resources":
            listed = CFN.list_stack_resources(StackName=stack)
            summaries = listed.get("StackResourceSummaries", [])
            return {
                "ok": True,
                "logical_ids": sorted([item["LogicalResourceId"] for item in summaries]),
                "physical_ids": {
                    item["LogicalResourceId"]: item.get("PhysicalResourceId")
                    for item in summaries
                },
                "statuses": sorted(set([item["ResourceStatus"] for item in summaries])),
            }
        if call == "get_template":
            return {"ok": True, "template": _template(CFN.get_template(StackName=stack))}
        if call == "describe_stacks":
            described = CFN.describe_stacks(StackName=stack)
            held = described["Stacks"][0]
            return {
                "ok": True,
                "stack_id": held["StackId"],
                "stack_name": held["StackName"],
                "status": held["StackStatus"],
                "outputs": {
                    output["OutputKey"]: output["OutputValue"]
                    for output in held.get("Outputs", [])
                },
            }
        if call == "describe_stack_events":
            reported = CFN.describe_stack_events(StackName=stack).get("StackEvents", [])
            return {
                "ok": True,
                "logical_ids": sorted(set([item["LogicalResourceId"] for item in reported])),
                "statuses": sorted(set([item["ResourceStatus"] for item in reported])),
            }
        if call == "list_stacks":
            listed = CFN.list_stacks().get("StackSummaries", [])
            return {"ok": True, "names": sorted([item["StackName"] for item in listed])}
        if call == "validate_template":
            validated = CFN.validate_template(TemplateBody=event["template"])
            return {
                "ok": True,
                "description": validated.get("Description"),
                "parameters": sorted(
                    [item["ParameterKey"] for item in validated.get("Parameters", [])]
                ),
            }
        return {"ok": False, "error_code": "NoSuchCall"}
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "error_code": error.response["Error"]["Code"],
            "message": error.response["Error"].get("Message"),
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
"#;

/// The stack's own description, the parameter it declares and the topic that parameter names:
/// read back through `ValidateTemplate` and through the stack's outputs.
const STACK_DESCRIPTION: &str = "The probe function and the topic it reports on";
const TOPIC_PARAMETER: &str = "TopicLabel";

/// The template the probe function is deployed by: the function itself, a second resource
/// beside it so a resource listing carries more than one entry, a declared parameter and an
/// output. The inline source is archived into the deployment package by CloudFormation, which
/// is how a function whose handler is written here comes to exist at all.
fn function_stack(role_arn: &str, topic: &str, function: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": STACK_DESCRIPTION,
        "Parameters": {
            TOPIC_PARAMETER: { "Type": "String", "Default": topic },
        },
        "Resources": {
            "Notices": {
                "Type": "AWS::SNS::Topic",
                "Properties": { "TopicName": { "Ref": TOPIC_PARAMETER } },
            },
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": RUNTIME,
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
        "Outputs": {
            "TopicArn": { "Value": { "Ref": "Notices" } },
        },
    })
    .to_string()
}

/// The whole emulator over one engine, exactly as the binary assembles it.
fn emulator() -> MultiServiceRouter {
    let mut router = ::aws_emulator_router::build_router(::aws_emulator_router::new_engine());
    // Signature verification belongs to the emulator's front door, not to this suite: what is
    // under test is what a call answered and how a credential was AUTHORIZED. A handler's own
    // calls are signed by the runtime with the credential it was given.
    router.set_request_authentication(false);
    router
}

/// The session a call the running function makes is signed as: "the execution role grants the
/// function permission to use Amazon Web Services services"
/// (`FunctionConfiguration$Role`), and the session a managed runtime opens for a run is named
/// after the function.
fn execution_session(role: &str, function: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role}/{function}",
        ::engine::emulator_account_id()
    )
}

/// Look again until the stack `stack` reports [`CREATE_COMPLETE`], and answer the last status it
/// reported. Waits on that CONDITION rather than on a span.
async fn wait_for_stack(router: &MultiServiceRouter, stack: &str) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let body = served_query(
            router,
            "cloudformation",
            CFN_VERSION,
            "DescribeStacks",
            &[("StackName", stack)],
        )
        .await;
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == CREATE_COMPLETE || Instant::now() >= giving_up_at {
            return reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Look again until `function` reports [`ACTIVE`], which is the state it cannot be invoked
/// before. Waits on that CONDITION rather than on a span.
async fn wait_for_active(router: &MultiServiceRouter, function: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < giving_up_at {
        let answered = dispatch(
            router,
            "lambda",
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            "application/json",
            Vec::new(),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "setup: GetFunctionConfiguration({function}) must be served, got {} {}",
            answered.status, answered.text
        );
        let configuration = answered.json();
        if configuration["State"] == json!(ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("setup: {function} never reported {ACTIVE}, last {last}");
}

/// Invoke the probe function with `event` and answer what the handler reported.
///
/// A host running many functions at once has only so many run slots, and an invocation that
/// finds none is answered the service's own capacity refusal ([`THROTTLED`]) — which is not what
/// is under test here, so this looks again until one is not throttled, as a client retries a
/// throttled invocation. The wait is on that CONDITION, bounded by [`DEADLINE`], so a loaded
/// host delays this rather than deciding it; every other answer is asserted at once.
async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &Value,
) -> Value {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut answered = invoke_once(router, function, event).await;
    while answered.status == THROTTLED && Instant::now() < giving_up_at {
        tokio::time::sleep(LOOK_AGAIN).await;
        answered = invoke_once(router, function, event).await;
    }
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200 from Invoke({function}), got {} {}",
        answered.status, answered.text
    );
    let signal = answered
        .headers
        .get("x-amz-function-error")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).to_string());
    assert_eq!(
        signal, None,
        "case {case:?}: expected no function error, got {signal:?} with {}",
        answered.text
    );
    answered.json()
}

/// One invocation of the probe function with `event`, as it came back.
async fn invoke_once(router: &MultiServiceRouter, function: &str, event: &Value) -> Answered {
    dispatch(
        router,
        "lambda",
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        "application/json",
        event.to_string().into_bytes(),
    )
    .await
}

/// The strings a reported list member carries, in the order the handler sorted them.
fn reported_list(reported: &Value, member: &str) -> Vec<String> {
    reported[member]
        .as_array()
        .map(|held| {
            held.iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A reported map member, as the map a client compares against the service's own answer.
fn reported_map(reported: &Value, member: &str) -> BTreeMap<String, String> {
    reported[member]
        .as_object()
        .map(|held| {
            held.iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// RULE, for the ANSWER: each call the handler makes through the SDK the runtime ships is
/// served by CloudFormation itself, so what the handler reads back is the very record a client
/// reading the same stack reads.
///
/// One row per operation — the stack's resources, its template, its own record, its events, the
/// account's stacks, and a template validated outside any stack — each asserted against what
/// CloudFormation answers the same question over the wire.
#[tokio::test]
async fn a_handlers_sdk_calls_read_the_stack_cloudformation_holds() {
    const ROLE: &str = "cfn-reads-role";
    const FUNCTION: &str = "cfn-reads-probe";
    const STACK: &str = "cfn-reads-stack";
    const TOPIC: &str = "cfn-reads-notices";

    let router = emulator();
    let role_arn = execution_role(&router, ROLE).await;
    // The grant under test here is not the subject: the role may make every CloudFormation
    // call, so what each row asserts is the ANSWER.
    put_role_policy(&router, ROLE, &allow("cloudformation:*", "*")).await;
    let template = function_stack(&role_arn, TOPIC, FUNCTION);
    served_query(
        &router,
        "cloudformation",
        CFN_VERSION,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let reached = wait_for_stack(&router, STACK).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {reached}"
    );
    wait_for_active(&router, FUNCTION).await;

    // The stack as CloudFormation answers a client about it, which is what every row below is
    // compared against.
    let described = served_query(
        &router,
        "cloudformation",
        CFN_VERSION,
        "DescribeStacks",
        &[("StackName", STACK)],
    )
    .await;
    let stack_id = element(&described, "StackId");
    let listed = served_query(
        &router,
        "cloudformation",
        CFN_VERSION,
        "ListStackResources",
        &[("StackName", STACK)],
    )
    .await;
    let mut held_resources = elements(&listed, "LogicalResourceId");
    held_resources.sort();
    let read = served_query(
        &router,
        "cloudformation",
        CFN_VERSION,
        "GetTemplate",
        &[("StackName", STACK)],
    )
    .await;
    let held_template = elements(&read, "TemplateBody")
        .into_iter()
        .next()
        .unwrap_or_default();

    // The stack's RESOURCES, as the handler read them.
    let case = "the stack's resources";
    let reported = invoke(
        &router,
        case,
        FUNCTION,
        &json!({ "call": "list_stack_resources", "stack": STACK }),
    )
    .await;
    assert_eq!(
        reported["ok"],
        json!(true),
        "case {case:?}: expected the call to be served, got {reported}"
    );
    assert_eq!(
        reported_list(&reported, "logical_ids"),
        held_resources,
        "case {case:?}: expected the resources CloudFormation reports ({held_resources:?}), \
         got {reported}"
    );
    assert_eq!(
        reported_list(&reported, "statuses"),
        vec![CREATE_COMPLETE.to_string()],
        "case {case:?}: expected every resource to report {CREATE_COMPLETE}, got {reported}"
    );
    let physical = reported_map(&reported, "physical_ids");
    assert!(
        physical
            .get("Notices")
            .is_some_and(|arn| arn.starts_with("arn:aws:sns:") && arn.ends_with(TOPIC)),
        "case {case:?}: expected the topic's own ARN as its physical id, got {physical:?}"
    );

    // The stack's TEMPLATE.
    let case = "the stack's template";
    let reported = invoke(
        &router,
        case,
        FUNCTION,
        &json!({ "call": "get_template", "stack": STACK }),
    )
    .await;
    assert_eq!(
        reported["ok"],
        json!(true),
        "case {case:?}: expected the call to be served, got {reported}"
    );
    let carried = reported["template"].as_str().unwrap_or_default();
    let parsed: Value = serde_json::from_str(carried).unwrap_or(Value::Null);
    let submitted: Value = serde_json::from_str(&template).expect("the template is a document");
    assert_eq!(
        parsed["Resources"], submitted["Resources"],
        "case {case:?}: expected the template the stack was deployed from, got {carried}"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&held_template).unwrap_or(Value::Null),
        parsed,
        "case {case:?}: expected the body CloudFormation answers a client, got {carried}"
    );

    // The stack's own RECORD, with the outputs it reports.
    let case = "the stack's own record";
    let reported = invoke(
        &router,
        case,
        FUNCTION,
        &json!({ "call": "describe_stacks", "stack": STACK }),
    )
    .await;
    assert_eq!(
        (
            reported["ok"].clone(),
            reported["stack_id"].clone(),
            reported["stack_name"].clone(),
            reported["status"].clone(),
        ),
        (
            json!(true),
            json!(stack_id),
            json!(STACK),
            json!(CREATE_COMPLETE),
        ),
        "case {case:?}: expected the stack CloudFormation reports ({stack_id}), got {reported}"
    );
    let outputs = outputs_of(&described);
    assert_eq!(
        reported_map(&reported, "outputs"),
        outputs,
        "case {case:?}: expected the outputs CloudFormation reports ({outputs:?}), got \
         {reported}"
    );

    // The EVENTS the stack's operation progressed through.
    let case = "the stack's events";
    let reported = invoke(
        &router,
        case,
        FUNCTION,
        &json!({ "call": "describe_stack_events", "stack": STACK }),
    )
    .await;
    assert_eq!(
        reported["ok"],
        json!(true),
        "case {case:?}: expected the call to be served, got {reported}"
    );
    let logical_ids = reported_list(&reported, "logical_ids");
    for named in [STACK, "Notices", "Probe"] {
        assert!(
            logical_ids.contains(&named.to_string()),
            "case {case:?}: expected an event for {named:?}, got {logical_ids:?}"
        );
    }
    assert!(
        reported_list(&reported, "statuses").contains(&CREATE_COMPLETE.to_string()),
        "case {case:?}: expected the events to carry {CREATE_COMPLETE}, got {reported}"
    );

    // The account's STACKS.
    let case = "the account's stacks";
    let reported = invoke(&router, case, FUNCTION, &json!({ "call": "list_stacks" })).await;
    let names = reported_list(&reported, "names");
    assert_eq!(
        (reported["ok"].clone(), names.clone()),
        (json!(true), vec![STACK.to_string()]),
        "case {case:?}: expected the one stack the account holds, got {reported}"
    );

    // A TEMPLATE validated outside any stack: what the document declares, as CloudFormation
    // reads it.
    let case = "a template validated";
    let reported = invoke(
        &router,
        case,
        FUNCTION,
        &json!({ "call": "validate_template", "template": template }),
    )
    .await;
    assert_eq!(
        (reported["ok"].clone(), reported["description"].clone()),
        (json!(true), json!(STACK_DESCRIPTION)),
        "case {case:?}: expected the template's own description, got {reported}"
    );
    assert_eq!(
        reported_list(&reported, "parameters"),
        vec![TOPIC_PARAMETER.to_string()],
        "case {case:?}: expected the parameter the template declares, got {reported}"
    );
}

/// What one row of the authorization table states: the document in force on the execution role,
/// and whether each of the three calls is then served.
struct Row {
    case: &'static str,
    /// The inline document put on the role for this row, or `None` for a role carrying only the
    /// AWS-managed execution-role policy.
    inline: fn(&str) -> Option<Value>,
    /// Whether the resource listing (judged on the STACK's own ARN) is served.
    resources_served: bool,
    /// Whether the template read (judged on the same ARN, under its own action) is served.
    template_served: bool,
    /// Whether the account-wide listing (judged on every resource of the service) is served.
    listing_served: bool,
    /// Whether a refusal of this row is IAM's explicit deny rather than its default deny.
    denied: bool,
}

/// The rows: what the rule generalises over is the document in force — WHERE a grant comes
/// from, WHICH action it names, WHICH resource it names, and which EFFECT it states — asked
/// about three calls whose action and resource differ, so a grant of one action never covers
/// another and a grant on one stack never covers another.
const ROWS: &[Row] = &[
    Row {
        case: "a role carrying only the managed execution-role policy",
        inline: |_stack| None,
        resources_served: false,
        template_served: false,
        listing_served: false,
        denied: false,
    },
    Row {
        case: "a grant of every action of the service on every resource",
        inline: |_stack| Some(allow("cloudformation:*", "*")),
        resources_served: true,
        template_served: true,
        listing_served: true,
        denied: false,
    },
    Row {
        case: "a grant of the resource listing alone, on the stack's own ARN",
        inline: |stack| Some(allow("cloudformation:ListStackResources", stack)),
        resources_served: true,
        template_served: false,
        listing_served: false,
        denied: false,
    },
    Row {
        case: "a grant of every action, on another stack's ARN",
        inline: |stack| {
            Some(allow(
                "cloudformation:*",
                &stack.replace("stack/", "stack/elsewhere-"),
            ))
        },
        resources_served: false,
        template_served: false,
        listing_served: false,
        denied: false,
    },
    Row {
        case: "a grant of everything with an explicit deny of the template read",
        inline: |_stack| Some(allow_everything_but("cloudformation:GetTemplate")),
        resources_served: true,
        template_served: false,
        listing_served: true,
        denied: true,
    },
];

/// RULE, for the DECISION: each call a running handler makes to CloudFormation is decided
/// against the documents in force on its execution role at the instant of the call, as
/// `cloudformation:<the operation's own name>` on the resource the request names — the stack's
/// own ARN for a call about one stack, and every resource of the service in the account and
/// Region for a call that names none. A call no readable document allows, and one a readable
/// document denies, is refused `AccessDenied` at HTTP 403 with IAM's own sentence.
#[tokio::test]
async fn a_handlers_sdk_calls_are_decided_by_its_execution_roles_documents() {
    const ROLE: &str = "cfn-grants-role";
    const FUNCTION: &str = "cfn-grants-probe";
    const STACK: &str = "cfn-grants-stack";
    const TOPIC: &str = "cfn-grants-notices";

    let router = emulator();
    let role_arn = execution_role(&router, ROLE).await;
    let template = function_stack(&role_arn, TOPIC, FUNCTION);
    served_query(
        &router,
        "cloudformation",
        CFN_VERSION,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    let reached = wait_for_stack(&router, STACK).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {reached}"
    );
    wait_for_active(&router, FUNCTION).await;

    let principal = execution_session(ROLE, FUNCTION);
    // The resource each call is judged on: the stack's own ARN, which is its stack id, for the
    // two calls that name the stack, and every resource of this service in the account and
    // Region for the account-wide listing, which names none.
    let described = served_query(
        &router,
        "cloudformation",
        CFN_VERSION,
        "DescribeStacks",
        &[("StackName", STACK)],
    )
    .await;
    let stack_arn = element(&described, "StackId");
    let every_resource = format!(
        "arn:aws:cloudformation:{REGION}:{}:*",
        ::engine::emulator_account_id()
    );

    for row in ROWS {
        if let Some(document) = (row.inline)(&stack_arn) {
            put_role_policy(&router, ROLE, &document).await;
        }
        // (what the call is, its event, whether it is served, the resource it is judged on)
        let calls: [(&str, Value, bool, &str); 3] = [
            (
                "ListStackResources",
                json!({ "call": "list_stack_resources", "stack": STACK }),
                row.resources_served,
                &stack_arn,
            ),
            (
                "GetTemplate",
                json!({ "call": "get_template", "stack": STACK }),
                row.template_served,
                &stack_arn,
            ),
            (
                "ListStacks",
                json!({ "call": "list_stacks" }),
                row.listing_served,
                &every_resource,
            ),
        ];
        for (operation, event, served, resource) in calls {
            let case = format!("{} / {operation}", row.case);
            let reported = invoke(&router, &case, FUNCTION, &event).await;
            if served {
                assert_eq!(
                    reported["ok"],
                    json!(true),
                    "case {case:?}: expected the granted call to be served, got {reported}"
                );
                continue;
            }
            let action = action_for(operation);
            let reason = if row.denied {
                EXPLICIT_DENY.to_string()
            } else {
                no_grant(&action)
            };
            assert_eq!(
                (
                    reported["ok"].clone(),
                    reported["error_code"].clone(),
                    reported["status"].clone(),
                ),
                (
                    json!(false),
                    json!(ACCESS_DENIED),
                    json!(ACCESS_DENIED_STATUS),
                ),
                "case {case:?}: expected the call to be refused {ACCESS_DENIED_STATUS} \
                 {ACCESS_DENIED}, got {reported}"
            );
            assert_eq!(
                reported["message"],
                json!(not_authorized(&principal, &action, resource, &reason)),
                "case {case:?}: expected IAM's sentence naming {action} on {resource}, got \
                 {reported}"
            );
        }
    }
}
