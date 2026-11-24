// SPDX-License-Identifier: CC0-1.0

//! The DEPENDENT RESOURCE behind a create, end to end over the wire, with AWS IoT, IAM and
//! AWS Lambda mounted on ONE engine — the shape `core/router/src/lib.rs` mounts them in —
//! so the function a certificate provider names is a function `CreateFunction` created, the
//! role a stream names is a role `CreateRole` created, and the fleet index a dynamic thing
//! group queries is the one `UpdateIndexingConfiguration` enabled.
//!
//! The RULE asserted here: a create whose resource can only work if another resource is
//! usable resolves that resource through the component that OWNS it BEFORE anything is
//! written, and refuses the create — in that operation's own modelled error and wording —
//! when the owner answers that the resource is unusable, leaving nothing behind. The three
//! dependencies this service's creates carry:
//!
//! * a certificate provider IS the Lambda function it names, invoked by this service
//!   principal, so a function AWS Lambda holds nothing under, or whose resource policy
//!   admits this service's `lambda:InvokeFunction` nothing, is `InvalidRequestException`
//!   (400, "Access denied during lambda validation: <lambdaFunctionArn>") — on the create
//!   and on the update that repoints a provider at a new function;
//! * a stream is READ UNDER the role it names, so a role IAM answers this service cannot
//!   assume is `InvalidRequestException` (400, "Permission denied when attempting to assume
//!   role <roleArn>.") — and because the refused create wrote nothing, `DeleteStream` for
//!   that id then answers the modelled `ResourceNotFoundException` (404) naming the account;
//! * a dynamic thing group is a VIEW of the thing fleet index, so a create made while that
//!   index is not enabled is `ResourceNotFoundException` (404, "AWS IoT Fleet Indexing is
//!   not enabled. Please enable index by calling UpdateIndexingConfiguration.").
//!
//! Each table spans what its rule generalises over rather than one recorded request: WHICH
//! answer the owner gives about the dependency (it holds one that admits this service, one
//! that admits another, one narrowed to another resource, one that admits nothing, and none
//! at all), and — for the fleet index — WHICH indexing configuration the account has.
//!
//! What is NOT refused is the case where nothing answered: a deployment with no Lambda to
//! ask leaves the create going ahead, which is what the suites mounting IoT alone
//! (`crate::certificate_providers`, `crate::streams`) show.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService as IamService;
use service_com_amazonaws_iot_awsiotservice::state::{
    CERTIFICATE_PROVIDER_OPERATIONS, SERVICE_PRINCIPAL,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService as IotService;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService as LambdaService;
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_authorizer_invocation::package;
use crate::provisioning_role_trust::trust_policy;
use crate::topic_rule_kinesis_action::{form_encode, xml_text};

/// The Region every call below is signed for.
const REGION: &str = "eu-west-2";

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";

/// The modelled error a refused create carries on the two creates that take a dependent
/// resource of another service (`@httpError 400`).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The modelled error a create refused for a resource that does not exist carries
/// (`@httpError 404`).
const NOT_FOUND: &str = "ResourceNotFoundException";

/// A service principal that is NOT this service, for a grant made to somebody else.
const OTHER_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The IAM action an invocation of a function performs, which is what a certificate
/// provider's function has to admit this service.
const INVOKE_FUNCTION: &str = "lambda:InvokeFunction";

/// Where AWS Lambda's `CreateFunction` and `AddPermission` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and entry point of every deployment package this suite uploads.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The Lambda runtime every function here declares.
const RUNTIME: &str = "python3.12";

/// A certificate provider's function: it signs nothing here, because what is under test is
/// whether the provider can be created at all, not what the function answers.
const PROVIDER_SOURCE: &str = r#"
def lambda_handler(event, context):
    return {"certificatePem": ""}
"#;

/// One file of a stream: the id a device asks for it by and the S3 object it is. No Amazon
/// S3 is mounted here, so nothing is established about the object and the create is decided
/// by the role alone.
fn stream_file() -> Value {
    json!({ "fileId": 0, "s3Location": { "bucket": "firmware-images", "key": "firmware.bin" } })
}

/// AWS IoT, IAM and AWS Lambda on ONE shared engine, behind the real router.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(IotService::new(Arc::clone(&engine)));
        router.register_service(IamService::new(Arc::clone(&engine)));
        router.register_service(LambdaService::new(Arc::clone(&engine)));
        Self { router }
    }

    /// One request through the real router, claimed by the service its credential scope
    /// names. Signature verification is off in this harness, so the signature itself is
    /// never checked.
    async fn send(
        &self,
        signing_name: &str,
        method: &str,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Reply {
        let host = format!("{signing_name}.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", content_type)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDTESTIOT/20260101/{REGION}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        Reply { status, raw }
    }

    /// One IoT control-plane call: restJson1, the body carrying the operation's members.
    async fn iot(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        self.send("iot", method, path, "application/json", payload)
            .await
    }

    /// One AWS Lambda call: restJson1 as well, on that service's own bindings.
    async fn lambda(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        self.send("lambda", method, path, "application/json", payload)
            .await
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
        let reply = self
            .send(
                "iam",
                "POST",
                "/",
                "application/x-www-form-urlencoded",
                body.into_bytes(),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case:?}: {action} answered {}: {}",
            reply.status,
            reply.text()
        );
        reply.text()
    }

    /// A role whose trust policy admits `principals`, answered with the ARN IAM persisted.
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

    /// A Lambda function this account holds, answered with the ARN Lambda minted. Its
    /// execution role is one IAM holds, exactly as a client provisions one.
    async fn given_function(&self, case: &str, name: &str) -> String {
        let role = self
            .given_role(case, &format!("{name}-exec"), &[OTHER_PRINCIPAL])
            .await;
        let created = self
            .lambda(
                "POST",
                FUNCTIONS_URI,
                Some(&json!({
                    "FunctionName": name,
                    "Role": role,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Timeout": 10,
                    "Code": {
                        "ZipFile": base64::engine::general_purpose::STANDARD
                            .encode(package(PROVIDER_SOURCE)),
                    },
                })),
            )
            .await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: CreateFunction {name:?} answered {} body {}",
            created.status,
            created.text()
        );
        created
            .member("FunctionArn")
            .unwrap_or_else(|| panic!("case {case:?}: CreateFunction minted no ARN: {}", created.text()))
    }

    /// `AddPermission` on `function`: the statement that grants `principal` the `action`,
    /// narrowed to `source_arn` when the row names one.
    async fn given_permission(
        &self,
        case: &str,
        function: &str,
        principal: &str,
        action: &str,
        source_arn: Option<&str>,
    ) {
        let mut request = json!({
            "StatementId": "preflight-grant",
            "Action": action,
            "Principal": principal,
        });
        if let Some(source_arn) = source_arn {
            request["SourceArn"] = json!(source_arn);
        }
        let granted = self
            .lambda(
                "POST",
                &format!("{FUNCTIONS_URI}/{function}/policy"),
                Some(&request),
            )
            .await;
        assert_eq!(
            granted.status,
            201,
            "case {case:?}: AddPermission on {function:?} answered {} body {}",
            granted.status,
            granted.text()
        );
    }

    /// `POST /certificate-providers/{name}` — `CreateCertificateProvider`.
    async fn create_provider(&self, name: &str, function_arn: &str) -> Reply {
        self.iot(
            "POST",
            &format!("/certificate-providers/{name}"),
            Some(&json!({
                "lambdaFunctionArn": function_arn,
                "accountDefaultForOperations": [provider_operation()],
            })),
        )
        .await
    }

    /// `PUT /certificate-providers/{name}` — `UpdateCertificateProvider`.
    async fn update_provider(&self, name: &str, function_arn: &str) -> Reply {
        self.iot(
            "PUT",
            &format!("/certificate-providers/{name}"),
            Some(&json!({ "lambdaFunctionArn": function_arn })),
        )
        .await
    }

    /// `GET /certificate-providers/{name}` — `DescribeCertificateProvider`, the reader that
    /// proves what a create persisted and that a refused one persisted nothing.
    async fn describe_provider(&self, name: &str) -> Reply {
        self.iot("GET", &format!("/certificate-providers/{name}"), None)
            .await
    }

    /// `POST /streams/{streamId}` — `CreateStream`.
    async fn create_stream(&self, stream_id: &str, role_arn: &str) -> Reply {
        self.iot(
            "POST",
            &format!("/streams/{stream_id}"),
            Some(&json!({ "files": [stream_file()], "roleArn": role_arn })),
        )
        .await
    }

    /// `GET /streams/{streamId}` — `DescribeStream`.
    async fn describe_stream(&self, stream_id: &str) -> Reply {
        self.iot("GET", &format!("/streams/{stream_id}"), None).await
    }

    /// `DELETE /streams/{streamId}` — `DeleteStream`.
    async fn delete_stream(&self, stream_id: &str) -> Reply {
        self.iot("DELETE", &format!("/streams/{stream_id}"), None)
            .await
    }

    /// `POST /indexing/config` — `UpdateIndexingConfiguration`, the call that brings the
    /// fleet index a dynamic thing group queries into existence.
    async fn update_indexing(&self, case: &str, body: &Value) {
        let updated = self.iot("POST", "/indexing/config", Some(body)).await;
        assert_eq!(
            updated.status,
            200,
            "case {case:?}: UpdateIndexingConfiguration answered {} body {}",
            updated.status,
            updated.text()
        );
    }

    /// `POST /dynamic-thing-groups/{thingGroupName}` — `CreateDynamicThingGroup`.
    async fn create_dynamic_group(&self, name: &str, body: &Value) -> Reply {
        self.iot("POST", &format!("/dynamic-thing-groups/{name}"), Some(body))
            .await
    }

    /// `GET /thing-groups/{thingGroupName}` — `DescribeThingGroup`, which reads a group of
    /// either kind, so it is what shows a refused create wrote nothing.
    async fn describe_group(&self, name: &str) -> Reply {
        self.iot("GET", &format!("/thing-groups/{name}"), None).await
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

/// The one operation a certificate provider may be the account default for, read off the
/// modelled `CertificateProviderOperation` catalog rather than spelled here.
fn provider_operation() -> &'static str {
    CERTIFICATE_PROVIDER_OPERATIONS
        .first()
        .expect("the model declares at least one certificate provider operation")
}

