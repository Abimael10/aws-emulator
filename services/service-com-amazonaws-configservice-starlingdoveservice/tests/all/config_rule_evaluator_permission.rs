// SPDX-License-Identifier: CC0-1.0

//! Whether Config takes a Custom Lambda rule at all, end to end.
//!
//! The rule under test: **a Config rule whose evaluator is an AWS Lambda function is
//! created only when Config could INVOKE that function — the `SourceIdentifier` names a
//! function the account holds and that function's own resource-based policy allows the
//! Config service principal `lambda:InvokeFunction` on it. Anything else is refused
//! `InsufficientPermissionsException` (HTTP 400) and leaves the account holding no such
//! rule. A rule with no Lambda evaluator — a Config managed rule — is not subject to the
//! reading at all.**
//!
//! Config and Lambda are mounted on ONE engine, as `core/router/src/lib.rs` mounts them, so
//! the policy Config reads is the one `AddPermission` actually wrote: nothing here fakes a
//! grant.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_configservice_starlingdoveservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::custom_lambda_rule_evaluation::package;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below declares, and the module and attribute its package
/// exposes its handler as. No function here is invoked: the rule is refused or taken before
/// any evaluation pass runs.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The service principal Config invokes a rule's evaluator as, which is the principal the
/// function's policy has to name.
const CONFIG_PRINCIPAL: &str = "config.amazonaws.com";

/// The IAM action invoking an evaluator is.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The error code Config refuses a rule whose evaluator it cannot invoke with
/// (`PutConfigRule`: "For PutConfigRule, the Lambda function cannot be invoked. Check the
/// function ARN, and check the function's permissions").
const INSUFFICIENT_PERMISSIONS: &str = "InsufficientPermissionsException";

/// An account that is not this emulator's, for a grant scoped to the wrong one.
const ANOTHER_ACCOUNT: &str = "210987654321";

/// A Config MANAGED rule's `SourceIdentifier`, from the managed-rule list
/// (`Source$SourceIdentifier`: "For Config Managed rules, a predefined identifier from a
/// list").
const MANAGED_RULE_IDENTIFIER: &str = "S3_BUCKET_VERSIONING_ENABLED";

/// Config, IAM and Lambda over one shared engine.
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
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        Self { router }
    }

    /// One Config call (`awsJson1_1`), answering the HTTP status and the parsed body.
    async fn call(&self, operation: &str, body: &Value) -> (u16, Value) {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/config/aws4_request, \
             SignedHeaders=host;x-amz-date, Signature=deadbeef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("config.{REGION}.amazonaws.com"))
            .header("authorization", authorization)
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("StarlingDoveService.{operation}"))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// A Config call that must answer 200.
    async fn ok(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "{operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// One Lambda `restJson1` call, as its status and parsed body.
    async fn lambda(&self, method: &str, uri: &str, body: Option<&Value>) -> (u16, Value) {
        let payload = body
            .map(|value| value.to_string().into_bytes())
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("lambda.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/lambda/\
                     aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .body(bytes::Bytes::from(payload))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Put the customer managed configuration recorder a Config rule needs — "you must
    /// create a configuration recorder before you can create or update a Config rule" —
    /// with its own role created in IAM first.
    async fn given_recorder(&self) {
        let role = service_common::iam_client::iam_client_through(
            &smithy_server_runtime::EmbeddedEmulator::from_router(self.router.clone()),
            REGION,
        )
        .create_role()
        .role_name("config-role")
        .assume_role_policy_document(
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Principal": { "Service": CONFIG_PRINCIPAL },
                    "Action": "sts:AssumeRole",
                }],
            })
            .to_string(),
        )
        .send()
        .await
        .expect("creating the recorder's role must succeed")
        .role
        .expect("CreateRole answers the role")
        .arn;
        self.ok(
            "PutConfigurationRecorder",
            &json!({
                "ConfigurationRecorder": {
                    "name": "the-recorder",
                    "roleARN": role,
                    "recordingGroup": { "allSupported": true },
                }
            }),
        )
        .await;
    }
}

/// The handler every function below is packaged with. No function here is invoked: the rule
/// is refused or taken before any evaluation pass runs, so what the handler would answer is
/// not what these rows are about.
const HANDLER_SOURCE: &str = r#"
def lambda_handler(event, context):
    return {}
"#;

