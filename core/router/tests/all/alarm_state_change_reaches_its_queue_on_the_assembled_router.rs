// SPDX-License-Identifier: CC0-1.0

//! An alarm transition reaches the SQS target of its EventBridge rule on a router that
//! VERIFIES request signatures — the configuration the emulator serves traffic in — with
//! CloudWatch called over its FORM wire.
//!
//! The RULE under test: **every change of a metric alarm's `StateValue` is published once
//! onto the account's default EventBridge bus as `source` `aws.cloudwatch` / `detail-type`
//! `CloudWatch Alarm State Change`, carrying the alarm's own ARN in `resources` and a
//! `detail` naming the alarm, the state it moved TO and the state it moved OUT OF; a rule
//! whose `EventPattern` matches that envelope delivers it to that rule's targets, and until
//! such a rule exists the same transitions are delivered nowhere.** Sources: the
//! `SetAlarmState` documentation ("when the updated state differs from the previous value,
//! the action configured for the appropriate state is invoked"), and `PutTargets` for the
//! grant a queue target needs ("for Lambda functions, Amazon SNS topics and Amazon SQS
//! queues, EventBridge relies on resource-based policies").
//!
//! [`crate::elb_alarm_transition_reaches_its_rule_queue`] pins that rule over the whole
//! assembled emulator already, and the crate-local suites of
//! `service-com-amazonaws-cloudwatch-graniteserviceversion20100801` pin it over three
//! services mounted alone. All of them turn request authentication OFF and call CloudWatch
//! over its `awsJson1_0` wire, so none of them can see the two axes this suite adds:
//!
//! * **The signatures are verified.** Every call below is signed with the keys this process
//!   publishes once authentication is on — and so is the publication CloudWatch makes
//!   THROUGH THE EMULATOR'S OWN CALLBACK, which a router that authenticates every request
//!   has to accept before EventBridge ever sees the event.
//! * **CloudWatch is called over `awsQuery`.** The form body an AWS CLI or an `awsQuery` SDK
//!   sends is a second of the protocols this service's model declares side by side
//!   (`smithy_query(query="service_info", model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`:
//!   `awsJson1_0`, `awsQuery`, `rpcv2Cbor`), and a transition asked for in one protocol owes
//!   the same publication as one asked for in another.
//!
//! The rows differ along which dimension of the envelope the matching rule filters on: the
//! alarm's name inside `detail`, and the alarm's own ARN in `resources`.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on an explicitly sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call is signed for and addressed in, and so the `region` the envelopes
/// must report.
const REGION: &str = "us-west-2";

/// The `source` and `detail-type` an alarm state change is published under, spelled out here
/// rather than imported, so a wrong constant in the implementation cannot make this suite
/// pass.
const SOURCE: &str = "aws.cloudwatch";
const DETAIL_TYPE: &str = "CloudWatch Alarm State Change";

/// The service principal EventBridge delivers to an SQS target as, which the queue's own
/// policy has to allow the send for.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// CloudWatch's signing name and the API version its form wire carries.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";
const CLOUDWATCH_API_VERSION: &str = "2010-08-01";

/// The states a row drives the alarm through.
const ALARM: &str = "ALARM";
const OK: &str = "OK";

/// The series each row's alarm watches: a namespace of the client's own — `PutMetricData`
/// refuses an `AWS/` namespace — and the metric a client mirrors a load balancer's target
/// health into.
const NAMESPACE: &str = "Custom/TargetHealth";
const METRIC: &str = "UnHealthyHostCount";

/// The period the alarms aggregate over, as the modelled seconds member carries it.
const PERIOD: &str = "60";

/// How long a row waits for the publication and its delivery before giving up, and how often
/// it looks meanwhile. Generous: what is asserted is that the envelope arrives at all, never
/// how fast the host runs.
const DELIVERY_WAIT: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long the NEGATIVE half keeps looking at the queue before concluding that a transition
/// published with no rule on the bus was delivered nowhere. A bound on an absence, so it is
/// the one span a slow host may spend without changing the verdict: a delivery that would
/// have happened is what this looks for, and the positive half below proves the same wiring
/// delivers in a fraction of it.
const WITHHELD_WINDOW: Duration = Duration::from_secs(5);

