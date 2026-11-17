// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING function's own EventBridge publication is decided by: the identity-based
//! policies in force on the function's execution role — the ones embedded in it and the
//! managed ones attached to it alike — and what the bus does with a publication they allow.
//!
//! RULE: a call an AWS SDK makes from inside a running function is a real call made as a
//! session of that function's execution role, so the service serving it decides it against
//! EVERY identity-based policy in force on that role at the instant of the call, whether
//! embedded in the role or attached to it as a managed policy: a publication those documents
//! allow is served — `FailedEntryCount` 0 and an `EventId` per entry — and the event is then
//! matched against the bus's rules and delivered to the rule's target, while a publication
//! they do not allow is refused with `AccessDeniedException` (HTTP 400) naming the
//! assumed-role session, the action `events:PutEvents` and the ARN of the bus the entry
//! addressed, and NOTHING of it reaches the bus or the rule's target. Either way the
//! invocation is HTTP 200 with no `X-Amz-Function-Error`, because what decides that is
//! whether the handler returned — a refusal the handler catches is not an error of the run.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), the identity a function's own call speaks for is the
//! session `arn:aws:sts::<account>:assumed-role/<role>/<function>`, and with neither an
//! identity-based nor a resource-based allow a request is denied (IAM User Guide, *Policy
//! evaluation logic*). EventBridge's `PutEvents` declares no authorization error of its own
//! (`smithy_query(query="operation", operation="PutEvents")` names `InternalException`
//! only), so the refusal is the AWS-common one, and its output carries `FailedEntryCount`
//! beside one `PutEventsResultEntry` per entry
//! (`smithy_query(query="operation", operation="PutEvents")`), which is what a served
//! publication is read off.
//!
//! Six crates have to agree for that to hold — CloudFormation deploys each row's role and
//! the function running as it, IAM holds the roles, their embedded documents and the managed
//! policies attached to them, the token service mints the session the handler's call is
//! signed as, Lambda runs the handler and supplies it the SDK, EventBridge takes the
//! decision and matches what it accepted, and Amazon SQS holds what the rule delivered — so
//! the suite lives here: a crate-local suite can only ever see one side of it.
//!
//! What the neighbours pin and this one does not repeat: `lambda_in_function_sqs_authorization`
//! drives the table of policy shapes a QUEUE call is decided by,
//! `lambda_python_runtime_bundles_the_aws_sdk` that every managed Python runtime carries the
//! SDK at all, and `lambda_handler_fans_out_to_sns_sqs_and_eventbridge` one handler reaching
//! three services over three protocols. What only this suite reaches is the TABLE of
//! identity-document shapes one in-function publication is decided by, and the rule's target
//! as the place each decision is read back — which is what a least-privilege publisher's
//! execution role is written for.
//!
//! The rows differ along what the rule generalises over: WHERE the document in force comes
//! from (a managed policy AWS owns and attached, a managed policy the account owns and
//! attached, a document embedded in the role), WHICH action it names (the publication's own,
//! every action of the service, another operation's), WHICH resource it names (every
//! resource, the bus the entry addresses, another bus of the account) and which EFFECT it
//! states (an allow, an allow with an explicit deny beside it). Each row asserts both halves:
//! what the SDK gave the handler, and what the rule's target queue holds afterwards — so a
//! refusal that still published, and a grant that answered without delivering, both fail here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode, physical_ids_of};

/// The Region every request here is signed for, and so the Region the buses, the rule, the
/// queue and the functions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// Where `Invoke` and the configuration read are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The bus every row's handler publishes onto, and another bus of the account, which one
/// row's statement names instead of it.
const BUS: &str = "default";
const OTHER_BUS: &str = "xsvc-in-fn-events-elsewhere";

/// The rule watching the bus for the handlers' events, and the queue it targets: the place a
/// publication EventBridge accepted is read back from.
const RULE: &str = "xsvc-in-fn-events-rule";
const SINK: &str = "xsvc-in-fn-events-sink";

/// The `source` and `detail-type` every handler publishes under, which is all the rule
/// matches on — so a message on its queue is one of these publications and nothing else.
const SOURCE: &str = "com.demo.xsvc-in-fn-events";
const DETAIL_TYPE: &str = "In Function Publication";

