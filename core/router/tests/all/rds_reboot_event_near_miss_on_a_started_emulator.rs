// SPDX-License-Identifier: CC0-1.0

//! The RDS -> EventBridge -> SQS near-miss routing scenario, driven against the emulator a
//! customer STARTS: the built binary, on its own port, with request authentication on,
//! reached over TLS with SigV4-signed calls.
//!
//! RULE: a lifecycle change Amazon RDS records for a DB instance is published once onto the
//! account's DEFAULT event bus in that instance's Region, and EventBridge delivers that one
//! envelope to the targets of EVERY rule whose pattern agrees with each field it pins and to
//! no rule that differs from the envelope in any one field — the pattern a rule is matched by
//! being the one it holds NOW, so a rule rewritten to the agreeing pattern is delivered the
//! next publication.
//!
//! The sibling suite [`crate::rds_reboot_event_near_miss_rules`] pins that rule on a router
//! assembled inside the test process, over the table of fields a near-miss rule can differ
//! in. This suite is the one that pins it on the emulator a client RUNS: the credentials are
//! verified, each call arrives over its own TLS connection, the publication Amazon RDS makes
//! through the emulator's own callback is served by the binary's multi-thread runtime, and
//! one process serves every call — so a chain that holds in process and not on the started
//! emulator is a failure a test reports rather than one only a client sees.
//!
//! ONE start is paid for here, since starting the binary costs what starting the emulator
//! costs, and it drives the whole scenario: the catalogs a client orders from, the network,
//! the DB instance, the agreeing rule, the three near-miss rules, the reboot, and the repair
//! leg in which each near-miss rule is repointed at the agreeing pattern and must then
//! receive. Every wait is on a CONDITION, under a generous deadline, and every assertion
//! quotes the emulator's own log, which is where a publication that could not be made says
//! so.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::Started;
use crate::query_wire::{element, elements, opt_element};

/// The Region the started emulator's published configuration names, which is the Region
/// [`Started`] signs and addresses every call for.
const REGION: &str = "us-east-1";

/// The account id every generated emulator presents, which the ARNs a pattern pins carry.
const ACCOUNT: &str = "000000000000";

/// The `source` and `detail-type` Amazon RDS publishes a DB instance change under, and the
/// detail-type of another RDS source type a near-miss row pins instead. Spelled out here
/// rather than imported from the service crate, so a wrong constant in the implementation
/// cannot make this suite pass.
const EVENT_SOURCE: &str = "aws.rds";
const DB_INSTANCE_EVENT: &str = "RDS DB Instance Event";
const DB_SNAPSHOT_EVENT: &str = "RDS DB Snapshot Event";

/// `detail.EventID` of the change a reboot records, and of ANOTHER change of the same source
/// type (the stop of a DB instance) a near-miss row pins instead.
///
/// Source: the per-source-type tables of "Amazon RDS event categories and event messages"
/// (Amazon RDS User Guide) — the DB instance rows whose messages are `DB instance restarted`
/// and `DB instance stopped`.
const RESTART_EVENT_ID: &str = "RDS-EVENT-0006";
const STOP_EVENT_ID: &str = "RDS-EVENT-0087";

/// The service principal EventBridge delivers to an SQS target as, which is what the queue's
/// resource policy admits.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The DB instance the scenario provisions and reboots.
const INSTANCE: &str = "emu-started-near-miss";

/// An identifier the account holds no DB instance for, so a row can pin an ARN that resolves
/// to nothing.
const ABSENT_INSTANCE: &str = "emu-started-absent";

/// The engine, class and storage the scenario orders.
const ENGINE: &str = "postgres";
const INSTANCE_CLASS: &str = "db.t4g.micro";
const ALLOCATED_STORAGE: &str = "20";

/// How long any one condition is waited for, and how often it is looked at. Generous next to
/// the spans this emulator's own transitions and in-process deliveries take.
const DEADLINE: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// One Amazon RDS call over its awsQuery wire, which must answer 200.
fn rds(emulator: &Started, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = emulator.form_call("rds", "2014-10-31", action, params);
    assert_eq!(
        status, 200,
        "case {action:?}: the call must be served, got HTTP {status} with {body}"
    );
    body
}

