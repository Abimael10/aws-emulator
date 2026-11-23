// SPDX-License-Identifier: CC0-1.0

//! Which of CloudFront's control-plane calls reach an EventBridge rule's SQS target, as the
//! rule's own `State` decides it and the target queue's own policy admits it.
//!
//! RULE: while a trail of the Region is logging management events, EVERY control-plane call
//! the account serves — the ones that only REPORT it included — is also published onto the
//! account's DEFAULT event bus, with `source` `aws.<endpoint-prefix>` of the serving service,
//! `detail-type` `AWS API Call via CloudTrail` and the CloudTrail record of the call as the
//! event's `detail`, carrying that call's `eventName`, its `readOnly` kind and as `requestID`
//! the id the CALLER was answered under. WHICH rules such an event is matched against is the
//! rule's `State`: `DISABLED` matches no event at all, `ENABLED` "matches events against the
//! rule, except for Amazon Web Services management events delivered through CloudTrail" —
//! which parts the READS of this channel from it while its writes reach it like any other
//! event — and `ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENTS` "is enabled for all events,
//! including Amazon Web Services management events delivered through CloudTrail", so the
//! reads reach it too. No state admits what the rule's own `EventPattern` excludes, and a
//! matched rule hands the envelope to its SQS target when that queue's policy admits
//! `events.amazonaws.com` for the delivery, whose `aws:SourceArn` is the ARN of the RULE that
//! matched.
//!
//! Only a cross-service suite can pin this: the calls are served by CloudFront (restXml),
//! the delivery condition belongs to CloudTrail (awsJson1_1), the state and the hand-over
//! belong to EventBridge (awsJson1_1) and what arrived is read from Amazon SQS (awsJson1_0)
//! — four services over ONE engine, as `core/router/src/lib.rs` mounts them. The record of
//! each call on its own is this crate's `cloudfront_api_call_events`, and the same `State`
//! rule over a pattern written for the WHOLE channel is `cloudtrail_management_event_rule_state`;
//! what this suite adds is the rule read through a pattern that names its calls by
//! `detail.eventName`, over the resource surfaces a client audits CloudFront through, onto
//! queues whose grant is scoped to the matching rule.
//!
//! The rows span what the rule generalises over rather than one call: each of the three rule
//! states, a near miss naming the OTHER CloudTrail channel's `detail-type` (which the widest
//! state does not bypass), the FORM of the call (a conditional replace of a sub-resource's
//! configuration, a tag change addressed by resource ARN, a get of one resource by id, a list
//! over a collection) and the read/write kind itself.
//!
//! CloudFront is a GLOBAL service, so its records are filed in the partition's leader Region
//! and a rule matching its API-call events has to live there; every request below is signed
//! for that Region for exactly that reason.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for: the Region a GLOBAL service's management
/// events are filed in, which is where a rule matching them lives.
const REGION: &str = "us-east-1";

/// The account every emulated resource belongs to.
const ACCOUNT: &str = "000000000000";

/// CloudFront's API version, which every modelled route of it is under.
const API_VERSION: &str = "2020-05-31";

/// The `source` an event published for a CloudFront call carries.
const CLOUDFRONT_EVENT_SOURCE: &str = "aws.cloudfront";

/// The `eventSource` the CloudTrail record of a CloudFront call names.
const CLOUDFRONT_RECORD_SOURCE: &str = "cloudfront.amazonaws.com";

/// The `detail-type` CloudTrail's management-event fan-out carries
/// (`service_common::api_call_events::API_CALL_DETAIL_TYPE`).
const API_CALL_DETAIL_TYPE: &str = "AWS API Call via CloudTrail";

/// The `detail-type` of the OTHER CloudTrail channel, which none of these events carries:
/// the near-miss row's pattern names it.
const SERVICE_EVENT_DETAIL_TYPE: &str = "AWS Service Event via CloudTrail";

