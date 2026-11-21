// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the KMS decision a SYNCHRONOUS Express run is subject to for
//! the principal that CALLED it, driven over each service's own wire protocol through
//! the production `MultiServiceRouter` with Step Functions, KMS, IAM and STS mounted on
//! ONE engine — so the key policy the decision reads is the document KMS's own handlers
//! stored, the identity policies are the ones IAM's handlers wrote, and the session a
//! call signs as is the one STS's own `AssumeRole` minted.
//!
//! The rule under test: whether the caller of an operation that reports data a state
//! machine's customer managed key sealed may have that key used on its behalf is KMS's
//! own decision over the two authorities KMS weighs — the key's own resource-based
//! policy and the calling principal's identity-based policies — so a key-policy
//! statement naming the CALLING PRINCIPAL ITSELF allows the call outright with no
//! identity policy of its own, one naming the account delegates to that principal's own
//! documents, and an explicit deny in the key policy refuses it; a refusal is the
//! modelled `KmsAccessDeniedException` (HTTP 400) carrying the sentence KMS worded for
//! the document that withheld the action, which is a different sentence for an explicit
//! deny than for a grant that is merely absent. The key's own KEY STATE is decided
//! first and for every caller, as `KmsInvalidStateException` (HTTP 400) carrying that
//! key state. Nothing is latched: each call is decided against the documents and the key
//! record of that instant, so a statement put back, or a key re-enabled, serves the very
//! next call.
//!
//! `StartSyncExecution` is the operation driven here because it is the one whose whole
//! answer is the run's data: "callers must have kms:Decrypt permission to decrypt the
//! definition. Alternatively, you can call the API with includedData = METADATA_ONLY to
//! get a successful response without the encrypted definition"
//! (`smithy_query(query="doc",shape="StartSyncExecutionInput",member="includedData")`).
//!
//! The rows differ along what the rule generalises over: WHICH document carries the
//! grant (the key policy naming the calling role, the key policy naming the account with
//! the role's own policy allowing the call, neither, an explicit deny, and a key policy
//! that names another principal only), the ACTIONS the statement lists (the two a read
//! of sealed data needs, and `kms:*`), the `includedData` projection the call asks for
//! (`ALL_DATA`, an absent member, and `METADATA_ONLY`), and the key states that admit no
//! cryptographic use of the key (disabled, and pending deletion).
//!
//! The harness is this suite's own, because no other suite here mounts all four of these
//! services: the rule needs a caller that is a SESSION of a role, which only STS can
//! mint, and mounting STS inside the shared `execution_data_encryption::Chain` would
//! change the router a dozen sibling suites are decided on. Everything that does not
//! depend on which services are mounted — the wire descriptions, the request encoding,
//! the readers and the policy helpers — is the one `execution_data_encryption`,
//! `identity_policy_authorization` and `state_machine_lifecycle` already publish, reached
//! through `crate::<suite>` because `tests/all/main.rs` is this binary's crate root.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::execution_data_encryption::{
    customer_managed, form_encode, xml_element, Wire, KMS, KMS_ACCESS_DENIED,
    KMS_ACCESS_DENIED_STATUS, SFN,
};
use crate::identity_policy_authorization::{ACCOUNT_KEY, IAM_VERSION};
use crate::state_machine_lifecycle::{error_code, string, ACCOUNT, HELLO_DEFINITION, REGION};

/// The STS API version the `AssumeRole` setup calls are made at, from that model's own
/// service shape (`smithy_query(query="service_info",model_path="services/\
/// service-com-amazonaws-sts-awssecuritytokenservicev20110615/smithy-model/\
/// com-amazonaws-sts-awssecuritytokenservicev20110615.json")`).
const STS_VERSION: &str = "2011-06-15";

/// The only key-policy name KMS holds a document under.
const POLICY_NAME: &str = "default";

