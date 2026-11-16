// SPDX-License-Identifier: CC0-1.0

//! What `CreateDomainName` requires of the CERTIFICATE a custom domain name would present,
//! asserted on the wire.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees. [`Gateway`], [`Answer`] and [`assert_failure`] are this crate's one wire-test
//! harness, reused rather than standing a second emulator up another way.
//!
//! The RULE under test: **a custom domain name terminates TLS for the requests it serves, so
//! a create has to name the certificate the endpoint type it resolved to would PRESENT** —
//! `regionalCertificateArn` for a `REGIONAL` endpoint, and `certificateArn` or the uploaded
//! `certificateBody` + `certificatePrivateKey` pair for an edge-optimized or private one.
//! A create that names none is refused 400 `BadRequestException` with "A certificate was not
//! provided for the endpoint type `<TYPE>`." — the type the request resolved to, which is
//! `EDGE` for a request that configures no endpoint at all — and stores nothing, so the name
//! it was sent for still resolves to nothing afterwards.
//!
//! That refusal is a COMPLETENESS check and stays distinguishable from the FORM check a
//! value that is no certificate ARN earns (the service's fixed unreadable-ARN sentence),
//! which the last case pins.
//!
//! The rows span what the rule generalises over: each endpoint type, each member that names
//! a certificate (including the two that name none — the user-friendly name and the ownership
//! certificate), the member of the OTHER endpoint type, the uploaded material whole and half,
//! and a request that configures no endpoint type at all.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    certificate_not_provided, INVALID_ARN, NO_SUCH_DOMAIN_NAME,
};

use crate::rest_api_lifecycle::{assert_failure, page_items, string_member, Answer, Gateway};

const BAD_REQUEST: &str = "BadRequestException";
const NOT_FOUND: &str = "NotFoundException";

/// One certificate of this account, in the form a certificate member has to name
/// (`arn:<partition>:acm:<region>:<account>:certificate/<id>`). Nothing here holds
/// certificates — Certificate Manager is not mounted on this suite's engine, so no
/// certificate's STATE is established and the state gate the
/// `domain_name_certificate_state` suite owns refuses nothing.
const CERTIFICATE_ARN: &str =
    "arn:aws:acm:us-west-2:000000000000:certificate/11111111-2222-3333-4444-555555555555";

/// A second certificate of the account, so a case can name one for each endpoint.
const SECOND_CERTIFICATE_ARN: &str =
    "arn:aws:acm:us-west-2:000000000000:certificate/66666666-7777-8888-9999-000000000000";

/// The leaf and the key a request uploads instead of referencing a certificate, and the
/// intermediates between them and the root.
const CERTIFICATE_BODY: &str = "-----BEGIN CERTIFICATE-----\nMIIBleaf\n-----END CERTIFICATE-----";
const CERTIFICATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMIIBkey\n-----END PRIVATE KEY-----";
const CERTIFICATE_CHAIN: &str = "-----BEGIN CERTIFICATE-----\nMIIBca\n-----END CERTIFICATE-----";

/// `CreateDomainName` with `body`.
async fn create_domain(gateway: &Gateway, body: Value) -> Answer {
    gateway.call("POST", "/domainnames", Some(body)).await
}

/// `GetDomainName` for `domain_name`, with the `domainNameId` a PRIVATE record is addressed
/// by as well when it has one.
async fn get_domain(gateway: &Gateway, domain_name: &str, domain_name_id: Option<&str>) -> Answer {
    let uri = match domain_name_id {
        Some(id) => format!("/domainnames/{domain_name}?domainNameId={id}"),
        None => format!("/domainnames/{domain_name}"),
    };
    gateway.call("GET", &uri, None).await
}