/// Create the function `name` and answer the ARN Lambda assigned it.
async fn given_function(case: &str, emulator: &Emulator, name: &str) -> String {
    let (status, created) = emulator
        .lambda(
            "POST",
            FUNCTIONS_URI,
            Some(&json!({
                "FunctionName": name,
                "Role": format!(
                    "arn:aws:iam::{}:role/config-rule-evaluator",
                    engine::emulator_account_id()
                ),
                "Runtime": RUNTIME,
                "Handler": HANDLER,
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD
                        .encode(package(HANDLER_SOURCE)),
                },
            })),
        )
        .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateFunction answered {status} {created}"
    );
    created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no FunctionArn in {created}"))
        .to_string()
}

/// Grant `principal` the action `action` on the function `name`, scoped to `source_account`
/// where one is given — one `AddPermission` call, which is what writes the function's
/// resource-based policy.
async fn given_permission(
    case: &str,
    emulator: &Emulator,
    name: &str,
    principal: &str,
    action: &str,
    source_account: Option<&str>,
) {
    let mut body = json!({
        "StatementId": "AllowExecutionFromConfig",
        "Action": action,
        "Principal": principal,
    });
    if let Some(account) = source_account {
        body["SourceAccount"] = json!(account);
    }
    let (status, granted) = emulator
        .lambda(
            "POST",
            &format!("{FUNCTIONS_URI}/{name}/policy"),
            Some(&body),
        )
        .await;
    assert_eq!(
        status, 201,
        "case {case:?}: AddPermission answered {status} {granted}"
    );
}