/// The ARN a certificate provider created under `name` carries, which is the
/// `aws:SourceArn` a grant for THAT provider is narrowed by.
fn provider_arn(name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:certificateprovider/{name}")
}

/// How a function this service may not invoke is refused, as AWS IoT words it.
fn lambda_validation_refusal(function_arn: &str) -> String {
    format!("Access denied during lambda validation: {function_arn}")
}

/// How a create made while the account has no fleet index is refused, as AWS IoT words it:
/// entirely fixed text, spelled out here rather than read off the constant the handler
/// returns, so a changed sentence fails this suite.
const FLEET_INDEXING_NOT_ENABLED: &str = "AWS IoT Fleet Indexing is not enabled. Please \
                                          enable index by calling \
                                          UpdateIndexingConfiguration.";

/// One row of the certificate-provider table: which grant the function carries, and whether
/// the create is accepted.
struct FunctionCase {
    label: &'static str,
    provider: &'static str,
    /// The function name Lambda holds, or `None` for an ARN naming no function at all.
    function: Option<&'static str>,
    /// The principal the function's policy grants, when it carries a grant.
    granted_to: Option<&'static str>,
    /// The action that grant names.
    action: &'static str,
    /// Whether the grant is narrowed to the provider under test, to another provider, or
    /// not at all.
    narrowed_to: Option<&'static str>,
    accepted: bool,
}

