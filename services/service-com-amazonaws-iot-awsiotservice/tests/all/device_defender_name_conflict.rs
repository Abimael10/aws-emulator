// SPDX-License-Identifier: CC0-1.0

//! How this service REFUSES a DEVICE DEFENDER create under a name the account already
//! holds that resource under, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is the status, the
//! `x-amzn-errortype` a client resolves the shape from, the `message` an SDK surfaces and
//! which members the 409 body carries.
//!
//! The RULE under test: a Device Defender named-resource create (a scheduled audit, a
//! mitigation action) refused because the account already holds the resource answers
//! `ResourceAlreadyExistsException` (409) whose message is the family's one fixed clause,
//! `Resource already exists for account with different attributes, `, followed by the ARN
//! of the resource that ALREADY exists — the record's full identity (partition, region,
//! account, resource path), read from the store. Nothing else of the refused request is in
//! the sentence, and the stored record is left exactly as it was.
//!
//! The table spans what that rule generalises over — the family (both creates), the form
//! and length of the name within the pattern both families share (`^[a-zA-Z0-9_-]+$`,
//! length max 128, per `smithy_query(query="shape",shape="ScheduledAuditName,MitigationActionName")`),
//! the region the request is signed for (which the ARN in the sentence must follow, so no
//! region segment is baked in), and HOW the second request differs from the first (a
//! different schedule, a different role, different parameters, different tags) — rather
//! than one recorded request.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use service_com_amazonaws_iot_awsiotservice::state::AUDIT_CHECK_NAMES;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The fixed text of the clause this family states a taken name with: everything but the
/// ARN of the resource that already exists.
const ALREADY_EXISTS_CLAUSE: &str = "Resource already exists for account with different attributes,";

/// The role an account's audit configuration and a mitigation action name.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/device-defender";

/// A DIFFERENT role, so the second create of a mitigation action asks for something other
/// than what the stored record holds.
const REPLACEMENT_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/device-defender-other";

/// A value only the REFUSED request carries. The sentence names the existing resource and
/// nothing of the request, so it may never appear in the message.
const SECOND_ATTEMPT_MARKER: &str = "second-attempt-marker";

/// Which Device Defender named resource a row is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// `CreateScheduledAudit` on `/audit/scheduledaudits/{scheduledAuditName}`.
    ScheduledAudit,
    /// `CreateMitigationAction` on `/mitigationactions/actions/{actionName}`.
    MitigationAction,
}

impl Family {
    /// The path of the single resource `name` names, which both families bind the name to
    /// as an `@httpLabel`.
    fn path(self, name: &str) -> String {
        match self {
            Self::ScheduledAudit => format!("/audit/scheduledaudits/{name}"),
            Self::MitigationAction => format!("/mitigationactions/actions/{name}"),
        }
    }

    /// The resource-type segment of this family's ARNs.
    fn arn_segment(self) -> &'static str {
        match self {
            Self::ScheduledAudit => "scheduledaudit",
            Self::MitigationAction => "mitigationaction",
        }
    }

    /// The member of the create's output that carries the ARN it minted.
    fn arn_member(self) -> &'static str {
        match self {
            Self::ScheduledAudit => "scheduledAuditArn",
            Self::MitigationAction => "actionArn",
        }
    }

    /// The body that creates the resource.
    fn first_body(self) -> Value {
        match self {
            Self::ScheduledAudit => json!({
                "frequency": "DAILY",
                "targetCheckNames": [AUDIT_CHECK_NAMES[0]],
            }),
            Self::MitigationAction => json!({
                "roleArn": ROLE_ARN,
                "actionParams": { "updateDeviceCertificateParams": { "action": "DEACTIVATE" } },
            }),
        }
    }

    /// The body of the second create under the SAME name: a different resource in every
    /// member the family lets a caller choose, carrying a value only this request has.
    fn second_body(self) -> Value {
        match self {
            Self::ScheduledAudit => json!({
                "frequency": "WEEKLY",
                "dayOfWeek": "MON",
                "targetCheckNames": [AUDIT_CHECK_NAMES[1]],
                "tags": [{ "Key": "attempt", "Value": SECOND_ATTEMPT_MARKER }],
            }),
            Self::MitigationAction => json!({
                "roleArn": REPLACEMENT_ROLE_ARN,
                "actionParams": {
                    "replaceDefaultPolicyVersionParams": { "templateName": "BLANK_POLICY" },
                },
                "tags": [{ "Key": "attempt", "Value": SECOND_ATTEMPT_MARKER }],
            }),
        }
    }
}

/// Names of the pattern both families share, `^[a-zA-Z0-9_-]+$` within the modelled
/// length: differing in length (the shortest the pattern admits, and the longest the
/// length allows), case and separator.
fn names() -> Vec<String> {
    vec![
        "a".to_string(),
        "defender-audit_1".to_string(),
        "Edge_Node-7".to_string(),
        "n".repeat(128),
    ]
}

/// The regions the rows sign for, so the ARN the sentence carries is read back from the
/// request rather than from a constant.
fn regions() -> Vec<&'static str> {
    vec!["us-east-1", "eu-west-3", "ap-northeast-1", "sa-east-1"]
}

/// Every row: each family, under each form of name, in a region of its own.
fn cases() -> Vec<(Family, String, &'static str)> {
    let mut rows = Vec::new();
    for family in [Family::ScheduledAudit, Family::MitigationAction] {
        for (index, name) in names().into_iter().enumerate() {
            let regions = regions();
            let region = regions[index % regions.len()];
            rows.push((family, name, region));
        }
    }
    rows
}