/// Every `domainName` the account holds.
async fn listed_names(gateway: &Gateway, case: &str) -> BTreeSet<String> {
    let listed = gateway.call("GET", "/domainnames?limit=500", None).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected 200 from GetDomainNames, got {} with {:?}",
        listed.status, listed.body
    );
    page_items(&listed.body, case)
        .iter()
        .map(|item| string_member(item, "domainName", case))
        .collect()
}

/// What the rule says of one request: the create is stored, or refused for the endpoint type
/// it resolved to.
enum Verdict {
    /// The request names its endpoint's certificate, so the record is stored.
    Stored,
    /// The request names none, so it is refused for this endpoint type.
    NoCertificateFor(&'static str),
}

/// One request the rule is asserted over.
struct Row {
    case: &'static str,
    /// The create body, without its `domainName` (the case's own name is put in).
    body: Value,
    verdict: Verdict,
}

/// The rows, spanning each endpoint type crossed with what the request names for it.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "no endpoint configuration and no certificate at all",
            body: json!({}),
            verdict: Verdict::NoCertificateFor("EDGE"),
        },
        Row {
            case: "an edge-optimized endpoint with no certificate",
            body: json!({ "endpointConfiguration": { "types": ["EDGE"] } }),
            verdict: Verdict::NoCertificateFor("EDGE"),
        },
        Row {
            case: "a regional endpoint with no certificate",
            body: json!({ "endpointConfiguration": { "types": ["REGIONAL"] } }),
            verdict: Verdict::NoCertificateFor("REGIONAL"),
        },
        Row {
            case: "a private endpoint with no certificate",
            body: json!({ "endpointConfiguration": { "types": ["PRIVATE"] } }),
            verdict: Verdict::NoCertificateFor("PRIVATE"),
        },
        Row {
            // `certificateName` is "the user-friendly name of the certificate", which names
            // no certificate.
            case: "an edge-optimized endpoint naming only a certificate name",
            body: json!({ "certificateName": "the-edge-one" }),
            verdict: Verdict::NoCertificateFor("EDGE"),
        },
        Row {
            case: "a regional endpoint naming only a regional certificate name",
            body: json!({
                "endpointConfiguration": { "types": ["REGIONAL"] },
                "regionalCertificateName": "the-regional-one",
            }),
            verdict: Verdict::NoCertificateFor("REGIONAL"),
        },
        Row {
            // The OTHER endpoint's member: a regional certificate is "used by regional
            // endpoint", so it is not what an edge-optimized endpoint presents.
            case: "an edge-optimized endpoint naming the regional certificate",
            body: json!({ "regionalCertificateArn": CERTIFICATE_ARN }),
            verdict: Verdict::NoCertificateFor("EDGE"),
        },
        Row {
            case: "a regional endpoint naming the edge-optimized certificate",
            body: json!({
                "endpointConfiguration": { "types": ["REGIONAL"] },
                "certificateArn": CERTIFICATE_ARN,
            }),
            verdict: Verdict::NoCertificateFor("REGIONAL"),
        },
        Row {
            case: "a private endpoint naming the regional certificate",
            body: json!({
                "endpointConfiguration": { "types": ["PRIVATE"] },
                "regionalCertificateArn": CERTIFICATE_ARN,
            }),
            verdict: Verdict::NoCertificateFor("PRIVATE"),
        },
        Row {
            // The ownership certificate "validates ownership of your custom domain" rather
            // than serving its traffic.
            case: "a regional endpoint naming only the ownership certificate",
            body: json!({
                "endpointConfiguration": { "types": ["REGIONAL"] },
                "ownershipVerificationCertificateArn": CERTIFICATE_ARN,
            }),
            verdict: Verdict::NoCertificateFor("REGIONAL"),
        },
        Row {
            // Half of the uploaded material: a leaf with no key cannot complete a handshake.
            case: "an edge-optimized endpoint uploading a certificate body only",
            body: json!({
                "certificateName": "half-uploaded",
                "certificateBody": CERTIFICATE_BODY,
                "certificateChain": CERTIFICATE_CHAIN,
            }),
            verdict: Verdict::NoCertificateFor("EDGE"),
        },
        Row {
            case: "an edge-optimized endpoint uploading a private key only",
            body: json!({ "certificatePrivateKey": CERTIFICATE_KEY }),
            verdict: Verdict::NoCertificateFor("EDGE"),
        },
        Row {
            case: "an edge-optimized endpoint naming the regional certificate and an ownership one",
            body: json!({
                "regionalCertificateArn": CERTIFICATE_ARN,
                "ownershipVerificationCertificateArn": SECOND_CERTIFICATE_ARN,
            }),
            verdict: Verdict::NoCertificateFor("EDGE"),
        },
        Row {
            case: "an edge-optimized endpoint naming its certificate",
            body: json!({ "certificateArn": CERTIFICATE_ARN }),
            verdict: Verdict::Stored,
        },
        Row {
            case: "no endpoint configuration, naming the edge-optimized certificate",
            body: json!({
                "certificateName": "defaulted",
                "certificateArn": CERTIFICATE_ARN,
            }),
            verdict: Verdict::Stored,
        },
        Row {
            case: "an edge-optimized endpoint uploading the leaf, the key and the chain",
            body: json!({
                "certificateName": "uploaded",
                "certificateBody": CERTIFICATE_BODY,
                "certificatePrivateKey": CERTIFICATE_KEY,
                "certificateChain": CERTIFICATE_CHAIN,
            }),
            verdict: Verdict::Stored,
        },
        Row {
            case: "an edge-optimized endpoint uploading the leaf and the key only",
            body: json!({
                "certificateBody": CERTIFICATE_BODY,
                "certificatePrivateKey": CERTIFICATE_KEY,
            }),
            verdict: Verdict::Stored,
        },
        Row {
            case: "a regional endpoint naming its certificate",
            body: json!({
                "endpointConfiguration": { "types": ["REGIONAL"] },
                "regionalCertificateName": "the-regional-one",
                "regionalCertificateArn": CERTIFICATE_ARN,
            }),
            verdict: Verdict::Stored,
        },
        Row {
            case: "a regional endpoint naming its certificate and the ownership one",
            body: json!({
                "endpointConfiguration": { "types": ["REGIONAL"] },
                "regionalCertificateArn": CERTIFICATE_ARN,
                "ownershipVerificationCertificateArn": SECOND_CERTIFICATE_ARN,
            }),
            verdict: Verdict::Stored,
        },
        Row {
            case: "a private endpoint naming its certificate",
            body: json!({
                "endpointConfiguration": { "types": ["PRIVATE"] },
                "certificateArn": CERTIFICATE_ARN,
            }),
            verdict: Verdict::Stored,
        },
        Row {
            case: "a private endpoint uploading the leaf and the key",
            body: json!({
                "endpointConfiguration": { "types": ["PRIVATE"] },
                "certificateBody": CERTIFICATE_BODY,
                "certificatePrivateKey": CERTIFICATE_KEY,
            }),
            verdict: Verdict::Stored,
        },
    ]
}