/// The stack the roles and the functions are deployed by.
const STACK: &str = "xsvc-in-fn-events-probes";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The action a publication onto a bus performs, as a statement names it and as a refusal
/// reports it, and another operation's action, which grants nothing of it.
const PUT_EVENTS_ACTION: &str = "events:PutEvents";
const PUT_RULE_ACTION: &str = "events:PutRule";

/// The code and status EventBridge refuses a publication the policies in force do not permit
/// with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u64 = 400;

/// The managed policy AWS owns that every Lambda execution role carries: it grants the log
/// writes a function makes and nothing of any other service, so a role carrying only it is
/// granted no publication.
const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The managed policy the ACCOUNT owns, created by this suite and attached to one row's role:
/// the other half of "attached counts too", for a document this emulator holds itself.
const PUBLISH_POLICY: &str = "xsvc-in-fn-events-publish";

/// The service principal the sink queue's policy admits, because a delivery to a queue is
/// made by the delivering service itself and "EventBridge relies on resource-based policies"
/// (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The runtime every function is created with: a managed Python runtime, which carries the
/// SDK the handler imports.
const RUNTIME: &str = "python3.12";

/// The module and attribute the deployment package carries its handler as. CloudFormation
/// archives inline code under the handler's own file stem, so the module is `index`.
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization decision and not how
/// fast an interpreter gets through one SDK call.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long work the CLOCK owes — a stack completing, a function becoming `Active`, an event
/// reaching the rule's target — is waited for before giving up, and how long between two
/// looks. A GENEROUS ceiling rather than an expectation: every wait here is on the CONDITION,
/// and nothing asserts how long anything took.
const DEADLINE: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The handler every row runs. It imports the SDK AT MODULE SCOPE — which is where a package
/// holding only this module has to be able to resolve it — builds a client from it there too,
/// and publishes ONE event, reporting what the publication answered or the refusal it raised.
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
            "sdk": boto3.__name__,
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {
        "sdk": boto3.__name__,
        "ok": True,
        "failed_count": answered["FailedEntryCount"],
        "event_ids": [entry["EventId"] for entry in answered["Entries"]],
    }
"#;

/// WHERE the document that decides a row's publication comes from, and what it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// Nothing but the managed policy AWS owns that every execution role carries.
    OnlyBasicExecution,
    /// An embedded document allowing the publication's own action on every resource.
    EmbeddedOnEveryResource,
    /// An embedded document allowing it on the bus the entry addresses.
    EmbeddedOnTheBus,
    /// An embedded document allowing EVERY action of the service on every resource.
    EmbeddedServiceWildcard,
    /// A managed policy of the ACCOUNT, attached to the role, allowing the publication.
    AttachedAccountManaged,
    /// An embedded document allowing ANOTHER operation's action on the bus.
    EmbeddedOtherAction,
    /// An embedded document allowing the publication's own action on ANOTHER bus.
    EmbeddedOnAnotherBus,
    /// An embedded document allowing it on every resource, with an explicit deny of it on the
    /// bus beside that allow.
    EmbeddedAllowAndDeny,
}

impl Grant {
    /// The statements the role's EMBEDDED document carries, or none where the row's grant is
    /// not an embedded one.
    fn statements(self) -> Vec<Value> {
        match self {
            Self::OnlyBasicExecution | Self::AttachedAccountManaged => Vec::new(),
            Self::EmbeddedOnEveryResource => vec![allow(PUT_EVENTS_ACTION, "*")],
            Self::EmbeddedOnTheBus => vec![allow(PUT_EVENTS_ACTION, &bus_arn(BUS))],
            Self::EmbeddedServiceWildcard => vec![allow("events:*", "*")],
            Self::EmbeddedOtherAction => vec![allow(PUT_RULE_ACTION, &bus_arn(BUS))],
            Self::EmbeddedOnAnotherBus => vec![allow(PUT_EVENTS_ACTION, &bus_arn(OTHER_BUS))],
            Self::EmbeddedAllowAndDeny => vec![
                allow(PUT_EVENTS_ACTION, "*"),
                json!({
                    "Effect": "Deny",
                    "Action": PUT_EVENTS_ACTION,
                    "Resource": bus_arn(BUS),
                }),
            ],
        }
    }

