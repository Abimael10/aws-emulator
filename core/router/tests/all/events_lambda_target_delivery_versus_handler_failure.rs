// SPDX-License-Identifier: CC0-1.0

//! Where an EventBridge rule's Lambda function target DELIVERY ends and the target's own RUN
//! begins — end to end over the real `MultiServiceRouter` with CloudFormation, EventBridge,
//! Lambda, Amazon SQS, CloudWatch Logs and IAM mounted on one engine.
//!
//! The rule pinned here: **a matched entry is handed to a function target by invoking it
//! ASYNCHRONOUSLY ("Event – Invoke the function asynchronously", `Invoke$InvocationType`), and
//! that handover happens exactly where the FUNCTION's own resource-based policy admits
//! `events.amazonaws.com` the `lambda:InvokeFunction` on it for the INVOKING rule ("for Lambda
//! functions … EventBridge relies on resource-based policies", `PutTargets`). The delivery is
//! complete once that invocation is ACCEPTED, so whatever the run then comes to belongs to the
//! function and not to the delivery: a handler that raises is observable in that function's own
//! log group and nothing of its event reaches the queue the target's `DeadLetterConfig` names.
//! A handover no document admits is a delivery FAILURE, and that event is written to that queue
//! carrying the bytes the target would have received plus the attributes naming the failed
//! handover — `RULE_ARN` for the rule that could not deliver, `TARGET_ARN` for the target it
//! could not invoke, and `ERROR_CODE` for EventBridge's own condition, which for an invocation
//! withheld for want of an invoke grant is the permission one.**
//!
//! Six crates have to agree for that to hold (CloudFormation archives each inline handler into
//! a deployment package this emulator can run, EventBridge matches and hands over, Lambda
//! decides the invocation against the function's policy and then RUNS the handler, Amazon SQS
//! holds both what a handler wrote and what was dead-lettered, CloudWatch Logs holds what a run
//! reported, and IAM answers for the execution role each handler's own calls are judged
//! against), so the suite lives here rather than in any one crate. Every observation is the one
//! a client makes — the success queue, the dead-letter queue and the function's log group —
//! never an emulator internal.
//!
//! The probes differ along what the rule generalises over: what the invoked handler DOES
//! (forwards the envelope, or raises), and WHY a handover cannot be made (a statement scoped by
//! `SourceArn` to another rule, and a function carrying no resource-based policy at all) — two
//! provenances for the same condition, on two different functions, so the `TARGET_ARN` and
//! `RULE_ARN` assertions are load-bearing. All four entries go out in ONE `PutEvents` request
//! against four rules sharing ONE dead-letter queue, which is what makes each probe the
//! others' control: the two dead letters and the forwarded envelope are waited for, and only
//! then is each probe's "this marker is NOT here" read off the same snapshots.
//!
//! `crate::events_lambda_target_dead_letter_capture` is the suite for what a dead-lettered
//! message carries for each failure class, and
//! `crate::events_lambda_target_permission_granted_after_the_rule` for the document being
//! re-read per delivery; this one is the suite for the boundary between a delivery EventBridge
//! could not make and a run that failed after it did.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::outputs_of;
use crate::sns_lambda_subscription_permission_boundary::{
    cloudformation, iam, lambda, sqs, trust_policy, ACCOUNT, CREATE_COMPLETE, LOOK_INTERVAL,
};

/// The Region every request here is signed for — the one the shared stack reader
/// ([`wait_for_stack_status`]) and the shared awsQuery helpers sign for.
const REGION: &str = "us-east-1";

/// The one stack this suite deploys, and the prefix every resource name takes from it.
const STACK: &str = "events-lambda-delivery-vs-run";

/// The envelope members every entry published here carries, and what each rule's pattern
/// matches on: a `source`, a `detail-type`, and the marker nested inside `detail` that tells
/// one entry's rule from another's.
const SOURCE: &str = "sim2real.cloudfront";
const DETAIL_TYPE: &str = "AWS API Call via CloudTrail";
const EVENT_NAME: &str = "CreateInvalidation";
const MARKER_MEMBER: &str = "invalidationMarker";

