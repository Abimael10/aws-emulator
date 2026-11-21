// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `RegisterThing`: provisioning ONE device from a
//! provisioning template supplied inline.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * a `RegisterThing` provisions every resource the template's `Resources` section
//!   declares, with the call's `parameters` substituted into them, and answers each
//!   provisioned resource's ARN under the LOGICAL ID the template declares it as — every
//!   one of which addresses a record the registry that owns it hands back
//!   (`DescribeThing`, `GetPolicy`, `DescribeCertificate`, `ListAttachedPolicies`);
//! * a `templateBody` that is no provisioning template is refused and nothing is
//!   provisioned;
//! * a template declaring a resource this service cannot provision against the account's
//!   state fails the registration, and nothing is provisioned.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    existing_thing_message, missing_policy_message, MAX_POLICIES_PER_TARGET,
};
use service_com_amazonaws_iot_awsiotservice::state::thing_provisioning::{
    json_syntax_detail, template_format_error_message, undescribed_resource_message,
    unheld_certificate_message, unmodeled_certificate_status_message, unnamed_certificate_message,
    unnamed_policy_message, unprovisionable_thing_message, unreadable_policy_document_message,
    unresolvable_reference_message, unsupported_resource_type_message, NOT_AN_OBJECT_DETAIL,
    NO_RESOURCES_DETAIL, NO_THING_RESOURCE_DETAIL,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for.
const REGION: &str = "eu-west-2";

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

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

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
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
            .expect("the RegisterThing request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    /// `RegisterThing`: `POST /things` carrying the template body and the parameters.
    async fn register_thing(&self, template_body: &Value, parameters: &Value) -> Reply {
        self.call(
            "POST",
            "/things",
            Some(&json!({
                "templateBody": template_body.to_string(),
                "parameters": parameters,
            })),
        )
        .await
    }

    /// `RegisterThing` with a `templateBody` that is not even JSON.
    async fn register_thing_with_text(&self, template_body: &str) -> Reply {
        self.call(
            "POST",
            "/things",
            Some(&json!({ "templateBody": template_body })),
        )
        .await
    }

    /// `DescribeThing`: `GET /things/{thingName}`.
    async fn describe_thing(&self, thing_name: &str) -> Reply {
        self.call("GET", &format!("/things/{thing_name}"), None).await
    }

    /// `CreateThing`: `POST /things/{thingName}`.
    async fn create_thing(&self, thing_name: &str, body: &Value) -> Reply {
        self.call("POST", &format!("/things/{thing_name}"), Some(body))
            .await
    }

    /// `CreateThingType`: `POST /thing-types/{thingTypeName}`.
    async fn create_thing_type(&self, thing_type_name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/thing-types/{thing_type_name}"),
            Some(&json!({})),
        )
        .await
    }

    /// `GetPolicy`: `GET /policies/{policyName}`.
    async fn get_policy(&self, policy_name: &str) -> Reply {
        self.call("GET", &format!("/policies/{policy_name}"), None)
            .await
    }

    /// `CreatePolicy`: `POST /policies/{policyName}`.
    async fn create_policy(&self, policy_name: &str, document: &Value) -> Reply {
        self.call(
            "POST",
            &format!("/policies/{policy_name}"),
            Some(&json!({ "policyDocument": document.to_string() })),
        )
        .await
    }

    /// `CreateKeysAndCertificate`: `POST /keys-and-certificate`, which issues the
    /// certificate a provisioning template names.
    async fn create_certificate(&self) -> Reply {
        self.call("POST", "/keys-and-certificate?setAsActive=false", None)
            .await
    }

    /// `AttachPolicy`: `PUT /target-policies/{policyName}` carrying the target.
    async fn attach_policy(&self, policy_name: &str, target: &str) -> Reply {
        self.call(
            "PUT",
            &format!("/target-policies/{policy_name}"),
            Some(&json!({ "target": target })),
        )
        .await
    }

    /// `DescribeCertificate`: `GET /certificates/{certificateId}`.
    async fn describe_certificate(&self, certificate_id: &str) -> Reply {
        self.call("GET", &format!("/certificates/{certificate_id}"), None)
            .await
    }

    /// `ListAttachedPolicies`: `POST /attached-policies/{target}`, whose target is ONE
    /// path segment.
    async fn list_attached_policies(&self, target: &str) -> Reply {
        let segment = target
            .replace('%', "%25")
            .replace(':', "%3A")
            .replace('/', "%2F");
        self.call("POST", &format!("/attached-policies/{segment}"), None)
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

/// A certificate id of the modelled 64 hexadecimal characters that no account holds.
const ABSENT_CERTIFICATE_ID: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The ARN a thing registered in this suite's region carries.
fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// The ARN a policy created in this suite's region carries.
fn policy_arn(policy_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:policy/{policy_name}")
}

/// The ARN of a certificate in this suite's region.
fn certificate_arn(certificate_id: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:cert/{certificate_id}")
}

/// An IoT policy document a device may be provisioned with.
fn device_policy_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["iot:Connect", "iot:Publish"],
            "Resource": "*",
        }],
    })
}