/// The modelled error a call the key's own key state cannot serve answers, and the
/// status it carries: "The KMS key is not in valid state, for example: Disabled or
/// Deleted" (HTTP 400).
const KMS_INVALID_STATE: &str = "KmsInvalidStateException";
const KMS_INVALID_STATE_STATUS: u16 = 400;

/// The workflow type `StartSyncExecution` serves: "Starts a Synchronous Express state
/// machine execution. StartSyncExecution is not available for STANDARD workflows."
const EXPRESS: &str = "EXPRESS";

/// The terminal status a run of [`HELLO_DEFINITION`] reaches, as the modelled
/// `SyncExecutionStatus` names it, and the result that one `Pass` state produces —
/// spelled out here rather than imported, so a wrong constant in the service cannot make
/// a test pass.
const SUCCEEDED: &str = "SUCCEEDED";

/// The input every run below is started with: a document whose spacing and member order
/// are the caller's, so an `input` reported back verbatim is distinguishable from one
/// re-encoded from a parsed copy.
const INPUT: &str = r#"{"marker": "least-privilege-caller", "attempt": 1}"#;

/// The three sentences AWS words a KMS authorization failure with, one per document that
/// withheld the action — spelled out here rather than imported from the implementation,
/// for the reason [`SUCCEEDED`] is.
const NO_RESOURCE_POLICY: &str = "because no resource-based policy allows the kms:Decrypt action";
const NO_IDENTITY_POLICY: &str = "because no identity-based policy allows the kms:Decrypt action";
const EXPLICIT_DENY: &str = "with an explicit deny in a resource-based policy";

/// Step Functions, KMS, IAM and STS over ONE engine — the shape `core/router/src/lib.rs`
/// mounts them in.
///
/// All four are mounted because the decision under test crosses all four: the key a state
/// machine names and the policy on it are KMS's own records, the calling role and its
/// identity policies are IAM's, and the credential a call signs as is a session STS
/// minted.
struct Services {
    router: MultiServiceRouter,
}

impl Services {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the caller's access key id from.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// Call one operation of one `awsJson` service with `access_key_id` in the SigV4
    /// credential scope, and answer `(status, body)`.
    async fn call_as(
        &self,
        access_key_id: &str,
        wire: &Wire,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{}/aws4_request, \
             SignedHeaders=host;x-amz-target, Signature=0123456789abcdef",
            wire.signing_name
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{}.{REGION}.amazonaws.com", wire.signing_name))
            .header(
                "content-type",
                format!("application/x-amz-json-{}", wire.json_version),
            )
            .header("x-amz-target", format!("{}.{operation}", wire.target_prefix))
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "case {operation}: expected a JSON body, got {error} for {:?}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, parsed)
    }

    /// [`Self::call_as`] for the account's own published credential, which no identity
    /// policy governs — the position AWS account root credentials are in, and the one the
    /// setup calls are made from.
    async fn call(&self, wire: &Wire, operation: &str, body: &Value) -> (u16, Value) {
        self.call_as(ACCOUNT_KEY, wire, operation, body).await
    }

    /// One KMS call as the account, requiring a 200.
    async fn kms(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.call(&KMS, operation, body).await;
        assert_eq!(
            status, 200,
            "case {operation}: the setup call must succeed, got {status} with {parsed:?}"
        );
        parsed
    }

    /// One Step Functions call as the account, requiring a 200.
    async fn sfn(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.call(&SFN, operation, body).await;
        assert_eq!(
            status, 200,
            "case {operation}: the setup call must succeed, got {status} with {parsed:?}"
        );
        parsed
    }

    /// One awsQuery call to IAM or STS, signed with the account's own credential.
    async fn query(
        &self,
        signing_name: &str,
        version: &str,
        action: &str,
        params: &[(&str, String)],
    ) -> String {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(&response.into_body()).to_string();
        assert_eq!(
            status, 200,
            "case {action:?}: the setup call must succeed, got {status} with {text}"
        );
        text
    }

