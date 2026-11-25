// SPDX-License-Identifier: CC0-1.0

//! What an `https` SUBSCRIBER THAT VERIFIES what it is POSTed admits, on the emulator a client
//! STARTS: the built binary, on its own port, with request authentication on, reached over TLS
//! with SigV4-signed calls.
//!
//! The endpoint is a Lambda function URL this same emulator published, whose handler does what
//! the Amazon SNS Developer Guide tells a subscriber to do before trusting a delivery — it
//! "verif[ies] that the signing certificate URL is from an Amazon SNS address", then
//! "retrieve[s] the certificate … and extract[s] the public key from it" and checks the
//! signature ("Verifying the signatures of Amazon SNS messages") — and answers 200 only for a
//! document that passed, 403 for every other, which is what a webhook that authenticates its
//! deliveries does. What it admitted, and the reason it refused anything, is read back out of
//! an Amazon SQS queue, from outside the endpoint.
//!
//! RULE: one document SNS delivers names ONE authority in every URL member it carries — its
//! `SigningCertURL` beside the `SubscribeURL`/`UnsubscribeURL` the token travels in — that
//! authority is this service's own endpoint host for the message's Region, and the certificate
//! is SERVED there to the subscriber's own fetch, so the signature verifies and the delivery
//! is admitted. A document announcing an address the subscriber's fetch cannot reach is
//! refused whatever the signature over it says, and the `Token` a confirmation carries then
//! never reaches the endpoint — which is the only path a subscription to a URL has to
//! `ConfirmSubscription`.
//!
//! The owning crate's `signing_certificate_address` suite spans the authorities a publishing
//! client can reach and the kinds of document the members travel in, on a router it assembles
//! with authentication off. The emulator a client runs is a different configuration of the
//! same code: the credentials are verified, each call arrives over its own TLS connection, and
//! the subscriber's own fetch of the certificate is made from inside a running handler — so a
//! verification that succeeds in-process and not on the started emulator is a failure this
//! suite reports rather than one only a client sees. The harness that starts the binary is the
//! shared one ([`crate::cfn_deployed_pipeline_on_a_started_emulator::Started`]).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::cfn_deployed_pipeline_on_a_started_emulator::{deployed, output, Started};
use crate::query_wire::opt_element;

/// The stack the execution role and the verifying function are deployed by.
const STACK: &str = "started-emulator-verifying-subscriber";

/// How long work the clock owes is waited for (the stack settling, the function reaching
/// `Active`, a POST reaching the endpoint and its report reaching the queue), and how often
/// this suite looks meanwhile. Every wait is on a CONDITION and leaves as soon as it holds.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How long the client waits on one `ReceiveMessage`, in seconds — "the duration (in seconds)
/// for which the call waits for a message to arrive in the queue before returning"
/// (`ReceiveMessageRequest$WaitTimeSeconds`).
const WAIT_TIME_SECONDS: u64 = 2;

/// The handler behind the function URL: an `https` subscriber that ADMITS only a document it
/// could verify the documented way, and reports every request it was sent on the queue its
/// environment names.
///
/// The verification is written out of the guest's own standard library, because that is what a
/// handler has: the digests are `hashlib`'s and the arithmetic is Python's, and what is written
/// here is the DER walk to the certificate's public key and the EMSA-PKCS1-v1_5 padding layout
/// RFC 8017 section 9.2 fixes.
const VERIFYING_SUBSCRIBER: &str = r#"import base64
import hashlib
import json
import os
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

# The DigestInfo prefix of the digest each SignatureVersion selects.
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


def handler(event, context):
    body = event.get("body") or ""
    if event.get("isBase64Encoded"):
        body = base64.b64decode(body).decode("utf-8")
    try:
        document = json.loads(body)
    except ValueError:
        document = {}
    refused = refusal(document) if document else "the request carries no document"
    boto3.client("sqs").send_message(
        QueueUrl=os.environ["QUEUE_URL"],
        MessageBody=json.dumps({"refused": refused, "document": document}),
    )
    if refused:
        return {"statusCode": 403, "body": "refused"}
    return {"statusCode": 200, "body": "admitted"}