/// The service principal EventBridge hands a resource-policy-governed target over as, and the
/// action an invocation of a function performs — what a grant for this service has to name.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The condition EventBridge names on a dead letter for a delivery it WITHHELD because no
/// document admits the invocation, spelled here rather than imported from the service so a
/// wrong value in the service would fail this suite.
const NO_PERMISSIONS: &str = "NO_PERMISSIONS";

/// The attributes a dead-lettered record carries about the handover that failed.
const RULE_ARN_ATTRIBUTE: &str = "RULE_ARN";
const TARGET_ARN_ATTRIBUTE: &str = "TARGET_ARN";
const ERROR_CODE_ATTRIBUTE: &str = "ERROR_CODE";

/// The execution role every declared function is created with, provisioned in IAM before the
/// stack so the template names a role that service really holds.
const ROLE_NAME: &str = "events-delivery-vs-run-exec";

/// Each target's retry allowance, at the published lower bound of both members
/// (`Target$RetryPolicy` -> `RetryPolicy`: `MaximumRetryAttempts` range 0..185,
/// `MaximumEventAgeInSeconds` range 60..86400), so a handover that cannot be made is
/// dead-lettered without a retry.
const MAXIMUM_RETRY_ATTEMPTS: i64 = 0;
const MAXIMUM_EVENT_AGE_IN_SECONDS: i64 = 60;

/// The statement ids the two grants are written under.
const OWN_RULE_STATEMENT_ID: &str = "AllowEventBridgeInvokeForTheForwardingRule";
const UNSCOPED_STATEMENT_ID: &str = "AllowEventBridgeInvokeUnscoped";

/// How long a wait for work the CLOCK owes goes on — a function leaving its creation
/// transition, a delivery, a run's own log line.
///
/// A ceiling on PATIENCE, never a span anything here counts on: every wait below looks again
/// until the thing it waits for is there or this deadline passes, so a slow or loaded host is
/// waited for and only something that never arrives at all fails the test.
const WAIT_DEADLINE: Duration = Duration::from_secs(180);

/// CloudFormation, EventBridge, Lambda, Amazon SQS, CloudWatch Logs and IAM over ONE engine, as
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
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature is never checked: this router runs with request authentication
/// off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target: &str,
    json_version: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", target)
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(
        router,
        "events",
        &format!("AWSEvents.{operation}"),
        "1.1",
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// Every message the log group `group` holds right now, as a client reads them.
///
/// A group that is not there yet reads as no lines at all — which is exactly what a client
/// polling for a line that never came sees, and is the honest answer for a function nothing
/// has invoked. Any OTHER refusal is this suite's own failure rather than an observation, so
/// it is raised instead of being folded into the same emptiness: a negative read must not be
/// satisfiable by a service that did not answer.
async fn log_lines(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, answered) = rpc(
        router,
        "logs",
        "Logs_20140328.FilterLogEvents",
        "1.1",
        &json!({ "logGroupName": group }),
    )
    .await;
    if status != 200 {
        let reported = answered["__type"]
            .as_str()
            .or_else(|| answered["code"].as_str())
            .unwrap_or_default();
        assert!(
            reported.contains(LOG_GROUP_ABSENT_CODE),
            "FilterLogEvents({group:?}): expected either the lines or the absence of the \
             group, got {status} with {answered}"
        );
        return Vec::new();
    }
    answered["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|event| event["message"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The error CloudWatch Logs answers a read of a log group that is not there.
const LOG_GROUP_ABSENT_CODE: &str = "ResourceNotFoundException";

/// Create the queue `name` where Amazon SQS holds it, and answer its url with the `QueueArn`
/// read back off the queue rather than built here.
async fn given_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a QueueUrl for {name:?}, got {created}"))
        .to_string();
    let attributes = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a QueueArn for {name:?}, got {attributes}"))
        .to_string();
    (url, arn)
}

/// Let this service write dead letters onto the queue at `url`: the dead-letter write is an
/// `sqs:SendMessage` by the service principal, so the QUEUE's own policy has to admit it for
/// what reaches it to be decided by the function target's document instead.
async fn admit_dead_letter_writes(router: &MultiServiceRouter, url: &str, arn: &str) {
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    });
    sqs(
        router,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
    )
    .await;
}

