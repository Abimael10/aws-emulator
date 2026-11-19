// SPDX-License-Identifier: CC0-1.0

//! A least-privilege publisher deployed as ONE CloudFormation stack: the custom event bus, the
//! rule watching it, the queue that rule delivers to, the execution roles and the functions
//! that publish all come out of the same template — and what each function's own in-function
//! publication is allowed to do is decided by the identity policies the stack put on its role.
//!
//! RULE: a call an AWS SDK makes from inside a running function is a real signed call made as a
//! session of that function's execution role, so the service serving it decides it against the
//! identity-based policies in force on that role AND THE RESOURCE THE REQUEST NAMES — however
//! that role and that resource were provisioned. A publication onto a bus some statement in
//! force allows is served (`FailedEntryCount` 0, one `EventId` per entry) and the event is then
//! matched by that bus's rules and delivered to their targets; one no statement in force allows
//! is refused `AccessDeniedException` (HTTP 400) naming the assumed-role session, the action
//! `events:PutEvents` and the ARN of the bus the entry addressed, with nothing of it published
//! and nothing delivered — and a statement scoped to ANOTHER bus of the same account grants
//! this publication nothing, because the resource a statement names is part of the decision.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`); the identity a function's own call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`; with neither an identity-based nor a
//! resource-based allow a request is denied, and an explicit `Deny` refuses whatever an `Allow`
//! says (IAM User Guide, *Policy evaluation logic*). EventBridge's `PutEvents` declares no
//! authorization error of its own (`smithy_query(query="operation", operation="PutEvents")`
//! names `InternalException` only), so the refusal is the AWS-common one, and a served
//! publication is read off the `FailedEntryCount` and the `PutEventsResultEntry` list that
//! operation's output carries.
//!
//! Seven crates have to agree for that to hold — CloudFormation evaluates the template and
//! makes every call, IAM holds the roles and their documents, the token service mints the
//! session the handler's call is signed as, Lambda runs the handler and supplies it the SDK,
//! EventBridge takes the decision and matches what it accepted, and Amazon SQS holds what the
//! rule delivered — so the suite lives here: a crate-local suite can only ever see one side of
//! it.
//!
//! What the neighbours pin and this one does not repeat: `lambda_in_function_events_authorization`
//! drives the TABLE of identity-document shapes a publication onto the account's DEFAULT bus is
//! decided by, the EventBridge crate's `handler_execution_role_publication` pins the same
//! decision for a role written through IAM's own operations, and
//! `cloudformation_provisions_event_buses_and_bus_policies` pins that a declared
//! `AWS::Events::EventBus` really exists in EventBridge. What only this suite reaches is the
//! decision about a CUSTOM bus the SAME stack declared: the refusal naming the ARN that stack
//! published for its own bus, a grant scoped to that ARN serving the publication, and a grant
//! scoped to the account's default bus leaving it refused — which is the boundary a per-bus
//! execution-role scope is written for, and which a suite that only ever publishes onto
//! `default` cannot show.
//!
//! The rows differ along what the rule generalises over: WHERE the document in force comes from
//! (nothing but the managed policy AWS owns, a document embedded in the role by the template, a
//! standalone `AWS::IAM::RolePolicy` resource), WHICH resource it names (the stack's own bus by
//! the ARN the stack published, every bus of the account by an ARN wildcard, another bus of the
//! account), WHICH action it names (the publication's own, another operation's) and which EFFECT
//! it states (an allow, an allow with an explicit deny beside it). Each row asserts both halves:
//! what the SDK gave the handler, and what the stack's own rule target holds afterwards.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, outputs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region the stack's bus, rule, queue
/// and functions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// Where `Invoke` and the configuration read are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The stack that deploys everything this suite observes.
const STACK: &str = "cfn-declared-bus-publishers";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The custom bus the stack declares, the rule it puts on it, the queue that rule targets and
/// the target's id — all named by the template, so a read here addresses the stack's own
/// resources.
const BUS: &str = "cfn-declared-bus";
const RULE: &str = "cfn-declared-bus-rule";
const SINK: &str = "cfn-declared-bus-sink";
const TARGET_ID: &str = "sink";