/// The grant a row writes on its function, as [`given_permission`] takes it: the principal,
/// the action, and the account the grant is scoped to.
type Grant = (&'static str, &'static str, Option<&'static str>);

/// The names of the Config rules the account holds, through `DescribeConfigRules`.
async fn held_rules(emulator: &Emulator) -> Vec<String> {
    let read = emulator.ok("DescribeConfigRules", &json!({})).await;
    read["ConfigRules"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|rule| rule["ConfigRuleName"].as_str().map(str::to_string))
        .collect()
}

/// The error code a refused Config call answered under.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .or_else(|| body["code"].as_str())
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The message a refused Config call carried.
fn error_message(body: &Value) -> String {
    ["message", "Message"]
        .iter()
        .find_map(|key| body.get(*key).and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// How Config words the refusal of a rule whose evaluator it cannot invoke: the sentence
/// `InsufficientPermissionsException` states for this case, around the one function the
/// request named.
fn expected_message(function: &str) -> String {
    format!(
        "The AWS Lambda function {function} cannot be invoked. Check the function ARN, and \
         check the function's permissions."
    )
}

/// A Custom Lambda rule is created exactly when the function's own policy lets Config
/// invoke it. The rows differ along what the function's policy says — nothing at all, the
/// documented grant, the same grant scoped to this account, a grant of every Lambda action,
/// a grant to another service principal, a grant of another action, a grant scoped to
/// another account — and along whether the `SourceIdentifier` names a function at all.
#[tokio::test]
async fn a_custom_lambda_rule_is_created_only_when_config_may_invoke_its_evaluator() {
    let account = engine::emulator_account_id();
    let cases: [(&str, bool, Option<Grant>, bool); 8] = [
        (
            "no resource-based policy at all",
            true,
            None,
            false,
        ),
        (
            "the documented grant for Config",
            true,
            Some((CONFIG_PRINCIPAL, INVOKE_ACTION, None)),
            true,
        ),
        (
            "the documented grant scoped to this account",
            true,
            Some((CONFIG_PRINCIPAL, INVOKE_ACTION, Some(account))),
            true,
        ),
        (
            "a grant of every Lambda action",
            true,
            Some((CONFIG_PRINCIPAL, "lambda:*", None)),
            true,
        ),
        (
            "a grant to another service principal",
            true,
            Some(("events.amazonaws.com", INVOKE_ACTION, None)),
            false,
        ),
        (
            "a grant of another action",
            true,
            Some((CONFIG_PRINCIPAL, "lambda:GetFunction", None)),
            false,
        ),
        (
            "a grant scoped to another account",
            true,
            Some((CONFIG_PRINCIPAL, INVOKE_ACTION, Some(ANOTHER_ACCOUNT))),
            false,
        ),
        (
            "an identifier naming no function at all",
            false,
            None,
            false,
        ),
    ];

    for (index, (case, created, grant, accepted)) in cases.into_iter().enumerate() {
        let emulator = Emulator::new();
        emulator.given_recorder().await;
        let name = format!("evaluator-{index}");
        let function = if created {
            let arn = given_function(case, &emulator, &name).await;
            if let Some((principal, action, source_account)) = grant {
                given_permission(case, &emulator, &name, principal, action, source_account)
                    .await;
            }
            arn
        } else {
            format!("arn:aws:lambda:{REGION}:{account}:function:{name}")
        };

        let rule = format!("rule-{index}");
        let (status, body) = emulator
            .call(
                "PutConfigRule",
                &json!({
                    "ConfigRule": {
                        "ConfigRuleName": rule,
                        "Source": {
                            "Owner": "CUSTOM_LAMBDA",
                            "SourceIdentifier": function,
                            "SourceDetails": [{
                                "EventSource": "aws.config",
                                "MessageType": "ConfigurationItemChangeNotification",
                            }],
                        },
                    }
                }),
            )
            .await;

        if accepted {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the rule to be created, got {status} with {body}"
            );
            // Read back through the rule read path: the rule the write claims to have
            // created is one the account holds.
            let held = held_rules(&emulator).await;
            assert_eq!(
                held,
                vec![rule.clone()],
                "case {case:?}: expected the account to hold {rule:?}, got {held:?}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the rule to be refused, got {status} with {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, INSUFFICIENT_PERMISSIONS,
                "case {case:?}: expected {INSUFFICIENT_PERMISSIONS}, got {code:?} in {body}"
            );
            // The refusal NAMES the function it could not invoke, which is the function
            // the request pointed the rule at.
            let message = error_message(&body);
            assert_eq!(
                message,
                expected_message(&function),
                "case {case:?}: expected the refusal to name {function:?}, got {message:?}"
            );
            // A refused write creates nothing: the account holds no rule of that name.
            let held = held_rules(&emulator).await;
            assert!(
                held.is_empty(),
                "case {case:?}: expected the refusal to leave no rule, got {held:?}"
            );
        }
    }
}

/// The reading is about the EVALUATOR, so a rule that has none is not subject to it: a
/// Config managed rule names a predefined identifier rather than a function, and is created
/// with no AWS Lambda function in the account at all.
#[tokio::test]
async fn a_rule_with_no_lambda_evaluator_is_not_subject_to_the_reading() {
    let emulator = Emulator::new();
    emulator.given_recorder().await;
    let rule = "managed-rule";
    let (status, body) = emulator
        .call(
            "PutConfigRule",
            &json!({
                "ConfigRule": {
                    "ConfigRuleName": rule,
                    "Source": {
                        "Owner": "AWS",
                        "SourceIdentifier": MANAGED_RULE_IDENTIFIER,
                    },
                }
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"a Config managed rule\": expected the rule to be created, got {status} with \
         {body}"
    );
    let held = held_rules(&emulator).await;
    assert_eq!(
        held,
        vec![rule.to_string()],
        "case \"a Config managed rule\": expected the account to hold {rule:?}, got {held:?}"
    );
}

/// An UPDATE of a rule is read the same way: Config re-reads the evaluator the write points
/// the rule at, so pointing a taken rule at a function nothing admits Config on is refused
/// and the rule keeps the definition it had.
#[tokio::test]
async fn an_update_that_points_a_rule_at_an_uninvokable_evaluator_is_refused() {
    let emulator = Emulator::new();
    emulator.given_recorder().await;
    let case = "an update to an uninvokable evaluator";
    let permitted = given_function(case, &emulator, "permitted-evaluator").await;
    given_permission(
        case,
        &emulator,
        "permitted-evaluator",
        CONFIG_PRINCIPAL,
        INVOKE_ACTION,
        None,
    )
    .await;
    let withheld = given_function(case, &emulator, "withheld-evaluator").await;

    let rule = "updated-rule";
    let put = |identifier: String| {
        json!({
            "ConfigRule": {
                "ConfigRuleName": rule,
                "Source": {
                    "Owner": "CUSTOM_LAMBDA",
                    "SourceIdentifier": identifier,
                    "SourceDetails": [{
                        "EventSource": "aws.config",
                        "MessageType": "ConfigurationItemChangeNotification",
                    }],
                },
            }
        })
    };
    emulator.ok("PutConfigRule", &put(permitted.clone())).await;

    let (status, body) = emulator
        .call("PutConfigRule", &put(withheld.clone()))
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the update to be refused, got {status} with {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, INSUFFICIENT_PERMISSIONS,
        "case {case:?}: expected {INSUFFICIENT_PERMISSIONS}, got {code:?} in {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message,
        expected_message(&withheld),
        "case {case:?}: expected the refusal to name {withheld:?}, got {message:?}"
    );
    let read = emulator.ok("DescribeConfigRules", &json!({})).await;
    let identifier = read["ConfigRules"][0]["Source"]["SourceIdentifier"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        identifier, permitted,
        "case {case:?}: expected the rule to keep {permitted:?}, got {identifier:?}"
    );
}
