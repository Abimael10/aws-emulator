// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of DOMAIN CONFIGURATIONS: `CreateDomainConfiguration`, the
//! `DescribeDomainConfiguration` reader that proves what it persisted, and
//! `DeleteDomainConfiguration`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what
//! an AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * a configuration created without a `domainName` configures the AWS-managed domain
//!   the account's endpoint already serves, and one carrying its own domain name is
//!   the customer-managed kind;
//! * a `serverCertificateArn` may only name a certificate that EXISTS, and whether one
//!   exists is the holder's answer: this suite writes the certificate into the one
//!   shared record Certificate Manager keeps of a certificate's serving material and
//!   so exercises both answers. A certificate nothing is held for is refused
//!   `InvalidRequestException` (400) with the fixed `Certificate <arn> does not
//!   exist.` and nothing created, which is what makes the following read a 404;
//! * a custom-domain configuration may only present a server certificate a device can
//!   establish trust in — one that chains to a certificate authority — or else carry
//!   the `validationCertificateArn` that proves domain ownership; a configuration for a
//!   domain of the caller's own that presents no certificate at all, and one whose
//!   certificate chains to no authority and carries no validation certificate, are each
//!   refused `InvalidRequestException` (400) and create nothing;
//! * that `validationCertificateArn` must itself name a certificate a PUBLIC authority
//!   signed, so one the holder does not have and one the account imported are each
//!   refused `InvalidRequestException` (400) and create nothing;
//! * a created configuration is read back member for member, and the name is its
//!   identity, so a second create under it is a conflict;
//! * a delete states the configuration's ABSENCE, so a name the account holds none
//!   under answers the empty 200 and removes nothing else, while a read of it refuses;
//! * the listing reports every configuration the account holds, filtered by
//!   `serviceType` when the request names one and paged by `pageSize`.

use std::sync::Arc;
use std::time::SystemTime;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_acm_certificatemanager::EmulatedService as CertificateManager;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region this suite signs for; the ARN assertions read it back.
const REGION: &str = "eu-central-1";
/// The configurations the SERVICE manages for every account — the settings of the
/// account's own endpoints, which no caller creates — as this service holds the catalog.
const RESERVED: &[service_com_amazonaws_iot_awsiotservice::state::ReservedDomainConfiguration] =
    service_com_amazonaws_iot_awsiotservice::state::RESERVED_DOMAIN_CONFIGURATIONS;
/// How long [`Emulator::await_status`] keeps looking for the status it waits for: far
/// longer than the transition needs, so a loaded machine cannot make a case fail.
const AWAIT_STATUS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);
/// How long it waits between two looks, which bounds nothing the cases assert.
const AWAIT_STATUS_POLL: std::time::Duration = std::time::Duration::from_millis(50);

// What the account holds for its OWN endpoints, which no caller created.
#[path = "domain_configurations/reserved_endpoint_configurations.rs"]
mod reserved_endpoint_configurations;

// What a reader reports for the settings an endpoint always has, and the ARN a
// configuration carries.
#[path = "domain_configurations/effective_endpoint_settings.rs"]
mod effective_endpoint_settings;

// WHEN a configuration may be deleted.
#[path = "domain_configurations/deletion_status_guard.rs"]
mod deletion_status_guard;

// The authorization service a configuration names.
#[path = "domain_configurations/authorizer_resolution.rs"]
mod authorizer_resolution;