/// The bus a statement of one row names INSTEAD of the stack's own: the account's default bus,
/// which every account has and which no row ever publishes onto.
const OTHER_BUS: &str = "default";

/// The `source` and `detail-type` every handler publishes under, which is all the rule matches
/// on — so a message on its queue is one of these publications and nothing else.
const SOURCE: &str = "com.demo.cfn-declared-bus";
const DETAIL_TYPE: &str = "Declared Bus Publication";

/// The action a publication onto a bus performs, as a statement names it and as a refusal
/// reports it, and another operation's action, which grants nothing of it.
const PUT_EVENTS_ACTION: &str = "events:PutEvents";
const PUT_RULE_ACTION: &str = "events:PutRule";

/// The code and status EventBridge refuses a publication the policies in force do not permit
/// with (`AccessDeniedException`, `@httpError(400)` for this service).
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u64 = 400;

/// The managed policy AWS owns that a Lambda execution role is created with: it grants the log
/// writes a function makes and nothing of any other service, so a role carrying only it is
/// granted no publication.
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The service principal the sink queue's policy admits, because a delivery to a queue is made
/// by the delivering service itself and "EventBridge relies on resource-based policies"
/// (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The runtime every function is created with: a managed Python runtime, which carries the SDK
/// the handler imports.
const RUNTIME: &str = "python3.12";

/// The module and attribute the deployment package carries its handler as. CloudFormation
/// archives inline code under the handler's own file stem, so the module is `index`.
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is the authorization decision and not how fast an
/// interpreter gets through one SDK call.
const FUNCTION_TIMEOUT: u64 = 120;

/// The output keys the template declares.
const BUS_NAME_OUTPUT: &str = "BusName";
const BUS_ARN_OUTPUT: &str = "BusArn";
const SINK_URL_OUTPUT: &str = "SinkUrl";

/// How long work the CLOCK owes — a stack completing, a function becoming `Active`, an event
/// reaching the rule's target — is waited for before giving up, and how long between two looks.
/// A GENEROUS ceiling rather than an expectation: every wait here is on the CONDITION, and
/// nothing asserts how long anything took.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The handler every row runs. It imports the SDK at module scope, publishes ONE event onto the
/// bus the invocation names, and reports what the publication answered or the refusal it raised
/// — so the invocation is a 200 with no `FunctionError` whichever way the call went.
const PROBE_HANDLER: &str = r#"
import json

import boto3
from botocore.exceptions import ClientError

EVENTS = boto3.client("events")


def handler(event, context):
    try:
        answered = EVENTS.put_events(
            Entries=[
                {
                    "EventBusName": event["bus"],
                    "Source": event["source"],
                    "DetailType": event["detail_type"],
                    "Detail": json.dumps({"marker": event["marker"]}),
                }
            ]
        )
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {
        "ok": True,
        "failed_count": answered["FailedEntryCount"],
        "event_ids": [entry["EventId"] for entry in answered["Entries"]],
    }
"#;

/// WHERE the document that decides a row's publication is attached by the template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attachment {
    /// Nothing but the managed policy AWS owns that every execution role carries.
    OnlyBasicExecution,
    /// The role's own inline `Policies` property ("adds or updates an inline policy document
    /// that is embedded in the specified IAM role").
    RoleInlinePolicies,
    /// A standalone `AWS::IAM::RolePolicy` resource, which names the role it embeds into.
    RolePolicyResource,
}