/// RULE: a certificate provider is created only over a Lambda function AWS Lambda holds and
/// whose resource policy admits this service's invocation of it, and a refused create
/// leaves nothing behind.
#[tokio::test]
async fn a_certificate_provider_is_created_only_over_an_invocable_function() {
    // The rows span WHICH answer AWS Lambda gives about the function: a grant to this
    // service narrowed to this provider, the same grant un-narrowed, a grant narrowed to
    // ANOTHER provider, a grant to another service, a grant of another action, a function
    // carrying no policy at all, and no function under that name at all.
    let rows = [
        FunctionCase {
            label: "a function granted to this service for this provider",
            provider: "narrowed",
            function: Some("cert-signer-narrowed"),
            granted_to: Some(SERVICE_PRINCIPAL),
            action: INVOKE_FUNCTION,
            narrowed_to: Some("narrowed"),
            accepted: true,
        },
        FunctionCase {
            label: "a function granted to this service with no narrowing",
            provider: "broad",
            function: Some("cert-signer-broad"),
            granted_to: Some(SERVICE_PRINCIPAL),
            action: INVOKE_FUNCTION,
            narrowed_to: None,
            accepted: true,
        },
        FunctionCase {
            label: "a grant narrowed to another provider's ARN",
            provider: "elsewhere",
            function: Some("cert-signer-elsewhere"),
            granted_to: Some(SERVICE_PRINCIPAL),
            action: INVOKE_FUNCTION,
            narrowed_to: Some("some-other-provider"),
            accepted: false,
        },
        FunctionCase {
            label: "a function granted to another service principal",
            provider: "stranger",
            function: Some("cert-signer-stranger"),
            granted_to: Some(OTHER_PRINCIPAL),
            action: INVOKE_FUNCTION,
            narrowed_to: None,
            accepted: false,
        },
        FunctionCase {
            label: "a function granted another action to this service",
            provider: "wrongaction",
            function: Some("cert-signer-wrongaction"),
            granted_to: Some(SERVICE_PRINCIPAL),
            action: "lambda:GetFunction",
            narrowed_to: None,
            accepted: false,
        },
        FunctionCase {
            label: "a function carrying no resource policy at all",
            provider: "ungranted",
            function: Some("cert-signer-ungranted"),
            granted_to: None,
            action: INVOKE_FUNCTION,
            narrowed_to: None,
            accepted: false,
        },
        FunctionCase {
            label: "an ARN naming no function this account holds",
            provider: "absent",
            function: None,
            granted_to: None,
            action: INVOKE_FUNCTION,
            narrowed_to: None,
            accepted: false,
        },
    ];

    for FunctionCase {
        label: case,
        provider,
        function,
        granted_to,
        action,
        narrowed_to,
        accepted,
    } in rows
    {
        // One emulator per row: an account holds at most one certificate provider, since
        // every provider claims the one operation's account default.
        let emulator = Emulator::new();
        let function_arn = match function {
            Some(name) => {
                let arn = emulator.given_function(case, name).await;
                if let Some(principal) = granted_to {
                    emulator
                        .given_permission(
                            case,
                            name,
                            principal,
                            action,
                            narrowed_to.map(provider_arn).as_deref(),
                        )
                        .await;
                }
                arn
            }
            None => format!("arn:aws:lambda:{REGION}:{ACCOUNT_ID}:function:never-created"),
        };

        let created = emulator.create_provider(provider, &function_arn).await;
        if accepted {
            assert_eq!(
                created.status,
                200,
                "case {case:?}: the create must succeed, got {} body {}",
                created.status,
                created.text()
            );
            let read = emulator.describe_provider(provider).await;
            assert_eq!(
                read.status,
                200,
                "case {case:?}: the created provider must be readable, got {} body {}",
                read.status,
                read.text()
            );
            assert_eq!(
                read.member("lambdaFunctionArn").as_deref(),
                Some(function_arn.as_str()),
                "case {case:?}: the read must report the function the provider was created \
                 over, got {}",
                read.text()
            );
            continue;
        }

        assert_eq!(
            created.status,
            400,
            "case {case:?}: a function this service cannot invoke must be refused 400, got {} \
             body {}",
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
            created.member("message"),
            Some(lambda_validation_refusal(&function_arn)),
            "case {case:?}: got {}",
            created.text()
        );
        let described = emulator.describe_provider(provider).await;
        assert_eq!(
            described.status,
            404,
            "case {case:?}: a refused create must record no provider, got {} body {}",
            described.status,
            described.text()
        );
    }
}

