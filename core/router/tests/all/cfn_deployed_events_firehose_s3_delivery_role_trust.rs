// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed EventBridge -> Firehose -> Amazon S3 pipeline MOVES DATA: an
//! event published onto the bus after the stack settles becomes an object under the Amazon S3
//! prefix that stack's delivery stream declared — and it becomes one only where the declared
//! delivery role is one Firehose can assume.
//!
//! The rule under test: **a resource CloudFormation provisions is created through its owning
//! service's own API, so a stack-deployed rule and delivery stream move data exactly as a
//! directly created pair does.** For every target of a rule whose `EventPattern` matched,
//! EventBridge makes the call that target's service is invoked by — `firehose:PutRecord` for a
//! delivery stream — ACTING AS the target's `RoleArn` ("EventBridge relies on IAM roles that
//! you specify in the `RoleARN` argument in `PutTargets`"), and the stream then writes the
//! flushed buffer under its destination's declared `Prefix` using the destination's own
//! `RoleARN` ("the Amazon Resource Name (ARN) of the Amazon Web Services credentials"), a role
//! Firehose has to be able to assume. So the object appears only where BOTH roles admit the
//! call made under them, and a declared delivery role whose trust policy names another service
//! principal leaves that prefix permanently empty instead.
//!
//! Only a cross-service suite can ask this: the flow spans five services over ONE engine
//! exactly as `core/router/src/lib.rs` mounts them — CloudFormation to deploy, IAM for the two
//! roles the two calls are made under, EventBridge for the rule and the publication, Firehose
//! for the delivery stream, and Amazon S3 for the object the delivery writes. The two halves
//! are pinned separately beside this suite (`events_rule_firehose_target_delivery` for a
//! directly created pair, `cloudformation_firehose_stream_direct_put_delivers` for a deployed
//! stream taking a direct put); what only this suite reaches is their COMPOSITION, which is
//! the pipeline a client actually deploys.
//!
//! The rows differ along what the rule generalises over: the trust policy of each of the two
//! roles the pipeline is driven under (the service principal that assumes it, or another
//! service's), what the target role's own policy allows (the record write, or another action),
//! what the target receives (the published envelope, or its own constant `Input`), and whether
//! the destination declares a key `Prefix` at all. Three rows must deliver nothing and two
//! must deliver, so neither answer can be had from a store that writes everything or nothing.
//! Nothing is asserted about how fast the host ran: a prefix that is due is polled until it
//! holds its object, and a prefix that must stay empty is re-read at every one of those
//! readings.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The request builder, the two policy builders and the Amazon S3 / EventBridge readers the
// directly created half of this pipeline already exports: ONE definition of each leg of this
// flow, so the deployed pipeline is driven and read over exactly the wire the directly
// created one is. Every call this suite makes — CloudFormation's included — goes through that
// one builder, so every service of the pipeline is addressed in the same Region.
use crate::events_rule_fans_out_to_firehose_and_queue_targets::{
    delivered_object, events, keys_under,
};
use crate::events_rule_firehose_target_delivery::{identity_policy, send, trust_policy};
use crate::query_wire::{element, elements, form_encode, outputs_of};

/// How long a stack is given to report itself settled, and how often it is asked. The spans
/// the services behind a stack choose take the time they take, so the bound is a generous
/// wall-clock deadline whose only job is that the wait TERMINATES: many times over what the
/// deployment takes here, because a host running every suite of this repository at once runs
/// each of them many times slower, and a row the clock would have served must not fail for
/// that.
const DEPLOY_DEADLINE: Duration = Duration::from_secs(150);

/// How long a delivery that IS due is waited for before a row fails: far longer than the
/// emulator's own delivery span, for the same reason.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(100);

/// How long the prefixes that must hold NOTHING go on being re-read once every due prefix
/// holds its object — spent re-reading them rather than sleeping once, so what the assertion
/// judges is the latest reading.
const SILENCE_WINDOW: Duration = Duration::from_secs(2);

/// How often a stack, or a prefix, is read while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `StackStatus` a deployment whose declarations every owning service accepts settles in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The service principal Firehose assumes a destination's role as, the one EventBridge
/// assumes a target's role as, and one that is NEITHER: a role trusting only the last cannot
/// be assumed by either of them.
const FIREHOSE_PRINCIPAL: &str = "firehose.amazonaws.com";
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const OTHER_PRINCIPAL: &str = "ec2.amazonaws.com";