/// The key pair this process publishes once request authentication is on, which every call
/// below is signed with.
struct Signer {
    key_id: String,
    secret: String,
}

impl Signer {
    /// Sign `request` for `signing_name` in [`REGION`], in the `Authorization` header an SDK
    /// request carries.
    fn sign(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
    ) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }
}

/// One SIGNED `POST /` on the regional endpoint of `signing_name`, answering the HTTP status
/// and the body bytes as answered.
async fn call(
    router: &MultiServiceRouter,
    signer: &Signer,
    signing_name: &str,
    content_type: &str,
    target: Option<&str>,
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", content_type);
    if let Some(target) = target {
        builder = builder.header("x-amz-target", target);
    }
    let built = builder.body(body).expect("the test request is well formed");
    let signed = signer.sign(built, signing_name);
    let (parts, payload) = signed.into_parts();
    let request = http::Request::from_parts(parts, bytes::Bytes::from(payload));
    let response = router.dispatch(request).await;
    (response.status().as_u16(), response.body().to_vec())
}

/// One CloudWatch call over its `awsQuery` FORM wire, requiring a 200 and answering the XML
/// body. The form body is built with the ONE encoder this test crate holds.
async fn monitoring(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    action: &str,
    params: &[(&str, String)],
) -> String {
    let mut body = format!("Action={action}&Version={CLOUDWATCH_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let (status, payload) = call(
        router,
        signer,
        CLOUDWATCH_SIGNING_NAME,
        "application/x-www-form-urlencoded",
        None,
        body.into_bytes(),
    )
    .await;
    let text = String::from_utf8_lossy(&payload).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {action}, got {status} with {text}"
    );
    text
}

/// One `X-Amz-Target` RPC call, requiring a 200 and answering the parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    signing_name: &str,
    content_type: &str,
    target: &str,
    body: &Value,
) -> Value {
    let (status, payload) = call(
        router,
        signer,
        signing_name,
        content_type,
        Some(target),
        body.to_string().into_bytes(),
    )
    .await;
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {target}, got {status} with {parsed}"
    );
    parsed
}

/// One EventBridge call (`awsJson1_1`).
async fn events(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        signer,
        case,
        "events",
        "application/x-amz-json-1.1",
        &format!("AWSEvents.{operation}"),
        body,
    )
    .await
}

/// One Amazon SQS call (`awsJson1_0`).
async fn sqs(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        signer,
        case,
        "sqs",
        "application/x-amz-json-1.0",
        &format!("AmazonSQS.{operation}"),
        body,
    )
    .await
}