/// One emulator: the shared engine plus a router with this service mounted, reached the
/// way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
    region: &'static str,
}

impl Emulator {
    fn new(region: &'static str) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router, region }
    }

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let region = self.region;
        let host = format!("iot.{region}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            // The credential scope names the `iot` signing name the model's
            // `aws.auth#sigv4` trait declares, which is what routes the request, and the
            // region the served ARNs are built for.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the Device Defender request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    /// Enable every audit check for the account, which is what lets a scheduled audit
    /// target one.
    async fn enable_every_check(&self, case: &str) {
        let mut checks = Map::new();
        for check_name in AUDIT_CHECK_NAMES {
            checks.insert((*check_name).to_string(), json!({ "enabled": true }));
        }
        let reply = self
            .dispatch(
                "PATCH",
                "/audit/configuration",
                Some(&json!({
                    "roleArn": ROLE_ARN,
                    "auditCheckConfigurations": Value::Object(checks),
                })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected UpdateAccountAuditConfiguration to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    error_type: Option<String>,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        Self {
            status,
            error_type,
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

    /// The error code a restJson1 error response carries.
    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    /// The message a restJson1 error response carries.
    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }
}

/// RULE: the refusal of a create under a taken name states the family's fixed clause and
/// the ARN of the resource that already exists — for either Device Defender family, any
/// form of name, and whatever region the request is signed for.
#[tokio::test]
async fn a_taken_device_defender_name_is_refused_with_the_existing_resources_arn() {
    for (family, name, region) in cases() {
        let case = format!("{family:?} {name:?} in {region}");
        // A fresh emulator per row: the refusal must depend only on what this row created.
        let emulator = Emulator::new(region);
        emulator.enable_every_check(&case).await;

        let created = emulator
            .dispatch("POST", &family.path(&name), Some(&family.first_body()))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the first create to answer 200, got {} body {}",
            created.status,
            created.text()
        );
        let minted = created.json()[family.arn_member()]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no {} in {}", family.arn_member(), created.text()))
            .to_string();
        let expected_arn = format!(
            "arn:aws:iot:{region}:{ACCOUNT_ID}:{segment}/{name}",
            segment = family.arn_segment()
        );
        assert_eq!(
            minted, expected_arn,
            "case {case:?}: expected the create to mint {expected_arn}, got {minted}"
        );

        let stored = emulator.dispatch("GET", &family.path(&name), None).await;
        assert_eq!(
            stored.status, 200,
            "case {case:?}: expected the created resource to read back, got {} body {}",
            stored.status,
            stored.text()
        );
        let stored_record = stored.json();

        let refused = emulator
            .dispatch("POST", &family.path(&name), Some(&family.second_body()))
            .await;
        assert_eq!(
            refused.status, 409,
            "case {case:?}: expected status 409 from the second create, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceAlreadyExistsException"),
            "case {case:?}: expected x-amzn-errortype ResourceAlreadyExistsException, got {:?}",
            refused.error_type
        );
        assert_eq!(
            refused.error_code(),
            "ResourceAlreadyExistsException",
            "case {case:?}: expected ResourceAlreadyExistsException, got {}",
            refused.text()
        );
        let expected_message = format!("{ALREADY_EXISTS_CLAUSE} {minted}");
        assert_eq!(
            refused.error_message(),
            expected_message,
            "case {case:?}: expected message {expected_message:?}, got {:?}",
            refused.error_message()
        );

        // Nothing of the REFUSED request is in the sentence: only the existing record's
        // ARN varies.
        assert!(
            !refused.error_message().contains(SECOND_ATTEMPT_MARKER),
            "case {case:?}: the sentence must carry no value of the refused request, got {:?}",
            refused.error_message()
        );

        // A refused create writes nothing: the stored resource is still the first one.
        let after = emulator.dispatch("GET", &family.path(&name), None).await;
        assert_eq!(
            after.json(),
            stored_record,
            "case {case:?}: expected the refused create to leave the stored record as it was, \
             got {}",
            after.text()
        );
    }
}

/// RULE: the clause is ONE family's, so both Device Defender creates word one collision
/// one way — the fixed text is identical and only the ARN differs.
#[tokio::test]
async fn both_device_defender_creates_state_the_collision_in_one_clause() {
    let name = "shared-clause";
    let region = "us-west-2";
    let emulator = Emulator::new(region);
    emulator.enable_every_check("the shared clause").await;

    let mut sentences = Vec::new();
    for family in [Family::ScheduledAudit, Family::MitigationAction] {
        let case = format!("{family:?}");
        let created = emulator
            .dispatch("POST", &family.path(name), Some(&family.first_body()))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the first create to answer 200, got {} body {}",
            created.status,
            created.text()
        );
        let refused = emulator
            .dispatch("POST", &family.path(name), Some(&family.second_body()))
            .await;
        let message = refused.error_message();
        let arn = format!(
            "arn:aws:iot:{region}:{ACCOUNT_ID}:{segment}/{name}",
            segment = family.arn_segment()
        );
        assert_eq!(
            message,
            format!("{ALREADY_EXISTS_CLAUSE} {arn}"),
            "case {case:?}: expected the family's clause with this resource's ARN, got {message:?}"
        );
        sentences.push((family, message));
    }

    // The two sentences differ in the ARN alone.
    let fixed_text: Vec<String> = sentences
        .iter()
        .map(|(family, message)| {
            message
                .replace(family.arn_segment(), "<segment>")
                .replace(name, "<name>")
        })
        .collect();
    assert_eq!(
        fixed_text[0], fixed_text[1],
        "expected one fixed text for both families, got {:?} and {:?}",
        sentences[0].1, sentences[1].1
    );
}
