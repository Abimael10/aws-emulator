// SPDX-License-Identifier: CC0-1.0

//! WHICH AUTHORITY issued the certificate a custom domain is created with.
//!
//! A custom domain's HTTPS handshake is terminated by the distribution AppSync serves it
//! through, and such a distribution presents only a certificate some authority a client can
//! VERIFY it against issued — so the rule asserted here is: `CreateDomainName` is accepted
//! only for a certificate that carries such an issuance, and one the account holds without it
//! is refused with the modelled 400 `BadRequestException` and AppSync's fixed sentence for a
//! certificate it will not serve with, whatever names that certificate covers. A refused pair
//! leaves no domain behind: the name is absent from the per-domain read and from the account's
//! listing alike.
//!
//! `certificateArn` admits a certificate of EITHER service that holds one for an account
//! (`^arn:[a-z-]*:(acm|iam):…:(certificate|server-certificate)/…$`,
//! `smithy_query(query="operation",operation="CreateDomainName")`), so the rows span every
//! route a certificate reaches an account by, each driven through the owning service's own
//! operation over the same engine:
//!
//! * Certificate Manager's public issuance (`RequestCertificate`) — served;
//! * a certificate a client brought into Certificate Manager itself (`ImportCertificate`) —
//!   refused, when nothing publicly trusted anchors what was brought in;
//! * a certificate a PRIVATE certificate authority issued (`RequestCertificate` naming
//!   `CertificateAuthorityArn`), which no public trust store carries — refused;
//! * a SELF-SIGNED certificate the account uploaded to Identity and Access Management
//!   (`UploadServerCertificate`), the other ARN form the member admits, which has no authority
//!   above it at all — refused;
//! * a certificate the account uploaded there that ANOTHER authority signed — refused too when
//!   that authority is one of this suite's own making, because an authority a caller generates
//!   locally anchors no client's trust store, whether the certificate covers the requested name
//!   or some OTHER name; WHERE a chain has to end is `crate::domain_certificate_public_trust`'s
//!   subject;
//! * and, as the other half of the rule, one ARN of each form that NEITHER service resolves
//!   anything for, which says nothing about the certificate and is refused on no ground.
//!
//! Each row's premise is read back through the owning service's own read —
//! `DescribeCertificate` for the certificates Certificate Manager holds (its `Type` and the
//! `CertificateAuthorityArn` behind it) and `GetServerCertificate` for the ones Identity and
//! Access Management holds — so no row rests on a provenance this file assumed. The name
//! FORMS the same rule is asserted over are `crate::domain_certificate_provenance`'s rows;
//! this suite varies the issuing authority instead.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::json;

use crate::domain_certificate_provenance::{
    acm_call, appsync_call, certificate_arn, create_domain, imported, message, minted, read_domain,
    reported_type, requested, ACCOUNT, CERTIFICATE_INVALID, DOMAIN_NAMES_PATH,
    DOMAIN_NOT_FOUND_ON_READ, REGION,
};
use crate::identity_authorization::{form_encode, xml_element};

use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The `Type` Certificate Manager reports for a certificate it issued, and for one a client
/// imported. Source: `smithy_query(query="shape",shape="CertificateType")` of the acm model.
const TYPE_AMAZON_ISSUED: &str = "AMAZON_ISSUED";
const TYPE_IMPORTED: &str = "IMPORTED";

/// A private certificate authority ARN of the form `CertificateAuthorityArn`'s pattern
/// requires. Source:
/// `smithy_query(query="operation",operation="RequestCertificate")` of the acm model.
const PRIVATE_CA_ARN: &str = "arn:aws:acm-pca:us-east-1:000000000000:certificate-authority/\
                              12345678-1234-1234-1234-123456789012";

/// The IAM API version an awsQuery call names. Source: the iam model's service version.
const IAM_API_VERSION: &str = "2010-05-08";

/// The names the account's uploaded server certificates are held under: a
/// `serverCertificateNameType` value that is also admissible inside `certificateArn`'s own
/// pattern, so the ARN Identity and Access Management mints for each is one a create may name.
const UPLOADED_CERTIFICATE_NAME: &str = "appsync-domain-uploaded";
const UPLOADED_SIGNED_CERTIFICATE_NAME: &str = "appsync-domain-uploaded-signed";
const UPLOADED_ELSEWHERE_CERTIFICATE_NAME: &str = "appsync-domain-uploaded-elsewhere";

