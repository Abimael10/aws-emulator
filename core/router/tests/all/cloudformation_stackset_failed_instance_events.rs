// SPDX-License-Identifier: CC0-1.0

//! What a stack-set operation publishes onto the account's default EventBridge bus when the
//! stack one of its instances deploys FAILS, observed where a client observes it: on the
//! Amazon SQS queue a rule on that bus delivers to.
//!
//! The rule pinned here: **a stack-set operation performs no work of its own — "a stack
//! instance refers to a stack in a specific account and Region" — so its terminal status is
//! what became of the stacks its instances deploy: an operation one of whose instances' stacks
//! did not carry the deployment through concludes FAILED, that instance is "OUTDATED: the
//! stack isn't currently up to date with the StackSet because the associated stack failed
//! during a CreateStackSet or UpdateStackSet operation" with detailed status FAILED and an
//! explanation naming the RESOURCE of that stack the deployment could not carry through, and
//! all of it is published — the operation's own RUNNING and then FAILED status under
//! "CloudFormation StackSet Operation Status Change", and the instance's status, detailed
//! status and explanation under "CloudFormation StackSet StackInstance Status Change",
//! carrying the instance's own stack id.** The explanation the event states is the one
//! `ListStackInstances` reports, so a client that polls and one that only listens cannot
//! disagree.
//!
//! Why it matters: an alarm on a failed rollout, and a pipeline gating on one, never fire
//! unless the operation really concludes and says so on the bus — and a failure nothing
//! explains is indistinguishable from a rollout still going out.
//!
//! What this suite OWNS, beside `cloudformation_stackset_status_change_events.rs`: that suite
//! pins the lifecycle of a stack set whose instances DEPLOY (ACTIVE, RUNNING -> SUCCEEDED,
//! CURRENT/SUCCEEDED, DELETED). This one pins the other outcome of the same rule — the
//! operation and the instance a FAILED deployment leaves, and the explanation that failure
//! carries — which that suite never reaches.
//!
//! The rows differ along WHERE the failure comes from, which is what the rule generalises
//! over: an `AWS::CloudFormation::WaitCondition` nothing signals, which the CLOCK fails after
//! `CreateStackInstances` has answered, and a resource the owning service REFUSES, which the
//! deploying call's own walk finds. The recorded case is the first row.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element};

/// The Region every call below is signed for, which is the Region the stack sets of this
/// suite belong to, the Region their instances are deployed into and the Region their events
/// are published in.
pub(crate) const REGION: &str = "us-west-2";

/// The account this emulator serves, which is the account the events carry and the account
/// the stack instances are deployed into.
pub(crate) const ACCOUNT: &str = "000000000000";

/// The `source` every event CloudFormation publishes carries.
pub(crate) const SOURCE: &str = "aws.cloudformation";

/// The two `detail-type` values this suite reads.
pub(crate) const OPERATION_CHANGE: &str = "CloudFormation StackSet Operation Status Change";
pub(crate) const INSTANCE_CHANGE: &str = "CloudFormation StackSet StackInstance Status Change";

/// The statuses asserted on, as the model spells them.
pub(crate) const RUNNING: &str = "RUNNING";
pub(crate) const SUCCEEDED: &str = "SUCCEEDED";
pub(crate) const FAILED: &str = "FAILED";
const STOPPED: &str = "STOPPED";
const OUTDATED: &str = "OUTDATED";

/// The `detail.action` of the operation each row drives.
pub(crate) const CREATE: &str = "CREATE";

/// How long a poll or a drain waits for something the clock owes, and how often it looks.
///
/// The budget is far longer than any span this emulator gives these transitions, so a loaded
/// host cannot fail a row: what every row asserts is the CONDITION — a terminal status, an
/// event delivered — never the time it took.
pub(crate) const BUDGET: Duration = Duration::from_secs(60);
/// See [`BUDGET`].
pub(crate) const LOOK_INTERVAL: Duration = Duration::from_millis(25);