/// The action EventBridge's call on a delivery stream performs, which a target's role has to
/// be allowed to make, and one Firehose operation that is NOT it.
const PUT_RECORD_ACTION: &str = "firehose:PutRecord";
const OTHER_FIREHOSE_ACTION: &str = "firehose:DescribeDeliveryStream";

/// The `detail-type` every publication below carries, and the constant one row's target
/// receives instead of the envelope (`Target$Input`: "valid JSON text passed to the target").
const DETAIL_TYPE: &str = "Order Created";
const CONSTANT_INPUT: &str = r#"{"fixed":"constant-input"}"#;

/// What a row's destination prefix must hold once the publication is served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The published envelope reached the stream and was delivered.
    Envelope,
    /// The target's own constant input reached the stream and was delivered.
    Constant,
    /// Nothing was ever written: one of the two roles does not admit the call made under it.
    Nothing,
}

/// One deployed pipeline: the roles its template declares, what its target receives, and what
/// its destination prefix then holds.
struct Row {
    case: &'static str,
    /// The stack this row deploys, which every other name it uses is derived from.
    stack: &'static str,
    /// The service principal the DELIVERY role (the destination's `RoleARN`) trusts.
    delivery_trusts: &'static str,
    /// The service principal the TARGET role (the rule target's `RoleArn`) trusts.
    target_trusts: &'static str,
    /// The action the target role's own policy allows.
    target_allows: &'static str,
    /// The target's own `Input`, or `None` for a target taking the event as published.
    input: Option<&'static str>,
    /// The `Prefix` the destination declares, or `None` where it declares none at all.
    prefix: Option<&'static str>,
    /// Whether every declaration in the template is one its owning service accepts, so the
    /// deployment settles in [`CREATE_COMPLETE`].
    deploys: bool,
    expect: Expect,
}

impl Row {
    /// The bucket the template declares, and the one its objects are read back from.
    fn bucket(&self) -> String {
        format!("{}-data", self.stack)
    }

    /// The role the template declares the DELIVERY under.
    fn delivery_role(&self) -> String {
        format!("{}-delivery", self.stack)
    }

    /// The role the template declares the rule TARGET under.
    fn target_role(&self) -> String {
        format!("{}-target", self.stack)
    }

    /// The `DeliveryStreamName` the template declares.
    fn stream(&self) -> String {
        format!("{}-stream", self.stack)
    }

    /// The rule name the template declares.
    fn rule(&self) -> String {
        format!("{}-rule", self.stack)
    }

    /// The event `source` this row's rule matches on, which no other row's rule matches: one
    /// publication carries every row's entry, so what differs between the rows is the roles
    /// and not which entries reached them.
    fn source(&self) -> String {
        format!("com.demo.{}", self.stack)
    }

    /// The `Detail` this row's entry carries, as the JSON text `PutEvents` takes.
    fn detail(&self) -> String {
        json!({ "marker": self.stack }).to_string()
    }

    /// The prefix this row's delivered object is read back from: the declared one, or the
    /// bucket's root where the destination declares none.
    fn read_prefix(&self) -> String {
        self.prefix.unwrap_or_default().to_string()
    }