/// The common name of the certificate authority this suite mints to sign a leaf with, so an
/// uploaded certificate can carry an issuer that is not its own subject.
const AUTHORITY_COMMON_NAME: &str = "appsync-authority-test signing authority";

/// A server certificate name the account uploads nothing under, so the ARN built from it
/// resolves to nothing.
const UNHELD_CERTIFICATE_NAME: &str = "appsync-domain-never-uploaded";

/// A certificate id Certificate Manager mints for nothing (it mints a random one), so the
/// ARN built from it resolves to nothing in the account.
const NIL_CERTIFICATE_ID: &str = "00000000-0000-0000-0000-000000000000";

/// This service beside the two that hold certificates for an account, over ONE engine, so
/// each certificate a create names is the certificate the owning service really holds.
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

/// One answer from Identity and Access Management: the status and the awsQuery XML body.
struct QueryAnswer {
    status: u16,
    text: String,
}

/// One awsQuery call to Identity and Access Management, the way an SDK drives it: the action
/// and its parameters form-encoded in the body of `POST /`.
async fn iam_call(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, String)],
) -> QueryAnswer {
    let mut body = format!("Action={action}&Version={IAM_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR/20260101/{REGION}/iam/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    QueryAnswer {
        status,
        text: String::from_utf8_lossy(response.body()).to_string(),
    }
}

/// A real certificate covering `names` that ANOTHER authority signed, as the PEM pair
/// `UploadServerCertificate` takes: an authority of this suite's own making signs the leaf, so
/// the uploaded certificate carries an issuer above itself rather than its own subject — an
/// issuer no public trust store carries.
fn minted_by_an_authority(names: &[&str]) -> (String, String) {
    let authority_key =
        rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("a key pair");
    let mut authority =
        rcgen::CertificateParams::new(Vec::<String>::new()).expect("certificate parameters");
    authority.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let mut authority_subject = rcgen::DistinguishedName::new();
    authority_subject.push(rcgen::DnType::CommonName, AUTHORITY_COMMON_NAME);
    authority.distinguished_name = authority_subject;
    let authority_certificate = authority
        .self_signed(&authority_key)
        .expect("a certificate authority");

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
        .signed_by(&key, &authority_certificate, &authority_key)
        .expect("a certificate signed by the authority")
        .pem();
    (certificate, key.serialize_pem())
}

/// Upload the PEM pair `material` to Identity and Access Management under `name`, and answer
/// the ARN that service minted for it: a certificate the account brought in itself.
async fn uploaded(
    router: &MultiServiceRouter,
    name: &str,
    material: (String, String),
) -> String {
    uploaded_material(router, name, material, None).await
}

/// The same upload, with `chain` as the authorities offered with the certificate — which is
/// what a certificate an authority above it signed is presented with.
pub async fn uploaded_material(
    router: &MultiServiceRouter,
    name: &str,
    material: (String, String),
    chain: Option<String>,
) -> String {
    let (certificate, private_key) = material;
    let mut params = vec![
        ("ServerCertificateName", name.to_string()),
        ("CertificateBody", certificate),
        ("PrivateKey", private_key),
    ];
    if let Some(chain) = chain {
        params.push(("CertificateChain", chain));
    }
    let answer = iam_call(router, "UploadServerCertificate", &params).await;
    assert_eq!(
        answer.status, 200,
        "case UploadServerCertificate({name}): expected 200, got {} with {}",
        answer.status, answer.text
    );
    xml_element(&answer.text, "Arn").unwrap_or_else(|| {
        panic!(
            "case UploadServerCertificate({name}): expected the minted Arn, got {}",
            answer.text
        )
    })
}