/// WHAT the statements of the attached document cover. Ignored by
/// [`Attachment::OnlyBasicExecution`], which attaches no document of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Statements {
    /// Allow the publication on the stack's OWN bus, named by the ARN the stack published for
    /// it (`Fn::GetAtt` of the declared bus).
    PublishOnTheDeclaredBus,
    /// Allow the publication on every bus of this account, by an ARN wildcard the declared
    /// bus's own ARN matches.
    PublishOnEveryBusOfTheAccount,
    /// Allow the publication on ANOTHER bus of the account — the default one — and on no other.
    PublishOnAnotherBus,
    /// Allow ANOTHER operation's action on the stack's own bus.
    AnotherActionOnTheDeclaredBus,
    /// Allow the publication on every resource, with an explicit deny of it on the stack's own
    /// bus beside that allow.
    PublishAllowedAndExplicitlyDenied,
}

/// WHY a refusal was reached, which is the clause IAM's sentence ends with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// No document in force states anything about the call, so it falls to IAM's default deny.
    NoPolicyAllows,
    /// A document in force denies the call outright, whatever any allow says.
    ExplicitDeny,
}

impl Reason {
    /// The clause the refusal's sentence ends with. Written from [`PUT_EVENTS_ACTION`] rather
    /// than spelled out again, so the action a refusal names cannot drift from the one a
    /// statement grants.
    fn clause(self) -> String {
        match self {
            Self::NoPolicyAllows => format!(
                "because no identity-based policy allows the {PUT_EVENTS_ACTION} action"
            ),
            Self::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        }
    }
}

/// What the row's publication earns: the grant in force served it, or it was refused for this
/// reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Served,
    Refused(Reason),
}

/// One row: how the template attaches the document, what it says, and what the publication onto
/// the stack's own bus therefore earns.
struct Row {
    case: &'static str,
    attachment: Attachment,
    statements: Statements,
    outcome: Outcome,
}

const ROWS: &[Row] = &[
    Row {
        case: "the role carries only the managed policy AWS owns, so the publication falls to \
               IAM's default deny",
        attachment: Attachment::OnlyBasicExecution,
        statements: Statements::PublishOnTheDeclaredBus,
        outcome: Outcome::Refused(Reason::NoPolicyAllows),
    },
    Row {
        case: "the role's embedded document names the bus the stack declared, so the \
               publication is served",
        attachment: Attachment::RoleInlinePolicies,
        statements: Statements::PublishOnTheDeclaredBus,
        outcome: Outcome::Served,
    },
    Row {
        case: "a standalone role policy names every bus of the account, so the publication is \
               served",
        attachment: Attachment::RolePolicyResource,
        statements: Statements::PublishOnEveryBusOfTheAccount,
        outcome: Outcome::Served,
    },
    Row {
        case: "the embedded document names another bus of the account, so the publication onto \
               the declared one is refused",
        attachment: Attachment::RoleInlinePolicies,
        statements: Statements::PublishOnAnotherBus,
        outcome: Outcome::Refused(Reason::NoPolicyAllows),
    },
    Row {
        case: "the embedded document grants another action of the service on the declared bus, \
               so the publication is refused",
        attachment: Attachment::RoleInlinePolicies,
        statements: Statements::AnotherActionOnTheDeclaredBus,
        outcome: Outcome::Refused(Reason::NoPolicyAllows),
    },
    Row {
        case: "the embedded document allows the publication and denies it, so the explicit deny \
               wins",
        attachment: Attachment::RoleInlinePolicies,
        statements: Statements::PublishAllowedAndExplicitlyDenied,
        outcome: Outcome::Refused(Reason::ExplicitDeny),
    },
];

/// The role and the function the row at `index` is deployed as, and the marker its publication
/// carries — one of each per row, so no row can read another's effect.
fn role_name(index: usize) -> String {
    format!("cfn-declared-bus-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("cfn-declared-bus-fn-{index}")
}
fn marker(index: usize) -> String {
    format!("cfn-declared-bus-marker-{index}")
}

/// The assumed-role session a call made by an invocation of the row at `index` speaks for: the
/// session of the function's execution role, named for the FUNCTION.
fn session_arn(index: usize) -> String {
    format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{}/{}",
        role_name(index),
        function_name(index)
    )
}

/// The ARN of the event bus `name` in this account and Region, as a statement names a bus and as
/// a refusal reports the resource.
fn bus_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/{name}")
}