"#;

/// `text` as JSON, or null for a body that is not JSON at all.
fn parsed(text: &str) -> Value {
    if text.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// One SNS call that must be answered, as the XML text it answered.
fn sns(emulator: &Started, action: &str, members: &[(&str, &str)]) -> String {
    let (status, text) = emulator.form_call("sns", "2010-03-31", action, members);
    assert_eq!(
        status, 200,
        "SNS {action} must answer 200, got {status} with {text}; the log:\n{}",
        emulator.log()
    );
    text
}

/// One Amazon SQS call, which must answer 200.
fn sqs(emulator: &Started, operation: &str, body: &Value) -> Value {
    let (status, text) = emulator.rpc(
        "sqs",
        &format!("AmazonSQS.{operation}"),
        "application/x-amz-json-1.0",
        body,
    );
    let answered = parsed(&text);
    assert_eq!(
        status, 200,
        "Amazon SQS {operation} must answer 200, got {status} with {answered}; the log:\n{}",
        emulator.log()
    );
    answered
}

/// One Lambda `restJson1` call, as its status and parsed body.
fn lambda(emulator: &Started, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let (status, text) = emulator.call(
        "lambda",
        method,
        path,
        &[("content-type", "application/json")],
        payload.into_bytes(),
    );
    (status, parsed(&text))
}

/// The stack this suite deploys: the execution role the handler's own write is judged against,
/// and the verifying function running as it.
fn wiring(queue_url: &str) -> Value {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "VerifierRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    // The ONE action the handler itself takes: its report of what it
                    // admitted. Fetching the certificate carries no credential at all.
                    "Policies": [{
                        "PolicyName": "report-what-it-admitted",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["sqs:SendMessage"],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Verifier": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["VerifierRole", "Arn"] },
                    "Timeout": 60,
                    "Environment": { "Variables": { "QUEUE_URL": queue_url } },
                    "Code": { "ZipFile": VERIFYING_SUBSCRIBER },
                },
            },
        },
        "Outputs": { "FunctionName": { "Value": { "Ref": "Verifier" } } },
    })
}