/// Request a certificate covering `name` from a PRIVATE certificate authority, and answer
/// the ARN Certificate Manager stored it under: a certificate that service holds without
/// having issued it from its public authority.
async fn requested_from_private_ca(router: &MultiServiceRouter, name: &str) -> String {
    let answer = acm_call(
        router,
        "RequestCertificate",
        json!({"DomainName": name, "CertificateAuthorityArn": PRIVATE_CA_ARN}),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case RequestCertificate({name}, private CA): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    certificate_arn(&answer, &format!("RequestCertificate({name}, private CA)"))
}

/// The private certificate authority Certificate Manager reports behind the certificate at
/// `arn`, or `None` when it reports none (or resolves nothing for the ARN).
async fn reported_authority(router: &MultiServiceRouter, arn: &str) -> Option<String> {
    let read = acm_call(router, "DescribeCertificate", json!({"CertificateArn": arn})).await;
    if read.status != 200 {
        return None;
    }
    read.body["Certificate"]["CertificateAuthorityArn"]
        .as_str()
        .map(str::to_string)
}

/// The ARN Identity and Access Management reports for the server certificate held under
/// `name`, or `None` when the account holds none.
pub async fn reported_server_certificate(router: &MultiServiceRouter, name: &str) -> Option<String> {
    let read = iam_call(
        router,
        "GetServerCertificate",
        &[("ServerCertificateName", name.to_string())],
    )
    .await;
    if read.status != 200 {
        return None;
    }
    xml_element(&read.text, "Arn")
}

/// The custom domain names the account's listing reports, in the order it pages them.
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

/// Where the certificate a row names came from.
#[derive(Debug, Clone, Copy)]
enum Issuance {
    /// Certificate Manager's own public issuance.
    PubliclyIssued,
    /// A client imported it into Certificate Manager.
    Imported,
    /// A private certificate authority issued it, and Certificate Manager holds it.
    PrivateAuthority,
    /// The account uploaded a SELF-SIGNED certificate to Identity and Access Management.
    UploadedSelfSigned,
    /// The account uploaded a certificate an authority of this suite's own making signed to
    /// Identity and Access Management.
    UploadedByAuthority,
    /// The account uploaded a certificate that authority signed that covers some OTHER
    /// name than the one the domain is created under.
    UploadedForAnotherName,
    /// A well-formed Certificate Manager ARN of this account and Region that resolves to
    /// nothing.
    UnheldManaged,
    /// A well-formed server certificate ARN of this account that resolves to nothing.
    UnheldServerCertificate,
}

/// The name each row creates its domain under: one name space per issuance, so no row's
/// domain can be the one another row created.
const PUBLIC_NAME: &str = "public.appsync-authority-test.com";
const IMPORTED_NAME: &str = "imported.appsync-authority-test.com";
const PRIVATE_NAME: &str = "private-ca.appsync-authority-test.com";
const UPLOADED_NAME: &str = "uploaded.appsync-authority-test.com";
const UPLOADED_SIGNED_NAME: &str = "uploaded-signed.appsync-authority-test.com";
const UPLOADED_ELSEWHERE_NAME: &str = "uploaded-elsewhere.appsync-authority-test.com";
const UNHELD_MANAGED_NAME: &str = "unheld-managed.appsync-authority-test.com";
const UNHELD_UPLOADED_NAME: &str = "unheld-uploaded.appsync-authority-test.com";

/// The server certificate name whose Identity and Access Management record a row's ARN must
/// be, for a row that names an ARN of that form at all.
fn server_certificate_name(issuance: Issuance) -> Option<&'static str> {
    match issuance {
        Issuance::UploadedSelfSigned => Some(UPLOADED_CERTIFICATE_NAME),
        Issuance::UploadedByAuthority => Some(UPLOADED_SIGNED_CERTIFICATE_NAME),
        Issuance::UploadedForAnotherName => Some(UPLOADED_ELSEWHERE_CERTIFICATE_NAME),
        Issuance::UnheldServerCertificate => Some(UNHELD_CERTIFICATE_NAME),
        Issuance::PubliclyIssued
        | Issuance::Imported
        | Issuance::PrivateAuthority
        | Issuance::UnheldManaged => None,
    }
}

/// Every certificate the rows name, as the ARN each is addressed by.
struct Certificates {
    publicly_issued: String,
    imported: String,
    private_authority: String,
    uploaded_self_signed: String,
    uploaded_by_authority: String,
    uploaded_for_another_name: String,
    unheld_managed: String,
    unheld_server_certificate: String,
}

