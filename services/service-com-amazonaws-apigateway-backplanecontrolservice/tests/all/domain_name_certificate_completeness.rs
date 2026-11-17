// SPDX-License-Identifier: CC0-1.0

//! What `CreateDomainName` requires of the CERTIFICATE a custom domain name would present,
//! and what the registry of custom domain names therefore holds, asserted on the wire.
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
//!
//! Two further rules follow the refused create through the rest of the surface, which is
//! where a client meets its consequence:
//!
//! * **an operation that addresses a custom domain name by its own identifier reports the
//!   record that identifier resolves to, and refuses an identifier no stored record answers
//!   to with the modelled `NotFoundException` and this service's identifier sentence** — so a
//!   name whose create was refused, a name never sent at all, a deleted record and a private
//!   record addressed without the `domainNameId` that identifies it are all alike absent to
//!   `GetDomainName`, `UpdateDomainName` and `DeleteDomainName`, while the records the
//!   registry does hold still read;
//! * **the endpoint address a stored record publishes is one THIS emulator answers on** —
//!   `regionalDomainName` is the address "you set up this association by adding a DNS record
//!   that points the custom domain name to", so it is the `host:port` of the listener the
//!   record was opened on, it is the same value through every operation that reports the
//!   record (the create, the read, the collection and the patch), and a client can dial it.
//!   What that address then SERVES is the `regional_custom_domain_front_door` suite's
//!   subject, whose dial this suite reuses.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::{
    certificate_not_provided, INVALID_ARN, NO_SUCH_DOMAIN_NAME,
};

use crate::domain_name_collection::created_domain;
use crate::patch_updates::{patch, replace};
use crate::regional_custom_domain_front_door::dial;
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

// ---------------------------------------------------------------------------
// What the registry holds afterwards, and what every reader of it reports
// ---------------------------------------------------------------------------

/// One identifier a request addresses a custom domain name by: the `domainName` the path
/// carries and, for a private record, the `domainNameId` that identifies it among the
/// records sharing that name.
struct Addressed {
    case: &'static str,
    domain_name: String,
    domain_name_id: Option<String>,
}

impl Addressed {
    /// The URI `GetDomainName`, `UpdateDomainName` and `DeleteDomainName` all address one
    /// record through.
    fn uri(&self) -> String {
        match &self.domain_name_id {
            Some(id) => format!("/domainnames/{}?domainNameId={id}", self.domain_name),
            None => format!("/domainnames/{}", self.domain_name),
        }
    }
}

/// `DeleteDomainName` for `uri`.
async fn delete_domain(gateway: &Gateway, uri: &str) -> Answer {
    gateway.call("DELETE", uri, None).await
}

/// The record `domain_name` is reported by in the account's collection, or `None` where the
/// collection reports no record under that name.
async fn listed_record(gateway: &Gateway, domain_name: &str, case: &str) -> Option<Value> {
    let listed = gateway.call("GET", "/domainnames?limit=500", None).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected 200 from GetDomainNames, got {} with {:?}",
        listed.status, listed.body
    );
    page_items(&listed.body, case)
        .into_iter()
        .find(|item| item["domainName"].as_str() == Some(domain_name))
}

