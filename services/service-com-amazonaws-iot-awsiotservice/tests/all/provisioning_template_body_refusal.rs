// SPDX-License-Identifier: CC0-1.0

//! How the fleet-provisioning plane REFUSES a `templateBody` it cannot read as a
//! provisioning template.
//!
//! The rule under test: a `templateBody` that is no provisioning template is refused with
//! the modelled `InvalidRequestException` (HTTP 400) carrying ONE fixed sentence — "The
//! template body is invalid" — whatever way the document fails to be a template, and the
//! refusal names no position in the body, carries no parser complaint and echoes nothing
//! the request sent. Both writers of a template body answer it: the template create
//! (`POST /provisioning-templates`) and the version create
//! (`POST /provisioning-templates/{templateName}/versions`).
//!
//! The rows span the FORMS a body can take: nothing at all, JSON that stops early, text
//! that is not JSON, every JSON kind that is not an object, and objects that are JSON but
//! declare no resources. The suite also drives bodies that ARE templates, so what is
//! asserted is a refusal of the class and not a blanket refusal.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for.
const REGION: &str = "ap-south-1";

/// The role a template provisions under: its form is the caller's, so one well-formed ARN
/// serves every case here.
const PROVISIONING_ROLE: &str = "arn:aws:iam::000000000000:role/IoTProvisioningRole";

/// The sentence AWS IoT refuses a template body it cannot read with.
///
/// Written out here rather than taken from the crate's own constant on purpose: a test
/// that read the implementation's string would agree with it whatever it said.
const INVALID_TEMPLATE_BODY_MESSAGE: &str = "The template body is invalid";