/// The three values `RuleState` publishes, as `PutRule$State` spells them
/// (`smithy_query(query="shape", shape="RuleState")`).
const STATE_DISABLED: &str = "DISABLED";
const STATE_ENABLED: &str = "ENABLED";
const STATE_ALL_MANAGEMENT_EVENTS: &str = "ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENTS";

/// How long the arrival of the events a row expects is waited for. Generous: the suite waits
/// for the CONDITION (each expected envelope on its own sink) and this is only the point at
/// which waiting longer cannot help.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);

/// How long between two looks at a sink while waiting for one.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Whether a call of a row only REPORTS the account's state or CHANGES it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A `Get` / `List` read: recorded and published with `readOnly` true.
    Reporting,
    /// A call that changes the account: recorded and published with `readOnly` false.
    Mutating,
}

/// One CloudFront control-plane call this suite makes, and its read/write kind.
#[derive(Debug, Clone, Copy)]
struct Call {
    case: &'static str,
    /// The operation's modelled name, which is the record's `eventName` and what each
    /// pattern below names.
    event_name: &'static str,
    kind: Kind,
}

/// Every subject call, in the order they are made: the reads in between two writes, with a
/// WRITE LAST — the barrier whose arrival proves the channel ran past every read before an
/// absence is read off a sink.
const CALLS: &[Call] = &[
    Call {
        case: "a conditional replace of a sub-resource's configuration",
        event_name: "UpdateOriginAccessControl",
        kind: Kind::Mutating,
    },
    Call {
        case: "a get of one resource by id",
        event_name: "GetOriginAccessControl",
        kind: Kind::Reporting,
    },
    Call {
        case: "a list over a collection",
        event_name: "ListDistributions",
        kind: Kind::Reporting,
    },
    Call {
        case: "a tag change addressed by resource ARN",
        event_name: "TagResource",
        kind: Kind::Mutating,
    },
];

/// The `detail.eventName` list every pattern below selects on: all four subject calls, so
/// what parts the rows is the rule's `State` and the channel its pattern names, never a
/// narrower pattern.
fn subject_event_names() -> Value {
    Value::Array(
        CALLS
            .iter()
            .map(|call| json!(call.event_name))
            .collect::<Vec<Value>>(),
    )
}

/// One rule this suite installs: its `State`, the pattern it matches with, and which of the
/// calls above must reach its own sink.
struct Row {
    case: &'static str,
    state: &'static str,
    /// The rule's `EventPattern`.
    pattern: Value,
    /// Whether an event of this channel whose record is a READ reaches this rule.
    expects_reads: bool,
    /// Whether an event of this channel whose record is a WRITE reaches it.
    expects_writes: bool,
}

/// The rows, each on its own queue so what arrived at one is never read as another's.
fn rows() -> Vec<Row> {
    let api_call_channel = json!({
        "source": [CLOUDFRONT_EVENT_SOURCE],
        "detail-type": [API_CALL_DETAIL_TYPE],
        "detail": { "eventName": subject_event_names() },
    });
    vec![
        Row {
            case: "an ENABLED rule naming these calls",
            state: STATE_ENABLED,
            pattern: api_call_channel.clone(),
            expects_reads: false,
            expects_writes: true,
        },
        Row {
            case: "a rule enabled for all CloudTrail management events, naming these calls",
            state: STATE_ALL_MANAGEMENT_EVENTS,
            pattern: api_call_channel.clone(),
            expects_reads: true,
            expects_writes: true,
        },
        Row {
            case: "a DISABLED rule naming these calls",
            state: STATE_DISABLED,
            pattern: api_call_channel,
            expects_reads: false,
            expects_writes: false,
        },
        Row {
            // The widest state admits nothing the pattern excludes: these events are of the
            // API-call channel, so a pattern naming the OTHER CloudTrail channel matches
            // none of them however widely the rule is enabled.
            case: "a rule enabled for all management events, naming the service-event channel",
            state: STATE_ALL_MANAGEMENT_EVENTS,
            pattern: json!({
                "source": [CLOUDFRONT_EVENT_SOURCE],
                "detail-type": [SERVICE_EVENT_DETAIL_TYPE],
                "detail": { "eventName": subject_event_names() },
            }),
            expects_reads: false,
            expects_writes: false,
        },
    ]
}

