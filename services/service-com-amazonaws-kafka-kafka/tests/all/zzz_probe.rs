// SPDX-License-Identifier: CC0-1.0

//! A long-lived connection to one broker, driven the way a client drives it: many
//! requests, one after another, over the SASL/IAM path.
//!
//! RULE under test: a connection answers each request frame with the answer to THAT
//! frame, in the order the frames arrived, for as long as the client keeps sending — so a
//! producer that appends on one connection reads back consecutive offsets, the fetch that
//! follows reads every record it appended, and the group calls after them are answered on
//! the same connection. Nothing of one answer is left behind for the next request to read.
//!
//! The rows span what the rule generalises over: how many requests the connection has
//! already served (the first and the fiftieth are answered alike) and which API answers
//! (the negotiation, the SASL exchange, topic administration, appends, a fetch, and the
//! group coordinator), since the rule is about the SEQUENCE rather than about any one
//! answer.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at
//! the SASL/IAM address `GetBootstrapBrokers` published.

use ::bytes::Bytes;
use ::kafka_protocol::messages::{
    ApiKey, FindCoordinatorRequest, FindCoordinatorResponse, SaslAuthenticateRequest,
    SaslAuthenticateResponse, SaslHandshakeRequest, SaslHandshakeResponse,
};
use ::kafka_protocol::protocol::StrBytes;
use serde_json::json;

use crate::broker_data_plane::{Client, AWS_MSK_IAM, CLIENT_ID, LOOPBACK};
use crate::kafka_harness::{ClusterRequest, Emulator, REGION};

/// How many appends this suite sends one after another on the one connection: enough that
/// the answers would have drifted out of step long before the last of them.
const APPENDS: i64 = 50;

/// The topic the appends go to, and the group the coordinator is asked about.
const TOPIC: &str = "in-step";
const GROUP: &str = "in-step-readers";

/// RULE: every request of a long-lived connection is answered with its own answer, in
/// order.
#[tokio::test]
async fn one_connection_answers_every_request_in_step() {
    let case = "fifty appends and a fetch on one connection";
    let emulator = Emulator::with_iam();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("TLS"),
            sasl_iam: Some(true),
            ..ClusterRequest::named("in-step")
        })
        .await;
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerStringSaslIam", case)
        .await;
    let mut client = iam_client(case, &authority).await;

    client.create_topics(case, &[(TOPIC, 1, 1, &[])], false).await;
    let reported = client.metadata(case).await;
    assert!(
        reported.topics.iter().any(|topic| {
            topic
                .name
                .as_ref()
                .is_some_and(|name| name.0.as_str() == TOPIC)
                && topic.error_code == 0
        }),
        "case {case:?}: the topic the connection just created must be reported, got {:?}",
        reported
            .topics
            .iter()
            .map(|topic| (
                topic.name.as_ref().map(|name| name.0.to_string()),
                topic.error_code
            ))
            .collect::<Vec<_>>()
    );

    // Each append is one request on the same connection, and the offset it is
    // acknowledged with is the one the record took: an answer read out of step would
    // report another append's offset.
    for expected in 0..APPENDS {
        let value = format!("record-{expected}");
        let offset = client
            .produce(case, TOPIC, 0, &[(1_700_000_000_000 + expected, None, &value)])
            .await;
        assert_eq!(
            offset, expected,
            "case {case:?}: append {expected} must be acknowledged at offset {expected}, got \
             {offset}"
        );
    }

    // The fetch that follows those fifty answers reads every record they appended, in
    // the order they were appended.
    let read = client.fetch(case, TOPIC, 0, 0).await;
    let values: Vec<String> = read.iter().map(|(_, _, value)| value.clone()).collect();
    let appended: Vec<String> = (0..APPENDS).map(|index| format!("record-{index}")).collect();
    assert_eq!(
        values, appended,
        "case {case:?}: the fetch must read back every record that was appended, in order"
    );

    // And the connection is still in step for an API of another kind afterwards.
    let coordinator: FindCoordinatorResponse = client
        .call(
            case,
            ApiKey::FindCoordinator,
            3,
            &FindCoordinatorRequest::default().with_key(StrBytes::from_static_str(GROUP)),
        )
        .await;
    assert_eq!(
        coordinator.error_code, 0,
        "case {case:?}: the group's coordinator must be reported, got error {}",
        coordinator.error_code
    );
    assert!(
        coordinator.host.as_str() == LOOPBACK && coordinator.port > 0,
        "case {case:?}: the coordinator must be named at an address of this emulator, got \
         {}:{}",
        coordinator.host.as_str(),
        coordinator.port
    );
}

/// A client on the SASL/IAM path, authenticated with the credential this account's
/// default access key speaks for: what this suite is about is the SEQUENCE of answers, so
/// the connection is authenticated with a credential no policy narrows.
async fn iam_client(case: &str, authority: &str) -> Client {
    let mut client = Client::tls(case, authority).await;
    client.api_versions(case).await;
    let handshake: SaslHandshakeResponse = client
        .call(
            case,
            ApiKey::SaslHandshake,
            1,
            &SaslHandshakeRequest::default().with_mechanism(StrBytes::from_static_str(AWS_MSK_IAM)),
        )
        .await;
    assert_eq!(
        handshake.error_code, 0,
        "case {case:?}: the cluster must offer the IAM mechanism, got error {}",
        handshake.error_code
    );
    let payload = json!({
        "version": "2020_10_22",
        "host": LOOPBACK,
        "user-agent": CLIENT_ID,
        "action": "kafka-cluster:Connect",
        "x-amz-algorithm": "AWS4-HMAC-SHA256",
        "x-amz-credential": format!(
            "{}/20261101/{REGION}/kafka-cluster/aws4_request",
            crate::kafka_harness::DEFAULT_ACCESS_KEY_ID
        ),
        "x-amz-date": "20261101T000000Z",
        "x-amz-signedheaders": "host",
        "x-amz-signature": "a-signature",
    });
    let authenticated: SaslAuthenticateResponse = client
        .call(
            case,
            ApiKey::SaslAuthenticate,
            2,
            &SaslAuthenticateRequest::default()
                .with_auth_bytes(Bytes::from(payload.to_string().into_bytes())),
        )
        .await;
    assert_eq!(
        authenticated.error_code, 0,
        "case {case:?}: the IAM exchange must authenticate the connection, got error {}",
        authenticated.error_code
    );
    client
}
