// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a server write that asks for a protocol the requested identity provider
//! cannot serve at all is answered with, driven end-to-end through the same
//! `MultiServiceRouter` the emulator boots.
//!
//! ONE rule, asserted over a SET of configurations rather than over the one request a
//! report cited: **where a member's value makes a whole protocol FAMILY unservable, that
//! gate is answered before any rule about how a protocol of that family would be
//! configured** — so every protocol of the family earns the one refusal the condition they
//! share, whatever certificate, endpoint type or protocol ordering the request carries.
//!
//! `Protocols`' own documentation states both halves at once ("If `Protocol` includes
//! either `FTP` or `FTPS`, then the `EndpointType` must be `VPC` and the
//! `IdentityProviderType` must be either `AWS_DIRECTORY_SERVICE`, `AWS_LAMBDA`, or
//! `API_GATEWAY`") beside the per-protocol ones ("If you select `FTPS`, you must choose a
//! certificate stored in Certificate Manager (ACM)") —
//! `smithy_query(query="doc",shape="com.amazonaws.transfer#CreateServerRequest",member="Protocols")`.
//! An identity provider that cannot serve the family cannot be made to serve it by any
//! certificate or endpoint type, so the family gate is the condition the caller has to act
//! on and the one the refusal has to name.
//!
//! The rows differ along what the rule generalises over: which of the two family protocols
//! the request names, how many protocols the list holds and in which order, whether a
//! member of the family is accompanied by `SFTP` or `AS2`, whether the per-protocol rules
//! would have been satisfied, left unsatisfied or offended (a certificate present, absent
//! and the empty string the member's `len 0..1600` admits; every `EndpointType` value),
//! whether `IdentityProviderType` is stated or left to its documented default, and which
//! operation performs the write (`CreateServer` and `UpdateServer`, which share the one
//! cross-member reading).
//!
//! The complement is asserted too, so the gate is shown to be a gate and not a blanket
//! refusal: an identity provider that CAN serve the family reaches the per-protocol rules,
//! and a request satisfying all of them is served.

use serde_json::{json, Value};
use service_com_amazonaws_transfer_transferservice::state as st;

use crate::harness::{member_of, Answer, Emulator};

/// An ACM certificate ARN for the rows that carry one; `Certificate` is `len 0..1600`.
const CERTIFICATE: &str = "arn:aws:acm:us-east-1:000000000000:certificate/abcd";

/// A `VpcId` for the rows whose `EndpointType` hosts the endpoint in a VPC.
const VPC_ID: &str = "vpc-0123456789abcdef0";

/// A `VpcEndpointId` for the `VPC_ENDPOINT` rows.
const VPC_ENDPOINT_ID: &str = "vpce-0123456789abcdef0";

/// The `Function` an `AWS_LAMBDA` identity provider names.
const LAMBDA_IDP: &str = "arn:aws:lambda:us-east-1:000000000000:function:idp";

/// The `DirectoryId` an `AWS_DIRECTORY_SERVICE` identity provider names.
const DIRECTORY_ID: &str = "d-0123456789";

/// The `Url` an `API_GATEWAY` identity provider names. The row that carries it names no
/// `InvocationRole`, so this service has no invocation to verify for it and the
/// cross-member reading is what answers.
const API_GATEWAY_IDP: &str = "https://abcdef0123.execute-api.us-east-1.amazonaws.com/prod";

/// The whole message the family gate states, written from the rule rather than from the
/// implementation: the service's fixed text for the condition, with the
/// `IdentityProviderType` taken from the request (or, where the request states none, the
/// value the model documents as that member's default — both read from the crate under
/// test rather than copied).
///
/// The one place this crate states that sentence: `refusal_message_rules.rs` reads it
/// from here too, where the family gate is one of the three server-configuration defects
/// whose sentences have to differ.
pub(crate) fn family_gate_message(identity_provider_type: &str) -> String {
    format!(
        "{} and {} protocols unsupported for IdentityProviderType {identity_provider_type}",
        st::PROTOCOL_FTP, st::PROTOCOL_FTPS
    )
}

/// The `Message` of a refusal, as the awsJson envelope carries it.
fn message(answer: &Answer) -> String {
    member_of(&answer.body, "Message")
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Assert `answer` is this service's modelled client refusal carrying exactly `expected`.
fn assert_refused_with(case: &str, answer: &Answer, expected: &str) {
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "InvalidRequestException",
        "case {case:?}: expected InvalidRequestException, got body {}",
        answer.body
    );
    let actual = message(answer);
    assert_eq!(
        actual, expected,
        "case {case:?}: expected {expected:?}, got {actual:?}"
    );
}

