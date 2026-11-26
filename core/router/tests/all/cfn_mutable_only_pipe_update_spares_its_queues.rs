// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack UPDATE that restates ONE mutable member of an
//! `AWS::Pipes::Pipe` does to the pipe and to the stack's own sibling queues.
//!
//! RULE: an `UpdateStack` that restates only properties the owning service's own update API
//! takes is an IN-PLACE update — the resource keeps its physical resource id, the change is
//! applied through that API, and the resource goes on being served — and a sibling resource
//! whose declared properties the revision does not change at all is not touched: an unnamed
//! `AWS::SQS::Queue` created by the same stack keeps the queue url it was created under, so
//! the url a client read from `Ref`/`Outputs`/`DescribeStackResources` before the update still
//! serves `SendMessage`, `ReceiveMessage` and `GetQueueAttributes` after it. For the pipe that
//! means EventBridge Pipes reports the same `Arn` and `CreationTime`, `CurrentState` `RUNNING`
//! and the member the revision restated at its new value, with every other member as it was —
//! and the configuration is live on the poller, not merely reported: the filter in force after
//! the update decides which records reach the target, and the records it rejects are consumed
//! from the source and never delivered.
//!
//! Why it matters: a client that cannot evolve a pipe's filter through CloudFormation cannot
//! roll a filter change out at all, and a queue url that stops resolving after an unrelated
//! in-place update is a 400 `AWS.SimpleQueueService.NonExistentQueue` for a queue the service
//! still holds — which silently breaks every producer and consumer the stack published it to,
//! and would destroy the records still in it.
//!
//! Four crates have to agree for this to hold — CloudFormation plans and applies the update,
//! EventBridge Pipes applies the pipe's change and runs the poller, Amazon SQS holds the source
//! and the target, IAM holds the execution role — so the suite lives here rather than in any
//! one of them.
//! `core/router/tests/all/cloudformation_pipe_update_rollback_restores_the_working_role.rs` is
//! the FAILING side of the same relation (an update the pipe cannot stabilise rolls the stack
//! back), and `core/router/tests/all/pipes_filtered_sqs_delivery.rs` is the table of which
//! records a pattern admits.
//!
//! The rows differ along what the rule generalises over: WHICH mutable member of the pipe the
//! revision restates — the source filter's pattern, the pipe's description, the batch size its
//! Amazon SQS source is polled under — each of which is a member `UpdatePipe` takes, and none
//! of which the two sibling queues declare anything about. Every row restates exactly one of
//! the three and asserts all three, so a row also proves the two it left alone stayed.
//!
//! `LastModifiedTime` is asserted MONOTONE (never earlier than the value before the update,
//! never earlier than `CreationTime`) rather than strictly later: that service reports its
//! instants at whole-second granularity, so an update applied inside the same second as the
//! create reports the same number, and which second a run lands in is the host clock's to
//! decide. Every wait below looks again until the condition it needs holds, on a generous
//! deadline; nothing is asserted about elapsed time.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{form_encode, opt_element, outputs_of, physical_ids_of};

/// The Region every row deploys in.
const REGION: &str = "us-east-1";

/// How long a stack operation is given to reach a terminal status.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long work the pipe has to do on its own is given before a row fails. Far above the
/// poller's own interval and the span a pipe takes to come to rest.
const BUDGET: Duration = Duration::from_secs(30);

/// How long an assertion that a record is NEVER delivered watches for, once everything the
/// filter admits has arrived and the source has drained. A delivery that was going to be made
/// would have been by then: this is many times the poller's own interval.
const QUIET: Duration = Duration::from_secs(2);

/// How often a stack, a queue or a pipe is looked at again while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// `TargetParameters.InputTemplate`, so each delivered message carries the source record's own
/// body and a delivery can be attributed to the record that caused it.
const BODY_TEMPLATE: &str = "<$.body>";

/// The logical ids every row declares its resources under.
const SOURCE_QUEUE: &str = "SourceQueue";
const TARGET_QUEUE: &str = "TargetQueue";
const PIPE: &str = "ThePipe";

