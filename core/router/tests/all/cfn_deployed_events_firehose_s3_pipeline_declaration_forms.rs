// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation-deployed EventBridge -> Firehose -> Amazon S3 pipeline delivers whichever
//! way its template SPELLS the wiring: the Amazon S3 destination under either of the two
//! destination properties the type publishes, the three ARNs the pipeline is wired by read
//! either as an `Fn::GetAtt` attribute or as an ARN an `Fn::Sub` builds from the pseudo
//! parameters and a resource's `Ref`, and the stream's type stated or left to its default.
//!
//! The rule under test: **CloudFormation resolves every declared property — an intrinsic
//! function included — to the one value the owning service is then asked to create the
//! resource with, so two templates whose declarations resolve to the same identities deploy
//! the same working pipeline, and the deployed rule's declared `State` is what decides whether
//! a publication reaches it.** `Ref` of a bucket, a role and a delivery stream each return
//! that resource's name and `Fn::GetAtt … Arn` its ARN, so an ARN composed out of the former
//! and an ARN read from the latter name the SAME resource; `S3DestinationConfiguration` and
//! `ExtendedS3DestinationConfiguration` are two spellings of one Amazon S3 destination (the
//! stream reports both under `S3DestinationDescription`); an omitted `DeliveryStreamType`
//! leaves the stream at its documented `DirectPut` default; and a rule whose `State` is
//! `DISABLED` is one "EventBridge does not match any events against".
//!
//! Only a cross-service suite can ask this: the flow spans the five services
//! `core/router/src/lib.rs` mounts on ONE engine — CloudFormation to deploy, IAM for the two
//! roles the pipeline's two calls are made under, EventBridge for the rule and the
//! publication, Firehose for the delivery stream, and Amazon S3 for the delivered object. The
//! ROLE halves of this contract are pinned beside this suite
//! (`cfn_deployed_events_firehose_s3_delivery_role_trust`); what this one adds is the
//! DECLARATION FORM: that the pipeline is the same pipeline however the template writes it
//! down, read back out of each owning service and then driven end to end.
//!
//! The rows differ along what the rule generalises over: which destination property carries
//! the Amazon S3 location, which intrinsic form each ARN is read in, whether the destination
//! declares a key `Prefix`, whether `DeliveryStreamType` is stated at all, and the rule's
//! declared `State`. Three rows must deliver and one must deliver nothing, so neither answer
//! can come from a store that writes everything or nothing. Nothing is asserted about how fast
//! the host ran: a prefix that is due is polled until it holds its object, and the prefix that
//! must stay empty is re-read at every one of those readings.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The CloudFormation caller, the EventBridge/Firehose callers, the two policy builders and the
// Amazon S3 readers this flow's suites already share: ONE definition of each leg, so a
// deployment driven here is driven over exactly the wire the suites beside it use, and every
// call lands in the same Region.
use crate::cfn_deployed_events_firehose_s3_delivery_role_trust::cloudformation;
use crate::events_rule_fans_out_to_firehose_and_queue_targets::{
    delivered_object, events, firehose, keys_under,
};
use crate::events_rule_firehose_target_delivery::{identity_policy, trust_policy, REGION};
use crate::query_wire::{element, outputs_of};

/// How long a stack is given to report itself settled. The spans the services behind a stack
/// choose take the time they take, so this is a generous wall-clock deadline whose only job is
/// that the wait TERMINATES: many times over what the deployment takes here, because a host
/// running every suite of this repository at once runs each of them many times slower.
const DEPLOY_DEADLINE: Duration = Duration::from_secs(150);

/// How long a delivery that IS due is waited for before a row fails, for the same reason.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(100);

/// How long the prefix that must hold NOTHING goes on being re-read once every due prefix holds
/// its object — spent re-reading it rather than sleeping once, so what the assertion judges is
/// the latest reading.
const SILENCE_WINDOW: Duration = Duration::from_secs(2);

/// How often a stack, or a prefix, is read while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `StackStatus` a deployment whose declarations every owning service accepts settles in.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `DeliveryStreamStatus` a stream that takes data reports: "After the Firehose stream is
/// created, its status is ACTIVE and it now accepts data" (`CreateDeliveryStream`).
const ACTIVE: &str = "ACTIVE";