    /// The row's template: the destination bucket, the two roles, the delivery stream writing
    /// into that bucket, and the rule whose target is that stream.
    fn template(&self) -> String {
        let mut destination = json!({
            "BucketARN": { "Fn::GetAtt": ["Bucket", "Arn"] },
            "RoleARN": { "Fn::GetAtt": ["DeliveryRole", "Arn"] },
            "CompressionFormat": "UNCOMPRESSED",
            // The shortest buffering the hints admit (published ranges `SizeInMBs` 1..128,
            // `IntervalInSeconds` 0..900), so a row waits for the delivery and not for an
            // interval it would sit out.
            "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
        });
        if let Some(prefix) = self.prefix {
            destination["Prefix"] = json!(prefix);
        }
        let mut target = json!({
            "Id": "t1",
            "Arn": { "Fn::GetAtt": ["Stream", "Arn"] },
            "RoleArn": { "Fn::GetAtt": ["TargetRole", "Arn"] },
        });
        if let Some(input) = self.input {
            target["Input"] = json!(input);
        }
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Description": "an EventBridge rule delivering through Firehose to S3",
            "Resources": {
                "Bucket": {
                    "Type": "AWS::S3::Bucket",
                    "Properties": { "BucketName": self.bucket() },
                },
                "DeliveryRole": {
                    "Type": "AWS::IAM::Role",
                    "Properties": {
                        "RoleName": self.delivery_role(),
                        "AssumeRolePolicyDocument": trust_policy(self.delivery_trusts),
                        "Policies": [{
                            "PolicyName": "delivery",
                            "PolicyDocument": identity_policy("s3:PutObject"),
                        }],
                    },
                },
                "TargetRole": {
                    "Type": "AWS::IAM::Role",
                    "Properties": {
                        "RoleName": self.target_role(),
                        "AssumeRolePolicyDocument": trust_policy(self.target_trusts),
                        "Policies": [{
                            "PolicyName": "invoke",
                            "PolicyDocument": identity_policy(self.target_allows),
                        }],
                    },
                },
                "Stream": {
                    "Type": "AWS::KinesisFirehose::DeliveryStream",
                    "Properties": {
                        "DeliveryStreamName": self.stream(),
                        "DeliveryStreamType": "DirectPut",
                        "ExtendedS3DestinationConfiguration": destination,
                    },
                },
                "Rule": {
                    "Type": "AWS::Events::Rule",
                    "Properties": {
                        "Name": self.rule(),
                        "EventPattern": { "source": [self.source()] },
                        "State": "ENABLED",
                        "Targets": [target],
                    },
                },
            },
            "Outputs": {
                "StreamName": { "Value": { "Ref": "Stream" } },
                "RuleName": { "Value": { "Ref": "Rule" } },
            },
        })
        .to_string()
    }
}

/// The deployed pipelines under test.
const ROWS: &[Row] = &[
    Row {
        case: "both roles assumable, delivering the envelope under a declared prefix",
        stack: "cfn-ebfh-good",
        delivery_trusts: FIREHOSE_PRINCIPAL,
        target_trusts: EVENTS_PRINCIPAL,
        target_allows: PUT_RECORD_ACTION,
        input: None,
        prefix: Some("raw/events/"),
        deploys: true,
        expect: Expect::Envelope,
    },
    Row {
        case: "a target carrying its own constant input, to a destination declaring no prefix",
        stack: "cfn-ebfh-constant",
        delivery_trusts: FIREHOSE_PRINCIPAL,
        target_trusts: EVENTS_PRINCIPAL,
        target_allows: PUT_RECORD_ACTION,
        input: Some(CONSTANT_INPUT),
        prefix: None,
        deploys: true,
        expect: Expect::Constant,
    },
    Row {
        case: "a delivery role trusting only another service principal",
        stack: "cfn-ebfh-untrusted-delivery",
        delivery_trusts: OTHER_PRINCIPAL,
        target_trusts: EVENTS_PRINCIPAL,
        target_allows: PUT_RECORD_ACTION,
        input: None,
        prefix: Some("raw/events/"),
        // The destination's role is resolved by the owning service when the stream is
        // written, so this declaration is one Firehose itself refuses: the deployment reports
        // that refusal rather than completing.
        deploys: false,
        expect: Expect::Nothing,
    },
    Row {
        case: "a target role trusting only another service principal",
        stack: "cfn-ebfh-untrusted-target",
        delivery_trusts: FIREHOSE_PRINCIPAL,
        target_trusts: OTHER_PRINCIPAL,
        target_allows: PUT_RECORD_ACTION,
        input: None,
        prefix: Some("raw/events/"),
        deploys: true,
        expect: Expect::Nothing,
    },
    Row {
        case: "a target role whose policy allows another action",
        stack: "cfn-ebfh-unpermitted-target",
        delivery_trusts: FIREHOSE_PRINCIPAL,
        target_trusts: EVENTS_PRINCIPAL,
        target_allows: OTHER_FIREHOSE_ACTION,
        input: None,
        prefix: Some("raw/events/"),
        deploys: true,
        expect: Expect::Nothing,
    },
];

/// The emulator the suite drives: the five services the pipeline spans, on ONE engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signature verification is the emulator's concern, not any of these services'.
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
        service_com_amazonaws_firehose_firehose_20150804::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

