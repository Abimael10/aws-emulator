// SPDX-License-Identifier: CC0-1.0

//! WHERE the certificate that verifies a delivered Amazon SNS message is published, and who
//! may fetch it.
//!
//! The RULE asserted here: one document SNS delivers names ONE authority in EVERY URL member
//! it carries — the `SigningCertURL` (`SigningCertUrl` in a function's event record) beside
//! its `SubscribeURL` / `UnsubscribeURL` siblings — and that authority is Amazon SNS's own
//! endpoint host for the Region the TOPIC is in, `sns.<region>.amazonaws.com`, as an `https`
//! URL on the default port, whatever address the publishing client itself reached this
//! emulator at; the certificate's path names the certificate file
//! (`/SimpleNotificationService-<id>.pem`); and an UNAUTHENTICATED fetch of that URL answers
//! the X.509 certificate whose public key verifies the message's signature.
//!
//! That is what the documented verification procedure needs of it: a subscriber "verif[ies]
//! that the signing certificate URL is from an Amazon SNS address" before fetching it, then
//! "retrieve[s] the certificate … and extract[s] the public key from it" and checks the
//! signature (Amazon SNS Developer Guide, "Verifying the signatures of Amazon SNS
//! messages"). A URL on any other host — a sibling member's host included, and the address
//! some client happened to reach this process at above all, which is an endpoint of no
//! service and so a document a subscriber's own fetch never reaches — is one such a
//! subscriber refuses the message over, and a fetch that needs a credential is one it cannot
//! make: it holds none for the service that delivered to it.
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
/// machine this process runs on talks to — still names the certificate on this SERVICE's own
/// endpoint host for the message's Region, not on the address that client happened to reach:
/// an address of the loopback interface is an endpoint of no service, so a certificate
/// announced there is one a subscriber's own fetch never resolves, while the host named here
/// is claimed by this service however the fetch arrives. The rows differ along the authority
/// the request reached: a loopback address with a port, and a host name with one.
#[tokio::test]
async fn a_publish_reaching_the_emulators_own_endpoint_names_the_certificate_on_the_endpoint_host()
{
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
        assert_published_on_the_sns_endpoint(case, region, &url);
        let (status, certificate) = fetched_anonymously(&emulator, &url).await;
        assert_eq!(
            (status, certificate.starts_with("-----BEGIN CERTIFICATE-----")),
            (200, true),
            "case {case:?}: the announced address must answer the certificate to a client that \
             reached this emulator at {authority:?}, got {status} / {certificate}"
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

/// The Region a request carrying NO credential addresses, which is the account default this
/// API reads for one (`state::request_region`): the Region a link followed with no credential
/// of its own is served in.
const DEFAULT_VISIT_REGION: &str = "us-east-1";

/// The authority `url` addresses: what follows its `https://` up to the path, which is the
/// `host[:port]` a client would connect to. A failing case for a value that is no `https` URL
/// at all, because every member read here is one.
fn https_authority(case: &str, member: &str, url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {case:?}: {member} must be an https URL, got {url:?}"));
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Assert `url`'s authority is `announced`, which also says it carries no port component,
/// since `announced` is a bare host.
fn assert_announced_at(case: &str, member: &str, announced: &str, url: &str) {
    let authority = https_authority(case, member, url);
    assert_eq!(
        authority, announced,
        "case {case:?}: {member} must be announced on {announced:?}, on the default https port \
         and no other, got {url:?}"
    );
}

/// One row: the authority the publishing request reaches this emulator at, the Region its
/// topic lives in, and the authority every URL member of the document it produces must then
/// name.
struct AuthorityRow {
    case: &'static str,
    /// The `Host` the call carries, as a client's own endpoint names it.
    reached: &'static str,
    /// The Region the topic is created in, which is the Region the delivered message belongs
    /// to.
    region: &'static str,
    /// The authority the delivered document announces: this service's own endpoint host for
    /// that Region.
    announced: &'static str,
}

/// The rows, differing along the FORM of the address the client reached — the published data
/// port on the loopback interface, an ephemeral port on it, a host name carrying a port, a
/// host name carrying none, an IPv6 literal, and one of AWS's own endpoint hosts (the address
/// a caller that resolves AWS's own names reached) — and along the Region the message belongs
/// to, which is the one thing the announced authority does depend on.
fn authority_rows() -> Vec<AuthorityRow> {
    vec![
        AuthorityRow {
            case: "a loopback address on the published data port",
            reached: "127.0.0.1:9297",
            region: "us-east-1",
            announced: "sns.us-east-1.amazonaws.com",
        },
        AuthorityRow {
            case: "a loopback address on an ephemeral port, a topic of another Region",
            reached: "127.0.0.1:43677",
            region: "us-west-2",
            announced: "sns.us-west-2.amazonaws.com",
        },
        AuthorityRow {
            case: "a host name carrying a port",
            reached: "localhost:38211",
            region: "us-east-1",
            announced: "sns.us-east-1.amazonaws.com",
        },
        AuthorityRow {
            case: "a host name carrying no port, a topic of a third Region",
            reached: "localhost",
            region: "eu-central-1",
            announced: "sns.eu-central-1.amazonaws.com",
        },
        AuthorityRow {
            case: "an IPv6 loopback literal carrying a port",
            reached: "[::1]:9297",
            region: "us-east-1",
            announced: "sns.us-east-1.amazonaws.com",
        },
        AuthorityRow {
            case: "one of AWS's own endpoint hosts",
            reached: "sns.us-east-1.amazonaws.com",
            region: "us-east-1",
            announced: "sns.us-east-1.amazonaws.com",
        },
    ]
}

/// RULE: every URL member of ONE delivered notification envelope names ONE authority — this
/// service's own endpoint host for the message's Region, on the default `https` port, whatever
/// address the publishing client reached — and that authority SERVES both of the documents
/// those members lead to: the signing certificate the envelope's `Signature` verifies against,
/// and the unsubscribe call its sibling link writes.
#[tokio::test]
async fn every_url_member_of_a_delivered_envelope_names_one_served_authority() {
    for (index, row) in authority_rows().iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let emulator = emulator(false);
        let topic = element(
            &sns_on(
                &emulator,
                row.reached,
                region,
                "CreateTopic",
                &[("Name", &format!("announced-authority-{index}"))],
            )
            .await,
            "TopicArn",
        );
        let (queue_url, queue_arn) =
            given_queue(&emulator, region, &format!("announced-authority-{index}")).await;
        let subscription = element(
            &sns_on(
                &emulator,
                row.reached,
                region,
                "Subscribe",
                &[
                    ("TopicArn", &topic),
                    ("Protocol", "sqs"),
                    ("Endpoint", &queue_arn),
                    ("ReturnSubscriptionArn", "true"),
                ],
            )
            .await,
            "SubscriptionArn",
        );
        sns_on(
            &emulator,
            row.reached,
            region,
            "Publish",
            &[("TopicArn", &topic), ("Message", "a body to verify")],
        )
        .await;

        let envelope = delivered(&emulator, region, &queue_url, case).await;
        let certificate_url = envelope["SigningCertURL"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no SigningCertURL in {envelope}"))
            .to_string();
        let unsubscribe_url = envelope["UnsubscribeURL"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no UnsubscribeURL in {envelope}"))
            .to_string();
        // ONE authority for both members, and the certificate's path is the file name AWS
        // names that document by.
        assert_announced_at(case, "SigningCertURL", row.announced, &certificate_url);
        assert_announced_at(case, "UnsubscribeURL", row.announced, &unsubscribe_url);
        assert_published_at(case, row.announced, &certificate_url);
        assert!(
            unsubscribe_url
                .ends_with(&format!("?Action=Unsubscribe&SubscriptionArn={subscription}")),
            "case {case:?}: the unsubscribe link must write the call that ends this \
             subscription, got {unsubscribe_url:?}"
        );

        // The certificate is SERVED at the announced authority, and it verifies what was
        // delivered — the whole of what the documented verification procedure does.
        let (status, certificate) = fetched_anonymously(&emulator, &certificate_url).await;
        assert_eq!(
            (status, certificate.starts_with("-----BEGIN CERTIFICATE-----")),
            (200, true),
            "case {case:?}: an anonymous fetch of {certificate_url:?} must answer the PEM \
             certificate, got {status} / {certificate}"
        );
        let version = envelope["SignatureVersion"].as_str().unwrap_or_default();
        let signature = ::aws_smithy_types::base64::decode(
            envelope["Signature"].as_str().unwrap_or_default(),
        )
        .unwrap_or_else(|error| panic!("case {case:?}: the signature is base64: {error}"));
        assert!(
            verifies(
                &certificate,
                version,
                &canonical_string(&envelope),
                &signature,
            ),
            "case {case:?}: the certificate at {certificate_url:?} must verify the delivered \
             signature over {SIGNED_MEMBERS:?}, got {envelope}"
        );

        // And the sibling link is served at that same authority: following it performs the
        // call it writes, which is what makes the announced host ONE address and not two.
        //
        // Only for a subscription of the Region a visit resolves to: a link is followed with
        // NO credential, so the Region such a request addresses is the account default
        // (`state::request_region`), and a subscription of another Region is refused by this
        // API's region isolation — which is that reading's subject, not this rule's.
        if row.region == DEFAULT_VISIT_REGION {
            let (status, ended) = fetched_anonymously(&emulator, &unsubscribe_url).await;
            assert_eq!(
                (status, ended.contains("UnsubscribeResponse")),
                (200, true),
                "case {case:?}: following {unsubscribe_url:?} must perform the unsubscribe it \
                 writes, got {status} / {ended}"
            );
        }
    }
}

/// The handler of an `https` subscriber that does what the Developer Guide tells a subscriber
/// to do before trusting anything it was POSTed, and ADMITS only what passes: it reads
/// `SigningCertURL` out of the document, requires it to be `https` on this service's own
/// endpoint host for the Region the message's `TopicArn` names with no port other than the
/// default and the certificate file for a path, FETCHES it over its own HTTP client, rebuilds
/// the canonical string the document's `Type` selects, and verifies the RSASSA-PKCS1-v1_5
/// signature with the public key of the certificate it fetched. It reports every request on
/// the capture queue whose url is formatted into it — with the reason it refused one, or an
/// empty reason for one it admitted — and answers 200 only for a document it admitted, 403
/// for every other, which is what a webhook that verifies its deliveries does.
///
/// Written out here rather than asked of a library because the guest's own standard library
/// is what a handler has: the digests are `hashlib`'s, the arithmetic is Python's, and what is
/// written is the DER walk to the certificate's public key and the padding layout RFC 8017
/// section 9.2 fixes.
const VERIFYING_SUBSCRIBER: &str = r#"
import base64
import hashlib
import json
import re
import urllib.parse
import urllib.request

import boto3


# The members each delivered document's signature is taken over, in the order the canonical
# string carries them (Developer Guide, "Verifying the signatures of Amazon SNS messages").
SIGNED_MEMBERS = {
    "Notification": ["Message", "MessageId", "Subject", "Timestamp", "TopicArn", "Type"],
    "SubscriptionConfirmation": [
        "Message", "MessageId", "SubscribeURL", "Timestamp", "Token", "TopicArn", "Type",
    ],
    "UnsubscribeConfirmation": [
        "Message", "MessageId", "SubscribeURL", "Timestamp", "Token", "TopicArn", "Type",
    ],
}

# The file name the signing certificate is published under.
CERTIFICATE_PATH = re.compile(r"^/SimpleNotificationService-[0-9a-f]+\.pem$")

# The EMSA-PKCS1-v1_5 DigestInfo prefix of the digest each SignatureVersion selects.
DIGEST_INFO = {
    "1": bytes.fromhex("3021300906052b0e03021a05000414"),
    "2": bytes.fromhex("3031300d060960864801650304020105000420"),
}


def tlvs(data, at, end, out):
    """Every (tag, start, end) in the DER between `at` and `end`, descending into each."""
    while at + 1 < end:
        tag = data[at]
        length = data[at + 1]
        at += 2
        if length & 0x80:
            count = length & 0x7F
            length = int.from_bytes(data[at:at + count], "big")
            at += count
        if at + length > end:
            raise ValueError("the DER runs past its container")
        out.append((tag, at, at + length))
        if tag & 0x20:
            tlvs(data, at, at + length, out)
        at += length
    return out


def public_key(pem):
    """The RSA (modulus, exponent) the certificate certifies, or None."""
    der = base64.b64decode(
        "".join(line for line in pem.splitlines() if "CERTIFICATE" not in line)
    )
    for tag, start, end in tlvs(der, 0, len(der), []):
        if tag != 0x03 or start + 1 >= end:
            continue
        # A SubjectPublicKeyInfo's BIT STRING holds the RSAPublicKey SEQUENCE of two
        # INTEGERs, after the octet counting its unused bits.
        key = der[start + 1:end]
        try:
            items = tlvs(key, 0, len(key), [])
        except (ValueError, IndexError):
            continue
        numbers = [item for item in items if item[0] == 0x02]
        if len(items) == 3 and len(numbers) == 2:
            return (
                int.from_bytes(key[numbers[0][1]:numbers[0][2]], "big"),
                int.from_bytes(key[numbers[1][1]:numbers[1][2]], "big"),
            )
    return None


def verifies(pem, version, canonical, signature):
    """Whether `signature` is the signature of `canonical` under the certificate's key."""
    key = public_key(pem)
    prefix = DIGEST_INFO.get(version)
    if key is None or prefix is None:
        return False
    modulus, exponent = key
    if version == "2":
        digest = hashlib.sha256(canonical).digest()
    else:
        digest = hashlib.sha1(canonical).digest()
    size = (modulus.bit_length() + 7) // 8
    if len(signature) != size:
        return False
    recovered = pow(int.from_bytes(signature, "big"), exponent, modulus)
    padding = b"\xff" * (size - 3 - len(prefix) - len(digest))
    return recovered.to_bytes(size, "big") == (
        b"\x00\x01" + padding + b"\x00" + prefix + digest
    )


def refusal(document):
    """Why this request is refused, or "" for one a verifying subscriber admits."""
    members = SIGNED_MEMBERS.get(document.get("Type"))
    if members is None:
        return "no signed member set for Type %r" % (document.get("Type"),)
    announced = "sns.%s.amazonaws.com" % (document.get("TopicArn", "").split(":")[3],)
    address = urllib.parse.urlparse(document.get("SigningCertURL") or "")
    if address.scheme != "https":
        return "SigningCertURL is not https: %r" % (document.get("SigningCertURL"),)
    if address.hostname != announced:
        return "SigningCertURL names %r, not the endpoint host %r" % (
            address.hostname, announced,
        )
    if address.port not in (None, 443):
        return "SigningCertURL names port %r" % (address.port,)
    if CERTIFICATE_PATH.match(address.path) is None:
        return "SigningCertURL path %r is no certificate file" % (address.path,)
    try:
        with urllib.request.urlopen(document["SigningCertURL"], timeout=60) as answered:
            if answered.status != 200:
                return "the certificate fetch answered %r" % (answered.status,)
            pem = answered.read().decode("ascii")
    except OSError as error:
        return "the certificate fetch failed: %s" % (error,)
    canonical = "".join(
        "%s\n%s\n" % (member, document[member]) for member in members if member in document
    ).encode("utf-8")
    signature = base64.b64decode(document.get("Signature") or "")
    if not verifies(pem, document.get("SignatureVersion"), canonical, signature):
        return "the signature does not verify against the fetched certificate"
    return ""


def lambda_handler(event, context):
    body = event.get("body") or ""
    if event.get("isBase64Encoded"):
        body = base64.b64decode(body).decode("utf-8")
    try:
        document = json.loads(body)
    except ValueError:
        document = {}
    refused = refusal(document) if document else "the request carries no document"
    boto3.client("sqs").send_message(
        QueueUrl="{queue_url}",
        MessageBody=json.dumps({"refused": refused, "document": document}),
    )
    if refused:
        return {"statusCode": 403, "body": "refused"}
    return {"statusCode": 200, "body": "admitted"}
"#;

/// How long the reports of the verifying subscriber are waited for: far longer than the
/// chain — a POST, one run of the handler, its own fetch and its write — takes, so a loaded
/// machine does not fail a case the emulator would have served. Every wait LOOKS for the
/// report and leaves the moment it is there.
const REPORT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// How often such a wait looks again.
const REPORT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

/// The next `expected` reports the verifying subscriber wrote, each DELETED as it is read so a
/// later wait sees only later ones.
async fn reported(
    emulator: &crate::lambda_subscription_delivery::Emulator,
    capture_url: &str,
    expected: usize,
) -> Vec<Value> {
    use crate::lambda_subscription_delivery::sqs as delivery_sqs;

    let giving_up_at = std::time::Instant::now() + REPORT_DEADLINE;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        let received = delivery_sqs(
            emulator,
            "ReceiveMessage",
            &json!({"QueueUrl": capture_url, "MaxNumberOfMessages": 10}),
        )
        .await;
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default().to_string();
            delivery_sqs(
                emulator,
                "DeleteMessage",
                &json!({
                    "QueueUrl": capture_url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
            collected.push(serde_json::from_str(&body).unwrap_or_else(|error| {
                panic!("the subscriber reports JSON, got {error} for {body:?}")
            }));
        }
        if collected.len() >= expected || std::time::Instant::now() >= giving_up_at {
            return collected;
        }
        tokio::time::sleep(REPORT_INTERVAL).await;
    }
}

/// RULE, on the subscriber this whole addressing exists for: an `https` endpoint that performs
/// the documented verification before trusting what it was POSTed ADMITS every document this
/// service delivers to it — because the document names ONE authority in its URL members, that
/// authority is this service's own endpoint host for the message's Region, the certificate is
/// served there to the subscriber's own fetch, and the signature verifies against it. The
/// `Token` the admitted confirmation carries then confirms the subscription, and the
/// notification that follows is admitted the same way.
///
/// The endpoint is a real one served by another service of this same emulator — a Lambda
/// function URL — so the verification is performed by the subscriber, over its own HTTP
/// client, on the bytes it really received: a document announcing an address its fetch cannot
/// reach is refused here exactly as a webhook refuses one, and no token reaches the
/// subscription. The rows are the two documents such a subscriber is POSTed: the confirmation
/// request and, once confirmed, the notification of a publish.
#[tokio::test]
async fn a_verifying_https_subscriber_admits_what_it_is_posted() {
    use crate::https_subscription_delivery::given_function_url;
    use crate::lambda_subscription_dead_letter::given_queue as capture_queue;
    use crate::lambda_subscription_delivery::{
        emulator as delivery_emulator, given_function_running, publish, sns as delivery_sns,
        subscribe,
    };
    use crate::sqs_subscription_delivery::element as delivery_element;

    let case = "a verifying https subscriber";
    let emulator = delivery_emulator();
    let (capture_url, _capture_arn) = capture_queue(&emulator, "verified-capture").await;
    let function = "verifying-subscriber";
    given_function_running(
        &emulator,
        function,
        &VERIFYING_SUBSCRIBER.replace("{queue_url}", &capture_url),
    )
    .await;
    let sink = given_function_url(&emulator, function).await;
    let topic = delivery_element(
        &delivery_sns(&emulator, "CreateTopic", &[("Name", "verified-delivery")]).await,
        "TopicArn",
    );
    let subscription = subscribe(&emulator, &topic, "https", &format!("{sink}/take/verified"))
        .await;

    // The confirmation request: admitted, which is what a subscriber verifying the delivery
    // does with a document whose announced certificate it could fetch and whose signature it
    // verified.
    let reports = reported(&emulator, &capture_url, 1).await;
    let report = reports
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: the subscriber reported nothing"));
    assert_eq!(
        (
            report["refused"].as_str(),
            report["document"]["Type"].as_str(),
        ),
        (Some(""), Some("SubscriptionConfirmation")),
        "case {case:?}: the verifying subscriber must admit the confirmation it was POSTed, \
         got {report}"
    );
    let document = report["document"].clone();
    let certificate_url = document["SigningCertURL"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no SigningCertURL in {document}"))
        .to_string();
    let subscribe_url = document["SubscribeURL"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no SubscribeURL in {document}"))
        .to_string();
    // The two URL members of ONE document, read against each other: the authority the
    // subscriber is told to fetch the certificate from is the one the document's own link
    // names, and it is this service's own endpoint host for the message's Region.
    let announced = https_authority(case, "SubscribeURL", &subscribe_url);
    assert_announced_at(case, "SigningCertURL", &announced, &certificate_url);
    assert_published_on_the_sns_endpoint(case, DEFAULT_VISIT_REGION, &certificate_url);

    // Only a subscriber that ADMITTED the POST holds the token, so this is the documented
    // confirmation path reached through the verification above.
    let token = document["Token"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no Token in {document}"))
        .to_string();
    let confirmed = delivery_sns(
        &emulator,
        "ConfirmSubscription",
        &[("TopicArn", &topic), ("Token", &token)],
    )
    .await;
    assert_eq!(
        delivery_element(&confirmed, "SubscriptionArn"),
        subscription,
        "case {case:?}: the verified confirmation's token must confirm {subscription:?}, got \
         {confirmed}"
    );

    // And the notification of a publish is admitted the same way, over its own canonical
    // string and the same announced certificate.
    let message = "a body for a verifying subscriber";
    let message_id = publish(&emulator, &topic, &[("Message", message)]).await;
    let reports = reported(&emulator, &capture_url, 1).await;
    let report = reports
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: the subscriber reported no notification"));
    assert_eq!(
        (
            report["refused"].as_str(),
            report["document"]["Type"].as_str(),
            report["document"]["MessageId"].as_str(),
            report["document"]["Message"].as_str(),
        ),
        (
            Some(""),
            Some("Notification"),
            Some(message_id.as_str()),
            Some(message),
        ),
        "case {case:?}: the verifying subscriber must admit the notification of \
         {message_id:?}, got {report}"
    );
    assert_announced_at(
        case,
        "the notification's SigningCertURL",
        &announced,
        report["document"]["SigningCertURL"].as_str().unwrap_or_default(),
    );
}