    /// A symmetric encryption key carrying `policy`, as its key ARN.
    ///
    /// The lockout safety check is bypassed because some documents below withhold every
    /// action from everyone: these suites are about what the document says to Step
    /// Functions, not about that check.
    async fn create_key(&self, policy: &Value) -> String {
        let created = self
            .kms(
                "CreateKey",
                &json!({
                    "KeySpec": "SYMMETRIC_DEFAULT",
                    "Policy": policy.to_string(),
                    "BypassPolicyLockoutSafetyCheck": true,
                }),
            )
            .await;
        string(&created["KeyMetadata"], "Arn")
    }

    /// Replace the key's policy, which is how a caller withdraws a grant and puts it
    /// back.
    async fn put_policy(&self, key: &str, policy: &Value) {
        self.kms(
            "PutKeyPolicy",
            &json!({
                "KeyId": key,
                "PolicyName": POLICY_NAME,
                "Policy": policy.to_string(),
                "BypassPolicyLockoutSafetyCheck": true,
            }),
        )
        .await;
    }

    /// An IAM role of this account carrying `policy` as its inline policy and `trust` as
    /// the document saying who may assume it, as its ARN.
    async fn role(&self, role_name: &str, trust: &Value, policy: &Value) -> String {
        let created = self
            .query(
                "iam",
                IAM_VERSION,
                "CreateRole",
                &[
                    ("RoleName", role_name.to_string()),
                    ("AssumeRolePolicyDocument", trust.to_string()),
                ],
            )
            .await;
        let arn = xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("CreateRole answers the role's Arn, got {created}"));
        self.query(
            "iam",
            IAM_VERSION,
            "PutRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
        arn
    }

    /// Assume the role `role_name` through STS: the session's access key id — what a call
    /// of that caller presents — and the session ARN, which is the principal a refusal
    /// blames.
    async fn assume(&self, role_name: &str) -> (String, String) {
        let assumed = self
            .query(
                "sts",
                STS_VERSION,
                "AssumeRole",
                &[
                    ("RoleArn", format!("arn:aws:iam::{ACCOUNT}:role/{role_name}")),
                    ("RoleSessionName", "caller".to_string()),
                ],
            )
            .await;
        (
            xml_element(&assumed, "AccessKeyId")
                .unwrap_or_else(|| panic!("AssumeRole answers an AccessKeyId, got {assumed}")),
            xml_element(&assumed, "Arn")
                .unwrap_or_else(|| panic!("AssumeRole answers the session's Arn, got {assumed}")),
        )
    }

    /// An EXPRESS state machine whose data is sealed under `key`, as its ARN.
    async fn express_machine(&self, name: &str, key: &str, role_arn: &str) -> String {
        let created = self
            .sfn(
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": HELLO_DEFINITION,
                    "roleArn": role_arn,
                    "type": EXPRESS,
                    "encryptionConfiguration": customer_managed(key),
                }),
            )
            .await;
        string(&created, "stateMachineArn")
    }
}

/// The trust policy a role this account's own credentials assume carries.
fn trusted_by_the_account() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
            "Action": "sts:AssumeRole",
        }]
    })
}

/// The trust policy a role Step Functions acts as carries.
fn trusted_by_step_functions() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "states.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }]
    })
}

/// "The default key policy … gives the Amazon Web Services account that owns the KMS key
/// permission to use it in all cryptographic operations"
/// (`smithy_query(query="doc",shape="CreateKeyRequest",member="Policy")`): the statement
/// names the account root, so it DELEGATES the decision to that account's own
/// identity-based policies rather than granting to every principal in it.
fn account_root_statement() -> Value {
    json!({
        "Sid": "EnableIAMUserPermissions",
        "Effect": "Allow",
        "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
        "Action": "kms:*",
        "Resource": "*"
    })
}

/// A statement stating `effect` for ONE named principal over `actions`.
fn statement_for(principal: &str, effect: &str, actions: Value) -> Value {
    json!({
        "Sid": "TheCaller",
        "Effect": effect,
        "Principal": { "AWS": principal },
        "Action": actions,
        "Resource": "*"
    })
}

