// SPDX-License-Identifier: CC0-1.0

//! How MANY findings a refused request BODY is reported with, and which members they are
//! about.
//!
//! The rule under test: a request whose JSON BODY carries values outside the `@pattern`s of
//! the members they were sent for is answered HTTP 400 `ValidationException` whose lower-case
//! `message` is ONE bracketed list holding one finding per refused member — each finding
//! quoting the member's expression as the MODEL declares it (anchors and all, because the
//! body-schema validator quotes its own schema's expression) and the string the request sent,
//! and nothing else: no member name, no count, and none of the `N validation error(s)
//! detected` framing a value bound to the request line is refused in (`request_line_refusal`).
//!
//! `model_pattern_refusal` pins that rule over requests breaking ONE pattern each. This suite
//! is the other dimension the rule generalises over: HOW MANY of a body's members the schema
//! refused at once, which is what makes the answer a LIST rather than a sentence. Its rows
//! therefore differ along the finding count (one, two, three), which operation carries the
//! body (both creates), the depth the refused member sits at (a top-level member, two members
//! inside a union member's structure) and which members are refused together — including
//! `ClientToken`, the one patterned body member both creates share and the only one whose
//! expression admits any printable ASCII run.
//!
//! Every expression below is quoted from the model (smithy_query
//! operation="CreateConnector,CreateChallenge", shape="ClientToken,AzureApplicationId,
//! AzureDomain,VpcEndpointId", model_path="services/service-com-amazonaws-pcaconnectorscep-pcaconnectorscep/smithy-model/com-amazonaws-pcaconnectorscep-pcaconnectorscep.json")
//! and every value a row sends is inside that member's own `@length` bounds — asserted per
//! row — so the one constraint each value breaks is its pattern.
//!
//! The last test is this suite's positive control: the SAME members, carrying values their
//! patterns admit, are refused by nothing and the connector the create minted is what a later
//! read answers with. The lifecycle that round trip is one step of is `connector_lifecycle`'s
//! subject; here it is only the other side of this rule.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{self, label, new_router, Answer};

/// The Region every call in this suite is signed for.
const REGION: &str = "eu-west-2";

/// `CertificateAuthorityArn`'s published pattern, with `@length(5, 200)`.
const CERTIFICATE_AUTHORITY_ARN_PATTERN: &str = r"^arn:aws(-[a-z]+)*:acm-pca:[a-z]+(-[a-z]+)+-[1-9]\d*:\d{12}:certificate-authority\/[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$";

/// `ConnectorArn`'s published pattern, with `@length(5, 200)`.
const CONNECTOR_ARN_PATTERN: &str = r"^arn:aws(-[a-z]+)*:pca-connector-scep:[a-z]+(-[a-z]+)+-[1-9]\d*:\d{12}:connector\/[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$";

/// `VpcEndpointId`'s published pattern, with `@length(13, 22)`.
const VPC_ENDPOINT_ID_PATTERN: &str = r"^vpce-[0-9a-f]{8}([0-9a-f]{9})?$";

/// `ClientToken`'s published pattern, with `@length(1, 64)`: any run of printable ASCII
/// characters, space excluded.
const CLIENT_TOKEN_PATTERN: &str = r"^[!-~]+$";

/// `AzureApplicationId`'s published pattern, with `@length(15, 100)`.
const AZURE_APPLICATION_ID_PATTERN: &str = r"^[a-zA-Z0-9]{2,15}-[a-zA-Z0-9]{2,15}-[a-zA-Z0-9]{2,15}-[a-zA-Z0-9]{2,15}-[a-zA-Z0-9]{2,15}$";

/// `AzureDomain`'s published pattern, with `@length(1, 256)`.
const AZURE_DOMAIN_PATTERN: &str = r"^[a-zA-Z0-9._-]+$";

/// A private-CA ARN the `CertificateAuthorityArn` pattern admits, for the rows whose refusal
/// is about some other member.
const CA_ARN: &str =
    "arn:aws:acm-pca:eu-west-2:000000000000:certificate-authority/11111111-2222-3333-4444-555555555555";

/// A connector ARN the `ConnectorArn` pattern admits, likewise.
const WELL_FORMED_CONNECTOR_ARN: &str =
    "arn:aws:pca-connector-scep:eu-west-2:000000000000:connector/22222222-3333-4444-5555-666666666666";

/// An Azure application id its pattern admits, likewise.
const AZURE_APPLICATION_ID: &str = "0123456789ab-cdef-0123-4567-89abcdef0123";

/// One member of a row the schema refuses: the expression the model declares for it, the
/// value the request sends, and that member's published `@length` bounds — carried so the row
/// can prove the value breaks the pattern ALONE.
struct Refused {
    member: &'static str,
    pattern: &'static str,
    value: &'static str,
    length: (usize, usize),
}