/// The `DeliveryStreamType` an `AWS::KinesisFirehose::DeliveryStream` that states none is
/// created with: "DirectPut: Provider applications access the Firehose stream directly", the
/// member's documented default.
const DIRECT_PUT: &str = "DirectPut";

/// The service principal Firehose assumes a destination's role as, and the one EventBridge
/// assumes a rule target's role as.
const FIREHOSE_PRINCIPAL: &str = "firehose.amazonaws.com";
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// What the delivery role is allowed to do to the bucket, and the call EventBridge makes on a
/// delivery-stream target, which the target role is allowed to make.
const PUT_OBJECT_ACTION: &str = "s3:PutObject";
const PUT_RECORD_ACTION: &str = "firehose:PutRecord";

/// The two `RuleState` values a deployed rule is declared in here: "ENABLED: ... EventBridge
/// matches events against the rule" and "DISABLED: ... EventBridge does not match any events
/// against the rule" (`PutRule$State`).
const ENABLED: &str = "ENABLED";
const DISABLED: &str = "DISABLED";

/// The `detail-type` every publication below carries.
const DETAIL_TYPE: &str = "Order Created";

/// Which destination property a row's template declares the Amazon S3 location under. Both are
/// published by `AWS::KinesisFirehose::DeliveryStream` and both are `CreateDeliveryStream`
/// members (`smithy_query(operation="CreateDeliveryStream")`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// `S3DestinationConfiguration`.
    Plain,
    /// `ExtendedS3DestinationConfiguration`.
    Extended,
}

impl Destination {
    /// The property name the template writes.
    fn property(self) -> &'static str {
        match self {
            Destination::Plain => "S3DestinationConfiguration",
            Destination::Extended => "ExtendedS3DestinationConfiguration",
        }
    }
}

/// How a row's template NAMES the resources its pipeline is wired by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reference {
    /// `Fn::GetAtt` of the declaring resource's `Arn` attribute.
    GetAtt,
    /// An ARN an `Fn::Sub` composes from the pseudo parameters and the resource's `Ref`, which
    /// returns that resource's name.
    Sub,
}

impl Reference {
    /// The bucket ARN as this form writes it.
    fn bucket_arn(self) -> Value {
        match self {
            Reference::GetAtt => json!({ "Fn::GetAtt": ["Bucket", "Arn"] }),
            Reference::Sub => json!({ "Fn::Sub": "arn:${AWS::Partition}:s3:::${Bucket}" }),
        }
    }

    /// One declared role's ARN as this form writes it.
    fn role_arn(self, logical_id: &str) -> Value {
        match self {
            Reference::GetAtt => json!({ "Fn::GetAtt": [logical_id, "Arn"] }),
            Reference::Sub => json!({
                "Fn::Sub": format!(
                    "arn:${{AWS::Partition}}:iam::${{AWS::AccountId}}:role/${{{logical_id}}}"
                )
            }),
        }
    }

    /// The delivery stream's ARN as this form writes it.
    fn stream_arn(self) -> Value {
        match self {
            Reference::GetAtt => json!({ "Fn::GetAtt": ["Stream", "Arn"] }),
            Reference::Sub => json!({
                "Fn::Sub": "arn:${AWS::Partition}:firehose:${AWS::Region}:${AWS::AccountId}\
                            :deliverystream/${Stream}"
            }),
        }
    }
}

/// One deployed pipeline: how its template spells the wiring, and whether a publication its
/// rule's pattern matches then reaches the bucket.
struct Row {
    case: &'static str,
    /// The stack this row deploys, which every other name it uses is derived from.
    stack: &'static str,
    destination: Destination,
    reference: Reference,
    /// The `Prefix` the destination declares, or `None` where it declares none at all.
    prefix: Option<&'static str>,
    /// The `DeliveryStreamType` the template states, or `None` to leave it to its default.
    stream_type: Option<&'static str>,
    /// The `State` the rule is declared in.
    state: &'static str,
    /// Whether the published event reaches this row's bucket at all.
    delivers: bool,
}

impl Row {
    fn bucket(&self) -> String {
        format!("{}-data", self.stack)
    }

    fn delivery_role(&self) -> String {
        format!("{}-delivery", self.stack)
    }

    fn target_role(&self) -> String {
        format!("{}-target", self.stack)
    }

    fn stream(&self) -> String {
        format!("{}-stream", self.stack)
    }

