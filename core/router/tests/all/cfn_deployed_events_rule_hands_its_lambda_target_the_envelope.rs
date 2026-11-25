// SPDX-License-Identifier: CC0-1.0

//! WHAT a CloudFormation-deployed EventBridge rule hands the Lambda function it targets, read
//! where a client reads it: on the Amazon SQS queue the same stack declares and the deployed
//! handler forwards to — end to end over the real `MultiServiceRouter` with CloudFormation,
//! EventBridge, Lambda, Amazon SQS and IAM mounted on one engine.
//!
//! The rule pinned here: **a published entry is matched against every rule of the bus it names,
//! and each target of every matching ENABLED rule is handed the WHOLE event envelope — `version`
//! (the event format's own schema version), `id` (the identifier `PutEvents` answered for that
//! entry), `detail-type`, `source`, `resources` and `detail` exactly as the entry published them,
//! and `account`, `region` and `time` from the publication's own context — while a rule the
//! declaration put in `DISABLED` state and a rule no entry matches are handed nothing at all.**
//! None of that depends on how the rule, its target or the invoke grant came to exist: a stack's
//! `AWS::Events::Rule` and `AWS::Lambda::Permission` leave behind the same rule, target and
//! statement a client's own `PutRule`/`PutTargets`/`AddPermission` do, which is why this suite
//! asserts the deployed control plane (`ListTargetsByRule` and the function's own `GetPolicy`)
//! and then the delivery that control plane promises.
//!
//! Five crates have to agree for that to hold — CloudFormation provisions the queues, the
//! functions, the two rules and the statements; EventBridge matches each entry and hands the
//! envelope over; Lambda decides the invocation against the function's resource policy and then
//! RUNS the handler; Amazon SQS holds what the handler forwarded; IAM holds the execution role
//! the handler's own write is judged against — so the suite lives here rather than in any one
//! crate. Every observation is a client's: the stack's outputs, two modelled reads, and the
//! bodies on the published queues.
//!
//! The rows are the entries of ONE `PutEvents` request, and differ along exactly what the rule
//! generalises over: the shape of `detail` (absent members, a flat document, a nested one
//! carrying an array and non-ASCII text), how many ARNs `resources` lists (none, one, two),
//! WHICH of the two `detail-type` values the pattern names the entry carries, and whether the
//! entry's `source` and `detail-type` are named by the pattern at all. Each forwarded envelope is
//! tied back to its row by the `EventId` that entry's publication answered, so a row publishing
//! an empty `detail` is correlated by the same means as every other.
//!
//! `crate::cloudformation_events_rule_lambda_target_permission_boundary` is the suite for WHICH
//! deployed grant admits the handover and where an inadmissible one is dead-lettered, and
//! `crate::events_lambda_target_qualified_arn_envelope` for the qualifier a target ARN addresses;
//! what only this one reads is the whole envelope arriving at a client-visible sink out of a
//! declaratively deployed pipeline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, drain, execution_role, lambda, stayed_quiet, CREATE_COMPLETE, LOOK_INTERVAL,
    ROLE_NAME,
};

/// The Region every request here is signed for — the one the helpers above address, and so the
/// Region the envelope must report.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which is the `account` of every envelope it builds.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The `version` an EventBridge envelope carries: the schema version of the event FORMAT, a
/// constant of that format rather than anything one event decides.
const ENVELOPE_VERSION: &str = "0";

/// The stack that declares the whole pipeline.
const STACK: &str = "cfn-events-lambda-envelope";

/// The `source` both deployed rules match on, and one neither names.
const SOURCE: &str = "cfn.events.envelope";
const OTHER_SOURCE: &str = "cfn.events.elsewhere";

/// The two `detail-type` values the deployed pattern names, and one it does not.
const DETAIL_TYPE: &str = "Envelope Probe";
const SECOND_DETAIL_TYPE: &str = "Envelope Probe Again";
const OTHER_DETAIL_TYPE: &str = "Envelope Probe Unnamed";