/// RULE: the update that repoints a provider at a new function is held to the same
/// validation as the create, and a refused update leaves the stored function as it was —
/// while an accepted one is what the next read reports.
#[tokio::test]
async fn repointing_a_provider_is_held_to_the_same_function_validation() {
    let emulator = Emulator::new();
    let first = emulator.given_function("the first function", "cert-signer-first").await;
    emulator
        .given_permission(
            "the first function",
            "cert-signer-first",
            SERVICE_PRINCIPAL,
            INVOKE_FUNCTION,
            Some(&provider_arn("repointed")),
        )
        .await;
    assert_eq!(
        emulator.create_provider("repointed", &first).await.status,
        200,
        "the provider must be creatable over a function this service may invoke"
    );

    // A second function the account holds, left granting nothing: repointing at it is the
    // refusal, and the stored function is untouched.
    let second = emulator
        .given_function("the second function", "cert-signer-second")
        .await;
    let refused = emulator.update_provider("repointed", &second).await;
    assert_eq!(
        refused.status,
        400,
        "case a function granting nothing: expected 400, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        (refused.error_code(), refused.member("message")),
        (
            INVALID_REQUEST.to_string(),
            Some(lambda_validation_refusal(&second))
        ),
        "case a function granting nothing: got {}",
        refused.text()
    );
    assert_eq!(
        emulator
            .describe_provider("repointed")
            .await
            .member("lambdaFunctionArn")
            .as_deref(),
        Some(first.as_str()),
        "case a function granting nothing: the refused update must change nothing"
    );

    // The same update, once the second function admits this service: the write takes, and
    // the read reports the NEW function.
    emulator
        .given_permission(
            "the second function granted",
            "cert-signer-second",
            SERVICE_PRINCIPAL,
            INVOKE_FUNCTION,
            Some(&provider_arn("repointed")),
        )
        .await;
    let updated = emulator.update_provider("repointed", &second).await;
    assert_eq!(
        updated.status,
        200,
        "case the function granted: expected 200, got {} body {}",
        updated.status,
        updated.text()
    );
    assert_eq!(
        emulator
            .describe_provider("repointed")
            .await
            .member("lambdaFunctionArn")
            .as_deref(),
        Some(second.as_str()),
        "case the function granted: the read must report the new function"
    );
}

