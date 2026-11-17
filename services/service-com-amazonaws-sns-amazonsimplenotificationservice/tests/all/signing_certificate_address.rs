// SPDX-License-Identifier: CC0-1.0

//! WHERE the certificate that verifies a delivered Amazon SNS message is published, and who
//! may fetch it.
//!
//! The RULE asserted here: the `SigningCertURL` a signed message carries (`SigningCertUrl`
//! in a function's event record) is an `https` URL on Amazon SNS's OWN endpoint host for the
//! Region the TOPIC is in — `sns.<region>.amazonaws.com` — whose path names the certificate
//! file (`/SimpleNotificationService-<id>.pem`), and an UNAUTHENTICATED fetch of that URL
//! answers the X.509 certificate whose public key verifies the message's signature.
//!
//! That is what the documented verification procedure needs of it: a subscriber "verif[ies]
//! that the signing certificate URL is from an Amazon SNS address" before fetching it, then
//! "retrieve[s] the certificate … and extract[s] the public key from it" and checks the
//! signature (Amazon SNS Developer Guide, "Verifying the signatures of Amazon SNS
//! messages"). A URL on any other host is one such a subscriber refuses the message over,
//! and a fetch that needs a credential is one it cannot make: it holds none for the service
//! that delivered to it.
//!
//! The rows differ along what the rule generalises over: the Region the topic lives in
//! (which is the host the URL names), the `SignatureVersion` the topic signs under (both
//! published values, which select the two digests), and the KIND of document the URL travels
//! in — the JSON envelope a queue subscriber receives and the `Records` envelope a
//! subscribed function receives, which spell the member differently.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::notification_signature::{canonical_string, verifies, SIGNED_MEMBERS};
use crate::topics_subscriptions_publish::percent_encode;

/// The account every ARN below carries.
const ACCOUNT: &str = "000000000000";

/// SNS and Amazon SQS over ONE engine, driven in whatever Region a row names: a subscription
/// of a topic in that Region delivers to a queue of that Region, which is where the delivered
/// envelope — and the certificate URL it carries — is read.
struct Emulator {
    router: MultiServiceRouter,
}

fn emulator(authenticates: bool) -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(authenticates);
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    Emulator { router }
}

/// A SigV4 `Authorization` whose credential scope names `signing_name` in `region`, which is
/// what routes the request and what the Region of its resources is read from. Neither the key
/// id nor the signature is a credential: this harness turns request authentication off.
fn authorization(region: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{region}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery form call against SNS in `region`, which must succeed.
async fn sns(
    emulator: &Emulator,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    sns_on(
        emulator,
        &format!("sns.{region}.amazonaws.com"),
        region,
        action,
        params,
    )
    .await
}

/// The same, addressed to `authority` — the host the request REACHES, which for a client on
/// the machine this emulator runs on is the emulator's own endpoint rather than one of AWS's
/// own names. The credential scope still names the Region, which is what routes the call and
/// what its resources belong to.
async fn sns_on(
    emulator: &Emulator,
    authority: &str,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-03-31");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&percent_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", authority)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(region, "sns"))
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "{action} in {region}: expected HTTP 200, got {status} / {text}"
    );
    text
}