/// Create the execution role the template names, where IAM holds it: trusted by Lambda, and
/// granting both the CloudWatch Logs calls a run's own log delivery makes and the Amazon SQS
/// calls the forwarding handler makes. A role short of either leaves nothing for a client to
/// read, which is not the boundary under test.
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
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "logs:CreateLogGroup",
                "logs:CreateLogStream",
                "logs:PutLogEvents",
                "sqs:GetQueueUrl",
                "sqs:SendMessage",
            ],
            "Resource": "*",
        }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", ROLE_NAME),
            ("PolicyName", "write-its-logs-and-the-success-queue"),
            ("PolicyDocument", &policy),
        ],
    )
    .await;
}

/// The handler of the FORWARDING function: it writes the whole envelope it was handed to the
/// success queue, which is where a client observes that the target really was invoked. The
/// queue is addressed by NAME through that service's own `GetQueueUrl`, so the source carries
/// nothing the deployment had to compute.
const FORWARDING_SOURCE: &str = r#"
import json
import boto3


def lambda_handler(event, context):
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName="SUCCESS_QUEUE_NAME")["QueueUrl"]
    client.send_message(QueueUrl=url, MessageBody=json.dumps(event))
    return {"forwarded": event["detail"]["MARKER_MEMBER"]}
"#;

/// The handler of the RAISING function: it reports the marker it was handed and then fails, so
/// a client can tell that the invocation was made AND that the run did not finish — the case
/// that must not be dead-lettered.
const RAISING_SOURCE: &str = r#"
def lambda_handler(event, context):
    marker = event["detail"]["MARKER_MEMBER"]
    print("handling " + marker)
    raise RuntimeError("SENTENCE " + marker)
"#;

/// The sentence the raising handler fails with, before the marker it was handed.
const RAISED_SENTENCE: &str = "simulated CreateInvalidation failure for";

/// The class the python runtime names that failure in its own error record.
const RAISED_CLASS: &str = "RuntimeError";

/// The handler of the function NO document admits. Nothing ever runs it — the probe's assertion
/// is that nothing does — so it computes nothing.
const IDLE_SOURCE: &str = r#"
def lambda_handler(event, context):
    return {"ran": True}
"#;

/// Which function a probe's rule targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// The function whose handler forwards the envelope to the success queue, and whose policy
    /// carries one statement scoped by `SourceArn` to the FORWARDING probe's rule.
    Forwarder,
    /// The function whose handler raises, and whose policy carries one unscoped statement.
    Raiser,
    /// The function carrying no resource-based policy at all.
    Idle,
}

impl Target {
    /// The logical id this function is declared under, which is also the stem of its name and
    /// of the stack outputs reporting it.
    fn logical_id(self) -> &'static str {
        match self {
            Self::Forwarder => "Forwarder",
            Self::Raiser => "Raiser",
            Self::Idle => "Idle",
        }
    }

    /// The name the template gives it.
    fn function_name(self) -> String {
        format!("{STACK}-{}", self.logical_id().to_lowercase())
    }

    /// The log group this function's runs report into: "by default, Lambda functions send logs
    /// to a default log group named `/aws/lambda/<function name>`".
    fn log_group(self) -> String {
        format!("/aws/lambda/{}", self.function_name())
    }

    /// Every function this suite deploys.
    fn each() -> [Self; 3] {
        [Self::Forwarder, Self::Raiser, Self::Idle]
    }
}

/// What must have become of one probe's entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The handover was made and the handler ran: the envelope it forwarded is on the success
    /// queue, and nothing of this entry is dead-lettered.
    Forwarded,
    /// The handover was made and the handler then RAISED: its own line and the sentence it
    /// failed with are in that function's log group, and nothing of this entry is
    /// dead-lettered — a failed run is not a failed delivery.
    Raised,
    /// The handover could NOT be made: this entry is on the dead-letter queue under the
    /// permission condition, naming the rule and the target of the handover that failed, and
    /// the target's own log group reports nothing for it.
    DeadLettered,
}

/// One probe: a rule of its own, matching on its own marker, with one function target.
struct Probe {
    case: &'static str,
    /// The `detail.invalidationMarker` the entry carries, which is also what this probe's rule
    /// pattern names and so what makes the rule match that entry and no other.
    marker: &'static str,
    target: Target,
    outcome: Outcome,
}

