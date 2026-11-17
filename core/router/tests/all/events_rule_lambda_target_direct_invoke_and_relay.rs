// SPDX-License-Identifier: CC0-1.0

//! What a deployed rule's Lambda function target serves — a client's own SYNCHRONOUS
//! invocation of it, and the rule-driven handover of a published event — end to end over the
//! real `MultiServiceRouter` with CloudFormation, EventBridge, Lambda, Amazon SQS and IAM
//! mounted on one engine.
//!
//! The rule pinned here has two halves, and neither depends on how the rule, the target or the
//! grant came to exist — a stack's `AWS::Events::Rule` and `AWS::Lambda::Permission` leave
//! behind the same rule, target and statement a client's own `PutRule`/`PutTargets`/
//! `AddPermission` do:
//!
//! * a `RequestResponse` `Invoke` of that function RUNS its handler under the function's
//!   execution role and answers HTTP 200 with the handler's own return value as the payload and
//!   NO `X-Amz-Function-Error` ("if present, indicates that an error occurred during function
//!   execution", bound to that header by `Invoke`), so an AWS-SDK call the handler makes really
//!   takes effect — every time it is invoked, which is what a client's warm-up loop rests on;
//! * every entry a publication ACCEPTS (`FailedEntryCount` 0, the only promise `PutEvents`
//!   models) is matched against each ENABLED rule's `EventPattern` by comparing every member
//!   the pattern names as the whole string, case included, and each target of a matching rule
//!   whose own resource-based policy admits `events.amazonaws.com` is handed that event ONCE,
//!   carrying the whole envelope: `source`, `detail-type` and `detail` exactly as published,
//!   the `id` the publication answered, and `resources` the entry's `Resources` as submitted —
//!   the EMPTY list where the entry publishes none. An entry differing only in `detail-type` is
//!   handed to nothing.
//!
//! Both halves are asserted in ONE run, in the order a client writes them: the direct
//! invocations first (a warm-up, whose own relay is drained), then every NON-matching entry,
//! whose target sink must stay empty, and only then the matching ones. A negative leg alone is
//! uninformative — a dispatcher that delivers nothing satisfies it for free — so the deliveries
//! waited for afterwards are what make the quiet window mean something.
//!
//! The rows differ along what the rule generalises over: WHETHER the rule, its target and the
//! statement were declared by the stack or written through the three API calls afterwards; and,
//! within each row, how many resource ARNs the matching entry publishes (none, one, two) and
//! HOW a non-matching entry's `detail-type` differs from the pinned one (another value, the
//! same value in another case, the pinned value extended).
//!
//! Every observation is one a client makes: the `Invoke` response, and the queue the stack
//! declares, which the function's handler writes with its own AWS SDK and this suite reads
//! through `ReceiveMessage`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, iam, sqs, trust_policy, ACCOUNT, CREATE_COMPLETE, LOOK_INTERVAL,
};

/// The Region every request here is signed for — the one `wait_for_stack_status` polls in.
const REGION: &str = "us-east-1";

/// The `source` the rule's pattern names. Every entry this suite publishes carries it, so a
/// row that is not delivered is not delivered for the reason the row states.
const SOURCE: &str = "com.example.cdn.pipeline";

/// The `detail-type` the rule's pattern pins, and the three strings a non-matching row differs
/// from it by: another detail type, the pinned one in another case, and the pinned one
/// extended. All four are values a client publishes.
const DETAIL_TYPE: &str = "CDN Invalidation Requested";
/// See [`DETAIL_TYPE`].
const OTHER_DETAIL_TYPE: &str = "CDN Distribution State-Change";
/// See [`DETAIL_TYPE`].
const RECASED_DETAIL_TYPE: &str = "cdn invalidation requested";
/// See [`DETAIL_TYPE`].
const EXTENDED_DETAIL_TYPE: &str = "CDN Invalidation Requested (replayed)";

