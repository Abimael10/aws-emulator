// SPDX-License-Identifier: CC0-1.0

//! Each state an instance moves into reaches the EventBridge rule scoped to that state on the
//! emulator a client STARTS: the built binary, on its own port, with request authentication
//! on, reached over TLS with SigV4-signed calls.
//!
//! The RULE under test: **Amazon EC2 publishes every state an instance moves INTO onto the
//! account's default event bus as `source` `aws.ec2` / `detail-type`
//! `EC2 Instance State-change Notification`, naming the instance in `resources` and carrying
//! the instance's id and that state in the event `detail`; EventBridge routes each envelope by
//! its CONTENT, so a rule whose `EventPattern` names ONE state receives that state's envelope
//! and none of the other states'.**
//!
//! [`crate::ec2_instance_state_changes_reach_their_own_rules`] pins that rule over a router
//! assembled inside the test process, with authentication off, on a runtime the test chose.
//! The emulator a client runs is a different configuration of the same code: the publication
//! Amazon EC2 makes is signed with the credentials THAT process vends, each state the clock
//! owes the instance is published from a task of its own on the binary's own multi-thread
//! runtime, EventBridge's delivery is authorized by the queue's own policy, and one process
//! serves every call. This suite is the one that pins the chain THERE, so a state change that
//! is delivered in-process and not on the started emulator is a failure a test reports rather
//! than one only a client sees.
//!
//! The legs span the states one instance's lifecycle passes through, each behind a rule and a
//! queue of its own: the `pending` and `running` of a launch and of a start, the `stopping`
//! and `stopped` of a stop, and the `shutting-down` and `terminated` of a termination. Every
//! rule's pattern is the `$or` of its state branch and a BARRIER branch of the caller's own
//! source, so a queue that receives no state change is one whose PATTERN withheld it rather
//! than a rule that was never live.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::{element, elements};

/// The Region the started emulator's calls are signed for, which is the `region` the envelopes
/// must report. It must stay the one [`Started`] signs with.
const REGION: &str = "us-east-1";

/// The zone the subnet of this suite sits in.
const ZONE: &str = "us-east-1a";

/// What the envelope says about itself, spelled out here rather than imported from the service
/// crate, so a wrong constant in the implementation cannot make this suite pass.
const EVENT_SOURCE: &str = "aws.ec2";
const INSTANCE_STATE_CHANGE: &str = "EC2 Instance State-change Notification";

/// The public Systems Manager parameter a client resolves the Amazon Linux 2023 image id
/// through, which is the image the instance here is launched from.
const AL2023_PARAMETER: &str =
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// The instance type the launch asks for.
const INSTANCE_TYPE: &str = "t3.micro";

/// The service principal EventBridge delivers to a queue target as, which is what the queue's
/// own resource policy has to allow.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The caller's own source the barrier event carries, and the detail type it goes under.
const BARRIER_SOURCE: &str = "ec2statechange.started.barrier";
const BARRIER_DETAIL_TYPE: &str = "Barrier";

/// The states one instance's lifecycle passes through, each the subject of a rule of its own.
/// They are `InstanceStateName` values (`smithy_query` query="shape"
/// shape="com.amazonaws.ec2#InstanceStateName"), spelled as that enum spells them.
const STATES: [&str; 6] = [
    "pending",
    "running",
    "stopping",
    "stopped",
    "shutting-down",
    "terminated",
];

/// How long a leg waits for what it waits for, and how often it looks.
///
/// Generous on purpose: every wait is on a CONDITION, looked for again until it holds, so a
/// fast machine never waits the whole span and a loaded one is given room to spend.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How many further reads a leg makes when asserting that nothing MORE arrives, so a delivery
/// that arrives late still fails the leg.
const HELD_READS: usize = 5;

/// One `X-Amz-Target` RPC call over the started emulator's signed-call plumbing, which must
/// answer 200.
fn rpc_ok(
    emulator: &Started,
    case: &str,
    signing_name: &str,
    target: &str,
    content_type: &str,
    body: &Value,
) -> Value {
    let (status, text) = emulator.rpc(signing_name, target, content_type, body);
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {target}, got {status} with {parsed}; the \
         log:\n{}",
        emulator.log()
    );
    parsed
}

/// One EventBridge call that must answer 200.
fn events(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    rpc_ok(
        emulator,
        case,
        "events",
        &format!("AWSEvents.{operation}"),
        "application/x-amz-json-1.1",
        body,
    )
}

/// One Amazon SQS call that must answer 200.
fn sqs(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    rpc_ok(
        emulator,
        case,
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    )
}