    /// The managed policies ATTACHED to the role: the one AWS owns that every execution role
    /// carries, and the account's own where the row's grant is that one.
    fn attachments(self, account_managed: &str) -> Vec<String> {
        let mut attached = vec![BASIC_EXECUTION_POLICY.to_string()];
        if self == Self::AttachedAccountManaged {
            attached.push(account_managed.to_string());
        }
        attached
    }
}

/// One `Allow` statement of an identity-based document.
fn allow(action: &str, resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": action, "Resource": resource })
}

/// What the publication EARNS: EventBridge serves it, or refuses it with the clause naming
/// the document that withheld the grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The policies in force allow it: the entry is accepted and delivered to the rule's
    /// target.
    Served,
    /// They do not, and the refusal closes with this clause.
    Refused(&'static str),
}

/// The clause a refusal closes with when no document in force states anything about the
/// publication — IAM's implicit deny.
const NO_POLICY_ALLOWS: &str = "because no identity-based policy allows the events:PutEvents \
                                action";

/// The clause it closes with when a document in force DENIES it explicitly.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// One row: the documents in force on one execution role, and what the publication the
/// function running as it makes earns.
struct Row {
    case: &'static str,
    grant: Grant,
    outcome: Outcome,
}

const ROWS: &[Row] = &[
    // The case the rule is stated about, both ways round: a role carrying only the managed
    // policy every execution role carries publishes nothing, and the same role with the
    // publication allowed on every resource publishes and is delivered.
    Row {
        case: "only the basic-execution policy AWS owns is attached",
        grant: Grant::OnlyBasicExecution,
        outcome: Outcome::Refused(NO_POLICY_ALLOWS),
    },
    Row {
        case: "an embedded document allows the publication on every resource",
        grant: Grant::EmbeddedOnEveryResource,
        outcome: Outcome::Served,
    },
    Row {
        case: "an embedded document allows it on the bus the entry addresses",
        grant: Grant::EmbeddedOnTheBus,
        outcome: Outcome::Served,
    },
    Row {
        case: "an embedded document allows every action of the service",
        grant: Grant::EmbeddedServiceWildcard,
        outcome: Outcome::Served,
    },
    Row {
        case: "a managed policy of the account allows it and is attached to the role",
        grant: Grant::AttachedAccountManaged,
        outcome: Outcome::Served,
    },
    Row {
        case: "an embedded document allows another operation's action only",
        grant: Grant::EmbeddedOtherAction,
        outcome: Outcome::Refused(NO_POLICY_ALLOWS),
    },
    Row {
        case: "an embedded document allows the publication on another bus",
        grant: Grant::EmbeddedOnAnotherBus,
        outcome: Outcome::Refused(NO_POLICY_ALLOWS),
    },
    Row {
        case: "an embedded document allows it everywhere and denies it on the bus",
        grant: Grant::EmbeddedAllowAndDeny,
        outcome: Outcome::Refused(EXPLICIT_DENY),
    },
];

/// The role one row runs as, the function it runs in and the marker that run publishes — the
/// first two named here because the refusal's own sentence names both: the identity a
/// function's call is signed as is the session `assumed-role/<role>/<function>`.
fn role_name(index: usize) -> String {
    format!("xsvc-in-fn-events-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-in-fn-events-probe-{index}")
}
fn marker(index: usize) -> String {
    format!("xsvc-in-fn-events-marker-{index}")
}

/// The ARN of an event bus of this account in this Region, which is the resource a
/// publication onto it is authorized on and the one a refusal reports.
fn bus_arn(name: &str) -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/{name}")
}

/// The ARN of a queue of this account in this Region.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
}

/// The session a call made by an invocation of the row's function speaks for: the session a
/// function's calls are made under is named for the FUNCTION, which is how a refusal says
/// which function was refused.
fn session_arn(index: usize) -> String {
    format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{}/{}",
        role_name(index),
        function_name(index)
    )
}