/// RULE: a `RegisterThing` provisions every resource the template declares, with the
/// call's parameters substituted into it, and reports each ARN under the logical id the
/// template declared the resource as — and each of those records reads back.
#[tokio::test]
async fn every_resource_a_template_declares_is_provisioned_and_reads_back() {
    // The rows span what the rule generalises over: how the thing's name is arrived at (a
    // parameter the call supplies, a default the template declares, a literal), which
    // resources the template declares beside the thing, and the logical ids it declares
    // them under.
    struct Case {
        label: &'static str,
        template: Value,
        parameters: Value,
        thing_name: &'static str,
        thing_logical_id: &'static str,
        attributes: Value,
        thing_type_name: Option<&'static str>,
        policy: Option<(&'static str, &'static str)>,
    }

    let cases = vec![
        Case {
            label: "a thing alone, named by a parameter the call supplies",
            template: json!({
                "Parameters": { "SerialNumber": { "Type": "String" } },
                "Resources": {
                    "device": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": { "ThingName": { "Ref": "SerialNumber" } },
                    },
                },
            }),
            parameters: json!({ "SerialNumber": "sn-0001" }),
            thing_name: "sn-0001",
            thing_logical_id: "device",
            attributes: json!({}),
            thing_type_name: None,
            policy: None,
        },
        Case {
            label: "a thing named by the default the template declares",
            template: json!({
                "Parameters": { "SerialNumber": { "Type": "String", "Default": "sn-default" } },
                "Resources": {
                    "thing_resource": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": { "ThingName": { "Ref": "SerialNumber" } },
                    },
                },
            }),
            parameters: json!({}),
            thing_name: "sn-default",
            thing_logical_id: "thing_resource",
            attributes: json!({}),
            thing_type_name: None,
            policy: None,
        },
        Case {
            label: "a thing with attributes and a thing type",
            template: json!({
                "Parameters": { "Model": { "Type": "String" } },
                "Resources": {
                    "thing": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": {
                            "ThingName": "lit-thing",
                            "ThingTypeName": "sensor",
                            "AttributePayload": {
                                "attributes": { "model": { "Ref": "Model" }, "line": "alpha" },
                            },
                        },
                    },
                },
            }),
            parameters: json!({ "Model": "x1" }),
            thing_name: "lit-thing",
            thing_logical_id: "thing",
            attributes: json!({ "model": "x1", "line": "alpha" }),
            thing_type_name: Some("sensor"),
            policy: None,
        },
        Case {
            label: "a thing and a policy created from the document the template states",
            template: json!({
                "Resources": {
                    "thing": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": { "ThingName": "policied-thing" },
                    },
                    "policy": {
                        "Type": "AWS::IoT::Policy",
                        "Properties": {
                            "PolicyName": "device-policy",
                            "PolicyDocument": device_policy_document(),
                        },
                    },
                },
            }),
            parameters: json!({}),
            thing_name: "policied-thing",
            thing_logical_id: "thing",
            attributes: json!({}),
            thing_type_name: None,
            policy: Some(("policy", "device-policy")),
        },
        Case {
            label: "a thing and a policy the account already holds",
            template: json!({
                "Resources": {
                    "the_thing": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": { "ThingName": "held-policy-thing" },
                    },
                    "the_policy": {
                        "Type": "AWS::IoT::Policy",
                        "Properties": { "PolicyName": "held-policy" },
                    },
                },
            }),
            parameters: json!({}),
            thing_name: "held-policy-thing",
            thing_logical_id: "the_thing",
            attributes: json!({}),
            thing_type_name: None,
            policy: Some(("the_policy", "held-policy")),
        },
    ];

    for case in cases {
        let label = case.label;
        let emulator = Emulator::new();
        if let Some(thing_type_name) = case.thing_type_name {
            let created = emulator.create_thing_type(thing_type_name).await;
            assert_eq!(
                created.status,
                200,
                "case {label}: the thing type setup must succeed, got {} body {}",
                created.status,
                created.text()
            );
        }
        // The row that names a policy the account already holds needs it held FIRST, so the
        // provisioning reuses it rather than creating it.
        if label == "a thing and a policy the account already holds" {
            let created = emulator
                .create_policy("held-policy", &device_policy_document())
                .await;
            assert_eq!(
                created.status,
                200,
                "case {label}: the policy setup must succeed, got {} body {}",
                created.status,
                created.text()
            );
        }

        let registered = emulator
            .register_thing(&case.template, &case.parameters)
            .await;
        assert_eq!(
            registered.status,
            200,
            "case {label}: expected 200, got {} body {}",
            registered.status,
            registered.text()
        );

        // The ARNs are keyed by the LOGICAL ID the template declared each resource under.
        let mut expected_arns = serde_json::Map::new();
        expected_arns.insert(
            case.thing_logical_id.to_string(),
            json!(thing_arn(case.thing_name)),
        );
        if let Some((logical_id, policy_name)) = case.policy {
            expected_arns.insert(logical_id.to_string(), json!(policy_arn(policy_name)));
        }
        assert_eq!(
            registered.json()["resourceArns"],
            Value::Object(expected_arns),
            "case {label}: the ARNs must be keyed by logical id, got {}",
            registered.text()
        );
        assert_eq!(
            registered.json()["certificatePem"],
            Value::Null,
            "case {label}: a template declaring no certificate reports no PEM, got {}",
            registered.text()
        );

        // Every ARN addresses a record the registry that owns it hands back.
        let described = emulator.describe_thing(case.thing_name).await;
        assert_eq!(
            described.status,
            200,
            "case {label}: the provisioned thing must read back, got {} body {}",
            described.status,
            described.text()
        );
        let thing = described.json();
        assert_eq!(
            thing["thingArn"],
            json!(thing_arn(case.thing_name)),
            "case {label}: the read must report the ARN the registration did, got {}",
            described.text()
        );
        assert_eq!(
            thing["attributes"],
            case.attributes,
            "case {label}: the attributes the template substituted must be on record, got {}",
            described.text()
        );
        assert_eq!(
            thing["thingTypeName"],
            case.thing_type_name.map_or(Value::Null, |name| json!(name)),
            "case {label}: the thing type the template named must be on record, got {}",
            described.text()
        );

        if let Some((_, policy_name)) = case.policy {
            let policy = emulator.get_policy(policy_name).await;
            assert_eq!(
                policy.status,
                200,
                "case {label}: the provisioned policy must read back, got {} body {}",
                policy.status,
                policy.text()
            );
            assert_eq!(
                policy.json()["policyArn"],
                json!(policy_arn(policy_name)),
                "case {label}: the read must report the ARN the registration did, got {}",
                policy.text()
            );
            assert_eq!(
                policy.json()["policyDocument"],
                json!(device_policy_document().to_string()),
                "case {label}: the policy must carry the document it was provisioned with, \
                 got {}",
                policy.text()
            );
        }
    }
}