/// The whole emulator over one engine, exactly as the binary assembles it: which service
/// claims each call — CloudFormation's own in-process `PutEvents` and EventBridge's delivery
/// into Amazon SQS included — is decided by the production mount.
pub(crate) fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` and `region`, which is what
/// routes the request. The signature is never checked here.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call, as its status and XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(String, String)],
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
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name, REGION))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name, REGION))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One `(key, value)` request member, owned so a row can build names like `Accounts.member.1`.
pub(crate) fn member(key: &str, value: &str) -> (String, String) {
    (key.to_string(), value.to_string())
}

/// One CloudFormation call that must answer 200.
pub(crate) async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(String, String)],
) -> String {
    let (status, body) = query(router, "cloudformation", "2010-05-15", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with {body}"
    );
    body
}

/// One EventBridge call that must answer 200.
pub(crate) async fn events(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SQS call that must answer 200.
pub(crate) async fn sqs(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A queue whose own policy admits the delivering service, and a rule on the default bus that
/// delivers every event matching `pattern` to it: the queue's url.
///
/// "For Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge relies on
/// resource-based policies" (`PutTargets`), so the queue carries the statement that admits
/// `events.amazonaws.com`; the rule is created BEFORE the stack set, since an event published
/// while the bus carries no matching rule reaches no target.
pub(crate) async fn wired_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    pattern: &Value,
) -> String {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
        .to_string();
    let arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": arn,
        }],
    });
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
    )
    .await;
    events(
        router,
        case,
        "PutRule",
        &json!({ "Name": format!("{name}-rule"), "EventPattern": pattern.to_string() }),
    )
    .await;
    let targeted = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": format!("{name}-rule"),
            "Targets": [{ "Id": "queue", "Arn": arn }],
        }),
    )
    .await;
    assert_eq!(
        targeted["FailedEntryCount"].as_i64(),
        Some(0),
        "case {case:?}: the queue target must be accepted, got {targeted}"
    );
    url
}

/// Every event body waiting on the queue at `url`, parsed, and taken off the queue.
pub(crate) async fn drain(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|message| message["Body"].as_str())
        .map(|body| serde_json::from_str(body).unwrap_or(Value::Null))
        .collect()
}

/// Drain the queue at `url` into `seen` until `enough` is satisfied or [`BUDGET`] runs out.
pub(crate) async fn collect_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    seen: &mut Vec<Value>,
    enough: impl Fn(&[Value]) -> bool,
) {
    let deadline = Instant::now() + BUDGET;
    loop {
        seen.extend(drain(router, case, url).await);
        if enough(seen) || Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The instant the event carries, compared as TEXT where this suite orders two events: the
/// envelope's `time` is one fixed-width RFC 3339 form for every event, so lexicographic order
/// over it is chronological order.
pub(crate) fn time_of(event: &Value) -> &str {
    event["time"].as_str().unwrap_or_default()
}

/// The status whatever changed now carries, which each of these events states under
/// `status-details`.
pub(crate) fn status_of(event: &Value) -> &str {
    event["detail"]["status-details"]["status"]
        .as_str()
        .unwrap_or_default()
}

/// The one operation event about `operation_id` reporting `status`, or `None`.
pub(crate) fn operation_event<'a>(
    seen: &'a [Value],
    operation_id: &str,
    status: &str,
) -> Option<&'a Value> {
    seen.iter().find(|event| {
        event["detail-type"] == OPERATION_CHANGE
            && event["detail"]["stack-set-operation-id"] == json!(operation_id)
            && status_of(event) == status
    })
}

/// The one instance event about the stack `stack_id`, or `None`.
fn instance_event<'a>(seen: &'a [Value], stack_id: &str) -> Option<&'a Value> {
    seen.iter().find(|event| {
        event["detail-type"] == INSTANCE_CHANGE && event["detail"]["stack-id"] == json!(stack_id)
    })
}

/// What every one of these events must carry, whichever change it reports.
pub(crate) fn assert_envelope(case: &str, event: &Value, stack_set_arn: &str) {
    for (name, expected) in [
        ("source", json!(SOURCE)),
        ("account", json!(ACCOUNT)),
        ("region", json!(REGION)),
        ("resources", json!([stack_set_arn])),
    ] {
        assert_eq!(
            event[name], expected,
            "case {case:?}: expected {name} {expected}, got {event}"
        );
    }
    assert_eq!(
        event["detail"]["stack-set-arn"],
        json!(stack_set_arn),
        "case {case:?}: expected detail.stack-set-arn {stack_set_arn:?}, got {event}"
    );
    for name in ["version", "id", "time"] {
        assert!(
            !event[name].as_str().unwrap_or_default().is_empty(),
            "case {case:?}: the envelope must carry a {name}, got {event}"
        );
    }
}

/// The pattern of a rule that observes EVERY stack-set operation and instance event of this
/// service.
pub(crate) fn every_stack_set_event() -> Value {
    json!({
        "source": [SOURCE],
        "detail-type": [OPERATION_CHANGE, INSTANCE_CHANGE],
    })
}

/// The template of the stack set whose instance's stack fails at a wait condition nothing
/// signals, shared with the suite that pins the FAILED-only rule over the same failure.
///
/// The wait OUTLASTS the span this emulator gives a transition, so an operation that concluded
/// on its own clock would publish the success it was headed for while the instance's stack was
/// still waiting — which is the recorded case, and what makes the FAILED conclusion below
/// evidence that the operation waited for the stack it deployed.
pub(crate) fn unsignalled_wait_template() -> String {
    json!({"Resources": {
        "Handle": {"Type": "AWS::CloudFormation::WaitConditionHandle"},
        "Wait": {
            "Type": "AWS::CloudFormation::WaitCondition",
            "Properties": {"Handle": {"Ref": "Handle"}, "Timeout": "5", "Count": "1"},
        },
    }})
    .to_string()
}

/// One row: the stack set, the template its instance deploys, and the resource whose failure
/// the explanation must name.
struct Row {
    case: &'static str,
    stack_set: &'static str,
    template: String,
    /// The logical id and the type of the resource the deployment could not carry through.
    names: (&'static str, &'static str),
    /// A fragment of the diagnosis the explanation must also carry, for a row whose failure
    /// carries one of its own; `None` where the owning service's wording is that service's to
    /// choose and this suite asserts only the resource the explanation names.
    diagnosis: Option<&'static str>,
}

/// The class the rule covers: every way the stack an instance deploys can fail.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a wait condition nothing signals, which the clock fails",
            stack_set: "events-instance-wait",
            template: unsignalled_wait_template(),
            names: ("Wait", "AWS::CloudFormation::WaitCondition"),
            diagnosis: Some("WaitCondition timed out"),
        },
        Row {
            case: "a resource the owning service refuses, which the deploying call finds",
            stack_set: "events-instance-refused",
            // A FIFO queue whose name does not carry the suffix Amazon SQS requires.
            template: json!({"Resources": {"Subject": {
                "Type": "AWS::SQS::Queue",
                "Properties": {
                    "QueueName": "events-instance-refused-subject",
                    "FifoQueue": true,
                },
            }}})
            .to_string(),
            names: ("Subject", "AWS::SQS::Queue"),
            diagnosis: None,
        },
    ]
}

/// Poll `DescribeStackSetOperation` until the operation reaches a terminal status: the body
/// that reported it.
pub(crate) async fn settled_operation(
    router: &MultiServiceRouter,
    case: &str,
    stack_set: &str,
    operation_id: &str,
) -> String {
    let deadline = Instant::now() + BUDGET;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let body = cloudformation(
            router,
            case,
            "DescribeStackSetOperation",
            &[
                member("StackSetName", stack_set),
                member("OperationId", operation_id),
            ],
        )
        .await;
        let reported = opt_element(&body, "Status").unwrap_or_default();
        if reported == SUCCEEDED || reported == FAILED || reported == STOPPED {
            return body;
        }
        if seen.last().map(String::as_str) != Some(reported.as_str()) {
            seen.push(reported);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the operation to reach a terminal status, saw {seen:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// A SELF_MANAGED stack set created from `template` — with the administration and execution
/// roles such a stack set names — and ONE stack instance deployed into the caller's own
/// account and Region: the stack set's ARN and the `OperationId` that deployment answered,
/// which is what a client monitors and what joins its events to the call.
pub(crate) async fn deployed_stack_set(
    router: &MultiServiceRouter,
    case: &str,
    stack_set: &str,
    template: &str,
) -> (String, String) {
    let created = cloudformation(
        router,
        case,
        "CreateStackSet",
        &[
            member("StackSetName", stack_set),
            member("TemplateBody", template),
            member("PermissionModel", "SELF_MANAGED"),
            member(
                "AdministrationRoleARN",
                &format!("arn:aws:iam::{ACCOUNT}:role/{stack_set}-admin"),
            ),
            member("ExecutionRoleName", "StackSetExecutionRole"),
        ],
    )
    .await;
    let stack_set_id = opt_element(&created, "StackSetId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateStackSet answered no id: {created}"));
    let stack_set_arn = format!("arn:aws:cloudformation:{REGION}:{ACCOUNT}:stackset/{stack_set_id}");
    let deployed = cloudformation(
        router,
        case,
        "CreateStackInstances",
        &[
            member("StackSetName", stack_set),
            member("Accounts.member.1", ACCOUNT),
            member("Regions.member.1", REGION),
        ],
    )
    .await;
    let operation_id = opt_element(&deployed, "OperationId").unwrap_or_else(|| {
        panic!("case {case:?}: CreateStackInstances answered no OperationId: {deployed}")
    });
    (stack_set_arn, operation_id)
}

/// The whole rule, over every row of the table.
#[tokio::test]
async fn a_failed_instance_stack_fails_its_operation_and_says_so_on_the_bus() {
    for row in rows() {
        let case = row.case;
        // A fresh emulator per row, so one row's events never decide another's.
        let router = router();
        let queue = wired_queue(&router, case, row.stack_set, &every_stack_set_event()).await;
        let (stack_set_arn, operation_id) =
            deployed_stack_set(&router, case, row.stack_set, &row.template).await;

        // 1. The operation reaches a terminal status, and it is the one its instance's stack
        // reached: FAILED, never the success it was headed for.
        let settled = settled_operation(&router, case, row.stack_set, &operation_id).await;
        let reported = opt_element(&settled, "Status").unwrap_or_default();
        assert_eq!(
            reported, FAILED,
            "case {case:?}: expected the operation to conclude {FAILED}, got {settled}"
        );

        // 2. What the instance reports, read the way a client reads it.
        let listed = cloudformation(
            &router,
            case,
            "ListStackInstances",
            &[member("StackSetName", row.stack_set)],
        )
        .await;
        let stack_id = opt_element(&listed, "StackId")
            .unwrap_or_else(|| panic!("case {case:?}: the instance reports no StackId: {listed}"));
        let instance = (
            opt_element(&listed, "Status").unwrap_or_default(),
            opt_element(&listed, "DetailedStatus").unwrap_or_default(),
        );
        assert_eq!(
            (instance.0.as_str(), instance.1.as_str()),
            (OUTDATED, FAILED),
            "case {case:?}: expected the instance to report {OUTDATED}/{FAILED}, got {listed}"
        );
        let listed_reason = elements(&listed, "StatusReason")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: the instance explains nothing: {listed}"));
        let (logical_id, resource_type) = row.names;
        for stated in [Some(logical_id), Some(resource_type), row.diagnosis]
            .into_iter()
            .flatten()
        {
            assert!(
                listed_reason.contains(stated),
                "case {case:?}: expected the explanation to carry {stated:?}, got \
                 {listed_reason:?}"
            );
        }

        // 3. ...and what a client that never polls sees on its queue: the operation's own two
        // statuses, and the instance's.
        let mut seen: Vec<Value> = Vec::new();
        collect_until(&router, case, &queue, &mut seen, |seen| {
            operation_event(seen, &operation_id, RUNNING).is_some()
                && operation_event(seen, &operation_id, FAILED).is_some()
                && instance_event(seen, &stack_id).is_some()
        })
        .await;

        let running = operation_event(&seen, &operation_id, RUNNING)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected an {OPERATION_CHANGE:?} reporting {RUNNING} for \
                     {operation_id}: {seen:?}"
                )
            })
            .clone();
        let concluded = operation_event(&seen, &operation_id, FAILED)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected an {OPERATION_CHANGE:?} reporting {FAILED} for \
                     {operation_id}: {seen:?}"
                )
            })
            .clone();
        let instance_change = instance_event(&seen, &stack_id)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected an {INSTANCE_CHANGE:?} for the instance's stack \
                     {stack_id}: {seen:?}"
                )
            })
            .clone();
        assert!(
            operation_event(&seen, &operation_id, SUCCEEDED).is_none(),
            "case {case:?}: no event may report a success for a deployment that failed: {seen:?}"
        );

        for event in [&running, &concluded, &instance_change] {
            assert_envelope(case, event, &stack_set_arn);
        }
        for event in [&running, &concluded] {
            assert_eq!(
                event["detail"]["action"],
                json!(CREATE),
                "case {case:?}: expected detail.action {CREATE}, got {event}"
            );
        }
        assert!(
            time_of(&running) <= time_of(&concluded),
            "case {case:?}: the start cannot be stamped after the conclusion, got {} and {}",
            time_of(&running),
            time_of(&concluded)
        );

        // The instance's event states exactly what the instance reports — one explanation,
        // whether a client polls or listens.
        assert_eq!(
            status_of(&instance_change),
            OUTDATED,
            "case {case:?}: expected the instance event to report {OUTDATED}, got \
             {instance_change}"
        );
        assert_eq!(
            instance_change["detail"]["status-details"]["detailed-status"],
            json!(FAILED),
            "case {case:?}: expected detailed-status {FAILED}, got {instance_change}"
        );
        assert_eq!(
            instance_change["detail"]["status-details"]["status-reason"],
            json!(listed_reason),
            "case {case:?}: the event and ListStackInstances must report ONE explanation, got \
             {instance_change}"
        );
    }
}