/// The probes, which differ along what the rule generalises over: what the invoked handler
/// does, and why a handover cannot be made.
const PROBES: &[Probe] = &[
    Probe {
        case: "the target's function grants this service the invocation for THIS rule, so the \
               handler runs and forwards the envelope",
        marker: "marker-forwarded",
        target: Target::Forwarder,
        outcome: Outcome::Forwarded,
    },
    Probe {
        case: "the target's granted handler RAISES after the asynchronous invocation was \
               accepted, which is a delivery EventBridge made",
        marker: "marker-raised",
        target: Target::Raiser,
        outcome: Outcome::Raised,
    },
    Probe {
        case: "the target's function grants the invocation only to ANOTHER rule, so this rule's \
               handover cannot be made",
        marker: "marker-scoped-elsewhere",
        target: Target::Forwarder,
        outcome: Outcome::DeadLettered,
    },
    Probe {
        case: "the target's function carries no resource-based policy at all, so the handover \
               cannot be made",
        marker: "marker-no-policy",
        target: Target::Idle,
        outcome: Outcome::DeadLettered,
    },
];

/// The template deployed once: the three functions, each with its inline handler.
///
/// The functions come from a template because that is what archives inline source into a
/// deployment package this emulator can run (`Code.ZipFile` in a template is the function's
/// SOURCE TEXT, while the API's member is the archive's bytes). Everything the rule is about —
/// the rules, their targets, the grants — is wired through each service's OWN API below, the
/// way a client wires it.
fn template(success_queue: &str) -> String {
    let function = |name: String, source: String| {
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "FunctionName": name,
                "Role": format!("arn:aws:iam::{ACCOUNT}:role/{ROLE_NAME}"),
                "Runtime": "python3.12",
                "Handler": "index.lambda_handler",
                // Generous: nothing here tests a timeout, and a run shares this process with
                // every other test of this target.
                "Timeout": 60,
                "Code": { "ZipFile": source },
            },
        })
    };
    let forwarding = FORWARDING_SOURCE
        .replace("SUCCESS_QUEUE_NAME", success_queue)
        .replace("MARKER_MEMBER", MARKER_MEMBER);
    let raising = RAISING_SOURCE
        .replace("MARKER_MEMBER", MARKER_MEMBER)
        .replace("SENTENCE", RAISED_SENTENCE);
    let mut resources = json!({});
    let mut outputs = json!({});
    for (target, source) in [
        (Target::Forwarder, forwarding),
        (Target::Raiser, raising),
        (Target::Idle, IDLE_SOURCE.to_string()),
    ] {
        let id = target.logical_id();
        resources[id] = function(target.function_name(), source);
        outputs[format!("{id}Arn")] = json!({ "Value": { "Fn::GetAtt": [id, "Arn"] } });
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": outputs,
    })
    .to_string()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports `name` `Active` — "the function
/// is ready to be invoked" (`FunctionConfiguration$State`) — so a handover withheld later is
/// the policy's refusal and not the lifecycle's. The span waited for is that service's own, so
/// nothing here sleeps a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let giving_up_at = Instant::now() + WAIT_DEADLINE;
    loop {
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
        assert!(
            Instant::now() < giving_up_at,
            "Lambda never reported {name:?} Active; it last reported {reported}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Add one statement to a deployed function's resource-based policy through `AddPermission`,
/// which answers 201 with the created `Statement`.
async fn add_permission(
    router: &MultiServiceRouter,
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
        "expected HTTP 201 from Lambda AddPermission({statement_id}) on {function:?}, got \
         {status} with {answered}"
    );
}

/// Every message now waiting on the queue at `url`, folded into `seen` under the `MessageId`
/// Amazon SQS minted for it — so a message a later look sees again is the same one and not a
/// second delivery.
async fn collect_messages(
    router: &MultiServiceRouter,
    url: &str,
    seen: &mut BTreeMap<String, Value>,
) {
    let answered = sqs(
        router,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "MessageAttributeNames": ["All"],
        }),
    )
    .await;
    for message in answered["Messages"].as_array().cloned().unwrap_or_default() {
        let id = message["MessageId"].as_str().unwrap_or_default().to_string();
        seen.insert(id, message);
    }
}