/// RULE: a template declaring a certificate names a certificate the account holds; the
/// call reports that record's PEM and ARN, leaves it in the status the template asks for,
/// and attaches the declared policy to it.
#[tokio::test]
async fn a_declared_certificate_and_policy_are_provisioned_onto_the_certificate() {
    let emulator = Emulator::new();
    let issued = emulator.create_certificate().await;
    assert_eq!(
        issued.status,
        200,
        "the certificate setup must succeed, got {} body {}",
        issued.status,
        issued.text()
    );
    let certificate_id = issued.json()["certificateId"]
        .as_str()
        .unwrap_or_else(|| panic!("no certificateId in {}", issued.text()))
        .to_string();
    let certificate_pem = issued.json()["certificatePem"]
        .as_str()
        .unwrap_or_else(|| panic!("no certificatePem in {}", issued.text()))
        .to_string();

    // The rows span how a template may NAME the one certificate the account holds: by the
    // id a parameter supplies, and by the PEM of the record itself.
    let cases = [
        (
            "the certificate named by the id a parameter supplies",
            "sn-by-id",
            json!({ "CertificateId": { "Ref": "CertificateId" }, "Status": "ACTIVE" }),
            json!({ "CertificateId": certificate_id.clone() }),
        ),
        (
            "the certificate named by its PEM",
            "sn-by-pem",
            json!({ "CertificatePem": certificate_pem.clone(), "Status": "ACTIVE" }),
            json!({}),
        ),
    ];

    for (label, thing_name, certificate_properties, parameters) in cases {
        let template = json!({
            "Parameters": { "CertificateId": { "Type": "String" } },
            "Resources": {
                "thing": {
                    "Type": "AWS::IoT::Thing",
                    "Properties": { "ThingName": thing_name },
                },
                "certificate": {
                    "Type": "AWS::IoT::Certificate",
                    "Properties": certificate_properties,
                },
                "policy": {
                    "Type": "AWS::IoT::Policy",
                    "Properties": {
                        "PolicyName": "fleet-policy",
                        "PolicyDocument": device_policy_document(),
                    },
                },
            },
        });

        let registered = emulator.register_thing(&template, &parameters).await;
        assert_eq!(
            registered.status,
            200,
            "case {label}: expected 200, got {} body {}",
            registered.status,
            registered.text()
        );
        let body = registered.json();
        assert_eq!(
            body["resourceArns"],
            json!({
                "thing": thing_arn(thing_name),
                "certificate": certificate_arn(&certificate_id),
                "policy": policy_arn("fleet-policy"),
            }),
            "case {label}: the ARNs must be keyed by logical id, got {}",
            registered.text()
        );
        assert_eq!(
            body["certificatePem"],
            json!(certificate_pem),
            "case {label}: the PEM reported must be the stored certificate's, got {}",
            registered.text()
        );

        // The certificate is left in the status the template asked for, which its own
        // reader reports.
        let described = emulator.describe_certificate(&certificate_id).await;
        assert_eq!(
            described.status,
            200,
            "case {label}: the certificate must read back, got {} body {}",
            described.status,
            described.text()
        );
        assert_eq!(
            described.json()["certificateDescription"]["status"],
            json!("ACTIVE"),
            "case {label}: the template asked for ACTIVE, got {}",
            described.text()
        );

        // The device's policy governs it through the certificate it connects with, so the
        // attachment is recorded in the relation `ListAttachedPolicies` reads.
        let attached = emulator
            .list_attached_policies(&certificate_arn(&certificate_id))
            .await;
        assert_eq!(
            attached.status,
            200,
            "case {label}: the attachment listing must succeed, got {} body {}",
            attached.status,
            attached.text()
        );
        assert_eq!(
            attached.json()["policies"],
            json!([{
                "policyName": "fleet-policy",
                "policyArn": policy_arn("fleet-policy"),
            }]),
            "case {label}: the provisioned policy must be attached to the certificate, got {}",
            attached.text()
        );

        // The thing itself is on record under the name the template gave it.
        let thing = emulator.describe_thing(thing_name).await;
        assert_eq!(
            thing.status,
            200,
            "case {label}: the provisioned thing must read back, got {} body {}",
            thing.status,
            thing.text()
        );
    }
}

