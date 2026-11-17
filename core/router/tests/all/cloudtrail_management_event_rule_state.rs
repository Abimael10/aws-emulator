// SPDX-License-Identifier: CC0-1.0

//! Which of the account's CloudTrail management events a rule is matched against, as the
//! rule's own `State` decides it.
//!
//! RULE: while a trail of the Region is logging management events, EVERY management call the
//! account serves is published onto its default event bus under the `detail-type` `AWS API
//! Call via CloudTrail`, the reads included, each carrying its own read/write kind as the
//! record's `readOnly`. WHICH rules such an event reaches is the rule's `State`: `DISABLED`
//! matches no event at all, `ENABLED` "matches events against the rule, except for Amazon Web
//! Services management events delivered through CloudTrail" — which parts the READS of this
//! channel from it, the writes reaching it like any other event — and
//! `ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENTS` "is enabled for all events, including
//! Amazon Web Services management events delivered through CloudTrail", so a read reaches it
//! too. A state admits nothing its pattern excludes: a rule in the widest state still matches
//! only what its `EventPattern` selects.
//!
//! Only a cross-service suite can pin this: the calls are served by the services that own
//! them (CloudFront over restXml, Amazon SQS over awsJson1_0), the delivery decision belongs
//! to CloudTrail (awsJson1_1), the state and the hand-over belong to EventBridge (awsJson1_1)
//! and what arrived is read from Amazon SQS — five services over ONE engine, as
//! `core/router/src/lib.rs` mounts them. The half of the rule a plain `ENABLED` rule sees is
//! this crate's `cloudtrail_read_only_api_calls`.
//!
//! The rows span what the rule generalises over rather than the one call that motivated it:
//! each of the three rule states, a near miss that proves the widest state bypasses no
//! pattern, which service served the call (one GLOBAL service with an XML protocol, one
//! REGIONAL service with a JSON one), the FORM of the read (a `List` over a collection, a
//! `Get` of one resource by id) and the read/write kind itself.
//!
//! CloudFront is a GLOBAL service, so its records are filed in the partition's leader Region;
//! every request below is signed for that Region so that one bus and one trail cover them
//! all.

use std::collections::BTreeSet;
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
/// for the CONDITION (each expected envelope on its sink), and this is only the point at
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

/// One management call this suite makes, and its read/write kind.
#[derive(Debug, Clone, Copy)]
struct Call {
    case: &'static str,
    /// The operation's modelled name, which is the record's `eventName`.
    event_name: &'static str,
    /// The `source` the published event carries: `aws.<endpoint-prefix>` of the service that
    /// served it.
    source: &'static str,
    kind: Kind,
}

/// Every call below, in the order they are made: a write first, the reads in between, and a
/// write LAST — the barrier whose arrival proves the channel ran past every read before an
/// absence is read.
const CALLS: &[Call] = &[
    Call {
        case: "a global service's create",
        event_name: "CreateDistribution",
        source: "aws.cloudfront",
        kind: Kind::Mutating,
    },
    Call {
        case: "a global service's list of a collection",
        event_name: "ListDistributions",
        source: "aws.cloudfront",
        kind: Kind::Reporting,
    },
    Call {
        case: "a global service's get of one resource by id",
        event_name: "GetDistribution",
        source: "aws.cloudfront",
        kind: Kind::Reporting,
    },
    Call {
        case: "a regional service's list of a collection",
        event_name: "ListQueues",
        source: "aws.sqs",
        kind: Kind::Reporting,
    },
    Call {
        case: "a regional service's tagging write",
        event_name: "TagQueue",
        source: "aws.sqs",
        kind: Kind::Mutating,
    },
];

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
    let whole_channel = json!({ "detail-type": [API_CALL_DETAIL_TYPE] });
    vec![
        Row {
            case: "an ENABLED rule over the whole channel",
            state: STATE_ENABLED,
            pattern: whole_channel.clone(),
            expects_reads: false,
            expects_writes: true,
        },
        Row {
            case: "a rule enabled for all CloudTrail management events",
            state: STATE_ALL_MANAGEMENT_EVENTS,
            pattern: whole_channel.clone(),
            expects_reads: true,
            expects_writes: true,
        },
        Row {
            case: "a DISABLED rule over the whole channel",
            state: STATE_DISABLED,
            pattern: whole_channel,
            expects_reads: false,
            expects_writes: false,
        },
        Row {
            // The widest state admits nothing the pattern excludes: these events are of the
            // API-call channel, so a pattern naming the OTHER CloudTrail channel matches
            // none of them however the rule is enabled.
            case: "a rule enabled for all management events, naming the service-event channel",
            state: STATE_ALL_MANAGEMENT_EVENTS,
            pattern: json!({ "detail-type": [SERVICE_EVENT_DETAIL_TYPE] }),
            expects_reads: false,
            expects_writes: false,
        },
    ]
}

/// The five services this fan-out runs through, over ONE shared engine.
///
/// Amazon S3 is mounted beside them because a trail's destination bucket is resolved in the
/// account's object store: the trail is created the way a client creates one, against a bucket
/// that really exists.
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
async fn cloudtrail(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) {
    let (status, parsed) =
        rpc(router, "cloudtrail", "CloudTrail_20131101", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudTrail {operation}, got {status} with {parsed}"
    );
}