/// The members of `detail` the pattern names — one at the top of it, one nested a level deeper
/// — and the values it pins for them.
const EVENT_NAME_MEMBER: &str = "eventName";
/// See [`EVENT_NAME_MEMBER`].
const EVENT_NAME: &str = "CreateInvalidation";
/// See [`EVENT_NAME_MEMBER`].
const PARAMETERS_MEMBER: &str = "requestParameters";
/// See [`EVENT_NAME_MEMBER`].
const RESOURCE_MEMBER: &str = "distributionId";
/// See [`EVENT_NAME_MEMBER`].
const DISTRIBUTION_ID: &str = "E2WARMUPXYZ123";
/// See [`EVENT_NAME_MEMBER`].
const SECOND_DISTRIBUTION_ID: &str = "E3RELAYABC7890";

/// The member of `detail` the pattern does NOT name, which every entry carries so a delivery
/// is read back against the entry it came from.
const MARKER: &str = "marker";

/// The execution role the declared function is created with, provisioned in IAM before the
/// stack so the template names a role that service really holds.
const ROLE_NAME: &str = "events-relay-target-exec";

/// The service principal EventBridge hands a resource-policy-governed target over as, and the
/// action such a handover performs: "for Lambda functions … EventBridge relies on
/// resource-based policies" (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
/// See [`EVENTS_PRINCIPAL`].
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The payload the direct warm-up invocations carry, which is no event envelope at all.
const WARM_UP_PAYLOAD: &str = "warmup";

/// What the handler returns, whatever it was handed — the value a client's warm-up predicate
/// reads out of the `Invoke` payload.
const HANDLER_RESULT: &str = "ok";

/// How many direct invocations are made before the rule is exercised: more than one, because
/// what a warm-up loop rests on is that EVERY invocation runs the handler.
const WARM_UPS: usize = 2;

/// How long an observation of work the clock owes — the stack settling, the function becoming
/// `Active`, a handover being made, the handler's own relay arriving — waits before giving up,
/// and how long between two looks. The deadline is generous: what is under test is that the
/// work happens at all, never how fast this host gets to it.
const BUDGET: Duration = Duration::from_secs(60);

/// How long an observation that must stay EMPTY keeps looking before concluding nothing came.
/// Nothing rests on this window alone: the positive leg asserted afterwards is what shows the
/// dispatcher was live while it was open.
const QUIET_BUDGET: Duration = Duration::from_secs(3);

/// The placeholder the handler source carries the sink queue's NAME in, filled in per row, so
/// the handler resolves it through the service's own `GetQueueUrl` and the source carries
/// nothing the deployment had to compute.
const SINK_PLACEHOLDER: &str = "SINK_QUEUE_NAME";

/// The handler the stack's `AWS::Lambda::Function` declares inline: it relays whatever it was
/// handed to the sink queue with the AWS SDK, as its execution role, and reports that it ran.
/// Both halves of the rule are read off that one relay — the direct invocation's payload is
/// the warm-up document, the rule-driven one's is the event envelope.
const RELAY_SOURCE: &str = r#"
import json

import boto3


def lambda_handler(event, context):
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName="SINK_QUEUE_NAME")["QueueUrl"]
    client.send_message(QueueUrl=url, MessageBody=json.dumps(event, sort_keys=True))
    return {"ok": True}
"#;

/// CloudFormation, EventBridge, Lambda, Amazon SQS and IAM over ONE engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own SDK call is dispatched back into
/// this same router, so the queue it writes is the queue this suite reads.
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

/// Create the execution role the template names, where IAM holds it: trusted by Lambda, and
/// granting the write its handler makes with the AWS SDK — a handler calls another service
/// with temporary credentials for its execution role, so resolving the sink queue's URL by
/// name and the write itself are both judged against the policies in force on that role.
async fn execution_role(router: &MultiServiceRouter) {
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", ROLE_NAME),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    let relaying = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["sqs:GetQueueUrl", "sqs:SendMessage"],
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "relay-to-the-sink"),
            ("PolicyDocument", &relaying),
        ],
    )
    .await;
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("authorization", authorization("events"))
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