/// The four services this fan-out runs through, over ONE shared engine.
///
/// Amazon S3 is mounted beside them because a trail's destination bucket is resolved in the
/// account's object store: the trail is created the way a client creates one, against a
/// bucket that really exists.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signing is not the subject here, so requests are unauthenticated; the credential scope
    // is still carried, because it is what names the Region.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_cloudtrail_cloudtrail_20131101::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
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
        .header("authorization", authorization(signing_name))
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

/// An EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with {parsed}"
    );
    parsed
}

/// An Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// A CloudTrail call (`awsJson1_1`, `CloudTrail_20131101.<Operation>`) that must answer 200.
async fn cloudtrail(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) =
        rpc(router, "cloudtrail", "CloudTrail_20131101", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudTrail {operation}, got {status} with {parsed}"
    );
    parsed
}

/// What one CloudFront call answered: its status, the id the caller was answered under, the
/// entity version it reported and the raw body.
struct CloudFrontAnswer {
    status: u16,
    request_id: String,
    etag: String,
    body: String,
}

/// One CloudFront restXml call, with `if_match` carried as `If-Match` when it is given.
async fn cloudfront(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&str>,
    if_match: Option<&str>,
) -> CloudFrontAnswer {
    let mut request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "cloudfront.amazonaws.com")
        .header("authorization", authorization("cloudfront"));
    if body.is_some() {
        request = request.header("content-type", "application/xml");
    }
    if let Some(version) = if_match {
        request = request.header("if-match", version);
    }
    let request = request
        .body(Bytes::from(body.unwrap_or_default().to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    CloudFrontAnswer {
        status: response.status().as_u16(),
        request_id: header("x-amzn-requestid"),
        etag: header("etag"),
        body: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// The text of the first `<name>` element of an XML body, or `""`.
fn xml_element(body: &str, name: &str) -> String {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let Some(from) = body.find(&open).map(|at| at + open.len()) else {
        return String::new();
    };
    let Some(len) = body[from..].find(&close) else {
        return String::new();
    };
    body[from..from + len].to_string()
}

/// `PUT /<bucket>` plus `PUT /<bucket>?policy` — the bucket the trail delivers its log files
/// into, carrying the policy that lets CloudTrail write into it.
async fn create_bucket(router: &MultiServiceRouter, bucket: &str) {
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{bucket}"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/xml")
        .header("authorization", authorization("s3"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "setup: CreateBucket({bucket:?}) must succeed, got {} with {:?}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );

    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AWSCloudTrailWrite",
            "Effect": "Allow",
            "Principal": { "Service": "cloudtrail.amazonaws.com" },
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
            "Condition": {
                "StringEquals": { "s3:x-amz-acl": "bucket-owner-full-control" },
            },
        }],
    })
    .to_string();
    let request = http::Request::builder()
        .method("PUT")
        .uri(format!("/{bucket}?policy"))
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("s3"))
        .body(Bytes::from(policy))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    assert!(
        (200..300).contains(&response.status().as_u16()),
        "setup: PutBucketPolicy({bucket:?}) must succeed, got {} with {:?}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
}

/// Create a queue and hand back its url and ARN. The grant that admits the delivery is
/// attached later, once the rule it is scoped to has an ARN.
async fn create_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs(router, name, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueUrl for {name:?}, got {created}"))
        .to_string();
    let arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}");
    (url, arn)
}