impl Certificates {
    /// Create each certificate the table names through the owning service's own operation,
    /// and build the two ARNs that resolve to nothing.
    async fn created(router: &MultiServiceRouter) -> Self {
        Self {
            publicly_issued: requested(router, &[PUBLIC_NAME]).await,
            imported: imported(router, &[IMPORTED_NAME], None).await,
            private_authority: requested_from_private_ca(router, PRIVATE_NAME).await,
            uploaded_self_signed: uploaded(
                router,
                UPLOADED_CERTIFICATE_NAME,
                minted(&[UPLOADED_NAME]),
            )
            .await,
            uploaded_by_authority: uploaded(
                router,
                UPLOADED_SIGNED_CERTIFICATE_NAME,
                minted_by_an_authority(&[UPLOADED_SIGNED_NAME]),
            )
            .await,
            // Signed by an authority like the row above, but for a name no row creates a
            // domain under: the certificate is sound and serves the wrong host.
            uploaded_for_another_name: uploaded(
                router,
                UPLOADED_ELSEWHERE_CERTIFICATE_NAME,
                minted_by_an_authority(&["other.appsync-authority-test.com"]),
            )
            .await,
            unheld_managed: format!(
                "arn:aws:acm:{REGION}:{ACCOUNT}:certificate/{NIL_CERTIFICATE_ID}"
            ),
            unheld_server_certificate: format!(
                "arn:aws:iam::{ACCOUNT}:server-certificate/{UNHELD_CERTIFICATE_NAME}"
            ),
        }
    }

    /// The ARN `issuance`'s certificate is addressed by.
    fn arn(&self, issuance: Issuance) -> &str {
        match issuance {
            Issuance::PubliclyIssued => &self.publicly_issued,
            Issuance::Imported => &self.imported,
            Issuance::PrivateAuthority => &self.private_authority,
            Issuance::UploadedSelfSigned => &self.uploaded_self_signed,
            Issuance::UploadedByAuthority => &self.uploaded_by_authority,
            Issuance::UploadedForAnotherName => &self.uploaded_for_another_name,
            Issuance::UnheldManaged => &self.unheld_managed,
            Issuance::UnheldServerCertificate => &self.unheld_server_certificate,
        }
    }
}

/// One row: the name the create asks for, where its certificate came from, the premise the
/// owning service must report for that certificate, and whether the create is accepted.
struct Row {
    label: &'static str,
    domain_name: &'static str,
    issuance: Issuance,
    /// The `Type` Certificate Manager must report, `None` for an ARN it resolves nothing for
    /// or one it holds no certificate of.
    managed_type: Option<&'static str>,
    /// Whether Certificate Manager must report a private certificate authority behind it.
    private_authority: bool,
    accepted: bool,
}

/// The table: one row per route a certificate reaches an account by, plus one unresolvable
/// ARN of each of the two forms the member admits.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "a certificate Certificate Manager issued publicly",
            domain_name: PUBLIC_NAME,
            issuance: Issuance::PubliclyIssued,
            managed_type: Some(TYPE_AMAZON_ISSUED),
            private_authority: false,
            accepted: true,
        },
        Row {
            label: "a certificate the client imported",
            domain_name: IMPORTED_NAME,
            issuance: Issuance::Imported,
            managed_type: Some(TYPE_IMPORTED),
            private_authority: false,
            accepted: false,
        },
        Row {
            label: "a certificate a private certificate authority issued",
            domain_name: PRIVATE_NAME,
            issuance: Issuance::PrivateAuthority,
            managed_type: Some(TYPE_AMAZON_ISSUED),
            private_authority: true,
            accepted: false,
        },
        Row {
            label: "a self-signed server certificate the account uploaded",
            domain_name: UPLOADED_NAME,
            issuance: Issuance::UploadedSelfSigned,
            managed_type: None,
            private_authority: false,
            accepted: false,
        },
        Row {
            label: "a server certificate the account uploaded that a local authority signed",
            domain_name: UPLOADED_SIGNED_NAME,
            issuance: Issuance::UploadedByAuthority,
            managed_type: None,
            private_authority: false,
            accepted: false,
        },
        Row {
            label: "an uploaded, authority-signed server certificate for another name",
            domain_name: UPLOADED_ELSEWHERE_NAME,
            issuance: Issuance::UploadedForAnotherName,
            managed_type: None,
            private_authority: false,
            accepted: false,
        },
        Row {
            label: "a certificate ARN the account holds nothing under",
            domain_name: UNHELD_MANAGED_NAME,
            issuance: Issuance::UnheldManaged,
            managed_type: None,
            private_authority: false,
            accepted: true,
        },
        Row {
            label: "a server certificate ARN the account holds nothing under",
            domain_name: UNHELD_UPLOADED_NAME,
            issuance: Issuance::UnheldServerCertificate,
            managed_type: None,
            private_authority: false,
            accepted: true,
        },
    ]
}

