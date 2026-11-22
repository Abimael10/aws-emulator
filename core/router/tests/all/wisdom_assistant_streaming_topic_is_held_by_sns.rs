// SPDX-License-Identifier: CC0-1.0

//! The topic an Amazon Q in Connect assistant reports as its chat-message stream, on the
//! ASSEMBLED emulator with request authentication ON — the configuration the emulator serves
//! traffic in.
//!
//! RULE: an ARN a response reports for a resource another service OWNS names a resource that
//! service really holds. `AssistantData.integrationConfiguration.topicIntegrationArn` is "the
//! Amazon Resource Name (ARN) of the integrated Amazon SNS topic used for streaming chat
//! messages" (`smithy_query(query="shape",shape="AssistantIntegrationConfiguration")`), so
//! the topic it names is Amazon SNS's: `CreateAssistant` creates it THERE, over this
//! emulator's own in-process hop, and reports the ARN that service answered — and
//! `DeleteAssistant` releases it there. A client that takes the value to Amazon SNS therefore
//! reaches the stream the member advertises.
//!
//! This suite is the cross-service half: the hop is signed with the emulator's own
//! credentials and verified exactly as a client's own request is, so it pins that the topic
//! really is created on a router that VERIFIES signatures, and it asserts the topic where it
//! lives — in Amazon SNS, over that service's own `awsQuery` wire. The single-service half
//! (`services/service-com-amazonaws-qconnect-wisdomservice/tests/all/assistant_chat_streaming_topic.rs`)
//! pins the per-assistant, per-Region table and the reads that report the same ARN.
//!
//! The rows differ along what the rule generalises over: the create's input (name form and
//! length, description, tags, encryption configuration) and the Region signed for, none of
//! which decides whether the reported topic exists.

use std::collections::BTreeSet;
use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region a row signs for unless it exercises the Region axis itself.
const REGION: &str = "us-west-2";

/// The signing name BOTH views of the Amazon Q in Connect endpoint resolve to, and the host
/// a client addresses it on.
const WISDOM_SIGNING_NAME: &str = "wisdom";

/// Amazon SNS's signing name and the `awsQuery` API version its requests carry.
const SNS_SIGNING_NAME: &str = "sns";
const SNS_API_VERSION: &str = "2010-03-31";

/// The content type of an `awsQuery` request body.
const FORM: &str = "application/x-www-form-urlencoded";

/// The attribute `GetTopicAttributes` reports a topic's own ARN under.
const TOPIC_ARN_ATTRIBUTE: &str = "TopicArn";

/// The emulator as it is ASSEMBLED, on one engine, with request authentication ON: the
/// control plane's in-process call into Amazon SNS is dispatched back into this same router
/// and verified there.
fn emulator() -> MultiServiceRouter {
    let engine = ::aws_emulator_router::new_engine();
    let mut router = ::aws_emulator_router::build_router(engine);
    router.set_request_authentication(true);
    router
}

/// The credential every call below signs with: the pair this process publishes once request
/// authentication is on.
fn published() -> &'static (String, String) {
    static PUBLISHED: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    PUBLISHED.get_or_init(|| {
        let minted = ::emulator_server::SigV4Authenticator::minted_credentials()
            .expect("turning request authentication on publishes this process's keys");
        (
            minted.access_key_id.clone(),
            minted.secret_access_key.clone(),
        )
    })
}

/// `request`, signed for `signing_name` in `region` the way an SDK signs one.
fn signed(
    mut request: http::Request<Vec<u8>>,
    signing_name: &str,
    region: &str,
) -> http::Request<Bytes> {
    let (key_id, secret) = published();
    let identity = Credentials::new(key_id, secret, None, None, "test").into();
    let params = SigningParams::builder()
        .identity(&identity)
        .region(region)
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
    let (parts, payload) = request.into_parts();
    http::Request::from_parts(parts, Bytes::from(payload))
}

/// One signed request against the Amazon Q in Connect endpoint's `restJson1` surface: the
/// status and the JSON body.
async fn qconnect(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    region: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let host = format!("wisdom.{region}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    let payload = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            serde_json::to_vec(&value).expect("the body serializes")
        }
        None => Vec::new(),
    };
    let request = builder.body(payload).expect("a well-formed request");
    let response = router
        .dispatch(signed(request, WISDOM_SIGNING_NAME, region))
        .await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One signed Amazon SNS call over its `awsQuery` FORM wire: the status and the XML text.
async fn sns(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={SNS_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("sns.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", FORM)
        .body(body.into_bytes())
        .expect("a well-formed request");
    let response = router
        .dispatch(signed(request, SNS_SIGNING_NAME, region))
        .await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, text)
}

/// The `TopicArn` attribute Amazon SNS answers for `topic_arn`, or `None` when it holds no
/// topic under it. This is what makes an assertion about a reported ARN an assertion about a
/// topic that EXISTS.
async fn resolved_topic(
    router: &MultiServiceRouter,
    region: &str,
    topic_arn: &str,
) -> Option<String> {
    let (status, body) = sns(
        router,
        region,
        "GetTopicAttributes",
        &[("TopicArn", topic_arn)],
    )
    .await;
    if status != 200 {
        return None;
    }
    crate::query_wire::decoded_entries(&body)
        .get(TOPIC_ARN_ATTRIBUTE)
        .cloned()
}