/// The sentence EventBridge refuses a publication with. Written out here rather than built
/// from the service's own helper, so the wording has something to be wrong about.
fn refusal_message(index: usize, reason: &str) -> String {
    format!(
        "User: {} is not authorized to perform: {PUT_EVENTS_ACTION} on resource: {} {reason}",
        session_arn(index),
        bus_arn(BUS),
    )
}

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched
/// back into this same router, so the bus it publishes onto is the one this suite watches.
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

/// One `awsQuery` call that must be served, as the XML it carried.
async fn served_query(
    router: &MultiServiceRouter,
    case: &str,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, service, version, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {service} {action}, got {status} with {body}"
    );
    body
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

/// The trust policy a Lambda execution role needs: this service has to be able to assume the
/// role for a function to be handed it at all.
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

/// The managed policy of the ACCOUNT one row's role is granted by: created where a client
/// creates one, so the document the decision reads is one IAM itself holds. Answers its ARN.
async fn given_account_managed_policy(router: &MultiServiceRouter) -> String {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [allow(PUT_EVENTS_ACTION, "*")],
    })
    .to_string();
    let created = served_query(
        router,
        "setup",
        "iam",
        "2010-05-08",
        "CreatePolicy",
        &[
            ("PolicyName", PUBLISH_POLICY),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    let arn = element(&created, "Arn");
    assert!(
        arn.ends_with(&format!(":policy/{PUBLISH_POLICY}")),
        "setup: expected the created policy's own ARN, got {arn:?}"
    );
    arn
}

/// The other bus of the account, which one row's statement names instead of the bus the entry
/// addresses.
async fn given_other_bus(router: &MultiServiceRouter) {
    let created = events(router, "setup", "CreateEventBus", &json!({ "Name": OTHER_BUS })).await;
    assert_eq!(
        created["EventBusArn"].as_str(),
        Some(bus_arn(OTHER_BUS).as_str()),
        "setup: expected the created bus to report its own ARN, got {created}"
    );
}

/// The queue the rule targets, its policy admitting the delivering service scoped to that
/// rule, and the rule itself. Answers the queue's url.
async fn given_rule_and_sink(router: &MultiServiceRouter) -> String {
    let case = "setup";
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": SINK })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        attributes["Attributes"]["QueueArn"].as_str(),
        Some(queue_arn(SINK).as_str()),
        "setup: expected the queue ARN of this account and Region, got {attributes}"
    );

    let pattern = json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string();
    let rule = events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": BUS,
            "EventPattern": pattern,
            "State": "ENABLED",
        }),
    )
    .await;
    let rule_arn = rule["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a RuleArn in {rule}"))
        .to_string();

    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowRuleDelivery",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn(SINK),
            "Condition": {"ArnEquals": {"aws:SourceArn": rule_arn}},
        }],
    })
    .to_string();
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    )
    .await;

    let targets = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": RULE,
            "EventBusName": BUS,
            "Targets": [{ "Id": "sink", "Arn": queue_arn(SINK) }],
        }),
    )
    .await;
    assert_eq!(
        targets["FailedEntryCount"],
        json!(0),
        "setup: expected PutTargets to accept the queue target, got {targets}"
    );
    url
}