/// RULE: a `templateBody` that is no provisioning template is refused with the modelled
/// `InvalidRequestException` and nothing is provisioned.
#[tokio::test]
async fn a_body_that_is_no_provisioning_template_is_refused() {
    // The rows span the forms a body that is no provisioning template takes: not JSON at
    // all — on the first line and on a later one, since the parser's own line and column
    // are what the refusal reports — JSON that is no object, an object declaring no
    // `Resources`, and one whose `Resources` declare no thing.
    let cases = [
        (
            "a syntax error on the first line",
            "{ 'Resources': {} }".to_string(),
            template_format_error_message(&json_syntax_detail(1, 3)),
        ),
        (
            "a syntax error on a later line",
            "{\n  \"Resources\": {},\n  'Parameters': {}\n}".to_string(),
            template_format_error_message(&json_syntax_detail(3, 3)),
        ),
        (
            "a body that ends mid-template",
            "{\"Resources\":".to_string(),
            template_format_error_message(&json_syntax_detail(1, 13)),
        ),
        (
            "a JSON list",
            json!([{ "Resources": {} }]).to_string(),
            template_format_error_message(NOT_AN_OBJECT_DETAIL),
        ),
        (
            "a JSON string",
            json!("a template").to_string(),
            template_format_error_message(NOT_AN_OBJECT_DETAIL),
        ),
        (
            "an object declaring no Resources",
            json!({ "Parameters": { "SerialNumber": { "Type": "String" } } }).to_string(),
            template_format_error_message(NO_RESOURCES_DETAIL),
        ),
        (
            "Resources declaring no thing",
            json!({
                "Resources": {
                    "policy": {
                        "Type": "AWS::IoT::Policy",
                        "Properties": { "PolicyName": "orphan-policy" },
                    },
                },
            })
            .to_string(),
            template_format_error_message(NO_THING_RESOURCE_DETAIL),
        ),
    ];

    for (label, template_body, expected_message) in cases {
        let emulator = Emulator::new();
        let refused = emulator.register_thing_with_text(&template_body).await;
        assert_eq!(
            refused.status,
            400,
            "case {label}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {label}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.error_message(),
            expected_message,
            "case {label}: the refusal must name the rule broken, got {}",
            refused.text()
        );

        // Nothing is provisioned: the policy the last row declares is not in the registry.
        let policy = emulator.get_policy("orphan-policy").await;
        assert_eq!(
            policy.status,
            404,
            "case {label}: a refused registration provisions nothing, got {} body {}",
            policy.status,
            policy.text()
        );
    }
}

/// RULE: a policy resource stating a `PolicyDocument` and no `PolicyName` is provisioned
/// under a name this service mints, and that policy reads back under the ARN reported.
#[tokio::test]
async fn a_policy_stated_by_its_document_alone_is_minted_a_name() {
    // The rows span the forms a template may state the document in: the JSON object it is,
    // and the text of one.
    let cases = [
        ("the document as a JSON object", device_policy_document()),
        (
            "the document as the text of one",
            json!(device_policy_document().to_string()),
        ),
    ];

    for (label, stated_document) in cases {
        let emulator = Emulator::new();
        let template = json!({
            "Resources": {
                "thing": {
                    "Type": "AWS::IoT::Thing",
                    "Properties": { "ThingName": "minted-policy-thing" },
                },
                "policy": {
                    "Type": "AWS::IoT::Policy",
                    "Properties": { "PolicyDocument": stated_document },
                },
            },
        });

        let registered = emulator.register_thing(&template, &json!({})).await;
        assert_eq!(
            registered.status,
            200,
            "case {label}: expected 200, got {} body {}",
            registered.status,
            registered.text()
        );
        let arns = registered.json();
        let reported = arns["resourceArns"]["policy"]
            .as_str()
            .unwrap_or_else(|| panic!("case {label}: no policy ARN in {}", registered.text()))
            .to_string();
        let minted_name = reported
            .rsplit_once(":policy/")
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| panic!("case {label}: {reported} is no IoT policy ARN"));
        assert_ne!(
            minted_name, "policy",
            "case {label}: the name must be minted, not the logical id, got {reported}"
        );

        // The minted name addresses a policy the registry holds, carrying the document the
        // template stated.
        let policy = emulator.get_policy(&minted_name).await;
        assert_eq!(
            policy.status,
            200,
            "case {label}: the minted policy must read back, got {} body {}",
            policy.status,
            policy.text()
        );
        assert_eq!(
            policy.json()["policyArn"],
            json!(reported),
            "case {label}: the read must report the ARN the registration did, got {}",
            policy.text()
        );
        assert_eq!(
            policy.json()["policyDocument"],
            json!(device_policy_document().to_string()),
            "case {label}: the minted policy must carry the document stated, got {}",
            policy.text()
        );
    }
}