/// One Lambda `restJson1` call, as its status, its headers and its parsed body — the headers
/// because `X-Amz-Function-Error` is where a caller reads whether the run faulted.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
    invocation_type: Option<&str>,
) -> (u16, http::HeaderMap, Value) {
    let payload = match body {
        Some(value) => Bytes::from(value.to_string()),
        None => Bytes::new(),
    };
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"));
    if let Some(kind) = invocation_type {
        builder = builder.header("x-amz-invocation-type", kind);
    }
    let request = builder.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, headers, parsed)
}

/// The pattern the one rule of each row carries: the `source`, the `detail-type`, and two
/// members of `detail`, one of them nested a level deeper.
fn pattern() -> Value {
    json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE],
        "detail": {
            EVENT_NAME_MEMBER: [EVENT_NAME],
            PARAMETERS_MEMBER: { RESOURCE_MEMBER: [DISTRIBUTION_ID] },
        },
    })
}

/// How the rule, its target and the statement admitting the handover came to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wiring {
    /// Declared in the stack, as `AWS::Events::Rule` with its `Targets` and
    /// `AWS::Lambda::Permission`.
    Declared,
    /// Written after the stack settled, by `PutRule`, `AddPermission` and `PutTargets`.
    Written,
}

/// One row: where the wiring came from.
struct Row {
    case: &'static str,
    stack: &'static str,
    wiring: Wiring,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the stack declares the rule, its target and the invoke permission",
            stack: "events-relay-declared",
            wiring: Wiring::Declared,
        },
        Row {
            case: "the rule, its target and the invoke permission are written by PutRule, \
                   AddPermission and PutTargets after the stack settled",
            stack: "events-relay-written",
            wiring: Wiring::Written,
        },
    ]
}

/// One published entry: what it carries, and whether the rule's pattern matches it.
struct Entry {
    case: &'static str,
    /// The value of the one `detail` member the pattern does not name, which tells the entries
    /// apart wherever a delivery is read back.
    marker: &'static str,
    detail_type: &'static str,
    /// The entry's `Resources`, which the envelope reports as `resources`.
    resources: Vec<String>,
    /// Whether the pattern matches this entry, and so whether the target is handed it.
    delivered: bool,
}

/// A well-formed ARN of a distribution, as a client publishing an entry about one writes it.
fn distribution_arn(id: &str) -> String {
    format!(
        "arn:aws:cloudfront::{}:distribution/{id}",
        engine::emulator_account_id()
    )
}

/// Every entry each row publishes: three the pattern matches, differing in how many resource
/// ARNs they publish, and three it does not, differing in how the `detail-type` differs.
fn entries() -> Vec<Entry> {
    vec![
        Entry {
            case: "every member the pattern names matches and the entry publishes no resources",
            marker: "match-no-resources",
            detail_type: DETAIL_TYPE,
            resources: Vec::new(),
            delivered: true,
        },
        Entry {
            case: "every member the pattern names matches and the entry publishes one resource",
            marker: "match-one-resource",
            detail_type: DETAIL_TYPE,
            resources: vec![distribution_arn(DISTRIBUTION_ID)],
            delivered: true,
        },
        Entry {
            case: "every member the pattern names matches and the entry publishes two resources",
            marker: "match-two-resources",
            detail_type: DETAIL_TYPE,
            resources: vec![
                distribution_arn(DISTRIBUTION_ID),
                distribution_arn(SECOND_DISTRIBUTION_ID),
            ],
            delivered: true,
        },
        Entry {
            case: "the detail-type is another value, every other member matches",
            marker: "other-detail-type",
            detail_type: OTHER_DETAIL_TYPE,
            resources: Vec::new(),
            delivered: false,
        },
        Entry {
            case: "the detail-type is the pinned one in another case",
            marker: "recased-detail-type",
            detail_type: RECASED_DETAIL_TYPE,
            resources: vec![distribution_arn(DISTRIBUTION_ID)],
            delivered: false,
        },
        Entry {
            case: "the detail-type carries the pinned one as the prefix of a longer string",
            marker: "extended-detail-type",
            detail_type: EXTENDED_DETAIL_TYPE,
            resources: Vec::new(),
            delivered: false,
        },
    ]
}

