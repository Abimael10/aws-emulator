// SPDX-License-Identifier: CC0-1.0

//! A trust anchor whose source names a private certificate authority is admitted only when
//! the service that OWNS that authority answers for it.
//!
//! The rule under test, which two crates have to agree on (IAM Roles Anywhere judges the
//! source; Amazon Web Services Private CA owns the authority and answers the read), so the
//! suite lives here rather than in either crate:
//!
//! **a trust-anchor write resolves the certificate authority its `source` names through the
//! service that owns it, before anything is written** — a source of the `AWS_ACM_PCA` type
//! carries the ARN whose "root certificate of the Private Certificate Authority specified by
//! this ARN is used in trust validation for temporary credential requests"
//! (`smithy_query(query="shape", shape="SourceData")`, member `acmPcaArn`), so an ARN that
//! service answers no root certificate for describes no authority at all: the call is refused
//! HTTP 400 with the `ValidationException` error code in `x-amzn-errortype` and the fixed
//! sentence that error class carries, no trust anchor is created and no id is allocated.
//! A source of any other type names no resource of that service and is judged on its own
//! value alone.
//!
//! The rows differ along what the rule generalises over: WHY the read does not answer (the
//! authority was never created, it belongs to another account, another Region or another
//! partition, the value is not an ARN at all, its service segment names a different service,
//! its resource segment names another kind of resource or no id, its account segment is not
//! an account id or is one digit short of one, or the authority exists but holds no
//! certificate yet), against authorities that service really does answer for (two of them,
//! built through its own documented procedure, so no assertion can pass by echoing one
//! value) — and both writes that accept a `source`, because an update judges one exactly as a
//! create does.
//!
//! Nothing here asserts on the wall clock. The two spans this suite waits on belong to the
//! private CA service's own clock (finishing a CA, issuing a certificate); each is waited for
//! by polling that service's own read until it answers.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// How long the suite goes on polling for an outcome another service's clock owes, and how
/// often. Generous: the condition is polled, never timed, so a loaded host only polls more
/// often before the same answer arrives.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const POLLS: usize = 1_200;

/// The Region every call in this suite is signed for.
const REGION: &str = "eu-west-1";

/// The account every ARN this emulator mints carries, taken from the runtime rather than
/// respelled, so a row that names "another account" names one this emulator does not serve.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The `sourceType` a trust anchor that references a private certificate authority carries,
/// and the one whose data is a PEM bundle of its own
/// (`smithy_query(query="shape", shape="TrustAnchorType")`:
/// `["AWS_ACM_PCA", "CERTIFICATE_BUNDLE", "SELF_SIGNED_REPOSITORY"]`).
const ACM_PCA: &str = "AWS_ACM_PCA";
const CERTIFICATE_BUNDLE: &str = "CERTIFICATE_BUNDLE";

/// The template that produces a ROOT CA's own certificate — step 2 of the private CA
/// service's documented procedure for creating a root private CA, which is how this suite
/// drives an authority to one that answers its root certificate.
const ROOT_CA_TEMPLATE: &str = "arn:aws:acm-pca:::template/RootCACertificate/V1";

/// The status the private CA service reports for an authority that was created and is waiting
/// for the certificate that would put it in service.
const PENDING_CERTIFICATE: &str = "PENDING_CERTIFICATE";

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_rolesanywhere_rolesanywhere::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_acmpca_acmprivateca::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `REGION`, which
/// is what routes the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// What one call answered: the HTTP status, the parsed body and the modelled error code, taken
/// from `x-amzn-errortype` when the answer carries that header.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
    error_type: String,
}

impl Answer {
    /// The `message` an error body carries, or the empty string when it carries none.
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The `trustAnchor` a trust-anchor operation answered with.
    fn trust_anchor(&self) -> &Value {
        &self.body["trustAnchor"]
    }
}

/// One IAM Roles Anywhere call over its restJson1 wire, signed for `REGION`.
async fn roles_anywhere(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body
        .map(|value| Bytes::from(serde_json::to_vec(&value).expect("the request body serializes")))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("rolesanywhere.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("rolesanywhere"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .split(['#', '/', ':'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let body: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Answer {
        status,
        body,
        error_type,
    }
}