/// RULE: a `templateBody` declaring a resource this service cannot carry out — one that
/// names nothing to provision, or describes a policy that is no IoT policy — is refused
/// with the modelled `InvalidRequestException`, and nothing is provisioned.
#[tokio::test]
async fn a_declaration_that_names_nothing_to_provision_is_refused() {
    let thing_resource = json!({
        "Type": "AWS::IoT::Thing",
        "Properties": { "ThingName": { "Ref": "SerialNumber" } },
    });
    // The rows span the kinds of declaration this service cannot carry out: a certificate
    // named by nothing, one asking for a status no caller may set (an internal-only one, and
    // one outside the modelled `CertificateStatus` altogether), a policy named by nothing, a
    // policy document that is no IoT policy document, a resource of a kind nothing
    // provisions, a resource describing nothing, and a thing whose name no parameter
    // supplies.
    let cases: Vec<(&str, Value, Value, String)> = vec![
        (
            "a certificate named by nothing",
            json!({
                "Resources": {
                    "thing": thing_resource.clone(),
                    "cert": {
                        "Type": "AWS::IoT::Certificate",
                        "Properties": { "Status": "ACTIVE" },
                    },
                },
            }),
            json!({ "SerialNumber": "sn-unnamed-cert" }),
            unnamed_certificate_message("cert"),
        ),
        (
            // `PENDING_ACTIVATION` is modelled by `CertificateStatus` but is the service's
            // own: no writer of a certificate's status may set it.
            "a certificate asking for an internal-only status",
            json!({
                "Resources": {
                    "thing": thing_resource.clone(),
                    "cert": {
                        "Type": "AWS::IoT::Certificate",
                        "Properties": {
                            "CertificateId": ABSENT_CERTIFICATE_ID,
                            "Status": "PENDING_ACTIVATION",
                        },
                    },
                },
            }),
            json!({ "SerialNumber": "sn-internal-status" }),
            unmodeled_certificate_status_message("cert", "PENDING_ACTIVATION"),
        ),
        (
            "a certificate asking for a status the model does not declare",
            json!({
                "Resources": {
                    "thing": thing_resource.clone(),
                    "cert": {
                        "Type": "AWS::IoT::Certificate",
                        "Properties": {
                            "CertificateId": ABSENT_CERTIFICATE_ID,
                            "Status": "HALF_ON",
                        },
                    },
                },
            }),
            json!({ "SerialNumber": "sn-unmodelled-status" }),
            unmodeled_certificate_status_message("cert", "HALF_ON"),
        ),
        (
            "a policy stating neither a name nor a document",
            json!({
                "Resources": {
                    "thing": thing_resource.clone(),
                    "pol": {
                        "Type": "AWS::IoT::Policy",
                        "Properties": {},
                    },
                },
            }),
            json!({ "SerialNumber": "sn-unnamed-policy" }),
            unnamed_policy_message("pol"),
        ),
        (
            "a policy document that is no IoT policy document",
            json!({
                "Resources": {
                    "thing": thing_resource.clone(),
                    "pol": {
                        "Type": "AWS::IoT::Policy",
                        "Properties": {
                            "PolicyName": "bad-policy",
                            "PolicyDocument": { "Version": "2012-10-17", "NotStatement": [] },
                        },
                    },
                },
            }),
            json!({ "SerialNumber": "sn-bad-policy" }),
            unreadable_policy_document_message(
                "Policy document is Malformed: Missing required field Statement",
            ),
        ),
        (
            "a resource of a kind nothing provisions",
            json!({
                "Resources": {
                    "thing": thing_resource.clone(),
                    "queue": { "Type": "AWS::SQS::Queue", "Properties": {} },
                },
            }),
            json!({ "SerialNumber": "sn-queue" }),
            unsupported_resource_type_message("queue", "AWS::SQS::Queue"),
        ),
        (
            "a resource describing nothing",
            json!({
                "Resources": {
                    "thing": thing_resource.clone(),
                    "blank": { "Type": "AWS::IoT::Policy" },
                },
            }),
            json!({ "SerialNumber": "sn-blank" }),
            undescribed_resource_message("blank"),
        ),
        (
            // The name is a reference, and the parameter it names has no value here, so the
            // refusal is about the REFERENCE rather than about the resource: the rule that
            // owns it is pinned by `a_reference_nothing_resolves_is_refused_as_the_reference`.
            "a thing whose name no parameter supplies",
            json!({ "Resources": { "thing": thing_resource.clone() } }),
            json!({ "Model": "x1" }),
            unresolvable_reference_message("SerialNumber"),
        ),
        (
            // Every reference resolves here, and the resource still describes no thing the
            // registry can be addressed by: a thing is named by its `ThingName`.
            "a thing resource stating no ThingName",
            json!({
                "Resources": {
                    "thing": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": {
                            "AttributePayload": { "serialNumber": { "Ref": "SerialNumber" } },
                        },
                    },
                },
            }),
            json!({ "SerialNumber": "sn-nameless" }),
            unprovisionable_thing_message(),
        ),
    ];

    for (label, template, parameters, expected_message) in cases {
        let emulator = Emulator::new();
        let refused = emulator.register_thing(&template, &parameters).await;
        assert_eq!(
            refused.status,
            400,
            "case {label}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {label}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.error_message(),
            expected_message,
            "case {label}: the refusal must name the declaration at fault, got {}",
            refused.text()
        );

        // Nothing the template declared was provisioned: neither the policy it named nor
        // the thing it described.
        let policy = emulator.get_policy("bad-policy").await;
        assert_eq!(
            policy.status,
            404,
            "case {label}: a refused registration provisions no policy, got {} body {}",
            policy.status,
            policy.text()
        );
        if let Some(serial) = parameters["SerialNumber"].as_str() {
            let thing = emulator.describe_thing(serial).await;
            assert_eq!(
                thing.status,
                404,
                "case {label}: a refused registration registers no thing, got {} body {}",
                thing.status,
                thing.text()
            );
        }
    }
}

