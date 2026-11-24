// SPDX-License-Identifier: CC0-1.0

//! The PROVISIONING ROLE behind a fleet-provisioning template, end to end over the wire,
//! with AWS IoT and IAM mounted on ONE engine — the shape `core/router/src/lib.rs` mounts
//! them in — so the role a `CreateProvisioningTemplate` names is a role created with
//! `CreateRole`.
//!
//! The rules asserted here:
//!
//! * a template PROVISIONS under the role it names, so a create naming a role IAM answers
//!   this service cannot assume is refused (HTTP 400 `InvalidRequestException`, "The
//!   provisioning role cannot be assumed by AWS IoT") with NOTHING created;
//! * a create naming a role whose trust policy admits this service succeeds;
//! * because the refused create records nothing, `CreateProvisioningClaim` for that
//!   template name then answers the modelled `ResourceNotFoundException` (HTTP 404,
//!   "Unable to find template named <templateName>") instead of minting a claim
//!   certificate no device could ever exchange — and for a template that WAS created it
//!   mints one.
//!
//! The table spans what the rule generalises over rather than one recorded request:
//! WHICH answer IAM gives about the role (it holds one trusting this service, one
//! trusting another service, one trusting nobody, and no role under that name at all).

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    SERVICE_PRINCIPAL, UNASSUMABLE_PROVISIONING_ROLE_MESSAGE,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::topic_rule_kinesis_action::{form_encode, xml_text};

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The modelled error a refused create carries (`@httpError 400`).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The modelled error a claim for a template nobody created carries (`@httpError 404`).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// A service principal that is NOT this service, for a role trusting somebody else.
const OTHER_PRINCIPAL: &str = "lambda.amazonaws.com";

