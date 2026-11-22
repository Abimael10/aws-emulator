// SPDX-License-Identifier: CC0-1.0

//! Which EventBridge rules an Auto Scaling group's instance lifecycle steps reach, over a whole
//! capacity WALK and over a table of the fields a rule's pattern can pin.
//!
//! RULE: every instance an Auto Scaling group brings into service, and every instance it takes
//! out of service, is published ONCE onto the account's default event bus under `source`
//! `aws.autoscaling` and the `detail-type` of that step (`EC2 Instance Launch Successful` /
//! `EC2 Instance Terminate Successful`), naming the group and the instance in the `detail`;
//! EventBridge then delivers that one envelope to the targets of EVERY rule whose `EventPattern`
//! agrees with it in each field that pattern pins, and to NO rule that differs from it in any
//! one of them. The pattern a rule is judged by is the one it carries NOW — "if you are updating
//! an existing rule, the rule is replaced with what you specify in this PutRule command" — so a
//! rule rewritten to an agreeing pattern receives the group's next step, with no change to the
//! group and no further Auto Scaling call.
//!
//! [`crate::autoscaling_lifecycle_events_reach_a_corrected_rule`] pins the CORRECTED-rule leg of
//! that rule on one scale-out. This suite spans the rule itself: a rule per detail type the
//! service publishes, the two near misses a pattern can be wrong in one field by (another
//! group's name in the `detail`, another service's `source`), and the rule under repair — all
//! judged against ONE walk of the group's capacity up, back to zero and up again, which is the
//! walk a client makes when it corrects a rule and wants the next launch. The group is placed in
//! a subnet of the account's own DEFAULT VPC, which is the network a client that created none
//! has.
//!
//! The last case is about the READ rather than the rule: a consumer watching a group arms a
//! long poll on the target queue and waits, and what ends that call is the envelope the group's
//! next step publishes — "the call returns sooner than WaitTimeSeconds if a message becomes
//! available" (`ReceiveMessageRequest$WaitTimeSeconds`) — not the end of the window it asked
//! for. It is the one span this suite bounds, and it bounds it by the case's own window.
//!
//! [`crate::asg_lifecycle_events_on_a_started_emulator`] pins the same rule on the emulator a
//! client STARTS, where the publication is signed with the credentials that process vends.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for form-wire suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, items_of};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API version an ec2Query and an awsQuery request carries.
const EC2_API_VERSION: &str = "2016-11-15";
const ASG_VERSION: &str = "2011-01-01";

/// The `source` and the two `detail-type`s Auto Scaling publishes an instance lifecycle step
/// under, spelled out here rather than imported, so a wrong constant in the implementation
/// cannot make this suite pass.
const EVENT_SOURCE: &str = "aws.autoscaling";
const LAUNCH_SUCCESSFUL: &str = "EC2 Instance Launch Successful";
const TERMINATE_SUCCESSFUL: &str = "EC2 Instance Terminate Successful";

/// The group this suite walks, the name of a group it never creates (so a row can pin a group
/// the envelope is not about), and the launch template the group launches from.
const GROUP: &str = "asgwalk-group";
const OTHER_GROUP: &str = "asgwalk-other-group";
const TEMPLATE: &str = "asgwalk-template";

/// The image and instance type the launch template names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// How long a leg waits for the clock-owed moves it depends on — the launch, the termination and
/// the publications that report them — and how often it looks. Generous on purpose: every wait
/// looks for its CONDITION, so machine load decides when the leg finishes and never whether it
/// passes.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);

/// How many times a leg re-reads a queue that must stay EMPTY: the absence is asserted over a
/// run of reads rather than over one, so a delivery that arrives late still fails the leg.
const HELD_READS: usize = 10;

/// The window a WAITING read asks Amazon SQS to hold its call open for. Deliberately far longer
/// than anything in this suite takes, because what the case using it asserts is that the read
/// returns when the envelope is DELIVERED rather than at the end of this window: "the call
/// returns sooner than WaitTimeSeconds if a message becomes available"
/// (`ReceiveMessageRequest$WaitTimeSeconds`).
const WAITING_READ_SECONDS: u64 = 20;

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to the service `signing_name` addresses.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Amazon EC2 action that must succeed.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Auto Scaling action that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "autoscaling", form(action, ASG_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One awsJson RPC call to a service mounted beside Auto Scaling, the way a client makes it.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.body().clone();
    let parsed: Value = if payload.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| json!({ "__unparsed": String::from_utf8_lossy(&payload) }))
    };
    (status, parsed)
}