/// The principal EventBridge hands a resource-policy-governed target over as, and the action an
/// invocation of a function performs — what the stack's `AWS::Lambda::Permission` has to grant.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The condition operator and key Lambda writes a grant's `SourceArn` restriction as, which is
/// what scopes the deployed statement to one rule.
const ARN_LIKE: &str = "ArnLike";
const SOURCE_ARN_KEY: &str = "AWS:SourceArn";

/// How long one invocation is given, as the function's own `Timeout`: well past what the forward
/// needs on any host, because what is under test is the envelope rather than how fast an
/// interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long the deliveries are waited for before the suite gives up on them, and how long it
/// waits before concluding that an entry no pattern matched — and the `DISABLED` rule's target —
/// really were handed nothing. Both are waits on a CONDITION, looked at again every
/// [`LOOK_INTERVAL`]; the deadline is generous enough that a loaded host cannot decide the
/// outcome.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(90);
const QUIET_DEADLINE: Duration = Duration::from_secs(3);

/// How many times the function's state is polled before the suite gives up on it.
const POLLS: usize = 900;

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

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/events/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
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

/// Poll `GetFunctionConfiguration` until Lambda first reports `name` `Active` — "the function is
/// ready to be invoked" (`FunctionConfiguration$State`). The span is that service's own, so
/// nothing here sleeps a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
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
    panic!("Lambda never reported {name:?} Active");
}

/// The handler the stack declares inline for the ENABLED rule's target: it forwards the WHOLE
/// event it was handed to the sink the same stack declares, which is where the envelope is
/// observed. The queue is addressed by NAME through that service's own `GetQueueUrl`, so the
/// deployment computes nothing the source had to be told.
const FORWARDING_SOURCE: &str = r#"
import json
import boto3


def lambda_handler(event, context):
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName="SINK_NAME")["QueueUrl"]
    client.send_message(QueueUrl=url, MessageBody=json.dumps(event))
    return {"forwarded": 1}
"#;

/// The template the pipeline is deployed from: the sink the permitted target's handler forwards
/// to, a second sink nothing may ever write to, one function per sink, an ENABLED rule and a
/// `DISABLED` rule carrying the SAME event pattern with one function target each, and the
/// `AWS::Lambda::Permission` for each function scoped by `SourceArn` to the rule that targets
/// it.
fn template() -> String {
    let sink = format!("{STACK}-sink");
    let quiet = format!("{STACK}-quiet");
    let pattern = json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE, SECOND_DETAIL_TYPE],
    });
    let role = format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}");
    let function = |name: &str, queue: &str| {
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": name,
                "Role": role,
                "Runtime": "python3.12",
                "Handler": "index.lambda_handler",
                "Timeout": FUNCTION_TIMEOUT,
                "Code": { "ZipFile": FORWARDING_SOURCE.replace("SINK_NAME", queue) },
            },
        })
    };
    let rule = |name: &str, state: &str, target: &str| {
        json!({
            "Type": "AWS::Events::Rule",
            "Properties": {
                "Name": name,
                "EventPattern": pattern,
                "State": state,
                "Targets": [{
                    "Id": "forwarder",
                    "Arn": { "Fn::GetAtt": [target, "Arn"] },
                }],
            },
        })
    };
    let permission = |target: &str, rule: &str| {
        json!({
            "Type": "AWS::Lambda::Permission",
            "Properties": {
                "FunctionName": { "Fn::GetAtt": [target, "Arn"] },
                "Action": INVOKE_ACTION,
                "Principal": EVENTS_PRINCIPAL,
                "SourceArn": { "Fn::GetAtt": [rule, "Arn"] },
            },
        })
    };
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Sink": { "Type": "AWS::SQS::Queue", "Properties": { "QueueName": sink } },
            "Quiet": { "Type": "AWS::SQS::Queue", "Properties": { "QueueName": quiet } },
            "Forwarder": function(&format!("{STACK}-forwarder"), &sink),
            "Idle": function(&format!("{STACK}-idle"), &quiet),
            "EnabledRule": rule(&enabled_rule(), "ENABLED", "Forwarder"),
            "DisabledRule": rule(&disabled_rule(), "DISABLED", "Idle"),
            "InvokeForwarder": permission("Forwarder", "EnabledRule"),
            "InvokeIdle": permission("Idle", "DisabledRule"),
        },
        "Outputs": {
            "SinkUrl": { "Value": { "Ref": "Sink" } },
            "QuietUrl": { "Value": { "Ref": "Quiet" } },
            "RuleArn": { "Value": { "Fn::GetAtt": ["EnabledRule", "Arn"] } },
            "FunctionArn": { "Value": { "Fn::GetAtt": ["Forwarder", "Arn"] } },
        },
    })
    .to_string()
}