    fn rule(&self) -> String {
        format!("{}-rule", self.stack)
    }

    /// The event `source` this row's rule matches on, which no other row's rule matches: one
    /// publication carries every row's entry, so what differs between the rows is how their
    /// pipelines were declared and not which entries reached them.
    fn source(&self) -> String {
        format!("com.demo.{}", self.stack)
    }

    /// The `Detail` this row's entry carries, as the JSON text `PutEvents` takes.
    fn detail(&self) -> String {
        json!({ "marker": self.stack }).to_string()
    }

    /// The prefix this row's object is read back from: the declared one, or the bucket's root
    /// where the destination declares none.
    fn read_prefix(&self) -> String {
        self.prefix.unwrap_or_default().to_string()
    }

    /// The identities every reference form in this row's template has to resolve to, whichever
    /// way it wrote them down.
    fn bucket_arn(&self) -> String {
        format!("arn:aws:s3:::{}", self.bucket())
    }

    fn stream_arn(&self) -> String {
        format!(
            "arn:aws:firehose:{REGION}:{}:deliverystream/{}",
            engine::emulator_account_id(),
            self.stream()
        )
    }

    fn delivery_role_arn(&self) -> String {
        format!(
            "arn:aws:iam::{}:role/{}",
            engine::emulator_account_id(),
            self.delivery_role()
        )
    }

    fn target_role_arn(&self) -> String {
        format!(
            "arn:aws:iam::{}:role/{}",
            engine::emulator_account_id(),
            self.target_role()
        )
    }

    /// The row's template: the destination bucket, the two roles, the delivery stream writing
    /// into that bucket, and the rule whose target is that stream.
    fn template(&self) -> String {
        let mut destination = json!({
            "BucketARN": self.reference.bucket_arn(),
            "RoleARN": self.reference.role_arn("DeliveryRole"),
            "CompressionFormat": "UNCOMPRESSED",
            // The shortest buffering the hints admit (published ranges `SizeInMBs` 1..128,
            // `IntervalInSeconds` 0..900), so a row waits for the delivery and not for an
            // interval it would sit out.
            "BufferingHints": { "SizeInMBs": 1, "IntervalInSeconds": 0 },
        });
        if let Some(prefix) = self.prefix {
            destination["Prefix"] = json!(prefix);
        }
        let mut stream = json!({
            "DeliveryStreamName": self.stream(),
        });
        if let Some(stream_type) = self.stream_type {
            stream["DeliveryStreamType"] = json!(stream_type);
        }
        stream[self.destination.property()] = destination;
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
                        "AssumeRolePolicyDocument": trust_policy(FIREHOSE_PRINCIPAL),
                        "Policies": [{
                            "PolicyName": "delivery",
                            "PolicyDocument": identity_policy(PUT_OBJECT_ACTION),
                        }],
                    },
                },
                "TargetRole": {
                    "Type": "AWS::IAM::Role",
                    "Properties": {
                        "RoleName": self.target_role(),
                        "AssumeRolePolicyDocument": trust_policy(EVENTS_PRINCIPAL),
                        "Policies": [{
                            "PolicyName": "invoke",
                            "PolicyDocument": identity_policy(PUT_RECORD_ACTION),
                        }],
                    },
                },
                "Stream": {
                    "Type": "AWS::KinesisFirehose::DeliveryStream",
                    "Properties": stream,
                },
                "Rule": {
                    "Type": "AWS::Events::Rule",
                    "Properties": {
                        "Name": self.rule(),
                        "EventPattern": { "source": [self.source()] },
                        "State": self.state,
                        "Targets": [{
                            "Id": "t1",
                            "Arn": self.reference.stream_arn(),
                            "RoleArn": self.reference.role_arn("TargetRole"),
                        }],
                    },
                },
            },
            "Outputs": {
                "StreamArn": { "Value": { "Fn::GetAtt": ["Stream", "Arn"] } },
                "RuleName": { "Value": { "Ref": "Rule" } },
            },
        })
        .to_string()
    }
}