/// The marker the record `message` carries, read out of its body the way a consumer reads it:
/// `None` for a body that is not the envelope at all.
fn marker_of(message: &Value) -> Option<String> {
    let body = message["Body"].as_str()?;
    let envelope: Value = serde_json::from_str(body).ok()?;
    envelope
        .pointer(&format!("/detail/{MARKER_MEMBER}"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Every record of `messages` carrying `marker`.
fn carrying<'a>(messages: &'a BTreeMap<String, Value>, marker: &str) -> Vec<&'a Value> {
    messages
        .values()
        .filter(|message| marker_of(message).as_deref() == Some(marker))
        .collect()
}

/// The `StringValue` of the dead letter's `name` attribute.
fn attribute(message: &Value, name: &str) -> String {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Assert the envelope `message` carries is the one published for `marker`: a target is handed
/// the whole envelope, so what a client reads back is the entry's own `source`, `detail-type`
/// and `detail`.
fn assert_is_the_published_envelope(case: &str, message: &Value, marker: &str) {
    let body = message["Body"].as_str().unwrap_or_default();
    let envelope: Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("case {case:?}: the body is not JSON ({error}): {body:?}"));
    let marker_pointer = format!("/detail/{MARKER_MEMBER}");
    for (pointer, expected) in [
        ("/source", json!(SOURCE)),
        ("/detail-type", json!(DETAIL_TYPE)),
        ("/detail/eventName", json!(EVENT_NAME)),
        (marker_pointer.as_str(), json!(marker)),
    ] {
        let found = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            found, expected,
            "case {case:?}: expected the carried envelope's {pointer} to be {expected}, got \
             {found} in {envelope}"
        );
    }
    assert!(
        envelope["id"].as_str().is_some_and(|id| !id.is_empty()),
        "case {case:?}: expected the carried envelope to name the event's id, got {envelope}"
    );
}

/// What this test has observed so far, accumulated across looks.
#[derive(Debug, Default)]
struct Observed {
    /// The records the handler's own writes put on the success queue.
    forwarded: BTreeMap<String, Value>,
    /// The records EventBridge dead-lettered.
    dead_lettered: BTreeMap<String, Value>,
    /// Every line each function's own log group has reported, keyed by that function's logical
    /// id — so what one function's runs reported is never read as another's.
    log_lines: BTreeMap<&'static str, Vec<String>>,
}

impl Observed {
    /// Every line `target`'s own log group has reported.
    fn lines_of(&self, target: Target) -> &[String] {
        self.log_lines
            .get(target.logical_id())
            .map_or(&[], Vec::as_slice)
    }

    /// Whether every probe's OWN expectation is already satisfied, which is the condition the
    /// waiting below looks for: once it holds, every negative read afterwards is read off
    /// snapshots that a wrong delivery would already stand in.
    fn every_expectation_met(&self) -> bool {
        PROBES.iter().all(|probe| match probe.outcome {
            Outcome::Forwarded => !carrying(&self.forwarded, probe.marker).is_empty(),
            Outcome::Raised => {
                let lines = self.lines_of(probe.target);
                lines.iter().any(|line| line.contains(probe.marker))
                    && lines.iter().any(|line| line.contains(RAISED_SENTENCE))
            }
            Outcome::DeadLettered => !carrying(&self.dead_lettered, probe.marker).is_empty(),
        })
    }
}

