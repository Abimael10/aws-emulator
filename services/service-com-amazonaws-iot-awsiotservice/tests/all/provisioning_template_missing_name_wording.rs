// SPDX-License-Identifier: CC0-1.0

//! How each operation that addresses a fleet provisioning template BY NAME refuses a
//! `templateName` the account holds no template under, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is the status, the error
//! shape and the `message` an SDK surfaces, not an internal function's return value.
//!
//! The rule under test: the refusal of a missing provisioning template is each operation's
//! OWN sentence, so the lister of a template's versions states the template as not found,
//! the reader of ONE version states it could not find "a template" with that name, the
//! operations addressing the template RECORD state the same without the article, and the
//! delete of a template states what it could not DO and names no template at all. No one
//! of those wordings stands in for another, and all of them carry the modelled
//! `ResourceNotFoundException` (HTTP 404,
//! `smithy_query(query="operation",operation="ListProvisioningTemplateVersions")`). The
//! only variable part any of them has is the name the request carried in its URI label.
//!
//! The table spans what the rule generalises over: every operation that takes
//! `templateName` as an `@httpLabel`, and the value classes of the modelled `TemplateName`
//! (`length 1..36`, `pattern ^[0-9A-Za-z_-]+$`) — the shortest name, an ordinary one, each
//! punctuation and digit the pattern admits, and the longest.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for. None of these refusals is regional.
const REGION: &str = "ap-south-1";
/// The role a template provisions under: its form is the caller's, so the one template this
/// suite creates uses one well-formed ARN.
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

    async fn list_versions(&self, template_name: &str) -> Reply {
        self.dispatch(
            "GET",
            &format!("/provisioning-templates/{template_name}/versions"),
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

/// A provisioning template body: a `Resources` section declaring the thing provisioning
/// creates.
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

/// One operation that addresses a template by name: the request to make, and the sentence
/// it states a name the account holds none under with.
struct Addressing {
    /// The operation, as the case name in every assertion.
    operation: &'static str,
    method: &'static str,
    /// The path, with `{name}` standing for the `templateName` label.
    path: &'static str,
    /// The request body, for the operations that carry one.
    body: Option<fn() -> Value>,
    /// The sentence, with `{name}` standing for the name the request carried.
    message: &'static str,
}

/// Every operation whose `templateName` is an `@httpLabel`, each with its OWN sentence —
/// the wordings this rule says never stand in for one another.
const ADDRESSINGS: &[Addressing] = &[
    Addressing {
        operation: "ListProvisioningTemplateVersions",
        method: "GET",
        path: "/provisioning-templates/{name}/versions",
        body: None,
        message: "Template named {name} not found",
    },
    Addressing {
        operation: "DescribeProvisioningTemplateVersion",
        method: "GET",
        path: "/provisioning-templates/{name}/versions/1",
        body: None,
        message: "Unable to find a template named {name}",
    },
    Addressing {
        operation: "DescribeProvisioningTemplate",
        method: "GET",
        path: "/provisioning-templates/{name}",
        body: None,
        message: "Unable to find template named {name}",
    },
    Addressing {
        operation: "CreateProvisioningTemplateVersion",
        method: "POST",
        path: "/provisioning-templates/{name}/versions",
        body: Some(version_body),
        message: "Unable to find template named {name}",
    },
    Addressing {
        operation: "DeleteProvisioningTemplateVersion",
        method: "DELETE",
        path: "/provisioning-templates/{name}/versions/1",
        body: None,
        message: "Unable to find template named {name}",
    },
    Addressing {
        operation: "UpdateProvisioningTemplate",
        method: "PATCH",
        path: "/provisioning-templates/{name}",
        body: Some(enablement_body),
        message: "Unable to find template named {name}",
    },
    Addressing {
        operation: "DeleteProvisioningTemplate",
        method: "DELETE",
        path: "/provisioning-templates/{name}",
        body: None,
        message: "Unable to delete template, template not found",
    },
];

/// The body a version create carries.
fn version_body() -> Value {
    json!({ "templateBody": template_body(2) })
}

/// The body an update carries: a member the update may set on a template that exists.
fn enablement_body() -> Value {
    json!({ "enabled": true })
}

/// RULE: every operation addressing a provisioning template by name refuses a name the
/// account holds no template under with 404 `ResourceNotFoundException` and its OWN
/// sentence, the only variable part being the name the request carried.
#[tokio::test]
async fn each_operation_states_a_missing_provisioning_template_its_own_way() {
    let emulator = Emulator::new();
    // One template the account DOES hold, so a refusal below is the miss of the requested
    // name and not the operation refusing everything.
    let held = "held_Template-1";
    assert_eq!(
        emulator.create_template(held).await.status,
        200,
        "the template the served case addresses must be creatable"
    );

    // `TemplateName` is `length 1..36`, `^[0-9A-Za-z_-]+$`: the shortest name, an ordinary
    // one, every punctuation and digit class the pattern admits, and the longest.
    let missing = [
        "q".to_string(),
        "fleet-template".to_string(),
        "Fleet_Template-42".to_string(),
        "0123456789".to_string(),
        "n".repeat(36),
    ];

    for addressing in ADDRESSINGS {
        for template_name in &missing {
            let path = addressing.path.replace("{name}", template_name);
            let body = addressing.body.map(|make| make());
            let reply = emulator
                .dispatch(addressing.method, &path, body.as_ref())
                .await;
            let expected = addressing.message.replace("{name}", template_name);
            assert_eq!(
                (reply.status, reply.error_code(), reply.error_message()),
                (404, NOT_FOUND.to_string(), expected.clone()),
                "case {} on the missing template {template_name:?}: expected 404 {NOT_FOUND} \
                 {expected:?}, got {} {}",
                addressing.operation,
                reply.status,
                reply.text()
            );
        }
    }

    // The template the account holds is still served by the lister, so the sentence above
    // is the miss's and the listing's own reporting is untouched.
    let served = emulator.list_versions(held).await;
    assert_eq!(
        served.status,
        200,
        "case the template the account holds: expected 200, got {} body {}",
        served.status,
        served.text()
    );
    let versions = served.json()["versions"]
        .as_array()
        .unwrap_or_else(|| panic!("the listing reports a versions array, got {}", served.text()))
        .clone();
    assert_eq!(
        versions.len(),
        1,
        "case the template the account holds: expected its one version, got {}",
        served.text()
    );
    assert_eq!(
        versions[0]["versionId"],
        json!(1),
        "case the template the account holds: expected version 1, got {}",
        served.text()
    );
}

/// RULE: the lister states the miss of the TEMPLATE, whatever page of the version set the
/// request asked for, with the name reproduced verbatim from the request's URI label.
#[tokio::test]
async fn the_version_listing_states_the_missing_template_whatever_the_page_asked_for() {
    let emulator = Emulator::new();
    let template_name = "unheld-template";

    // The rows differ in the page the request asks for: no paging members at all, and a
    // page size at each end and in the middle of the modelled `MaxResults` range (1..250).
    for (case, query) in [
        ("no paging members", String::new()),
        ("the smallest page", "?maxResults=1".to_string()),
        ("a mid-range page", "?maxResults=25".to_string()),
        ("the largest page", "?maxResults=250".to_string()),
    ] {
        let reply = emulator
            .dispatch(
                "GET",
                &format!("/provisioning-templates/{template_name}/versions{query}"),
                None,
            )
            .await;
        let expected = format!("Template named {template_name} not found");
        assert_eq!(
            (reply.status, reply.error_code(), reply.error_message()),
            (404, NOT_FOUND.to_string(), expected.clone()),
            "case the missing template with {case}: expected 404 {NOT_FOUND} {expected:?}, got \
             {} {}",
            reply.status,
            reply.text()
        );
    }
}