/// The statuses this suite reads off a stack.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// What the state EventBridge Pipes reports a pipe that is moving records in is called.
const RUNNING: &str = "RUNNING";

/// The two attributes a queue's outstanding work is counted by: a record the pipe has taken is
/// invisible rather than gone, so a source that has really drained reports zero in BOTH.
const DEPTH_ATTRIBUTES: [&str; 2] = [
    "ApproximateNumberOfMessages",
    "ApproximateNumberOfMessagesNotVisible",
];

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly as
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
    router.register_service(service_com_amazonaws_pipes_pipes::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// One Amazon SQS call over its `awsJson1.0` wire.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// `DescribePipe` over EventBridge Pipes' own `restJson1` wire, on the URI that operation's
/// `@http` binding declares (`GET /v1/pipes/{Name}`).
async fn describe_pipe(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/v1/pipes/{name}"))
        .header("host", format!("pipes.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("pipes"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The stack as it reads once its operation has reached a TERMINAL status, polled rather than
/// slept for: how many spans the resources' own moves take is the deployment's to decide.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let began = Instant::now();
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStacks answered {status} {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") || began.elapsed() >= PATIENCE {
            return described;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Send `body` to the queue at `url`, which must be served.
async fn send(router: &MultiServiceRouter, case: &str, url: &str, body: &str) {
    let (status, answered) = sqs(
        router,
        "SendMessage",
        &json!({ "QueueUrl": url, "MessageBody": body }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SendMessage to {url} answered {status} {answered}"
    );
}

/// Every message body visible at `url` right now. `VisibilityTimeout: 0` takes no checkout, so
/// nothing read here is hidden from a later look.
async fn visible_at(router: &MultiServiceRouter, case: &str, url: &str) -> BTreeSet<String> {
    let (status, answered) = sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "VisibilityTimeout": 0 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage on {url} answered {status} {answered}"
    );
    answered["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| message["Body"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The queue's whole outstanding count: the visible records plus the ones a consumer is holding
/// ([`DEPTH_ATTRIBUTES`]). A queue url that no longer resolves fails the row here.
async fn depth_of(router: &MultiServiceRouter, case: &str, url: &str) -> u64 {
    let (status, answered) = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": DEPTH_ATTRIBUTES }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes on {url} answered {status} {answered}"
    );
    DEPTH_ATTRIBUTES
        .iter()
        .map(|attribute| {
            answered["Attributes"][*attribute]
                .as_str()
                .unwrap_or("0")
                .parse::<u64>()
                .unwrap_or_default()
        })
        .sum()
}

/// Watch the target until `wanted` has arrived, accumulating every body seen on the way.
async fn delivered_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: &str,
) -> BTreeSet<String> {
    let deadline = Instant::now() + BUDGET;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    loop {
        seen.extend(visible_at(router, case, url).await);
        if seen.contains(wanted) || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Watch the source until it holds nothing at all — the records the pipe admitted and the ones
/// it dropped are both consumed — accumulating what the TARGET showed meanwhile, so a record
/// delivered before the drain is not missed.
async fn drained(
    router: &MultiServiceRouter,
    case: &str,
    source_url: &str,
    target_url: &str,
    seen: &mut BTreeSet<String>,
) -> u64 {
    let deadline = Instant::now() + BUDGET;
    loop {
        let depth = depth_of(router, case, source_url).await;
        seen.extend(visible_at(router, case, target_url).await);
        if depth == 0 || Instant::now() >= deadline {
            return depth;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Keep watching the target for [`QUIET`] after everything expected has arrived, so a delivery
/// that was merely LATE is seen rather than missed by an assertion that nothing more comes.
async fn quietly_watched(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    seen: &mut BTreeSet<String>,
) {
    let until = Instant::now() + QUIET;
    while Instant::now() < until {
        seen.extend(visible_at(router, case, url).await);
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The trust policy the execution role carries: EventBridge Pipes acts as the pipe's `RoleArn`.
fn pipes_trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "pipes.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// One row: which mutable member of the pipe the revision restates, and the value of `k` the
/// filter in force AFTER the update admits.
struct Row {
    case: &'static str,
    /// The prefix every name this row declares carries, so two rows never share a resource.
    prefix: &'static str,
    /// The source filter's one pattern, before and after the update.
    pattern: (&'static str, &'static str),
    /// The pipe's `Description`, before and after the update.
    description: (&'static str, &'static str),
    /// `SourceParameters.SqsQueueParameters.BatchSize`, before and after the update.
    batch_size: (i64, i64),
    /// The `k` the filter in force after the update ADMITS, and the one it REJECTS.
    admits: &'static str,
    rejects: &'static str,
}

impl Row {
    fn stack(&self) -> String {
        format!("{}-stack", self.prefix)
    }

    fn pipe(&self) -> String {
        format!("{}-pipe", self.prefix)
    }

    /// A record body carrying `k`, tagged with this row's prefix and `stage` so no two sends of
    /// the suite share a body.
    fn record(&self, stage: &str, k: &str) -> String {
        json!({ "k": k, "m": format!("{}-{stage}", self.prefix) }).to_string()
    }

    /// The template this row deploys. The two Amazon SQS queues are UNNAMED, so their physical
    /// ids are the queue urls CloudFormation published for the names it generated — the very
    /// handles the `Outputs` carry.
    fn template(&self, pattern: &str, description: &str, batch_size: i64) -> String {
        json!({
            "Resources": {
                SOURCE_QUEUE: { "Type": "AWS::SQS::Queue" },
                TARGET_QUEUE: { "Type": "AWS::SQS::Queue" },
                "PipeRole": {
                    "Type": "AWS::IAM::Role",
                    "Properties": {
                        "AssumeRolePolicyDocument": pipes_trust_policy(),
                        "Policies": [{
                            "PolicyName": "move-records",
                            "PolicyDocument": {
                                "Version": "2012-10-17",
                                "Statement": [{
                                    "Effect": "Allow",
                                    "Action": [
                                        "sqs:ReceiveMessage",
                                        "sqs:DeleteMessage",
                                        "sqs:GetQueueAttributes",
                                        "sqs:SendMessage",
                                    ],
                                    "Resource": "*",
                                }],
                            },
                        }],
                    },
                },
                PIPE: {
                    "Type": "AWS::Pipes::Pipe",
                    "Properties": {
                        "Name": self.pipe(),
                        "Description": description,
                        "Source": { "Fn::GetAtt": [SOURCE_QUEUE, "Arn"] },
                        "Target": { "Fn::GetAtt": [TARGET_QUEUE, "Arn"] },
                        "RoleArn": { "Fn::GetAtt": ["PipeRole", "Arn"] },
                        "DesiredState": RUNNING,
                        "SourceParameters": {
                            "SqsQueueParameters": { "BatchSize": batch_size },
                            "FilterCriteria": { "Filters": [{ "Pattern": pattern }] },
                        },
                        "TargetParameters": { "InputTemplate": BODY_TEMPLATE },
                    },
                },
            },
            "Outputs": {
                "SourceQueueUrl": { "Value": { "Ref": SOURCE_QUEUE } },
                "TargetQueueUrl": { "Value": { "Ref": TARGET_QUEUE } },
            },
        })
        .to_string()
    }

    fn template_before(&self) -> String {
        self.template(self.pattern.0, self.description.0, self.batch_size.0)
    }

    fn template_after(&self) -> String {
        self.template(self.pattern.1, self.description.1, self.batch_size.1)
    }
}

/// The patterns the rows filter with: a body whose `k` is exactly `"a"`, and one whose `k` is
/// exactly `"b"`. Written compactly, and asserted back VERBATIM: a pattern is a string member
/// (`Filter$Pattern`), so the document is the client's own text.
const ADMITS_A: &str = r#"{"body":{"k":["a"]}}"#;
const ADMITS_B: &str = r#"{"body":{"k":["b"]}}"#;

/// The rows, each restating exactly ONE member `UpdatePipe` takes while the two sibling queues'
/// declarations are byte-for-byte the ones that were deployed.
const ROWS: &[Row] = &[
    Row {
        case: "the source filter's pattern",
        prefix: "pipe-inplace-filter",
        pattern: (ADMITS_A, ADMITS_B),
        description: ("moves records", "moves records"),
        batch_size: (1, 1),
        admits: "b",
        rejects: "a",
    },
    Row {
        case: "the pipe's description",
        prefix: "pipe-inplace-description",
        pattern: (ADMITS_A, ADMITS_A),
        description: ("moves records", "moves the records a filter admits"),
        batch_size: (1, 1),
        admits: "a",
        rejects: "b",
    },
    Row {
        case: "the batch size the Amazon SQS source is polled under",
        prefix: "pipe-inplace-batch",
        pattern: (ADMITS_A, ADMITS_A),
        description: ("moves records", "moves records"),
        batch_size: (1, 4),
        admits: "a",
        rejects: "b",
    },
];

/// RULE, every row: the stack is created and the pipe really moves a record the filter admits;
/// the revision that restates one mutable member of the pipe reaches `UPDATE_COMPLETE`; the pipe
/// keeps its `Arn` and `CreationTime`, reports `RUNNING`, a `LastModifiedTime` no earlier than
/// before, and all three members at the revision's values; both unnamed queues keep their
/// physical resource ids and the urls the stack published go on serving `SendMessage`,
/// `ReceiveMessage` and `GetQueueAttributes`; and the filter in force after the update is live on
/// the poller — the record it admits is forwarded, the record it rejects is consumed from the
/// source and never delivered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mutable_only_pipe_update_is_applied_in_place_and_spares_its_sibling_queues() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;
        let stack = row.stack();
        let pipe = row.pipe();

        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &row.template_before()),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack answered {status} {created}"
        );
        let described = settled_stack(&router, case, &stack).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: expected the stack {CREATE_COMPLETE}, got {reported:?} in {described}"
        );

        let outputs = outputs_of(&described);
        let output = |key: &str| {
            outputs
                .get(key)
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
        };
        let source_url = output("SourceQueueUrl");
        let target_url = output("TargetQueueUrl");

        let (status, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStackResources answered {status} {resources}"
        );
        let ids_before = physical_ids_of(&resources);

        let (status, before) = describe_pipe(&router, &pipe).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribePipe {pipe} answered {status} {before}"
        );
        let arn_before = before["Arn"].as_str().unwrap_or_default().to_string();
        let created_at = before["CreationTime"].as_i64().unwrap_or_default();
        let modified_before = before["LastModifiedTime"].as_i64().unwrap_or_default();
        assert!(
            !arn_before.is_empty() && created_at > 0,
            "case {case:?}: expected the created pipe to report an Arn and a CreationTime, got \
             {before}"
        );

        // The positive control: the deployed pipe is a live pipe that moves records its filter
        // admits. Every row's filter before the update admits k=a.
        let control = row.record("before", "a");
        send(&router, case, &source_url, &control).await;
        let seen = delivered_until(&router, case, &target_url, &control).await;
        assert!(
            seen.contains(&control),
            "case {case:?}: expected the deployed pipe to deliver {control:?} before the update, \
             saw {seen:?}"
        );

        // The update: the SAME template with exactly one member of the pipe restated.
        let (status, updated) = cloudformation(
            &router,
            "UpdateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &row.template_after()),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateStack answered {status} {updated}"
        );
        let described = settled_stack(&router, case, &stack).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, UPDATE_COMPLETE,
            "case {case:?}: expected the mutable-only revision to reach {UPDATE_COMPLETE}, got \
             {reported:?} in {described}"
        );

        // Nothing was replaced: every resource of the stack still answers to the id it was
        // created under, and the two queue urls the stack publishes are the ones it published.
        let (status, resources) =
            cloudformation(&router, "DescribeStackResources", &[("StackName", &stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStackResources answered {status} {resources}"
        );
        let ids_after = physical_ids_of(&resources);
        assert_eq!(
            ids_after, ids_before,
            "case {case:?}: expected every physical resource id to survive the update, they \
             moved to {ids_after:?} from {ids_before:?}"
        );
        let outputs_after = outputs_of(&described);
        assert_eq!(
            outputs_after.get("SourceQueueUrl"),
            Some(&source_url),
            "case {case:?}: expected the published source url {source_url:?} to survive, got \
             {outputs_after:?}"
        );
        assert_eq!(
            outputs_after.get("TargetQueueUrl"),
            Some(&target_url),
            "case {case:?}: expected the published target url {target_url:?} to survive, got \
             {outputs_after:?}"
        );

        // ...and the urls still RESOLVE in Amazon SQS: both answer `GetQueueAttributes` rather
        // than refusing a queue that does not exist, and the source — which the pipe drained
        // before the update — still holds nothing. (The target still holds the control record:
        // this suite reads it with no checkout and deletes nothing.)
        let target_depth = depth_of(&router, case, &target_url).await;
        assert!(
            target_depth >= 1,
            "case {case:?}: expected the surviving target {target_url} to be served and to still \
             hold the record delivered before the update, it reports {target_depth}"
        );
        let source_depth = depth_of(&router, case, &source_url).await;
        assert_eq!(
            source_depth, 0,
            "case {case:?}: expected the surviving source {source_url} to be served and drained \
             after the update, it reports {source_depth} outstanding records"
        );

        // The pipe is the SAME pipe, carrying the revision's configuration.
        let (status, after) = describe_pipe(&router, &pipe).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribePipe {pipe} answered {status} {after}"
        );
        assert_eq!(
            after["Arn"].as_str(),
            Some(arn_before.as_str()),
            "case {case:?}: expected the pipe to keep its Arn {arn_before:?}, it reports {after}"
        );
        assert_eq!(
            after["CreationTime"].as_i64(),
            Some(created_at),
            "case {case:?}: expected the pipe to keep its CreationTime {created_at}, it reports \
             {after}"
        );
        assert_eq!(
            after["CurrentState"].as_str(),
            Some(RUNNING),
            "case {case:?}: expected the updated pipe {RUNNING}, it reports {after}"
        );
        let modified_after = after["LastModifiedTime"].as_i64().unwrap_or_default();
        assert!(
            modified_after >= modified_before && modified_after >= created_at,
            "case {case:?}: expected LastModifiedTime to be at or after both the value before \
             the update ({modified_before}) and CreationTime ({created_at}), got \
             {modified_after}"
        );
        assert_eq!(
            after["Description"].as_str(),
            Some(row.description.1),
            "case {case:?}: expected the Description the revision states, it reports {after}"
        );
        assert_eq!(
            after["SourceParameters"]["SqsQueueParameters"]["BatchSize"].as_i64(),
            Some(row.batch_size.1),
            "case {case:?}: expected the BatchSize the revision states, it reports {after}"
        );
        assert_eq!(
            after["SourceParameters"]["FilterCriteria"]["Filters"],
            json!([{ "Pattern": row.pattern.1 }]),
            "case {case:?}: expected the one filter the revision states, it reports {after}"
        );

        // ...and the filter in force is LIVE on the poller, not merely reported: the record it
        // admits reaches the target, the record it rejects is consumed from the source and never
        // delivered.
        let rejected = row.record("after", row.rejects);
        let admitted = row.record("after", row.admits);
        send(&router, case, &source_url, &rejected).await;
        send(&router, case, &source_url, &admitted).await;
        let mut seen = delivered_until(&router, case, &target_url, &admitted).await;
        assert!(
            seen.contains(&admitted),
            "case {case:?}: expected the updated pipe to deliver {admitted:?}, saw {seen:?}"
        );
        let depth = drained(&router, case, &source_url, &target_url, &mut seen).await;
        assert_eq!(
            depth, 0,
            "case {case:?}: expected the source to drain to nothing once the pipe had decided \
             both records, it still holds {depth}"
        );
        quietly_watched(&router, case, &target_url, &mut seen).await;
        assert!(
            !seen.contains(&rejected),
            "case {case:?}: expected {rejected:?} to be consumed and never delivered, saw \
             {seen:?}"
        );
    }
}