/// One emulator: the shared engine plus a router with this service and the certificate
/// holder it resolves a server certificate against mounted on it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        // Certificate Manager HOLDS the certificates a domain configuration presents, so
        // it is mounted on the same engine: every certificate a case names is one the
        // holder's own API created, not a record written into the test.
        router.register_service(CertificateManager::new(engine));
        Self { router }
    }

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the domain-configuration request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    async fn create_configuration(&self, name: &str, body: &Value) -> Reply {
        self.dispatch("POST", &format!("/domainConfigurations/{name}"), Some(body))
            .await
    }

    async fn describe_configuration(&self, name: &str) -> Reply {
        self.dispatch("GET", &format!("/domainConfigurations/{name}"), None)
            .await
    }

    async fn delete_configuration(&self, name: &str) -> Reply {
        self.dispatch("DELETE", &format!("/domainConfigurations/{name}"), None)
            .await
    }

    async fn update_configuration(&self, name: &str, body: &Value) -> Reply {
        self.dispatch("PUT", &format!("/domainConfigurations/{name}"), Some(body))
            .await
    }

    /// Move one configuration into `DISABLED`, the status a delete is admissible in, and
    /// look until that status is in EFFECT: a configuration stops serving the connections
    /// it is serving before it is disabled, so the status the request asks for arrives a
    /// moment after the request is acknowledged.
    async fn disable_configuration(&self, case: &str, name: &str) {
        let updated = self
            .update_configuration(name, &json!({ "domainConfigurationStatus": "DISABLED" }))
            .await;
        assert_eq!(
            updated.status,
            200,
            "case {case}: expected the disable of {name} to answer 200, got {} body {}",
            updated.status,
            updated.text()
        );
        self.await_status(case, name, "DISABLED").await;
    }

    /// Read one configuration until it reports `status`, or a generous deadline passes:
    /// what is waited for is the CONDITION, never a fixed time.
    async fn await_status(&self, case: &str, name: &str, status: &str) {
        let deadline = std::time::Instant::now() + AWAIT_STATUS_DEADLINE;
        loop {
            let read = self.describe_configuration(name).await;
            let reported = read.json()["domainConfigurationStatus"].clone();
            if reported == json!(status) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {case}: {name} never reached {status}, last read {reported} body {}",
                read.text()
            );
            tokio::time::sleep(AWAIT_STATUS_POLL).await;
        }
    }

    /// The address this account's IoT data endpoint is published at, as
    /// `DescribeEndpoint` answers it: the domain a configuration for the AWS-managed
    /// domain configures, since it names none of its own.
    async fn account_data_endpoint(&self) -> String {
        let reply = self.dispatch("GET", "/endpoint", None).await;
        assert_eq!(
            reply.status,
            200,
            "case the account endpoint: expected 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["endpointAddress"]
            .as_str()
            .unwrap_or_else(|| panic!("no endpointAddress in {}", reply.text()))
            .to_string()
    }

    async fn list_configurations(&self, query: &str) -> Reply {
        self.dispatch("GET", &format!("/domainConfigurations?{query}"), None)
            .await
    }

    /// Call one Certificate Manager operation, so a case's certificates are the ones the
    /// HOLDER's own API created.
    async fn certificate_manager(&self, operation: &str, body: &Value) -> Reply {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("CertificateManager.{operation}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/acm/aws4_request, \
                     SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
                ),
            )
            .body(Bytes::from(body.to_string()))
            .expect("the certificate request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    /// IMPORT one certificate and its key into the holder, answering the ARN it was
    /// stored under: a certificate the ACCOUNT brought in, which no public authority
    /// signed.
    async fn import_certificate(
        &self,
        case: &str,
        certificate_pem: &str,
        private_key_pem: &str,
    ) -> String {
        let reply = self
            .certificate_manager(
                "ImportCertificate",
                &json!({
                    "Certificate": aws_smithy_types::base64::encode(certificate_pem.as_bytes()),
                    "PrivateKey": aws_smithy_types::base64::encode(private_key_pem.as_bytes()),
                }),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected ImportCertificate to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["CertificateArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no CertificateArn in {}", reply.text()))
            .to_string()
    }

    /// REQUEST one certificate for `domain_name` from the holder, answering the ARN it
    /// was stored under: a certificate Certificate Manager issues itself, so a public
    /// authority signs it.
    async fn request_certificate(&self, case: &str, domain_name: &str) -> String {
        let reply = self
            .certificate_manager(
                "RequestCertificate",
                &json!({ "DomainName": domain_name, "ValidationMethod": "DNS" }),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected RequestCertificate to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["CertificateArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no CertificateArn in {}", reply.text()))
            .to_string()
    }
}

/// A certificate and the private key that matches it: the material a case imports into
/// the holder. Minted by this service's own issuer, so the key really matches the
/// certificate — the holder refuses a pair that does not.
fn minted_certificate() -> (String, String) {
    let minted = service_com_amazonaws_iot_awsiotservice::state::mint_keys_and_certificate(
        REGION,
        ACCOUNT_ID,
        true,
        SystemTime::now(),
    )
    .expect("the suite can mint a certificate");
    (
        minted.certificate.certificate_pem,
        minted
            .key_pair
            .private_key
            .expect("the minted key pair carries its private half"),
    )
}

/// Create one custom authorizer through the account's own API, so a configuration
/// naming it names a record this service holds — "An object that specifies the
/// authorization service for a domain".
async fn create_authorizer(emulator: &Emulator, case: &str, authorizer_name: &str) {
    let created = emulator
        .dispatch(
            "POST",
            &format!("/authorizer/{authorizer_name}"),
            Some(&json!({
                "authorizerFunctionArn":
                    format!("arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:{authorizer_name}"),
            })),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "case {case}: expected CreateAuthorizer to answer 200, got {} body {}",
        created.status,
        created.text()
    );
}

/// Delete one custom authorizer through the account's own API.
async fn delete_authorizer(emulator: &Emulator, case: &str, authorizer_name: &str) {
    let deleted = emulator
        .dispatch("DELETE", &format!("/authorizer/{authorizer_name}"), None)
        .await;
    assert_eq!(
        deleted.status,
        200,
        "case {case}: expected DeleteAuthorizer to answer 200, got {} body {}",
        deleted.status,
        deleted.text()
    );
}

/// Assert that `arn` is the ARN of the configuration `name` a CALLER created: the
/// resource part NAMES the configuration and carries after it the short id the service
/// mints for it, which is the form AWS reports a created configuration's ARN in.
fn assert_created_arn(case: &str, name: &str, arn: &Value) {
    let reported = arn
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: no domainConfigurationArn in {arn}"));
    let prefix = format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:domainconfiguration/{name}/");
    assert!(
        reported.starts_with(&prefix),
        "case {case}: expected an ARN under {prefix:?}, got {reported:?}"
    );
    let minted_id = &reported[prefix.len()..];
    assert!(
        !minted_id.is_empty() && minted_id.chars().all(|c| c.is_ascii_digit()),
        "case {case}: expected the minted id after the name, got {reported:?}"
    );
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }
}