/// Admit `events.amazonaws.com` to send to `queue_arn`, scoped by `aws:SourceArn` to the ARN
/// of the rule whose deliveries this queue receives — the grant a client writes for a rule
/// target.
async fn admit_rule(
    router: &MultiServiceRouter,
    case: &str,
    queue_url: &str,
    queue_arn: &str,
    rule_arn: &str,
) {
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "events.amazonaws.com" },
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    });
    sqs(
        router,
        case,
        "SetQueueAttributes",
        &json!({ "QueueUrl": queue_url, "Attributes": { "Policy": policy.to_string() } }),
    )
    .await;
}

/// Receive up to ten messages from `url`, draining them, as their parsed bodies.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let answer = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    answer["Messages"]
        .as_array()
        .map(|messages| {
            messages
                .iter()
                .map(|message| {
                    let body = message["Body"].as_str().unwrap_or_default();
                    serde_json::from_str(body).unwrap_or_else(|error| {
                        panic!("case {case:?}: a delivered envelope is JSON ({error}): {body}")
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A `DistributionConfig` for `reference` — the smallest one CloudFront accepts.
fn distribution_config(reference: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Comment>api call rule states</Comment>\
         <Enabled>true</Enabled>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>primary</Id><DomainName>origin.example.com</DomainName>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior>\
         <TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         </DefaultCacheBehavior>\
         </DistributionConfig>"
    )
}

/// An `OriginAccessControlConfig` under `name`, carrying `description`.
fn origin_access_control_config(name: &str, description: &str) -> String {
    format!(
        "<OriginAccessControlConfig \
         xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{name}</Name>\
         <Description>{description}</Description>\
         <SigningProtocol>sigv4</SigningProtocol>\
         <SigningBehavior>always</SigningBehavior>\
         <OriginAccessControlOriginType>s3</OriginAccessControlOriginType>\
         </OriginAccessControlConfig>"
    )
}

/// The `detail.eventName` of each envelope that arrived.
fn arrived_names(arrived: &[Value]) -> Vec<String> {
    arrived
        .iter()
        .map(|envelope| {
            envelope
                .pointer("/detail/eventName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The calls a row expects on its own sink.
fn expected_of(row: &Row) -> BTreeSet<&'static str> {
    CALLS
        .iter()
        .filter(|call| match call.kind {
            Kind::Reporting => row.expects_reads,
            Kind::Mutating => row.expects_writes,
        })
        .map(|call| call.event_name)
        .collect()
}

#[tokio::test]
async fn a_rules_state_decides_which_cloudfront_api_calls_reach_its_conditioned_queue() {
    let router = router();

    // A trail of the Region, logging — the condition every publication hangs off.
    create_bucket(&router, "cloudfront-rule-state-trail").await;
    cloudtrail(
        &router,
        "setup",
        "CreateTrail",
        &json!({
            "Name": "cloudfront-rule-state",
            "S3BucketName": "cloudfront-rule-state-trail",
            "IsMultiRegionTrail": false,
            "IncludeGlobalServiceEvents": true,
        }),
    )
    .await;
    cloudtrail(
        &router,
        "setup",
        "StartLogging",
        &json!({ "Name": "cloudfront-rule-state" }),
    )
    .await;
    let trail_status = cloudtrail(
        &router,
        "setup",
        "GetTrailStatus",
        &json!({ "Name": "cloudfront-rule-state" }),
    )
    .await;
    assert_eq!(
        trail_status["IsLogging"],
        json!(true),
        "setup: the trail must be logging before any call is made, got {trail_status}"
    );

    // The resources the subject calls act on, created BEFORE any rule is installed so that
    // no call of this setup is one a rule below could have matched.
    let created = cloudfront(
        &router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&distribution_config("ref-cloudfront-rule-state")),
        None,
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateDistribution must succeed, got {} with {:?}",
        created.status, created.body
    );
    let distribution_arn = xml_element(&created.body, "ARN");
    assert!(
        !distribution_arn.is_empty(),
        "setup: the created distribution must name its ARN, got {:?}",
        created.body
    );
    let control = cloudfront(
        &router,
        "POST",
        &format!("/{API_VERSION}/origin-access-control"),
        Some(&origin_access_control_config(
            "cloudfront-rule-state-oac",
            "created",
        )),
        None,
    )
    .await;
    assert_eq!(
        control.status, 201,
        "setup: CreateOriginAccessControl must succeed, got {} with {:?}",
        control.status, control.body
    );
    let control_id = xml_element(&control.body, "Id");
    assert!(
        !control_id.is_empty() && !control.etag.is_empty(),
        "setup: the created control must name its id and version, got {:?} / {:?}",
        control.body,
        control.etag
    );

    // One rule per row, each on its own queue, all on the DEFAULT bus — the only bus an
    // AWS-published event reaches. Each queue's grant is scoped to its own rule's ARN, which
    // PutRule reports, so the delivery is admitted by the rule that makes it and nothing else.
    let rows = rows();
    let mut sinks: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let (queue_url, queue_arn) =
            create_queue(&router, &format!("cloudfront-rule-state-sink-{index}")).await;
        let rule = format!("cloudfront-rule-state-rule-{index}");
        let put_rule = events(
            &router,
            row.case,
            "PutRule",
            &json!({
                "Name": rule,
                "State": row.state,
                "EventPattern": row.pattern.to_string(),
            }),
        )
        .await;
        let rule_arn = put_rule["RuleArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected PutRule to report the rule's ARN, got {put_rule}",
                    row.case
                )
            })
            .to_string();
        admit_rule(&router, row.case, &queue_url, &queue_arn, &rule_arn).await;
        let put_targets = events(
            &router,
            row.case,
            "PutTargets",
            &json!({
                "Rule": rule,
                "Targets": [{ "Id": "sink", "Arn": queue_arn }],
            }),
        )
        .await;
        assert_eq!(
            put_targets["FailedEntryCount"], json!(0),
            "case {:?}: the target must be installed, got {put_targets}",
            row.case
        );
        sinks.push(queue_url);
    }

    // The subject calls, in the order [`CALLS`] states them. Each answer's own request id is
    // kept, because that is what the published record must name.
    let mut answered_as: BTreeMap<&'static str, String> = BTreeMap::new();

    let updated = cloudfront(
        &router,
        "PUT",
        &format!("/{API_VERSION}/origin-access-control/{control_id}/config"),
        Some(&origin_access_control_config(
            "cloudfront-rule-state-oac",
            "updated",
        )),
        Some(&control.etag),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "case {:?}: UpdateOriginAccessControl must succeed, got {} with {:?}",
        CALLS[0].case, updated.status, updated.body
    );
    answered_as.insert(CALLS[0].event_name, updated.request_id);

    let got = cloudfront(
        &router,
        "GET",
        &format!("/{API_VERSION}/origin-access-control/{control_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        got.status, 200,
        "case {:?}: GetOriginAccessControl must succeed, got {} with {:?}",
        CALLS[1].case, got.status, got.body
    );
    answered_as.insert(CALLS[1].event_name, got.request_id);

    let listed = cloudfront(
        &router,
        "GET",
        &format!("/{API_VERSION}/distribution?MaxItems=1"),
        None,
        None,
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {:?}: ListDistributions must succeed, got {} with {:?}",
        CALLS[2].case, listed.status, listed.body
    );
    answered_as.insert(CALLS[2].event_name, listed.request_id);

    let tagged = cloudfront(
        &router,
        "POST",
        &format!("/{API_VERSION}/tagging?Operation=Tag&Resource={distribution_arn}"),
        Some("<Tags><Items><Tag><Key>team</Key><Value>audit</Value></Tag></Items></Tags>"),
        None,
    )
    .await;
    assert!(
        (200..300).contains(&tagged.status),
        "case {:?}: TagResource must succeed, got {} with {:?}",
        CALLS[3].case,
        tagged.status,
        tagged.body
    );
    answered_as.insert(CALLS[3].event_name, tagged.request_id);

    for call in CALLS {
        let request_id = answered_as.get(call.event_name).map(String::as_str);
        assert!(
            request_id.is_some_and(|id| !id.is_empty()),
            "case {:?}: the answer must name the request it answers, got {request_id:?}",
            call.case
        );
    }

    // Wait for the CONDITION — every event a row expects on that row's own sink — rather than
    // for any span of time. The last call made is a write, so once a sink holds the writes it
    // expects, a read it does not expect would already have arrived had it been matched.
    let mut arrived: Vec<Vec<Value>> = vec![Vec::new(); rows.len()];
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let mut complete = true;
        for (index, row) in rows.iter().enumerate() {
            arrived[index].extend(drained(&router, row.case, &sinks[index]).await);
            let seen: BTreeSet<String> = arrived_names(&arrived[index]).into_iter().collect();
            if !expected_of(row).iter().all(|name| seen.contains(*name)) {
                complete = false;
            }
        }
        if complete {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "expected each row's own events to arrive, got {:?}",
            arrived
                .iter()
                .map(|sink| arrived_names(sink))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    for (index, row) in rows.iter().enumerate() {
        let delivered = arrived_names(&arrived[index]);
        for call in CALLS {
            let expected = match call.kind {
                Kind::Reporting => row.expects_reads,
                Kind::Mutating => row.expects_writes,
            };
            let present = delivered.iter().any(|name| name == call.event_name);
            assert_eq!(
                present, expected,
                "case {:?} / {:?}: expected {} for {}, got {delivered:?}",
                row.case,
                call.case,
                if expected { "the event" } else { "no event" },
                call.event_name
            );
        }
        // What arrived is the published record of the very call it names: CloudFront's own
        // `source`, this channel's `detail-type`, the Region and account the record is filed
        // in, the serving service, the call's read/write kind and the id its CALLER was
        // answered under.
        for envelope in &arrived[index] {
            let name = envelope
                .pointer("/detail/eventName")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some(call) = CALLS.iter().find(|call| call.event_name == name) else {
                continue;
            };
            assert_eq!(
                envelope["source"],
                json!(CLOUDFRONT_EVENT_SOURCE),
                "case {:?} / {:?}: expected source {CLOUDFRONT_EVENT_SOURCE:?}, got {envelope}",
                row.case,
                call.case
            );
            assert_eq!(
                envelope["detail-type"],
                json!(API_CALL_DETAIL_TYPE),
                "case {:?} / {:?}: expected detail-type {API_CALL_DETAIL_TYPE:?}, got {envelope}",
                row.case,
                call.case
            );
            assert_eq!(
                envelope["region"],
                json!(REGION),
                "case {:?} / {:?}: expected the event in {REGION:?}, got {envelope}",
                row.case,
                call.case
            );
            assert_eq!(
                envelope["account"],
                json!(ACCOUNT),
                "case {:?} / {:?}: expected the event in account {ACCOUNT:?}, got {envelope}",
                row.case,
                call.case
            );
            assert_eq!(
                envelope.pointer("/detail/eventSource"),
                Some(&json!(CLOUDFRONT_RECORD_SOURCE)),
                "case {:?} / {:?}: the record names the serving service, got {envelope}",
                row.case,
                call.case
            );
            assert_eq!(
                envelope.pointer("/detail/readOnly"),
                Some(&json!(call.kind == Kind::Reporting)),
                "case {:?} / {:?}: expected readOnly {}, got {envelope}",
                row.case,
                call.case,
                call.kind == Kind::Reporting
            );
            assert_eq!(
                envelope.pointer("/detail/requestID").and_then(Value::as_str),
                answered_as.get(call.event_name).map(String::as_str),
                "case {:?} / {:?}: the record names the call the caller was answered for, got \
                 {envelope}",
                row.case,
                call.case
            );
        }
    }
}