#[tokio::test]
async fn a_created_domain_name_must_name_the_certificate_its_endpoint_presents() {
    let gateway = Gateway::new();
    let mut stored: BTreeSet<String> = BTreeSet::new();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let domain_name = format!("complete-{index}.example.com");
        let mut request = row.body;
        request["domainName"] = json!(domain_name);
        let answer = create_domain(&gateway, request).await;
        match row.verdict {
            Verdict::Stored => {
                assert_eq!(
                    answer.status, 201,
                    "case {case:?}: expected 201 from CreateDomainName, got {} with {:?}",
                    answer.status, answer.body
                );
                // Read back through the operation a client reads a record with — a private
                // record by the `domainNameId` its create minted, which is what addresses
                // one — so what the create reported is what the store holds.
                let read = get_domain(
                    &gateway,
                    &domain_name,
                    answer.body["domainNameId"].as_str(),
                )
                .await;
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: expected 200 from GetDomainName, got {} with {:?}",
                    read.status, read.body
                );
                assert_eq!(
                    read.body["domainName"].as_str(),
                    Some(domain_name.as_str()),
                    "case {case:?}: expected the stored record, got {:?}",
                    read.body
                );
                stored.insert(domain_name);
            }
            Verdict::NoCertificateFor(endpoint_type) => {
                assert_failure(&answer, 400, BAD_REQUEST, case);
                let message = string_member(&answer.body, "message", case);
                let expected = certificate_not_provided(endpoint_type);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                // Nothing was stored, so the name the refused create was sent for resolves
                // to nothing.
                let read = get_domain(&gateway, &domain_name, None).await;
                assert_failure(&read, 404, NOT_FOUND, case);
                let read_message = string_member(&read.body, "message", case);
                assert_eq!(
                    read_message, NO_SUCH_DOMAIN_NAME,
                    "case {case:?}: expected the identifier refusal, got {read_message:?}"
                );
            }
        }
    }

    // Exactly the admitted records are the account's, so no refused create left a record
    // behind and no admitted one went missing.
    let listed = listed_names(&gateway, "after the rows").await;
    assert_eq!(
        listed, stored,
        "case \"after the rows\": expected the account to hold {stored:?}, got {listed:?}"
    );
}