/// An ACM certificate ARN of this account, numbered.
fn certificate_arn(index: usize) -> String {
    format!("arn:aws:acm:{REGION}:{ACCOUNT_ID}:certificate/cert-{index:06}")
}

/// The ARN of a certificate a PUBLIC authority signed, which is what Certificate
/// Manager ISSUES: the kind
/// `CreateDomainConfiguration$validationCertificateArn` documents ("This certificate
/// must be signed by a public certificate authority").
async fn trusted_validation_certificate(emulator: &Emulator, case: &str) -> String {
    emulator.request_certificate(case, "example.com").await
}

/// RULE: a created configuration is read back member for member, and which KIND of
/// domain it configures follows from whether it carries a domain name of its own.
#[tokio::test]
async fn a_created_domain_configuration_is_read_back_member_for_member() {
    let emulator = Emulator::new();
    let (certificate_pem, private_key_pem) = minted_certificate();
    let held = emulator
        .import_certificate("read back", &certificate_pem, &private_key_pem)
        .await;
    let trusted = trusted_validation_certificate(&emulator, "read back").await;
    // WHERE this account's data plane answers, which is the domain a configuration for
    // the AWS-managed domain configures.
    let account_endpoint = emulator.account_data_endpoint().await;
    // The authorization service one row names is one this account's own API created: a
    // configuration may only name an authorizer the account holds.
    create_authorizer(&emulator, "read back", "my-authorizer").await;

    // The rows span which optional members the request carries, and both kinds of
    // domain: one without a `domainName` (the AWS-managed domain, for which
    // `serverCertificateArns` is "not required") and one with its own name and the
    // certificate the service presents for it.
    let rows: Vec<(&str, &str, Value, &str, usize)> = vec![
        (
            "the AWS-managed domain, nothing optional",
            "managed.config",
            json!({}),
            "AWS_MANAGED",
            0,
        ),
        (
            "the AWS-managed domain with an authorizer and a service type",
            "managed-with-authorizer",
            json!({
                "serviceType": "DATA",
                "authorizerConfig": { "defaultAuthorizerName": "my-authorizer",
                                      "allowAuthorizerOverride": true },
                "tags": [{ "Key": "team", "Value": "fleet" }],
            }),
            "AWS_MANAGED",
            0,
        ),
        (
            "a customer-managed domain presenting a certificate the holder has",
            "customer_managed-1",
            json!({
                "domainName": "iot.example.com",
                "serverCertificateArns": [held.clone()],
                "validationCertificateArn": trusted.clone(),
                "applicationProtocol": "SECURE_MQTT",
                "authenticationType": "AWS_X509",
                "tlsConfig": { "securityPolicy": "IoTSecurityPolicy_TLS13_1_2_2022_10" },
                "serverCertificateConfig": { "enableOCSPCheck": false },
                "clientCertificateConfig": {
                    "clientCertificateCallbackArn":
                        "arn:aws:lambda:eu-central-1:000000000000:function:client-check"
                },
            }),
            "CUSTOMER_MANAGED",
            1,
        ),
    ];

    for (case, name, body, domain_type, certificate_count) in &rows {
        let created = emulator.create_configuration(name, body).await;
        assert_eq!(
            created.status,
            200,
            "case {case}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        assert_created_arn(case, name, &created.json()["domainConfigurationArn"]);

        let read = emulator.describe_configuration(name).await;
        assert_eq!(
            read.status,
            200,
            "case {case}: expected the read to answer 200, got {} body {}",
            read.status,
            read.text()
        );
        let described = read.json();
        assert_eq!(
            described["domainConfigurationName"],
            json!(name),
            "case {case}: the read must name the configuration, got {described}"
        );
        assert_eq!(
            described["domainConfigurationArn"], created.json()["domainConfigurationArn"],
            "case {case}: the read must report the ARN the create minted, got {described}"
        );
        assert_eq!(
            described["domainConfigurationStatus"],
            json!("ENABLED"),
            "case {case}: a configuration serves from the moment it is created, got {described}"
        );
        assert_eq!(
            described["serviceType"],
            json!(body
                .get("serviceType")
                .and_then(Value::as_str)
                .unwrap_or("DATA")),
            "case {case}: the read must report the stored service type, got {described}"
        );
        assert_eq!(
            described["domainType"],
            json!(domain_type),
            "case {case}: expected a {domain_type} domain, got {described}"
        );
        // A configuration naming a domain of its own carries that name; one for the
        // AWS-managed domain configures the account's own data endpoint, so it carries
        // the address `DescribeEndpoint` publishes that endpoint at.
        assert_eq!(
            described["domainName"],
            body.get("domainName")
                .cloned()
                .unwrap_or_else(|| json!(account_endpoint)),
            "case {case}: the read must report the domain the configuration configures, got \
             {described}"
        );
        let certificates = described["serverCertificates"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: no serverCertificates in {described}"));
        assert_eq!(
            certificates.len(),
            *certificate_count,
            "case {case}: expected {certificate_count} certificate(s), got {described}"
        );
        if let Some(certificate) = certificates.first() {
            assert_eq!(
                certificate["serverCertificateArn"],
                json!(held.clone()),
                "case {case}: the read must report the resolved certificate, got {described}"
            );
            assert_eq!(
                certificate["serverCertificateStatus"],
                json!("VALID"),
                "case {case}: a certificate the service could resolve is valid, got {described}"
            );
        }
        for member in [
            "authorizerConfig",
            "serverCertificateConfig",
            "clientCertificateConfig",
        ] {
            assert_eq!(
                described[member],
                body.get(member).cloned().unwrap_or(Value::Null),
                "case {case}: the read must report the stored {member}, got {described}"
            );
        }
        // The settings an endpoint ALWAYS has are reported whether or not the request
        // pinned them: `DEFAULT` is the `ApplicationProtocol` and the `AuthenticationType`
        // of an endpoint pinned to neither ("Use a combination of port and Application
        // Layer Protocol Negotiation (ALPN) to specify …"), and TLS is served under the
        // service's default security policy.
        for (member, effective) in [
            ("applicationProtocol", json!("DEFAULT")),
            ("authenticationType", json!("DEFAULT")),
            (
                "tlsConfig",
                json!({ "securityPolicy": "IoTSecurityPolicy_TLS13_1_2_2022_10" }),
            ),
        ] {
            assert_eq!(
                described[member],
                body.get(member).cloned().unwrap_or(effective.clone()),
                "case {case}: expected {member} {effective}, got {described}"
            );
        }
        assert!(
            described["lastStatusChangeDate"].is_number(),
            "case {case}: the read must report when the status last changed, got {described}"
        );
    }

    // The name is the configuration's identity, so a second create under it is a
    // conflict and the stored record is left alone.
    let conflict = emulator
        .create_configuration("managed.config", &json!({ "serviceType": "DATA" }))
        .await;
    assert_eq!(
        conflict.status,
        409,
        "case a name already taken: expected 409, got {} body {}",
        conflict.status,
        conflict.text()
    );
    assert_eq!(
        conflict.error_code(),
        "ResourceAlreadyExistsException",
        "case a name already taken: got {}",
        conflict.text()
    );

    // The delete removes the configuration once it serves nobody, after which a READ of
    // the name is a 404: the reader has a record to report and there is none.
    emulator
        .disable_configuration("the delete", "managed.config")
        .await;
    assert_eq!(
        emulator.delete_configuration("managed.config").await.status,
        200,
        "case the delete: expected 200"
    );
    let read = emulator.describe_configuration("managed.config").await;
    assert_eq!(
        read.status,
        404,
        "case a read of the deleted configuration: expected 404, got {} body {}",
        read.status,
        read.text()
    );
    assert_eq!(
        read.error_code(),
        "ResourceNotFoundException",
        "case a read of the deleted configuration: got {}",
        read.text()
    );
}

/// RULE: a `serverCertificateArn` the holder has no certificate for cannot be
/// presented to a device, so the create is refused and nothing is written — and the
/// refusal names the PRESENTED certificate whether or not the configuration also carries a
/// proof the holder cannot resolve.
#[tokio::test]
async fn a_certificate_the_holder_does_not_have_is_refused() {
    let emulator = Emulator::new();
    let (certificate_pem, private_key_pem) = minted_certificate();
    let held = emulator
        .import_certificate("refusals", &certificate_pem, &private_key_pem)
        .await;
    let trusted = trusted_validation_certificate(&emulator, "refusals").await;

    // The rows span the forms a certificate the holder does not have takes: another
    // account's certificate, another region's, one whose id nothing was recorded
    // under, and a value that is no certificate ARN at all.
    let unknown = [
        certificate_arn(2),
        format!("arn:aws:acm:{REGION}:000000000001:certificate/cert-000001"),
        format!("arn:aws:acm:us-east-1:{ACCOUNT_ID}:certificate/cert-000001"),
        format!("arn:aws:acm:{REGION}:{ACCOUNT_ID}:certificate/00000000-0000-0000-0000-000000000000"),
    ];
    for (index, certificate) in unknown.iter().enumerate() {
        let name = format!("refused-{index}");
        let refused = emulator
            .create_configuration(
                &name,
                &json!({
                    "domainName": "iot.example.com",
                    "serverCertificateArns": [certificate],
                }),
            )
            .await;
        assert_eq!(
            refused.status,
            400,
            "case {certificate}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {certificate}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.error_message(),
            format!("Certificate {certificate} does not exist."),
            "case {certificate}: the refusal must name the certificate, got {}",
            refused.text()
        );
        // Nothing was created: the read of the name the create asked for is a 404.
        let described = emulator.describe_configuration(&name).await;
        assert_eq!(
            described.status,
            404,
            "case {certificate}: a refused create must leave no configuration behind, got {} \
             body {}",
            described.status,
            described.text()
        );
    }

    // The same values, now sent with a `validationCertificateArn` the holder has no
    // certificate under either: the refusal is about the certificate the configuration
    // PRESENTS, which is what the configuration is about, so the proof it carries does not
    // change which certificate is named.
    let unresolvable_proof =
        format!("arn:aws:acm:{REGION}:{ACCOUNT_ID}:certificate/11111111-1111-1111-1111-111111111111");
    for (index, certificate) in unknown.iter().enumerate() {
        let name = format!("refused-with-proof-{index}");
        let refused = emulator
            .create_configuration(
                &name,
                &json!({
                    "domainName": "iot.example.com",
                    "serverCertificateArns": [certificate],
                    "validationCertificateArn": unresolvable_proof.clone(),
                }),
            )
            .await;
        assert_eq!(
            (refused.status, refused.error_code(), refused.error_message()),
            (
                400,
                "InvalidRequestException".to_string(),
                format!("Certificate {certificate} does not exist.")
            ),
            "case {certificate} beside an unresolvable proof: the refusal must name the \
             presented certificate, got {}",
            refused.text()
        );
        let described = emulator.describe_configuration(&name).await;
        assert_eq!(
            described.status,
            404,
            "case {certificate} beside an unresolvable proof: a refused create must leave no \
             configuration behind, got {} body {}",
            described.status,
            described.text()
        );
    }

    // The certificate the holder DOES have is accepted, so the refusals above are
    // about the certificate and not about the request's shape.
    let accepted = emulator
        .create_configuration(
            "accepted",
            &json!({
                "domainName": "iot.example.com",
                "serverCertificateArns": [held.clone()],
                "validationCertificateArn": trusted.clone(),
            }),
        )
        .await;
    assert_eq!(
        accepted.status,
        200,
        "case the certificate the holder has: expected 200, got {} body {}",
        accepted.status,
        accepted.text()
    );
}

/// RULE: a configuration for a domain of the caller's OWN has to present a server
/// certificate a device can chain to a certificate authority — one Certificate Manager
/// ISSUED — or else carry the `validationCertificateArn` that proves domain ownership;
/// a create with neither is refused with nothing created.
#[tokio::test]
async fn an_unchainable_server_certificate_needs_a_validation_certificate() {
    /// Which kind of certificate the holder has for a row: one the ACCOUNT brought in,
    /// which no public authority signed, or one Certificate Manager issued itself.
    enum Held {
        Imported,
        Issued,
        /// The row presents no server certificate at all.
        None,
    }
    /// What the create answers a row with: a record, or the refusal whose message
    /// begins with the text given.
    enum Expected {
        Created,
        Refused(&'static str),
    }
    /// The fixed text of the refusal of a server certificate whose trust is not
    /// established.
    const UNTRUSTED: &str = "Must provide validation certificate when using server certificates \
                             not signed by trusted CA or not within";

    // The rows span what the rule generalises over: which kind of certificate the holder
    // has for the one the request names, whether the request carries a validation
    // certificate, and whether it names a domain of its own at all.
    struct Case {
        label: &'static str,
        held: Held,
        domain_name: bool,
        validation_certificate: bool,
        expected: Expected,
    }
    let cases = [
        Case {
            label: "an imported certificate with no validation certificate",
            held: Held::Imported,
            domain_name: true,
            validation_certificate: false,
            expected: Expected::Refused(UNTRUSTED),
        },
        Case {
            label: "an imported certificate with a validation certificate",
            held: Held::Imported,
            domain_name: true,
            validation_certificate: true,
            expected: Expected::Created,
        },
        Case {
            label: "a certificate the holder issued, with no validation certificate",
            held: Held::Issued,
            domain_name: true,
            validation_certificate: false,
            expected: Expected::Created,
        },
        Case {
            label: "a certificate the holder issued, with a validation certificate",
            held: Held::Issued,
            domain_name: true,
            validation_certificate: true,
            expected: Expected::Created,
        },
        Case {
            label: "an imported certificate on the AWS-managed domain, which proves no \
                    ownership",
            held: Held::Imported,
            domain_name: false,
            validation_certificate: false,
            expected: Expected::Created,
        },
        Case {
            label: "the AWS-managed domain, which presents no certificate at all",
            held: Held::None,
            domain_name: false,
            validation_certificate: false,
            expected: Expected::Created,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let emulator = Emulator::new();
        let certificate = match case.held {
            Held::Imported => {
                let (certificate_pem, private_key_pem) = minted_certificate();
                Some(
                    emulator
                        .import_certificate(case.label, &certificate_pem, &private_key_pem)
                        .await,
                )
            }
            Held::Issued => Some(
                emulator
                    .request_certificate(case.label, "iot.example.com")
                    .await,
            ),
            Held::None => None,
        };
        let mut body = json!({});
        if case.domain_name {
            body["domainName"] = json!("iot.example.com");
        }
        if let Some(certificate) = certificate.as_ref() {
            body["serverCertificateArns"] = json!([certificate]);
        }
        if case.validation_certificate {
            // The ownership proof is a certificate a public authority signed, which is
            // the one Certificate Manager ISSUES: these rows are about the SERVER
            // certificate's trust, so the validation certificate itself is a trusted one.
            body["validationCertificateArn"] =
                json!(trusted_validation_certificate(&emulator, case.label).await);
        }

        let name = format!("trust-{index}");
        let created = emulator.create_configuration(&name, &body).await;
        let refusal = match case.expected {
            Expected::Created => {
                assert_eq!(
                    created.status,
                    200,
                    "case {}: expected 200, got {} body {}",
                    case.label,
                    created.status,
                    created.text()
                );
                let read = emulator.describe_configuration(&name).await;
                assert_eq!(
                    read.status,
                    200,
                    "case {}: the accepted configuration must read back, got {} body {}",
                    case.label,
                    read.status,
                    read.text()
                );
                continue;
            }
            Expected::Refused(refusal) => refusal,
        };
        assert_eq!(
            created.status,
            400,
            "case {}: expected 400, got {} body {}",
            case.label,
            created.status,
            created.text()
        );
        assert_eq!(
            created.error_code(),
            "InvalidRequestException",
            "case {}: got {}",
            case.label,
            created.text()
        );
        assert!(
            created.error_message().starts_with(refusal),
            "case {}: the refusal must state the rule {refusal:?}, got {:?}",
            case.label,
            created.error_message()
        );
        // Nothing was created, which is what makes the following read a 404 and keeps the
        // configuration out of the account's listing.
        let described = emulator.describe_configuration(&name).await;
        assert_eq!(
            described.status,
            404,
            "case {}: a refused create must leave no configuration behind, got {} body {}",
            case.label,
            described.status,
            described.text()
        );
        assert_eq!(
            described.error_message(),
            format!(
                "Could not find domain configuration for given account ID and domain \
                 configuration name, AccountId: {ACCOUNT_ID}, DomainConfigurationName: {name}"
            ),
            "case {}: the read must state the miss by account and name, got {}",
            case.label,
            described.text()
        );
        let listed = emulator.list_configurations("").await;
        assert_eq!(
            listed.status,
            200,
            "case {}: expected the listing to answer 200, got {} body {}",
            case.label,
            listed.status,
            listed.text()
        );
        assert!(
            !listed
                .json()["domainConfigurations"]
                .as_array()
                .unwrap_or_else(|| panic!("no domainConfigurations in {}", listed.text()))
                .iter()
                .any(|summary| summary["domainConfigurationName"] == json!(name)),
            "case {}: a refused create must leave the name out of the listing, got {}",
            case.label,
            listed.text()
        );
    }
}

/// RULE: the listing reports every configuration the account holds, projected from the
/// same record the reader projects, filtered by `serviceType` when the request names
/// one, and paged by `pageSize` with its own marker.
#[tokio::test]
async fn the_listing_reports_the_configurations_the_account_holds() {
    let emulator = Emulator::new();
    let (certificate_pem, private_key_pem) = minted_certificate();
    let held = emulator
        .import_certificate("listing", &certificate_pem, &private_key_pem)
        .await;
    let trusted = trusted_validation_certificate(&emulator, "listing").await;

    // The rows span the service types a configuration may deliver
    // (`smithy_query(query="shape",shape="ServiceType")`: CREDENTIAL_PROVIDER, DATA,
    // JOBS) and both kinds of domain.
    let rows = [
        ("data-managed", json!({ "serviceType": "DATA" })),
        ("jobs-managed", json!({ "serviceType": "JOBS" })),
        (
            "credentials-managed",
            json!({ "serviceType": "CREDENTIAL_PROVIDER" }),
        ),
        (
            "data-custom",
            json!({
                "serviceType": "DATA",
                "domainName": "iot.example.com",
                "serverCertificateArns": [held.clone()],
                "validationCertificateArn": trusted.clone(),
            }),
        ),
    ];
    for (name, body) in &rows {
        let created = emulator.create_configuration(name, body).await;
        assert_eq!(
            created.status,
            200,
            "case {name}: the configuration must be creatable, got {} body {}",
            created.status,
            created.text()
        );
    }

    let listed = emulator.list_configurations("").await;
    let summaries = listed.json()["domainConfigurations"]
        .as_array()
        .unwrap_or_else(|| panic!("no domainConfigurations in {}", listed.text()))
        .clone();
    // Beside the four created here, the account holds the configurations the SERVICE
    // manages for its own endpoints, which no caller created.
    let expected_total = rows.len() + RESERVED.len();
    assert_eq!(
        summaries.len(),
        expected_total,
        "case the whole listing: expected {expected_total} configurations, got {}",
        listed.text()
    );
    for summary in &summaries {
        let name = summary["domainConfigurationName"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let read = emulator.describe_configuration(&name).await;
        assert_eq!(
            summary["domainConfigurationArn"], read.json()["domainConfigurationArn"],
            "case {name}: the summary must carry the ARN the reader reports, got {summary}"
        );
        assert_eq!(
            summary["serviceType"], read.json()["serviceType"],
            "case {name}: the summary must carry the stored service type, got {summary}"
        );
    }

    // A listing filtered by service type reports the configurations of that type and no
    // other — the reserved ones included, each under the type its endpoint delivers, and
    // ahead of the account's own.
    for (service_type, created) in [
        ("DATA", vec!["data-custom", "data-managed"]),
        ("JOBS", vec!["jobs-managed"]),
        (
            "CREDENTIAL_PROVIDER",
            vec!["credentials-managed"],
        ),
    ] {
        let mut expected: Vec<&str> = RESERVED
            .iter()
            .filter(|reserved| reserved.service_type == service_type)
            .map(|reserved| reserved.name)
            .collect();
        let mut created = created;
        created.sort_unstable();
        expected.extend(created);
        let filtered = emulator
            .list_configurations(&format!("serviceType={service_type}"))
            .await;
        let names: Vec<String> = filtered.json()["domainConfigurations"]
            .as_array()
            .unwrap_or_else(|| panic!("no domainConfigurations in {}", filtered.text()))
            .iter()
            .map(|summary| {
                summary["domainConfigurationName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            names, expected,
            "case serviceType={service_type}: expected {expected:?}, got {names:?}"
        );
    }

    // The listing pages over that order, a page at a time, and its own marker resumes
    // exactly where the page stopped.
    for page_size in [1usize, 3, 10] {
        let mut seen: Vec<String> = Vec::new();
        let mut query = format!("pageSize={page_size}");
        loop {
            let page = emulator.list_configurations(&query).await;
            let names: Vec<String> = page.json()["domainConfigurations"]
                .as_array()
                .unwrap_or_else(|| panic!("no domainConfigurations in {}", page.text()))
                .iter()
                .map(|summary| {
                    summary["domainConfigurationName"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();
            assert!(
                names.len() <= page_size,
                "case pageSize={page_size}: a page must hold at most {page_size}, got {names:?}"
            );
            seen.extend(names);
            match page.json()["nextMarker"].as_str() {
                Some(marker) => query = format!("pageSize={page_size}&marker={marker}"),
                None => break,
            }
        }
        assert_eq!(
            seen.len(),
            expected_total,
            "case pageSize={page_size}: expected every configuration once, got {seen:?}"
        );
    }
}

/// RULE: the certificate a configuration proves domain ownership with has to be one a
/// device's own trust store can chain — "This certificate must be signed by a public
/// certificate authority" (`CreateDomainConfiguration$validationCertificateArn`) — so a
/// validation certificate the holder does not have, and one no public authority signed,
/// are each refused with nothing created.
#[tokio::test]
async fn a_validation_certificate_must_be_signed_by_a_public_authority() {
    /// Where the row's `validationCertificateArn` comes from.
    enum Proof {
        /// A certificate Certificate Manager ISSUED, which a public authority signed.
        Issued,
        /// A certificate the ACCOUNT imported, which no public authority signed.
        Imported,
        /// An ARN the holder has no certificate under at all.
        Unknown(&'static str),
    }
    /// What the create answers a row with: a record, or the refusal with this message.
    enum Expected {
        Created,
        Refused(String),
    }
    /// The fixed text AWS refuses a validation certificate no public authority signed
    /// with; it states the rule and echoes nothing of the request.
    const UNTRUSTED_PROOF: &str = "Validation certificate must be signed by trusted CA";

    // The rows span what the rule generalises over: which kind of certificate the
    // holder has for the ARN the request names, the forms an ARN it has none under
    // takes, and whether the configuration names a domain of the caller's own.
    struct Case {
        label: &'static str,
        proof: Proof,
        domain_name: bool,
    }
    let cases = [
        Case {
            label: "a validation certificate the holder issued",
            proof: Proof::Issued,
            domain_name: true,
        },
        Case {
            label: "a validation certificate the holder issued, on the AWS-managed domain",
            proof: Proof::Issued,
            domain_name: false,
        },
        Case {
            label: "a validation certificate the account imported",
            proof: Proof::Imported,
            domain_name: true,
        },
        Case {
            label: "a validation certificate the account imported, on the AWS-managed domain",
            proof: Proof::Imported,
            domain_name: false,
        },
        Case {
            label: "a validation certificate of another account",
            proof: Proof::Unknown("arn:aws:acm:eu-central-1:000000000001:certificate/cert-000042"),
            domain_name: true,
        },
        Case {
            label: "a validation certificate of another region",
            proof: Proof::Unknown("arn:aws:acm:us-east-1:000000000000:certificate/cert-000042"),
            domain_name: true,
        },
        Case {
            label: "a validation certificate nothing was recorded under",
            proof: Proof::Unknown(
                "arn:aws:acm:eu-central-1:000000000000:certificate/\
                 00000000-0000-0000-0000-000000000042",
            ),
            domain_name: true,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let emulator = Emulator::new();
        // Every row presents a server certificate the holder ISSUED, so what the create
        // answers is about the validation certificate and not about the one presented.
        let presented = emulator
            .request_certificate(case.label, "iot.example.com")
            .await;
        let (validation_certificate, expected) = match case.proof {
            Proof::Issued => (
                trusted_validation_certificate(&emulator, case.label).await,
                Expected::Created,
            ),
            Proof::Imported => {
                let (certificate_pem, private_key_pem) = minted_certificate();
                (
                    emulator
                        .import_certificate(case.label, &certificate_pem, &private_key_pem)
                        .await,
                    Expected::Refused(UNTRUSTED_PROOF.to_string()),
                )
            }
            Proof::Unknown(arn) => (
                arn.to_string(),
                Expected::Refused(format!("Certificate {arn} does not exist.")),
            ),
        };

        let mut body = json!({
            "serverCertificateArns": [presented],
            "validationCertificateArn": validation_certificate,
        });
        if case.domain_name {
            body["domainName"] = json!("iot.example.com");
        }

        let name = format!("proof-{index}");
        let created = emulator.create_configuration(&name, &body).await;
        let refusal = match expected {
            Expected::Created => {
                assert_eq!(
                    created.status,
                    200,
                    "case {}: expected 200, got {} body {}",
                    case.label,
                    created.status,
                    created.text()
                );
                let read = emulator.describe_configuration(&name).await;
                assert_eq!(
                    read.status,
                    200,
                    "case {}: the accepted configuration must read back, got {} body {}",
                    case.label,
                    read.status,
                    read.text()
                );
                continue;
            }
            Expected::Refused(message) => message,
        };
        assert_eq!(
            created.status,
            400,
            "case {}: expected 400, got {} body {}",
            case.label,
            created.status,
            created.text()
        );
        assert_eq!(
            created.error_code(),
            "InvalidRequestException",
            "case {}: got {}",
            case.label,
            created.text()
        );
        assert_eq!(
            created.error_message(),
            refusal,
            "case {}: expected the refusal {refusal:?}, got {:?}",
            case.label,
            created.error_message()
        );
        // Nothing was written, which is what makes the following read a 404 and keeps
        // the name out of the account's listing.
        let described = emulator.describe_configuration(&name).await;
        assert_eq!(
            described.status,
            404,
            "case {}: a refused create must leave no configuration behind, got {} body {}",
            case.label,
            described.status,
            described.text()
        );
        let listed = emulator.list_configurations("").await;
        assert!(
            !listed.json()["domainConfigurations"]
                .as_array()
                .unwrap_or_else(|| panic!("no domainConfigurations in {}", listed.text()))
                .iter()
                .any(|summary| summary["domainConfigurationName"] == json!(name)),
            "case {}: a refused create must leave the name out of the listing, got {}",
            case.label,
            listed.text()
        );
    }
}

/// RULE: a delete states the ABSENCE of the configuration it names, so a name the
/// account holds none under is already in the asked-for state: the delete answers the
/// modelled empty success and removes nothing else, while a READ of that name still
/// refuses.
#[tokio::test]
async fn deleting_a_configuration_the_account_does_not_hold_succeeds() {
    let emulator = Emulator::new();
    let (certificate_pem, private_key_pem) = minted_certificate();
    let held = emulator
        .import_certificate("idempotent delete", &certificate_pem, &private_key_pem)
        .await;

    // One configuration the account DOES hold, so every row below also shows the delete
    // of an absent name removes nothing else.
    assert_eq!(
        emulator
            .create_configuration("Kept.config-1", &json!({ "serviceType": "DATA" }))
            .await
            .status,
        200,
        "case the kept configuration: expected 200"
    );
    // ... and one created and then deleted, so a repeat of its delete is one of the rows.
    assert_eq!(
        emulator
            .create_configuration("gone.config", &json!({ "serviceType": "JOBS" }))
            .await
            .status,
        200,
        "case the configuration to delete: expected 200"
    );
    emulator
        .disable_configuration("the configuration to delete", "gone.config")
        .await;
    assert_eq!(
        emulator.delete_configuration("gone.config").await.status,
        200,
        "case the first delete: expected 200"
    );
    // ... and a name whose create was REFUSED, which is the teardown of a resource AWS
    // never made: an untrusted server certificate with no ownership proof.
    let refused = emulator
        .create_configuration(
            "never.made",
            &json!({ "domainName": "iot.example.com", "serverCertificateArns": [held] }),
        )
        .await;
    assert_eq!(
        refused.status,
        400,
        "case the refused create: expected 400, got {} body {}",
        refused.status,
        refused.text()
    );

    // The rows span the forms a name the account holds no configuration under takes: one
    // never asked for, one deleted already, one whose create was refused, and one
    // differing from a held name only in case, since the name is the identity byte for
    // byte.
    for case in [
        "never.asked-for",
        "gone.config",
        "never.made",
        "kept.config-1",
    ] {
        let deleted = emulator.delete_configuration(case).await;
        assert_eq!(
            deleted.status,
            200,
            "case {case}: expected the delete of an absent configuration to answer 200, got {} \
             body {}",
            deleted.status,
            deleted.text()
        );
        assert_eq!(
            deleted.json(),
            json!({}),
            "case {case}: the delete's output is empty, got {}",
            deleted.text()
        );
        // The READ of the same name keeps refusing: it has a record to report and there
        // is none.
        let read = emulator.describe_configuration(case).await;
        assert_eq!(
            read.status,
            404,
            "case {case}: expected the read of an absent configuration to answer 404, got {} \
             body {}",
            read.status,
            read.text()
        );
        assert_eq!(
            read.error_code(),
            "ResourceNotFoundException",
            "case {case}: got {}",
            read.text()
        );
    }

    // The configuration the account holds is untouched by every delete above.
    let kept = emulator.describe_configuration("Kept.config-1").await;
    assert_eq!(
        kept.status,
        200,
        "case the kept configuration: a delete of an absent name must remove nothing else, got \
         {} body {}",
        kept.status,
        kept.text()
    );
    assert_eq!(
        kept.json()["domainConfigurationName"],
        json!("Kept.config-1"),
        "case the kept configuration: got {}",
        kept.text()
    );
}