/// One Amazon Web Services Private CA call over its awsJson1_1 wire, signed for `REGION`.
async fn acm_pca(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("acm-pca.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("ACMPrivateCA.{operation}"))
        .header("authorization", authorization("acm-pca"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// A `Source` of `source_type` carrying the certificate data member the model ties to it:
/// `acmPcaArn` is "Included for trust anchors of type `AWS_ACM_PCA`" and
/// `x509CertificateData` "Included for trust anchors of type `CERTIFICATE_BUNDLE`"
/// (`smithy_query(query="shape", shape="SourceData")`).
fn source(source_type: &str, data: &str) -> Value {
    let source_data = if source_type == ACM_PCA {
        json!({"acmPcaArn": data})
    } else {
        json!({"x509CertificateData": data})
    };
    json!({"sourceType": source_type, "sourceData": source_data})
}

/// Create a private certificate authority through that service's OWN front door and answer
/// its ARN, leaving it where the create leaves it.
async fn created_private_ca(router: &MultiServiceRouter, common_name: &str) -> String {
    let (status, created) = acm_pca(
        router,
        "CreateCertificateAuthority",
        &json!({
            "CertificateAuthorityConfiguration": {
                "KeyAlgorithm": "RSA_2048",
                "SigningAlgorithm": "SHA256WITHRSA",
                "Subject": {"CommonName": common_name}
            },
            "CertificateAuthorityType": "ROOT"
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the private CA service must create {common_name:?}, got {status} {created:?}"
    );
    created
        .get("CertificateAuthorityArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no CertificateAuthorityArn in {created:?}"))
        .to_string()
}

/// The status that service reports for `arn`, asked rather than assumed.
async fn reported_status(router: &MultiServiceRouter, arn: &str) -> String {
    let (status, reported) = acm_pca(
        router,
        "DescribeCertificateAuthority",
        &json!({"CertificateAuthorityArn": arn}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the private CA service must describe {arn:?}, got {status} {reported:?}"
    );
    reported["CertificateAuthority"]["Status"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A private certificate authority that exists and holds no certificate yet: created, then
/// polled through that service's own read until it reports the authority is waiting for one.
async fn pending_certificate_private_ca(router: &MultiServiceRouter, common_name: &str) -> String {
    let arn = created_private_ca(router, common_name).await;
    for _ in 0..POLLS {
        if reported_status(router, &arn).await == PENDING_CERTIFICATE {
            return arn;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("the private CA service never reported {arn:?} as {PENDING_CERTIFICATE}");
}

/// A private certificate authority driven all the way through that service's own documented
/// procedure for a root CA — its certification request signed by itself and imported back —
/// so that service answers a root certificate for it. Answers the ARN and the PEM certificate
/// that service then HOLDS for it.
async fn active_private_ca(router: &MultiServiceRouter, common_name: &str) -> (String, String) {
    let arn = pending_certificate_private_ca(router, common_name).await;
    let csr = csr_of(router, &arn).await;
    let (status, issued) = acm_pca(
        router,
        "IssueCertificate",
        &json!({
            "CertificateAuthorityArn": arn,
            "Csr": ::aws_smithy_types::base64::encode(csr.as_bytes()),
            "SigningAlgorithm": "SHA256WITHRSA",
            "TemplateArn": ROOT_CA_TEMPLATE,
            "Validity": {"Type": "YEARS", "Value": 10},
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the private CA service must issue {arn:?} its own certificate, got {status} {issued:?}"
    );
    let certificate_arn = issued
        .get("CertificateArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no CertificateArn in {issued:?}"))
        .to_string();
    let certificate = issued_certificate(router, &arn, &certificate_arn).await;
    let (status, imported) = acm_pca(
        router,
        "ImportCertificateAuthorityCertificate",
        &json!({
            "CertificateAuthorityArn": arn,
            "Certificate": ::aws_smithy_types::base64::encode(certificate.as_bytes()),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the private CA service must import {arn:?}'s certificate, got {status} {imported:?}"
    );

    // The certificate that service now HOLDS for the authority, read through its own front
    // door rather than assumed to be the one that was imported.
    let (status, held) = acm_pca(
        router,
        "GetCertificateAuthorityCertificate",
        &json!({"CertificateAuthorityArn": arn}),
    )
    .await;
    assert_eq!(
        status, 200,
        "the private CA service must answer {arn:?}'s own certificate, got {status} {held:?}"
    );
    let held = held
        .get("Certificate")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("no Certificate in {held:?}"))
        .to_string();
    (arn, held)
}

/// The certification request that service publishes for `arn`, polled until it answers it:
/// the instant a created authority has one is owed on that service's own clock.
async fn csr_of(router: &MultiServiceRouter, arn: &str) -> String {
    for _ in 0..POLLS {
        let (status, reported) = acm_pca(
            router,
            "GetCertificateAuthorityCsr",
            &json!({"CertificateAuthorityArn": arn}),
        )
        .await;
        if status == 200 {
            return reported
                .get("Csr")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("no Csr in {reported:?}"))
                .to_string();
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("the private CA service never published a certification request for {arn:?}");
}

/// The body of the certificate `certificate_arn` names, polled until that service has
/// finished issuing it.
async fn issued_certificate(
    router: &MultiServiceRouter,
    ca_arn: &str,
    certificate_arn: &str,
) -> String {
    for _ in 0..POLLS {
        let (status, reported) = acm_pca(
            router,
            "GetCertificate",
            &json!({"CertificateAuthorityArn": ca_arn, "CertificateArn": certificate_arn}),
        )
        .await;
        if status == 200 {
            return reported
                .get("Certificate")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("no Certificate in {reported:?}"))
                .to_string();
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("the private CA service never issued {certificate_arn:?}");
}

/// A certificate-authority ARN in the form that service's own `Arn` members are modelled
/// with: `arn:<partition>:acm-pca:<region>:<account>:certificate-authority/<id>`.
fn ca_arn(partition: &str, region: &str, account: &str, id: &str) -> String {
    format!("arn:{partition}:acm-pca:{region}:{account}:certificate-authority/{id}")
}

/// The fixed sentence a trust-anchor write is refused with when the authority its source
/// names is one this service could not read. `write` is the gerund the refusal names the
/// operation by.
fn unreadable_authority_message(write: &str) -> String {
    format!("Error {write} TrustAnchor. Given AWS PCA did not allow get request, or invalid ARN.")
}

/// A minimal valid create body for `name`, carrying `source`.
fn create_body(name: &str, source: Value) -> Value {
    json!({"name": name, "source": source, "enabled": true})
}

/// Assert `answer` is the refusal a trust-anchor write answers an unreadable authority with:
/// the status, the error code (in the response header AWS carries it in) and the whole
/// sentence of that error class.
fn assert_unreadable_refusal(case: &str, write: &str, answer: &Answer) {
    assert_eq!(
        (answer.status, answer.error_type.as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException, got {} / {} / {:?}",
        answer.status,
        answer.error_type,
        answer.body
    );
    assert_eq!(
        answer.message(),
        unreadable_authority_message(write),
        "case {case:?}: expected the fixed sentence of that error class, got {:?}",
        answer.message()
    );
    assert!(
        answer.body.get("trustAnchor").is_none(),
        "case {case:?}: a refused write answers no trustAnchor, got {:?}",
        answer.body
    );
}

/// How many trust anchors this account holds, through the service's own listing.
async fn trust_anchor_count(router: &MultiServiceRouter) -> usize {
    let listed = roles_anywhere(router, "GET", "/trustanchors", None).await;
    assert_eq!(
        listed.status, 200,
        "expected 200 from ListTrustAnchors, got {} / {:?}",
        listed.status, listed.body
    );
    listed.body["trustAnchors"].as_array().map_or(0, Vec::len)
}

/// One row of the refusal table: why the authority a source names is one the owning service
/// answers nothing for.
#[derive(Debug)]
struct Row {
    case: &'static str,
    acm_pca_arn: String,
}

/// Every way this suite spells an authority that service answers no root certificate for,
/// against the one it does. `absent` is a CA ARN of this account and Region that was never
/// created; `pending` is an authority that really exists and holds no certificate yet.
fn refusal_rows(pending: &str) -> Vec<Row> {
    vec![
        Row {
            case: "a CA ARN of this account and Region that was never created",
            acm_pca_arn: ca_arn(
                "aws",
                REGION,
                ACCOUNT,
                "11111111-2222-3333-4444-555555555555",
            ),
        },
        Row {
            case: "a second never-created CA ARN, differing only in its id",
            acm_pca_arn: ca_arn(
                "aws",
                REGION,
                ACCOUNT,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            ),
        },
        Row {
            case: "a CA ARN of another account",
            acm_pca_arn: ca_arn(
                "aws",
                REGION,
                "123456789012",
                "22222222-3333-4444-5555-666677778888",
            ),
        },
        Row {
            case: "a CA ARN of another Region",
            acm_pca_arn: ca_arn(
                "aws",
                "ap-northeast-1",
                ACCOUNT,
                "33333333-4444-5555-6666-777788889999",
            ),
        },
        Row {
            case: "a CA ARN of another partition",
            acm_pca_arn: ca_arn(
                "aws-cn",
                REGION,
                ACCOUNT,
                "44444444-5555-6666-7777-888899990000",
            ),
        },
        Row {
            case: "a value that is not an ARN at all",
            acm_pca_arn: "not-a-certificate-authority".to_string(),
        },
        Row {
            case: "an ARN whose service segment names another service",
            acm_pca_arn: format!(
                "arn:aws:acm:{REGION}:{ACCOUNT}:certificate/55555555-6666-7777-8888-99990000aaaa"
            ),
        },
        Row {
            case: "an ARN whose account segment is not an account id",
            acm_pca_arn: ca_arn(
                "aws",
                REGION,
                "not-an-account",
                "66666666-7777-8888-9999-0000aaaabbbb",
            ),
        },
        Row {
            case: "an ARN whose account segment is one digit short of an account id",
            acm_pca_arn: ca_arn(
                "aws",
                REGION,
                &ACCOUNT[1..],
                "77777777-8888-9999-0000-aaaabbbbcccc",
            ),
        },
        Row {
            case: "an ARN naming another kind of that service's resources",
            acm_pca_arn: format!(
                "arn:aws:acm-pca:{REGION}:{ACCOUNT}:template/EndEntityCertificate"
            ),
        },
        Row {
            case: "a CA ARN carrying no id",
            acm_pca_arn: ca_arn("aws", REGION, ACCOUNT, ""),
        },
        Row {
            case: "an ARN carrying no resource at all",
            acm_pca_arn: format!("arn:aws:acm-pca:{REGION}:{ACCOUNT}:"),
        },
        Row {
            case: "an authority that exists and holds no certificate yet",
            acm_pca_arn: pending.to_string(),
        },
    ]
}

/// The whole rule on the create path: every row the owning service answers nothing for is
/// refused with that error class and creates nothing, while the authority it DOES answer for
/// is admitted and reads back.
#[tokio::test]
async fn create_trust_anchor_admits_only_a_private_ca_the_owning_service_answers_for() {
    let router = build_router();
    let (readable, held) = active_private_ca(&router, "root.readable.internal").await;
    let (other_readable, _) = active_private_ca(&router, "root.second.internal").await;
    let pending = pending_certificate_private_ca(&router, "root.pending.internal").await;
    assert_eq!(
        reported_status(&router, &pending).await,
        PENDING_CERTIFICATE,
        "the pending authority must be one that service holds without a certificate"
    );

    // The authorities that service answers for are admitted, and so is a source of the type
    // that names no resource of that service at all: the rule reaches only the type whose
    // data is an authority's ARN.
    let accepted: Vec<(&str, Value)> = vec![
        (
            "the private CA that service answers a root certificate for",
            source(ACM_PCA, &readable),
        ),
        (
            "a second private CA that service answers for",
            source(ACM_PCA, &other_readable),
        ),
        (
            "a certificate bundle, which names no resource of that service",
            source(CERTIFICATE_BUNDLE, &held),
        ),
    ];
    for (index, (case, submitted)) in accepted.into_iter().enumerate() {
        let before = trust_anchor_count(&router).await;
        let name = format!("admitted-{index}");
        let created = roles_anywhere(
            &router,
            "POST",
            "/trustanchors",
            Some(create_body(&name, submitted.clone())),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "case {case:?}: expected 201 from CreateTrustAnchor, got {} / {:?}",
            created.status, created.body
        );
        assert_eq!(
            created.trust_anchor().get("source"),
            Some(&submitted),
            "case {case:?}: expected the source the create carried, got {:?}",
            created.body
        );
        let id = created.trust_anchor()["trustAnchorId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no trustAnchorId in {:?}", created.body))
            .to_string();
        let read = roles_anywhere(&router, "GET", &format!("/trustanchor/{id}"), None).await;
        assert_eq!(
            (read.status, read.trust_anchor().get("source")),
            (200, Some(&submitted)),
            "case {case:?}: GetTrustAnchor must return the record the create stored, got {} / \
             {:?}",
            read.status,
            read.body
        );
        assert_eq!(
            trust_anchor_count(&router).await,
            before + 1,
            "case {case:?}: an admitted create files exactly one trust anchor"
        );
    }

    // And every row that service answers nothing for is refused, with nothing stored.
    for (index, row) in refusal_rows(&pending).into_iter().enumerate() {
        let case = row.case;
        let before = trust_anchor_count(&router).await;
        let answer = roles_anywhere(
            &router,
            "POST",
            "/trustanchors",
            Some(create_body(
                &format!("refused-{index}"),
                source(ACM_PCA, &row.acm_pca_arn),
            )),
        )
        .await;
        assert_unreadable_refusal(case, "creating", &answer);
        assert_eq!(
            trust_anchor_count(&router).await,
            before,
            "case {case:?}: a refused create files no trust anchor, and so allocates no id"
        );
    }
}

/// The same rule on the update path, which judges a submitted source exactly as the create
/// does: a replacement naming an authority that service answers nothing for is refused and
/// leaves the stored record alone, and one naming an authority it answers for replaces it.
#[tokio::test]
async fn update_trust_anchor_judges_a_submitted_private_ca_source_the_same_way() {
    let router = build_router();
    let (readable, _) = active_private_ca(&router, "root.updated.internal").await;
    let (replacement, _) = active_private_ca(&router, "root.replacement.internal").await;
    let pending = pending_certificate_private_ca(&router, "root.half.internal").await;

    let stored = source(ACM_PCA, &readable);
    let created = roles_anywhere(
        &router,
        "POST",
        "/trustanchors",
        Some(create_body("updated-anchor", stored.clone())),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "expected 201 from CreateTrustAnchor, got {} / {:?}",
        created.status, created.body
    );
    let id = created.trust_anchor()["trustAnchorId"]
        .as_str()
        .unwrap_or_else(|| panic!("no trustAnchorId in {:?}", created.body))
        .to_string();
    let path = format!("/trustanchor/{id}");

    for row in refusal_rows(&pending) {
        let case = row.case;
        let answer = roles_anywhere(
            &router,
            "PATCH",
            &path,
            Some(json!({"source": source(ACM_PCA, &row.acm_pca_arn)})),
        )
        .await;
        assert_unreadable_refusal(case, "updating", &answer);
        let read = roles_anywhere(&router, "GET", &path, None).await;
        assert_eq!(
            (read.status, read.trust_anchor().get("source")),
            (200, Some(&stored)),
            "case {case:?}: a refused update leaves the stored source alone, got {} / {:?}",
            read.status,
            read.body
        );
    }

    let replaced = source(ACM_PCA, &replacement);
    let answer = roles_anywhere(
        &router,
        "PATCH",
        &path,
        Some(json!({"source": replaced.clone()})),
    )
    .await;
    assert_eq!(
        (answer.status, answer.trust_anchor().get("source")),
        (200, Some(&replaced)),
        "an update naming an authority that service answers for replaces the source, got {} / \
         {:?}",
        answer.status,
        answer.body
    );
    let read = roles_anywhere(&router, "GET", &path, None).await;
    assert_eq!(
        (read.status, read.trust_anchor().get("source")),
        (200, Some(&replaced)),
        "GetTrustAnchor must return the source the update wrote, got {} / {:?}",
        read.status,
        read.body
    );
}