/// RULE: a custom domain is created only with a certificate an authority a client can verify
/// it against issued FOR THAT NAME — which, of the routes a certificate reaches an account by,
/// is Certificate Manager's own public issuance covering the name. A certificate the account
/// holds without such an issuance — imported into Certificate Manager, issued by a private
/// certificate authority, uploaded having signed itself, or uploaded under an authority of the
/// caller's own making — and one that serves some other host are refused with the modelled 400
/// and AppSync's fixed sentence, and leave no domain behind; an ARN neither service resolves
/// anything for is refused on no ground.
#[tokio::test]
async fn create_domain_name_requires_a_certificate_an_authority_issued() {
    let router = router();
    let certificates = Certificates::created(&router).await;

    for row in rows() {
        let arn = certificates.arn(row.issuance);

        // The row's premise, read off the service that HOLDS the certificate rather than
        // assumed here.
        let managed_type = reported_type(&router, arn).await;
        assert_eq!(
            managed_type.as_deref(),
            row.managed_type,
            "case {}: expected Certificate Manager to report type {:?} for {arn}, got \
             {managed_type:?}",
            row.label,
            row.managed_type
        );
        let authority = reported_authority(&router, arn).await;
        assert_eq!(
            authority.is_some(),
            row.private_authority,
            "case {}: expected a private certificate authority behind {arn} to be {}, got \
             {authority:?}",
            row.label,
            row.private_authority
        );
        if let Some(name) = server_certificate_name(row.issuance) {
            let held = reported_server_certificate(&router, name).await;
            let expected = matches!(
                row.issuance,
                Issuance::UploadedSelfSigned
                    | Issuance::UploadedByAuthority
                    | Issuance::UploadedForAnotherName
            )
            .then(|| arn.to_string());
            assert_eq!(
                held, expected,
                "case {}: expected Identity and Access Management to hold {expected:?} under \
                 {name}, got {held:?}",
                row.label
            );
        }

        let answer = create_domain(&router, row.domain_name, arn).await;
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
            continue;
        }

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
            message(&answer),
            CERTIFICATE_INVALID,
            "case {}: expected AppSync's certificate refusal, got {:?}",
            row.label,
            answer.body
        );
        // A refused create writes nothing, so the name is one the account does not hold.
        let read = read_domain(&router, row.domain_name).await;
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

    // The account's INVENTORY agrees with the per-domain reads: a refused create left
    // nothing to list.
    let listed = listed_domains(&router).await;
    let mut expected: Vec<&str> = rows()
        .iter()
        .filter(|row| row.accepted)
        .map(|row| row.domain_name)
        .collect();
    expected.sort_unstable();
    let mut reported: Vec<&str> = listed.iter().map(String::as_str).collect();
    reported.sort_unstable();
    assert_eq!(
        reported, expected,
        "case ListDomainNames: expected exactly the accepted domains, got {reported:?}"
    );
}

/// RULE: the issuing authority is read at the moment of the create, from the service that
/// HOLDS the certificate — so one name refused under a self-signed certificate the account
/// uploaded is served under a certificate Certificate Manager issued for it, with nothing of
/// the refused attempt remembered.
#[tokio::test]
async fn the_issuing_authority_is_read_at_the_create() {
    let router = router();
    let domain = "rotated.appsync-authority-test.com";

    let brought_in = uploaded(&router, "appsync-domain-rotated", minted(&[domain])).await;
    let refused = create_domain(&router, domain, &brought_in).await;
    assert_eq!(
        (refused.status, refused.error_type.as_str(), message(&refused)),
        (400, "BadRequestException", CERTIFICATE_INVALID),
        "case \"a self-signed server certificate\": expected the modelled 400 refusal, got {} \
         {:?} with {:?}",
        refused.status,
        refused.error_type,
        refused.body
    );

    let issued = requested(&router, &[domain]).await;
    let served = create_domain(&router, domain, &issued).await;
    assert_eq!(
        served.status, 200,
        "case \"the same name under a publicly issued certificate\": expected 200, got {} \
         with {:?}",
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