/// RULE: an operation that addresses a custom domain name by its own identifier reports the
/// record that identifier resolves to, and refuses an identifier no stored record answers to
/// with the modelled `NotFoundException` and this service's identifier sentence.
///
/// The rows span the ways an identifier resolves to no record — a name whose create was
/// refused for want of its endpoint's certificate (one per endpoint type), a name no request
/// ever sent, a record that was created and then deleted, a private record addressed without
/// the id that identifies it, and a stored public name qualified with an id no record answers
/// to — crossed with every operation that addresses a record that way. The records the
/// registry DOES hold are read back afterwards, so what is asserted is absence rather than an
/// operation that refuses everything.
#[tokio::test]
async fn an_identifier_the_registry_holds_no_record_for_is_the_modelled_not_found() {
    let gateway = Gateway::new();

    // The records the registry holds, admitted through the service's own create — one per
    // endpoint type, so no refusal below can be about the endpoint type an operation was
    // handed. `created_domain` names the certificate the endpoint type presents, which the
    // rule above requires of every admitted create.
    let regional = "held-regional.example.com";
    let edge = "held-edge.example.com";
    let private = "held-private.example.com";
    created_domain(
        &gateway,
        regional,
        json!({ "endpointConfiguration": { "types": ["REGIONAL"] } }),
    )
    .await;
    created_domain(&gateway, edge, json!({})).await;
    let private_record = created_domain(
        &gateway,
        private,
        json!({ "endpointConfiguration": { "types": ["PRIVATE"] } }),
    )
    .await;
    let private_id = string_member(&private_record, "domainNameId", "the private record");

    // A record created and then deleted: the delete is what makes its name resolve to
    // nothing again.
    let deleted = "deleted-regional.example.com";
    created_domain(
        &gateway,
        deleted,
        json!({ "endpointConfiguration": { "types": ["REGIONAL"] } }),
    )
    .await;
    let removed = delete_domain(&gateway, &format!("/domainnames/{deleted}")).await;
    assert_eq!(
        removed.status, 202,
        "case \"the deleted record\": expected 202 from DeleteDomainName, got {} with {:?}",
        removed.status, removed.body
    );

    // Creates refused for want of the certificate their endpoint type would present: each
    // leaves the registry holding no record under the name it was sent for.
    let uncertified = [
        (
            "the refused regional create",
            "uncertified-regional.example.com",
            json!({ "endpointConfiguration": { "types": ["REGIONAL"] } }),
            "REGIONAL",
        ),
        (
            "the refused private create",
            "uncertified-private.example.com",
            json!({ "endpointConfiguration": { "types": ["PRIVATE"] } }),
            "PRIVATE",
        ),
        (
            "the refused create that configured no endpoint",
            "uncertified-default.example.com",
            json!({}),
            "EDGE",
        ),
    ];
    for (case, domain_name, endpoint, endpoint_type) in &uncertified {
        let mut request = endpoint.clone();
        request["domainName"] = json!(domain_name);
        let answer = create_domain(&gateway, request).await;
        assert_failure(&answer, 400, BAD_REQUEST, case);
        let message = string_member(&answer.body, "message", case);
        let expected = certificate_not_provided(endpoint_type);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }

    let mut absent = vec![
        Addressed {
            case: "a name no request ever sent",
            domain_name: "never-requested.example.com".to_string(),
            domain_name_id: None,
        },
        Addressed {
            case: "the deleted record",
            domain_name: deleted.to_string(),
            domain_name_id: None,
        },
        Addressed {
            // "A domainNameId is supported only for private custom domain names", and a
            // private record needs it to be identified at all, so the name alone addresses
            // no record.
            case: "a private record addressed without the id that identifies it",
            domain_name: private.to_string(),
            domain_name_id: None,
        },
        Addressed {
            case: "a stored name qualified with an id no record answers to",
            domain_name: regional.to_string(),
            domain_name_id: Some("zzzzzzzzzz".to_string()),
        },
    ];
    for (case, domain_name, _endpoint, _endpoint_type) in &uncertified {
        absent.push(Addressed {
            case,
            domain_name: (*domain_name).to_string(),
            domain_name_id: None,
        });
    }

    for addressed in &absent {
        let uri = addressed.uri();
        // The patch is one this resource supports, so a refusal can only be about the
        // identifier it was sent for.
        let answers = [
            ("GetDomainName", gateway.call("GET", &uri, None).await),
            (
                "UpdateDomainName",
                patch(
                    &gateway,
                    &uri,
                    vec![replace("/regionalCertificateName", "renamed")],
                )
                .await,
            ),
            ("DeleteDomainName", delete_domain(&gateway, &uri).await),
        ];
        for (operation, answer) in answers {
            let case = format!("{} through {operation}", addressed.case);
            assert_failure(&answer, 404, NOT_FOUND, &case);
            let message = string_member(&answer.body, "message", &case);
            assert_eq!(
                message, NO_SUCH_DOMAIN_NAME,
                "case {case:?}: expected the identifier refusal, got {message:?}"
            );
        }
    }

    // The records the registry holds are untouched by every refusal above and still read
    // back through the identifier that addresses them.
    for (case, domain_name, domain_name_id) in [
        ("the regional record the registry holds", regional, None),
        ("the edge-optimized record the registry holds", edge, None),
        (
            "the private record the registry holds",
            private,
            Some(private_id.as_str()),
        ),
    ] {
        let read = get_domain(&gateway, domain_name, domain_name_id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected 200 from GetDomainName, got {} with {:?}",
            read.status, read.body
        );
        assert_eq!(
            string_member(&read.body, "domainName", case),
            domain_name,
            "case {case:?}: expected the record its identifier addresses, got {:?}",
            read.body
        );
    }

    // And the collection reports exactly those records: no refused create and no deleted
    // record is among them.
    let listed = listed_names(&gateway, "the registry").await;
    let held: BTreeSet<String> = [regional, edge, private]
        .into_iter()
        .map(str::to_string)
        .collect();
    assert_eq!(
        listed, held,
        "case \"the registry\": expected the account to hold {held:?}, got {listed:?}"
    );
}

