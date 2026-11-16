// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the one precondition a custom domain name's certificate members
//! carry: the STATE Certificate Manager holds the certificate in.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — with Certificate Manager mounted beside
//! this service on the same engine, so every certificate a case names is brought into being
//! through that service's OWN API (`ImportCertificate`, `RequestCertificate`) and every
//! status a case weighs is read back through its own `DescribeCertificate` rather than
//! written into a store by hand.
//!
//! The RULE under test, one level above the recorded call:
//!
//! > A member that names an Amazon Web Services-managed certificate — `certificateArn`,
//! > `regionalCertificateArn`, `ownershipVerificationCertificateArn` — configures the custom
//! > domain name to USE that certificate, so the certificate has to be one Certificate
//! > Manager reports as `ISSUED` at the moment of the call: a call configuring one in any
//! > other status is the operation's modelled `BadRequestException` (400) whose message is
//! > the fixed sentence [`CERTIFICATE_NOT_ISSUED`], with nothing stored and nothing changed.
//! > The gate is ordered AFTER the unreadable-ARN refusal, so a value that is no certificate
//! > ARN at all still earns that one, and it weighs a STATE alone: an answer establishing no
//! > status refuses nothing.
//!
//! And the other half of the same seam, which the suite's last test drives:
//!
//! > A custom domain name that references a certificate is a resource Certificate Manager has
//! > to report as USING it (`CertificateDetail$InUseBy`) and protect it for, so every call
//! > that creates, repoints or removes the record tells that service what the record
//! > references now — and a reference it gives up stays in use until the work that takes it
//! > away has run.
//!
//! The rows span what the rule generalises over: the statuses Certificate Manager reports
//! (`ISSUED` from an import, `PENDING_VALIDATION` from a DNS-validated request whose evidence
//! nobody published, `FAILED` from a request for a name the registries delegate to nobody),
//! the three members that name a certificate, the endpoint types a domain name is served by
//! (`REGIONAL`, `EDGE`, `PRIVATE`), both calls that configure a certificate (the create and
//! the patch), a certificate Certificate Manager holds no record of, and the ways a value is
//! no certificate ARN at all.
//!
//! [`Gateway`], [`gateway_with_acm_and_s3`], [`assert_failure`], [`page_items`] and
//! [`string_member`] are this crate's one wire-test harness, and [`leaf`] /
//! [`import_certificate`] are the front-door suite's one spelling of a real certificate
//! brought into Certificate Manager.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::INVALID_ARN;

use crate::regional_custom_domain_front_door::{import_certificate, leaf};
use crate::rest_api_lifecycle::{
    assert_failure, gateway_with_acm_and_s3, page_items, string_member, Answer, Gateway,
};

/// The Region every call of this suite is signed for, which is the one the harness's
/// credential scope carries — so the apigateway call and the Certificate Manager call that
/// stood the certificate up address the same Region.
const REGION: &str = "us-east-1";

const BAD_REQUEST: &str = "BadRequestException";

/// The status of a certificate Certificate Manager has issued, one value of the
/// `CertificateStatus` catalog of that service's model.
const ISSUED: &str = "ISSUED";

/// The sentence AWS answers a call configuring a certificate that is not [`ISSUED`] with.
///
/// Spelled out here rather than read from the crate: it is the wording a client's error
/// handling keys on, so this suite pins it independently of the constant the handler carries.
/// Fixed text with no variable part — it names the STATE, not the certificate.
const CERTIFICATE_NOT_ISSUED: &str = "Certificate is not in an ISSUED state, and cannot be used";

/// The `TLS_1_2` security policy every case asks for, one of the values the model's
/// `SecurityPolicy` declares (`smithy_query(query="shape", shape="SecurityPolicy")`).
const SECURITY_POLICY: &str = "TLS_1_2";

/// How long a case waits for the status Certificate Manager's clock owes a requested
/// certificate. A GENEROUS bound on work that is all in this process and driven by that
/// service's own stored due instant; the wait is for the STATUS, looked at again and again,
/// never for a span.
const STATUS_DEADLINE: Duration = Duration::from_secs(60);

/// How long a case waits between two looks at a status.
const LOOK: Duration = Duration::from_millis(50);

/// The members of a `DomainName` that name a Certificate Manager certificate
/// (`smithy_query(query="operation", operation="CreateDomainName")`).
const CERTIFICATE_MEMBERS: &[&str] = &[
    "regionalCertificateArn",
    "certificateArn",
    "ownershipVerificationCertificateArn",
];