/// The `detail` one entry publishes: the members the pattern pins, plus the marker it does
/// not name.
fn detail(entry: &Entry) -> Value {
    json!({
        MARKER: entry.marker,
        EVENT_NAME_MEMBER: EVENT_NAME,
        PARAMETERS_MEMBER: { RESOURCE_MEMBER: DISTRIBUTION_ID },
    })
}

/// The template one row deploys: the sink queue the handler writes, and the function, plus —
/// for [`Wiring::Declared`] — the rule with its one function target and the statement admitting
/// this service's handover, scoped by `SourceArn` to that rule.
fn template(stack: &str, wiring: Wiring) -> String {
    let sink = format!("{stack}-sink");
    let function = format!("{stack}-relay");
    let mut resources = json!({
        "Sink": {
            "Type": "AWS::SQS::Queue",
            "Properties": { "QueueName": sink },
        },
        "Relay": {
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": function,
                "Role": format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
                "Runtime": "python3.12",
                "Handler": "index.lambda_handler",
                "Timeout": 30,
                "Code": { "ZipFile": RELAY_SOURCE.replace(SINK_PLACEHOLDER, &sink) },
            },
        },
    });
    let mut outputs = json!({
        "SinkUrl": { "Value": { "Ref": "Sink" } },
        "FunctionArn": { "Value": { "Fn::GetAtt": ["Relay", "Arn"] } },
    });
    if wiring == Wiring::Declared {
        resources["Rule"] = json!({
            "Type": "AWS::Events::Rule",
            "Properties": {
                "Name": format!("{stack}-rule"),
                "EventPattern": pattern(),
                "State": "ENABLED",
                "Targets": [{
                    "Id": "target-1",
                    "Arn": { "Fn::GetAtt": ["Relay", "Arn"] },
                }],
            },
        });
        resources["InvokeRelay"] = json!({
            "Type": "AWS::Lambda::Permission",
            "Properties": {
                "FunctionName": { "Fn::GetAtt": ["Relay", "Arn"] },
                "Action": INVOKE_ACTION,
                "Principal": EVENTS_PRINCIPAL,
                "SourceArn": { "Fn::GetAtt": ["Rule", "Arn"] },
            },
        });
        outputs["RuleArn"] = json!({ "Value": { "Fn::GetAtt": ["Rule", "Arn"] } });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": outputs,
    })
    .to_string()
}

/// Every message waiting on the queue at `url`, taken off it: each is DELETED as it is read,
/// so what a later look answers is what arrived after this one rather than a copy whose
/// visibility timeout ran out — which is what lets a delivery be counted ONCE.
async fn drain(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
        }
    }
    messages
        .iter()
        .map(|message| {
            let body = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str(body).unwrap_or(Value::Null)
        })
        .collect()
}

/// Everything the queue at `url` holds once it holds `want` relays, so one still in flight is
/// looked for again rather than read as missing; gives up after [`BUDGET`] so a relay that
/// never arrives fails an assertion instead of hanging.
async fn relays_until(router: &MultiServiceRouter, url: &str, want: usize) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    let mut held: Vec<Value> = Vec::new();
    loop {
        held.extend(drain(router, url).await);
        if held.len() >= want || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Whatever the queue at `url` holds within [`QUIET_BUDGET`], looked for again and again so a
/// relay that did happen is seen rather than missed.
async fn whatever_arrives(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let until = Instant::now() + QUIET_BUDGET;
    let mut held: Vec<Value> = Vec::new();
    while Instant::now() < until {
        held.extend(drain(router, url).await);
        if !held.is_empty() {
            return held;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    held
}

/// Poll `GetFunctionConfiguration` until Lambda first reports `name` `Active` — "during this
/// time, you can't invoke or modify the function" (`CreateFunction`) — and answer the ARN it
/// reports for it. The span is that service's own, so nothing here sleeps a fixed span on its
/// behalf.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (_, _, reported) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
            None,
        )
        .await;
        if reported["State"].as_str() == Some("Active") {
            return reported["FunctionArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a FunctionArn for {name}, got {reported}")
                })
                .to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {name:?} Active, last read {reported}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Publish `publishing` as the entries of ONE request, assert every one of them is ACCEPTED,
/// and answer the `EventId` each was given.
async fn publish(router: &MultiServiceRouter, case: &str, publishing: &[&Entry]) -> Vec<String> {
    let entries: Vec<Value> = publishing
        .iter()
        .map(|entry| {
            json!({
                "Source": SOURCE,
                "DetailType": entry.detail_type,
                "Detail": detail(entry).to_string(),
                "Resources": entry.resources,
            })
        })
        .collect();
    let published = events(router, case, "PutEvents", &json!({ "Entries": entries })).await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "case {case:?}: an accepted publication reports no failed entry, got {published}"
    );
    let ids: Vec<String> = published["Entries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry["EventId"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        ids.len(),
        publishing.len(),
        "case {case:?}: expected one answered EventId per published entry, got {published}"
    );
    ids
}