#[tokio::test]
async fn a_stack_deployed_rule_delivers_its_events_through_firehose_to_s3() {
    let router = emulator();

    // Every stack is created before any of them is awaited, so the suite spends ONE
    // provisioning span rather than one per row.
    for row in ROWS {
        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &row.template()),
                // Every template declares the two IAM roles its pipeline runs under, by name.
                ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from CreateStack, got {status} with body {body}",
            row.case
        );
    }

    for row in ROWS {
        let described = await_settled(&router, row).await;
        let reported = element(&described, "StackStatus");
        if row.deploys {
            assert_eq!(
                reported, CREATE_COMPLETE,
                "case {:?}: expected the stack to reach {CREATE_COMPLETE}, got {reported} in \
                 {described}",
                row.case
            );
            let outputs = outputs_of(&described);
            assert_eq!(
                outputs.get("StreamName").map(String::as_str),
                Some(row.stream().as_str()),
                "case {:?}: expected the stack to publish its declared stream name, got \
                 {outputs:?}",
                row.case
            );
            assert_eq!(
                outputs.get("RuleName").map(String::as_str),
                Some(row.rule().as_str()),
                "case {:?}: expected the stack to publish its declared rule name, got \
                 {outputs:?}",
                row.case
            );
        } else {
            assert_ne!(
                reported, CREATE_COMPLETE,
                "case {:?}: a declaration its owning service refuses is no deployed pipeline, \
                 so the stack must report the refusal; got {reported} in {described}",
                row.case
            );
            // And the refusal is a DIAGNOSIS the client can act on rather than a silent empty
            // prefix: the stack names the resource it could not carry through, and that
            // resource's event carries the owning service's own message, which names the role
            // the declaration pointed Firehose at.
            assert!(
                element(&described, "StackStatusReason").contains("Stream"),
                "case {:?}: expected the stack's reason to name the resource that failed, got \
                 {described}",
                row.case
            );
            let reasons = failure_reasons(&router, row).await;
            assert!(
                reasons
                    .iter()
                    .any(|reason| reason.contains(&row.delivery_role())),
                "case {:?}: expected an event diagnosing the stream by naming the declared \
                 delivery role {:?}, got {reasons:?}",
                row.case,
                row.delivery_role()
            );
        }
    }

    // ONE publication carrying every row's entry: each row's rule matches its own source, so
    // what differs between the rows is which calls their roles admit.
    let entries: Vec<Value> = ROWS
        .iter()
        .map(|row| {
            json!({
                "Source": row.source(),
                "DetailType": DETAIL_TYPE,
                "Detail": row.detail(),
            })
        })
        .collect();
    let published = events(
        &router,
        "the publication",
        "PutEvents",
        json!({ "Entries": entries }),
    )
    .await;
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "the publication must be accepted whole: {published}"
    );

    // The rows whose two roles both admit the call hold their object; the others hold nothing
    // at every reading taken meanwhile.
    await_delivered(&router, ROWS).await;
    // And they still hold nothing after as long a wait again, re-read throughout it.
    let until = Instant::now() + SILENCE_WINDOW;
    while Instant::now() < until {
        assert_holding_nothing(&router, ROWS).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert_holding_nothing(&router, ROWS).await;

    // Each delivered object is what its TARGET received, under the destination's declared
    // prefix and the automatic time segments.
    for row in ROWS.iter().filter(|row| row.expect != Expect::Nothing) {
        let case = row.case;
        let keys = keys_of(&router, row).await;
        assert_eq!(
            keys.len(),
            1,
            "case {case:?}: one flushed buffer is one object, got {keys:?}"
        );
        assert_key_layout(row, &keys[0]);
        let body = delivered_object(&router, case, &row.bucket(), &row.read_prefix()).await;
        match row.expect {
            Expect::Constant => {
                let expected = row.input.unwrap_or_default();
                assert_eq!(
                    body, expected,
                    "case {case:?}: expected the target's own input {expected:?}, got {body:?}"
                );
            }
            Expect::Envelope => {
                for expected in [row.source(), DETAIL_TYPE.to_string(), row.detail()] {
                    assert!(
                        body.contains(&expected),
                        "case {case:?}: the delivered object carries the published envelope, \
                         expected {expected:?} in {body:?}"
                    );
                }
            }
            Expect::Nothing => unreachable!("filtered out above"),
        }
    }
}

/// Poll `row`'s stack until it first reports a settled status, and answer the `DescribeStacks`
/// body that status was read from.
async fn await_settled(router: &MultiServiceRouter, row: &Row) -> String {
    let deadline = Instant::now() + DEPLOY_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", row.stack)]).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}",
            row.case
        );
        let reported = element(&body, "StackStatus");
        if reported.ends_with("_COMPLETE") || reported.ends_with("_FAILED") {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: expected the stack to settle, last saw {reported:?} in {body}",
            row.case
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every `ResourceStatusReason` `row`'s stack events carry, read through
/// `DescribeStackEvents` — where the owning service's own refusal of a resource is reported
/// ("Success/failure message associated with the resource").
async fn failure_reasons(router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", row.stack)]).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body {body}",
        row.case
    );
    elements(&body, "ResourceStatusReason")
}