// ---------------------------------------------------------------------------
// Certificate Manager, over the same router
// ---------------------------------------------------------------------------

/// One call to the Certificate Manager service mounted beside this one, answering the status
/// and the JSON body it answered with.
///
/// That service speaks `awsJson1_1`: a POST carrying the operation in `x-amz-target`. The
/// request goes through the SAME router the apigateway calls go through, so both services
/// read one engine.
async fn acm_call(gateway: &Gateway, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("acm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CertificateManager.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/acm/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the certificate manager request is well formed");
    let response = gateway.send(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// The same call, asserting Certificate Manager accepted it.
async fn acm(gateway: &Gateway, operation: &str, body: Value, case: &str) -> Value {
    let (status, parsed) = acm_call(gateway, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} with {parsed:?}"
    );
    parsed
}

/// `RequestCertificate` for `domain_name`, DNS-validated, answering the ARN it was stored
/// under. Nothing publishes the validation evidence, so what the request leaves behind is the
/// status that service decides for the name it covers.
async fn request_certificate(
    gateway: &Gateway,
    domain_name: &str,
    token: &str,
    case: &str,
) -> String {
    let answered = acm(
        gateway,
        "RequestCertificate",
        json!({
            "DomainName": domain_name,
            "ValidationMethod": "DNS",
            "IdempotencyToken": token,
        }),
        case,
    )
    .await;
    string_member(&answered, "CertificateArn", case)
}

/// The status Certificate Manager reports for `certificate_arn`, read through its own
/// `DescribeCertificate` — so each row's premise is that service's answer rather than an
/// assumption about it.
async fn certificate_status(gateway: &Gateway, certificate_arn: &str, case: &str) -> String {
    let answered = acm(
        gateway,
        "DescribeCertificate",
        json!({ "CertificateArn": certificate_arn }),
        case,
    )
    .await;
    string_member(&answered["Certificate"], "Status", case)
}

/// The status of `certificate_arn` once it is `expected`, asserting that it got there.
///
/// A status Certificate Manager's own clock owes a request is not there the instant the
/// request answers, so the premise of a row is reached by LOOKING AGAIN until it holds —
/// never by waiting out a span — and the deadline is generous enough that only a defect
/// reaches it.
async fn settled_certificate_status(
    gateway: &Gateway,
    certificate_arn: &str,
    expected: &str,
    case: &str,
) -> String {
    let deadline = Instant::now() + STATUS_DEADLINE;
    loop {
        let status = certificate_status(gateway, certificate_arn, case).await;
        if status == expected {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the certificate manager to report {expected:?}, got \
             {status:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// A certificate a case points a custom domain name at: the ARN, and the status Certificate
/// Manager holds it in (`None` for one that service holds no record of).
struct Certificate {
    case: &'static str,
    arn: String,
    status: Option<String>,
}

impl Certificate {
    /// Whether a custom domain name may be configured to use this certificate: only an
    /// `ISSUED` one — and a certificate no status could be established for is not refused by
    /// this rule, so an ARN that service holds no record of is admitted here (what an absent
    /// certificate itself earns is a different precondition, with a different refusal).
    fn admitted(&self) -> bool {
        self.status.as_deref().is_none_or(|status| status == ISSUED)
    }
}

/// The certificates every case of this suite is weighed against, each brought into being
/// through Certificate Manager's own API, plus an ARN that service holds no record of.
async fn certificates(gateway: &Gateway) -> Vec<Certificate> {
    let issued = import_certificate(
        gateway,
        "an imported certificate",
        &leaf("issued.example.com"),
    )
    .await;
    // A name the public DNS delegates to a registrant: its holder could publish the
    // validation evidence, which nobody here does, so the request waits for it.
    let pending = request_certificate(
        gateway,
        "pending.apigw-certificate-gate.com",
        "pendingtoken",
        "a pending request",
    )
    .await;
    // A name the registries reserve rather than delegate: nobody can prove control of it, so
    // the request fails outright instead of waiting.
    let failed = request_certificate(
        gateway,
        "failed.example.com",
        "failedtoken",
        "a failed request",
    )
    .await;

    // Each row's PREMISE is the status that service reports for it, waited for by looking
    // again: a request that fails does so on Certificate Manager's own clock.
    let mut built = Vec::new();
    for (case, arn, expected) in [
        ("an imported certificate", issued, ISSUED),
        (
            "a DNS validation nobody has completed",
            pending,
            "PENDING_VALIDATION",
        ),
        ("a request that failed", failed, "FAILED"),
    ] {
        let status = settled_certificate_status(gateway, &arn, expected, case).await;
        built.push(Certificate {
            case,
            arn,
            status: Some(status),
        });
    }
    built.push(Certificate {
        case: "an ARN the certificate manager holds no record of",
        arn: engine::emulator_arn(
            "acm",
            REGION,
            "certificate/11111111-2222-3333-4444-555555555555",
        ),
        status: None,
    });
    built
}

// ---------------------------------------------------------------------------
// The calls under test
// ---------------------------------------------------------------------------

/// `CreateDomainName` with `body`.
async fn create_domain(gateway: &Gateway, body: Value) -> Answer {
    gateway.call("POST", "/domainnames", Some(body)).await
}

/// `GetDomainName` for `domain_name`, asserting the record is there to be read.
async fn get_domain(gateway: &Gateway, domain_name: &str, case: &str) -> Value {
    let answer = gateway
        .call("GET", &format!("/domainnames/{domain_name}"), None)
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected GetDomainName({domain_name}) to answer 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// `UpdateDomainName` for `domain_name` with one `replace` of `path` to `value`.
async fn patch_domain(gateway: &Gateway, domain_name: &str, path: &str, value: &str) -> Answer {
    gateway
        .call(
            "PATCH",
            &format!("/domainnames/{domain_name}"),
            Some(json!({
                "patchOperations": [{ "op": "replace", "path": path, "value": value }]
            })),
        )
        .await
}

/// Every `domainName` the account holds, in page order.
async fn listed_names(gateway: &Gateway, case: &str) -> Vec<String> {
    let listed = gateway.call("GET", "/domainnames?limit=500", None).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected GetDomainNames to answer 200, got {} with {:?}",
        listed.status, listed.body
    );
    page_items(&listed.body, case)
        .iter()
        .map(|item| string_member(item, "domainName", case))
        .collect()
}

/// A create body for `domain_name` served by `endpoint_type`, with `member` set to
/// `certificate_arn`.
fn create_body(
    domain_name: &str,
    endpoint_type: &str,
    member: &str,
    certificate_arn: &str,
) -> Value {
    let mut body = json!({
        "domainName": domain_name,
        "endpointConfiguration": { "types": [endpoint_type] },
        "securityPolicy": SECURITY_POLICY,
    });
    body[member] = json!(certificate_arn);
    body
}

#[tokio::test]
async fn a_created_domain_name_may_only_use_a_certificate_the_manager_has_issued() {
    let gateway = gateway_with_acm_and_s3();
    let certificates = certificates(&gateway).await;

    // The members that name a certificate, each crossed with an endpoint type a domain name is
    // served by: `certificateArn` is the edge-optimized and the private endpoint's,
    // `regionalCertificateArn` the regional one's, and `ownershipVerificationCertificateArn`
    // validates ownership of the domain whichever endpoint serves it.
    let configurations: &[(&str, &str)] = &[
        ("regionalCertificateArn", "REGIONAL"),
        ("certificateArn", "EDGE"),
        ("certificateArn", "PRIVATE"),
        ("ownershipVerificationCertificateArn", "REGIONAL"),
    ];

    let mut admitted_names: Vec<String> = Vec::new();
    let mut refused_names: Vec<String> = Vec::new();
    // The certificate the endpoint of the `ownershipVerificationCertificateArn` rows
    // PRESENTS. A create must name the certificate its endpoint type serves traffic with —
    // a completeness rule of its own, which the `domain_name_certificate_completeness` suite
    // owns — and an ownership certificate is not one, so those rows name an ISSUED
    // certificate for the endpoint and vary only the ownership certificate, which is what
    // this suite weighs.
    let serving = certificates
        .iter()
        .find(|certificate| certificate.status.as_deref() == Some(ISSUED))
        .expect("the suite imports one issued certificate")
        .arn
        .clone();
    for (index, (member, endpoint_type)) in configurations.iter().enumerate() {
        for (row, certificate) in certificates.iter().enumerate() {
            let case =
                format!("{member} on a {endpoint_type} domain name with {}", certificate.case);
            let domain_name = format!("d{index}-{row}.example.com");
            let mut body = create_body(&domain_name, endpoint_type, member, &certificate.arn);
            if *member == "ownershipVerificationCertificateArn" {
                body["regionalCertificateArn"] = json!(serving);
            }
            let answer = create_domain(&gateway, body).await;
            if certificate.admitted() {
                assert_eq!(
                    answer.status, 201,
                    "case {case:?}: expected 201, got {} with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.body[*member].as_str(),
                    Some(certificate.arn.as_str()),
                    "case {case:?}: expected the certificate it was created with echoed, got {:?}",
                    answer.body
                );
                admitted_names.push(domain_name);
            } else {
                assert_failure(&answer, 400, BAD_REQUEST, &case);
                let message = string_member(&answer.body, "message", &case);
                assert_eq!(
                    message, CERTIFICATE_NOT_ISSUED,
                    "case {case:?}: expected the state refusal, got {message:?}"
                );
                refused_names.push(domain_name);
            }
        }
    }
    assert!(
        !refused_names.is_empty() && !admitted_names.is_empty(),
        "case \"the create table\": expected both verdicts to be exercised, got {} refused and \
         {} admitted",
        refused_names.len(),
        admitted_names.len()
    );

    // A refused create left NOTHING behind, and every admitted one is there to be read.
    let listed = listed_names(&gateway, "after the creates").await;
    for refused in &refused_names {
        assert!(
            !listed.contains(refused),
            "case \"after the creates\": expected no record for the refused {refused:?}, got \
             {listed:?}"
        );
    }
    for name in &admitted_names {
        assert!(
            listed.contains(name),
            "case \"after the creates\": expected the admitted {name:?} among {listed:?}"
        );
    }

    // The gate weighs the certificate's STATE, not the name or the shape of the request: the
    // very name a pending certificate was refused under is created once the certificate it
    // names is one the manager has issued, and reads back carrying it.
    let issued = certificates
        .iter()
        .find(|certificate| certificate.status.as_deref() == Some(ISSUED))
        .expect("the suite imports one issued certificate");
    let retried = refused_names
        .first()
        .expect("the suite refuses at least one create")
        .clone();
    let case = "a refused create retried with an issued certificate";
    let answer = create_domain(
        &gateway,
        create_body(&retried, "REGIONAL", "regionalCertificateArn", &issued.arn),
    )
    .await;
    assert_eq!(
        answer.status, 201,
        "case {case:?}: expected 201, got {} with {:?}",
        answer.status, answer.body
    );
    let read = get_domain(&gateway, &retried, case).await;
    assert_eq!(
        read["regionalCertificateArn"].as_str(),
        Some(issued.arn.as_str()),
        "case {case:?}: expected the stored record to carry the issued certificate, got {read:?}"
    );
}

#[tokio::test]
async fn the_state_refusal_does_not_displace_the_unreadable_arn_refusal() {
    let gateway = gateway_with_acm_and_s3();

    // A value that is no certificate ARN at all is refused for its FORM, before any state is
    // asked about — so the two refusals stay distinguishable by their message. The rows span
    // the ways a value is not the ARN of a certificate.
    let rows: &[(&str, &str)] = &[
        ("a value that is no ARN at all", "not-an-arn"),
        (
            "an ARN of another service",
            "arn:aws:iam::000000000000:role/certificate",
        ),
        (
            "an ARN of another certificate-manager resource",
            "arn:aws:acm:us-east-1:000000000000:certificate-authority/11111111",
        ),
    ];
    for (index, (case, value)) in rows.iter().enumerate() {
        let domain_name = format!("form{index}.example.com");
        let answer = create_domain(
            &gateway,
            create_body(&domain_name, "REGIONAL", "regionalCertificateArn", value),
        )
        .await;
        assert_failure(&answer, 400, BAD_REQUEST, case);
        let message = string_member(&answer.body, "message", case);
        assert_eq!(
            message, INVALID_ARN,
            "case {case:?}: expected the unreadable-ARN refusal, got {message:?}"
        );
        let listed = listed_names(&gateway, case).await;
        assert!(
            !listed.contains(&domain_name),
            "case {case:?}: expected no record for a refused create, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn a_patch_may_only_configure_a_certificate_the_manager_has_issued() {
    let gateway = gateway_with_acm_and_s3();
    let certificates = certificates(&gateway).await;
    let issued = certificates
        .iter()
        .find(|certificate| certificate.status.as_deref() == Some(ISSUED))
        .expect("the suite imports one issued certificate")
        .arn
        .clone();
    let rotated =
        import_certificate(&gateway, "a second import", &leaf("rotated.example.com")).await;

    let domain_name = "patched.example.com";
    let created = create_domain(
        &gateway,
        create_body(domain_name, "REGIONAL", "regionalCertificateArn", &issued),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case \"the record every patch is applied to\": expected 201, got {} with {:?}",
        created.status, created.body
    );

    // What the record carries after every call: the create set the regional certificate and
    // nothing else, and only an ADMITTED patch moves an entry — so a refused patch is checked
    // against the record as the admitted calls left it rather than against whatever is there.
    let mut held: BTreeMap<&str, Option<String>> = CERTIFICATE_MEMBERS
        .iter()
        .map(|member| (*member, None))
        .collect();
    held.insert("regionalCertificateArn", Some(issued.clone()));

    // The rows: every member a patch can point at a certificate, crossed with the states a
    // certificate can be in.
    for certificate in &certificates {
        for member in CERTIFICATE_MEMBERS {
            let case = format!("a patch of {member} to {}", certificate.case);
            let answer =
                patch_domain(&gateway, domain_name, &format!("/{member}"), &certificate.arn).await;
            if certificate.admitted() {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: expected 200, got {} with {:?}",
                    answer.status, answer.body
                );
                assert_eq!(
                    answer.body[*member].as_str(),
                    Some(certificate.arn.as_str()),
                    "case {case:?}: expected the patched certificate reported, got {:?}",
                    answer.body
                );
                held.insert(member, Some(certificate.arn.clone()));
            } else {
                assert_failure(&answer, 400, BAD_REQUEST, &case);
                let message = string_member(&answer.body, "message", &case);
                assert_eq!(
                    message, CERTIFICATE_NOT_ISSUED,
                    "case {case:?}: expected the state refusal, got {message:?}"
                );
            }
            // Whatever the verdict, what a later caller reads is what the ADMITTED calls left:
            // a refused patch changed nothing, on the member it named or on any other.
            let read = get_domain(&gateway, domain_name, &case).await;
            for (checked, expected) in &held {
                assert_eq!(
                    read[*checked].as_str(),
                    expected.as_deref(),
                    "case {case:?}: expected the stored {checked} to be {expected:?}, got {read:?}"
                );
            }
        }
    }

    // A rotation onto ANOTHER issued certificate goes through and is read back, so the gate is
    // state-specific rather than a blanket refusal of a certificate change.
    let case = "a rotation onto a second issued certificate";
    let answer = patch_domain(&gateway, domain_name, "/regionalCertificateArn", &rotated).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    let read = get_domain(&gateway, domain_name, case).await;
    assert_eq!(
        read["regionalCertificateArn"].as_str(),
        Some(rotated.as_str()),
        "case {case:?}: expected the rotated certificate stored, got {read:?}"
    );

    // A patch that touches something ELSE is weighed against no certificate at all, so it goes
    // through whatever the record already references.
    let case = "a patch of a member that names no certificate";
    let answer = patch_domain(
        &gateway,
        domain_name,
        "/regionalCertificateName",
        "rotated-name",
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    let read = get_domain(&gateway, domain_name, case).await;
    assert_eq!(
        read["regionalCertificateName"].as_str(),
        Some("rotated-name"),
        "case {case:?}: expected the patched name stored, got {read:?}"
    );
}

// ---------------------------------------------------------------------------
// What the certificate's own service is told about the record
// ---------------------------------------------------------------------------

/// `DeleteDomainName` for `domain_name`, asserting the removal was accepted.
async fn delete_domain(gateway: &Gateway, domain_name: &str, case: &str) {
    let answer = gateway
        .call("DELETE", &format!("/domainnames/{domain_name}"), None)
        .await;
    assert_eq!(
        answer.status, 202,
        "case {case:?}: expected DeleteDomainName to answer 202, got {} with {:?}",
        answer.status, answer.body
    );
}

/// The resources Certificate Manager reports as using `certificate_arn`
/// (`CertificateDetail$InUseBy`), read through its own `DescribeCertificate`.
async fn in_use_by(gateway: &Gateway, certificate_arn: &str, case: &str) -> Vec<String> {
    let answered = acm(
        gateway,
        "DescribeCertificate",
        json!({ "CertificateArn": certificate_arn }),
        case,
    )
    .await;
    answered["Certificate"]["InUseBy"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected an InUseBy list, got {answered:?}"))
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected an ARN, got {entry:?}"))
                .to_string()
        })
        .collect()
}

/// The resources using `certificate_arn` once `resource_arn` is no longer among them.
///
/// A reference the owner gives up is given up once the work that takes it away has run, so
/// this LOOKS AGAIN until the reference is gone — never waits a span out — and the deadline is
/// generous enough that only a defect reaches it.
async fn released_from(
    gateway: &Gateway,
    certificate_arn: &str,
    resource_arn: &str,
    case: &str,
) -> Vec<String> {
    let deadline = Instant::now() + STATUS_DEADLINE;
    loop {
        let using = in_use_by(gateway, certificate_arn, case).await;
        if !using.iter().any(|arn| arn == resource_arn) {
            return using;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {resource_arn} to stop using the certificate, still \
             {using:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

#[tokio::test]
async fn the_certificate_manager_is_told_which_certificates_a_domain_name_uses() {
    let gateway = gateway_with_acm_and_s3();
    let presented = import_certificate(
        &gateway,
        "the certificate the endpoint presents",
        &leaf("presented.example.com"),
    )
    .await;
    let ownership = import_certificate(
        &gateway,
        "the certificate that validates ownership",
        &leaf("ownership.example.com"),
    )
    .await;
    let rotated = import_certificate(
        &gateway,
        "the certificate the record is rotated onto",
        &leaf("rotated.example.com"),
    )
    .await;

    let domain_name = "in-use.example.com";
    let mut body = create_body(
        domain_name,
        "REGIONAL",
        "regionalCertificateArn",
        &presented,
    );
    body["ownershipVerificationCertificateArn"] = json!(ownership);
    let created = create_domain(&gateway, body).await;
    assert_eq!(
        created.status, 201,
        "case \"the record under test\": expected 201, got {} with {:?}",
        created.status, created.body
    );
    // The ARN the SERVICE answered with, so what is asserted on is the resource's own
    // identity rather than one spelled here.
    let resource_arn = string_member(&created.body, "domainNameArn", "the record under test");

    // Every certificate the record references is reported as in use BY that record.
    for (case, certificate_arn) in [
        ("the certificate the endpoint presents", &presented),
        ("the certificate that validates ownership", &ownership),
    ] {
        let using = in_use_by(&gateway, certificate_arn, case).await;
        assert_eq!(
            using,
            vec![resource_arn.clone()],
            "case {case:?}: expected the domain name to be reported as using it, got {using:?}"
        );
    }
    // A certificate nothing references is used by nothing.
    let using = in_use_by(&gateway, &rotated, "a certificate no record references").await;
    assert!(
        using.is_empty(),
        "case \"a certificate no record references\": expected nothing using it, got {using:?}"
    );

    // And the owner PROTECTS what is in use: the certificate cannot be deleted out from under
    // the domain name presenting it.
    let case = "a certificate the domain name is using";
    let (status, refused) = acm_call(
        &gateway,
        "DeleteCertificate",
        json!({ "CertificateArn": presented }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the delete to be refused with 400, got {status} with {refused:?}"
    );
    let kind = refused["__type"].as_str().unwrap_or_default();
    assert!(
        kind.contains("ResourceInUse"),
        "case {case:?}: expected the in-use refusal, got {kind:?} with {refused:?}"
    );

    // A reference the record gives up by being POINTED ELSEWHERE is released once the endpoint
    // still presenting it has migrated; the certificate it was pointed at is in use at once.
    let case = "a reference given up by a patch";
    let patched = patch_domain(&gateway, domain_name, "/regionalCertificateArn", &rotated).await;
    assert_eq!(
        patched.status, 200,
        "case {case:?}: expected 200, got {} with {:?}",
        patched.status, patched.body
    );
    let using = in_use_by(&gateway, &rotated, case).await;
    assert_eq!(
        using,
        vec![resource_arn.clone()],
        "case {case:?}: expected the record to be using the certificate it was pointed at, got \
         {using:?}"
    );
    let released = released_from(&gateway, &presented, &resource_arn, case).await;
    assert!(
        released.is_empty(),
        "case {case:?}: expected the given-up certificate to be free, got {released:?}"
    );

    // A reference the record gives up by being REMOVED is released the same way, and then the
    // owner lets the certificate go.
    let case = "a reference given up by the delete";
    delete_domain(&gateway, domain_name, case).await;
    for certificate_arn in [&rotated, &ownership] {
        let released = released_from(&gateway, certificate_arn, &resource_arn, case).await;
        assert!(
            released.is_empty(),
            "case {case:?}: expected {certificate_arn} to be free, got {released:?}"
        );
    }
    let (status, answered) = acm_call(
        &gateway,
        "DeleteCertificate",
        json!({ "CertificateArn": rotated }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the certificate to be deletable once nothing uses it, got \
         {status} with {answered:?}"
    );
}