/// RULE: a declaration this service can carry out that the account's STATE will not take
/// fails the registration with the modelled `ResourceRegistrationFailureException`, and
/// nothing is provisioned.
#[tokio::test]
async fn a_resource_the_account_will_not_take_fails_the_registration() {
    let thing_resource = json!({
        "Type": "AWS::IoT::Thing",
        "Properties": { "ThingName": { "Ref": "SerialNumber" } },
    });

    // The rows span the ways the account's state refuses a resource a template declares: a
    // certificate named by an id it holds none under, one named by a PEM it holds none of,
    // a policy named by a name it holds none under with no document to create one from, and
    // a thing name it holds with a different configuration.
    let cases: Vec<(&str, Value, Value, String)> = vec![
        (
            "a certificate id the account holds none under",
            json!({
                "Resources": {
                    "thing": thing_resource.clone(),
                    "cert": {
                        "Type": "AWS::IoT::Certificate",
                        "Properties": { "CertificateId": ABSENT_CERTIFICATE_ID },
                    },
                },
            }),
            json!({ "SerialNumber": "sn-absent-cert" }),
            unheld_certificate_message(Some(ABSENT_CERTIFICATE_ID)),
        ),
        (
            "a certificate PEM the account holds none of",
            json!({
                "Resources": {
                    "thing": thing_resource.clone(),
                    "cert": {
                        "Type": "AWS::IoT::Certificate",
                        "Properties": {
                            "CertificatePem": "-----BEGIN CERTIFICATE-----\nAAAA\n\
                                               -----END CERTIFICATE-----\n",
                        },
                    },
                },
            }),
            json!({ "SerialNumber": "sn-absent-pem" }),
            unheld_certificate_message(None),
        ),
        (
            "a policy the account holds none under, with no document",
            json!({
                "Resources": {
                    "thing": thing_resource.clone(),
                    "pol": {
                        "Type": "AWS::IoT::Policy",
                        "Properties": { "PolicyName": "absent-policy" },
                    },
                },
            }),
            json!({ "SerialNumber": "sn-absent-policy" }),
            missing_policy_message("absent-policy"),
        ),
        (
            "a thing name the registry holds with a different configuration",
            json!({ "Resources": { "thing": thing_resource } }),
            json!({ "SerialNumber": "taken-thing" }),
            existing_thing_message("taken-thing"),
        ),
    ];

    for (label, template, parameters, expected_message) in cases {
        let emulator = Emulator::new();
        // The conflict row needs the name held FIRST, with a configuration the template
        // does not describe.
        if label == "a thing name the registry holds with a different configuration" {
            let created = emulator
                .create_thing(
                    "taken-thing",
                    &json!({ "attributePayload": { "attributes": { "owner": "someone" } } }),
                )
                .await;
            assert_eq!(
                created.status,
                200,
                "case {label}: the thing setup must succeed, got {} body {}",
                created.status,
                created.text()
            );
        }

        let refused = emulator.register_thing(&template, &parameters).await;
        assert_eq!(
            refused.status,
            400,
            "case {label}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "ResourceRegistrationFailureException",
            "case {label}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.error_message(),
            expected_message,
            "case {label}: the refusal must name the resource the account would not take, \
             got {}",
            refused.text()
        );

        // Nothing the template declared was provisioned: neither the policy it named nor
        // the thing it described.
        let policy = emulator.get_policy("absent-policy").await;
        assert_eq!(
            policy.status,
            404,
            "case {label}: a failed registration provisions no policy, got {} body {}",
            policy.status,
            policy.text()
        );
        if let Some(serial) = parameters["SerialNumber"].as_str() {
            if serial != "taken-thing" {
                let thing = emulator.describe_thing(serial).await;
                assert_eq!(
                    thing.status,
                    404,
                    "case {label}: a failed registration registers no thing, got {} body {}",
                    thing.status,
                    thing.text()
                );
            }
        }
    }
}

