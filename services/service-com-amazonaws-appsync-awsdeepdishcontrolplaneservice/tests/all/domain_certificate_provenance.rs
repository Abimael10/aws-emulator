// SPDX-License-Identifier: CC0-1.0

//! WHERE the certificate a custom domain is created with came from.
//!
//! A custom domain is served by a distribution AppSync owns, and such a distribution
//! presents only certificates Certificate Manager ISSUED — so the rule asserted here is:
//! `CreateDomainName` is accepted only when the certificate the account holds at the ARN it
//! names is one that service issued, and a certificate a client brought into the account
//! itself is refused with 400 `BadRequestException` and AppSync's fixed sentence for a
//! certificate it will not serve with, whatever names that certificate covers and whatever
//! chain it was brought in with, while nothing publicly trusted anchors that chain. A refused
//! pair leaves no domain behind.
//!
//! The rule generalises over the PROVENANCE of the certificate (requested from Certificate
//! Manager, imported self-signed, imported with another authority's certificate as its
//! chain) and, independently, over the FORM of the name the domain is created under (the
//! certificate's own name, one of its subject alternative names, a name one label under a
//! wildcard) — so the table's rows differ along both: a name form that is accepted for an
//! issued certificate is refused for an imported one.
//!
//! Every certificate is one Certificate Manager really holds, created through that
//! service's own `RequestCertificate` or `ImportCertificate` over the same engine, and each
//! row's premise is read back with that service's own `DescribeCertificate`: the `Type` it
//! reports for the certificate is asserted before the create is tried, so no row rests on a
//! provenance this file assumed. The two ARNs that resolve to NOTHING — a well-formed
//! certificate ARN of this account and an Identity and Access Management server certificate
//! ARN the account holds nothing under — pin the other half of the rule: nothing is refused
//! on evidence that is not there. Which certificates the account holds in that OTHER service
//! is `crate::domain_certificate_issuing_authority`'s subject.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region a custom domain's certificate is read from, which is the Region every
/// certificate here is created in. Source: the service's own `state::CERTIFICATE_REGION`.
pub const REGION: &str = "us-east-1";

/// The account id every emulated ARN carries.
pub const ACCOUNT: &str = "000000000000";

/// AppSync's fixed sentence for a certificate it will not serve a custom domain with:
/// recorded behavior, interpolating no value of the request.
pub const CERTIFICATE_INVALID: &str = "Certificate is invalid.";

/// AppSync's fixed text for a custom domain this account does not hold, on the operations
/// that READ one — what a refused create leaves behind.
pub const DOMAIN_NOT_FOUND_ON_READ: &str = "Domain name not found";

/// `CreateDomainName`'s path.
pub const DOMAIN_NAMES_PATH: &str = "/v1/domainnames";

/// The two values `CertificateDetail$Type` carries for a certificate this account holds:
/// one Certificate Manager issued, and one a client imported.
/// Source: `smithy_query(query="shape", shape="CertificateType")` of the acm model.
const TYPE_AMAZON_ISSUED: &str = "AMAZON_ISSUED";
const TYPE_IMPORTED: &str = "IMPORTED";

/// One answer from the emulator: the parts a case branches on.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: u16,
    /// The `x-amzn-errortype` header the error shape is resolved from.
    pub error_type: String,
    pub body: Value,
}

/// A router with this service and Certificate Manager mounted over ONE engine, so the
/// certificate a create names is the certificate that service holds.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_acm_certificatemanager::EmulatedService::new(engine),
    );
    router
}

/// Read one answer off the router.
pub async fn dispatch(router: &MultiServiceRouter, request: http::Request<Bytes>) -> Answer {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer {
        status,
        error_type,
        body,
    }
}