/// A queue a rule can target: its url and the ARN a rule names it by, with its own policy
/// allowing EventBridge the send.
async fn given_target_queue(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    name: &str,
) -> (String, String) {
    let created = sqs(
        router,
        signer,
        case,
        "CreateQueue",
        &json!({"QueueName": name}),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        signer,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridgeDelivery",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        router,
        signer,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// The alarm a row drives — a static threshold over one series of the client's own, put over
/// the form wire — and its ARN as CloudWatch reports it.
async fn given_alarm(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    name: &str,
) -> String {
    monitoring(
        router,
        signer,
        case,
        "PutMetricAlarm",
        &[
            ("AlarmName", name.to_string()),
            ("Namespace", NAMESPACE.to_string()),
            ("MetricName", METRIC.to_string()),
            ("Dimensions.member.1.Name", "Row".to_string()),
            ("Dimensions.member.1.Value", name.to_string()),
            ("Statistic", "Maximum".to_string()),
            ("Period", PERIOD.to_string()),
            ("EvaluationPeriods", "1".to_string()),
            ("Threshold", "0".to_string()),
            ("ComparisonOperator", "GreaterThanThreshold".to_string()),
        ],
    )
    .await;
    let described = monitoring(
        router,
        signer,
        case,
        "DescribeAlarms",
        &[("AlarmNames.member.1", name.to_string())],
    )
    .await;
    element(&described, "AlarmArn")
}

/// Ask for the alarm `name` to be in `state`, over the form wire.
async fn force_state(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    name: &str,
    state: &str,
) {
    monitoring(
        router,
        signer,
        case,
        "SetAlarmState",
        &[
            ("AlarmName", name.to_string()),
            ("StateValue", state.to_string()),
            ("StateReason", "driven by this suite".to_string()),
        ],
    )
    .await;
}

/// The `StateValue` CloudWatch reports for the alarm `name`, read over the form wire.
async fn reported_state(
    router: &MultiServiceRouter,
    signer: &Signer,
    case: &str,
    name: &str,
) -> String {
    let described = monitoring(
        router,
        signer,
        case,
        "DescribeAlarms",
        &[("AlarmNames.member.1", name.to_string())],
    )
    .await;
    element(&described, "StateValue")
}

/// Receive everything waiting on the queue at `url` and delete it; answers the parsed
/// envelopes its message bodies carried.
async fn drain(router: &MultiServiceRouter, signer: &Signer, case: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        router,
        signer,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut envelopes = Vec::new();
    for message in &messages {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                signer,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
        envelopes.push(
            serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default())
                .unwrap_or(Value::Null),
        );
    }
    envelopes
}

/// Whether `held` carries an envelope reporting the move into `OK`.
fn reports_ok(held: &[Value]) -> bool {
    held.iter()
        .any(|event| event.pointer("/detail/state/value") == Some(&json!(OK)))
}

/// One row: which dimension of the envelope the matching rule filters on.
struct Row {
    case: &'static str,
    /// The rule's pattern, over the alarm's name and its ARN.
    pattern: fn(&str, &str) -> Value,
}

/// RULE: on a router that verifies request signatures, a metric alarm's state change asked
/// for over CloudWatch's form wire is published onto the default event bus as
/// `aws.cloudwatch` / `CloudWatch Alarm State Change`, naming the alarm's ARN in `resources`
/// and the state it moved to and out of in `detail`, so a rule whose pattern matches it
/// delivers it to the rule's SQS target — and nothing is delivered while no such rule
/// exists.
#[test]
fn an_alarm_transition_reaches_its_rule_s_queue_on_an_authenticating_router() {
    std::thread::Builder::new()
        .name("alarm-state-change-authenticated".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(drive());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// Every row, against ONE assembled router per row.
async fn drive() {
    let rows = [
        Row {
            case: "a rule on the alarm's name inside the detail",
            pattern: |name, _| {
                json!({
                    "source": [SOURCE],
                    "detail-type": [DETAIL_TYPE],
                    "detail": {"alarmName": [name]},
                })
            },
        },
        Row {
            case: "a rule on the alarm's own ARN in the envelope's resources",
            pattern: |_, arn| {
                json!({
                    "source": [SOURCE],
                    "detail-type": [DETAIL_TYPE],
                    "resources": [arn],
                })
            },
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let engine = aws_emulator_router::new_engine();
        let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
        // The whole row runs with request authentication ON, as the emulator serves traffic.
        router.set_request_authentication(true);
        let published = ::emulator_server::SigV4Authenticator::minted_credentials()
            .expect("turning authentication on publishes this process's keys");
        let signer = Signer {
            key_id: published.access_key_id.clone(),
            secret: published.secret_access_key.clone(),
        };

        let alarm_name = format!("target-health-{index}");
        let (url, queue_arn) =
            given_target_queue(&router, &signer, case, &format!("alarm-events-{index}")).await;
        let alarm_arn = given_alarm(&router, &signer, case, &alarm_name).await;

        // The negative half: with no rule on the bus, the transitions this asks for are
        // delivered nowhere, however long the queue is watched.
        force_state(&router, &signer, case, &alarm_name, ALARM).await;
        let forced = reported_state(&router, &signer, case, &alarm_name).await;
        assert_eq!(
            forced, ALARM,
            "case {case:?}: the state the client asked for must be reported, got {forced:?}"
        );
        force_state(&router, &signer, case, &alarm_name, OK).await;
        let watching = Instant::now();
        while watching.elapsed() < WITHHELD_WINDOW {
            let withheld = drain(&router, &signer, case, &url).await;
            assert!(
                withheld.is_empty(),
                "case {case:?}: with no rule on the bus the queue must receive nothing, got \
                 {withheld:?}"
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }

        // Then the rule, and the same pair of transitions again.
        let rule_name = format!("alarm-rule-{index}");
        events(
            &router,
            &signer,
            case,
            "PutRule",
            &json!({
                "Name": &rule_name,
                "EventPattern": (row.pattern)(&alarm_name, &alarm_arn).to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let targeted = events(
            &router,
            &signer,
            case,
            "PutTargets",
            &json!({
                "Rule": &rule_name,
                "Targets": [{"Id": "q", "Arn": &queue_arn}],
            }),
        )
        .await;
        assert_eq!(
            targeted["FailedEntryCount"],
            json!(0),
            "case {case:?}: the queue target must be accepted, got {targeted}"
        );

        // The loop a client retrying a transition runs: ask for the pair again and look at
        // the queue, until the envelope reporting the move into `OK` is there or the
        // deadline passes. The wait is on the CONDITION, so a slow host delays a row
        // instead of breaking it.
        let started = Instant::now();
        let mut held: Vec<Value> = Vec::new();
        while started.elapsed() < DELIVERY_WAIT && !reports_ok(&held) {
            force_state(&router, &signer, case, &alarm_name, ALARM).await;
            force_state(&router, &signer, case, &alarm_name, OK).await;
            held.extend(drain(&router, &signer, case, &url).await);
            if !reports_ok(&held) {
                tokio::time::sleep(LOOK_INTERVAL).await;
            }
        }
        let delivered: Vec<&Value> = held
            .iter()
            .filter(|event| event.pointer("/detail/state/value") == Some(&json!(OK)))
            .collect();
        assert!(
            !delivered.is_empty(),
            "case {case:?}: expected an envelope reporting the {OK} transition of \
             {alarm_name:?} on {url}, got {held:?}"
        );
        // A transition reports the state it moved OUT OF, so the pair the loop asked for —
        // `ALARM` and then `OK`, one call after the other — is reported as the move out of
        // `ALARM` that it is.
        assert!(
            delivered.iter().any(|envelope| {
                envelope.pointer("/detail/previousState/value") == Some(&json!(ALARM))
            }),
            "case {case:?}: expected an envelope reporting the move out of {ALARM} the \
             loop's pair made, got {delivered:?}"
        );
        for envelope in delivered {
            for (pointer, expected) in [
                ("/source", json!(SOURCE)),
                ("/detail-type", json!(DETAIL_TYPE)),
                ("/region", json!(REGION)),
                ("/account", json!(engine::EMULATOR_ACCOUNT_ID)),
                ("/resources", json!([&alarm_arn])),
                ("/detail/alarmName", json!(&alarm_name)),
                ("/detail/state/value", json!(OK)),
            ] {
                let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected {pointer} to be {expected}, got {actual} in \
                     {envelope}"
                );
            }
            // Every transition carries WHEN it happened and WHY, which is what a consumer of
            // the bus reads off one.
            for pointer in ["/detail/state/timestamp", "/detail/state/reason"] {
                let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
                assert!(
                    actual.as_str().is_some_and(|text| !text.is_empty()),
                    "case {case:?}: expected {pointer} to carry a value, got {actual} in \
                     {envelope}"
                );
            }
        }
    }
}