/// One emulator: the shared engine plus a router with this service mounted.
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
            .expect("the provisioning-template request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    async fn create_template(&self, template_name: &str, template_body: &str) -> Reply {
        self.dispatch(
            "POST",
            "/provisioning-templates",
            Some(&json!({
                "templateName": template_name,
                "templateBody": template_body,
                "provisioningRoleArn": PROVISIONING_ROLE,
            })),
        )
        .await
    }

    async fn describe_template(&self, template_name: &str) -> Reply {
        self.dispatch(
            "GET",
            &format!("/provisioning-templates/{template_name}"),
            None,
        )
        .await
    }

    async fn create_version(&self, template_name: &str, template_body: &str) -> Reply {
        self.dispatch(
            "POST",
            &format!("/provisioning-templates/{template_name}/versions?setAsDefault=false"),
            Some(&json!({ "templateBody": template_body })),
        )
        .await
    }

    async fn describe_version(&self, template_name: &str, version_id: i64) -> Reply {
        self.dispatch(
            "GET",
            &format!("/provisioning-templates/{template_name}/versions/{version_id}"),
            None,
        )
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

/// The bodies that are no provisioning template, one row per FORM of failure: an empty
/// body, JSON that stops early (both an object and an array), text that is not JSON at
/// all, each JSON kind that is not an object (array, string, number, boolean, null), and
/// objects that parse but declare no resources.
fn bodies_that_are_no_template() -> Vec<(&'static str, &'static str)> {
    vec![
        ("an empty body", ""),
        ("only whitespace", "   "),
        ("an object that stops at its first key", "{not json"),
        (
            "an object that stops before its closing brace",
            r#"{"Resources":{}"#,
        ),
        ("an array that stops early", "[{\"Resources\":{}}"),
        ("text that is not JSON at all", "not json at all"),
        ("a JSON array", "[]"),
        ("an array carrying a template", r#"[{"Resources":{}}]"#),
        ("a JSON string", r#""Resources""#),
        ("a JSON number", "7"),
        ("a JSON boolean", "true"),
        ("JSON null", "null"),
        ("an empty object", "{}"),
        (
            "an object declaring parameters but no resources",
            r#"{"Parameters":{"SerialNumber":{"Type":"String"}}}"#,
        ),
        (
            "an object whose resources key is misspelled",
            r#"{"resources":{}}"#,
        ),
    ]
}

/// A provisioning template body, numbered so each one differs.
fn template_body(index: usize) -> String {
    json!({
        "Parameters": { "SerialNumber": { "Type": "String" } },
        "Resources": {
            "thing": {
                "Type": "AWS::IoT::Thing",
                "Properties": { "ThingName": format!("device-{index}") },
            },
        },
    })
    .to_string()
}

/// RULE: a `CreateProvisioningTemplate` whose `templateBody` is no provisioning template
/// is refused with `InvalidRequestException` (400) and the one fixed sentence, with no
/// parser position in it, and leaves no template behind.
#[tokio::test]
async fn a_template_create_refuses_every_body_that_is_no_template_with_one_sentence() {
    let emulator = Emulator::new();

    for (index, (case, body)) in bodies_that_are_no_template().into_iter().enumerate() {
        let template_name = format!("refused-{index}");
        let refused = emulator.create_template(&template_name, body).await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {}",
            refused.text()
        );
        let message = refused.error_message();
        assert_eq!(
            message, INVALID_TEMPLATE_BODY_MESSAGE,
            "case {case:?}: expected {INVALID_TEMPLATE_BODY_MESSAGE:?}, got {message:?}"
        );
        // A parser's complaint names a line and a column; AWS's sentence carries no
        // number at all, so a digit in the message is the emulator's reading leaking out.
        assert!(
            !message.chars().any(|character| character.is_ascii_digit()),
            "case {case:?}: the refusal must name no position in the body, got {message:?}"
        );
        assert_eq!(
            emulator.describe_template(&template_name).await.status,
            404,
            "case {case:?}: a refused create must leave no template behind"
        );
    }
}

/// RULE: the same refusal, with the same sentence, answers
/// `CreateProvisioningTemplateVersion`; the template keeps the versions it had.
#[tokio::test]
async fn a_version_create_refuses_every_body_that_is_no_template_with_one_sentence() {
    let emulator = Emulator::new();

    let created = emulator.create_template("held", &template_body(1)).await;
    assert_eq!(
        created.status,
        200,
        "the template the version cases write to must exist: got {} body {}",
        created.status,
        created.text()
    );

    for (case, body) in bodies_that_are_no_template() {
        let refused = emulator.create_version("held", body).await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {case:?}: expected InvalidRequestException, got {}",
            refused.text()
        );
        let message = refused.error_message();
        assert_eq!(
            message, INVALID_TEMPLATE_BODY_MESSAGE,
            "case {case:?}: expected {INVALID_TEMPLATE_BODY_MESSAGE:?}, got {message:?}"
        );
        assert!(
            !message.chars().any(|character| character.is_ascii_digit()),
            "case {case:?}: the refusal must name no position in the body, got {message:?}"
        );
        // Nothing was written: the template still holds only the version its create made.
        assert_eq!(
            emulator.describe_version("held", 2).await.status,
            404,
            "case {case:?}: a refused version create must add no version"
        );
    }
}

/// RULE: the refusal is of the class, not of every body — a body that IS a provisioning
/// template is accepted by both writers and read back.
#[tokio::test]
async fn a_body_that_is_a_template_is_still_accepted_by_both_writers() {
    let emulator = Emulator::new();

    // The rows differ in how much beside `Resources` the body carries, which is the
    // caller's to shape: a bare resources section, one declaring a resource, and one
    // carrying parameters and unrelated members beside it.
    let accepted: Vec<(&str, String)> = vec![
        ("an empty resources section", r#"{"Resources":{}}"#.to_string()),
        ("a resources section declaring a thing", template_body(2)),
        (
            "resources beside parameters and an unrelated member",
            json!({
                "Parameters": { "SerialNumber": { "Type": "String" } },
                "Resources": { "thing": { "Type": "AWS::IoT::Thing" } },
                "DeviceConfiguration": { "fleet": "north" },
            })
            .to_string(),
        ),
    ];

    for (index, (case, body)) in accepted.into_iter().enumerate() {
        let template_name = format!("accepted-{index}");
        let created = emulator.create_template(&template_name, &body).await;
        assert_eq!(
            created.status,
            200,
            "case {case:?}: expected 200, got {} body {}",
            created.status,
            created.text()
        );

        // Read the body back through the version the create stored: what was accepted is
        // what the store holds.
        let version = emulator.describe_version(&template_name, 1).await;
        assert_eq!(
            version.status,
            200,
            "case {case:?}: the create's version must be readable, got {} body {}",
            version.status,
            version.text()
        );
        assert_eq!(
            version.json()["templateBody"],
            json!(body),
            "case {case:?}: the version must hand back the body the create carried, got {}",
            version.text()
        );

        // The version create accepts it too, and the new version holds it.
        let added = emulator.create_version(&template_name, &body).await;
        assert_eq!(
            added.status,
            200,
            "case {case:?}: the version create must accept a template body, got {} body {}",
            added.status,
            added.text()
        );
        let second = emulator.describe_version(&template_name, 2).await;
        assert_eq!(
            second.status,
            200,
            "case {case:?}: the added version must be readable, got {} body {}",
            second.status,
            second.text()
        );
        assert_eq!(
            second.json()["templateBody"],
            json!(body),
            "case {case:?}: the added version must hand back the body it was given, got {}",
            second.text()
        );
    }
}
