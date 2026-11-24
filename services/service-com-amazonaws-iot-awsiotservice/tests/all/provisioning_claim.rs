// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `CreateProvisioningClaim`: the temporary certificate a
//! device is given to run a fleet provisioning template, and the refusal of a template
//! the account does not hold.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what an
//! AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * a claim is issued for an EXISTING provisioning template: the response carries a
//!   real certificate and the key pair for it, the certificate is on record under the id
//!   reported — `DescribeCertificate` reads it back — and its validity ends at the
//!   `expiration` reported, since a claim certificate is temporary;
//! * a claim certificate is SHORT-LIVED: its validity is the documented five minutes
//!   after the call that minted it, for every claim and every template;
//! * a `templateName` the account holds no template under is
//!   `ResourceNotFoundException` (404) and nothing is issued.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for.
const REGION: &str = "ap-south-1";

/// The role a provisioning template provisions under, of more than the modelled minimum
/// length (`RoleArn`, len 20..2048).
const PROVISIONING_ROLE: &str = "arn:aws:iam::123456789012:role/fleet-provisioning";

/// How long a provisioning claim certificate is valid for, in seconds: AWS IoT documents
/// the claim as a temporary credential that expires five minutes after it is created
/// (AWS IoT Core Developer Guide, fleet provisioning "Provisioning by claim"). Written
/// out here rather than read from the crate, so this suite pins the published duration
/// instead of whatever the emulator happens to hold.
const DOCUMENTED_CLAIM_VALIDITY_SECONDS: f64 = 300.0;

/// The tolerance the timestamp assertions allow: the `expiration`, the `notAfter` and the
/// `creationDate` of one claim are all derived from the SAME instant, so they differ only
/// by their wire encoding, never by how fast the host runs.
const SAME_INSTANT_TOLERANCE_SECONDS: f64 = 1.0;