/// RULE: a declaration the attachment relation will not take fails the registration before
/// anything is written — the thing is not registered and the policy is not created.
///
/// The ceiling under test is this service's own on policies per target
/// (`MAX_POLICIES_PER_TARGET`): a certificate already holding that many attached policies
/// cannot take the one the template declares.
#[tokio::test]
async fn a_policy_the_certificate_cannot_take_leaves_nothing_provisioned() {
    let emulator = Emulator::new();
    let issued = emulator.create_certificate().await;
    assert_eq!(
        issued.status,
        200,
        "the certificate setup must succeed, got {} body {}",
        issued.status,
        issued.text()
    );
    let certificate_id = issued.json()["certificateId"]
        .as_str()
        .unwrap_or_else(|| panic!("no certificateId in {}", issued.text()))
        .to_string();
    let target = certificate_arn(&certificate_id);

    // Fill the certificate's attachments up to the ceiling through the operations that own
    // the relation, so the state the registration meets is one a client could have built.
    for index in 0..MAX_POLICIES_PER_TARGET {
        let policy_name = format!("filler-{index}");
        let created = emulator
            .create_policy(&policy_name, &device_policy_document())
            .await;
        assert_eq!(
            created.status,
            200,
            "case filler {index}: the policy setup must succeed, got {} body {}",
            created.status,
            created.text()
        );
        let attached = emulator.attach_policy(&policy_name, &target).await;
        assert_eq!(
            attached.status,
            200,
            "case filler {index}: the attach setup must succeed, got {} body {}",
            attached.status,
            attached.text()
        );
    }

    let template = json!({
        "Resources": {
            "thing": {
                "Type": "AWS::IoT::Thing",
                "Properties": { "ThingName": "over-the-ceiling" },
            },
            "certificate": {
                "Type": "AWS::IoT::Certificate",
                "Properties": { "CertificateId": certificate_id },
            },
            "policy": {
                "Type": "AWS::IoT::Policy",
                "Properties": {
                    "PolicyName": "one-too-many",
                    "PolicyDocument": device_policy_document(),
                },
            },
        },
    });

    let refused = emulator.register_thing(&template, &json!({})).await;
    assert_eq!(
        refused.status,
        400,
        "case the ceiling: expected 400, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_code(),
        "ResourceRegistrationFailureException",
        "case the ceiling: got {}",
        refused.text()
    );
    assert!(
        refused
            .error_message()
            .contains(&MAX_POLICIES_PER_TARGET.to_string()),
        "case the ceiling: the refusal must name the limit {MAX_POLICIES_PER_TARGET}, got {}",
        refused.error_message()
    );

    // Nothing was provisioned: the thing is not in the registry and the policy was not
    // created, so the refusal did not leave the account half-provisioned.
    let thing = emulator.describe_thing("over-the-ceiling").await;
    assert_eq!(
        thing.status,
        404,
        "case the ceiling: a refused registration registers no thing, got {} body {}",
        thing.status,
        thing.text()
    );
    let policy = emulator.get_policy("one-too-many").await;
    assert_eq!(
        policy.status,
        404,
        "case the ceiling: a refused registration creates no policy, got {} body {}",
        policy.status,
        policy.text()
    );
}

/// The one-based line, and the one-based column in bytes, at which `token` first occurs in
/// `body`: where a caller reading the refusal's position is pointed in its own body.
fn position_of(body: &str, token: &str) -> (usize, usize) {
    let at = body
        .find(token)
        .unwrap_or_else(|| panic!("the case body {body:?} must carry {token:?}"));
    let before = &body[..at];
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    (1 + before.matches('\n').count(), 1 + at - line_start)
}

/// RULE: where a `templateBody` is not well-formed JSON, the position the refusal reports is
/// the one-based line and column of the FIRST character of the construct the parse could not
/// read — never a position inside it or past it.
#[tokio::test]
async fn a_template_syntax_refusal_reports_where_the_fault_starts() {
    // Each row names the body and the TEXT of the construct at fault; what is asserted is
    // that the reported position is where that text starts. The rows span the two ways a
    // parse stops — part-way THROUGH the offending text (a bare word that begins one of
    // JSON's literal tokens `null`, `true` and `false` is matched character by character
    // until one does not match) and ON its first character (nothing that could start a value
    // or a member name was there) — at the body's very first character, later on its first
    // line, and on a line further down.
    let cases = [
        (
            "a word beginning a literal, at the first character of the body",
            "not a provisioning template".to_string(),
            "not",
        ),
        (
            "a word beginning another literal, at the first character of the body",
            "this is not a provisioning template".to_string(),
            "this",
        ),
        (
            "a word beginning the third literal, at the first character of the body",
            "fleet template, unquoted".to_string(),
            "fleet",
        ),
        (
            "a character that can start no value, at the first character of the body",
            "#not a provisioning template".to_string(),
            "#",
        ),
        (
            "text after a value that parsed",
            "false-ish".to_string(),
            "-",
        ),
        (
            "a word beginning a literal inside a declaration",
            "{\"Resources\": [nope]}".to_string(),
            "nope",
        ),
        (
            "a word beginning a literal on a later line",
            "{\n  \"Resources\": nothing\n}".to_string(),
            "nothing",
        ),
        (
            "an unquoted member name on a later line",
            "{\n  Resources: {}\n}".to_string(),
            "Resources",
        ),
        (
            "a member name quoted the way JSON does not",
            "{ 'Resources': {} }".to_string(),
            "'",
        ),
    ];

    for (label, body, offender) in cases {
        let emulator = Emulator::new();
        let refused = emulator.register_thing_with_text(&body).await;
        assert_eq!(
            refused.status,
            400,
            "case {label}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {label}: got {}",
            refused.text()
        );
        let (line, column) = position_of(&body, offender);
        assert_eq!(
            refused.error_message(),
            template_format_error_message(&json_syntax_detail(line, column)),
            "case {label}: the refusal must report line {line}, column {column} — where \
             {offender:?} starts in {body:?} — got {}",
            refused.error_message()
        );
    }
}