/// One Systems Manager call that must answer 200.
fn ssm(emulator: &Started, case: &str, operation: &str, body: &Value) -> Value {
    rpc_ok(
        emulator,
        case,
        "ssm",
        &format!("AmazonSSM.{operation}"),
        "application/x-amz-json-1.1",
        body,
    )
}

/// One Amazon EC2 call that must answer 200, as its XML body.
fn ec2(emulator: &Started, case: &str, action: &str, params: &[(&str, &str)]) -> String {
    let (status, text) = emulator.form_call("ec2", "2016-11-15", action, params);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon EC2 {action}, got {status} with {text}; \
         the log:\n{}",
        emulator.log()
    );
    text
}

/// A string member of a JSON answer, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// The pattern the rule of `state` carries: the `$or` of its state branch and the barrier
/// branch that proves the rule, its target and the queue's policy deliver at all.
fn pattern_for(state: &str) -> Value {
    json!({
        "$or": [
            {
                "source": [EVENT_SOURCE],
                "detail-type": [INSTANCE_STATE_CHANGE],
                "detail": { "state": [state] },
            },
            {
                "source": [BARRIER_SOURCE],
                "detail": { "nonce": [state] },
            },
        ],
    })
}

/// An ENABLED rule named for `state`, with a queue of its own as its one target; answers that
/// queue's url.
fn rule_for(emulator: &Started, case: &str, state: &str) -> String {
    let name = format!("started-ec2-state-{state}");
    let created = events(
        emulator,
        case,
        "PutRule",
        &json!({
            "Name": &name,
            "State": "ENABLED",
            "EventPattern": pattern_for(state).to_string(),
        }),
    );
    let rule_arn = member(case, &created, "/RuleArn");

    let queue = sqs(emulator, case, "CreateQueue", &json!({ "QueueName": &name }));
    let url = member(case, &queue, "/QueueUrl");
    let attributes = sqs(
        emulator,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    );
    let queue_arn = member(case, &attributes, "/Attributes/QueueArn");
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": &queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": &rule_arn } },
        }],
    })
    .to_string();
    sqs(
        emulator,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
    );
    let targets = events(
        emulator,
        case,
        "PutTargets",
        &json!({
            "Rule": &name,
            "Targets": [{ "Id": "sqs-target", "Arn": &queue_arn }],
        }),
    );
    assert_eq!(
        targets["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutTargets reported a failed entry: {targets}"
    );
    url
}

/// Every envelope waiting on the queue at `url`, deleting what it hands over.
fn drain(emulator: &Started, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        emulator,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default();
        envelopes.push(serde_json::from_str::<Value>(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the body is not an event envelope ({error}): {body}")
        }));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                case,
                "DeleteMessage",
                &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
            );
        }
    }
    envelopes
}