/// A key policy carrying `statements`.
fn key_policy(statements: Vec<Value>) -> Value {
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// An identity policy allowing every action of Step Functions and nothing of KMS: the
/// least-privilege caller, which relies on the key's own policy for the key.
fn states_only() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": "states:*", "Resource": "*" }]
    })
}

/// The same, plus the decrypt a read of sealed data makes — what a key policy that
/// delegates to the account leaves to the principal's own documents.
fn states_and_decrypt() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["states:*", "kms:Decrypt", "kms:DescribeKey"],
            "Resource": "*",
        }]
    })
}

/// The policy a state machine's own execution role carries: the cryptographic calls
/// sealing an execution's data makes.
fn execution_role_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["kms:GenerateDataKey", "kms:Decrypt"],
            "Resource": "*",
        }]
    })
}

/// The message a refusal carries.
fn message(answer: &Value) -> String {
    answer
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One row of the authorization table: the key policy the key carries, what the calling
/// role's own documents allow, and what the run answers.
struct Row {
    case: &'static str,
    /// The key policy, built from the ARN of the role the call is made as.
    policy: fn(&str) -> Value,
    /// Whether the calling role's own identity policy allows the decrypt.
    caller_allows_decrypt: bool,
    /// The sentence the refusal carries, or `None` when the run is served.
    refused_with: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The positive control: the documented least-privilege caller — a key policy
            // naming the calling role with exactly the two actions a read of sealed data
            // makes, and no KMS permission of the role's own.
            case: "the key policy grants the calling role the decrypt and the key read",
            policy: |role| {
                key_policy(vec![
                    account_root_statement(),
                    statement_for(role, "Allow", json!(["kms:Decrypt", "kms:DescribeKey"])),
                ])
            },
            caller_allows_decrypt: false,
            refused_with: None,
        },
        Row {
            // The same grant written as every operation of the service, and with NO
            // statement for the account at all: a statement naming the principal itself
            // is the whole authority a call needs.
            case: "the key policy grants the calling role every cryptographic operation",
            policy: |role| key_policy(vec![statement_for(role, "Allow", json!("kms:*"))]),
            caller_allows_decrypt: false,
            refused_with: None,
        },
        Row {
            case: "the key policy delegates to the account and the role's own policy allows it",
            policy: |_role| key_policy(vec![account_root_statement()]),
            caller_allows_decrypt: true,
            refused_with: None,
        },
        Row {
            case: "the key policy delegates to the account and the role allows nothing of KMS",
            policy: |_role| key_policy(vec![account_root_statement()]),
            caller_allows_decrypt: false,
            refused_with: Some(NO_IDENTITY_POLICY),
        },
        Row {
            case: "the key policy denies the calling role explicitly",
            policy: |role| {
                key_policy(vec![
                    account_root_statement(),
                    statement_for(role, "Deny", json!(["kms:Decrypt", "kms:DescribeKey"])),
                ])
            },
            caller_allows_decrypt: true,
            refused_with: Some(EXPLICIT_DENY),
        },
        Row {
            case: "the key policy names another principal only",
            policy: |_role| {
                key_policy(vec![statement_for(
                    &format!("arn:aws:iam::{ACCOUNT}:role/another-reader"),
                    "Allow",
                    json!(["kms:Decrypt", "kms:DescribeKey"]),
                )])
            },
            caller_allows_decrypt: true,
            refused_with: Some(NO_RESOURCE_POLICY),
        },
    ]
}

/// The two sentences a refusal does NOT carry when it carries `stated`: the wording of an
/// explicit deny and that of a grant merely absent have to be distinguishable, so a
/// client is not sent to re-check a document that is already correct.
fn other_sentences(stated: &str) -> Vec<&'static str> {
    [NO_RESOURCE_POLICY, NO_IDENTITY_POLICY, EXPLICIT_DENY]
        .into_iter()
        .filter(|sentence| *sentence != stated)
        .collect()
}