/// The name each declared rule is created under, which is also its physical resource id.
fn enabled_rule() -> String {
    format!("{STACK}-enabled")
}
fn disabled_rule() -> String {
    format!("{STACK}-disabled")
}

/// One row: an entry of the single `PutEvents` request, and whether the deployed pattern names
/// it.
struct Row {
    case: &'static str,
    source: &'static str,
    detail_type: &'static str,
    resources: Vec<&'static str>,
    detail: Value,
    matched: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an empty detail and no resources, under the first detail-type",
            source: SOURCE,
            detail_type: DETAIL_TYPE,
            resources: vec![],
            detail: json!({}),
            matched: true,
        },
        Row {
            case: "a flat detail and one resource ARN",
            source: SOURCE,
            detail_type: DETAIL_TYPE,
            resources: vec!["arn:aws:s3:::cfn-events-envelope-bucket"],
            detail: json!({ "probe": "cfn", "attempt": 1, "settled": true }),
            matched: true,
        },
        Row {
            case: "a nested detail carrying an array and non-ASCII text, two resource ARNs",
            source: SOURCE,
            detail_type: DETAIL_TYPE,
            resources: vec![
                "arn:aws:s3:::cfn-events-envelope-bucket",
                "arn:aws:sqs:us-east-1:000000000000:cfn-events-envelope-resource",
            ],
            detail: json!({
                "probe": "cfn",
                "nested": { "depth": [1, 2, 3], "label": "caf\u{e9} \"quoted\"" },
            }),
            matched: true,
        },
        Row {
            case: "the second detail-type the pattern names",
            source: SOURCE,
            detail_type: SECOND_DETAIL_TYPE,
            resources: vec!["arn:aws:s3:::cfn-events-envelope-bucket"],
            detail: json!({ "probe": "cfn", "attempt": 2 }),
            matched: true,
        },
        Row {
            case: "a source the pattern does not name",
            source: OTHER_SOURCE,
            detail_type: DETAIL_TYPE,
            resources: vec![],
            detail: json!({ "probe": "cfn" }),
            matched: false,
        },
        Row {
            case: "a detail-type the pattern does not name",
            source: SOURCE,
            detail_type: OTHER_DETAIL_TYPE,
            resources: vec!["arn:aws:s3:::cfn-events-envelope-bucket"],
            detail: json!({ "probe": "cfn" }),
            matched: false,
        },
    ]
}

/// Every body the queue at `url` has handed over, parsed as the envelope a forward carries.
async fn forwarded(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    drain(router, url)
        .await
        .into_iter()
        .map(|body| {
            serde_json::from_str(&body).unwrap_or_else(|error| {
                panic!("expected every forwarded body to be the JSON envelope, got {body:?} ({error})")
            })
        })
        .collect()
}

/// Whether `envelopes` holds one carrying `id`.
fn holds(envelopes: &[Value], id: &str) -> bool {
    envelopes.iter().any(|envelope| envelope["id"] == json!(id))
}