/// One Amazon EC2 call over its ec2Query wire, which must answer 200.
fn ec2(emulator: &Started, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = emulator.form_call("ec2", "2016-11-15", action, params);
    assert_eq!(
        status, 200,
        "case {action:?}: the call must be served, got HTTP {status} with {body}"
    );
    body
}

/// One RPC call, which must answer 200, as the JSON it answered.
fn rpc(
    emulator: &Started,
    signing_name: &str,
    target: &str,
    json_version: &str,
    body: &Value,
) -> Value {
    let (status, answered) = emulator.rpc(
        signing_name,
        target,
        &format!("application/x-amz-json-{json_version}"),
        body,
    );
    assert_eq!(
        status, 200,
        "case {target:?}: the call must be served, got HTTP {status} with {answered}"
    );
    serde_json::from_str(&answered)
        .unwrap_or_else(|error| panic!("case {target:?}: {answered} must be JSON: {error}"))
}

/// One EventBridge call.
fn events(emulator: &Started, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        "events",
        &format!("AWSEvents.{operation}"),
        "1.1",
        body,
    )
}

/// One Amazon SQS call.
fn sqs(emulator: &Started, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "1.0",
        body,
    )
}

/// A string member of a JSON answer, or a panic naming the case and what came back.
fn member(case: &str, body: &Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {pointer} in {body}"))
        .to_string()
}

/// The pattern that agrees with the restart of the instance at `arn` in every field it pins.
fn agreeing_pattern(arn: &str) -> Value {
    json!({
        "source": [EVENT_SOURCE],
        "detail-type": [DB_INSTANCE_EVENT],
        "detail": { "EventID": [RESTART_EVENT_ID], "SourceArn": [arn] },
    })
}

/// Put the ENABLED rule `name` on the default bus with `pattern`; answers its ARN.
fn put_rule(emulator: &Started, name: &str, pattern: &Value) -> String {
    let put = events(
        emulator,
        "PutRule",
        &json!({ "Name": name, "EventPattern": pattern.to_string(), "State": "ENABLED" }),
    );
    member(name, &put, "/RuleArn")
}

/// The queue `name`, admitting EventBridge's send on behalf of the rule `rule_arn` and
/// targeted by it; answers its URL.
fn queue_for_rule(emulator: &Started, name: &str, rule_arn: &str) -> String {
    let created = sqs(emulator, "CreateQueue", &json!({ "QueueName": name }));
    let url = member(name, &created, "/QueueUrl");
    let attributes = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    );
    let queue_arn = member(name, &attributes, "/Attributes/QueueArn");
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    });
    sqs(
        emulator,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": &url,
            "Attributes": { "Policy": document.to_string() },
        }),
    );
    let targeted = events(
        emulator,
        "PutTargets",
        &json!({
            "Rule": name,
            "Targets": [{ "Id": "the-queue", "Arn": queue_arn }],
        }),
    );
    assert_eq!(
        targeted["FailedEntryCount"].as_i64(),
        Some(0),
        "case {name:?}: the queue target must be accepted, got {targeted}"
    );
    url
}

/// Every envelope now waiting on the queue at `url`.
fn drain(emulator: &Started, url: &str) -> Vec<Value> {
    let received = sqs(
        emulator,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    );
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|message| {
            let raw = message["Body"].as_str().unwrap_or_default();
            serde_json::from_str::<Value>(raw).unwrap_or(Value::Null)
        })
        .collect()
}

