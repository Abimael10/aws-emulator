// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `DetachPrincipalPolicy`, the DEPRECATED spelling of
//! `DetachPolicy`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name, with the principal travelling in the
//! `x-amzn-iot-principal` header the model binds it to — so what is asserted is what an
//! AWS SDK client would see, not an internal function's return value.
//!
//! The rule under test: the deprecated call REMOVES the pair (policy, principal) from the
//! one persisted policy-attachment relation both spellings of the attach write, so after
//! it every reader of that relation (`ListAttachedPolicies` over the principal,
//! `ListTargetsForPolicy` over the policy) stops reporting the pair and every guard that
//! reads it — the certificate delete's and the policy delete's — stops holding its
//! resource back. The removal is idempotent and reports nothing about whether the pair was
//! there, while a principal of no supported form is refused `InvalidRequestException`
//! (400) and a well-formed one naming a resource this account does not hold
//! `ResourceNotFoundException` (404), with the relation left as it was.
//!
//! The tables span the class the rule generalises over rather than one recorded request:
//! each principal form the modelled member documents (a certificate ARN, a thing group ARN
//! and a Cognito identity), which spelling of the attach recorded the pair and which
//! spelling of the detach removes it, how many policies the principal holds, and policy
//! names of several forms the modelled `PolicyName` pattern `^[\w+=,.@-]+$` admits.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region the harness signs for.
const REGION: &str = "eu-west-1";
/// An IoT policy document every policy in this suite is created with: the document is not
/// what this suite is about, so one valid document serves every case.
const POLICY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iot:Connect","Resource":"*"}]}"#;

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` of an error body.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// One string member of the body, or the empty string when it is absent.
    fn member(&self, name: &str) -> String {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The `policyName` of every entry of a `ListAttachedPolicies` answer, in the order
    /// the listing reported them.
    fn policy_names(&self) -> Vec<String> {
        self.entries("policies")
            .iter()
            .map(|entry| {
                entry
                    .get("policyName")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    /// The `targets` of a `ListTargetsForPolicy` answer.
    fn targets(&self) -> Vec<String> {
        self.entries("targets")
            .iter()
            .map(|entry| entry.as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// One list member of the body, or an empty list when it is absent.
    fn entries(&self, name: &str) -> Vec<Value> {
        self.body
            .get(name)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether the body carries no member at all — what an operation whose output shape
    /// is `Unit` answers with.
    fn is_empty_body(&self) -> bool {
        match &self.body {
            Value::Null => true,
            Value::Object(members) => members.is_empty(),
            _ => false,
        }
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
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

    /// One request, with any number of extra headers — the legacy pair carries its
    /// principal in one of them.
    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        headers: &[(&str, &str)],
    ) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host;x-amz-date, Signature=0000"
                ),
            )
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// `CreatePolicy`: `POST /policies/{policyName}`.
    async fn create_policy(&self, policy_name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/policies/{policy_name}"),
            Some(&json!({ "policyDocument": POLICY_DOCUMENT })),
            &[],
        )
        .await
    }

    /// `GetPolicy`: `GET /policies/{policyName}`.
    async fn get_policy(&self, policy_name: &str) -> Reply {
        self.call("GET", &format!("/policies/{policy_name}"), None, &[])
            .await
    }

    /// `DeletePolicy`: `DELETE /policies/{policyName}` — the teardown an attachment holds
    /// back.
    async fn delete_policy(&self, policy_name: &str) -> Reply {
        self.call("DELETE", &format!("/policies/{policy_name}"), None, &[])
            .await
    }

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, which issues the
    /// certificate a policy is attached to.
    async fn create_certificate(&self, set_as_active: bool) -> Reply {
        self.call(
            "POST",
            &format!("/keys-and-certificate?setAsActive={set_as_active}"),
            None,
            &[],
        )
        .await
    }

    /// `DeleteCertificate`: `DELETE /certificates/{certificateId}` — the other teardown
    /// an attachment holds back.
    async fn delete_certificate(&self, certificate_id: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/certificates/{certificate_id}"),
            None,
            &[],
        )
        .await
    }

    /// `CreateThingGroup`: `POST /thing-groups/{thingGroupName}`.
    async fn create_thing_group(&self, name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/thing-groups/{name}"),
            Some(&json!({})),
            &[],
        )
        .await
    }

    /// `AttachPrincipalPolicy`: `PUT /principal-policies/{policyName}` carrying the
    /// principal in the `x-amzn-iot-principal` header the model binds it to.
    async fn attach_principal_policy(&self, policy_name: &str, principal: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/principal-policies/{policy_name}"),
            None,
            &[("x-amzn-iot-principal", principal)],
        )
        .await
    }

    /// `DetachPrincipalPolicy`: `DELETE /principal-policies/{policyName}`, the principal
    /// in the same header the attach carries it in.
    async fn detach_principal_policy(&self, policy_name: &str, principal: &str) -> Reply {
        self.call(
            "DELETE",
            &format!("/principal-policies/{policy_name}"),
            None,
            &[("x-amzn-iot-principal", principal)],
        )
        .await
    }

    /// `AttachPolicy`: `PUT /target-policies/{policyName}` carrying the target in the
    /// body — the CURRENT spelling of the same effect.
    async fn attach_policy(&self, policy_name: &str, target: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/target-policies/{policy_name}"),
            Some(&json!({ "target": target })),
            &[],
        )
        .await
    }

    /// `DetachPolicy`: `POST /target-policies/{policyName}` carrying the target.
    async fn detach_policy(&self, policy_name: &str, target: &str) -> Reply {
        self.call(
            "POST",
            &format!("/target-policies/{policy_name}"),
            Some(&json!({ "target": target })),
            &[],
        )
        .await
    }

    /// `ListAttachedPolicies`: `POST /attached-policies/{target}`, whose target is ONE
    /// path segment (`crate::policy_attachments::as_path_segment`).
    async fn list_attached_policies(&self, target: &str) -> Reply {
        self.call(
            "POST",
            &format!(
                "/attached-policies/{}",
                crate::policy_attachments::as_path_segment(target)
            ),
            None,
            &[],
        )
        .await
    }

    /// `ListTargetsForPolicy`: `POST /policy-targets/{policyName}`.
    async fn list_targets_for_policy(&self, policy_name: &str) -> Reply {
        self.call("POST", &format!("/policy-targets/{policy_name}"), None, &[])
            .await
    }
}

/// The ARN of a thing group created in this suite's region.
fn thing_group_arn(name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/{name}")
}

/// The ARN of a certificate in this suite's region, for a certificate id this account does
/// not hold.
fn certificate_arn(certificate_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:cert/{certificate_id}")
}

/// Create `policy_name`, asserting the create succeeded: every detach case starts from a
/// pair that was recorded.
async fn given_policy(emulator: &Emulator, policy_name: &str) {
    let created = emulator.create_policy(policy_name).await;
    assert_eq!(
        created.status, 200,
        "creating the policy {policy_name} must succeed, got {} {:?}",
        created.status, created.body
    );
}

/// Issue a certificate, answering its `(certificateId, certificateArn)`.
async fn given_certificate(emulator: &Emulator, set_as_active: bool) -> (String, String) {
    let issued = emulator.create_certificate(set_as_active).await;
    assert_eq!(
        issued.status, 200,
        "issuing a certificate must succeed, got {} {:?}",
        issued.status, issued.body
    );
    (
        issued.member("certificateId"),
        issued.member("certificateArn"),
    )
}

/// Create a thing group, answering its `thingGroupArn`.
async fn given_thing_group(emulator: &Emulator, name: &str) -> String {
    let created = emulator.create_thing_group(name).await;
    assert_eq!(
        created.status, 200,
        "creating the thing group {name} must succeed, got {} {:?}",
        created.status, created.body
    );
    created.member("thingGroupArn")
}

/// Which spelling of the attach or the detach a row drives: the two are two names for one
/// write of one relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// `AttachPrincipalPolicy` / `DetachPrincipalPolicy`, the deprecated pair.
    Principal,
    /// `AttachPolicy` / `DetachPolicy`, the current pair.
    Target,
}

/// RULE: the deprecated detach removes the pair from the one relation both attach
/// spellings write, so every reader of it stops reporting the pair — whichever spelling
/// recorded it, and whatever form the principal takes.
#[tokio::test]
async fn detaching_a_policy_from_a_principal_removes_the_pair_both_listings_report() {
    let emulator = Emulator::new();

    let (_, active_certificate) = given_certificate(&emulator, true).await;
    let (_, inactive_certificate) = given_certificate(&emulator, false).await;
    let group = given_thing_group(&emulator, "detach-group").await;

    // (case, the principal, the policy name, which spelling recorded the pair). The rows
    // differ along every axis the rule generalises over: each principal form the modelled
    // member documents, a certificate that is ACTIVE and one that is INACTIVE, the
    // spelling the pair was attached with, and the form of the policy name.
    let cases: Vec<(&str, String, &str, Spelling)> = vec![
        (
            "an ACTIVE certificate, attached the legacy way",
            active_certificate.clone(),
            "DetachMe",
            Spelling::Principal,
        ),
        (
            "an INACTIVE certificate, attached the current way",
            inactive_certificate.clone(),
            "detach_me-2",
            Spelling::Target,
        ),
        (
            "a thing group, attached the legacy way",
            group.clone(),
            "detach+me=3,.@-",
            Spelling::Principal,
        ),
        (
            "a Cognito identity, attached the current way",
            format!("{REGION}:0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9"),
            "detach.me.4",
            Spelling::Target,
        ),
    ];

    for (label, principal, policy_name, attached_with) in &cases {
        given_policy(&emulator, policy_name).await;
        let attached = match attached_with {
            Spelling::Principal => emulator.attach_principal_policy(policy_name, principal).await,
            Spelling::Target => emulator.attach_policy(policy_name, principal).await,
        };
        assert_eq!(
            attached.status, 200,
            "case {label}: the attach must answer 200, got {} {:?}",
            attached.status, attached.body
        );
        assert_eq!(
            emulator
                .list_attached_policies(principal)
                .await
                .policy_names(),
            vec![(*policy_name).to_string()],
            "case {label}: the pair must be recorded before it is removed"
        );

        let detached = emulator.detach_principal_policy(policy_name, principal).await;
        assert_eq!(
            detached.status, 200,
            "case {label}: DetachPrincipalPolicy must answer 200, got {} {:?}",
            detached.status, detached.body
        );
        assert!(
            detached.is_empty_body(),
            "case {label}: the modelled output is Unit, so the body carries no member, \
             got {:?}",
            detached.body
        );

        // Both readers of the relation stop reporting the pair.
        let listed = emulator.list_attached_policies(principal).await;
        assert_eq!(
            listed.status, 200,
            "case {label}: ListAttachedPolicies must answer 200, got {} {:?}",
            listed.status, listed.body
        );
        assert!(
            listed.policy_names().is_empty(),
            "case {label}: the principal must hold no policy afterwards, got {:?}",
            listed.policy_names()
        );
        let targets = emulator.list_targets_for_policy(policy_name).await;
        assert!(
            targets.targets().is_empty(),
            "case {label}: the policy must name no target afterwards, got {:?}",
            targets.targets()
        );

        // IDEMPOTENT: the same call again reports nothing different, and the pair is
        // still gone.
        let again = emulator.detach_principal_policy(policy_name, principal).await;
        assert_eq!(
            again.status, 200,
            "case {label}: a repeated detach must answer 200, got {} {:?}",
            again.status, again.body
        );
        assert!(
            emulator
                .list_attached_policies(principal)
                .await
                .policy_names()
                .is_empty(),
            "case {label}: a repeated detach changes nothing"
        );

        // And what the attachment held back is possible again: nothing names the policy,
        // so the delete of it succeeds and the policy is gone.
        let deleted = emulator.delete_policy(policy_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {label}: the detached policy must be deletable, got {} {:?}",
            deleted.status, deleted.body
        );
        let read = emulator.get_policy(policy_name).await;
        assert_eq!(
            read.status, 404,
            "case {label}: the deleted policy must be gone, got {} {:?}",
            read.status, read.body
        );
    }
}

/// RULE: the detach removes exactly the pair it names — the principal's other policies and
/// the policy's other principals stay attached — and either spelling removes what either
/// spelling recorded.
#[tokio::test]
async fn the_detach_removes_exactly_the_pair_it_names() {
    let emulator = Emulator::new();

    let (_, certificate) = given_certificate(&emulator, true).await;
    let group = given_thing_group(&emulator, "shared-group").await;
    for policy_name in ["first-policy", "second-policy", "third-policy"] {
        given_policy(&emulator, policy_name).await;
    }

    // Three policies on the certificate, and one of them also on the thing group: the
    // relation holds rows in both directions for the detach to leave alone.
    for policy_name in ["first-policy", "second-policy", "third-policy"] {
        let attached = emulator.attach_principal_policy(policy_name, &certificate).await;
        assert_eq!(
            attached.status, 200,
            "attaching {policy_name} must answer 200, got {} {:?}",
            attached.status, attached.body
        );
    }
    let attached = emulator.attach_principal_policy("second-policy", &group).await;
    assert_eq!(
        attached.status, 200,
        "attaching second-policy to the group must answer 200, got {} {:?}",
        attached.status, attached.body
    );

    // (case, the spelling the detach is driven with, the policy it names, what the
    // certificate must hold afterwards). The rows alternate the spelling, so neither is
    // the only one that removes a row.
    let rows: Vec<(&str, Spelling, &str, Vec<&str>)> = vec![
        (
            "the legacy spelling removes one of three",
            Spelling::Principal,
            "first-policy",
            vec!["second-policy", "third-policy"],
        ),
        (
            "the current spelling removes what the legacy attach recorded",
            Spelling::Target,
            "third-policy",
            vec!["second-policy"],
        ),
        (
            "the legacy spelling removes the last one",
            Spelling::Principal,
            "second-policy",
            vec![],
        ),
    ];

    for (label, spelling, policy_name, remaining) in &rows {
        let detached = match spelling {
            Spelling::Principal => {
                emulator
                    .detach_principal_policy(policy_name, &certificate)
                    .await
            }
            Spelling::Target => emulator.detach_policy(policy_name, &certificate).await,
        };
        assert_eq!(
            detached.status, 200,
            "case {label}: the detach must answer 200, got {} {:?}",
            detached.status, detached.body
        );
        let listed = emulator.list_attached_policies(&certificate).await;
        let expected: Vec<String> = remaining.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            listed.policy_names(),
            expected,
            "case {label}: expected {expected:?} left on the certificate, got {:?}",
            listed.policy_names()
        );
    }

    // The thing group's row was never named by any of the detaches above, so it stands.
    let listed = emulator.list_attached_policies(&group).await;
    assert_eq!(
        listed.policy_names(),
        vec!["second-policy".to_string()],
        "the group's own attachment is untouched by the certificate's detaches, got {:?}",
        listed.policy_names()
    );
    let targets = emulator.list_targets_for_policy("second-policy").await;
    assert_eq!(
        targets.targets(),
        vec![group.clone()],
        "the policy must still name the group as a target, got {:?}",
        targets.targets()
    );
}

/// RULE: the documented teardown of a device certificate — detach its policy, then delete
/// the certificate, then delete the policy — completes, because the detach is what both
/// delete guards read.
#[tokio::test]
async fn the_detach_releases_both_deletes_the_attachment_held_back() {
    let emulator = Emulator::new();

    // Issued INACTIVE, which is the state a certificate is deletable in; the rows differ
    // in which spelling recorded the attachment, so neither delete depends on it.
    for (label, policy_name, attached_with) in [
        ("attached the legacy way", "teardown-legacy", Spelling::Principal),
        ("attached the current way", "teardown-current", Spelling::Target),
    ] {
        let (certificate_id, certificate) = given_certificate(&emulator, false).await;
        given_policy(&emulator, policy_name).await;
        let attached = match attached_with {
            Spelling::Principal => {
                emulator
                    .attach_principal_policy(policy_name, &certificate)
                    .await
            }
            Spelling::Target => emulator.attach_policy(policy_name, &certificate).await,
        };
        assert_eq!(
            attached.status, 200,
            "case {label}: the attach must answer 200, got {} {:?}",
            attached.status, attached.body
        );

        // While the pair is recorded, both deletes are held back: that is the state the
        // detach has to change.
        let held = emulator.delete_certificate(&certificate_id).await;
        assert_eq!(
            held.status, 409,
            "case {label}: an attached certificate must be held back, got {} {:?}",
            held.status, held.body
        );
        let held_policy = emulator.delete_policy(policy_name).await;
        assert_eq!(
            held_policy.status, 409,
            "case {label}: an attached policy must be held back, got {} {:?}",
            held_policy.status, held_policy.body
        );

        let detached = emulator
            .detach_principal_policy(policy_name, &certificate)
            .await;
        assert_eq!(
            detached.status, 200,
            "case {label}: DetachPrincipalPolicy must answer 200, got {} {:?}",
            detached.status, detached.body
        );

        let deleted = emulator.delete_certificate(&certificate_id).await;
        assert_eq!(
            deleted.status, 200,
            "case {label}: the detached certificate must be deletable, got {} {:?}",
            deleted.status, deleted.body
        );
        let deleted_policy = emulator.delete_policy(policy_name).await;
        assert_eq!(
            deleted_policy.status, 200,
            "case {label}: the detached policy must be deletable, got {} {:?}",
            deleted_policy.status, deleted_policy.body
        );
    }
}

/// RULE: a principal of no supported form is refused 400 and a well-formed one naming a
/// resource this account does not hold 404 — and a refused detach removes nothing.
#[tokio::test]
async fn a_detach_from_a_principal_that_does_not_exist_is_refused() {
    let emulator = Emulator::new();

    let (_, certificate) = given_certificate(&emulator, true).await;
    given_policy(&emulator, "held-policy").await;
    let attached = emulator
        .attach_principal_policy("held-policy", &certificate)
        .await;
    assert_eq!(
        attached.status, 200,
        "the pair every refusal below must leave standing is recorded first, got {} {:?}",
        attached.status, attached.body
    );

    // A certificate id this account holds no certificate under: 64 hex characters, the
    // modelled `CertificateId` form (`length 64..64, pattern ^(0x)?[a-fA-F0-9]+$`).
    let not_found: Vec<(&str, String, String)> = vec![
        (
            "an unknown certificate principal",
            certificate_arn(&"cd".repeat(32)),
            "The certificate given in the principal does not exist.".to_string(),
        ),
        (
            "an unknown thing group principal",
            thing_group_arn("no-such-group"),
            format!("ThingGroup {ACCOUNT_ID}:no-such-group not found."),
        ),
    ];
    for (label, principal, message) in &not_found {
        let refused = emulator.detach_principal_policy("held-policy", principal).await;
        assert_eq!(
            refused.status, 404,
            "case {label}: expected 404, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("ResourceNotFoundException"),
            "case {label}: expected ResourceNotFoundException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            &refused.message(),
            message,
            "case {label}: expected the message {message:?}, got {:?}",
            refused.message()
        );
    }

    // Strings that name no principal of this account's policies at all. The third field
    // is whether the value is an ARN, which is what decides whether the refusal names it:
    // the API names the address it read, and names nothing when it read none.
    let invalid = vec![
        ("a bare name", "plant-floor".to_string(), false),
        ("a path with no ARN prefix", "cert/abc".to_string(), false),
        (
            "an ARN of another service",
            format!("arn:aws:iam::{ACCOUNT_ID}:role/device"),
            true,
        ),
        (
            "an IoT ARN of a resource no policy attaches to",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/sensor-1"),
            true,
        ),
        (
            "a certificate ARN with no id",
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:cert/"),
            true,
        ),
        ("a Cognito identity with no id", format!("{REGION}:"), false),
    ];
    for (label, principal, names_the_principal) in &invalid {
        let refused = emulator.detach_principal_policy("held-policy", principal).await;
        assert_eq!(
            refused.status, 400,
            "case {label}: expected 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/"),
            "case {label}: expected InvalidRequestException, got {:?}",
            refused.error_code
        );
        assert_eq!(
            refused.message(),
            // One wording rule, stated once: the principal-policy plane resolves its
            // `principal` through the same resolver the target plane resolves `target`
            // with.
            crate::policy_attachments::invalid_target_message(principal, *names_the_principal),
            "case {label}: unexpected refusal message {:?}",
            refused.message()
        );
    }

    // Nothing above removed the pair that was recorded.
    let listed = emulator.list_attached_policies(&certificate).await;
    assert_eq!(
        listed.policy_names(),
        vec!["held-policy".to_string()],
        "a refused detach removes nothing, got {:?}",
        listed.policy_names()
    );
}

/// RULE: the removal reports nothing about whether the pair was there, so a policy name
/// nothing is attached under — one the account holds no policy under included — is the
/// same 200 as a pair that was removed.
#[tokio::test]
async fn detaching_a_pair_the_relation_does_not_hold_is_the_same_success() {
    let emulator = Emulator::new();

    let (_, certificate) = given_certificate(&emulator, true).await;
    let group = given_thing_group(&emulator, "untouched-group").await;
    given_policy(&emulator, "existing-policy").await;

    // (case, the policy named, the principal named): a policy that exists but is attached
    // to nothing, a policy the account holds nothing under, and both over two principal
    // forms — none of them names a recorded pair.
    let rows: Vec<(&str, &str, String)> = vec![
        (
            "a policy that exists, attached to nothing",
            "existing-policy",
            certificate.clone(),
        ),
        (
            "a policy the account holds nothing under",
            "never-created-policy",
            certificate.clone(),
        ),
        (
            "a policy that exists, over a thing group it is not attached to",
            "existing-policy",
            group.clone(),
        ),
        (
            "a policy the account holds nothing under, over a thing group",
            "never-created-policy",
            group.clone(),
        ),
    ];

    for (label, policy_name, principal) in &rows {
        let detached = emulator.detach_principal_policy(policy_name, principal).await;
        assert_eq!(
            detached.status, 200,
            "case {label}: expected 200, got {} {:?}",
            detached.status, detached.body
        );
        assert!(
            detached.is_empty_body(),
            "case {label}: the modelled output is Unit, got {:?}",
            detached.body
        );
    }

    // And nothing was written: the policy that exists is still attached to nothing, and
    // the name the account holds nothing under still names no policy.
    assert!(
        emulator
            .list_targets_for_policy("existing-policy")
            .await
            .targets()
            .is_empty(),
        "the detaches recorded no attachment"
    );
    assert_eq!(
        emulator.get_policy("never-created-policy").await.status,
        404,
        "the detach created no policy"
    );
}