/// Every `ServerId` the store holds, as `ListServers` reports them.
async fn listed_server_ids(emulator: &Emulator) -> Vec<String> {
    let listed = emulator.call("ListServers", json!({})).await;
    assert_eq!(
        listed.status, 200,
        "ListServers: expected 200, got {} with body {}",
        listed.status, listed.body
    );
    let mut ids: Vec<String> = listed
        .get("Servers")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|server| member_of(server, "ServerId").as_str().map(str::to_string))
        .collect();
    ids.sort();
    ids
}

/// One row: the case, the `CreateServer` request, and the `IdentityProviderType` the
/// refusal must name — the one the request states, or the documented default where it
/// states none.
type GatedRow<'a> = (&'a str, Value, &'a str);

/// The rows that ask a `SERVICE_MANAGED` server for a member of the FTP/FTPS family, over
/// every per-protocol configuration the request could carry.
fn gated_rows() -> Vec<GatedRow<'static>> {
    let vpc = json!({"VpcId": VPC_ID});
    let vpc_endpoint = json!({"VpcEndpointId": VPC_ENDPOINT_ID});
    let service_managed = st::IDENTITY_PROVIDER_SERVICE_MANAGED;
    let defaulted = st::DEFAULT_IDENTITY_PROVIDER_TYPE;

    vec![
        // ---- FTPS, whose per-protocol rule is the ACM certificate ----
        (
            "FTPS with no certificate on a public endpoint",
            json!({
                "Protocols": ["SFTP", "FTPS"],
                "EndpointType": "PUBLIC",
                "IdentityProviderType": service_managed,
            }),
            service_managed,
        ),
        (
            "FTPS alone with no certificate and no EndpointType at all",
            json!({"Protocols": ["FTPS"], "IdentityProviderType": service_managed}),
            service_managed,
        ),
        (
            "FTPS with the empty certificate the member's length bound admits",
            json!({
                "Protocols": ["FTPS"],
                "Certificate": "",
                "IdentityProviderType": service_managed,
            }),
            service_managed,
        ),
        (
            "FTPS with a certificate, so the per-protocol rule is satisfied",
            json!({
                "Protocols": ["FTPS"],
                "Certificate": CERTIFICATE,
                "IdentityProviderType": service_managed,
            }),
            service_managed,
        ),
        (
            "FTPS with a certificate on the VPC endpoint the family's other half wants",
            json!({
                "Protocols": ["SFTP", "FTPS"],
                "Certificate": CERTIFICATE,
                "EndpointType": "VPC",
                "EndpointDetails": vpc,
                "IdentityProviderType": service_managed,
            }),
            service_managed,
        ),
        (
            "FTPS with no certificate on a VPC_ENDPOINT endpoint",
            json!({
                "Protocols": ["FTPS"],
                "EndpointType": "VPC_ENDPOINT",
                "EndpointDetails": vpc_endpoint,
                "IdentityProviderType": service_managed,
            }),
            service_managed,
        ),
        (
            "FTPS with no IdentityProviderType at all, which takes the documented default",
            json!({"Protocols": ["SFTP", "FTPS"]}),
            defaulted,
        ),
        // ---- FTP, whose per-protocol rule is the VPC-hosted endpoint ----
        (
            "FTP on a public endpoint",
            json!({
                "Protocols": ["SFTP", "FTP"],
                "EndpointType": "PUBLIC",
                "IdentityProviderType": service_managed,
            }),
            service_managed,
        ),
        (
            "FTP on the VPC endpoint the family's other half wants",
            json!({
                "Protocols": ["FTP"],
                "EndpointType": "VPC",
                "EndpointDetails": vpc,
                "IdentityProviderType": service_managed,
            }),
            service_managed,
        ),
        (
            "FTP with no IdentityProviderType at all, which takes the documented default",
            json!({"Protocols": ["FTP"]}),
            defaulted,
        ),
        // ---- both family members at once, and the family beside AS2 ----
        (
            "FTP and FTPS together, in that order, with nothing else configured",
            json!({"Protocols": ["FTP", "FTPS"], "IdentityProviderType": service_managed}),
            service_managed,
        ),
        (
            "FTPS before FTP, so the order the list holds them in cannot decide the refusal",
            json!({"Protocols": ["FTPS", "FTP"], "IdentityProviderType": service_managed}),
            service_managed,
        ),
        (
            "all four protocols with every per-protocol rule satisfied",
            json!({
                "Protocols": ["SFTP", "FTP", "FTPS", "AS2"],
                "Certificate": CERTIFICATE,
                "EndpointType": "VPC",
                "EndpointDetails": vpc,
                "IdentityProviderType": service_managed,
            }),
            service_managed,
        ),
        (
            "FTPS beside AS2 on a public endpoint, where AS2 has its own endpoint rule",
            json!({"Protocols": ["AS2", "FTPS"], "IdentityProviderType": service_managed}),
            service_managed,
        ),
    ]
}