/// One row of the stream table: which answer IAM gives about the role a create names, and
/// whether the create is accepted.
struct RoleCase {
    label: &'static str,
    stream_id: &'static str,
    /// The role name IAM holds, or `None` for an ARN naming no role of this account.
    role_name: Option<&'static str>,
    /// The service principals the role's trust policy admits.
    principals: &'static [&'static str],
    /// An ARN to name instead of a role of this account, for the rows IAM is not asked
    /// about at all.
    foreign_role: Option<&'static str>,
    accepted: bool,
}

/// RULE: a stream is created only under a role this service can assume, and because a
/// refused create writes nothing, the delete of that stream id then answers the modelled
/// not-found, naming the account.
#[tokio::test]
async fn a_stream_is_created_only_under_a_role_this_service_can_assume() {
    // The rows span WHICH answer IAM gives about the role: it holds one trusting this
    // service, one trusting this service beside another, one trusting only another service,
    // no role under that name at all, and a value naming ANOTHER account's role — which is
    // not this emulator's IAM to answer for, so it establishes nothing.
    let rows = [
        RoleCase {
            label: "a role whose trust policy admits this service",
            stream_id: "trusted-stream",
            role_name: Some("IoTStreamRole"),
            principals: &[SERVICE_PRINCIPAL],
            foreign_role: None,
            accepted: true,
        },
        RoleCase {
            label: "a role trusting this service beside another",
            stream_id: "shared-stream",
            role_name: Some("SharedStreamRole"),
            principals: &[OTHER_PRINCIPAL, SERVICE_PRINCIPAL],
            foreign_role: None,
            accepted: true,
        },
        RoleCase {
            label: "a role whose trust policy admits only another service",
            stream_id: "other-service-stream",
            role_name: Some("LambdaOnlyStreamRole"),
            principals: &[OTHER_PRINCIPAL],
            foreign_role: None,
            accepted: false,
        },
        RoleCase {
            label: "a role whose trust policy admits nobody",
            stream_id: "untrusting-stream",
            role_name: Some("UntrustingStreamRole"),
            principals: &[],
            foreign_role: None,
            accepted: false,
        },
        RoleCase {
            label: "an ARN naming no role this account holds",
            stream_id: "absent-role-stream",
            role_name: None,
            principals: &[],
            foreign_role: None,
            accepted: false,
        },
        RoleCase {
            label: "an ARN naming another account's role",
            stream_id: "foreign-role-stream",
            role_name: None,
            principals: &[],
            foreign_role: Some("arn:aws:iam::123456789012:role/CrossAccountStreamRole"),
            accepted: true,
        },
    ];

    let emulator = Emulator::new();
    for RoleCase {
        label: case,
        stream_id,
        role_name,
        principals,
        foreign_role,
        accepted,
    } in rows
    {
        let role_arn = match (role_name, foreign_role) {
            (Some(role_name), _) => emulator.given_role(case, role_name, principals).await,
            (None, Some(foreign)) => foreign.to_string(),
            (None, None) => format!("arn:aws:iam::{ACCOUNT_ID}:role/NeverCreated"),
        };

        let created = emulator.create_stream(stream_id, &role_arn).await;
        if accepted {
            assert_eq!(
                created.status,
                200,
                "case {case:?}: the create must succeed, got {} body {}",
                created.status,
                created.text()
            );
            let read = emulator.describe_stream(stream_id).await;
            assert_eq!(
                read.status,
                200,
                "case {case:?}: the created stream must be readable, got {} body {}",
                read.status,
                read.text()
            );
            assert_eq!(
                read.json()["streamInfo"]["roleArn"].as_str(),
                Some(role_arn.as_str()),
                "case {case:?}: the read must report the role the stream reads under, got {}",
                read.text()
            );
            continue;
        }

        assert_eq!(
            created.status,
            400,
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
            created.member("message"),
            Some(format!(
                "Permission denied when attempting to assume role {role_arn}."
            )),
            "case {case:?}: got {}",
            created.text()
        );

        // The refused create recorded nothing, so the reader and the DELETE of that id both
        // report the miss — the delete in the sentence that names the account.
        let described = emulator.describe_stream(stream_id).await;
        assert_eq!(
            described.status,
            404,
            "case {case:?}: a refused create must record no stream, got {} body {}",
            described.status,
            described.text()
        );
        let deleted = emulator.delete_stream(stream_id).await;
        assert_eq!(
            deleted.status,
            404,
            "case {case:?}: deleting a stream nobody created must answer 404, got {} body {}",
            deleted.status,
            deleted.text()
        );
        assert_eq!(
            (deleted.error_code(), deleted.member("message")),
            (
                NOT_FOUND.to_string(),
                Some(format!(
                    "Requested stream {stream_id} does not exist in given account {ACCOUNT_ID}"
                ))
            ),
            "case {case:?}: got {}",
            deleted.text()
        );
    }
}

