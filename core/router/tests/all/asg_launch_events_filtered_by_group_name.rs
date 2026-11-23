// SPDX-License-Identifier: CC0-1.0

//! Which EventBridge rules the launch of an Auto Scaling group's INITIAL capacity reaches, when
//! the rules of the bus are filtered by the group's name.
//!
//! RULE: every instance an Auto Scaling group brings into service is published ONCE onto the
//! account's default event bus under `source` `aws.autoscaling` and `detail-type`
//! `EC2 Instance Launch Successful`, whatever asked for the capacity that launched it — the
//! group's own create included — naming the group and the instance in the `detail` and both their
//! ARNs in `resources`; EventBridge delivers that one envelope to the targets of every rule whose
//! `EventPattern` agrees with it and to no rule that differs from it, so every matching rule
//! hands over an envelope carrying the SAME event id.
//!
//! The table below spans the shape of the pattern a client filters such events by: an exact group
//! name, a `prefix` over a family of names, and the two near misses of each (another group's
//! name, a prefix nothing is named under). Two groups are created, so a row that names one of
//! them is asserted to receive that group's launch and to withhold the other's, and the `prefix`
//! row is asserted to receive BOTH — each with the same event id the exact row saw, because one
//! publication reaches every matching rule.
//!
//! Every row's pattern is written as the `$or` of the launch branch it filters by and a BARRIER
//! branch of the caller's own source, which is how a client proves its rule, its target and the
//! queue's resource policy deliver at all before it provokes any Auto Scaling activity: the
//! barrier event is published and awaited on each queue first, so a row that then receives
//! nothing is a row whose PATTERN withheld the launch rather than a rule that was never live.
//!
//! What this adds to the suites beside it: `asg_lifecycle_event_delivery_over_a_capacity_walk.rs`
//! drives the same rule over a capacity WALK of one group whose launches a later
//! `SetDesiredCapacity` asks for, and `asg_lifecycle_events_on_a_started_emulator.rs` drives it
//! against the binary a client starts. This one pins the leg those two do not: the capacity a
//! group's own `CreateAutoScalingGroup` asks for, two groups at once, and the group-name FILTER
//! that decides which rule sees which group's launch.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for form-wire suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API version an ec2Query and an awsQuery request carries.
const EC2_API_VERSION: &str = "2016-11-15";
const ASG_VERSION: &str = "2011-01-01";

/// The `source` and the `detail-type` Auto Scaling publishes a successful launch under, spelled
/// out here rather than imported, so a wrong constant in the implementation cannot make this
/// suite pass.
const EVENT_SOURCE: &str = "aws.autoscaling";
const LAUNCH_SUCCESSFUL: &str = "EC2 Instance Launch Successful";

/// The family the two groups of this suite are named under, and a family nothing is named under.
const FAMILY: &str = "asgfilter-";
const OTHER_FAMILY: &str = "asgabsent-";

/// The caller's own source and detail type the barrier event carries: every rule under test also
/// names them, so the barrier proves the rule, its target and the queue's policy deliver.
const BARRIER_SOURCE: &str = "asgfilter.barrier";
const BARRIER_DETAIL_TYPE: &str = "Barrier";

/// The image and instance type the launch templates name.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// The zone the subnet of this suite sits in.
const ZONE: &str = "us-east-1a";

/// How long a wait looks for the clock-owed moves it depends on — the launch and the publication
/// that reports it — and how often it looks. Generous on purpose: every wait looks for its
/// CONDITION, so machine load decides when a leg finishes and never whether it passes.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(100);

/// How many times a leg re-reads a queue that must stay EMPTY: the absence is asserted over a run
/// of reads rather than over one, so a delivery that arrives late still fails the leg.
const HELD_READS: usize = 5;

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(String, String)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// One request parameter.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
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
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Auto Scaling action that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
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

/// A queue created through Amazon SQS itself, whose resource policy grants
/// `events.amazonaws.com` the `sqs:SendMessage` a rule's target needs — only for calls made on
/// behalf of `rule_arn`, which is how a client scopes such a grant. Answers the queue's url.
async fn given_queue(router: &MultiServiceRouter, name: &str, rule_arn: &str) -> String {
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
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    })
    .to_string();
    sqs(
        router,
        "SetQueueAttributes",
        json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
    )
    .await;
    url
}

/// The pattern one rule of the table carries: the `$or` of the launch branch this row filters
/// launches by (`group_term`, a group name or a `prefix` term) and the barrier branch that proves
/// the rule delivers at all, which is the caller's own source carrying this row's `nonce`.
fn or_pattern(group_term: &Value, nonce: &str) -> Value {
    json!({
        "$or": [
            {
                "source": [EVENT_SOURCE],
                "detail-type": [LAUNCH_SUCCESSFUL],
                "detail": { "AutoScalingGroupName": [group_term] },
            },
            {
                "source": [BARRIER_SOURCE],
                "detail": { "nonce": [nonce] },
            },
        ],
    })
}