/// RULE: a server write naming FTP or FTPS under an identity provider that cannot serve
/// that family is refused `InvalidRequestException` (HTTP 400) with the family gate's
/// message — the same message for every member of the family and every per-protocol
/// configuration — and persists no server.
#[tokio::test]
async fn the_family_gate_is_answered_before_every_per_protocol_rule() {
    let emulator = Emulator::new();

    let mut stated: Vec<(String, String)> = Vec::new();
    for (case, request, identity_provider_type) in gated_rows() {
        let answer = emulator.call("CreateServer", request).await;
        let expected = family_gate_message(identity_provider_type);
        assert_refused_with(case, &answer, &expected);
        assert!(
            answer.body.get("ServerId").is_none(),
            "case {case:?}: a refused create must answer no ServerId, got {}",
            answer.body
        );
        stated.push((case.to_string(), message(&answer)));
    }

    // One condition, one refusal: every row above shares the same identity provider, so
    // every row states the same sentence whatever it named around it.
    let first = stated
        .first()
        .expect("the table has rows")
        .1
        .clone();
    for (case, said) in &stated {
        assert_eq!(
            *said, first,
            "case {case:?}: expected the one family refusal {first:?}, got {said:?}"
        );
    }

    // Nothing in the table was served.
    let listed = listed_server_ids(&emulator).await;
    assert!(
        listed.is_empty(),
        "case \"the store after the table\": expected no server, got {listed:?}"
    );
}

/// RULE (the same rule, the other side of the gate): an identity provider that CAN serve
/// the family passes the gate, so the per-protocol rules are the ones answered — and a
/// request satisfying all of them is served.
#[tokio::test]
async fn an_identity_provider_that_serves_the_family_reaches_the_per_protocol_rules() {
    let emulator = Emulator::new();

    let vpc = json!({"VpcId": VPC_ID});
    let lambda = json!({"Function": LAMBDA_IDP});
    let directory = json!({"DirectoryId": DIRECTORY_ID});
    let api_gateway = json!({"Url": API_GATEWAY_IDP});

    // (case, request, the refusal's whole message, or None to accept)
    let rows: Vec<(&str, Value, Option<String>)> = vec![
        (
            "AWS_LAMBDA, FTPS with no certificate: the certificate rule answers",
            json!({
                "Protocols": ["SFTP", "FTPS"],
                "EndpointType": "VPC",
                "EndpointDetails": vpc,
                "IdentityProviderType": st::IDENTITY_PROVIDER_AWS_LAMBDA,
                "IdentityProviderDetails": lambda,
            }),
            Some("Certificate must be specified when Protocols includes FTPS".to_string()),
        ),
        (
            "API_GATEWAY, FTPS with the empty certificate: the certificate rule answers",
            json!({
                "Protocols": ["FTPS"],
                "Certificate": "",
                "EndpointType": "VPC",
                "EndpointDetails": vpc,
                "IdentityProviderType": st::IDENTITY_PROVIDER_API_GATEWAY,
                "IdentityProviderDetails": api_gateway,
            }),
            Some("Certificate must be specified when Protocols includes FTPS".to_string()),
        ),
        (
            "AWS_DIRECTORY_SERVICE, FTP on a public endpoint: the endpoint rule answers",
            json!({
                "Protocols": ["FTP"],
                "EndpointType": "PUBLIC",
                "IdentityProviderType": st::IDENTITY_PROVIDER_AWS_DIRECTORY_SERVICE,
                "IdentityProviderDetails": directory,
            }),
            Some("EndpointType must be VPC when Protocols includes FTP".to_string()),
        ),
        (
            "AWS_LAMBDA, FTPS with a certificate on a public endpoint: the endpoint rule",
            json!({
                "Protocols": ["FTPS"],
                "Certificate": CERTIFICATE,
                "EndpointType": "PUBLIC",
                "IdentityProviderType": st::IDENTITY_PROVIDER_AWS_LAMBDA,
                "IdentityProviderDetails": lambda,
            }),
            Some("EndpointType must be VPC when Protocols includes FTPS".to_string()),
        ),
        (
            "AWS_LAMBDA, FTP and FTPS with every per-protocol rule satisfied",
            json!({
                "Protocols": ["SFTP", "FTP", "FTPS"],
                "Certificate": CERTIFICATE,
                "EndpointType": "VPC",
                "EndpointDetails": vpc,
                "IdentityProviderType": st::IDENTITY_PROVIDER_AWS_LAMBDA,
                "IdentityProviderDetails": lambda,
            }),
            None,
        ),
        (
            "SERVICE_MANAGED with SFTP alone: the family rule says nothing about it",
            json!({"Protocols": [st::PROTOCOL_SFTP]}),
            None,
        ),
    ];

    let mut served: Vec<String> = Vec::new();
    for (case, request, refusal) in rows {
        let answer = emulator.call("CreateServer", request).await;
        match refusal {
            Some(expected) => {
                assert_refused_with(case, &answer, &expected);
                assert!(
                    answer.body.get("ServerId").is_none(),
                    "case {case:?}: a refused create must answer no ServerId, got {}",
                    answer.body
                );
            }
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected HTTP 200, got {} with body {}",
                    answer.status, answer.body
                );
                let server_id = answer
                    .get("ServerId")
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: a served create answers a ServerId: {}", answer.body)
                    })
                    .to_string();
                // The create is backed by state: the reader reports the protocols it
                // persisted.
                let described = emulator
                    .call("DescribeServer", json!({"ServerId": &server_id}))
                    .await;
                assert_eq!(
                    described.status, 200,
                    "case {case:?}: expected DescribeServer 200, got {} with body {}",
                    described.status, described.body
                );
                let server = member_of(&described.body, "Server").clone();
                let protocols = member_of(&server, "Protocols").clone();
                assert!(
                    protocols
                        .as_array()
                        .is_some_and(|named| named.contains(&json!(st::PROTOCOL_SFTP))),
                    "case {case:?}: expected the stored server to serve SFTP, got {protocols}"
                );
                served.push(server_id);
            }
        }
    }

    served.sort();
    let listed = listed_server_ids(&emulator).await;
    assert_eq!(
        listed, served,
        "case \"the store after the table\": expected only the served servers {served:?}, \
         got {listed:?}"
    );
}