/// The template the suite deploys: one execution role per row, carrying that row's embedded
/// document and that row's attachments, and one function per row running as it. The inline
/// source is archived into the deployment package by CloudFormation, which is what makes the
/// package hold exactly ONE module — the handler's — and no SDK of its own.
fn stack_template(account_managed: &str) -> String {
    let mut resources = serde_json::Map::new();
    for (index, row) in ROWS.iter().enumerate() {
        let role_logical = format!("Role{index}");
        let mut properties = serde_json::Map::new();
        properties.insert("RoleName".to_string(), json!(role_name(index)));
        properties.insert("AssumeRolePolicyDocument".to_string(), trust_policy());
        properties.insert(
            "ManagedPolicyArns".to_string(),
            json!(row.grant.attachments(account_managed)),
        );
        let statements = row.grant.statements();
        if !statements.is_empty() {
            properties.insert(
                "Policies".to_string(),
                json!([{
                    "PolicyName": "publication-grants",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": statements,
                    },
                }]),
            );
        }
        resources.insert(
            role_logical.clone(),
            json!({ "Type": "AWS::IAM::Role", "Properties": Value::Object(properties) }),
        );
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": [role_logical, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Deploy the stack, wait for `CREATE_COMPLETE` and answer the physical id of each declared
/// resource.
async fn deploy(router: &MultiServiceRouter, account_managed: &str) -> BTreeMap<String, String> {
    let template = stack_template(account_managed);
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
    // The stack's completion is owed from the instant the owning services report its
    // resources ready, which no fixed sleep can name: the shared wait looks until it reports
    // it, under a deadline generous enough that a loaded host does not decide the outcome.
    let (reported, body) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
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
        "setup: the stack must reach {CREATE_COMPLETE}, got {body} with resource failures \
         {failures:?}"
    );
    let resources = served_query(
        router,
        "setup",
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    physical_ids_of(&resources)
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

/// `Invoke` the row's function synchronously, asking the handler to publish `marker`, and
/// answer what it reported.
///
/// The invocation itself must be HTTP 200 with no `FunctionError` whichever way the
/// publication went: the handler catches a refusal and reports it, so a function error here
/// is the crash the runtime must not produce — and the report naming the SDK's own module is
/// what says the package's `import` resolved at all.
async fn invoked(router: &MultiServiceRouter, case: &str, function: &str, marker: &str) -> Value {
    let event = json!({
        "bus": BUS,
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
    assert_eq!(
        payload["sdk"],
        json!("boto3"),
        "case {case:?}: expected the run to report the SDK module it imported, got {payload}"
    );
    payload
}

/// Every marker that has arrived on the rule's target queue, taken off it as it arrives and
/// accumulated across the whole suite — so a "nothing arrived" read is made over everything
/// the queue ever held rather than over one receive.
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
                "case {case:?}: expected {wanted:?} to be delivered to the rule's queue, got {:?}",
                self.seen
            );
            tokio::time::sleep(LOOK).await;
        }
    }

    /// Take what the queue hands over now, keeping each delivered event's marker and deleting
    /// the message so one delivery is counted once.
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
                "case {case:?}: expected the published source on the rule's queue, got {body}"
            );
            assert_eq!(
                envelope["detail-type"],
                json!(DETAIL_TYPE),
                "case {case:?}: expected the published detail-type on the rule's queue, got \
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

/// Every row: the publication a deployed function makes with the SDK the runtime supplies is
/// decided against every identity-based policy in force on its execution role, and what the
/// rule's target queue holds afterwards follows that decision.
#[tokio::test]
async fn an_in_function_publication_is_decided_against_its_execution_role() {
    let router = build_router();
    let account_managed = given_account_managed_policy(&router).await;
    given_other_bus(&router).await;
    let url = given_rule_and_sink(&router).await;
    let physical = deploy(&router, &account_managed).await;
    let mut sink = Sink {
        url,
        seen: Vec::new(),
    };

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let logical = format!("Probe{index}");
        let function = physical
            .get(&logical)
            .cloned()
            .unwrap_or_else(|| panic!("setup: the stack must report a physical id for {logical}"));
        assert_eq!(
            function,
            function_name(index),
            "setup: expected the stack to report the function it declared for {case:?}"
        );
        wait_until_active(&router, &function).await;

        let published = marker(index);
        let report = invoked(&router, case, &function, &published).await;
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
                // The accepted event is matched by the rule and delivered to its target, which
                // is where the publication's effect is read.
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
                assert_eq!(
                    report["message"],
                    json!(refusal_message(index, reason)),
                    "case {case:?}: expected the refusal IAM words for it, got {report}"
                );
            }
        }
    }

    // Nothing of a refused publication ever reached the bus. The read is taken at an instant
    // by which an event planted through the SAME path as the account has already been
    // delivered, so the absence is the refusal's and not the clock's.
    let control = "an event planted by the account after the refused publications";
    let sentinel = "xsvc-in-fn-events-sentinel";
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
            "case {:?}: expected nothing of the refused publication on the rule's queue, got {:?}",
            row.case,
            sink.seen
        );
    }
}