impl Refused {
    /// The finding the body-schema validator states about this member.
    fn finding(&self) -> String {
        format!(
            "ECMA 262 regex \"{}\" does not match input string \"{}\"",
            self.pattern, self.value
        )
    }
}

/// One row: a request body, and every member of it the schema refuses.
struct Row {
    name: &'static str,
    path: &'static str,
    body: Value,
    refused: Vec<Refused>,
}

fn rows() -> Vec<Row> {
    vec![
        // One finding: the member both creates share, which no other suite exercises.
        Row {
            name: "CreateConnector's ClientToken alone",
            path: "/connectors",
            body: json!({ "CertificateAuthorityArn": CA_ARN, "ClientToken": "token one" }),
            refused: vec![Refused {
                member: "ClientToken",
                pattern: CLIENT_TOKEN_PATTERN,
                value: "token one",
                length: (1, 64),
            }],
        },
        Row {
            name: "CreateChallenge's ClientToken alone",
            path: "/challenges",
            body: json!({
                "ConnectorArn": WELL_FORMED_CONNECTOR_ARN,
                "ClientToken": "token two",
            }),
            refused: vec![Refused {
                member: "ClientToken",
                pattern: CLIENT_TOKEN_PATTERN,
                value: "token two",
                length: (1, 64),
            }],
        },
        // Two findings, from two top-level members of one body.
        Row {
            name: "CreateConnector's CertificateAuthorityArn and ClientToken",
            path: "/connectors",
            body: json!({ "CertificateAuthorityArn": "not-an-arn", "ClientToken": "token three" }),
            refused: vec![
                Refused {
                    member: "CertificateAuthorityArn",
                    pattern: CERTIFICATE_AUTHORITY_ARN_PATTERN,
                    value: "not-an-arn",
                    length: (5, 200),
                },
                Refused {
                    member: "ClientToken",
                    pattern: CLIENT_TOKEN_PATTERN,
                    value: "token three",
                    length: (1, 64),
                },
            ],
        },
        Row {
            name: "CreateChallenge's ConnectorArn and ClientToken",
            path: "/challenges",
            body: json!({ "ConnectorArn": "also-not-an-arn", "ClientToken": "token four" }),
            refused: vec![
                Refused {
                    member: "ConnectorArn",
                    pattern: CONNECTOR_ARN_PATTERN,
                    value: "also-not-an-arn",
                    length: (5, 200),
                },
                Refused {
                    member: "ClientToken",
                    pattern: CLIENT_TOKEN_PATTERN,
                    value: "token four",
                    length: (1, 64),
                },
            ],
        },
        // Three findings, from three top-level members.
        Row {
            name: "CreateConnector's CertificateAuthorityArn, VpcEndpointId and ClientToken",
            path: "/connectors",
            body: json!({
                "CertificateAuthorityArn": "not-an-arn",
                "VpcEndpointId": "vpce-zzzzzzzzz",
                "ClientToken": "token five",
            }),
            refused: vec![
                Refused {
                    member: "CertificateAuthorityArn",
                    pattern: CERTIFICATE_AUTHORITY_ARN_PATTERN,
                    value: "not-an-arn",
                    length: (5, 200),
                },
                Refused {
                    member: "VpcEndpointId",
                    pattern: VPC_ENDPOINT_ID_PATTERN,
                    value: "vpce-zzzzzzzzz",
                    length: (13, 22),
                },
                Refused {
                    member: "ClientToken",
                    pattern: CLIENT_TOKEN_PATTERN,
                    value: "token five",
                    length: (1, 64),
                },
            ],
        },
        // Two findings from members nested inside a union member's structure, so the count is
        // not a property of the body's top level.
        Row {
            name: "CreateConnector's Intune application id and domain together",
            path: "/connectors",
            body: json!({
                "CertificateAuthorityArn": CA_ARN,
                "MobileDeviceManagement": {
                    "Intune": {
                        "AzureApplicationId": "not-an-application-id!!",
                        "Domain": "not a domain",
                    }
                },
            }),
            refused: vec![
                Refused {
                    member: "AzureApplicationId",
                    pattern: AZURE_APPLICATION_ID_PATTERN,
                    value: "not-an-application-id!!",
                    length: (15, 100),
                },
                Refused {
                    member: "Domain",
                    pattern: AZURE_DOMAIN_PATTERN,
                    value: "not a domain",
                    length: (1, 256),
                },
            ],
        },
        // A nested member refused beside a top-level one of the same body.
        Row {
            name: "CreateConnector's ClientToken beside its Intune domain",
            path: "/connectors",
            body: json!({
                "CertificateAuthorityArn": CA_ARN,
                "ClientToken": "token six",
                "MobileDeviceManagement": {
                    "Intune": {
                        "AzureApplicationId": AZURE_APPLICATION_ID,
                        "Domain": "also not a domain",
                    }
                },
            }),
            refused: vec![
                Refused {
                    member: "ClientToken",
                    pattern: CLIENT_TOKEN_PATTERN,
                    value: "token six",
                    length: (1, 64),
                },
                Refused {
                    member: "Domain",
                    pattern: AZURE_DOMAIN_PATTERN,
                    value: "also not a domain",
                    length: (1, 256),
                },
            ],
        },
    ]
}