/// Both halves of the rule, per row: the deployed function serves every direct
/// `RequestResponse` invocation with its handler's own result, and the wiring — however it came
/// to exist — hands it each matching entry once with the envelope as published, and hands it no
/// entry whose `detail-type` differs.
#[tokio::test]
async fn a_rules_lambda_target_serves_direct_invocations_and_is_handed_each_matching_event() {
    for row in rows() {
        let case = row.case;
        let router = build_router();
        execution_role(&router).await;

        cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &template(row.stack, row.wiring)),
            ],
        )
        .await;
        let (status, described) = wait_for_stack_status(&router, row.stack, CREATE_COMPLETE).await;
        assert_eq!(
            status, CREATE_COMPLETE,
            "case {case:?}: expected the stack to deploy, got {status} with {described}"
        );
        let outputs = outputs_of(&described);
        let sink_url = outputs.get("SinkUrl").cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish SinkUrl, got {described}")
        });
        let function = format!("{}-relay", row.stack);
        let function_arn = wait_until_active(&router, case, &function).await;

        // Leg one: the direct invocations a client's warm-up loop makes. Each must RUN the
        // handler — answering its own result with no function error — and each run's own SDK
        // write must take effect, which is read off the sink queue afterwards.
        for attempt in 1..=WARM_UPS {
            let (status, headers, answered) = lambda(
                &router,
                "POST",
                &format!("/2015-03-31/functions/{function}/invocations"),
                Some(&json!({ WARM_UP_PAYLOAD: true })),
                Some("RequestResponse"),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from direct invocation {attempt}, got {status} \
                 with {answered}"
            );
            assert!(
                !headers.contains_key("x-amz-function-error"),
                "case {case:?}: expected direct invocation {attempt} to carry no function error, \
                 got {headers:?} with {answered}"
            );
            assert_eq!(
                answered,
                json!({ HANDLER_RESULT: true }),
                "case {case:?}: expected direct invocation {attempt} to answer the handler's own \
                 result, got {answered}"
            );
        }
        let warmed = relays_until(&router, &sink_url, WARM_UPS).await;
        assert_eq!(
            warmed.len(),
            WARM_UPS,
            "case {case:?}: expected each of the {WARM_UPS} direct invocations to have relayed \
             what it was handed, got {warmed:?}"
        );
        for relayed in &warmed {
            assert_eq!(
                relayed,
                &json!({ WARM_UP_PAYLOAD: true }),
                "case {case:?}: expected a direct invocation to relay the payload it was \
                 invoked with, got {relayed}"
            );
        }

        // The wiring, where the row says it comes from. The statement is scoped by
        // `aws:SourceArn` to the rule either way.
        if row.wiring == Wiring::Written {
            let rule = format!("{}-rule", row.stack);
            let written = events(
                &router,
                case,
                "PutRule",
                &json!({
                    "Name": rule,
                    "EventPattern": pattern().to_string(),
                    "State": "ENABLED",
                }),
            )
            .await;
            let rule_arn = written["RuleArn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a RuleArn, got {written}"))
                .to_string();
            let (status, _, granted) = lambda(
                &router,
                "POST",
                &format!("/2015-03-31/functions/{function}/policy"),
                Some(&json!({
                    "StatementId": "AllowEventBridgeInvoke",
                    "Action": INVOKE_ACTION,
                    "Principal": EVENTS_PRINCIPAL,
                    "SourceArn": rule_arn,
                })),
                None,
            )
            .await;
            assert_eq!(
                status, 201,
                "case {case:?}: expected HTTP 201 from AddPermission, got {status} with {granted}"
            );
            let targeted = events(
                &router,
                case,
                "PutTargets",
                &json!({
                    "Rule": rule,
                    "Targets": [{ "Id": "target-1", "Arn": function_arn }],
                }),
            )
            .await;
            assert_eq!(
                targeted["FailedEntryCount"],
                json!(0),
                "case {case:?}: expected the target to be accepted, got {targeted}"
            );
        }

        let all = entries();
        let differing: Vec<&Entry> = all.iter().filter(|entry| !entry.delivered).collect();
        let matching: Vec<&Entry> = all.iter().filter(|entry| entry.delivered).collect();

        // Leg two, negative half: every entry whose detail-type differs, published first and
        // accepted, reaches the target with nothing.
        publish(&router, case, &differing).await;
        let early = whatever_arrives(&router, &sink_url).await;
        assert!(
            early.is_empty(),
            "case {case:?}: expected the entries whose detail-type differs from the pinned one \
             to be handed to nothing, got {early:?}"
        );

        // Leg two, positive half — what makes the negative one mean something: each matching
        // entry is handed to the target once, carrying the envelope as published.
        let matching_ids = publish(&router, case, &matching).await;
        let relayed = relays_until(&router, &sink_url, matching.len()).await;
        assert_eq!(
            relayed.len(),
            matching.len(),
            "case {case:?}: expected the target to be handed each of the {} matching entries \
             once, got {relayed:?}",
            matching.len()
        );
        for (entry, id) in matching.iter().zip(&matching_ids) {
            let envelope = relayed
                .iter()
                .find(|envelope| envelope["detail"][MARKER] == json!(entry.marker));
            let Some(envelope) = envelope else {
                panic!(
                    "case {case:?}/{:?}: expected the target to be handed the matching entry, \
                     got {relayed:?}",
                    entry.case
                );
            };
            assert_eq!(
                envelope["source"],
                json!(SOURCE),
                "case {case:?}/{:?}: expected the handed envelope to carry the published \
                 source, got {envelope}",
                entry.case
            );
            assert_eq!(
                envelope["detail-type"],
                json!(entry.detail_type),
                "case {case:?}/{:?}: expected the handed envelope to carry the published \
                 detail-type, got {envelope}",
                entry.case
            );
            assert_eq!(
                envelope["detail"],
                detail(entry),
                "case {case:?}/{:?}: expected the handed envelope to carry the published detail \
                 unchanged, got {envelope}",
                entry.case
            );
            assert_eq!(
                envelope["resources"],
                json!(entry.resources),
                "case {case:?}/{:?}: expected the handed envelope to report the entry's \
                 Resources as submitted, got {envelope}",
                entry.case
            );
            assert_eq!(
                envelope["id"],
                json!(id),
                "case {case:?}/{:?}: expected the handed envelope to carry the EventId the \
                 publication answered, got {envelope}",
                entry.case
            );
            assert_eq!(
                envelope["region"],
                json!(REGION),
                "case {case:?}/{:?}: expected the handed envelope to report the Region it was \
                 published in, got {envelope}",
                entry.case
            );
            assert_eq!(
                envelope["account"],
                json!(engine::emulator_account_id()),
                "case {case:?}/{:?}: expected the handed envelope to report the publishing \
                 account, got {envelope}",
                entry.case
            );
        }

        // The differing entries are handed to nothing LATER either: by now every handover the
        // matching entries owed has been made, so one of a differing entry would be here.
        let late = whatever_arrives(&router, &sink_url).await;
        assert!(
            late.is_empty(),
            "case {case:?}: expected nothing further to be handed to the target, got {late:?}"
        );
        for entry in &differing {
            assert!(
                !relayed
                    .iter()
                    .any(|envelope| envelope["detail"][MARKER] == json!(entry.marker)),
                "case {case:?}/{:?}: expected the entry whose detail-type differs to be handed \
                 to nothing, got {relayed:?}",
                entry.case
            );
        }
    }
}