/// Call this service the way an SDK does: restJson1 over its own paths.
pub async fn appsync_call(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(&value).expect("the request body serializes"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://appsync.{REGION}.amazonaws.com{path}"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR/20260101/{REGION}/appsync/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(payload)
        .expect("a well-formed request");
    dispatch(router, request).await
}

/// Call `operation` of the Certificate Manager service mounted on the same router, the way
/// an SDK drives it: `POST /` with `X-Amz-Target: CertificateManager.<Op>`, signed for that
/// service in the Region its certificates live in.
pub async fn acm_call(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("acm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CertificateManager.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR/20260101/{REGION}/acm/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    dispatch(router, request).await
}

/// A real self-signed certificate covering `names`, as the PEM pair `ImportCertificate`
/// takes: the leaf and the private key that matches it. The first name is also the subject
/// common name, which is the name Certificate Manager reports as the certificate's own.
pub fn minted(names: &[&str]) -> (String, String) {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("a key pair");
    let mut params = rcgen::CertificateParams::new(
        names
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<String>>(),
    )
    .expect("certificate parameters");
    let mut subject = rcgen::DistinguishedName::new();
    subject.push(
        rcgen::DnType::CommonName,
        *names.first().expect("a certificate covers at least one name"),
    );
    params.distinguished_name = subject;
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2100, 1, 1);
    let certificate = params
        .self_signed(&key)
        .expect("a self-signed certificate")
        .pem();
    (certificate, key.serialize_pem())
}

/// Request a certificate covering `names` from Certificate Manager, and answer the ARN it
/// was stored under: a certificate that service ISSUES.
pub async fn requested(router: &MultiServiceRouter, names: &[&str]) -> String {
    let (domain_name, alternatives) = names
        .split_first()
        .expect("a certificate covers at least one name");
    let mut request = json!({"DomainName": domain_name, "ValidationMethod": "DNS"});
    if !alternatives.is_empty() {
        request["SubjectAlternativeNames"] = json!(alternatives);
    }
    let answer = acm_call(router, "RequestCertificate", request).await;
    assert_eq!(
        answer.status, 200,
        "case RequestCertificate({names:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    certificate_arn(&answer, &format!("RequestCertificate({names:?})"))
}

/// Import a certificate covering `names` into Certificate Manager, with `chain` as the
/// certificate chain the request carries, and answer the ARN it was stored under: a
/// certificate the CLIENT brought in, whichever authority the chain names.
pub async fn imported(
    router: &MultiServiceRouter,
    names: &[&str],
    chain: Option<String>,
) -> String {
    let (certificate, private_key) = minted(names);
    let mut request = json!({
        "Certificate": ::aws_smithy_types::base64::encode(certificate.as_bytes()),
        "PrivateKey": ::aws_smithy_types::base64::encode(private_key.as_bytes()),
    });
    if let Some(chain) = chain {
        request["CertificateChain"] = json!(::aws_smithy_types::base64::encode(chain.as_bytes()));
    }
    let answer = acm_call(router, "ImportCertificate", request).await;
    assert_eq!(
        answer.status, 200,
        "case ImportCertificate({names:?}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    certificate_arn(&answer, &format!("ImportCertificate({names:?})"))
}

/// The `CertificateArn` a create answered with.
pub fn certificate_arn(answer: &Answer, case: &str) -> String {
    answer.body["CertificateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: expected a CertificateArn, got {:?}", answer.body))
        .to_string()
}

/// The `Type` Certificate Manager reports for the certificate at `arn`, or `None` for an
/// ARN it resolves nothing for.
pub async fn reported_type(router: &MultiServiceRouter, arn: &str) -> Option<String> {
    let read = acm_call(router, "DescribeCertificate", json!({"CertificateArn": arn})).await;
    if read.status != 200 {
        return None;
    }
    read.body["Certificate"]["Type"]
        .as_str()
        .map(str::to_string)
}

/// Which certificate a row names, which is the PROVENANCE half of the table.
#[derive(Debug, Clone, Copy)]
enum CertificateChoice {
    /// One Certificate Manager issued for a single name.
    IssuedOneName,
    /// One Certificate Manager issued carrying a subject alternative name beside its own.
    IssuedTwoNames,
    /// One Certificate Manager issued for `*.<apex>`.
    IssuedWildcard,
    /// One the client imported, self-signed, for a single name.
    ImportedOneName,
    /// One the client imported carrying a subject alternative name beside its own, with
    /// another authority's certificate as its chain — a certificate signed outside Amazon.
    ImportedTwoNamesWithChain,
    /// One the client imported for `*.<apex>`.
    ImportedWildcard,
    /// A well-formed certificate ARN of this account and Region that nothing resolves for.
    Unresolvable,
    /// An Identity and Access Management server certificate ARN, the other form
    /// `certificateArn` admits, which the account holds no server certificate under.
    IamServerCertificate,
}

/// The names the rows are written about. Each provenance gets its own name space, so one
/// row's domain can never be the one another row created.
const ISSUED_ONE: &str = "issued-one.appsync-provenance-test.com";
const ISSUED_SUBJECT: &str = "issued-subject.appsync-provenance-test.com";
const ISSUED_ALTERNATIVE: &str = "issued-alternative.appsync-provenance-test.com";
const ISSUED_WILDCARD_APEX: &str = "issued-wild.appsync-provenance-test.com";
const IMPORTED_ONE: &str = "imported-one.appsync-provenance-test.com";
const IMPORTED_SUBJECT: &str = "imported-subject.appsync-provenance-test.com";
const IMPORTED_ALTERNATIVE: &str = "imported-alternative.appsync-provenance-test.com";
const IMPORTED_WILDCARD_APEX: &str = "imported-wild.appsync-provenance-test.com";

/// A certificate id no request mints (Certificate Manager mints a random one), so the ARN
/// built from it resolves to nothing in the account.
const NIL_CERTIFICATE_ID: &str = "00000000-0000-0000-0000-000000000000";

/// Every certificate the rows name, as the ARN each is addressed by, beside the provenance
/// Certificate Manager reports for it.
struct Certificates {
    issued_one_name: String,
    issued_two_names: String,
    issued_wildcard: String,
    imported_one_name: String,
    imported_two_names: String,
    imported_wildcard: String,
    unresolvable: String,
    server_certificate: String,
}

impl Certificates {
    /// Create every real certificate the table names through Certificate Manager's own
    /// operations, and build the two ARNs that resolve to nothing.
    async fn created(router: &MultiServiceRouter) -> Self {
        let foreign_authority = minted(&["authority.appsync-provenance-test.com"]).0;
        Self {
            issued_one_name: requested(router, &[ISSUED_ONE]).await,
            issued_two_names: requested(router, &[ISSUED_SUBJECT, ISSUED_ALTERNATIVE]).await,
            issued_wildcard: requested(router, &[&format!("*.{ISSUED_WILDCARD_APEX}")]).await,
            imported_one_name: imported(router, &[IMPORTED_ONE], None).await,
            imported_two_names: imported(
                router,
                &[IMPORTED_SUBJECT, IMPORTED_ALTERNATIVE],
                Some(foreign_authority),
            )
            .await,
            imported_wildcard: imported(
                router,
                &[&format!("*.{IMPORTED_WILDCARD_APEX}")],
                None,
            )
            .await,
            unresolvable: format!(
                "arn:aws:acm:{REGION}:{ACCOUNT}:certificate/{NIL_CERTIFICATE_ID}"
            ),
            server_certificate: format!(
                "arn:aws:iam::{ACCOUNT}:server-certificate/appsync_provenance_test"
            ),
        }
    }

    /// The ARN `certificate` names.
    fn arn(&self, certificate: CertificateChoice) -> &str {
        match certificate {
            CertificateChoice::IssuedOneName => &self.issued_one_name,
            CertificateChoice::IssuedTwoNames => &self.issued_two_names,
            CertificateChoice::IssuedWildcard => &self.issued_wildcard,
            CertificateChoice::ImportedOneName => &self.imported_one_name,
            CertificateChoice::ImportedTwoNamesWithChain => &self.imported_two_names,
            CertificateChoice::ImportedWildcard => &self.imported_wildcard,
            CertificateChoice::Unresolvable => &self.unresolvable,
            CertificateChoice::IamServerCertificate => &self.server_certificate,
        }
    }
}

/// One row: the name a create asks for, the certificate it names, the provenance
/// Certificate Manager must report for that certificate (`None` for an ARN it resolves
/// nothing for), and whether the create is accepted.
struct Row {
    label: &'static str,
    domain_name: String,
    certificate: CertificateChoice,
    provenance: Option<&'static str>,
    accepted: bool,
}

/// The table: the same three name forms under each of the two provenances, plus the two
/// ARNs that resolve to nothing.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "an issued certificate's only name",
            domain_name: ISSUED_ONE.to_string(),
            certificate: CertificateChoice::IssuedOneName,
            provenance: Some(TYPE_AMAZON_ISSUED),
            accepted: true,
        },
        Row {
            label: "an issued certificate's subject alternative name",
            domain_name: ISSUED_ALTERNATIVE.to_string(),
            certificate: CertificateChoice::IssuedTwoNames,
            provenance: Some(TYPE_AMAZON_ISSUED),
            accepted: true,
        },
        Row {
            label: "one label under an issued wildcard certificate",
            domain_name: format!("api.{ISSUED_WILDCARD_APEX}"),
            certificate: CertificateChoice::IssuedWildcard,
            provenance: Some(TYPE_AMAZON_ISSUED),
            accepted: true,
        },
        Row {
            label: "an imported self-signed certificate's only name",
            domain_name: IMPORTED_ONE.to_string(),
            certificate: CertificateChoice::ImportedOneName,
            provenance: Some(TYPE_IMPORTED),
            accepted: false,
        },
        Row {
            label: "an imported certificate's subject alternative name, with another \
                    authority's certificate as its chain",
            domain_name: IMPORTED_ALTERNATIVE.to_string(),
            certificate: CertificateChoice::ImportedTwoNamesWithChain,
            provenance: Some(TYPE_IMPORTED),
            accepted: false,
        },
        Row {
            label: "one label under an imported wildcard certificate",
            domain_name: format!("api.{IMPORTED_WILDCARD_APEX}"),
            certificate: CertificateChoice::ImportedWildcard,
            provenance: Some(TYPE_IMPORTED),
            accepted: false,
        },
        Row {
            label: "a certificate ARN the account resolves nothing for",
            domain_name: "unresolvable.appsync-provenance-test.com".to_string(),
            certificate: CertificateChoice::Unresolvable,
            provenance: None,
            accepted: true,
        },
        Row {
            label: "an IAM server certificate ARN the account holds nothing under",
            domain_name: "server-cert.appsync-provenance-test.com".to_string(),
            certificate: CertificateChoice::IamServerCertificate,
            provenance: None,
            accepted: true,
        },
    ]
}