/// One emulator: a router with this service mounted on its own engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
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
            .expect("the provisioning-claim request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    async fn create_template(&self, case: &str, template_name: &str, enabled: bool) {
        let reply = self
            .dispatch(
                "POST",
                "/provisioning-templates",
                Some(&json!({
                    "templateName": template_name,
                    "templateBody": template_body(),
                    "provisioningRoleArn": PROVISIONING_ROLE,
                    "enabled": enabled,
                })),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected CreateProvisioningTemplate to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    async fn create_claim(&self, template_name: &str) -> Reply {
        self.dispatch(
            "POST",
            &format!("/provisioning-templates/{template_name}/provisioning-claim"),
            None,
        )
        .await
    }

    async fn describe_certificate(&self, certificate_id: &str) -> Reply {
        self.dispatch("GET", &format!("/certificates/{certificate_id}"), None)
            .await
    }
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

/// A provisioning template body: the one resource a provisioned device gets.
fn template_body() -> String {
    json!({
        "Parameters": { "SerialNumber": { "Type": "String" } },
        "Resources": {
            "thing": {
                "Type": "AWS::IoT::Thing",
                "Properties": { "ThingName": "device-1" },
            },
        },
    })
    .to_string()
}

/// The instants the service holds for the claim certificate `certificate_id`, read back
/// through `DescribeCertificate`: `(creationDate, notBefore, notAfter)`.
///
/// ONE reader for both rules below, so neither can disagree with the other about where a
/// claim's instants come from; it also asserts the read reports the id it was asked about,
/// since a claim that is not on record under the id reported has no instants to compare.
async fn certificate_instants(
    emulator: &Emulator,
    certificate_id: &str,
    label: &str,
) -> (f64, f64, f64) {
    let described = emulator.describe_certificate(certificate_id).await;
    assert_eq!(
        described.status,
        200,
        "case {label}: expected the claim certificate to read back, got {} body {}",
        described.status,
        described.text()
    );
    let description = described.json();
    assert_eq!(
        description["certificateDescription"]["certificateId"],
        json!(certificate_id),
        "case {label}: the read must report the issued id, got {}",
        described.text()
    );
    let read = |value: &Value, member: &str| -> f64 {
        value.as_f64().unwrap_or_else(|| {
            panic!(
                "case {label}: no {member} for {certificate_id} in {}",
                described.text()
            )
        })
    };
    let validity = &description["certificateDescription"]["validity"];
    (
        read(
            &description["certificateDescription"]["creationDate"],
            "creationDate",
        ),
        read(&validity["notBefore"], "notBefore"),
        read(&validity["notAfter"], "notAfter"),
    )
}

/// RULE: a claim for a template the account holds is a real certificate and key pair,
/// on record under the id reported and valid until the expiration reported.
#[tokio::test]
async fn a_claim_is_issued_for_a_template_the_account_holds() {
    // The rows span what the rule generalises over: whether the template is enabled, and
    // the form of its name (the modelled `TemplateName` pattern admits letters, digits,
    // `_` and `-`).
    let cases = [
        ("an enabled template", "fleet-template", true),
        ("a template that is not enabled", "disabled_template_1", false),
    ];

    for (label, template_name, enabled) in cases {
        let emulator = Emulator::new();
        emulator.create_template(label, template_name, enabled).await;

        let claim = emulator.create_claim(template_name).await;
        assert_eq!(
            claim.status,
            200,
            "case {label}: expected 200, got {} body {}",
            claim.status,
            claim.text()
        );
        let body = claim.json();
        let certificate_id = body["certificateId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label}: no certificateId in {}", claim.text()))
            .to_string();
        assert_eq!(
            certificate_id.len(),
            64,
            "case {label}: the id must be the modelled 64-character one, got {certificate_id}"
        );
        assert!(
            body["certificatePem"]
                .as_str()
                .is_some_and(|pem| pem.contains("BEGIN CERTIFICATE")),
            "case {label}: expected a PEM certificate, got {}",
            claim.text()
        );
        assert!(
            body["keyPair"]["PrivateKey"]
                .as_str()
                .is_some_and(|key| key.contains("PRIVATE KEY")),
            "case {label}: expected the claim's private key, got {}",
            claim.text()
        );
        assert!(
            body["keyPair"]["PublicKey"]
                .as_str()
                .is_some_and(|key| key.contains("PUBLIC KEY")),
            "case {label}: expected the claim's public key, got {}",
            claim.text()
        );
        let expiration = body["expiration"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {label}: no expiration in {}", claim.text()));

        // The certificate is ON RECORD under the id reported, and the validity it holds
        // ends exactly at the expiration this response reported — a claim certificate is
        // temporary, so the two are one instant.
        let (_, not_before, not_after) =
            certificate_instants(&emulator, &certificate_id, label).await;
        assert!(
            (not_after - expiration).abs() < SAME_INSTANT_TOLERANCE_SECONDS,
            "case {label}: expected the certificate to expire at the reported expiration \
             {expiration}, got {not_after}"
        );
        let validity = not_after - not_before;
        assert!(
            (validity - DOCUMENTED_CLAIM_VALIDITY_SECONDS).abs() < SAME_INSTANT_TOLERANCE_SECONDS,
            "case {label}: expected a claim valid for \
             {DOCUMENTED_CLAIM_VALIDITY_SECONDS}s, got {validity}s"
        );

        // Two claims for one template are two certificates: each call issues its own.
        let second = emulator.create_claim(template_name).await;
        assert_eq!(
            second.status,
            200,
            "case {label}: expected a second claim to answer 200, got {} body {}",
            second.status,
            second.text()
        );
        assert_ne!(
            second.json()["certificateId"],
            json!(certificate_id),
            "case {label}: each claim must be its own certificate, got {}",
            second.text()
        );
    }
}

/// RULE: a claim certificate's validity is the documented five minutes, measured from the
/// call that minted it — the `expiration` reported, the certificate's `notAfter` and its
/// `creationDate` all come from that one instant, so the claim a device is handed is
/// short-lived whatever template it was issued for.
#[tokio::test]
async fn a_claim_certificate_expires_the_documented_span_after_it_is_issued() {
    // The rows span what the rule generalises over: the template a claim is issued for
    // (enabled or not, either admitted `TemplateName` form) and WHICH claim of that
    // template it is — every claim carries its own five minutes, not a shared deadline.
    let cases = [
        ("the first claim for an enabled template", "fleet-template", true, 1),
        ("a later claim for an enabled template", "fleet-template", true, 3),
        (
            "a claim for a template that is not enabled",
            "disabled_template_1",
            false,
            2,
        ),
    ];

    for (label, template_name, enabled, claims) in cases {
        let emulator = Emulator::new();
        emulator.create_template(label, template_name, enabled).await;

        let mut last = None;
        for issue in 1..=claims {
            let claim = emulator.create_claim(template_name).await;
            assert_eq!(
                claim.status,
                200,
                "case {label} (claim {issue}): expected 200, got {} body {}",
                claim.status,
                claim.text()
            );
            last = Some(claim);
        }
        let claim = last.expect("every case issues at least one claim");
        let body = claim.json();
        let certificate_id = body["certificateId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label}: no certificateId in {}", claim.text()))
            .to_string();
        let expiration = body["expiration"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {label}: no expiration in {}", claim.text()));

        let (created, not_before, not_after) =
            certificate_instants(&emulator, &certificate_id, label).await;

        let reported_ttl = expiration - created;
        assert!(
            (reported_ttl - DOCUMENTED_CLAIM_VALIDITY_SECONDS).abs()
                < SAME_INSTANT_TOLERANCE_SECONDS,
            "case {label}: expected the claim to expire {DOCUMENTED_CLAIM_VALIDITY_SECONDS}s \
             after it was issued, got {reported_ttl}s"
        );
        let certificate_ttl = not_after - not_before;
        assert!(
            (certificate_ttl - DOCUMENTED_CLAIM_VALIDITY_SECONDS).abs()
                < SAME_INSTANT_TOLERANCE_SECONDS,
            "case {label}: expected the certificate itself to be valid for \
             {DOCUMENTED_CLAIM_VALIDITY_SECONDS}s, got {certificate_ttl}s"
        );
        assert!(
            (not_before - created).abs() < SAME_INSTANT_TOLERANCE_SECONDS,
            "case {label}: expected the certificate to be valid from its creation \
             {created}, got {not_before}"
        );
        assert!(
            (not_after - expiration).abs() < SAME_INSTANT_TOLERANCE_SECONDS,
            "case {label}: expected the certificate to expire at the reported expiration \
             {expiration}, got {not_after}"
        );
    }
}

/// RULE: a `templateName` the account holds no template under is refused and nothing is
/// issued.
#[tokio::test]
async fn a_claim_for_a_template_the_account_does_not_hold_is_refused() {
    let emulator = Emulator::new();
    emulator.create_template("setup", "fleet-template", true).await;

    // The rows span the forms a name the account holds no template under takes: one
    // never created, one differing only in case, and one whose template was deleted.
    let deleted = "gone-template";
    emulator.create_template("setup", deleted, true).await;
    let removed = emulator
        .dispatch(
            "DELETE",
            &format!("/provisioning-templates/{deleted}"),
            None,
        )
        .await;
    assert_eq!(
        removed.status,
        200,
        "case the deleted template: the delete must succeed, got {} body {}",
        removed.status,
        removed.text()
    );

    for template_name in ["absent-template", "FLEET-TEMPLATE", deleted] {
        let refused = emulator.create_claim(template_name).await;
        assert_eq!(
            refused.status,
            404,
            "case {template_name}: expected 404, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "ResourceNotFoundException",
            "case {template_name}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.error_message(),
            format!("Unable to find template named {template_name}"),
            "case {template_name}: the refusal must name the template, got {}",
            refused.text()
        );
    }
}