/// Every topic ARN Amazon SNS holds in `region`.
async fn held_topics(router: &MultiServiceRouter, region: &str) -> BTreeSet<String> {
    let (status, body) = sns(router, region, "ListTopics", &[]).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from Amazon SNS ListTopics, got {status} with {body}"
    );
    crate::query_wire::elements(&body, "TopicArn")
        .into_iter()
        .collect()
}

/// Create one assistant and answer its `assistantId` and the topic ARN it reports.
async fn created_assistant(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    body: Value,
) -> (String, String) {
    let (status, answer) = qconnect(router, "POST", "/assistants", region, Some(body)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateAssistant, got {status} with {answer}"
    );
    let assistant_id = answer
        .pointer("/assistant/assistantId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no assistantId in {answer}"))
        .to_string();
    let topic = answer
        .pointer("/assistant/integrationConfiguration/topicIntegrationArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no topicIntegrationArn in {answer}"))
        .to_string();
    (assistant_id, topic)
}

/// One row of the create table.
struct Row {
    label: &'static str,
    region: &'static str,
    body: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "minimal create",
            region: REGION,
            body: json!({ "name": "signed-minimal", "type": "AGENT" }),
        },
        Row {
            label: "described, three tags",
            region: REGION,
            body: json!({
                "name": "signed.described_1",
                "type": "AGENT",
                "description": "An assistant, with punctuation. -",
                "tags": { "team": "alpha", "a.b": "beta", "cost-centre": "42" },
            }),
        },
        Row {
            label: "longest legal name, customer managed key, another Region",
            region: "eu-west-1",
            body: json!({
                "name": "n".repeat(255),
                "type": "AGENT",
                "serverSideEncryptionConfiguration": { "kmsKeyId": "alias/aws/wisdom" },
            }),
        },
    ]
}

/// For every row, the topic the create reports is a topic Amazon SNS holds in the Region the
/// assistant was created in — resolvable through that service's own API and listed by it.
#[tokio::test]
async fn a_created_assistant_reports_a_topic_amazon_sns_holds() {
    for row in rows() {
        // One emulator per row, so Amazon SNS starts out holding no topic at all and the
        // listing below names exactly this row's.
        let router = emulator();
        let case = row.label;
        let (assistant_id, topic) = created_assistant(&router, case, row.region, row.body).await;

        let resolved = resolved_topic(&router, row.region, &topic).await;
        assert_eq!(
            resolved.as_ref(),
            Some(&topic),
            "case {case:?}: expected Amazon SNS to answer GetTopicAttributes for the reported \
             topic {topic:?}, got {resolved:?}"
        );
        let held = held_topics(&router, row.region).await;
        assert_eq!(
            held,
            BTreeSet::from([topic.clone()]),
            "case {case:?}: expected Amazon SNS to hold exactly the assistant's topic \
             {topic:?}, got {held:?}"
        );
        // The topic is the assistant's own, in the Region the assistant lives in.
        assert!(
            topic.starts_with(&format!("arn:aws:sns:{}:", row.region)),
            "case {case:?}: expected an Amazon SNS topic ARN of {}, got {topic:?}",
            row.region
        );
        // And a read of the assistant reports the same topic the create did.
        let (status, answer) = qconnect(
            &router,
            "GET",
            &format!("/assistants/{assistant_id}"),
            row.region,
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetAssistant, got {status} with {answer}"
        );
        let read = answer
            .pointer("/assistant/integrationConfiguration/topicIntegrationArn")
            .and_then(Value::as_str);
        assert_eq!(
            read,
            Some(topic.as_str()),
            "case {case:?}: expected GetAssistant to report the same topic {topic:?}, \
             got {read:?}"
        );
    }
}

/// Deleting the assistant releases ITS topic in Amazon SNS and leaves every other topic the
/// account holds — another assistant's, and one no assistant owns — in place.
#[tokio::test]
async fn deleting_an_assistant_releases_only_its_own_topic_in_amazon_sns() {
    let router = emulator();
    // A topic of the account that belongs to no assistant, created through Amazon SNS's own
    // API: the delete below must leave it alone.
    let (status, created) = sns(
        &router,
        REGION,
        "CreateTopic",
        &[("Name", "topic-no-assistant-owns")],
    )
    .await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from Amazon SNS CreateTopic, got {status} with {created}"
    );
    let unrelated = element(&created, "TopicArn");

    let (doomed_id, doomed_topic) = created_assistant(
        &router,
        "doomed assistant",
        REGION,
        json!({ "name": "signed-doomed", "type": "AGENT" }),
    )
    .await;
    let (_, surviving_topic) = created_assistant(
        &router,
        "surviving assistant",
        REGION,
        json!({ "name": "signed-surviving", "type": "AGENT" }),
    )
    .await;

    let (status, answer) = qconnect(
        &router,
        "DELETE",
        &format!("/assistants/{doomed_id}"),
        REGION,
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "expected HTTP 204 from DeleteAssistant, got {status} with {answer}"
    );

    let resolved = resolved_topic(&router, REGION, &doomed_topic).await;
    assert_eq!(
        resolved, None,
        "expected Amazon SNS to hold no topic under {doomed_topic:?} once its assistant was \
         deleted, got {resolved:?}"
    );
    let held = held_topics(&router, REGION).await;
    assert_eq!(
        held,
        BTreeSet::from([surviving_topic.clone(), unrelated.clone()]),
        "expected the surviving assistant's topic {surviving_topic:?} and the unrelated topic \
         {unrelated:?} to be all Amazon SNS still holds, got {held:?}"
    );
}