/// RULE: the endpoint address a stored record publishes is one THIS emulator answers on, and
/// every operation that reports the record reports that same address.
///
/// The rows differ in the create the record was admitted by and in the patch applied to it
/// afterwards: the address belongs to the endpoint the record was opened on, so neither the
/// configuration it was created with nor a later change to its members moves it. The last
/// case is the rule's boundary — only a regional endpoint is reached on such an address, so
/// an edge-optimized record publishes none at all.
#[tokio::test]
async fn a_stored_record_publishes_one_dialable_address_through_every_operation_that_reports_it() {
    let gateway = Gateway::new();

    let rows: Vec<(&str, Value, Vec<Value>)> = vec![
        (
            "a regional record",
            json!({ "endpointConfiguration": { "types": ["REGIONAL"] } }),
            vec![replace("/securityPolicy", "TLS_1_2")],
        ),
        (
            "a regional record whose certificate is renamed",
            json!({
                "endpointConfiguration": { "types": ["REGIONAL"] },
                "regionalCertificateName": "initial-certificate",
            }),
            vec![replace("/regionalCertificateName", "renewed-certificate")],
        ),
        (
            "a regional record whose routing mode is replaced",
            json!({
                "endpointConfiguration": { "types": ["REGIONAL"] },
                "securityPolicy": "TLS_1_2",
            }),
            vec![replace("/routingMode", "ROUTING_RULE_ONLY")],
        ),
    ];

    for (index, (case, request, operations)) in rows.into_iter().enumerate() {
        let domain_name = format!("published-{index}.example.com");
        let created = created_domain(&gateway, &domain_name, request).await;
        let published = string_member(&created, "regionalDomainName", case);
        // The published value is an address a client DIALS rather than a name of another
        // service's: a host of some other provider's zone does not parse as one.
        assert!(
            published.parse::<std::net::SocketAddr>().is_ok(),
            "case {case:?}: expected an address a client can dial, got {published:?}"
        );
        // And this emulator answers on it: the listener the create opened accepts a
        // connection. What it then serves is the front-door suite's subject.
        drop(dial(case, &published).await);

        // Every operation that reports the record reports that one address: the read, the
        // collection, the patch and the read that follows it.
        let read = get_domain(&gateway, &domain_name, None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected 200 from GetDomainName, got {} with {:?}",
            read.status, read.body
        );
        let listed = listed_record(&gateway, &domain_name, case)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: expected {domain_name} to be listed"));
        let patched = patch(&gateway, &format!("/domainnames/{domain_name}"), operations).await;
        assert_eq!(
            patched.status, 200,
            "case {case:?}: expected 200 from UpdateDomainName, got {} with {:?}",
            patched.status, patched.body
        );
        let read_again = get_domain(&gateway, &domain_name, None).await;
        for (reporter, body) in [
            ("GetDomainName", &read.body),
            ("GetDomainNames", &listed),
            ("UpdateDomainName", &patched.body),
            ("GetDomainName after the patch", &read_again.body),
        ] {
            assert_eq!(
                body["regionalDomainName"].as_str(),
                Some(published.as_str()),
                "case {case:?}: expected {reporter} to report {published:?}, got {:?}",
                body["regionalDomainName"]
            );
        }
    }

    // The boundary: an edge-optimized record is reached through its CloudFront
    // distribution, so no operation reports a regional address for it.
    let edge_name = "published-edge.example.com";
    let edge = created_domain(&gateway, edge_name, json!({})).await;
    let read = get_domain(&gateway, edge_name, None).await;
    let listed = listed_record(&gateway, edge_name, "an edge-optimized record")
        .await
        .unwrap_or_else(|| panic!("case \"an edge-optimized record\": expected it to be listed"));
    for (reporter, body) in [
        ("CreateDomainName", &edge),
        ("GetDomainName", &read.body),
        ("GetDomainNames", &listed),
    ] {
        assert!(
            body["regionalDomainName"].is_null(),
            "case \"an edge-optimized record\": expected {reporter} to report no regional \
             address, got {:?}",
            body["regionalDomainName"]
        );
    }
}