/// `StartSyncExecution` on a CMK-encrypted EXPRESS state machine follows the authorities
/// KMS weighs over the CALLER's use of that key: a key policy naming the calling
/// principal itself serves the run with the posted input reported back verbatim, one that
/// delegates to the account leaves the decision to the principal's own documents, and a
/// refusal is the modelled `KmsAccessDeniedException` carrying the sentence for the
/// document that withheld the action.
#[tokio::test]
async fn a_synchronous_run_follows_the_key_policy_for_the_calling_principal() {
    let services = Services::new();
    let execution_role = services
        .role(
            "sync-run-execution-role",
            &trusted_by_step_functions(),
            &execution_role_policy(),
        )
        .await;

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            policy,
            caller_allows_decrypt,
            refused_with,
        } = row;

        let caller_role = format!("sync-run-caller-{index}");
        let identity = if caller_allows_decrypt {
            states_and_decrypt()
        } else {
            states_only()
        };
        let caller_arn = services
            .role(&caller_role, &trusted_by_the_account(), &identity)
            .await;
        let (access_key_id, session_arn) = services.assume(&caller_role).await;

        // The key is created, and the state machine configured on it, while the account's
        // own statement still grants: what is under test is the RUN, not the setup.
        let key = services.create_key(&key_policy(vec![account_root_statement()])).await;
        let machine = services
            .express_machine(&format!("sync-run-{index}"), &key, &execution_role)
            .await;
        services.put_policy(&key, &policy(&caller_arn)).await;

        let request = json!({
            "stateMachineArn": &machine,
            "input": INPUT,
            "includedData": "ALL_DATA",
        });
        let (status, answer) = services
            .call_as(&access_key_id, &SFN, "StartSyncExecution", &request)
            .await;

        let Some(sentence) = refused_with else {
            assert_eq!(
                status, 200,
                "case {case:?}: a caller the key permits must be served, got {status} with \
                 {answer:?}"
            );
            assert_eq!(
                answer.get("status").and_then(Value::as_str),
                Some(SUCCEEDED),
                "case {case:?}: expected the run to report {SUCCEEDED}, got {answer:?}"
            );
            // The whole-data projection reports the posted input VERBATIM — the spacing
            // and member order the caller wrote — and says the data was included.
            assert_eq!(
                answer.get("input").and_then(Value::as_str),
                Some(INPUT),
                "case {case:?}: expected the posted input reported back verbatim, got {answer:?}"
            );
            assert_eq!(
                answer
                    .get("inputDetails")
                    .and_then(|details| details.get("included"))
                    .and_then(Value::as_bool),
                Some(true),
                "case {case:?}: expected inputDetails to report the data as included, got \
                 {answer:?}"
            );
            // Round trip through the run's own result: what the definition computed.
            let output: Value = serde_json::from_str(
                answer
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
            .unwrap_or(Value::Null);
            assert_eq!(
                output,
                json!({ "msg": "hi" }),
                "case {case:?}: expected the run's own result, got {answer:?}"
            );
            assert!(
                !string(&answer, "executionArn").is_empty(),
                "case {case:?}: expected the run to report its execution ARN, got {answer:?}"
            );
            continue;
        };

        let code = error_code(&answer);
        assert_eq!(
            (status, code.as_str()),
            (KMS_ACCESS_DENIED_STATUS, KMS_ACCESS_DENIED),
            "case {case:?}: expected the modelled KMS refusal, got {status} {code} with {answer:?}"
        );
        let stated = message(&answer);
        // Every variable part of the relayed sentence comes from the request and the
        // state: the principal the request signed as, the operation on the key, and the
        // key itself.
        assert!(
            stated.contains(sentence)
                && stated.contains(&session_arn)
                && stated.contains("kms:Decrypt")
                && stated.contains(&key),
            "case {case:?}: expected the refusal to state {sentence:?} for {session_arn} on \
             {key}, got {stated:?}"
        );
        for other in other_sentences(sentence) {
            assert!(
                !stated.contains(other),
                "case {case:?}: the refusal must be worded for the document that withheld the \
                 action and not also {other:?}, got {stated:?}"
            );
        }

        // ...and the SAME request is served as soon as the key policy names the caller,
        // so nothing of the refusal was latched.
        services
            .put_policy(
                &key,
                &key_policy(vec![
                    account_root_statement(),
                    statement_for(&caller_arn, "Allow", json!(["kms:Decrypt", "kms:DescribeKey"])),
                ]),
            )
            .await;
        let (status, answer) = services
            .call_as(&access_key_id, &SFN, "StartSyncExecution", &request)
            .await;
        assert_eq!(
            (status, answer.get("status").and_then(Value::as_str)),
            (200, Some(SUCCEEDED)),
            "case {case:?}: the restored statement serves the same run, got {status} with \
             {answer:?}"
        );
    }
}

