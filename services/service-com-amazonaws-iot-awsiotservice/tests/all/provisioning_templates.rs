// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of FLEET PROVISIONING TEMPLATES: the template, its
//! versions, which version is in EFFECT, and the guard that keeps a template from
//! losing the version it provisions with.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what
//! an AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * a template is a name plus numbered bodies and a pointer to the one in effect: the
//!   create stores the first version and points at it, each version create adds one
//!   under a fresh id up to [`MAX_PROVISIONING_TEMPLATE_VERSIONS`], and the reader
//!   reports the body of the version the pointer names;
//! * the DEFAULT version cannot be deleted while it is the default
//!   (`InvalidRequestException`, 400) and becomes deletable once the pointer names
//!   another version;
//! * a body that is not a provisioning template is refused with nothing created, and a
//!   template or version the account does not hold is `ResourceNotFoundException`
//!   (404);
//! * `type` names one of the kinds the model's `TemplateType` enum declares, and any
//!   other value is refused with nothing created;
//! * the store a caller grows is bounded, and the ceiling never turns a name already
//!   taken into a limit refusal.
//!
//! The tags a template's create carries are the tag plane's business, and are covered with
//! every other taggable family in `authorizer_registry::resource_tagging`.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    MAX_PROVISIONING_TEMPLATES, MAX_PROVISIONING_TEMPLATE_VERSIONS,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region this suite signs for; the ARN assertions read it back.
const REGION: &str = "sa-east-1";
/// The role a template provisions under: its form is the caller's, so the suite uses
/// one well-formed ARN throughout.
const PROVISIONING_ROLE: &str = "arn:aws:iam::000000000000:role/IoTProvisioningRole";

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

    async fn create_template(&self, body: &Value) -> Reply {
        self.dispatch("POST", "/provisioning-templates", Some(body))
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

    async fn update_template(&self, template_name: &str, body: &Value) -> Reply {
        self.dispatch(
            "PATCH",
            &format!("/provisioning-templates/{template_name}"),
            Some(body),
        )
        .await
    }

    async fn delete_template(&self, template_name: &str) -> Reply {
        self.dispatch(
            "DELETE",
            &format!("/provisioning-templates/{template_name}"),
            None,
        )
        .await
    }

    async fn create_version(&self, template_name: &str, body: &str, set_as_default: bool) -> Reply {
        self.dispatch(
            "POST",
            &format!("/provisioning-templates/{template_name}/versions?setAsDefault={set_as_default}"),
            Some(&json!({ "templateBody": body })),
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

    async fn delete_version(&self, template_name: &str, version_id: i64) -> Reply {
        self.dispatch(
            "DELETE",
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

/// A provisioning template body, numbered so each version differs.
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

/// RULE: a created template is read back member for member, including the body of the
/// version in effect.
#[tokio::test]
async fn a_created_provisioning_template_is_read_back_member_for_member() {
    let emulator = Emulator::new();

    // The rows span which optional members the create carries and both values of
    // `enabled`.
    let rows: Vec<(&str, Value)> = vec![
        (
            "only the required members",
            json!({
                "templateName": "bare-template",
                "templateBody": template_body(1),
                "provisioningRoleArn": PROVISIONING_ROLE,
            }),
        ),
        (
            "every optional member",
            json!({
                "templateName": "full_template-1",
                "templateBody": template_body(2),
                "provisioningRoleArn": PROVISIONING_ROLE,
                "description": "provisions the fleet",
                "enabled": true,
                "type": "FLEET_PROVISIONING",
                "preProvisioningHook": {
                    "targetArn": "arn:aws:lambda:sa-east-1:000000000000:function:hook",
                    "payloadVersion": "2020-04-01",
                },
                "tags": [{ "Key": "team", "Value": "fleet" }],
            }),
        ),
        (
            "a JITP template that is not enabled",
            json!({
                "templateName": "jitp-template",
                "templateBody": template_body(3),
                "provisioningRoleArn": PROVISIONING_ROLE,
                "enabled": false,
                "type": "JITP",
            }),
        ),
    ];

    for (case, body) in &rows {
        let created = emulator.create_template(body).await;
        assert_eq!(
            created.status,
            200,
            "case {case}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        let template_name = body["templateName"].as_str().expect("a named template");
        let expected_arn =
            format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:provisioningtemplate/{template_name}");
        assert_eq!(
            created.json()["templateArn"],
            json!(expected_arn),
            "case {case}: expected the minted ARN, got {}",
            created.text()
        );
        assert_eq!(
            created.json()["defaultVersionId"],
            json!(1),
            "case {case}: the created body is version 1 and is in effect, got {}",
            created.text()
        );

        let read = emulator.describe_template(template_name).await;
        assert_eq!(
            read.status,
            200,
            "case {case}: expected the read to answer 200, got {} body {}",
            read.status,
            read.text()
        );
        let described = read.json();
        assert_eq!(
            described["templateBody"], body["templateBody"],
            "case {case}: the read reports the body of the version in effect, got {described}"
        );
        assert_eq!(
            described["provisioningRoleArn"], body["provisioningRoleArn"],
            "case {case}: the read reports the stored role, got {described}"
        );
        assert_eq!(
            described["enabled"],
            json!(body.get("enabled").and_then(Value::as_bool).unwrap_or(false)),
            "case {case}: a template created without `enabled` provisions nothing, got {described}"
        );
        assert_eq!(
            described["type"],
            json!(body
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("FLEET_PROVISIONING")),
            "case {case}: the read reports the stored type, got {described}"
        );
        if let Some(hook) = body.get("preProvisioningHook") {
            assert_eq!(
                described["preProvisioningHook"], *hook,
                "case {case}: the read reports the stored hook, got {described}"
            );
        }
        assert!(
            described["creationDate"].is_number(),
            "case {case}: the read reports when the template was created, got {described}"
        );
    }

    // The name is the template's identity, so a second create under it is a conflict.
    let conflict = emulator
        .create_template(&json!({
            "templateName": "bare-template",
            "templateBody": template_body(9),
            "provisioningRoleArn": PROVISIONING_ROLE,
        }))
        .await;
    assert_eq!(
        conflict.status,
        409,
        "case a name already taken: expected 409, got {} body {}",
        conflict.status,
        conflict.text()
    );
    let unchanged = emulator.describe_template("bare-template").await.json();
    assert_eq!(
        unchanged["templateBody"],
        json!(template_body(1)),
        "case a name already taken: the stored template must be untouched, got {unchanged}"
    );
}

/// RULE: the version a template provisions with cannot be deleted while it is the
/// default, and becomes deletable once another version is.
#[tokio::test]
async fn the_default_version_is_deletable_only_once_another_version_is_the_default() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator
            .create_template(&json!({
                "templateName": "guarded",
                "templateBody": template_body(1),
                "provisioningRoleArn": PROVISIONING_ROLE,
            }))
            .await
            .status,
        200,
        "the template the guard is read on must be creatable"
    );

    // A second version that does NOT become the default: version 1 is still in effect.
    let second = emulator
        .create_version("guarded", &template_body(2), false)
        .await;
    assert_eq!(
        second.status,
        200,
        "case a second version: expected 200, got {} body {}",
        second.status,
        second.text()
    );
    assert_eq!(
        second.json()["versionId"],
        json!(2),
        "case a second version: expected a fresh id, got {}",
        second.text()
    );
    assert_eq!(
        second.json()["isDefaultVersion"],
        json!(false),
        "case a second version: a version that did not ask to be the default is not, got {}",
        second.text()
    );

    // The default version is not deletable while it is the default.
    let guarded = emulator.delete_version("guarded", 1).await;
    assert_eq!(
        guarded.status,
        400,
        "case deleting the default version: expected 400, got {} body {}",
        guarded.status,
        guarded.text()
    );
    assert_eq!(
        guarded.error_code(),
        "InvalidRequestException",
        "case deleting the default version: got {}",
        guarded.text()
    );
    assert_eq!(
        guarded.error_message(),
        "Unable to delete template version 1, it is the default version",
        "case deleting the default version: got {}",
        guarded.text()
    );
    assert_eq!(
        emulator.describe_version("guarded", 1).await.status,
        200,
        "case deleting the default version: the refused delete must remove nothing"
    );

    // A version that is NOT the default is deletable, and the delete leaves the rest.
    let third = emulator
        .create_version("guarded", &template_body(3), false)
        .await;
    assert_eq!(third.json()["versionId"], json!(3));
    let deleted_third = emulator.delete_version("guarded", 3).await;
    assert_eq!(
        deleted_third.status,
        200,
        "case deleting a version that is not the default: expected 200, got {} body {}",
        deleted_third.status,
        deleted_third.text()
    );
    assert_eq!(
        emulator.describe_version("guarded", 3).await.status,
        404,
        "case deleting a version that is not the default: it must be gone"
    );

    // Pointing the template at another version frees the one that was the default.
    let repointed = emulator
        .update_template("guarded", &json!({ "defaultVersionId": 2 }))
        .await;
    assert_eq!(
        repointed.status,
        200,
        "case pointing at version 2: expected 200, got {} body {}",
        repointed.status,
        repointed.text()
    );
    let described = emulator.describe_template("guarded").await.json();
    assert_eq!(
        described["defaultVersionId"],
        json!(2),
        "case pointing at version 2: the read must report the new pointer, got {described}"
    );
    assert_eq!(
        described["templateBody"],
        json!(template_body(2)),
        "case pointing at version 2: the read must report that version's body, got {described}"
    );
    let now_deletable = emulator.delete_version("guarded", 1).await;
    assert_eq!(
        now_deletable.status,
        200,
        "case deleting the version that is no longer the default: expected 200, got {} body {}",
        now_deletable.status,
        now_deletable.text()
    );

    // A pointer may only name a version the template holds.
    let unknown_default = emulator
        .update_template("guarded", &json!({ "defaultVersionId": 7 }))
        .await;
    assert_eq!(
        unknown_default.status,
        400,
        "case pointing at a version the template does not hold: expected 400, got {} body {}",
        unknown_default.status,
        unknown_default.text()
    );
    assert_eq!(
        unknown_default.error_message(),
        "Unable to find a template version with ID 7 for template guarded",
        "case pointing at a version the template does not hold: got {}",
        unknown_default.text()
    );

    // The versions live inside the template, so deleting it takes them with it.
    assert_eq!(
        emulator.delete_template("guarded").await.status,
        200,
        "case deleting the template: expected 200"
    );
    for (case, reply, expected) in [
        (
            "the template",
            emulator.describe_template("guarded").await,
            "Unable to find template named guarded",
        ),
        (
            // The reader of ONE VERSION states its miss of the template with its own
            // article, which is a requirement of that operation.
            "its version",
            emulator.describe_version("guarded", 2).await,
            "Unable to find a template named guarded",
        ),
    ] {
        assert_eq!(
            reply.status,
            404,
            "case {case} after the template was deleted: expected 404, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_message(),
            expected,
            "case {case} after the template was deleted: got {}",
            reply.text()
        );
    }
}

/// RULE: a template body that is not a provisioning template is refused with nothing
/// created, and the versions a template holds are capped.
#[tokio::test]
async fn a_body_that_is_no_template_is_refused_and_the_versions_are_capped() {
    let emulator = Emulator::new();

    // The rows break the body rule a different way each: not JSON at all, JSON that is
    // not an object, and an object declaring no resources.
    let malformed = ["not json at all", "[]", r#"{"Parameters":{}}"#];
    for (index, body) in malformed.iter().enumerate() {
        let template_name = format!("malformed-{index}");
        let refused = emulator
            .create_template(&json!({
                "templateName": template_name,
                "templateBody": body,
                "provisioningRoleArn": PROVISIONING_ROLE,
            }))
            .await;
        assert_eq!(
            refused.status,
            400,
            "case {body:?}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {body:?}: got {}",
            refused.text()
        );
        assert_eq!(
            emulator.describe_template(&template_name).await.status,
            404,
            "case {body:?}: a refused create must leave no template behind"
        );
    }

    assert_eq!(
        emulator
            .create_template(&json!({
                "templateName": "capped",
                "templateBody": template_body(1),
                "provisioningRoleArn": PROVISIONING_ROLE,
            }))
            .await
            .status,
        200
    );
    // The create of the template is its first version, so the cap leaves one fewer.
    for index in 2..=MAX_PROVISIONING_TEMPLATE_VERSIONS {
        let created = emulator
            .create_version("capped", &template_body(index), false)
            .await;
        assert_eq!(
            created.status,
            200,
            "case version {index}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
    }
    let over = emulator
        .create_version("capped", &template_body(99), false)
        .await;
    assert_eq!(
        over.status,
        409,
        "case one past the cap: expected 409, got {} body {}",
        over.status,
        over.text()
    );
    assert_eq!(
        over.error_code(),
        "VersionsLimitExceededException",
        "case one past the cap: got {}",
        over.text()
    );
    // The cap holds: the version the refused create would have added is not there.
    let past_the_cap = i64::try_from(MAX_PROVISIONING_TEMPLATE_VERSIONS).expect("a small cap") + 1;
    assert_eq!(
        emulator
            .describe_version("capped", past_the_cap)
            .await
            .status,
        404,
        "case one past the cap: no version may be added past it"
    );

    // A version create for a template the account does not hold.
    let missing = emulator
        .create_version("no-such-template", &template_body(1), false)
        .await;
    assert_eq!(
        missing.status,
        404,
        "case a template the account does not hold: expected 404, got {} body {}",
        missing.status,
        missing.text()
    );
}

/// A create request for `template_name`, carrying the members every create needs.
fn create_request(template_name: &str) -> Value {
    json!({
        "templateName": template_name,
        "templateBody": template_body(4),
        "provisioningRoleArn": PROVISIONING_ROLE,
    })
}

/// The values the model's `TemplateType` enum declares
/// (`smithy_query(query="shape",shape="TemplateType")`).
///
/// Written out from the MODEL on purpose, rather than read from the crate's own
/// `ops::types::TEMPLATE_TYPE_VALUES`: a test that took the implementation's list would
/// agree with it whatever that list said, and so could not fail. Do not deduplicate the
/// two — update this one from the model when the model changes.
const MODELLED_TEMPLATE_TYPES: [&str; 2] = ["FLEET_PROVISIONING", "JITP"];

/// RULE: `type` names one of the kinds of template the model declares; the create stores
/// the kind it was given and its body as the version in effect, and a value outside the
/// modelled set is refused with nothing created.
#[tokio::test]
async fn the_template_type_is_one_of_the_modelled_values() {
    let emulator = Emulator::new();

    // Every modelled value — including `JITP`, which this suite's other cases do not
    // read back through the version reader.
    for template_type in MODELLED_TEMPLATE_TYPES {
        let template_name = format!("typed-{}", template_type.to_ascii_lowercase());
        let body = template_body(5);
        let created = emulator
            .create_template(&json!({
                "templateName": template_name,
                "templateBody": body,
                "provisioningRoleArn": PROVISIONING_ROLE,
                "type": template_type,
            }))
            .await;
        assert_eq!(
            created.status,
            200,
            "case {template_type}: expected 200, got {} body {}",
            created.status,
            created.text()
        );

        let described = emulator.describe_template(&template_name).await.json();
        assert_eq!(
            described["type"],
            json!(template_type),
            "case {template_type}: the read must report the kind the create was given, got \
             {described}"
        );
        // The body the create carried is version 1 and is the one in effect.
        let version = emulator.describe_version(&template_name, 1).await;
        assert_eq!(
            version.status,
            200,
            "case {template_type}: the created body must be readable as version 1, got {} body {}",
            version.status,
            version.text()
        );
        let read = version.json();
        assert_eq!(
            read["templateBody"],
            json!(body),
            "case {template_type}: version 1 must carry the body verbatim, got {read}"
        );
        assert_eq!(
            read["isDefaultVersion"],
            json!(true),
            "case {template_type}: version 1 is the version in effect, got {read}"
        );
    }

    // The rows outside the catalog differ in HOW they miss it: a case variant of a
    // modelled value, an unknown token, a modelled value with trailing space, and the
    // empty string.
    for unknown in ["fleet_provisioning", "JITR", "FLEET_PROVISIONING ", ""] {
        let template_name = "outside-the-catalog";
        let refused = emulator
            .create_template(&json!({
                "templateName": template_name,
                "templateBody": template_body(6),
                "provisioningRoleArn": PROVISIONING_ROLE,
                "type": unknown,
            }))
            .await;
        assert_eq!(
            refused.status,
            400,
            "case type {unknown:?}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case type {unknown:?}: got {}",
            refused.text()
        );
        assert_eq!(
            emulator.describe_template(template_name).await.status,
            404,
            "case type {unknown:?}: a refused create must leave no template behind"
        );
    }
}

/// RULE: the store a caller grows is bounded — at the ceiling a create under a fresh
/// name is refused with the modelled `LimitExceededException` (410) and records nothing,
/// while a name already taken is still the conflict it is below the ceiling, because
/// that create would add no record at all.
#[tokio::test]
async fn the_provisioning_template_store_is_bounded() {
    let emulator = Emulator::new();
    let ceiling = MAX_PROVISIONING_TEMPLATES;

    for index in 0..ceiling {
        let created = emulator
            .create_template(&create_request(&format!("bulk-{index}")))
            .await;
        assert_eq!(
            created.status,
            200,
            "case template {index} below the ceiling of {ceiling}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
    }

    let refused = emulator
        .create_template(&create_request("one-too-many"))
        .await;
    assert_eq!(
        refused.status,
        410,
        "case one past the ceiling of {ceiling}: expected 410, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_code(),
        "LimitExceededException",
        "case one past the ceiling of {ceiling}: got {}",
        refused.text()
    );
    let message = refused.error_message();
    assert!(
        message.contains(&ceiling.to_string()),
        "case one past the ceiling: the refusal must name the limit {ceiling}, got {message:?}"
    );
    assert_eq!(
        emulator.describe_template("one-too-many").await.status,
        404,
        "case one past the ceiling: a refused create must record nothing"
    );

    // A name already taken is still a CONFLICT at the ceiling.
    let conflict = emulator
        .create_template(&create_request("bulk-0"))
        .await;
    assert_eq!(
        conflict.status,
        409,
        "case a taken name at the ceiling: expected 409, got {} body {}",
        conflict.status,
        conflict.text()
    );
    assert_eq!(
        conflict.error_code(),
        "ResourceAlreadyExistsException",
        "case a taken name at the ceiling: got {}",
        conflict.text()
    );
}