/// Look at `instance_id` until it reports `state`, so the call that acts on it next is made
/// then and not before.
fn until_state(emulator: &Started, case: &str, instance_id: &str, state: &str) {
    let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let body = ec2(
            emulator,
            case,
            "DescribeInstances",
            &[("InstanceId.1", instance_id)],
        );
        if elements(&body, "name").iter().any(|name| name == state) {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: {instance_id} must reach {state}; the instance reads {body}; the \
             log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// RULE: on the emulator a client starts, every state an instance moves into is published and
/// delivered to the rule scoped to that state, and to no other state's rule.
#[test]
fn each_instance_state_reaches_its_own_rule_on_the_emulator_a_client_starts() {
    let setup = "the per-state wiring on a started emulator";
    let emulator = Started::start();

    // The network the instance is placed in, created through Amazon EC2 itself.
    let vpc = element(
        &ec2(
            &emulator,
            setup,
            "CreateVpc",
            &[("CidrBlock", "10.91.0.0/16")],
        ),
        "vpcId",
    );
    let subnet = element(
        &ec2(
            &emulator,
            setup,
            "CreateSubnet",
            &[
                ("VpcId", &vpc),
                ("CidrBlock", "10.91.1.0/24"),
                ("AvailabilityZone", ZONE),
            ],
        ),
        "subnetId",
    );
    let group = element(
        &ec2(
            &emulator,
            setup,
            "CreateSecurityGroup",
            &[
                ("VpcId", &vpc),
                ("GroupName", "started-ec2-state-changes"),
                ("GroupDescription", "the instance under test"),
            ],
        ),
        "groupId",
    );

    // The image, resolved the way a client resolves it: the published parameter.
    let resolved = ssm(
        &emulator,
        setup,
        "GetParameter",
        &json!({ "Name": AL2023_PARAMETER }),
    );
    let image = member(setup, &resolved, "/Parameter/Value");

    // One rule and one queue per state, each proven by its own barrier event before the
    // instance is launched.
    let mut urls: BTreeMap<&str, String> = BTreeMap::new();
    for state in STATES {
        urls.insert(state, rule_for(&emulator, setup, state));
    }
    for state in STATES {
        events(
            &emulator,
            setup,
            "PutEvents",
            &json!({ "Entries": [{
                "Source": BARRIER_SOURCE,
                "DetailType": BARRIER_DETAIL_TYPE,
                "Detail": json!({ "nonce": state }).to_string(),
            }] }),
        );
        let url = urls[state].clone();
        let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
        loop {
            let held = drain(&emulator, setup, &url);
            if held
                .iter()
                .any(|envelope| envelope["detail"]["nonce"] == json!(state))
            {
                break;
            }
            assert!(
                Instant::now() < giving_up_at,
                "case {setup:?}: the barrier event must reach the {state} rule's queue before \
                 the instance is launched, got {held:?}; the log:\n{}",
                emulator.log()
            );
            std::thread::sleep(LOOK_INTERVAL);
        }
    }

    // The instance, and the whole lifecycle a client drives it through.
    let case = "one instance's lifecycle";
    let launched = ec2(
        &emulator,
        case,
        "RunInstances",
        &[
            ("ImageId", &image),
            ("InstanceType", INSTANCE_TYPE),
            ("SubnetId", &subnet),
            ("SecurityGroupId.1", &group),
            ("MinCount", "1"),
            ("MaxCount", "1"),
        ],
    );
    let instance = element(&launched, "instanceId");

    until_state(&emulator, case, &instance, "running");
    ec2(
        &emulator,
        case,
        "StopInstances",
        &[("InstanceId.1", &instance)],
    );
    until_state(&emulator, case, &instance, "stopped");
    ec2(
        &emulator,
        case,
        "StartInstances",
        &[("InstanceId.1", &instance)],
    );
    until_state(&emulator, case, &instance, "running");
    ec2(
        &emulator,
        case,
        "TerminateInstances",
        &[("InstanceId.1", &instance)],
    );
    until_state(&emulator, case, &instance, "terminated");

    // What each state's rule received, kept as it arrives: the states of this instance.
    let mut taken: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let giving_up_at = Instant::now() + DELIVERY_DEADLINE;
    loop {
        for state in STATES {
            let url = urls[state].clone();
            for envelope in drain(&emulator, case, &url) {
                if envelope["detail-type"] != json!(INSTANCE_STATE_CHANGE)
                    || envelope["detail"]["instance-id"] != json!(&instance)
                {
                    continue;
                }
                assert_eq!(
                    envelope["source"],
                    json!(EVENT_SOURCE),
                    "case {case:?}: every state change carries the EC2 source, got {envelope}"
                );
                assert_eq!(
                    envelope["region"],
                    json!(REGION),
                    "case {case:?}: every state change is published in the call's Region, got \
                     {envelope}"
                );
                let names_instance = envelope["resources"].as_array().is_some_and(|resources| {
                    resources.iter().any(|resource| {
                        resource
                            .as_str()
                            .is_some_and(|arn| arn.ends_with(&format!("instance/{instance}")))
                    })
                });
                assert!(
                    names_instance,
                    "case {case:?}: every state change names {instance} in its resources, got \
                     {envelope}"
                );
                taken.entry(state).or_default().push(
                    envelope["detail"]["state"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                );
            }
        }
        let missing: Vec<&str> = STATES
            .into_iter()
            .filter(|state| {
                taken
                    .get(state)
                    .is_none_or(|seen| !seen.iter().any(|got| got == state))
            })
            .collect();
        if missing.is_empty() {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: every state of {instance} must reach its own rule's queue; \
             {missing:?} never arrived, and what did arrive is {taken:?}; the log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }

    // And nothing else: a rule scoped to one state receives no other state's envelope.
    for read in 0..HELD_READS {
        for state in STATES {
            let url = urls[state].clone();
            for envelope in drain(&emulator, case, &url) {
                if envelope["detail-type"] != json!(INSTANCE_STATE_CHANGE)
                    || envelope["detail"]["instance-id"] != json!(&instance)
                {
                    continue;
                }
                taken.entry(state).or_default().push(
                    envelope["detail"]["state"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                );
            }
            let foreign: Vec<&String> = taken
                .get(state)
                .map(|seen| seen.iter().filter(|got| *got != state).collect())
                .unwrap_or_default();
            assert!(
                foreign.is_empty(),
                "case {case:?}, read {read}: the rule scoped to {state} receives only that \
                 state's envelopes for {instance}, got {foreign:?}"
            );
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
}