/// One `PutEvents` request, four rules, one dead-letter queue: the delivery each rule could
/// make and the run each delivery then started, read where a client reads them.
#[tokio::test]
async fn a_function_targets_own_run_failure_is_not_a_delivery_failure() {
    let router = build_router();
    execution_role(&router).await;

    // The two queues a client observes on, created where Amazon SQS holds them. The success
    // queue is written by a HANDLER under the execution role, so its own policy decides
    // nothing; the dead-letter queue is written by this service, so it has to admit that.
    let success_name = format!("{STACK}-success");
    let (success_url, _) = given_queue(&router, &success_name).await;
    let (dead_letter_url, dead_letter_arn) =
        given_queue(&router, &format!("{STACK}-dlq")).await;
    admit_dead_letter_writes(&router, &dead_letter_url, &dead_letter_arn).await;

    cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template(&success_name)),
        ],
    )
    .await;
    let (status, described) = wait_for_stack_status(&router, STACK, CREATE_COMPLETE).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "expected the stack to deploy, got {status} with {described}"
    );
    let outputs = outputs_of(&described);
    let function_arn = |target: Target| {
        let name = format!("{}Arn", target.logical_id());
        outputs.get(&name).cloned().unwrap_or_else(|| {
            panic!("expected the stack to publish {name}, got {described}")
        })
    };

    // Every function has to be invocable before anything is published: "During this time, you
    // can't invoke or modify the function" (`CreateFunction`), so a handover withheld here
    // would be the lifecycle's refusal rather than the policy's.
    for target in [Target::Forwarder, Target::Raiser, Target::Idle] {
        wait_until_active(&router, &target.function_name()).await;
    }

    // One rule per probe, each matching on its own marker, so one published entry reaches
    // exactly one rule.
    let mut rule_arns: BTreeMap<&str, String> = BTreeMap::new();
    for probe in PROBES {
        let rule_name = format!("{STACK}-{}", probe.marker);
        let rule = events(
            &router,
            "PutRule",
            &json!({
                "Name": rule_name,
                "State": "ENABLED",
                "EventPattern": json!({
                    "source": [SOURCE],
                    "detail-type": [DETAIL_TYPE],
                    "detail": { MARKER_MEMBER: [probe.marker] },
                })
                .to_string(),
            }),
        )
        .await;
        let arn = rule["RuleArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {:?}: expected a RuleArn, got {rule}", probe.case)
            })
            .to_string();
        rule_arns.insert(probe.marker, arn);
    }

    // The two grants, written on the FUNCTIONS: one scoped by `SourceArn` to the forwarding
    // probe's rule — which therefore admits that rule's handover and no other rule's, however
    // many rules target the same function — and one unscoped on the raising function. The idle
    // function is granted nothing at all.
    let forwarding_probe = PROBES
        .iter()
        .find(|probe| probe.outcome == Outcome::Forwarded)
        .expect("one probe whose handover is admitted");
    let forwarding_rule_arn = rule_arns[forwarding_probe.marker].clone();
    add_permission(
        &router,
        &Target::Forwarder.function_name(),
        OWN_RULE_STATEMENT_ID,
        Some(&forwarding_rule_arn),
    )
    .await;
    add_permission(
        &router,
        &Target::Raiser.function_name(),
        UNSCOPED_STATEMENT_ID,
        None,
    )
    .await;

    // Each rule's single target: the probe's function, the shared dead-letter queue, and the
    // retry allowance above.
    for probe in PROBES {
        let wired = events(
            &router,
            "PutTargets",
            &json!({
                "Rule": format!("{STACK}-{}", probe.marker),
                "Targets": [{
                    "Id": "fn",
                    "Arn": function_arn(probe.target),
                    "DeadLetterConfig": { "Arn": dead_letter_arn },
                    "RetryPolicy": {
                        "MaximumRetryAttempts": MAXIMUM_RETRY_ATTEMPTS,
                        "MaximumEventAgeInSeconds": MAXIMUM_EVENT_AGE_IN_SECONDS,
                    },
                }],
            }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {:?}: expected the target to be accepted, got {wired}",
            probe.case
        );
    }

    // One request carrying one entry per probe. Every entry is ACCEPTED whatever its delivery
    // then comes to: "FailedEntryCount — the number of failed entries", which a
    // matched-but-undeliverable target is not one of.
    let entries: Vec<Value> = PROBES
        .iter()
        .map(|probe| {
            json!({
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": json!({
                    "eventName": EVENT_NAME,
                    MARKER_MEMBER: probe.marker,
                })
                .to_string(),
            })
        })
        .collect();
    let published = events(&router, "PutEvents", &json!({ "Entries": entries })).await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "expected every entry to be accepted, got {published}"
    );
    let answered = published["Entries"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        answered.len(),
        PROBES.len(),
        "expected one answered entry per probe, got {published}"
    );
    for (probe, entry) in PROBES.iter().zip(answered.iter()) {
        assert!(
            entry["EventId"].as_str().is_some_and(|id| !id.is_empty()),
            "case {:?}: expected the accepted entry to carry an EventId, got {entry}",
            probe.case
        );
    }

    // Look again until every probe's own expectation holds, or the deadline passes.
    let mut observed = Observed::default();
    let giving_up_at = Instant::now() + WAIT_DEADLINE;
    loop {
        collect_messages(&router, &success_url, &mut observed.forwarded).await;
        collect_messages(&router, &dead_letter_url, &mut observed.dead_lettered).await;
        for target in Target::each() {
            let reported = log_lines(&router, &target.log_group()).await;
            observed.log_lines.insert(target.logical_id(), reported);
        }
        if observed.every_expectation_met() || Instant::now() >= giving_up_at {
            break;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    for probe in PROBES {
        let case = probe.case;
        let marker = probe.marker;
        let target_arn = function_arn(probe.target);
        let rule_arn = rule_arns[marker].clone();
        let forwarded = carrying(&observed.forwarded, marker);
        let dead_lettered = carrying(&observed.dead_lettered, marker);
        let reported = observed.lines_of(probe.target);
        let group = probe.target.log_group();
        match probe.outcome {
            Outcome::Forwarded => {
                assert_eq!(
                    forwarded.len(),
                    1,
                    "case {case:?}: expected the invoked handler to forward the envelope \
                     carrying {marker:?} to the success queue exactly once, got {forwarded:?}"
                );
                assert_is_the_published_envelope(case, forwarded[0], marker);
                assert!(
                    dead_lettered.is_empty(),
                    "case {case:?}: expected a delivery that was MADE to dead-letter nothing, \
                     got {dead_lettered:?}"
                );
            }
            Outcome::Raised => {
                // The invocation was accepted and the run then failed: both halves are
                // readable in the function's own log group, and neither is a delivery failure.
                assert!(
                    reported.iter().any(|line| line.contains(marker)),
                    "case {case:?}: expected the raising function's log group {group:?} to \
                     report the marker {marker:?} it was handed, got {reported:?}"
                );
                let failure = format!("{RAISED_SENTENCE} {marker}");
                assert!(
                    reported.iter().any(|line| line.contains(&failure)),
                    "case {case:?}: expected the log group to report the sentence the handler \
                     failed with ({failure:?}), got {reported:?}"
                );
                assert!(
                    reported.iter().any(|line| line.contains(RAISED_CLASS)),
                    "case {case:?}: expected the log group to name the class of the raised \
                     value ({RAISED_CLASS}), got {reported:?}"
                );
                assert!(
                    dead_lettered.is_empty(),
                    "case {case:?}: expected a handler's own failure NEVER to be dead-lettered, \
                     got {dead_lettered:?}"
                );
            }
            Outcome::DeadLettered => {
                assert_eq!(
                    dead_lettered.len(),
                    1,
                    "case {case:?}: expected the handover that could not be made to be \
                     dead-lettered exactly once, got {dead_lettered:?}"
                );
                let record = dead_lettered[0];
                assert_is_the_published_envelope(case, record, marker);
                for (name, expected) in [
                    (RULE_ARN_ATTRIBUTE, rule_arn.clone()),
                    (TARGET_ARN_ATTRIBUTE, target_arn.clone()),
                    (ERROR_CODE_ATTRIBUTE, NO_PERMISSIONS.to_string()),
                ] {
                    let found = attribute(record, name);
                    assert_eq!(
                        found, expected,
                        "case {case:?}: expected the dead letter's {name} to be \
                         {expected:?}, got {found:?} in {record}"
                    );
                }
                assert!(
                    forwarded.is_empty(),
                    "case {case:?}: expected a handover no document admits to run nothing, so \
                     nothing of it reaches the success queue, got {forwarded:?}"
                );
                // And the function itself never ran for this entry: its own log group reports
                // no line carrying this marker. Read against the same snapshot in which the
                // admitted probes' lines DO stand, so it is the invocation that was withheld
                // and not the looking that was early.
                assert!(
                    !reported.iter().any(|line| line.contains(marker)),
                    "case {case:?}: expected the target no document admits never to be \
                     invoked, but its log group {group:?} reports {marker:?}: {reported:?}"
                );
            }
        }
    }
}