/// One EventBridge call that must succeed.
async fn events(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, answer) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "setup: EventBridge {operation} must succeed, got {status} with {answer}"
    );
    answer
}

/// One Amazon SQS call that must succeed.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, answer) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "setup: Amazon SQS {operation} must succeed, got {status} with {answer}"
    );
    answer
}

/// A queue created through Amazon SQS itself, granting `events.amazonaws.com` the
/// `sqs:SendMessage` a rule's target needs: EventBridge makes that call as its own service
/// principal and relies on the queue's resource-based policy for it. Answers the queue's url and
/// its ARN.
async fn given_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs(router, "CreateQueue", json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        "GetQueueAttributes",
        json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        }],
    })
    .to_string();
    sqs(
        router,
        "SetQueueAttributes",
        json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    (url, queue_arn)
}

/// The pattern a client watching ONE group writes: the source, the detail type, and the group
/// named by the member the event's `detail` names it in.
fn pattern(source: &str, detail_type: &str, group: &str) -> String {
    json!({
        "source": [source],
        "detail-type": [detail_type],
        "detail": { "AutoScalingGroupName": [group] },
    })
    .to_string()
}

/// Put (or re-put) the rule `name` with `pattern`, leaving its targets alone.
async fn put_rule(router: &MultiServiceRouter, name: &str, pattern: &str) {
    events(
        router,
        "PutRule",
        json!({ "Name": name, "EventPattern": pattern, "State": "ENABLED" }),
    )
    .await;
}

/// Point the rule `name` at the queue `queue_arn`.
async fn put_target(router: &MultiServiceRouter, name: &str, queue_arn: &str) {
    let answer = events(
        router,
        "PutTargets",
        json!({ "Rule": name, "Targets": [{ "Id": "sqs-target", "Arn": queue_arn }] }),
    )
    .await;
    assert_eq!(
        answer["FailedEntryCount"],
        json!(0),
        "setup: PutTargets reported a failed entry: {answer}"
    );
}

/// The rule `name` with `pattern` and a queue of its own as its target; answers the queue's url.
async fn given_rule(router: &MultiServiceRouter, name: &str, pattern: &str) -> String {
    let (url, queue_arn) = given_queue(router, name).await;
    put_rule(router, name, pattern).await;
    put_target(router, name, &queue_arn).await;
    url
}

/// Every envelope waiting on the queue at `url` right now, as parsed event envelopes, DELETING
/// what it hands over — so a later read of the same queue sees only what arrived since, rather
/// than an earlier envelope whose visibility timeout has lapsed.
async fn drain(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        "ReceiveMessage",
        json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut taken = Vec::with_capacity(messages.len());
    for message in &messages {
        let raw = message["Body"].as_str().unwrap_or_default();
        taken.push(serde_json::from_str::<Value>(raw).unwrap_or(Value::Null));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                "DeleteMessage",
                json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            )
            .await;
        }
    }
    taken
}

