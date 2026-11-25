// SPDX-License-Identifier: CC0-1.0

//! WHERE the trust chain of the certificate a custom domain is created with ENDS.
//!
//! The HTTPS handshake a custom domain answers on the name it was created under is terminated
//! with the certificate the create names, and a client completes that handshake only when it
//! can build a path from that certificate up to an authority its own trust store carries — so
//! the rule asserted here is: `CreateDomainName` takes a certificate only when its chain ends
//! in a PUBLIC trust anchor, and refuses one anchored anywhere else with the modelled 400
//! `BadRequestException` and AppSync's fixed sentence for a certificate it will not serve
//! with, however sound that certificate is and whatever names it covers. A refused pair leaves
//! no domain behind: the per-domain read answers the modelled 404 and the account's listing
//! holds only the names that were served.
//!
//! A certificate authority a caller generates locally is in no client's trust store, so every
//! certificate under one is refused. The rows span the ways such a certificate reaches an
//! account and is presented:
//!
//! * the service that HOLDS it — imported into Certificate Manager, or uploaded to Identity
//!   and Access Management as a server certificate, the two forms `certificateArn` admits
//!   (`^arn:[a-z-]*:(acm|iam):…:(certificate|server-certificate)/…$`,
//!   `smithy_query(query="operation",operation="CreateDomainName")`);
//! * how DEEP the chain to that authority is — a leaf the authority signed directly, and one
//!   an intermediate authority signed that the authority signed in turn, with both offered;
//! * whether the authorities are OFFERED with the certificate at all;
//! * and the FORM of the name the domain is created under — the certificate's own name, one of
//!   its subject alternative names, and a name one label under a wildcard.
//!
//! The two rows that ARE served are certificates Certificate Manager issued from its own
//! public authority, so no row rests on a blanket refusal: the same operation, in the same
//! store, serves the name whose certificate is publicly anchored and refuses the one whose
//! certificate is not.
//!
//! Every certificate is real material the owning service really holds, created through that
//! service's own operation over the same engine, and each row's premise is read back through
//! that service's own read before the create is tried. The suite that varies the PROVENANCE
//! Certificate Manager reports is `crate::domain_certificate_provenance`; this one varies
//! where the chain ends.

use std::sync::Arc;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService;

use crate::domain_certificate_issuing_authority::{reported_server_certificate, uploaded_material};
use crate::domain_certificate_provenance::{
    acm_call, appsync_call, certificate_arn, create_domain, message, read_domain, reported_type,
    requested, Answer, CERTIFICATE_INVALID, DOMAIN_NAMES_PATH, DOMAIN_NOT_FOUND_ON_READ,
};

/// The `Type` Certificate Manager reports for a certificate a client brought in, and for one
/// it issued itself. Source: `smithy_query(query="shape",shape="CertificateType")` of the acm
/// model.
const TYPE_IMPORTED: &str = "IMPORTED";
const TYPE_AMAZON_ISSUED: &str = "AMAZON_ISSUED";

/// The names the rows create their domains under: one name space per row, so no row's domain
/// can be the one another row created.
const ROOT_SIGNED_NAME: &str = "root-signed.appsync-trust-test.com";
const INTERMEDIATE_SIGNED_NAME: &str = "intermediate-signed.appsync-trust-test.com";
const NO_CHAIN_NAME: &str = "no-chain.appsync-trust-test.com";
const WILDCARD_APEX: &str = "wild.appsync-trust-test.com";
const UPLOADED_SUBJECT_NAME: &str = "uploaded-subject.appsync-trust-test.com";
const UPLOADED_ALTERNATIVE_NAME: &str = "uploaded-alternative.appsync-trust-test.com";
const UPLOADED_DEEP_NAME: &str = "uploaded-deep.appsync-trust-test.com";
const PUBLIC_NAME: &str = "public.appsync-trust-test.com";
const PUBLIC_WILDCARD_APEX: &str = "public-wild.appsync-trust-test.com";

/// The server certificate names the account's uploads are held under, each a
/// `serverCertificateNameType` value that is also admissible inside `certificateArn`'s own
/// pattern.
const UPLOADED_CERTIFICATE_NAME: &str = "appsync-trust-uploaded";
const UPLOADED_DEEP_CERTIFICATE_NAME: &str = "appsync-trust-uploaded-deep";