/// The findings a bracketed list holds, in no particular order: the list's own punctuation is
/// the brackets and `", "` between findings, and no finding of this model carries that
/// sequence (asserted per row), so the list parses back unambiguously.
///
/// Compared as a SET because the rule is one finding per refused member — which members are
/// reported, not the order the validating walk happened to reach them in.
fn listed(message: &str) -> Option<Vec<String>> {
    let inner = message.strip_prefix('[')?.strip_suffix(']')?;
    let mut findings: Vec<String> = inner.split(", ").map(str::to_string).collect();
    findings.sort();
    Some(findings)
}

async fn send(router: &MultiServiceRouter, row: &Row) -> Answer {
    harness::call(router, REGION, "POST", row.path, Some(row.body.clone())).await
}

/// A request body breaking several of the model's patterns at once is reported with one
/// finding per refused member, inside the ONE bracketed list.
#[tokio::test]
async fn every_body_member_the_schema_refuses_gets_its_own_finding_in_one_list() {
    let (router, _engine) = new_router();
    for row in rows() {
        let case = row.name;
        let mut expected: Vec<String> = Vec::new();
        for refused in &row.refused {
            let member = refused.member;
            let (min, max) = refused.length;
            let length = refused.value.chars().count();
            assert!(
                (min..=max).contains(&length),
                "case {case:?}: the value sent for {member:?} must stay inside its published \
                 length {min}..={max} so the one constraint it breaks is the pattern, got \
                 {length}"
            );
            let finding = refused.finding();
            assert!(
                !finding.contains(", "),
                "case {case:?}: the finding about {member:?} must carry no list separator, or \
                 the list cannot be read back, got {finding:?}"
            );
            expected.push(finding);
        }
        expected.sort();

        let answer = send(&router, &row).await;
        assert_eq!(
            (answer.status, answer.error_type.as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {} / {:?}",
            answer.status,
            answer.body
        );
        let message = answer.body["message"].as_str().unwrap_or_default();
        let findings = listed(message).unwrap_or_else(|| {
            panic!("case {case:?}: expected one bracketed list of findings, got {message:?}")
        });
        assert_eq!(
            findings, expected,
            "case {case:?}: expected one finding per refused member, got {message:?}"
        );
        // What this wording is NOT: the envelope a value bound to the REQUEST LINE is refused
        // in, which counts the clauses and names each member.
        assert!(
            !message.contains("validation error") && !message.contains(" at '"),
            "case {case:?}: a refused body names no member and counts nothing, got {message:?}"
        );
    }
}

/// The other side of the rule: a body every pattern admits is refused by nothing — the create
/// is served, and the connector it minted is what a later read answers with.
#[tokio::test]
async fn a_body_every_pattern_admits_is_served_and_reads_back() {
    let (router, engine) = new_router();
    // Values the same members' patterns admit, including the printable-ASCII `ClientToken`
    // run that every row above sends a space inside.
    let created = harness::call(
        &router,
        REGION,
        "POST",
        "/connectors",
        Some(json!({ "CertificateAuthorityArn": CA_ARN, "ClientToken": "token-seven!~" })),
    )
    .await;
    assert_eq!(
        (created.status, created.error_type.as_str()),
        (202, ""),
        "expected 202 from a create every pattern admits, got {} / {:?}",
        created.status,
        created.body
    );
    let arn = created.text("ConnectorArn");
    harness::release_pending(&engine, &arn);

    let read = harness::call(
        &router,
        REGION,
        "GET",
        &format!("/connectors/{}", label(&arn)),
        None,
    )
    .await;
    assert_eq!(
        (read.status, read.error_type.as_str()),
        (200, ""),
        "expected 200 from GetConnector on {arn:?}, got {} / {:?}",
        read.status,
        read.body
    );
    assert_eq!(
        read.connector("Arn").as_str(),
        Some(arn.as_str()),
        "expected GetConnector to answer the connector the create minted, got {} / {:?}",
        read.status,
        read.body
    );
    assert_eq!(
        read.connector("CertificateAuthorityArn").as_str(),
        Some(CA_ARN),
        "expected the stored connector to carry the CA the create named, got {:?}",
        read.body
    );
}