/// One row of the fleet-index table: the indexing configuration the account has when the
/// dynamic group is created, and whether the create is accepted.
struct IndexCase {
    label: &'static str,
    group: &'static str,
    /// The `UpdateIndexingConfiguration` body applied first, or `None` for an account that
    /// never updated the configuration at all.
    configuration: Option<fn() -> Value>,
    /// The `CreateDynamicThingGroup` body.
    request: fn() -> Value,
    accepted: bool,
}

/// RULE: a dynamic thing group is a view of the THING fleet index, so it is created only
/// while that index is enabled; refused otherwise with the modelled not-found and the
/// sentence naming the operation that enables it, and with nothing created.
#[tokio::test]
async fn a_dynamic_thing_group_is_created_only_over_an_enabled_thing_index() {
    // The rows span WHICH indexing configuration the account has — none at all, thing
    // indexing explicitly off, only the thing GROUP index enabled, and the thing index
    // enabled in each mode `ThingIndexingMode` admits beside `OFF` — and whether the
    // request names the index itself.
    let rows = [
        IndexCase {
            label: "an account that never enabled indexing",
            group: "no-indexing",
            configuration: None,
            request: || json!({ "queryString": "thingName:*" }),
            accepted: false,
        },
        IndexCase {
            label: "thing indexing explicitly off",
            group: "indexing-off",
            configuration: Some(|| {
                json!({ "thingIndexingConfiguration": { "thingIndexingMode": "OFF" } })
            }),
            request: || json!({ "queryString": "attributes.zone:north" }),
            accepted: false,
        },
        IndexCase {
            label: "only the thing group index enabled",
            group: "group-index-only",
            configuration: Some(|| {
                json!({
                    "thingGroupIndexingConfiguration": { "thingGroupIndexingMode": "ON" },
                })
            }),
            request: || json!({ "queryString": "thingName:*" }),
            accepted: false,
        },
        IndexCase {
            label: "the thing index enabled over the registry",
            group: "registry-index",
            configuration: Some(|| {
                json!({ "thingIndexingConfiguration": { "thingIndexingMode": "REGISTRY" } })
            }),
            request: || json!({ "queryString": "thingName:*" }),
            accepted: true,
        },
        IndexCase {
            label: "the thing index enabled over the registry and shadow, the index named",
            group: "shadow-index",
            configuration: Some(|| {
                json!({
                    "thingIndexingConfiguration": {
                        "thingIndexingMode": "REGISTRY_AND_SHADOW",
                    },
                })
            }),
            request: || {
                json!({
                    "queryString": "registry.version:[1 TO 10]",
                    "indexName": "AWS_Things",
                })
            },
            accepted: true,
        },
    ];

    for IndexCase {
        label: case,
        group,
        configuration,
        request,
        accepted,
    } in rows
    {
        // One emulator per row: the indexing configuration is one record for the account,
        // so a row must not inherit the one before it.
        let emulator = Emulator::new();
        if let Some(configuration) = configuration {
            emulator.update_indexing(case, &configuration()).await;
        }

        let created = emulator.create_dynamic_group(group, &request()).await;
        if accepted {
            assert_eq!(
                created.status,
                200,
                "case {case:?}: the create must succeed, got {} body {}",
                created.status,
                created.text()
            );
            let read = emulator.describe_group(group).await;
            assert_eq!(
                read.status,
                200,
                "case {case:?}: the created group must be readable, got {} body {}",
                read.status,
                read.text()
            );
            assert_eq!(
                read.member("queryString"),
                request()["queryString"].as_str().map(str::to_string),
                "case {case:?}: the read must report the query the group is, got {}",
                read.text()
            );
            continue;
        }

        assert_eq!(
            created.status,
            404,
            "case {case:?}: a create made with no thing index must be refused 404, got {} body \
             {}",
            created.status,
            created.text()
        );
        assert_eq!(
            created.error_code(),
            NOT_FOUND,
            "case {case:?}: got {}",
            created.text()
        );
        assert_eq!(
            created.member("message").as_deref(),
            Some(FLEET_INDEXING_NOT_ENABLED),
            "case {case:?}: got {}",
            created.text()
        );
        let described = emulator.describe_group(group).await;
        assert_eq!(
            described.status,
            404,
            "case {case:?}: a refused create must record no group, got {} body {}",
            described.status,
            described.text()
        );
    }
}