/// What the caller is asked for follows `includedData`: the projection that reports the
/// run's data needs the caller's decrypt — an absent member asks for all of it — while
/// `METADATA_ONLY` reports none of it, decrypts nothing and is served to a caller the key
/// permits nothing.
#[tokio::test]
async fn a_metadata_only_run_asks_the_caller_for_nothing_of_the_key() {
    let services = Services::new();
    let execution_role = services
        .role(
            "included-data-execution-role",
            &trusted_by_step_functions(),
            &execution_role_policy(),
        )
        .await;
    let caller_role = "included-data-caller";
    services
        .role(caller_role, &trusted_by_the_account(), &states_only())
        .await;
    let (access_key_id, _session) = services.assume(caller_role).await;

    let key = services.create_key(&key_policy(vec![account_root_statement()])).await;
    let machine = services
        .express_machine("included-data-run", &key, &execution_role)
        .await;

    // (case, the `includedData` the request carries, whether the run is served)
    let projections: Vec<(&str, Option<&str>, bool)> = vec![
        ("the whole-data projection", Some("ALL_DATA"), false),
        ("no includedData member at all", None, false),
        ("the metadata-only projection", Some("METADATA_ONLY"), true),
    ];

    for (case, included_data, served) in projections {
        let mut request = json!({ "stateMachineArn": &machine, "input": INPUT });
        if let Some(included_data) = included_data {
            request["includedData"] = json!(included_data);
        }
        let (status, answer) = services
            .call_as(&access_key_id, &SFN, "StartSyncExecution", &request)
            .await;
        if !served {
            let code = error_code(&answer);
            assert_eq!(
                (status, code.as_str()),
                (KMS_ACCESS_DENIED_STATUS, KMS_ACCESS_DENIED),
                "case {case:?}: a projection that reports the run's data needs the caller's \
                 decrypt, got {status} {code} with {answer:?}"
            );
            continue;
        }
        assert_eq!(
            (status, answer.get("status").and_then(Value::as_str)),
            (200, Some(SUCCEEDED)),
            "case {case:?}: a projection that reports none of the data needs no grant, got \
             {status} with {answer:?}"
        );
        for withheld in ["input", "inputDetails", "output", "outputDetails"] {
            assert!(
                answer.get(withheld).is_none(),
                "case {case:?}: expected {withheld:?} to be withheld, got {answer:?}"
            );
        }
    }
}