/// `CreateDomainName` for `domain_name` with `certificate_arn`.
pub async fn create_domain(
    router: &MultiServiceRouter,
    domain_name: &str,
    certificate_arn: &str,
) -> Answer {
    appsync_call(
        router,
        "POST",
        DOMAIN_NAMES_PATH,
        Some(json!({"domainName": domain_name, "certificateArn": certificate_arn})),
    )
    .await
}

/// `GetDomainName` for `domain_name`.
pub async fn read_domain(router: &MultiServiceRouter, domain_name: &str) -> Answer {
    appsync_call(
        router,
        "GET",
        &format!("{DOMAIN_NAMES_PATH}/{domain_name}"),
        None,
    )
    .await
}

/// The `message` member of an answer's body, or `""` when it carries none.
pub fn message(answer: &Answer) -> &str {
    answer.body["message"].as_str().unwrap_or_default()
}

/// RULE: a custom domain is created only with a certificate Certificate Manager ISSUED. An
/// imported certificate nothing publicly trusted anchors is refused with the modelled 400 and
/// AppSync's own sentence however well its names cover the domain, an issued one covering the
/// name is accepted, and an ARN the account resolves nothing for is refused on neither ground.
#[tokio::test]
async fn create_domain_name_requires_a_certificate_certificate_manager_issued() {
    let router = router();
    let certificates = Certificates::created(&router).await;

    for row in rows() {
        let arn = certificates.arn(row.certificate);
        // The row's premise, read off the service that HOLDS the certificate rather than
        // assumed here: this is the provenance the decision is taken on.
        let provenance = reported_type(&router, arn).await;
        assert_eq!(
            provenance.as_deref(),
            row.provenance,
            "case {}: expected Certificate Manager to report provenance {:?} for {arn}, got \
             {provenance:?}",
            row.label,
            row.provenance
        );

        let answer = create_domain(&router, &row.domain_name, arn).await;
        if row.accepted {
            assert_eq!(
                answer.status, 200,
                "case {}: expected the create to be accepted with 200, got {} with {:?}",
                row.label, answer.status, answer.body
            );
            assert_eq!(
                answer.body["domainNameConfig"]["certificateArn"].as_str(),
                Some(arn),
                "case {}: expected the created domain to report the certificate it was \
                 created with, got {:?}",
                row.label,
                answer.body
            );
            // Read it back through the service's own reader: what the create reported is
            // what the store holds.
            let read = read_domain(&router, &row.domain_name).await;
            assert_eq!(
                read.status, 200,
                "case {}: expected GetDomainName to report the created domain with 200, got \
                 {} with {:?}",
                row.label, read.status, read.body
            );
            assert_eq!(
                read.body["domainNameConfig"]["domainName"].as_str(),
                Some(row.domain_name.as_str()),
                "case {}: expected GetDomainName to report the created name, got {:?}",
                row.label,
                read.body
            );
            continue;
        }

        assert_eq!(
            answer.status, 400,
            "case {}: expected the modelled 400 refusal, got {} with {:?}",
            row.label, answer.status, answer.body
        );
        assert_eq!(
            answer.error_type, "BadRequestException",
            "case {}: expected BadRequestException, got {:?} with {:?}",
            row.label, answer.error_type, answer.body
        );
        assert_eq!(
            message(&answer),
            CERTIFICATE_INVALID,
            "case {}: expected AppSync's certificate refusal, got {:?}",
            row.label,
            answer.body
        );
        // A refused create writes nothing, so the name is one the account does not hold.
        let read = read_domain(&router, &row.domain_name).await;
        assert_eq!(
            read.status, 404,
            "case {}: expected the refused create to leave no domain behind (404), got {} \
             with {:?}",
            row.label, read.status, read.body
        );
        assert_eq!(
            message(&read),
            DOMAIN_NOT_FOUND_ON_READ,
            "case {}: expected AppSync's absent-domain text, got {:?}",
            row.label,
            read.body
        );
    }
}