/// Wait until every prefix that must be delivered to holds an object, requiring every prefix
/// that must stay empty to hold nothing at each reading taken meanwhile.
async fn await_delivered(router: &MultiServiceRouter, rows: &[Row]) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        assert_holding_nothing(router, rows).await;
        let mut pending: Vec<&str> = Vec::new();
        for row in rows {
            if row.expect != Expect::Nothing && keys_of(router, row).await.is_empty() {
                pending.push(row.case);
            }
        }
        if pending.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected a delivered object for each of {pending:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Require every prefix that must stay empty to hold nothing right now.
async fn assert_holding_nothing(router: &MultiServiceRouter, rows: &[Row]) {
    for row in rows {
        if row.expect != Expect::Nothing {
            continue;
        }
        let keys = keys_of(router, row).await;
        assert!(
            keys.is_empty(),
            "case {:?}: a call neither role admits writes nothing, so this prefix stays \
             empty; got {keys:?}",
            row.case
        );
    }
}

/// Every key `row`'s destination prefix holds, read through the one Amazon S3 reader this
/// flow's suites share.
///
/// A row whose deployment reported a refusal is the one row that may have no bucket at all —
/// the stack it declared its bucket in did not complete — so that row, and only that row, is
/// asked whether Amazon S3 holds the bucket first and reads as holding nothing when it does
/// not. Every DEPLOYED row's listing has to be served: "this prefix is empty" must not be
/// satisfiable by a bucket that is not there.
async fn keys_of(router: &MultiServiceRouter, row: &Row) -> Vec<String> {
    if !row.deploys && !holds_bucket(router, row).await {
        return Vec::new();
    }
    keys_under(router, row.case, &row.bucket(), &row.read_prefix()).await
}

/// Whether Amazon S3 holds `row`'s bucket, asked with `HeadBucket` — the operation S3
/// documents for the question ("You can use this operation to determine if a bucket
/// exists").
async fn holds_bucket(router: &MultiServiceRouter, row: &Row) -> bool {
    let (status, _) = send(
        router,
        "s3",
        "HEAD",
        &format!("/{}", row.bucket()),
        &[],
        Vec::new(),
    )
    .await;
    status == 200
}

/// Require `key` to be the destination's declared `Prefix`, then the automatic
/// `YYYY/MM/DD/HH` segments ("The \"YYYY/MM/DD/HH\" time format prefix is automatically used
/// for delivered Amazon S3 files"), then a name beginning with the stream's own.
fn assert_key_layout(row: &Row, key: &str) {
    let case = row.case;
    let declared = row.prefix.unwrap_or_default();
    let rest = key.strip_prefix(declared).unwrap_or_else(|| {
        panic!(
            "case {case:?}: the key must begin with the declared prefix {declared:?}, got \
             {key:?}"
        )
    });
    let segments: Vec<&str> = rest.split('/').collect();
    assert!(
        segments.len() == 5,
        "case {case:?}: the automatic YYYY/MM/DD/HH segments and the object's own name follow \
         the declared prefix, got {rest:?} in key {key:?}"
    );
    // The widths of those four segments, as the documented `YYYY/MM/DD/HH` format spells them.
    for (width, segment) in [4, 2, 2, 2].iter().zip(&segments) {
        assert!(
            segment.len() == *width && segment.bytes().all(|byte| byte.is_ascii_digit()),
            "case {case:?}: expected a {width}-digit time segment, got {segment:?} in key \
             {key:?}"
        );
    }
    assert!(
        segments[4].starts_with(&format!("{}-", row.stream())),
        "case {case:?}: the object's name begins with its stream, got {:?} in key {key:?}",
        segments[4]
    );
}

/// One CloudFormation action over its awsQuery wire, driven through the same request builder
/// as every other call this suite makes — so the deployment and the pipeline it deploys are
/// addressed in one Region. The form components go through the test crate's one awsQuery
/// encoder, so a template body full of `{`, `"` and `&` survives it.
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
    let (status, answered) = send(
        router,
        "cloudformation",
        "POST",
        "/",
        &[("content-type", "application/x-www-form-urlencoded")],
        body.into_bytes(),
    )
    .await;
    (status, String::from_utf8_lossy(&answered).to_string())
}