/// This service beside the two that hold certificates for an account, over ONE engine, so each
/// certificate a create names is the certificate the owning service really holds.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_acm_certificatemanager::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One certificate authority of this suite's own making: the certificate it is known by and
/// the key it signs with. Locally generated, so no client's trust store carries it.
struct Authority {
    certificate: rcgen::Certificate,
    key: rcgen::KeyPair,
}

impl Authority {
    /// A self-signed root authority under `common_name`.
    fn root(common_name: &str) -> Self {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("a key pair");
        let certificate = params_signed(authority_params(common_name), &key, None);
        Self { certificate, key }
    }

    /// An intermediate authority under `common_name` that THIS authority signed, so a leaf
    /// under it reaches the root through one more certificate.
    fn subordinate(&self, common_name: &str) -> Self {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("a key pair");
        let certificate = params_signed(authority_params(common_name), &key, Some(self));
        Self { certificate, key }
    }

    /// A real certificate covering `names` that this authority signed, as the PEM pair the
    /// import and the upload both take: the leaf and the private key that matches it. The
    /// first name is also the subject common name.
    fn signs(&self, names: &[&str]) -> (String, String) {
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
        let certificate = params_signed(params, &key, Some(self));
        (certificate.pem(), key.serialize_pem())
    }

    /// This authority's own certificate, as a chain carries it.
    fn pem(&self) -> String {
        self.certificate.pem()
    }
}

/// The parameters of a certificate authority under `common_name`: a certificate that may sign
/// others, inside a validity window both services admit material in.
fn authority_params(common_name: &str) -> rcgen::CertificateParams {
    let mut params =
        rcgen::CertificateParams::new(Vec::<String>::new()).expect("certificate parameters");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let mut subject = rcgen::DistinguishedName::new();
    subject.push(rcgen::DnType::CommonName, common_name);
    params.distinguished_name = subject;
    params
}

/// Sign `params` with `key`, by `issuer` when there is one and by the key itself when there is
/// none, inside the same validity window.
fn params_signed(
    mut params: rcgen::CertificateParams,
    key: &rcgen::KeyPair,
    issuer: Option<&Authority>,
) -> rcgen::Certificate {
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2100, 1, 1);
    match issuer {
        Some(issuer) => params
            .signed_by(key, &issuer.certificate, &issuer.key)
            .expect("a certificate the authority signed"),
        None => params.self_signed(key).expect("a self-signed certificate"),
    }
}

/// Import `material` into Certificate Manager with `chain` as the authorities offered with it,
/// and answer the ARN it was stored under.
async fn imported_material(
    router: &MultiServiceRouter,
    case: &str,
    material: (String, String),
    chain: Option<String>,
) -> String {
    let (certificate, private_key) = material;
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
        "case {case}: expected ImportCertificate to accept the material with 200, got {} with \
         {:?}",
        answer.status, answer.body
    );
    certificate_arn(&answer, case)
}