/// RULE: a template reference `{"Ref": "<parameter>"}` the call supplies no value for and the
/// template declares no `Default` for is refused with the modelled `InvalidRequestException`
/// naming the REFERENCE it could not resolve, whichever resource and whichever property
/// carries it.
#[tokio::test]
async fn a_reference_nothing_resolves_is_refused_as_the_reference() {
    // The rows span where a reference may sit — the thing's name, an attribute of its
    // payload, a certificate property and a policy property — and the parameter names differ
    // from row to row, because the name reported is the one the reference carries.
    let cases: Vec<(&str, Value, Value, &str, Option<&str>)> = vec![
        (
            "the thing's name, with nothing supplied at all",
            json!({
                "Resources": {
                    "thing": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": { "ThingName": { "Ref": "ThingName" } },
                    },
                },
            }),
            json!({}),
            "ThingName",
            None,
        ),
        (
            "the thing's name, with another parameter supplied",
            json!({
                "Resources": {
                    "thing": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": { "ThingName": { "Ref": "SerialNumber" } },
                    },
                },
            }),
            json!({ "Model": "x1" }),
            "SerialNumber",
            None,
        ),
        (
            "an attribute of the thing's payload",
            json!({
                "Resources": {
                    "thing": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": {
                            "ThingName": "referencing-attribute-thing",
                            "AttributePayload": {
                                "attributes": { "location": { "Ref": "Location" } },
                            },
                        },
                    },
                },
            }),
            json!({ "SerialNumber": "sn-attribute" }),
            "Location",
            Some("referencing-attribute-thing"),
        ),
        (
            "the thing's type",
            json!({
                "Resources": {
                    "thing": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": {
                            "ThingName": "referencing-type-thing",
                            "ThingTypeName": { "Ref": "DeviceFamily" },
                        },
                    },
                },
            }),
            json!({}),
            "DeviceFamily",
            Some("referencing-type-thing"),
        ),
        (
            "a certificate's id",
            json!({
                "Resources": {
                    "thing": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": { "ThingName": "referencing-certificate-thing" },
                    },
                    "cert": {
                        "Type": "AWS::IoT::Certificate",
                        "Properties": { "CertificateId": { "Ref": "CertificateId" } },
                    },
                },
            }),
            json!({}),
            "CertificateId",
            Some("referencing-certificate-thing"),
        ),
        (
            "a policy's name",
            json!({
                "Resources": {
                    "thing": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": { "ThingName": "referencing-policy-thing" },
                    },
                    "pol": {
                        "Type": "AWS::IoT::Policy",
                        "Properties": { "PolicyName": { "Ref": "PolicyName" } },
                    },
                },
            }),
            json!({}),
            "PolicyName",
            Some("referencing-policy-thing"),
        ),
    ];

    for (label, template, parameters, parameter, unprovisioned_thing) in cases {
        let emulator = Emulator::new();
        let refused = emulator.register_thing(&template, &parameters).await;
        assert_eq!(
            refused.status,
            400,
            "case {label}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {label}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.error_message(),
            unresolvable_reference_message(parameter),
            "case {label}: the refusal must name the reference to {parameter}, got {}",
            refused.error_message()
        );

        // Nothing was provisioned: a reference the call cannot resolve refuses the whole
        // template, so the thing it named by a literal is not in the registry either.
        if let Some(thing_name) = unprovisioned_thing {
            let thing = emulator.describe_thing(thing_name).await;
            assert_eq!(
                thing.status,
                404,
                "case {label}: a refused registration registers no thing, got {} body {}",
                thing.status,
                thing.text()
            );
        }
    }
}

/// RULE: a reference resolves from the parameters the call supplies OVER the `Default`s the
/// template declares, so a template whose references all resolve provisions the thing they
/// describe.
#[tokio::test]
async fn a_reference_a_parameter_or_a_default_resolves_is_provisioned() {
    // The rows span the two sources a reference resolves from, and the third case is the
    // precedence between them: a supplied value wins over the declared default.
    let cases = [
        (
            "the call supplies the parameter",
            json!({ "SerialNumber": "sn-supplied" }),
            json!({ "SerialNumber": { "Type": "String" } }),
            "sn-supplied",
        ),
        (
            "the template's Default supplies it",
            json!({}),
            json!({ "SerialNumber": { "Type": "String", "Default": "sn-defaulted" } }),
            "sn-defaulted",
        ),
        (
            "the call's value is taken over the Default",
            json!({ "SerialNumber": "sn-overridden" }),
            json!({ "SerialNumber": { "Type": "String", "Default": "sn-defaulted" } }),
            "sn-overridden",
        ),
    ];

    for (label, parameters, declared_parameters, expected_thing_name) in cases {
        let emulator = Emulator::new();
        let template = json!({
            "Parameters": declared_parameters,
            "Resources": {
                "thing": {
                    "Type": "AWS::IoT::Thing",
                    "Properties": { "ThingName": { "Ref": "SerialNumber" } },
                },
            },
        });

        let registered = emulator.register_thing(&template, &parameters).await;
        assert_eq!(
            registered.status,
            200,
            "case {label}: expected 200, got {} body {}",
            registered.status,
            registered.text()
        );
        assert_eq!(
            registered.json()["resourceArns"]["thing"],
            json!(thing_arn(expected_thing_name)),
            "case {label}: the reference must resolve to {expected_thing_name}, got {}",
            registered.text()
        );

        // The thing is on record under the name the reference resolved to.
        let thing = emulator.describe_thing(expected_thing_name).await;
        assert_eq!(
            thing.status,
            200,
            "case {label}: the provisioned thing must read back, got {} body {}",
            thing.status,
            thing.text()
        );
        assert_eq!(
            thing.json()["thingName"],
            json!(expected_thing_name),
            "case {label}: got {}",
            thing.text()
        );
    }
}