/// Collect envelopes off the queue at `url` until one has arrived or [`DEADLINE`] passes.
fn collect_one(emulator: &Started, case: &str, url: &str) -> Value {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let arrived = drain(emulator, url);
        if let Some(envelope) = arrived.into_iter().next() {
            return envelope;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: no envelope reached the queue inside {DEADLINE:?}; the \
             emulator's log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Poll `DescribeDBInstances` until the instance reports `status`, or [`DEADLINE`] passes.
fn wait_for_status(emulator: &Started, case: &str, identifier: &str, status: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let described = rds(
            emulator,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", identifier)],
        );
        let seen = element(&described, "DBInstanceStatus");
        if seen == status {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected the instance to report {status:?} inside {DEADLINE:?}, \
             last saw {seen:?}"
        );
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// RULE: on the emulator a client starts, one published DB instance change reaches every rule
/// whose pattern agrees with each field it pins, no rule differing in one field, and a rule
/// repointed at the agreeing pattern on the next publication.
#[test]
fn a_reboot_reaches_the_agreeing_rule_on_a_started_emulator() {
    let emulator = Started::start();

    // ---- the catalogs a client orders from ----
    let versions = rds(
        &emulator,
        "DescribeDBEngineVersions",
        &[("Engine", ENGINE), ("DefaultOnly", "true")],
    );
    let version = element(&versions, "EngineVersion");
    let orderable = rds(
        &emulator,
        "DescribeOrderableDBInstanceOptions",
        &[
            ("Engine", ENGINE),
            ("EngineVersion", &version),
            ("DBInstanceClass", INSTANCE_CLASS),
        ],
    );
    assert!(
        opt_element(&orderable, "Marker").is_none(),
        "the filtered orderable-options catalog is one page, got {orderable}"
    );
    let offered = elements(&orderable, "Name");
    let zones = ec2(&emulator, "DescribeAvailabilityZones", &[]);
    let available: Vec<String> = elements(&zones, "zoneName")
        .into_iter()
        .filter(|zone| offered.contains(zone))
        .collect();
    assert!(
        available.len() >= 2,
        "the orderable catalog and EC2 must agree on at least two zones, got {available:?} \
         out of {offered:?}"
    );

    // ---- the network the DB subnet group is over ----
    let vpc = ec2(&emulator, "CreateVpc", &[("CidrBlock", "10.71.0.0/16")]);
    let vpc_id = element(&vpc, "vpcId");
    let mut subnets = Vec::new();
    for (index, zone) in available.iter().take(2).enumerate() {
        let subnet = ec2(
            &emulator,
            "CreateSubnet",
            &[
                ("VpcId", &vpc_id),
                ("CidrBlock", &format!("10.71.{index}.0/24")),
                ("AvailabilityZone", zone),
            ],
        );
        subnets.push(element(&subnet, "subnetId"));
    }
    rds(
        &emulator,
        "CreateDBSubnetGroup",
        &[
            ("DBSubnetGroupName", "emu-started-subnets"),
            ("DBSubnetGroupDescription", "the near-miss scenario's subnets"),
            ("SubnetIds.SubnetIdentifier.1", &subnets[0]),
            ("SubnetIds.SubnetIdentifier.2", &subnets[1]),
        ],
    );

    // ---- the DB instance, polled to the status the next step waits for ----
    let created = rds(
        &emulator,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", INSTANCE),
            ("Engine", ENGINE),
            ("DBInstanceClass", INSTANCE_CLASS),
            ("AllocatedStorage", ALLOCATED_STORAGE),
            ("DBSubnetGroupName", "emu-started-subnets"),
        ],
    );
    let arn = element(&created, "DBInstanceArn");
    assert_eq!(
        arn,
        format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{INSTANCE}"),
        "the instance's ARN is the one a client writes its rule patterns over"
    );
    wait_for_status(&emulator, "the created instance", INSTANCE, "available");

    // ---- the agreeing rule and the three near-miss rules, each with its own queue ----
    let absent_arn = format!("arn:aws:rds:{REGION}:{ACCOUNT}:db:{ABSENT_INSTANCE}");
    let near_misses: [(&str, &str, Value); 3] = [
        (
            "another event identifier of the same source type",
            "emu-started-event-id",
            json!({
                "source": [EVENT_SOURCE],
                "detail-type": [DB_INSTANCE_EVENT],
                "detail": { "EventID": [STOP_EVENT_ID], "SourceArn": [&arn] },
            }),
        ),
        (
            "the source ARN of an instance the account holds no record of",
            "emu-started-source-arn",
            agreeing_pattern(&absent_arn),
        ),
        (
            "the detail-type of another RDS source type",
            "emu-started-detail-type",
            json!({
                "source": [EVENT_SOURCE],
                "detail-type": [DB_SNAPSHOT_EVENT],
                "detail": { "EventID": [RESTART_EVENT_ID], "SourceArn": [&arn] },
            }),
        ),
    ];
    let agreeing_rule = put_rule(&emulator, "emu-started-agreeing", &agreeing_pattern(&arn));
    let agreeing_queue = queue_for_rule(&emulator, "emu-started-agreeing", &agreeing_rule);
    let near: Vec<(&str, &str, String)> = near_misses
        .iter()
        .map(|(case, slug, pattern)| {
            let rule = put_rule(&emulator, slug, pattern);
            let url = queue_for_rule(&emulator, slug, &rule);
            (*case, *slug, url)
        })
        .collect();

    // ---- the reboot, which records and publishes the restart ----
    rds(
        &emulator,
        "RebootDBInstance",
        &[("DBInstanceIdentifier", INSTANCE)],
    );

    // The agreeing rule's queue is the positive control AND the witness for the rows below:
    // one publication is matched against every rule of the bus, so once it has arrived here
    // every other rule has been decided against the same event.
    let delivered = collect_one(&emulator, "the agreeing pattern", &agreeing_queue);
    let first_date = assert_restart_of("the agreeing pattern", &delivered, &arn);
    for (case, _, url) in &near {
        assert_holds_nothing(&emulator, case, url);
    }

    // ---- the repair: each near-miss rule repointed at the agreeing pattern ----
    wait_for_status(&emulator, "the rebooted instance", INSTANCE, "available");
    for (_, slug, _) in &near {
        put_rule(&emulator, slug, &agreeing_pattern(&arn));
    }
    rds(
        &emulator,
        "RebootDBInstance",
        &[("DBInstanceIdentifier", INSTANCE)],
    );
    for (case, _, url) in &near {
        let delivered = collect_one(&emulator, case, url);
        let dated = assert_restart_of(case, &delivered, &arn);
        // The envelope a repointed rule receives is the SECOND restart's own record, dated
        // when that restart happened, not the first restart again.
        assert_ne!(
            dated, first_date,
            "case {case:?}: expected the second restart's own date, got the first one \
             ({first_date:?}) in {delivered}"
        );
    }
}

/// Assert `envelope` is the envelope of the restart of the instance at `arn`, and answer the
/// `detail.Date` it carries.
fn assert_restart_of(case: &str, envelope: &Value, arn: &str) -> String {
    for (pointer, expected) in [
        ("/source", json!(EVENT_SOURCE)),
        ("/detail-type", json!(DB_INSTANCE_EVENT)),
        ("/account", json!(ACCOUNT)),
        ("/region", json!(REGION)),
        ("/resources", json!([arn])),
        ("/detail/EventID", json!(RESTART_EVENT_ID)),
        ("/detail/SourceArn", json!(arn)),
    ] {
        let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {pointer} to be {expected}, got {actual} in {envelope}"
        );
    }
    let dated = envelope
        .pointer("/detail/Date")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        dated.ends_with('Z') && dated.contains('T'),
        "case {case:?}: expected the detail to state when the change happened, got \
         {dated:?} in {envelope}"
    );
    dated
}

/// Assert the queue holds nothing: both depth attributes report zero, and a read hands over
/// no message at all.
fn assert_holds_nothing(emulator: &Started, case: &str, url: &str) {
    let reported = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "AttributeNames": [
                "ApproximateNumberOfMessages",
                "ApproximateNumberOfMessagesNotVisible",
            ],
        }),
    );
    for attribute in [
        "ApproximateNumberOfMessages",
        "ApproximateNumberOfMessagesNotVisible",
    ] {
        assert_eq!(
            reported["Attributes"][attribute],
            json!("0"),
            "case {case:?}: expected {attribute} \"0\", got {reported}"
        );
    }
    let held = drain(emulator, url);
    assert!(
        held.is_empty(),
        "case {case:?}: expected no message on the queue, got {held:?}"
    );
}