/// The custom domain names the account's listing reports.
async fn listed_domains(router: &MultiServiceRouter) -> Vec<String> {
    let answer = appsync_call(router, "GET", DOMAIN_NAMES_PATH, None).await;
    assert_eq!(
        answer.status, 200,
        "case ListDomainNames: expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body["domainNameConfigs"]
        .as_array()
        .map(|configs| {
            configs
                .iter()
                .filter_map(|config| config["domainName"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Which certificate a row names.
#[derive(Debug, Clone, Copy)]
enum Certificate {
    /// Imported into Certificate Manager: a leaf the local root authority signed, offered with
    /// that authority's certificate.
    ImportedRootSigned,
    /// Imported: a leaf a local INTERMEDIATE authority signed, offered with the intermediate
    /// and the root.
    ImportedIntermediateSigned,
    /// Imported: a leaf the local root authority signed, offered with NO chain at all.
    ImportedWithoutChain,
    /// Imported: a WILDCARD leaf the local root authority signed, offered with that authority.
    ImportedWildcard,
    /// Uploaded to Identity and Access Management: a leaf the local root authority signed,
    /// carrying a subject alternative name beside its own, offered with that authority.
    UploadedRootSigned,
    /// Uploaded: a leaf a local intermediate authority signed, offered with the intermediate
    /// and the root.
    UploadedIntermediateSigned,
    /// Requested from Certificate Manager's own public authority.
    PubliclyIssued,
    /// Requested from that authority for `*.<apex>`.
    PubliclyIssuedWildcard,
}

/// Every certificate the rows name, as the ARN each is addressed by.
struct Certificates {
    imported_root_signed: String,
    imported_intermediate_signed: String,
    imported_without_chain: String,
    imported_wildcard: String,
    uploaded_root_signed: String,
    uploaded_intermediate_signed: String,
    publicly_issued: String,
    publicly_issued_wildcard: String,
}

impl Certificates {
    /// Create every certificate the table names through the owning service's own operation.
    async fn created(router: &MultiServiceRouter) -> Self {
        let root = Authority::root("appsync-trust-test local root authority");
        let intermediate = root.subordinate("appsync-trust-test local intermediate authority");
        let deep_chain = format!("{}{}", intermediate.pem(), root.pem());
        Self {
            imported_root_signed: imported_material(
                router,
                "import a root-signed certificate",
                root.signs(&[ROOT_SIGNED_NAME]),
                Some(root.pem()),
            )
            .await,
            imported_intermediate_signed: imported_material(
                router,
                "import an intermediate-signed certificate",
                intermediate.signs(&[INTERMEDIATE_SIGNED_NAME]),
                Some(deep_chain.clone()),
            )
            .await,
            imported_without_chain: imported_material(
                router,
                "import a root-signed certificate with no chain",
                root.signs(&[NO_CHAIN_NAME]),
                None,
            )
            .await,
            imported_wildcard: imported_material(
                router,
                "import a root-signed wildcard certificate",
                root.signs(&[&format!("*.{WILDCARD_APEX}")]),
                Some(root.pem()),
            )
            .await,
            uploaded_root_signed: uploaded_material(
                router,
                UPLOADED_CERTIFICATE_NAME,
                root.signs(&[UPLOADED_SUBJECT_NAME, UPLOADED_ALTERNATIVE_NAME]),
                Some(root.pem()),
            )
            .await,
            uploaded_intermediate_signed: uploaded_material(
                router,
                UPLOADED_DEEP_CERTIFICATE_NAME,
                intermediate.signs(&[UPLOADED_DEEP_NAME]),
                Some(deep_chain),
            )
            .await,
            publicly_issued: requested(router, &[PUBLIC_NAME]).await,
            publicly_issued_wildcard: requested(
                router,
                &[&format!("*.{PUBLIC_WILDCARD_APEX}")],
            )
            .await,
        }
    }

    /// The ARN `certificate` is addressed by.
    fn arn(&self, certificate: Certificate) -> &str {
        match certificate {
            Certificate::ImportedRootSigned => &self.imported_root_signed,
            Certificate::ImportedIntermediateSigned => &self.imported_intermediate_signed,
            Certificate::ImportedWithoutChain => &self.imported_without_chain,
            Certificate::ImportedWildcard => &self.imported_wildcard,
            Certificate::UploadedRootSigned => &self.uploaded_root_signed,
            Certificate::UploadedIntermediateSigned => &self.uploaded_intermediate_signed,
            Certificate::PubliclyIssued => &self.publicly_issued,
            Certificate::PubliclyIssuedWildcard => &self.publicly_issued_wildcard,
        }
    }
}

/// The server certificate name a row's certificate is held under in Identity and Access
/// Management, for a row whose certificate lives there at all.
fn server_certificate_name(certificate: Certificate) -> Option<&'static str> {
    match certificate {
        Certificate::UploadedRootSigned => Some(UPLOADED_CERTIFICATE_NAME),
        Certificate::UploadedIntermediateSigned => Some(UPLOADED_DEEP_CERTIFICATE_NAME),
        _ => None,
    }
}

/// One row: the name the create asks for, the certificate it names, the `Type` Certificate
/// Manager must report for it (`None` for one that service holds nothing of), and whether the
/// create is accepted.
struct Row {
    label: &'static str,
    domain_name: String,
    certificate: Certificate,
    managed_type: Option<&'static str>,
    accepted: bool,
}

/// The table: one row per way a certificate under a locally generated authority reaches an
/// account and is presented, plus the two publicly anchored controls.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "an imported certificate a local root authority signed, offered with it",
            domain_name: ROOT_SIGNED_NAME.to_string(),
            certificate: Certificate::ImportedRootSigned,
            managed_type: Some(TYPE_IMPORTED),
            accepted: false,
        },
        Row {
            label: "an imported certificate a local intermediate authority signed, offered \
                    with the whole chain",
            domain_name: INTERMEDIATE_SIGNED_NAME.to_string(),
            certificate: Certificate::ImportedIntermediateSigned,
            managed_type: Some(TYPE_IMPORTED),
            accepted: false,
        },
        Row {
            label: "an imported certificate a local root authority signed, offered with no \
                    chain",
            domain_name: NO_CHAIN_NAME.to_string(),
            certificate: Certificate::ImportedWithoutChain,
            managed_type: Some(TYPE_IMPORTED),
            accepted: false,
        },
        Row {
            label: "one label under an imported wildcard certificate a local root authority \
                    signed",
            domain_name: format!("api.{WILDCARD_APEX}"),
            certificate: Certificate::ImportedWildcard,
            managed_type: Some(TYPE_IMPORTED),
            accepted: false,
        },
        Row {
            label: "a subject alternative name of an uploaded server certificate a local root \
                    authority signed",
            domain_name: UPLOADED_ALTERNATIVE_NAME.to_string(),
            certificate: Certificate::UploadedRootSigned,
            managed_type: None,
            accepted: false,
        },
        Row {
            label: "an uploaded server certificate a local intermediate authority signed, \
                    offered with the whole chain",
            domain_name: UPLOADED_DEEP_NAME.to_string(),
            certificate: Certificate::UploadedIntermediateSigned,
            managed_type: None,
            accepted: false,
        },
        Row {
            label: "a certificate Certificate Manager issued from its public authority",
            domain_name: PUBLIC_NAME.to_string(),
            certificate: Certificate::PubliclyIssued,
            managed_type: Some(TYPE_AMAZON_ISSUED),
            accepted: true,
        },
        Row {
            label: "one label under a publicly issued wildcard certificate",
            domain_name: format!("api.{PUBLIC_WILDCARD_APEX}"),
            certificate: Certificate::PubliclyIssuedWildcard,
            managed_type: Some(TYPE_AMAZON_ISSUED),
            accepted: true,
        },
    ]
}

/// RULE: a custom domain is created only with a certificate whose trust chain ends in an
/// authority a client's own trust store carries. A certificate under a locally generated
/// authority is refused with the modelled 400 `BadRequestException` and AppSync's fixed
/// sentence — however deep its chain, whether or not the authorities are offered with it, in
/// whichever of the two services holds it, and whatever name form it covers — and leaves no
/// domain behind, while one Certificate Manager issued from its own public authority serves
/// the name it covers.
#[tokio::test]
async fn create_domain_name_requires_a_publicly_anchored_certificate() {
    let router = router();
    let certificates = Certificates::created(&router).await;

    for row in rows() {
        let arn = certificates.arn(row.certificate);

        // The row's premise, read off the service that HOLDS the certificate rather than
        // assumed here: this is the certificate the decision is taken on.
        let managed_type = reported_type(&router, arn).await;
        assert_eq!(
            managed_type.as_deref(),
            row.managed_type,
            "case {}: expected Certificate Manager to report type {:?} for {arn}, got \
             {managed_type:?}",
            row.label,
            row.managed_type
        );
        if let Some(name) = server_certificate_name(row.certificate) {
            let held = reported_server_certificate(&router, name).await;
            assert_eq!(
                held.as_deref(),
                Some(arn),
                "case {}: expected Identity and Access Management to hold {arn} under {name}, \
                 got {held:?}",
                row.label
            );
        }

        let answer = create_domain(&router, &row.domain_name, arn).await;
        if row.accepted {
            assert_accepted(&router, &row, arn, &answer).await;
            continue;
        }
        assert_refused(&router, &row, &answer).await;
    }

    // The account's INVENTORY agrees with the per-domain reads: a refused create left nothing
    // to list.
    let listed = listed_domains(&router).await;
    let mut expected: Vec<String> = rows()
        .into_iter()
        .filter(|row| row.accepted)
        .map(|row| row.domain_name)
        .collect();
    expected.sort();
    let mut reported = listed;
    reported.sort();
    assert_eq!(
        reported, expected,
        "case ListDomainNames: expected exactly the publicly anchored domains, got {reported:?}"
    );
}

/// RULE, read the other way round: nothing of a refused create is remembered, and the chain is
/// read at the moment of the create — so the SAME name refused under a certificate a local
/// authority signed is served once the account holds a publicly anchored certificate for it.
#[tokio::test]
async fn the_chain_is_read_at_the_create() {
    let router = router();
    let domain = "rotated.appsync-trust-test.com";
    let root = Authority::root("appsync-trust-test rotation authority");

    let brought_in = imported_material(
        &router,
        "import a root-signed certificate for the rotated name",
        root.signs(&[domain]),
        Some(root.pem()),
    )
    .await;
    let refused = create_domain(&router, domain, &brought_in).await;
    assert_eq!(
        (
            refused.status,
            refused.error_type.as_str(),
            message(&refused)
        ),
        (400, "BadRequestException", CERTIFICATE_INVALID),
        "case \"a certificate a local authority signed\": expected the modelled 400 refusal, \
         got {} {:?} with {:?}",
        refused.status,
        refused.error_type,
        refused.body
    );
    let read = read_domain(&router, domain).await;
    assert_eq!(
        (read.status, message(&read)),
        (404, DOMAIN_NOT_FOUND_ON_READ),
        "case \"a certificate a local authority signed\": expected the refused create to leave \
         no domain behind, got {} with {:?}",
        read.status,
        read.body
    );

    let issued = requested(&router, &[domain]).await;
    let served = create_domain(&router, domain, &issued).await;
    assert_eq!(
        served.status, 200,
        "case \"the same name under a publicly issued certificate\": expected 200, got {} with \
         {:?}",
        served.status, served.body
    );
    assert_eq!(
        served.body["domainNameConfig"]["certificateArn"].as_str(),
        Some(issued.as_str()),
        "case \"the same name under a publicly issued certificate\": expected the domain to \
         report the issued certificate, got {:?}",
        served.body
    );
}

/// The create was accepted, and the domain it stored reads back with the certificate it was
/// created with.
async fn assert_accepted(
    router: &MultiServiceRouter,
    row: &Row,
    arn: &str,
    answer: &Answer,
) {
    assert_eq!(
        answer.status, 200,
        "case {}: expected the create to be accepted with 200, got {} with {:?}",
        row.label, answer.status, answer.body
    );
    assert_eq!(
        answer.body["domainNameConfig"]["certificateArn"].as_str(),
        Some(arn),
        "case {}: expected the created domain to report the certificate it was created with, \
         got {:?}",
        row.label,
        answer.body
    );
    let read = read_domain(router, &row.domain_name).await;
    assert_eq!(
        read.status, 200,
        "case {}: expected GetDomainName to report the created domain with 200, got {} with \
         {:?}",
        row.label, read.status, read.body
    );
    assert_eq!(
        read.body["domainNameConfig"]["domainName"].as_str(),
        Some(row.domain_name.as_str()),
        "case {}: expected GetDomainName to report the created name, got {:?}",
        row.label,
        read.body
    );
}

/// The create was refused with the modelled status, error and sentence, and left no domain
/// behind — which the per-domain read is what proves.
async fn assert_refused(router: &MultiServiceRouter, row: &Row, answer: &Answer) {
    assert_eq!(
        (answer.status, answer.error_type.as_str()),
        (400, "BadRequestException"),
        "case {}: expected the modelled 400 BadRequestException, got {} {:?} with {:?}",
        row.label,
        answer.status,
        answer.error_type,
        answer.body
    );
    assert_eq!(
        message(answer),
        CERTIFICATE_INVALID,
        "case {}: expected AppSync's certificate refusal, got {:?}",
        row.label,
        answer.body
    );
    let read = read_domain(router, &row.domain_name).await;
    assert_eq!(
        read.status, 404,
        "case {}: expected the refused create to leave no domain behind (404), got {} with {:?}",
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