/// The whole deployed pipeline: the control plane the stack installed, the envelope every matched
/// entry's target is handed, and the two places nothing may arrive.
#[tokio::test]
async fn a_deployed_rules_lambda_target_is_handed_the_whole_envelope_of_every_matched_entry() {
    let router = build_router();

    // The execution role the template names, created where IAM holds it: the handler's own
    // forward is judged against the policies in force on it.
    execution_role(&router).await;

    cloudformation(
        &router,
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template())],
    )
    .await;
    let (status, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "expected the pipeline to deploy, got {status} with {described}"
    );
    let outputs = outputs_of(&described);
    let output = |name: &str| {
        outputs.get(name).cloned().unwrap_or_else(|| {
            panic!("expected the stack to publish {name}, got {described}")
        })
    };
    let sink_url = output("SinkUrl");
    let quiet_url = output("QuietUrl");
    let rule_arn = output("RuleArn");
    let function_arn = output("FunctionArn");

    // The control plane the declaration promised, read exactly where a client reads it: the
    // rule's targets are the one function ARN, and the function's own document grants this
    // service the invocation scoped to that rule.
    let listed = events(
        &router,
        "the deployed control plane",
        "ListTargetsByRule",
        &json!({ "Rule": enabled_rule() }),
    )
    .await;
    let targets = listed["Targets"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        targets.len(),
        1,
        "expected the deployed rule to report exactly its one declared target, got {listed}"
    );
    assert_eq!(
        targets[0]["Arn"],
        json!(function_arn),
        "expected the reported target to be the function ARN {function_arn:?}, got {listed}"
    );

    let (policy_status, policy) = lambda(
        &router,
        "GET",
        &format!("/2015-03-31/functions/{STACK}-forwarder/policy"),
        None,
    )
    .await;
    assert_eq!(
        policy_status, 200,
        "expected HTTP 200 from GetPolicy, got {policy_status} with {policy}"
    );
    let document: Value = serde_json::from_str(policy["Policy"].as_str().unwrap_or_default())
        .unwrap_or_else(|error| panic!("expected GetPolicy to answer a document, got {policy} ({error})"));
    let statements = document["Statement"].as_array().cloned().unwrap_or_default();
    let granted = statements
        .iter()
        .find(|statement| statement["Principal"]["Service"] == json!(EVENTS_PRINCIPAL))
        .unwrap_or_else(|| {
            panic!("expected a statement for {EVENTS_PRINCIPAL:?}, got {document}")
        });
    assert_eq!(
        granted["Action"],
        json!(INVOKE_ACTION),
        "expected the statement to grant {INVOKE_ACTION:?}, got {granted}"
    );
    assert_eq!(
        granted["Condition"][ARN_LIKE][SOURCE_ARN_KEY],
        json!(rule_arn),
        "expected the statement to be scoped to the rule ARN {rule_arn:?}, got {granted}"
    );

    // Nothing is published until both targets are invocable: "During this time, you can't invoke
    // or modify the function" (`CreateFunction`), so a handover withheld before that would be the
    // lifecycle's refusal rather than the routing's.
    wait_until_active(&router, &format!("{STACK}-forwarder")).await;
    wait_until_active(&router, &format!("{STACK}-idle")).await;

    // One request carrying every row, so what each target is handed is decided by the entry and
    // not by when it was published. The identifier the answer gives each entry is what ties a
    // forwarded envelope back to its row.
    let rows = rows();
    let entries: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "Source": row.source,
                "DetailType": row.detail_type,
                "Resources": row.resources,
                "Detail": row.detail.to_string(),
            })
        })
        .collect();
    let answered = events(
        &router,
        "the publication",
        "PutEvents",
        &json!({ "Entries": entries }),
    )
    .await;
    assert_eq!(
        answered["FailedEntryCount"],
        json!(0),
        "expected every entry to be accepted, got {answered}"
    );
    let answered_entries = answered["Entries"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        answered_entries.len(),
        rows.len(),
        "expected one answered entry per published one, got {answered}"
    );
    let ids: Vec<String> = answered_entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let id = entry["EventId"].as_str().unwrap_or_default().to_string();
            assert!(
                !id.is_empty(),
                "case {:?}: expected the accepted entry to carry an EventId, got {answered}",
                rows[index].case
            );
            id
        })
        .collect();
    let mut distinct = ids.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        ids.len(),
        "expected each accepted entry to carry an identifier of its own, got {ids:?}"
    );

    // Wait for the envelope of every MATCHED row to reach the sink, looking again until they are
    // all there or the deadline passes.
    let expected: Vec<&String> = rows
        .iter()
        .zip(&ids)
        .filter(|(row, _)| row.matched)
        .map(|(_, id)| id)
        .collect();
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    let mut seen: Vec<Value> = Vec::new();
    loop {
        seen.extend(forwarded(&router, &sink_url).await);
        if expected.iter().all(|id| holds(&seen, id.as_str())) || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    // Then keep looking for the quiet budget, so an envelope no pattern matched would stand among
    // the bodies read below rather than being merely late.
    let quiet_until = Instant::now() + QUIET_DEADLINE;
    while Instant::now() < quiet_until {
        tokio::time::sleep(LOOK_INTERVAL).await;
        seen.extend(forwarded(&router, &sink_url).await);
    }

    for (row, id) in rows.iter().zip(&ids) {
        let case = row.case;
        if !row.matched {
            assert!(
                !holds(&seen, id),
                "case {case:?}: expected the entry the pattern does not name to reach no target, \
                 but the sink holds its envelope among {seen:?}"
            );
            continue;
        }
        let envelope = seen
            .iter()
            .find(|envelope| envelope["id"] == json!(id))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the envelope carrying id {id:?} to be forwarded to \
                     the sink, got {seen:?}"
                )
            });
        assert_eq!(
            envelope["version"],
            json!(ENVELOPE_VERSION),
            "case {case:?}: expected envelope version {ENVELOPE_VERSION:?}, got {envelope}"
        );
        assert_eq!(
            envelope["source"],
            json!(row.source),
            "case {case:?}: expected envelope source {:?}, got {envelope}",
            row.source
        );
        assert_eq!(
            envelope["detail-type"],
            json!(row.detail_type),
            "case {case:?}: expected envelope detail-type {:?}, got {envelope}",
            row.detail_type
        );
        assert_eq!(
            envelope["account"],
            json!(ACCOUNT),
            "case {case:?}: expected envelope account {ACCOUNT:?}, got {envelope}"
        );
        assert_eq!(
            envelope["region"],
            json!(REGION),
            "case {case:?}: expected envelope region {REGION:?}, got {envelope}"
        );
        assert_eq!(
            envelope["resources"],
            json!(row.resources),
            "case {case:?}: expected envelope resources {:?}, got {envelope}",
            row.resources
        );
        assert_eq!(
            envelope["detail"], row.detail,
            "case {case:?}: expected envelope detail {}, got {envelope}",
            row.detail
        );
        let time = envelope["time"].as_str().unwrap_or_default();
        assert!(
            time.contains('T') && time.ends_with('Z'),
            "case {case:?}: expected the envelope time to be an RFC3339 instant, got {time:?} in \
             {envelope}"
        );
    }

    // Every matched row reached the sink and nothing else did: the set of identifiers the sink
    // holds is exactly the set of matched entries.
    let mut held: Vec<String> = seen
        .iter()
        .filter_map(|envelope| envelope["id"].as_str().map(str::to_string))
        .collect();
    held.sort();
    held.dedup();
    let mut wanted: Vec<String> = expected.iter().map(|id| id.to_string()).collect();
    wanted.sort();
    assert_eq!(
        held, wanted,
        "expected the sink to hold the envelope of every matched entry and of nothing else, got \
         {seen:?}"
    );

    // The rule the declaration put in DISABLED state matched nothing, so its target never ran
    // and the queue its handler would have written to stayed empty.
    let on_quiet = stayed_quiet(&router, &quiet_url).await;
    assert!(
        on_quiet.is_empty(),
        "expected the DISABLED rule's target never to be invoked, got {on_quiet:?}"
    );
}