/// The next `expected` reports the subscriber wrote, each DELETED as it is read so a later
/// wait sees only later ones. Answers what it collected, which is how a case that saw nothing
/// fails with that.
fn reported(emulator: &Started, capture_url: &str, expected: usize) -> Vec<Value> {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut collected: Vec<Value> = Vec::new();
    loop {
        let received = sqs(
            emulator,
            "ReceiveMessage",
            &json!({
                "QueueUrl": capture_url,
                "MaxNumberOfMessages": 10,
                "WaitTimeSeconds": WAIT_TIME_SECONDS,
            }),
        );
        for message in received["Messages"].as_array().cloned().unwrap_or_default() {
            let body = message["Body"].as_str().unwrap_or_default().to_string();
            sqs(
                emulator,
                "DeleteMessage",
                &json!({
                    "QueueUrl": capture_url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            );
            collected.push(parsed(&body));
        }
        if collected.len() >= expected || Instant::now() >= giving_up_at {
            return collected;
        }
        std::thread::sleep(LOOK_INTERVAL);
    }
}

/// Stand up the endpoint and answer the capture queue's url beside the function URL the
/// subscription addresses.
fn given_verifier(emulator: &Started, case: &str) -> (String, String) {
    let created = sqs(
        emulator,
        "CreateQueue",
        &json!({"QueueName": format!("{STACK}-capture")}),
    );
    let capture_url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("the capture queue reports no url, got {created}"))
        .to_string();

    let outputs = deployed(emulator, case, STACK, &wiring(&capture_url).to_string());
    let function = output(&outputs, case, "FunctionName");

    // An invocation needs the function Active, which is a span the service owes.
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (_, configuration) = lambda(
            emulator,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        );
        if configuration["State"].as_str() == Some("Active") {
            break;
        }
        assert!(
            Instant::now() < giving_up_at,
            "the function {function} never reached Active, last read {configuration}; the \
             log:\n{}",
            emulator.log()
        );
        std::thread::sleep(LOOK_INTERVAL);
    }

    let (status, url) = lambda(
        emulator,
        "POST",
        &format!("/2021-10-31/functions/{function}/url"),
        Some(&json!({"AuthType": "NONE"})),
    );
    assert_eq!(
        status, 201,
        "CreateFunctionUrlConfig must answer 201, got {status} with {url}; the log:\n{}",
        emulator.log()
    );
    // `AuthType=NONE` leaves IAM out of it; the function's own resource policy is what admits
    // the anonymous caller an SNS delivery is.
    let (status, granted) = lambda(
        emulator,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(&json!({
            "StatementId": "public-url",
            "Action": "lambda:InvokeFunctionUrl",
            "Principal": "*",
            "FunctionUrlAuthType": "NONE",
        })),
    );
    assert_eq!(
        status, 201,
        "AddPermission must answer 201, got {status} with {granted}; the log:\n{}",
        emulator.log()
    );
    let endpoint = url["FunctionUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("no FunctionUrl in {url}"))
        .trim_end_matches('/')
        .to_string();
    (capture_url, endpoint)
}

/// RULE: on the emulator a client starts, an `https` subscriber that performs the documented
/// verification ADMITS the confirmation it is POSTed — so its `Token` confirms the
/// subscription — and admits every notification that follows.
#[test]
fn a_verifying_https_subscriber_admits_what_the_started_emulator_posts_it() {
    let case = "a verifying https subscriber on a started emulator";
    let emulator = Started::start();
    let (capture_url, endpoint) = given_verifier(&emulator, case);

    let topic = opt_element(
        &sns(&emulator, "CreateTopic", &[("Name", "verified-delivery")]),
        "TopicArn",
    )
    .unwrap_or_default();
    assert!(!topic.is_empty(), "CreateTopic must answer the topic's ARN");

    let subscribed = sns(
        &emulator,
        "Subscribe",
        &[
            ("TopicArn", &topic),
            ("Protocol", "https"),
            ("Endpoint", &format!("{endpoint}/take/verified")),
            ("ReturnSubscriptionArn", "true"),
        ],
    );
    let subscription = opt_element(&subscribed, "SubscriptionArn").unwrap_or_default();

    // The confirmation request, as the subscriber judged it: admitted, which it is only if the
    // certificate the document announced was fetchable by the subscriber itself and verified
    // the signature over the members it received.
    let reports = reported(&emulator, &capture_url, 1);
    let report = reports.first().cloned().unwrap_or(Value::Null);
    assert_eq!(
        (
            report["refused"].as_str(),
            report["document"]["Type"].as_str(),
        ),
        (Some(""), Some("SubscriptionConfirmation")),
        "case {case:?}: the verifying subscriber must admit the confirmation it was POSTed, \
         got {report}; the log:\n{}",
        emulator.log()
    );
    let token = report["document"]["Token"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !token.is_empty(),
        "case {case:?}: the admitted confirmation must carry a Token, got {report}"
    );
    let confirmed = sns(
        &emulator,
        "ConfirmSubscription",
        &[("TopicArn", &topic), ("Token", &token)],
    );
    assert_eq!(
        opt_element(&confirmed, "SubscriptionArn").as_deref(),
        Some(subscription.as_str()),
        "case {case:?}: the verified confirmation's token must confirm the subscription it \
         names, got {confirmed}"
    );

    // And the notification of a publish is admitted the same way, over its own canonical
    // string and the same announced certificate.
    let message = "the verified body";
    let published = sns(
        &emulator,
        "Publish",
        &[("TopicArn", &topic), ("Message", message)],
    );
    let message_id = opt_element(&published, "MessageId").unwrap_or_default();
    let reports = reported(&emulator, &capture_url, 1);
    let report = reports.first().cloned().unwrap_or(Value::Null);
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
         {message_id:?}, got {report}; the log:\n{}",
        emulator.log()
    );
}
