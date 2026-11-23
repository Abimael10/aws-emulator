// SPDX-License-Identifier: CC0-1.0

//! How the operations that address ONE VERSION of a fleet provisioning template refuse a
//! `versionId` the template does not hold, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is the status, the
//! error shape and the `message` an SDK surfaces, not an internal function's return
//! value.
//!
//! The rule under test: the operations that LOOK UP one version — the reader of one, and
//! the re-pointing of the version in effect — state the PAIR they could not resolve, the
//! version id the request carried and the template it was looked for within, while the
//! DELETE of one states what it could not DO and names the version alone, the way the
//! delete of a TEMPLATE states its own miss rather than the reader's. The miss of the
//! TEMPLATE itself is a third refusal of the same request, with the template family's own
//! sentence. Which error class carries the refusal is each operation's: a read and a delete
//! of a version answer the modelled `ResourceNotFoundException` (404), while an update
//! re-pointing `defaultVersionId` at a version the template does not hold is the modelled
//! `InvalidRequestException` (400).
//!
//! The table spans what the rule generalises over: the three operations that address one
//! version, a version that was DELETED and versions that never existed, and the value
//! classes of the modelled `versionId`
//! (`smithy_query(query="operation",operation="DescribeProvisioningTemplateVersion")`: an
//! `integer` with no declared range) — the smallest and largest an `integer` carries
//! included.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for. None of these refusals is regional.
const REGION: &str = "eu-north-1";
/// The role a template provisions under: its form is the caller's, so the suite uses one
/// well-formed ARN throughout.
const PROVISIONING_ROLE: &str = "arn:aws:iam::000000000000:role/IoTProvisioningRole";
/// The wire error shape of a resource this service does not hold.
const NOT_FOUND: &str = "ResourceNotFoundException";

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

    async fn create_template(&self, template_name: &str) -> Reply {
        self.dispatch(
            "POST",
            "/provisioning-templates",
            Some(&json!({
                "templateName": template_name,
                "templateBody": template_body(1),
                "provisioningRoleArn": PROVISIONING_ROLE,
            })),
        )
        .await
    }

    async fn create_version(&self, template_name: &str, index: usize) -> Reply {
        self.dispatch(
            "POST",
            &format!("/provisioning-templates/{template_name}/versions?setAsDefault=true"),
            Some(&json!({ "templateBody": template_body(index) })),
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

    async fn point_default_at(&self, template_name: &str, version_id: i64) -> Reply {
        self.dispatch(
            "PATCH",
            &format!("/provisioning-templates/{template_name}"),
            Some(&json!({ "defaultVersionId": version_id })),
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

/// The sentence the operations that LOOK UP one version state a `versionId` the template
/// does not hold with: the version and the template it was looked for within.
fn unresolvable_version_message(template_name: &str, version_id: i64) -> String {
    format!("Unable to find a template version with ID {version_id} for template {template_name}")
}

/// The sentence a DELETE of one version states the same miss with: the version alone.
fn undeletable_version_message(version_id: i64) -> String {
    format!("Unable to find template version {version_id}")
}

/// RULE: a lookup of one version states the version and the template it was looked for
/// within, and a delete of one states the version alone.
#[tokio::test]
async fn a_missing_template_version_is_stated_by_the_operation_that_could_not_use_it() {
    let emulator = Emulator::new();
    let template_name = "fleet_template-1";
    assert_eq!(
        emulator.create_template(template_name).await.status,
        200,
        "the template every case addresses must be creatable"
    );
    // Version 2 becomes the default, which is what makes version 1 deletable: one row
    // below asks for a version that was DELETED rather than one that never existed.
    assert_eq!(
        emulator.create_version(template_name, 2).await.status,
        200,
        "a second version must be creatable"
    );
    assert_eq!(
        emulator.delete_version(template_name, 1).await.status,
        200,
        "the version a row asks about must be deletable once it is no longer the default"
    );

    // `versionId` is a modelled `integer` with no declared range, so the rows span a
    // version that WAS there, ordinary ones that never were, and both ends of the type.
    let version_ids: [(&str, i64); 5] = [
        ("a version that was deleted", 1),
        ("a version that never existed", 7),
        ("a version at the type's floor", i32::MIN as i64),
        ("a version at the type's ceiling", i32::MAX as i64),
        ("a version below the first one", 0),
    ];

    for (case, version_id) in version_ids {
        let expected = unresolvable_version_message(template_name, version_id);
        let expected_delete = undeletable_version_message(version_id);

        let read = emulator.describe_version(template_name, version_id).await;
        assert_eq!(
            (read.status, read.error_code(), read.error_message()),
            (404, NOT_FOUND.to_string(), expected.clone()),
            "case DescribeProvisioningTemplateVersion for {case} ({version_id}): expected 404 \
             {NOT_FOUND} {expected:?}, got {} {}",
            read.status,
            read.text()
        );

        let deleted = emulator.delete_version(template_name, version_id).await;
        assert_eq!(
            (deleted.status, deleted.error_code(), deleted.error_message()),
            (404, NOT_FOUND.to_string(), expected_delete.clone()),
            "case DeleteProvisioningTemplateVersion for {case} ({version_id}): expected 404 \
             {NOT_FOUND} {expected_delete:?}, got {} {}",
            deleted.status,
            deleted.text()
        );

        // Re-pointing the template at a version it does not hold would leave it
        // provisioning from a body nothing stored: that is a LOOKUP of the version too, so
        // it states the same sentence the read does, under the error class the UPDATE
        // models for it.
        let repointed = emulator.point_default_at(template_name, version_id).await;
        assert_eq!(
            (
                repointed.status,
                repointed.error_code(),
                repointed.error_message()
            ),
            (
                400,
                "InvalidRequestException".to_string(),
                expected.clone()
            ),
            "case UpdateProvisioningTemplate for {case} ({version_id}): expected 400 \
             InvalidRequestException {expected:?}, got {} {}",
            repointed.status,
            repointed.text()
        );
    }

    // The version the template DOES hold is served, so the sentences above are the
    // lookup's and not the operation's.
    let held = emulator.describe_version(template_name, 2).await;
    assert_eq!(
        held.status,
        200,
        "case the version the template holds: expected 200, got {} body {}",
        held.status,
        held.text()
    );
    assert_eq!(
        held.json()["versionId"],
        json!(2),
        "case the version the template holds: expected version 2, got {}",
        held.text()
    );
    assert_eq!(
        held.json()["isDefaultVersion"],
        json!(true),
        "case the version the template holds: version 2 is the default, got {}",
        held.text()
    );
}

/// RULE: the miss of the TEMPLATE is a different refusal of the same request, and keeps the
/// template family's own sentence — the version sentence never stands in for it.
#[tokio::test]
async fn a_missing_template_keeps_its_own_sentence() {
    let emulator = Emulator::new();

    // `templateName` is length 1..36, pattern `^[0-9A-Za-z_-]+$`: the shortest name, an
    // ordinary one, each punctuation the pattern admits, and the longest.
    let missing_templates = [
        "a".to_string(),
        "fleet-template".to_string(),
        "Fleet_Template-2".to_string(),
        "t".repeat(36),
    ];

    for template_name in &missing_templates {
        let read = emulator.describe_version(template_name, 1).await;
        assert_eq!(
            (read.status, read.error_code(), read.error_message()),
            (
                404,
                NOT_FOUND.to_string(),
                format!("Unable to find a template named {template_name}")
            ),
            "case DescribeProvisioningTemplateVersion on the missing template \
             {template_name:?}: expected the template family's sentence, got {} {}",
            read.status,
            read.text()
        );
    }
}