#[tokio::test]
async fn the_completeness_refusal_does_not_displace_the_unreadable_arn_refusal() {
    let gateway = Gateway::new();

    // A value that is no certificate ARN at all is refused for its FORM — the service's
    // fixed unreadable-ARN sentence — whichever certificate member carried it, so the two
    // refusals stay distinguishable by their message. The rows span the ways a value is not
    // the ARN of a certificate, crossed with the member and endpoint type that would
    // otherwise COMPLETE the request.
    let rows: Vec<(&str, &str, &str, Value)> = vec![
        (
            "a certificate ARN that is no ARN at all",
            "certificateArn",
            "not-a-well-formed-acm-arn",
            json!({}),
        ),
        (
            "a certificate ARN of another service",
            "certificateArn",
            "arn:aws:iam::000000000000:role/certificates",
            json!({ "endpointConfiguration": { "types": ["PRIVATE"] } }),
        ),
        (
            "a regional certificate ARN that is no ARN at all",
            "regionalCertificateArn",
            "not-a-well-formed-acm-arn",
            json!({ "endpointConfiguration": { "types": ["REGIONAL"] } }),
        ),
        (
            "a certificate ARN that is blank",
            "certificateArn",
            "   ",
            json!({}),
        ),
        (
            "a regional certificate ARN of another certificate-manager resource",
            "regionalCertificateArn",
            "arn:aws:acm:us-west-2:000000000000:certificate-authority/11111111",
            json!({ "endpointConfiguration": { "types": ["REGIONAL"] } }),
        ),
    ];

    for (index, (case, member, value, endpoint)) in rows.into_iter().enumerate() {
        let domain_name = format!("form-{index}.example.com");
        let mut request = endpoint;
        request["domainName"] = json!(domain_name);
        request[member] = json!(value);
        let answer = create_domain(&gateway, request).await;
        assert_failure(&answer, 400, BAD_REQUEST, case);
        let message = string_member(&answer.body, "message", case);
        assert_eq!(
            message, INVALID_ARN,
            "case {case:?}: expected the unreadable-ARN refusal, got {message:?}"
        );
        for endpoint_type in ["EDGE", "REGIONAL", "PRIVATE"] {
            assert_ne!(
                message,
                certificate_not_provided(endpoint_type),
                "case {case:?}: expected the form refusal to differ from the completeness one"
            );
        }
        let read = get_domain(&gateway, &domain_name, None).await;
        assert_failure(&read, 404, NOT_FOUND, case);
    }
}