/// One CloudFront restXml call, as its status and raw body.
async fn cloudfront(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, String) {
    let mut request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "cloudfront.amazonaws.com")
        .header("authorization", authorization("cloudfront"));
    if body.is_some() {
        request = request.header("content-type", "application/xml");
    }
    let request = request
        .body(Bytes::from(body.unwrap_or_default().to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
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

/// Create a queue admitting `events.amazonaws.com`, and hand back its url and ARN.
async fn create_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = sqs(router, name, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: expected a QueueUrl for {name:?}, got {created}"))
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
        name,
        "SetQueueAttributes",
        &json!({ "QueueUrl": url, "Attributes": { "Policy": policy.to_string() } }),
    )
    .await;
    (url, arn)
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
         <Comment>management event rule state</Comment>\
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
async fn a_rules_state_decides_which_cloudtrail_management_events_reach_it() {
    let router = router();

    // A trail of the account, logging — the condition any delivery hangs off.
    create_bucket(&router, "management-event-state-trail").await;
    cloudtrail(
        &router,
        "setup",
        "CreateTrail",
        &json!({
            "Name": "management-event-state",
            "S3BucketName": "management-event-state-trail",
        }),
    )
    .await;
    cloudtrail(
        &router,
        "setup",
        "StartLogging",
        &json!({ "Name": "management-event-state" }),
    )
    .await;

    // Every queue first and every rule after, so that no queue this setup creates is a
    // management write any of these rules could already have matched.
    let rows = rows();
    let (subject_url, _) = create_queue(&router, "management-event-state-subject").await;
    let mut sinks = Vec::new();
    for index in 0..rows.len() {
        sinks.push(create_queue(&router, &format!("management-event-state-sink-{index}")).await);
    }
    for (index, (row, (_, sink_arn))) in rows.iter().zip(&sinks).enumerate() {
        let rule = format!("management-event-state-rule-{index}");
        events(
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
        let put = events(
            &router,
            row.case,
            "PutTargets",
            &json!({
                "Rule": rule,
                "Targets": [{ "Id": "sink", "Arn": sink_arn }],
            }),
        )
        .await;
        assert_eq!(
            put["FailedEntryCount"], json!(0),
            "case {:?}: the target must be installed, got {put}",
            row.case
        );
    }

    // The calls, in the order [`CALLS`] states them.
    let (status, created) = cloudfront(
        &router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Some(&distribution_config("ref-management-event-state")),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {:?}: CreateDistribution must succeed, got {status} with {created:?}",
        CALLS[0].case
    );
    let distribution_id = xml_element(&created, "Id");
    assert!(
        !distribution_id.is_empty(),
        "case {:?}: the created distribution must name its id, got {created:?}",
        CALLS[0].case
    );
    let (status, listed) = cloudfront(&router, "GET", &format!("/{API_VERSION}/distribution"), None).await;
    assert_eq!(
        status, 200,
        "case {:?}: ListDistributions must succeed, got {status} with {listed:?}",
        CALLS[1].case
    );
    let (status, got) = cloudfront(
        &router,
        "GET",
        &format!("/{API_VERSION}/distribution/{distribution_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: GetDistribution must succeed, got {status} with {got:?}",
        CALLS[2].case
    );
    sqs(&router, CALLS[3].case, "ListQueues", &json!({})).await;
    sqs(
        &router,
        CALLS[4].case,
        "TagQueue",
        &json!({ "QueueUrl": subject_url, "Tags": { "owner": "audit" } }),
    )
    .await;

    // Wait for the CONDITION — every event a row expects on that row's sink — rather than for
    // any span of time. The last call made is a write, so once a sink holds the writes it
    // expects, a read it does not expect would already have arrived had it been matched.
    let mut arrived: Vec<Vec<Value>> = vec![Vec::new(); rows.len()];
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let mut complete = true;
        for (index, row) in rows.iter().enumerate() {
            let (url, _) = &sinks[index];
            arrived[index].extend(drained(&router, row.case, url).await);
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
        // What arrived is the published record of the very call it names: its service's
        // `source`, this channel's `detail-type`, and the call's own read/write kind.
        for envelope in &arrived[index] {
            let name = envelope
                .pointer("/detail/eventName")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some(call) = CALLS.iter().find(|call| call.event_name == name) else {
                continue;
            };
            assert_eq!(
                envelope["detail-type"],
                json!(API_CALL_DETAIL_TYPE),
                "case {:?} / {:?}: expected detail-type {API_CALL_DETAIL_TYPE:?}, got {envelope}",
                row.case,
                call.case
            );
            assert_eq!(
                envelope["source"],
                json!(call.source),
                "case {:?} / {:?}: expected source {:?}, got {envelope}",
                row.case,
                call.case,
                call.source
            );
            assert_eq!(
                envelope.pointer("/detail/readOnly"),
                Some(&json!(call.kind == Kind::Reporting)),
                "case {:?} / {:?}: expected readOnly {}, got {envelope}",
                row.case,
                call.case,
                call.kind == Kind::Reporting
            );
        }
    }
}