/// One `awsJson1_0` call against Amazon SQS in `region`, which must succeed.
async fn sqs(emulator: &Emulator, region: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization(region, "sqs"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "{operation} in {region}: expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// FETCH `url` through this same router with NO credential at all, as the subscriber that was
/// handed it does: the host and the path are taken from the URL itself. Answers the status and
/// the body.
async fn fetched_anonymously(emulator: &Emulator, url: &str) -> (u16, String) {
    let addressed = url.split_once("://").map_or(url, |(_, rest)| rest);
    let (host, path) = match addressed.find('/') {
        Some(at) => (&addressed[..at], &addressed[at..]),
        None => (addressed, "/"),
    };
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", host)
        .body(bytes::Bytes::new())
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// The text of the first `<tag>…</tag>` element of an awsQuery response.
fn element(body: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body
        .find(&open)
        .unwrap_or_else(|| panic!("no <{tag}> in {body}"))
        + open.len();
    let end = body[start..]
        .find(&close)
        .unwrap_or_else(|| panic!("no </{tag}> in {body}"));
    body[start..start + end].to_string()
}

/// A queue of `region` an SNS topic may deliver to: its url and the ARN a subscription names
/// it by, carrying the policy that grants this service the send.
async fn given_queue(emulator: &Emulator, region: &str, name: &str) -> (String, String) {
    let created = sqs(emulator, region, "CreateQueue", &json!({"QueueName": name})).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateQueue {name}: no QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        emulator,
        region,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("GetQueueAttributes {name}: no QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowSns",
            "Effect": "Allow",
            "Principal": {"Service": "sns.amazonaws.com"},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        emulator,
        region,
        "SetQueueAttributes",
        &json!({"QueueUrl": url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// The one message waiting on the queue at `url`, as the JSON envelope it is.
async fn delivered(emulator: &Emulator, region: &str, url: &str, case: &str) -> Value {
    let received = sqs(
        emulator,
        region,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        messages.len(),
        1,
        "case {case:?}: expected exactly one delivered message, got {received}"
    );
    let body = messages[0]["Body"].as_str().unwrap_or_default();
    serde_json::from_str(body).unwrap_or_else(|error| {
        panic!("case {case:?}: the delivered body must be the JSON envelope: {error} / {body}")
    })
}

/// The address the certificate is published at, as far as the rule fixes it: the scheme, the
/// AUTHORITY the fetch is addressed to, and the file name's own two ends.
fn assert_published_at(case: &str, authority: &str, url: &str) {
    let expected_prefix = format!("https://{authority}/SimpleNotificationService-");
    assert!(
        url.starts_with(&expected_prefix) && url.ends_with(".pem"),
        "case {case:?}: expected the certificate at {expected_prefix}<id>.pem, got {url:?}"
    );
    assert!(
        !url.contains('?') && !url.contains('&'),
        "case {case:?}: the certificate address carries no query, got {url:?}"
    );
}

/// The same for a message whose request reached one of AWS's own hosts, where the address is
/// Amazon SNS's own endpoint of the message's Region.
fn assert_published_on_the_sns_endpoint(case: &str, region: &str, url: &str) {
    assert_published_at(case, &format!("sns.{region}.amazonaws.com"), url);
}

/// One row: the Region the topic lives in and the `SignatureVersion` it signs under.
struct Row {
    case: &'static str,
    region: &'static str,
    /// The version the topic is set to, or `None` to leave it on the account default.
    asked_for: Option<&'static str>,
    /// The version the delivered message must then declare.
    version: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a topic of the default Region, signing under the default version",
            region: "us-east-1",
            asked_for: None,
            version: "1",
        },
        Row {
            case: "a topic of another Region, signing under version 1",
            region: "us-west-2",
            asked_for: Some("1"),
            version: "1",
        },
        Row {
            case: "a topic of a third Region, signing under version 2",
            region: "eu-central-1",
            asked_for: Some("2"),
            version: "2",
        },
    ]
}

#[tokio::test]
async fn a_delivered_envelope_names_the_certificate_on_the_sns_endpoint_of_its_region() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let emulator = emulator(false);
        let topic = element(
            &sns(
                &emulator,
                region,
                "CreateTopic",
                &[("Name", &format!("signed-address-{index}"))],
            )
            .await,
            "TopicArn",
        );
        assert!(
            topic.starts_with(&format!("arn:aws:sns:{region}:{ACCOUNT}:")),
            "case {case:?}: expected a topic of {region}, got {topic:?}"
        );
        if let Some(asked_for) = row.asked_for {
            sns(
                &emulator,
                region,
                "SetTopicAttributes",
                &[
                    ("TopicArn", &topic),
                    ("AttributeName", "SignatureVersion"),
                    ("AttributeValue", asked_for),
                ],
            )
            .await;
        }
        let (queue_url, queue_arn) =
            given_queue(&emulator, region, &format!("signed-address-{index}")).await;
        sns(
            &emulator,
            region,
            "Subscribe",
            &[
                ("TopicArn", &topic),
                ("Protocol", "sqs"),
                ("Endpoint", &queue_arn),
            ],
        )
        .await;
        sns(
            &emulator,
            region,
            "Publish",
            &[("TopicArn", &topic), ("Message", "a signed body")],
        )
        .await;

        let envelope = delivered(&emulator, region, &queue_url, case).await;
        assert_eq!(
            envelope["SignatureVersion"].as_str(),
            Some(row.version),
            "case {case:?}: expected the version the topic signs under, got {envelope}"
        );
        let url = envelope["SigningCertURL"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no SigningCertURL in {envelope}"))
            .to_string();
        assert_published_on_the_sns_endpoint(case, region, &url);

        // A subscriber fetches that URL with no credential of its own and verifies the
        // message with the certificate it answers.
        let (status, certificate) = fetched_anonymously(&emulator, &url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the certificate address must answer an anonymous fetch, got \
             {status} / {certificate}"
        );
        assert!(
            certificate.starts_with("-----BEGIN CERTIFICATE-----"),
            "case {case:?}: expected a PEM certificate at {url:?}, got {certificate}"
        );
        let signature = envelope["Signature"].as_str().unwrap_or_default();
        let signature = ::aws_smithy_types::base64::decode(signature)
            .unwrap_or_else(|error| panic!("case {case:?}: the signature is base64: {error}"));
        assert!(
            verifies(
                &certificate,
                row.version,
                &canonical_string(&envelope),
                &signature,
            ),
            "case {case:?}: the certificate at {url:?} must verify the delivered signature \
             over {SIGNED_MEMBERS:?}, got {envelope}"
        );
    }
}

/// A publish whose request reached THE EMULATOR'S OWN ENDPOINT — the address a client on the
/// machine this process runs on talks to — names the certificate THERE, because that is the
/// address such a client (and such a subscriber) can fetch it at. The rows differ along the
/// authority the request reached: a loopback address with a port, and a host name with one.
#[tokio::test]
async fn a_publish_reaching_the_emulators_own_endpoint_names_the_certificate_there() {
    let rows = [
        ("a loopback address", "127.0.0.1:9297"),
        ("a host name", "localhost:38211"),
    ];
    for (index, (case, authority)) in rows.into_iter().enumerate() {
        let emulator = emulator(false);
        let region = "us-east-1";
        let topic = element(
            &sns_on(
                &emulator,
                authority,
                region,
                "CreateTopic",
                &[("Name", &format!("own-endpoint-{index}"))],
            )
            .await,
            "TopicArn",
        );
        let (queue_url, queue_arn) =
            given_queue(&emulator, region, &format!("own-endpoint-{index}")).await;
        sns_on(
            &emulator,
            authority,
            region,
            "Subscribe",
            &[
                ("TopicArn", &topic),
                ("Protocol", "sqs"),
                ("Endpoint", &queue_arn),
            ],
        )
        .await;
        sns_on(
            &emulator,
            authority,
            region,
            "Publish",
            &[("TopicArn", &topic), ("Message", "a signed body")],
        )
        .await;

        let envelope = delivered(&emulator, region, &queue_url, case).await;
        let url = envelope["SigningCertURL"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no SigningCertURL in {envelope}"))
            .to_string();
        assert_published_at(case, authority, &url);
        let (status, certificate) = fetched_anonymously(&emulator, &url).await;
        assert_eq!(
            (status, certificate.starts_with("-----BEGIN CERTIFICATE-----")),
            (200, true),
            "case {case:?}: the address a client reached this emulator at must answer the \
             certificate, got {status} / {certificate}"
        );
    }
}

/// The address is served to a caller presenting NO credential even on a router that verifies
/// every request signature — the configuration the emulator serves traffic in — because the
/// subscriber that was handed the URL holds no credential for this service. An id this
/// process holds no certificate for is answered by the same endpoint, as the document it does
/// not have rather than as a caller it would not admit.
#[tokio::test]
async fn the_certificate_address_admits_a_caller_with_no_credential() {
    let emulator = emulator(true);
    let rows = [
        (
            "an id this process holds no certificate for",
            "https://sns.us-east-1.amazonaws.com/SimpleNotificationService-00000000.pem",
        ),
        (
            "the same, on another Region's endpoint",
            "https://sns.eu-central-1.amazonaws.com/SimpleNotificationService-00000000.pem",
        ),
    ];
    for (case, url) in rows {
        let (status, body) = fetched_anonymously(&emulator, url).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the certificate endpoint's own answer for a document it \
             does not hold, got {status} / {body}"
        );
        assert_eq!(
            body, "no such signing certificate",
            "case {case:?}: expected that endpoint's own sentence, got {body:?}"
        );
    }
}

/// The handler of a subscribed function that FETCHES the certificate the record named it, as
/// a subscriber verifying the message does, and forwards what that fetch answered to the
/// capture queue — the whole point of the address being the subscriber's to follow.
///
/// The deadline the fetch names is deliberately far above what fetching an address inside
/// this same process costs, which is a fraction of a second: the fetch stays bounded, so a
/// handler is never left waiting on an address that answers nothing, while what the
/// subscriber reports is what that address answered rather than how loaded the machine was
/// when it asked.
const VERIFYING_HANDLER: &str = r#"
import json
import urllib.request

import boto3


def lambda_handler(event, context):
    record = event["Records"][0]["Sns"]
    url = record["SigningCertUrl"]
    try:
        with urllib.request.urlopen(url, timeout=60) as answered:
            status = answered.status
            head = answered.read().decode("ascii").splitlines()[0]
    except OSError as error:
        status, head = 0, "error: {0}".format(error)
    boto3.client("sqs").send_message(
        QueueUrl="{queue_url}",
        MessageBody=json.dumps({"url": url, "status": status, "head": head}),
    )
    return {"fetched": status}
"#;

/// An address on Amazon SNS's own endpoint whose file name is an id THIS PROCESS holds no
/// certificate for, which the certificate endpoint answers as the document it does not hold
/// ([`the_certificate_address_admits_a_caller_with_no_credential`]).
///
/// What the warm-up invocation below hands the handler: it names an address rather than a
/// document, because the warm-up is about standing the handler's execution environment up and
/// states nothing about any certificate — the delivery under test is what fetches one.
const NO_CERTIFICATE_HELD_URL: &str =
    "https://sns.us-east-1.amazonaws.com/SimpleNotificationService-00000000.pem";

/// How many times the capture queue is read while it keeps yielding what the warm-up's own run
/// wrote: that run makes ONE report, and one read answers up to ten messages, so this is far
/// above the rounds taking it off can need.
const DRAIN_ROUNDS: usize = 16;

/// Run the handler of `function` ONCE, directly, so the execution environment its next
/// invocation is served in is already STANDING — and take the report that run wrote off
/// `capture_url` for good, leaving the queue as empty as it found it.
///
/// WHY: an invocation that finds no execution environment standing builds one, and that
/// INIT phase — the interpreter this emulator embeds, the managed runtime inside it and the
/// handler module's own imports — is seconds of this process's own work, while the handler's
/// run that follows it is milliseconds ("Lambda maintains the execution environment for some
/// time", and an invocation that finds one is the handler's run alone). The chain under test
/// is bounded by what the LAMBDA SERVICE allows one delivery attempt, and an attempt the host
/// gives up on is not retried, so leaving that build inside the attempt makes the test's
/// outcome a measurement of how much of this host the other suites running beside it are
/// using. Paying it HERE, before the publish, leaves the attempt the handler's own run with
/// the whole of that allowance to spare.
///
/// Best effort by design: whatever this invocation comes to, the publish below drives exactly
/// the same chain — a run that could not be made here only leaves the delivery to pay the
/// initialization, as it did before.
///
/// Nothing here waits out a span: the invocation is answered once its handler has run, so what
/// that run wrote is already on the queue when the reads below ask for it.
async fn given_a_standing_environment(
    emulator: &crate::lambda_subscription_delivery::Emulator,
    function: &str,
    capture_url: &str,
) {
    use crate::lambda_subscription_delivery::{lambda, sqs as delivery_sqs};

    let _answered = lambda(
        emulator,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        json!({"Records": [{"Sns": {"SigningCertUrl": NO_CERTIFICATE_HELD_URL}}]}),
    )
    .await;
    // The invocation is answered after the handler has run, so whatever that run wrote is on
    // the queue by now: the queue is read while it keeps yielding messages, and each one is
    // DELETED rather than left in flight, so none of it can come back while the delivery's
    // own report is being waited for. The rounds are bounded ([`DRAIN_ROUNDS`]) because this
    // helper states nothing the test asserts and must always end.
    for _round in 0..DRAIN_ROUNDS {
        let received = delivery_sqs(
            emulator,
            "ReceiveMessage",
            &json!({"QueueUrl": capture_url, "MaxNumberOfMessages": 10}),
        )
        .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            return;
        }
        for message in messages {
            delivery_sqs(
                emulator,
                "DeleteMessage",
                &json!({
                    "QueueUrl": capture_url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
        }
    }
}

/// A SUBSCRIBER that follows the address reaches the certificate: the handler of a subscribed
/// function fetches the URL the record handed it — with no credential of its own, over its
/// own HTTP client — and is answered the PEM document, which is what lets it verify the
/// message at all.
#[tokio::test]
async fn a_subscriber_reaches_the_certificate_at_the_address_it_was_given() {
    use crate::lambda_subscription_dead_letter::given_queue as capture_queue;
    use crate::lambda_subscription_delivery::{
        emulator as delivery_emulator, given_function_running, grant, invoked, publish,
        sns as delivery_sns, subscribe, INVOKE_ACTION, LAMBDA_PROTOCOL, SNS_PRINCIPAL,
    };
    use crate::sqs_subscription_delivery::element as delivery_element;

    let emulator = delivery_emulator();
    let (capture_url, _capture_arn) = capture_queue(&emulator, "certificate-capture").await;
    let function = "certificate-verifier";
    let function_arn = given_function_running(
        &emulator,
        function,
        &VERIFYING_HANDLER.replace("{queue_url}", &capture_url),
    )
    .await;
    // The handler's execution environment is stood up HERE rather than inside the delivery
    // attempt the publish below sets off, so that attempt is the handler's own run and nothing
    // besides ([`given_a_standing_environment`]).
    given_a_standing_environment(&emulator, function, &capture_url).await;
    let topic = delivery_element(
        &delivery_sns(&emulator, "CreateTopic", &[("Name", "certificate-topic")]).await,
        "TopicArn",
    );
    grant(
        &emulator,
        function,
        "sns-invoke",
        INVOKE_ACTION,
        SNS_PRINCIPAL,
        Some(&topic),
    )
    .await;
    subscribe(&emulator, &topic, LAMBDA_PROTOCOL, &function_arn).await;
    publish(&emulator, &topic, &[("Message", "a message to verify")]).await;

    // The event is queued as the publish is answered and the handler runs a moment later, so
    // both waits below LOOK for what they wait on — the queued event, then the report the
    // handler's own fetch wrote — and are answered the moment it is there. Neither waits out a
    // span, and the ceiling they give up after is the one the LAMBDA SERVICE's own bounds set
    // for this chain (`DELIVERY_BUDGET`: the queueing delay, the wait for run capacity, the
    // initialization of the execution environment, the handler's run, and a retried attempt
    // after the published wait between them). So how loaded the machine is decides when this
    // test finishes and never whether it passes.
    let events = invoked(&emulator, function, 1).await;
    assert_eq!(
        events.len(),
        1,
        "expected the subscribed function to be invoked once, got {events:?}"
    );
    let held = crate::lambda_subscription_dead_letter::held(&emulator, &capture_url, 1).await;
    assert_eq!(
        held.len(),
        1,
        "expected the handler to report its own fetch of the certificate, got {held:?}"
    );
    let reported: Value = serde_json::from_str(&held[0])
        .unwrap_or_else(|error| panic!("the handler reports JSON, got {error} for {:?}", held[0]));
    assert_published_on_the_sns_endpoint(
        "a subscribed function's own record",
        "us-east-1",
        reported["url"].as_str().unwrap_or_default(),
    );
    assert_eq!(
        (reported["status"].as_i64(), reported["head"].as_str()),
        (Some(200), Some("-----BEGIN CERTIFICATE-----")),
        "expected the subscriber's own fetch of {:?} to answer the certificate, got {reported}",
        reported["url"]
    );
}