/// AWS IoT and IAM on ONE shared engine, behind the real router.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router }
    }

    /// One IoT control-plane call: restJson1, the body carrying the operation's members.
    async fn iot(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("host", format!("iot.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        Reply { status, raw }
    }

    /// One IAM call over the `awsQuery` protocol, answered with its XML body.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, &str)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://iam.{REGION}.amazonaws.com/"))
            .header("host", format!("iam.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDTESTIOT/20260101/{REGION}/iam/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", "application/x-www-form-urlencoded")
            .body(bytes::Bytes::from(body.into_bytes()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(&response.into_body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: {action} answered {status}: {text}"
        );
        text
    }

    /// A role whose trust policy admits `principals`, answered with the ARN IAM
    /// persisted.
    async fn given_role(&self, case: &str, name: &str, principals: &[&str]) -> String {
        let trust = trust_policy(principals);
        let created = self
            .iam(
                case,
                "CreateRole",
                &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
            )
            .await;
        xml_text(&created, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: CreateRole reported no Arn: {created}"))
    }

    /// `POST /provisioning-templates` — `CreateProvisioningTemplate`.
    async fn create_template(&self, template_name: &str, role_arn: &str) -> Reply {
        self.iot(
            "POST",
            "/provisioning-templates",
            Some(&json!({
                "templateName": template_name,
                "templateBody": template_body(),
                "provisioningRoleArn": role_arn,
                "enabled": true,
            })),
        )
        .await
    }

    /// `GET /provisioning-templates/{templateName}` — `DescribeProvisioningTemplate`, the
    /// reader that proves what a create persisted and that a refused create persisted
    /// nothing.
    async fn describe_template(&self, template_name: &str) -> Reply {
        self.iot(
            "GET",
            &format!("/provisioning-templates/{template_name}"),
            None,
        )
        .await
    }

    /// `POST /provisioning-templates/{templateName}/provisioning-claim` —
    /// `CreateProvisioningClaim`.
    async fn create_claim(&self, template_name: &str) -> Reply {
        self.iot(
            "POST",
            &format!("/provisioning-templates/{template_name}/provisioning-claim"),
            Some(&json!({})),
        )
        .await
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    raw: bytes::Bytes,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.raw).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.raw)
            .unwrap_or_else(|error| panic!("a response body is JSON ({error}): {}", self.text()))
    }

    fn member(&self, name: &str) -> Option<String> {
        self.json()[name].as_str().map(str::to_string)
    }

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }
}

/// A trust policy admitting each of `principals` to assume the role; one admitting nobody
/// when the list is empty.
///
/// Shared with the suites that resolve another caller-supplied role against this same IAM
/// (`crate::dependent_resource_preflight`), so one spelling of a trust document serves
/// them all.
pub(crate) fn trust_policy(principals: &[&str]) -> String {
    let statements: Vec<Value> = principals
        .iter()
        .map(|principal| {
            json!({
                "Effect": "Allow",
                "Principal": { "Service": principal },
                "Action": "sts:AssumeRole",
            })
        })
        .collect();
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// A provisioning template body: one `AWS::IoT::Thing` resource provisioned from a serial
/// number, which is the structure AWS documents for a provisioning template.
fn template_body() -> String {
    json!({
        "Parameters": { "SerialNumber": { "Type": "String" } },
        "Resources": {
            "thing": {
                "Type": "AWS::IoT::Thing",
                "Properties": { "ThingName": { "Ref": "SerialNumber" } },
            },
        },
    })
    .to_string()
}

/// One row of the table below: which answer IAM gives about the role a create names, and
/// whether the create is accepted.
struct Case {
    label: &'static str,
    template_name: &'static str,
    /// The role name IAM holds, or `None` for an ARN naming no role of this account.
    role_name: Option<&'static str>,
    /// The service principals the role's trust policy admits.
    principals: &'static [&'static str],
    /// Whether the create is accepted.
    accepted: bool,
}

/// RULE: a template is created only under a role this service can assume, and the claim
/// that template would mint follows what the create recorded.
#[tokio::test]
async fn a_template_is_created_only_under_a_role_this_service_can_assume() {
    // The rows span WHICH answer IAM gives about the role: it holds one trusting this
    // service, one trusting this service beside another, one trusting only another
    // service, and no role under that name at all.
    let rows = [
        Case {
            label: "a role whose trust policy admits this service",
            template_name: "trusted-template",
            role_name: Some("IoTProvisioningRole"),
            principals: &[SERVICE_PRINCIPAL],
            accepted: true,
        },
        Case {
            label: "a role trusting this service beside another",
            template_name: "shared-template",
            role_name: Some("SharedProvisioningRole"),
            principals: &[OTHER_PRINCIPAL, SERVICE_PRINCIPAL],
            accepted: true,
        },
        Case {
            label: "a role whose trust policy admits only another service",
            template_name: "other-service-template",
            role_name: Some("LambdaOnlyRole"),
            principals: &[OTHER_PRINCIPAL],
            accepted: false,
        },
        Case {
            label: "an ARN naming no role this account holds",
            template_name: "absent-role-template",
            role_name: None,
            principals: &[],
            accepted: false,
        },
    ];

    for Case {
        label: case,
        template_name,
        role_name,
        principals,
        accepted,
    } in rows
    {
        let emulator = Emulator::new();
        let role_arn = match role_name {
            Some(role_name) => emulator.given_role(case, role_name, principals).await,
            None => "arn:aws:iam::000000000000:role/NeverCreated".to_string(),
        };

        let created = emulator.create_template(template_name, &role_arn).await;
        if accepted {
            assert_eq!(
                created.status, 200,
                "case {case:?}: the create must succeed, got {} body {}",
                created.status,
                created.text()
            );
            assert_eq!(
                created.member("templateName").as_deref(),
                Some(template_name),
                "case {case:?}: the create reports the template it made, got {}",
                created.text()
            );
            assert_eq!(
                emulator.describe_template(template_name).await.status,
                200,
                "case {case:?}: the created template must be readable"
            );

            // The template exists, so the claim it mints is a certificate.
            let claim = emulator.create_claim(template_name).await;
            assert_eq!(
                claim.status, 200,
                "case {case:?}: a claim for a template that exists must be minted, got {} body \
                 {}",
                claim.status,
                claim.text()
            );
            assert!(
                claim
                    .member("certificateId")
                    .is_some_and(|id| id.len() == 64),
                "case {case:?}: the claim must carry a certificate id, got {}",
                claim.text()
            );
            continue;
        }

        assert_eq!(
            created.status, 400,
            "case {case:?}: a role this service cannot assume must be refused 400, got {} body \
             {}",
            created.status,
            created.text()
        );
        assert_eq!(
            created.error_code(),
            INVALID_REQUEST,
            "case {case:?}: got {}",
            created.text()
        );
        assert_eq!(
            created.member("message").as_deref(),
            Some(UNASSUMABLE_PROVISIONING_ROLE_MESSAGE),
            "case {case:?}: got {}",
            created.text()
        );

        // The refused create recorded nothing, so every later call about that template
        // name says so — including the claim, which mints no certificate for it.
        let described = emulator.describe_template(template_name).await;
        assert_eq!(
            described.status, 404,
            "case {case:?}: a refused create must record no template, got {} body {}",
            described.status,
            described.text()
        );
        let claim = emulator.create_claim(template_name).await;
        assert_eq!(
            claim.status, 404,
            "case {case:?}: a claim for a template nobody created must be refused 404, got {} \
             body {}",
            claim.status,
            claim.text()
        );
        assert_eq!(
            claim.error_code(),
            NOT_FOUND,
            "case {case:?}: got {}",
            claim.text()
        );
        assert_eq!(
            claim.member("message"),
            Some(format!("Unable to find template named {template_name}")),
            "case {case:?}: got {}",
            claim.text()
        );
        assert_eq!(
            claim.json()["certificatePem"],
            Value::Null,
            "case {case:?}: no claim certificate may be minted, got {}",
            claim.text()
        );
    }
}