/// Assert the queue at `url` holds nothing, over a RUN of reads: both depth attributes report
/// zero and a read hands over no message at all, every time.
async fn assert_holds_nothing(router: &MultiServiceRouter, case: &str, url: &str) {
    for read in 0..HELD_READS {
        let reported = sqs(
            router,
            "GetQueueAttributes",
            json!({
                "QueueUrl": url,
                "AttributeNames": [
                    "ApproximateNumberOfMessages",
                    "ApproximateNumberOfMessagesNotVisible",
                ],
            }),
        )
        .await;
        for attribute in [
            "ApproximateNumberOfMessages",
            "ApproximateNumberOfMessagesNotVisible",
        ] {
            assert_eq!(
                reported["Attributes"][attribute],
                json!("0"),
                "case {case:?}, read {read}: expected {attribute} \"0\", got {reported}"
            );
        }
        let held = drain(router, url).await;
        assert!(
            held.is_empty(),
            "case {case:?}, read {read}: expected no message on the queue, got {held:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Poll the queue at `url` until an envelope of `detail_type` naming `instance_id` has been
/// delivered, and answer it.
async fn envelope_naming(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    detail_type: &str,
    instance_id: &str,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    let mut seen: Vec<Value> = Vec::new();
    loop {
        seen.extend(drain(router, url).await);
        let found = seen.iter().find(|envelope| {
            envelope["detail-type"] == json!(detail_type)
                && envelope["detail"]["EC2InstanceId"] == json!(instance_id)
        });
        if let Some(envelope) = found {
            return envelope.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected an envelope of {detail_type} naming {instance_id} on the \
             queue, got {seen:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The instance ids the group reports right now.
async fn group_instances(router: &MultiServiceRouter) -> Vec<String> {
    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", GROUP)],
    )
    .await;
    elements(&body, "InstanceId")
}

/// Set the group's capacity and wait until it reports `wanted` instances; answers them.
async fn scaled_to(
    router: &MultiServiceRouter,
    case: &str,
    capacity: &str,
    wanted: usize,
) -> Vec<String> {
    asg(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", GROUP),
            ("DesiredCapacity", capacity),
            ("HonorCooldown", "false"),
        ],
    )
    .await;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let held = group_instances(router).await;
        if held.len() == wanted {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the group to hold {wanted} instance(s), got {held:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// A subnet of the account's own DEFAULT VPC, discovered the way a client that created no network
/// of its own discovers one.
async fn default_vpc_subnet(router: &MultiServiceRouter) -> String {
    let vpcs = ec2(
        router,
        "DescribeVpcs",
        &[("Filter.1.Name", "isDefault"), ("Filter.1.Value.1", "true")],
    )
    .await;
    let vpc = element(&vpcs, "vpcId");
    let subnets = ec2(
        router,
        "DescribeSubnets",
        &[
            ("Filter.1.Name", "vpc-id"),
            ("Filter.1.Value.1", vpc.as_str()),
        ],
    )
    .await;
    let first = items_of(&subnets)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("setup: the default VPC of {REGION} must have a subnet"))
        .to_string();
    elements(&first, "subnetId")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("setup: no subnetId in {first}"))
}

/// Assert `envelope` is the step `detail_type` of `instance_id` in this group.
fn assert_step_envelope(case: &str, envelope: &Value, detail_type: &str, instance_id: &str) {
    for (pointer, expected) in [
        ("/source", json!(EVENT_SOURCE)),
        ("/detail-type", json!(detail_type)),
        ("/region", json!(REGION)),
        ("/detail/AutoScalingGroupName", json!(GROUP)),
        ("/detail/EC2InstanceId", json!(instance_id)),
    ] {
        let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {pointer} to be {expected}, got {actual} in {envelope}"
        );
    }
}

#[tokio::test]
async fn a_groups_capacity_walk_reaches_only_the_rules_whose_patterns_agree_with_it() {
    let router = router();
    let subnet = default_vpc_subnet(&router).await;
    ec2(
        &router,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", TEMPLATE),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;

    // The table: one rule per detail type this service publishes, and the two near misses a
    // pattern can be wrong in ONE field by.
    let launch_queue = given_rule(
        &router,
        "asgwalk-launch",
        &pattern(EVENT_SOURCE, LAUNCH_SUCCESSFUL, GROUP),
    )
    .await;
    let terminate_queue = given_rule(
        &router,
        "asgwalk-terminate",
        &pattern(EVENT_SOURCE, TERMINATE_SUCCESSFUL, GROUP),
    )
    .await;
    let near_misses: [(&str, &str, String); 2] = [
        (
            "the name of a group the envelope is not about",
            "asgwalk-other-group",
            pattern(EVENT_SOURCE, LAUNCH_SUCCESSFUL, OTHER_GROUP),
        ),
        (
            "the source of another service",
            "asgwalk-other-source",
            pattern("aws.ec2", LAUNCH_SUCCESSFUL, GROUP),
        ),
    ];
    let mut near: Vec<(&str, String)> = Vec::new();
    for (case, name, written) in &near_misses {
        near.push((*case, given_rule(&router, name, written).await));
    }
    // And the rule under repair, which starts out naming the detail type a scale-out does NOT
    // publish.
    let repaired_queue = given_rule(
        &router,
        "asgwalk-repaired",
        &pattern(EVENT_SOURCE, TERMINATE_SUCCESSFUL, GROUP),
    )
    .await;

    asg(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", GROUP),
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("DesiredCapacity", "0"),
            ("LaunchTemplate.LaunchTemplateName", TEMPLATE),
            ("VPCZoneIdentifier", subnet.as_str()),
        ],
    )
    .await;

    // ---- up: the launch rule receives the step, and no rule that differs in one field does ----
    let case = "a scale-out";
    let held = scaled_to(&router, case, "1", 1).await;
    let first = held
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the group must name its instance"));
    let delivered = envelope_naming(&router, case, &launch_queue, LAUNCH_SUCCESSFUL, &first).await;
    assert_step_envelope(case, &delivered, LAUNCH_SUCCESSFUL, &first);
    // The launch has been matched against every rule of the bus by the time it arrived above, so
    // a queue that holds nothing now received nothing FROM IT.
    for (differing, url) in near
        .iter()
        .map(|(differing, url)| (*differing, url.clone()))
        .chain([
            ("the terminate detail-type", terminate_queue.clone()),
            (
                "the terminate detail-type, on the rule under repair",
                repaired_queue.clone(),
            ),
        ])
    {
        assert_holds_nothing(
            &router,
            &format!("{case}: a rule differing in {differing}"),
            &url,
        )
        .await;
    }

    // ---- the repair, then down and back up: the corrected rule receives the NEXT launch ----
    put_rule(
        &router,
        "asgwalk-repaired",
        &pattern(EVENT_SOURCE, LAUNCH_SUCCESSFUL, GROUP),
    )
    .await;

    let case = "a scale-in";
    let emptied = scaled_to(&router, case, "0", 0).await;
    assert!(
        emptied.is_empty(),
        "case {case:?}: the group holds nothing at capacity zero, got {emptied:?}"
    );
    let left =
        envelope_naming(&router, case, &terminate_queue, TERMINATE_SUCCESSFUL, &first).await;
    assert_step_envelope(case, &left, TERMINATE_SUCCESSFUL, &first);

    let case = "the repaired rule, on the group's next launch";
    let held = scaled_to(&router, case, "1", 1).await;
    let second = held
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the group must name its instance"));
    assert_ne!(
        second, first,
        "case {case:?}: the walk down and back up launches a NEW instance, got the first one \
         again"
    );
    let repaired =
        envelope_naming(&router, case, &repaired_queue, LAUNCH_SUCCESSFUL, &second).await;
    assert_step_envelope(case, &repaired, LAUNCH_SUCCESSFUL, &second);
    // One publication reaches every rule that agrees with it: the rule that matched all along
    // receives this launch too, and the near misses still receive nothing.
    let again = envelope_naming(&router, case, &launch_queue, LAUNCH_SUCCESSFUL, &second).await;
    assert_step_envelope(case, &again, LAUNCH_SUCCESSFUL, &second);
    for (differing, url) in &near {
        assert_holds_nothing(
            &router,
            &format!("{case}: a rule differing in {differing}"),
            url,
        )
        .await;
    }

    // ---- and a read already WAITING on the target is answered by the delivery itself ----
    // A client watching a group through a queue arms a long poll and waits; the envelope the
    // group's next step publishes is what ends that call, rather than the end of its own window.
    let case = "a waiting read, armed before the step it is waiting for";
    let emptied = scaled_to(&router, case, "0", 0).await;
    assert!(
        emptied.is_empty(),
        "case {case:?}: the group holds nothing at capacity zero, got {emptied:?}"
    );
    asg(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", GROUP),
            ("DesiredCapacity", "1"),
            ("HonorCooldown", "false"),
        ],
    )
    .await;
    let armed = Instant::now();
    let received = sqs(
        &router,
        "ReceiveMessage",
        json!({
            "QueueUrl": &launch_queue,
            "MaxNumberOfMessages": 10,
            "WaitTimeSeconds": WAITING_READ_SECONDS,
        }),
    )
    .await;
    let waited = armed.elapsed();
    let delivered: Vec<Value> = received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            let raw = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(raw).unwrap_or(Value::Null)
        })
        .collect();
    let third = group_instances(&router)
        .await
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the group must name its instance"));
    let answered = delivered
        .iter()
        .find(|envelope| envelope["detail"]["EC2InstanceId"] == json!(third))
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the armed read must be answered by the launch of {third}, got \
                 {delivered:?}"
            )
        });
    assert_step_envelope(case, answered, LAUNCH_SUCCESSFUL, &third);
    // The ONE bound this suite asserts a span against is the bound of the case itself: the read
    // was handed its envelope, so it cannot have waited out the window it asked for.
    assert!(
        waited < Duration::from_secs(WAITING_READ_SECONDS),
        "case {case:?}: a waiting read is ended by the delivery, not by the end of its own \
         {WAITING_READ_SECONDS}s window; it took {waited:?}"
    );
}