/// One KMS call about a key: the operation, and the request built from the key's ARN.
type KeyCall = (&'static str, fn(&str) -> Value);

/// One key state a cryptographic operation cannot use the key in, and how the account
/// puts the key back into one it can.
struct KeyState {
    case: &'static str,
    /// The KMS call that puts the key into that state.
    enter: KeyCall,
    /// The calls that bring the key back, in order.
    leave: &'static [KeyCall],
    /// The value the refusal reports for `kmsKeyState`, as this service's own
    /// `KmsKeyState` spells it.
    reported: &'static str,
    /// How KMS words that state in the sentence it refuses with.
    prose: &'static str,
}

fn key_states() -> Vec<KeyState> {
    vec![
        KeyState {
            case: "the key is disabled",
            enter: ("DisableKey", |key| json!({ "KeyId": key })),
            leave: &[("EnableKey", |key| json!({ "KeyId": key }))],
            reported: "DISABLED",
            prose: "is disabled.",
        },
        KeyState {
            case: "the key's deletion is scheduled",
            enter: ("ScheduleKeyDeletion", |key| {
                json!({ "KeyId": key, "PendingWindowInDays": 7 })
            }),
            // Cancelling a scheduled deletion leaves the key DISABLED, so putting it back
            // into use takes both calls.
            leave: &[
                ("CancelKeyDeletion", |key| json!({ "KeyId": key })),
                ("EnableKey", |key| json!({ "KeyId": key })),
            ],
            reported: "PENDING_DELETION",
            prose: "is pending deletion.",
        },
    ]
}

/// The key's own KEY STATE decides the run before any caller's grant does: a key in a
/// state no cryptographic operation can use refuses a caller the key policy names
/// outright, with `KmsInvalidStateException` carrying that key state — and serves the
/// very same request again once the account puts the key back.
#[tokio::test]
async fn a_key_in_an_unusable_state_refuses_even_a_granted_caller() {
    let services = Services::new();
    let execution_role = services
        .role(
            "key-state-execution-role",
            &trusted_by_step_functions(),
            &execution_role_policy(),
        )
        .await;

    for (index, state) in key_states().into_iter().enumerate() {
        let KeyState {
            case,
            enter,
            leave,
            reported,
            prose,
        } = state;

        let caller_role = format!("key-state-caller-{index}");
        let caller_arn = services
            .role(&caller_role, &trusted_by_the_account(), &states_only())
            .await;
        let (access_key_id, _session) = services.assume(&caller_role).await;

        let key = services
            .create_key(&key_policy(vec![
                account_root_statement(),
                statement_for(&caller_arn, "Allow", json!(["kms:Decrypt", "kms:DescribeKey"])),
            ]))
            .await;
        let machine = services
            .express_machine(&format!("key-state-run-{index}"), &key, &execution_role)
            .await;
        let request = json!({
            "stateMachineArn": &machine,
            "input": INPUT,
            "includedData": "ALL_DATA",
        });

        // The control: the caller the key policy names is served while the key is usable.
        let (status, answer) = services
            .call_as(&access_key_id, &SFN, "StartSyncExecution", &request)
            .await;
        assert_eq!(
            (status, answer.get("status").and_then(Value::as_str)),
            (200, Some(SUCCEEDED)),
            "case {case:?}: the granted caller must be served while the key is usable, got \
             {status} with {answer:?}"
        );

        let (operation, body) = enter;
        services.kms(operation, &body(&key)).await;
        let (status, answer) = services
            .call_as(&access_key_id, &SFN, "StartSyncExecution", &request)
            .await;
        let code = error_code(&answer);
        assert_eq!(
            (status, code.as_str()),
            (KMS_INVALID_STATE_STATUS, KMS_INVALID_STATE),
            "case {case:?}: expected the modelled key-state refusal, got {status} {code} with \
             {answer:?}"
        );
        assert_eq!(
            answer.get("kmsKeyState").and_then(Value::as_str),
            Some(reported),
            "case {case:?}: expected the refusal to report the key state the caller has to \
             change, got {answer:?}"
        );
        let stated = message(&answer);
        assert!(
            stated.contains(&key) && stated.contains(prose),
            "case {case:?}: expected the refusal to name the key and {prose:?}, got {stated:?}"
        );

        for (operation, body) in leave {
            services.kms(operation, &body(&key)).await;
        }
        let (status, answer) = services
            .call_as(&access_key_id, &SFN, "StartSyncExecution", &request)
            .await;
        assert_eq!(
            (status, answer.get("status").and_then(Value::as_str)),
            (200, Some(SUCCEEDED)),
            "case {case:?}: a key put back into use serves the same run, got {status} with \
             {answer:?}"
        );
    }
}