/// RULE: the provenance is read at the moment of the create, from the store Certificate
/// Manager holds — so the SAME domain name and the SAME certificate ARN flip between
/// refused and accepted when that service's record under the ARN changes.
///
/// A client that imports a certificate, is refused the domain, and then replaces that
/// certificate under a new ARN with one Certificate Manager issued is served — nothing of
/// the first attempt is remembered.
#[tokio::test]
async fn the_decision_follows_the_certificate_the_account_holds() {
    let router = router();
    let domain = "rotated.appsync-provenance-test.com";

    let brought_in = imported(&router, &[domain], None).await;
    let refused = create_domain(&router, domain, &brought_in).await;
    assert_eq!(
        (refused.status, refused.error_type.as_str()),
        (400, "BadRequestException"),
        "case \"an imported certificate\": expected the modelled 400 refusal, got {} {:?} \
         with {:?}",
        refused.status,
        refused.error_type,
        refused.body
    );

    let issued = requested(&router, &[domain]).await;
    let served = create_domain(&router, domain, &issued).await;
    assert_eq!(
        served.status, 200,
        "case \"the same name under an issued certificate\": expected 200, got {} with {:?}",
        served.status, served.body
    );
    assert_eq!(
        served.body["domainNameConfig"]["certificateArn"].as_str(),
        Some(issued.as_str()),
        "case \"the same name under an issued certificate\": expected the domain to report \
         the issued certificate, got {:?}",
        served.body
    );
}