/// The rule `name` with `pattern` and a queue of its own as its target; answers the queue's url.
async fn given_rule(router: &MultiServiceRouter, name: &str, pattern: &Value) -> String {
    let rule = events(
        router,
        "PutRule",
        json!({ "Name": name, "EventPattern": pattern.to_string(), "State": "ENABLED" }),
    )
    .await;
    let rule_arn = rule["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no RuleArn in {rule}"))
        .to_string();
    let url = given_queue(router, name, &rule_arn).await;
    let queue_arn = sqs(
        router,
        "GetQueueAttributes",
        json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
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
    url
}

/// Every envelope waiting on the queue at `url` right now, DELETING what it hands over.
async fn drain(router: &MultiServiceRouter, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        "ReceiveMessage",
        json!({ "QueueUrl": url, "MaxNumberOfMessages": 10, "WaitTimeSeconds": 1 }),
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

/// Poll the queue at `url` until an envelope of `LAUNCH_SUCCESSFUL` naming `group` arrives, and
/// answer it.
async fn launch_envelope(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    group: &str,
    seen: &mut Vec<Value>,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        seen.extend(drain(router, url).await);
        let found = seen.iter().find(|envelope| {
            envelope["detail-type"] == json!(LAUNCH_SUCCESSFUL)
                && envelope["detail"]["AutoScalingGroupName"] == json!(group)
        });
        if let Some(envelope) = found {
            return envelope.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a {LAUNCH_SUCCESSFUL} envelope naming {group} on the queue, \
             got {seen:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Assert the queue at `url` holds no envelope naming `group`, over a RUN of reads.
async fn assert_nothing_naming(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    group: &str,
    seen: &mut Vec<Value>,
) {
    for read in 0..HELD_READS {
        seen.extend(drain(router, url).await);
        let found = seen.iter().find(|envelope| {
            envelope["detail-type"] == json!(LAUNCH_SUCCESSFUL)
                && envelope["detail"]["AutoScalingGroupName"] == json!(group)
        });
        assert!(
            found.is_none(),
            "case {case:?}, read {read}: expected no envelope naming {group} on this queue, got \
             {found:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The instance ids the group `name` reports right now.
async fn group_instances(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[p("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    elements(&body, "InstanceId")
}

/// The ARN the group `name` reports.
async fn group_arn(router: &MultiServiceRouter, name: &str) -> String {
    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[p("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    element(&body, "AutoScalingGroupARN")
}

/// Wait until the group `name` reports exactly one instance, and answer its id.
async fn one_instance(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let held = group_instances(router, name).await;
        if held.len() == 1 {
            return held[0].clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the group {name} to hold one instance, got {held:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test]
async fn a_groups_initial_launch_reaches_every_rule_whose_pattern_names_its_group() {
    let router = router();

    // The network the groups place their instances in, created through Amazon EC2 itself.
    let vpc = element(
        &ec2(&router, "CreateVpc", &[p("CidrBlock", "10.0.0.0/16")]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            &router,
            "CreateSubnet",
            &[
                p("VpcId", &vpc),
                p("CidrBlock", "10.0.1.0/24"),
                p("AvailabilityZone", ZONE),
            ],
        )
        .await,
        "subnetId",
    );

    let group_a = format!("{FAMILY}a");
    let group_b = format!("{FAMILY}b");

    // One launch template per group, each named by the id Amazon EC2 minted for it.
    let mut templates: Vec<String> = Vec::new();
    for group in [&group_a, &group_b] {
        templates.push(element(
            &ec2(
                &router,
                "CreateLaunchTemplate",
                &[
                    p("LaunchTemplateName", &format!("{group}-template")),
                    p("LaunchTemplateData.ImageId", IMAGE_ID),
                    p("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
                ],
            )
            .await,
            "launchTemplateId",
        ));
    }

    // The table: the pattern shapes a client filters a group's launches by, each written as the
    // `$or` of the launch branch under test and a BARRIER branch of the caller's own — so one
    // `PutEvents` on that branch proves the rule, its target and the queue's policy are live
    // before any Auto Scaling activity is provoked.
    let exact_a = given_rule(
        &router,
        "asgfilter-exact-a",
        &or_pattern(&json!(group_a), "exact-a"),
    )
    .await;
    let exact_b = given_rule(
        &router,
        "asgfilter-exact-b",
        &or_pattern(&json!(group_b), "exact-b"),
    )
    .await;
    let family = given_rule(
        &router,
        "asgfilter-family",
        &or_pattern(&json!({ "prefix": FAMILY }), "family"),
    )
    .await;
    let other_family = given_rule(
        &router,
        "asgfilter-other-family",
        &or_pattern(&json!({ "prefix": OTHER_FAMILY }), "other-family"),
    )
    .await;

    // The barrier: one event of the caller's own source per rule, on that rule's own branch.
    for (case, url, nonce) in [
        ("the barrier on group a's rule", &exact_a, "exact-a"),
        ("the barrier on group b's rule", &exact_b, "exact-b"),
        ("the barrier on the family rule", &family, "family"),
        (
            "the barrier on the absent family's rule",
            &other_family,
            "other-family",
        ),
    ] {
        events(
            &router,
            "PutEvents",
            json!({ "Entries": [{
                "Source": BARRIER_SOURCE,
                "DetailType": BARRIER_DETAIL_TYPE,
                "Detail": json!({ "nonce": nonce }).to_string(),
            }] }),
        )
        .await;
        let deadline = Instant::now() + DEADLINE;
        loop {
            let held = drain(&router, url).await;
            if held
                .iter()
                .any(|envelope| envelope["detail"]["nonce"] == json!(nonce))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the barrier event must reach this queue before the groups are \
                 created, got {held:?}"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    // Both groups, each asking for its capacity IN ITS CREATE.
    for (group, template) in [(&group_a, &templates[0]), (&group_b, &templates[1])] {
        asg(
            &router,
            "CreateAutoScalingGroup",
            &[
                p("AutoScalingGroupName", group),
                p("MinSize", "0"),
                p("MaxSize", "1"),
                p("DesiredCapacity", "1"),
                p("LaunchTemplate.LaunchTemplateId", template),
                p("LaunchTemplate.Version", "$Latest"),
                p("VPCZoneIdentifier", &subnet),
            ],
        )
        .await;
    }

    let case = "a group's initial capacity";
    let instance_a = one_instance(&router, case, &group_a).await;
    let instance_b = one_instance(&router, case, &group_b).await;
    let arn_a = group_arn(&router, &group_a).await;
    let arn_b = group_arn(&router, &group_b).await;

    // The family rule receives BOTH launches.
    let mut family_seen: Vec<Value> = Vec::new();
    let family_a = launch_envelope(&router, case, &family, &group_a, &mut family_seen).await;
    let family_b = launch_envelope(&router, case, &family, &group_b, &mut family_seen).await;

    // Each exact rule receives its own group's launch, and never the sibling's.
    let mut seen_a: Vec<Value> = Vec::new();
    let delivered_a = launch_envelope(&router, case, &exact_a, &group_a, &mut seen_a).await;
    let mut seen_b: Vec<Value> = Vec::new();
    let delivered_b = launch_envelope(&router, case, &exact_b, &group_b, &mut seen_b).await;
    assert_nothing_naming(&router, case, &exact_a, &group_b, &mut seen_a).await;
    assert_nothing_naming(&router, case, &exact_b, &group_a, &mut seen_b).await;

    // And the rule whose prefix names nothing receives neither.
    let mut none_seen: Vec<Value> = Vec::new();
    for group in [&group_a, &group_b] {
        assert_nothing_naming(&router, case, &other_family, group, &mut none_seen).await;
    }

    // One publication per launch: the id the family rule saw is the id the exact rule saw.
    for (group, from_family, from_exact) in [
        (&group_a, &family_a, &delivered_a),
        (&group_b, &family_b, &delivered_b),
    ] {
        assert_eq!(
            from_family["id"], from_exact["id"],
            "case {case:?}: {group}'s launch is ONE event, so every matching rule delivers the \
             same id, got {from_family} and {from_exact}"
        );
    }

    // The envelope's own members, read off the group and the instance the API reports.
    for (group, instance, arn, envelope) in [
        (&group_a, &instance_a, &arn_a, &delivered_a),
        (&group_b, &instance_b, &arn_b, &delivered_b),
    ] {
        for (pointer, expected) in [
            ("/version", json!("0")),
            ("/source", json!(EVENT_SOURCE)),
            ("/detail-type", json!(LAUNCH_SUCCESSFUL)),
            ("/region", json!(REGION)),
            ("/detail/AutoScalingGroupName", json!(group)),
            ("/detail/EC2InstanceId", json!(instance)),
        ] {
            let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {pointer} to be {expected}, got {actual} in {envelope}"
            );
        }
        let resources = envelope["resources"].as_array().cloned().unwrap_or_default();
        // The account of the ARNs this envelope names is read off the group's OWN ARN, which the
        // API reported, rather than off the envelope being judged.
        let account = arn_account(arn);
        assert_eq!(
            envelope["account"],
            json!(account),
            "case {case:?}: the envelope's account is the account the group's ARN {arn} is under, \
             got {envelope}"
        );
        assert!(
            resources.contains(&json!(arn)),
            "case {case:?}: the envelope names the group's ARN {arn} in resources, got \
             {resources:?}"
        );
        let instance_arn = format!("arn:aws:ec2:{REGION}:{account}:instance/{instance}");
        assert!(
            resources.contains(&json!(instance_arn)),
            "case {case:?}: the envelope names the instance ARN {instance_arn} in resources, got \
             {resources:?}"
        );
        assert_eq!(
            resources.len(),
            2,
            "case {case:?}: the envelope names exactly the group and the instance in resources, \
             got {resources:?}"
        );
    }
}

/// The account segment of `arn`, which is the account every resource of this emulator is under.
fn arn_account(arn: &str) -> String {
    arn.split(':').nth(4).unwrap_or_default().to_string()
}