/// The deployed pipelines under test.
const ROWS: &[Row] = &[
    Row {
        case: "S3DestinationConfiguration, ARNs read as Fn::GetAtt attributes, a declared prefix",
        stack: "cfn-ebfh-plain-getatt",
        destination: Destination::Plain,
        reference: Reference::GetAtt,
        prefix: Some("plain/raw/"),
        stream_type: Some(DIRECT_PUT),
        state: ENABLED,
        delivers: true,
    },
    Row {
        case: "S3DestinationConfiguration, ARNs composed by Fn::Sub, no prefix, no stream type",
        stack: "cfn-ebfh-plain-sub",
        destination: Destination::Plain,
        reference: Reference::Sub,
        prefix: None,
        stream_type: None,
        state: ENABLED,
        delivers: true,
    },
    Row {
        case: "ExtendedS3DestinationConfiguration, ARNs composed by Fn::Sub, a declared prefix",
        stack: "cfn-ebfh-extended-sub",
        destination: Destination::Extended,
        reference: Reference::Sub,
        prefix: Some("extended/raw/"),
        stream_type: Some(DIRECT_PUT),
        state: ENABLED,
        delivers: true,
    },
    Row {
        case: "a pipeline whose rule is declared DISABLED",
        stack: "cfn-ebfh-disabled",
        destination: Destination::Plain,
        reference: Reference::GetAtt,
        prefix: Some("disabled/raw/"),
        stream_type: Some(DIRECT_PUT),
        state: DISABLED,
        delivers: false,
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
async fn a_deployed_pipeline_delivers_however_its_template_spells_the_wiring() {
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
        let case = row.case;
        let described = await_settled(&router, row).await;
        let reported = element(&described, "StackStatus");
        assert_eq!(
            reported, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {reported} in \
             {described}"
        );

        // Whichever form the template read them in, the stack publishes the resolved
        // identities: the stream's own ARN and the rule's own name.
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get("StreamArn").map(String::as_str),
            Some(row.stream_arn().as_str()),
            "case {case:?}: expected the stack to publish the stream ARN {:?}, got {outputs:?}",
            row.stream_arn()
        );
        assert_eq!(
            outputs.get("RuleName").map(String::as_str),
            Some(row.rule().as_str()),
            "case {case:?}: expected the stack to publish its declared rule name, got {outputs:?}"
        );

        // The resources exist in the services that OWN them, carrying the resolved
        // declarations — read back through each owning service's own describe.
        assert_stream(&router, row).await;
        assert_rule(&router, row).await;
    }

    // ONE publication carrying every row's entry: each row's rule matches its own source.
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

    // Every enabled pipeline's prefix comes to hold its object; the disabled one holds nothing
    // at each of those readings, and at every reading of as long a wait again.
    await_delivered(&router, ROWS).await;
    let until = Instant::now() + SILENCE_WINDOW;
    while Instant::now() < until {
        assert_holding_nothing(&router, ROWS).await;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert_holding_nothing(&router, ROWS).await;

    // And each delivered object is the published envelope, under the destination's declared
    // prefix and the automatic time segments.
    for row in ROWS.iter().filter(|row| row.delivers) {
        let case = row.case;
        let keys = keys_under(&router, case, &row.bucket(), &row.read_prefix()).await;
        assert_eq!(
            keys.len(),
            1,
            "case {case:?}: one flushed buffer is one object, got {keys:?}"
        );
        assert_key_layout(row, &keys[0]);
        let body = delivered_object(&router, case, &row.bucket(), &row.read_prefix()).await;
        for expected in [row.source(), DETAIL_TYPE.to_string(), row.detail()] {
            assert!(
                body.contains(&expected),
                "case {case:?}: the delivered object carries the published envelope, expected \
                 {expected:?} in {body:?}"
            );
        }
    }
}

/// Require Firehose itself to hold `row`'s stream, taking data, of the declared or defaulted
/// type, with the Amazon S3 destination the template declared.
///
/// `S3DestinationDescription` is where BOTH spellings are reported: a plain destination is
/// described there, and an extended one is described there beside its own
/// `ExtendedS3DestinationDescription`.
async fn assert_stream(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    let described = firehose(
        router,
        case,
        "DescribeDeliveryStream",
        json!({ "DeliveryStreamName": row.stream() }),
    )
    .await;
    let description = &described["DeliveryStreamDescription"];
    assert_eq!(
        description["DeliveryStreamStatus"],
        json!(ACTIVE),
        "case {case:?}: expected a stream taking data, got {description}"
    );
    assert_eq!(
        description["DeliveryStreamARN"],
        json!(row.stream_arn()),
        "case {case:?}: expected the stream ARN {:?}, got {description}",
        row.stream_arn()
    );
    // A template that states no type gets the member's documented default, not an empty one.
    assert_eq!(
        description["DeliveryStreamType"],
        json!(row.stream_type.unwrap_or(DIRECT_PUT)),
        "case {case:?}: expected the declared or defaulted stream type, got {description}"
    );
    let destination = &description["Destinations"][0]["S3DestinationDescription"];
    assert_eq!(
        destination["BucketARN"],
        json!(row.bucket_arn()),
        "case {case:?}: expected the destination to resolve to {:?}, got {destination}",
        row.bucket_arn()
    );
    assert_eq!(
        destination["RoleARN"],
        json!(row.delivery_role_arn()),
        "case {case:?}: expected the delivery role to resolve to {:?}, got {destination}",
        row.delivery_role_arn()
    );
    if let Some(prefix) = row.prefix {
        assert_eq!(
            destination["Prefix"],
            json!(prefix),
            "case {case:?}: expected the declared prefix {prefix:?}, got {destination}"
        );
    }
    if row.destination == Destination::Extended {
        assert_eq!(
            description["Destinations"][0]["ExtendedS3DestinationDescription"]["BucketARN"],
            json!(row.bucket_arn()),
            "case {case:?}: an extended destination is also described as one, got {description}"
        );
    }
}

/// Require EventBridge itself to hold `row`'s rule, in the declared state, with the one target
/// the template declared — both ARNs resolved to the resources the stack created.
async fn assert_rule(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    let described = events(router, case, "DescribeRule", json!({ "Name": row.rule() })).await;
    assert_eq!(
        described["State"],
        json!(row.state),
        "case {case:?}: expected the declared rule state, got {described}"
    );
    let listed = events(
        router,
        case,
        "ListTargetsByRule",
        json!({ "Rule": row.rule() }),
    )
    .await;
    let targets = listed["Targets"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected a target list, got {listed}"));
    assert_eq!(
        targets.len(),
        1,
        "case {case:?}: one declared target is one installed target, got {listed}"
    );
    assert_eq!(
        targets[0]["Arn"],
        json!(row.stream_arn()),
        "case {case:?}: expected the target to resolve to {:?}, got {listed}",
        row.stream_arn()
    );
    assert_eq!(
        targets[0]["RoleArn"],
        json!(row.target_role_arn()),
        "case {case:?}: expected the target role to resolve to {:?}, got {listed}",
        row.target_role_arn()
    );
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

/// Wait until every prefix that must be delivered to holds an object, requiring every prefix
/// that must stay empty to hold nothing at each reading taken meanwhile.
async fn await_delivered(router: &MultiServiceRouter, rows: &[Row]) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        assert_holding_nothing(router, rows).await;
        let mut pending: Vec<&str> = Vec::new();
        for row in rows {
            if row.delivers
                && keys_under(router, row.case, &row.bucket(), &row.read_prefix())
                    .await
                    .is_empty()
            {
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
        if row.delivers {
            continue;
        }
        let keys = keys_under(router, row.case, &row.bucket(), &row.read_prefix()).await;
        assert!(
            keys.is_empty(),
            "case {:?}: a rule that matches nothing invokes nothing, so this prefix stays \
             empty; got {keys:?}",
            row.case
        );
    }
}

/// Require `key` to be the destination's declared `Prefix`, then the automatic `YYYY/MM/DD/HH`
/// segments ("The \"YYYY/MM/DD/HH\" time format prefix is automatically used for delivered
/// Amazon S3 files"), then a name beginning with the stream's own.
fn assert_key_layout(row: &Row, key: &str) {
    let case = row.case;
    let declared = row.prefix.unwrap_or_default();
    let rest = key.strip_prefix(declared).unwrap_or_else(|| {
        panic!(
            "case {case:?}: the key must begin with the declared prefix {declared:?}, got {key:?}"
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
            "case {case:?}: expected a {width}-digit time segment, got {segment:?} in key {key:?}"
        );
    }
    assert!(
        segments[4].starts_with(&format!("{}-", row.stream())),
        "case {case:?}: the object's name begins with its stream, got {:?} in key {key:?}",
        segments[4]
    );
}