/// RULE (the same rule, the other write path): the gate belongs to the server
/// CONFIGURATION, not to one operation, so an update that would put a `SERVICE_MANAGED`
/// server on the family is refused the same way and leaves the stored server untouched.
#[tokio::test]
async fn an_update_onto_the_family_is_gated_the_same_way() {
    let emulator = Emulator::new();
    let server_id = emulator
        .create_server(json!({"Protocols": [st::PROTOCOL_SFTP]}))
        .await;

    // (case, the UpdateServer members beside ServerId)
    let rows: Vec<(&str, Value)> = vec![
        (
            "UpdateServer adding FTPS with no certificate",
            json!({"Protocols": ["SFTP", "FTPS"]}),
        ),
        (
            "UpdateServer adding FTPS with a certificate",
            json!({"Protocols": ["SFTP", "FTPS"], "Certificate": CERTIFICATE}),
        ),
        (
            "UpdateServer adding FTP on the VPC endpoint the family's other half wants",
            json!({
                "Protocols": ["SFTP", "FTP"],
                "EndpointType": "VPC",
                "EndpointDetails": {"VpcId": VPC_ID},
            }),
        ),
        (
            "UpdateServer adding both family members at once",
            json!({"Protocols": ["FTP", "FTPS"], "Certificate": CERTIFICATE}),
        ),
    ];

    let expected = family_gate_message(st::DEFAULT_IDENTITY_PROVIDER_TYPE);
    for (case, members) in rows {
        let mut request = members;
        request["ServerId"] = json!(&server_id);
        let answer = emulator.call("UpdateServer", request).await;
        assert_refused_with(case, &answer, &expected);

        // The refused update wrote nothing: the server still serves what it was created
        // with.
        let described = emulator
            .call("DescribeServer", json!({"ServerId": &server_id}))
            .await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: expected DescribeServer 200, got {} with body {}",
            described.status, described.body
        );
        let server = member_of(&described.body, "Server").clone();
        assert_eq!(
            member_of(&server, "Protocols"),
            &json!([st::PROTOCOL_SFTP]),
            "case {case:?}: a refused update must leave the stored protocols alone, got \
             {server}"
        );
        assert!(
            member_of(&server, "Certificate").is_null(),
            "case {case:?}: a refused update must store no certificate, got {server}"
        );
    }
}