/// The sentence a refusal of a publication onto `resource` by the row at `index` carries:
/// IAM's own wording, ending in the clause `reason` says why with.
fn refusal_message(index: usize, resource: &str, reason: Reason) -> String {
    format!(
        "User: {} is not authorized to perform: {PUT_EVENTS_ACTION} on resource: {resource} {}",
        session_arn(index),
        reason.clause()
    )
}

/// One `Allow` statement of an identity-based document.
fn allow(action: &str, resource: Value) -> Value {
    json!({ "Effect": "Allow", "Action": action, "Resource": resource })
}

/// The statements a row's document carries, written against the template's own references
/// wherever the value is an identifier the DEPLOYMENT computes — so the grant that is supposed to
/// name the stack's own bus names whatever ARN the stack really published for it.
fn statements(row: &Row) -> Vec<Value> {
    let declared_bus = json!({ "Fn::GetAtt": ["Bus", "Arn"] });
    match row.attachment {
        Attachment::OnlyBasicExecution => Vec::new(),
        _ => match row.statements {
            Statements::PublishOnTheDeclaredBus => {
                vec![allow(PUT_EVENTS_ACTION, declared_bus)]
            }
            // An ARN wildcard over every bus of this account and Region, which the declared
            // bus's own ARN matches.
            Statements::PublishOnEveryBusOfTheAccount => vec![allow(
                PUT_EVENTS_ACTION,
                json!(format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/*")),
            )],
            Statements::PublishOnAnotherBus => {
                vec![allow(PUT_EVENTS_ACTION, json!(bus_arn(OTHER_BUS)))]
            }
            Statements::AnotherActionOnTheDeclaredBus => {
                vec![allow(PUT_RULE_ACTION, declared_bus)]
            }
            Statements::PublishAllowedAndExplicitlyDenied => vec![
                allow(PUT_EVENTS_ACTION, json!("*")),
                json!({
                    "Effect": "Deny",
                    "Action": PUT_EVENTS_ACTION,
                    "Resource": declared_bus,
                }),
            ],
        },
    }
}

/// The document a row attaches, or `None` where it attaches none.
fn document(row: &Row) -> Option<Value> {
    let statements = statements(row);
    (!statements.is_empty())
        .then(|| json!({ "Version": "2012-10-17", "Statement": statements }))
}

/// The trust policy every execution role carries: Lambda has to be able to assume the role for a
/// function to be handed it at all.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The template the suite deploys: the custom bus, the queue, the queue's own policy, the rule
/// that carries the queue as its target, and one execution role plus one function per row with
/// that row's document attached the way that row attaches it.
///
/// The rule is declared to depend on the queue's policy, so the target is installed on a queue
/// that already admits the delivering service rather than on one that does not yet.
fn stack_template() -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Bus".to_string(),
        json!({
            "Type": "AWS::Events::EventBus",
            "Properties": { "Name": BUS },
        }),
    );
    resources.insert(
        "Sink".to_string(),
        json!({
            "Type": "AWS::SQS::Queue",
            "Properties": { "QueueName": SINK },
        }),
    );
    resources.insert(
        "SinkPolicy".to_string(),
        json!({
            "Type": "AWS::SQS::QueuePolicy",
            "Properties": {
                "Queues": [{ "Ref": "Sink" }],
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Sid": "AllowRuleDelivery",
                        "Effect": "Allow",
                        "Principal": { "Service": EVENTS_PRINCIPAL },
                        "Action": "sqs:SendMessage",
                        "Resource": { "Fn::GetAtt": ["Sink", "Arn"] },
                    }],
                },
            },
        }),
    );
    resources.insert(
        "Rule".to_string(),
        json!({
            "Type": "AWS::Events::Rule",
            "DependsOn": "SinkPolicy",
            "Properties": {
                "Name": RULE,
                "EventBusName": { "Ref": "Bus" },
                "State": "ENABLED",
                "EventPattern": { "source": [SOURCE], "detail-type": [DETAIL_TYPE] },
                "Targets": [{
                    "Id": TARGET_ID,
                    "Arn": { "Fn::GetAtt": ["Sink", "Arn"] },
                }],
            },
        }),
    );

    for (index, row) in ROWS.iter().enumerate() {
        let role_logical = format!("Role{index}");
        let attached = document(row);
        let policy_name = format!("publication-grant-{index}");
        let mut properties = serde_json::Map::new();
        properties.insert("RoleName".to_string(), json!(role_name(index)));
        properties.insert("AssumeRolePolicyDocument".to_string(), trust_policy());
        properties.insert(
            "ManagedPolicyArns".to_string(),
            json!([BASIC_EXECUTION_POLICY]),
        );
        if let (Attachment::RoleInlinePolicies, Some(attached)) = (row.attachment, attached.as_ref())
        {
            properties.insert(
                "Policies".to_string(),
                json!([{ "PolicyName": &policy_name, "PolicyDocument": attached }]),
            );
        }
        resources.insert(
            role_logical.clone(),
            json!({ "Type": "AWS::IAM::Role", "Properties": Value::Object(properties) }),
        );
        if let (Attachment::RolePolicyResource, Some(attached)) = (row.attachment, attached.as_ref())
        {
            resources.insert(
                format!("Grant{index}"),
                json!({
                    "Type": "AWS::IAM::RolePolicy",
                    "Properties": {
                        "PolicyName": &policy_name,
                        "RoleName": { "Ref": role_logical },
                        "PolicyDocument": attached,
                    },
                }),
            );
        }
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": { "Fn::GetAtt": [role_logical, "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": PROBE_HANDLER },
                },
            }),
        );
    }

    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": {
            BUS_NAME_OUTPUT: { "Value": { "Ref": "Bus" } },
            BUS_ARN_OUTPUT: { "Value": { "Fn::GetAtt": ["Bus", "Arn"] } },
            SINK_URL_OUTPUT: { "Value": { "Ref": "Sink" } },
        },
    })
    .to_string()
}

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the bus it publishes onto is the bus this stack declared.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite signs nothing, as in every other suite of this crate; the credential scope is
    // what routes a request. The HANDLER's own calls are signed by the runtime, with the
    // credential the invocation was given.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. Its credential is the long-term key every emulator publishes, so every call this
/// suite makes on its own behalf is the account's and not any row's role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One `awsQuery` call of `service`, as the HTTP status and the XML it answered.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
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

/// One `X-Amz-Target` RPC call that must be served, against the service whose signing name is
/// `signing_name`.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon SQS call that must be served (`awsJson1_0`, `AmazonSQS.<Operation>`).
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// One EventBridge call that must be served (`awsJson1_1`, `AWSEvents.<Operation>`).
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// Deploy the stack, wait for `CREATE_COMPLETE`, and answer `(its outputs, the logical -> physical
/// id map it reports)`.
async fn deploy(
    router: &MultiServiceRouter,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let template = stack_template();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares named IAM roles, which a create acknowledges before
            // CloudFormation provisions them.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant the owning services report its resources
    // ready, which no fixed sleep can name: the shared wait looks until it reports it, under a
    // deadline generous enough that a loaded host does not decide the outcome.
    let (reported, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, stack_events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&stack_events, "ResourceStatusReason")
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {described} with resource failures \
         {failures:?}"
    );
    let (status, resources) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from DescribeStackResources, got {status} with {resources}"
    );
    (outputs_of(&described), physical_ids_of(&resources))
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let configuration = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &[],
        )
        .await
        .2;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "setup: Lambda must report {name:?} Active, last reported {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// `Invoke` the row's function synchronously, asking the handler to publish `marker` onto `bus`,
/// and answer what it reported.
///
/// The invocation itself must be HTTP 200 with no `FunctionError` whichever way the publication
/// went: the handler catches a refusal and reports it, so a function error here is the crash the
/// runtime must not produce.
async fn invoked(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    bus: &str,
    marker: &str,
) -> Value {
    let event = json!({
        "bus": bus,
        "source": SOURCE,
        "detail_type": DETAIL_TYPE,
        "marker": marker,
    });
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
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
        "case {case:?}: expected no FunctionError, got {signal:?} with {payload}"
    );
    payload
}

/// Every marker that has arrived on the stack's rule target, taken off it as it arrives and
/// accumulated across the whole suite — so a "nothing arrived" read is made over everything the
/// queue ever held rather than over one receive.
struct Sink {
    url: String,
    seen: Vec<String>,
}

impl Sink {
    /// Look again at the queue until `wanted` has arrived on it, or the generous deadline
    /// passes; everything received on the way is kept.
    async fn until(&mut self, router: &MultiServiceRouter, case: &str, wanted: &str) {
        let giving_up_at = Instant::now() + DEADLINE;
        loop {
            self.drain(router, case).await;
            if self.holds(wanted) {
                return;
            }
            assert!(
                Instant::now() < giving_up_at,
                "case {case:?}: expected {wanted:?} to be delivered to the stack's rule target, \
                 got {:?}",
                self.seen
            );
            tokio::time::sleep(LOOK).await;
        }
    }

    /// Take what the queue hands over now, keeping each delivered event's marker and deleting the
    /// message so one delivery is counted once.
    async fn drain(&mut self, router: &MultiServiceRouter, case: &str) {
        let received = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": &self.url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default();
            let envelope: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            assert_eq!(
                envelope["source"],
                json!(SOURCE),
                "case {case:?}: expected the published source on the rule's target, got {body}"
            );
            assert_eq!(
                envelope["detail-type"],
                json!(DETAIL_TYPE),
                "case {case:?}: expected the published detail-type on the rule's target, got \
                 {body}"
            );
            if let Some(found) = envelope["detail"]["marker"].as_str() {
                self.seen.push(found.to_string());
            }
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({
                    "QueueUrl": &self.url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
        }
    }

    /// Whether `marker` has ever arrived on that queue.
    fn holds(&self, marker: &str) -> bool {
        self.seen.iter().any(|seen| seen == marker)
    }
}

/// Every row: the publication a stack-deployed function makes onto the bus the SAME stack
/// declared is decided against the identity documents that stack put on its execution role, and
/// what the stack's own rule target holds afterwards follows that decision.
#[tokio::test]
async fn an_in_function_publication_onto_a_declared_bus_is_decided_against_its_role() {
    let router = build_router();
    let (outputs, physical) = deploy(&router).await;

    // What the stack HAS: the bus, the rule and the queue it declared, and one role and one
    // function per row — plus the standalone grant exactly where a row attaches one that way.
    for logical in ["Bus", "Rule", "Sink", "SinkPolicy"] {
        assert!(
            physical.contains_key(logical),
            "setup: expected a physical id for {logical}, got {physical:?}"
        );
    }
    let bus = outputs
        .get(BUS_NAME_OUTPUT)
        .cloned()
        .unwrap_or_else(|| panic!("setup: no {BUS_NAME_OUTPUT} output in {outputs:?}"));
    assert_eq!(
        bus, BUS,
        "setup: expected the stack to publish the bus name it declared, got {bus:?}"
    );
    // The ARN the STACK published for its own bus: what a grant scoped to it names, and what a
    // refusal of a publication onto it reports as the resource.
    let declared_bus_arn = outputs
        .get(BUS_ARN_OUTPUT)
        .cloned()
        .unwrap_or_else(|| panic!("setup: no {BUS_ARN_OUTPUT} output in {outputs:?}"));
    assert_eq!(
        declared_bus_arn,
        bus_arn(BUS),
        "setup: expected the declared bus's own ARN in this account and Region, got \
         {declared_bus_arn:?}"
    );
    let url = outputs
        .get(SINK_URL_OUTPUT)
        .cloned()
        .unwrap_or_else(|| panic!("setup: no {SINK_URL_OUTPUT} output in {outputs:?}"));

    // The bus the stack reported as created is one EventBridge really holds, read through that
    // service's own API: whatever a row is refused, the bus is there, so a refusal is the
    // authorization decision and not a missing resource.
    let described = events(
        &router,
        "setup",
        "DescribeEventBus",
        &json!({ "Name": BUS }),
    )
    .await;
    assert_eq!(
        described["Arn"].as_str(),
        Some(declared_bus_arn.as_str()),
        "setup: expected EventBridge to hold the bus the stack published, got {described}"
    );

    let mut sink = Sink {
        url,
        seen: Vec::new(),
    };

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let logical = format!("Grant{index}");
        assert_eq!(
            physical.contains_key(&logical),
            row.attachment == Attachment::RolePolicyResource,
            "case {case:?}: expected the stack to hold {logical} only where the row attaches a \
             standalone role policy, got {physical:?}"
        );
        let function = physical
            .get(&format!("Probe{index}"))
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the stack must report its function"));
        assert_eq!(
            function,
            function_name(index),
            "case {case:?}: expected the stack to report the function it declared"
        );
        wait_until_active(&router, &function).await;

        let published = marker(index);
        let report = invoked(&router, case, &function, BUS, &published).await;
        match row.outcome {
            Outcome::Served => {
                assert_eq!(
                    report["ok"],
                    json!(true),
                    "case {case:?}: expected the publication to be served, got {report}"
                );
                assert_eq!(
                    report["failed_count"],
                    json!(0),
                    "case {case:?}: expected no failed entry, got {report}"
                );
                let ids = report["event_ids"].as_array().cloned().unwrap_or_default();
                assert_eq!(
                    ids.len(),
                    1,
                    "case {case:?}: expected one result entry per published entry, got {report}"
                );
                assert!(
                    ids[0].as_str().is_some_and(|id| !id.is_empty()),
                    "case {case:?}: expected EventBridge's own event id, got {report}"
                );
                // The accepted event is matched by the stack's rule and delivered to the target
                // that stack declared, which is where the publication's effect is read.
                sink.until(&router, case, &published).await;
            }
            Outcome::Refused(reason) => {
                assert_eq!(
                    report["ok"],
                    json!(false),
                    "case {case:?}: expected the publication to be refused, got {report}"
                );
                assert_eq!(
                    report["code"],
                    json!(ACCESS_DENIED),
                    "case {case:?}: expected EventBridge's own refusal code, got {report}"
                );
                assert_eq!(
                    report["status"],
                    json!(ACCESS_DENIED_STATUS),
                    "case {case:?}: expected the refusal on HTTP {ACCESS_DENIED_STATUS}, got \
                     {report}"
                );
                let expected = refusal_message(index, &declared_bus_arn, reason);
                assert_eq!(
                    report["message"],
                    json!(expected),
                    "case {case:?}: expected the refusal IAM words for it, got {report}"
                );
            }
        }
    }

    // Nothing of a refused publication ever reached the bus. The read is taken at an instant by
    // which an event planted through the SAME path by the account has already been delivered, so
    // the absence is the refusal's and not the clock's.
    let control = "an event planted by the account after the refused publications";
    let sentinel = "cfn-declared-bus-sentinel";
    let planted = events(
        &router,
        control,
        "PutEvents",
        &json!({
            "Entries": [{
                "EventBusName": BUS,
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({ "marker": sentinel }).to_string(),
            }],
        }),
    )
    .await;
    assert_eq!(
        planted["FailedEntryCount"],
        json!(0),
        "case {control:?}: expected the account's own publication to be accepted, got {planted}"
    );
    sink.until(&router, control, sentinel).await;
    for (index, row) in ROWS.iter().enumerate() {
        if row.outcome == Outcome::Served {
            continue;
        }
        let refused = marker(index);
        assert!(
            !sink.holds(&refused),
            "case {:?}: expected nothing of the refused publication on the stack's rule target, \
             got {:?}",
            row.case,
            sink.seen
        );
    }
}
