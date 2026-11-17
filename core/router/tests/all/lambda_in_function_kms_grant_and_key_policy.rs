// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING function's own AWS KMS call is authorized against: the grants on the key and
//! the key's own policy, decided for the session of the function's execution role.
//!
//! RULE: a call an AWS SDK makes from inside a running function is made as a session of that
//! function's execution role, and the service it names decides each such call afresh against the
//! authorities then in force — for a cryptographic operation on a KMS key those are the key's own
//! policy, the identity-based documents on the calling role, and the grants on that key issued to
//! it, where a grant permits exactly the operations it lists under its own encryption-context
//! constraints and an explicit `Deny` in either document overrides every allow; a request no
//! authority permits is refused the AWS-common `AccessDeniedException` (HTTP 400) naming the
//! assumed-role session, the `kms:<Operation>` action and the key's ARN, with the clause for the
//! document that has to change — `because no identity-based policy allows the <action> action`
//! where the key policy delegates to the account, and `with an explicit deny in a resource-based
//! policy` where the key policy denies outright.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`; with neither an identity-based nor a
//! resource-based allow a request is denied (IAM User Guide, *Policy evaluation logic*), and an
//! explicit deny wins over every allow. A grant's constraints "allow the permissions in the grant
//! only when the encryption context in the request matches (EncryptionContextEquals) or includes
//! (EncryptionContextSubset) the encryption context specified in the constraint"
//! (`smithy_query(query="doc",shape="CreateGrantRequest",member="Constraints")`), and KMS's own
//! model declares no authorization error at all
//! (`smithy_query(query="operation",operation="Decrypt")` lists `DisabledException`,
//! `KMSInvalidStateException`, `NotFoundException`, `InvalidCiphertextException`,
//! `IncorrectKeyException` and no refusal of permission), so the refusal is the AWS-common one.
//!
//! Five crates have to agree for that to hold — IAM holds the role and the inline document, STS
//! mints the session the function's calls are signed with, CloudFormation archives the inline
//! handler into a deployment package this emulator can run, Lambda runs it and supplies the SDK
//! the handler builds its `boto3.client("kms")` from, and KMS holds the key, the grants and the
//! policy and takes the decision — so the suite lives here: a crate-local suite can only ever see
//! one side of it. The KMS crate's own `grants` and `key_policy_deny_scope` suites pin the same
//! decision for a credential a client signs with directly, and
//! `lambda_runtime_sdk_reads_an_sse_kms_object` pins a key policy met through ANOTHER service's
//! request; what only this suite reaches is a handler's OWN `kms:Decrypt` being refused, served
//! while a grant naming its role is live, and refused again the moment that grant is revoked.
//!
//! The rows of the first test differ along what the grant rule generalises over: WHICH operations
//! the grant lists (the one asked for, another, several including it), WHICH encryption-context
//! constraint it carries (none, a subset the request carries, a subset it does not, an equality
//! over the whole context, an equality over part of it) and WHOSE grant it is (the function's own
//! role, another role of the account). The rows of the second differ along what the explicit-deny
//! rule generalises over: WHICH action the key policy denies (the decrypt, the encrypt, every
//! action of the service, none) and WHOSE access it denies (the function's role, another role) —
//! each row probed on BOTH operations the handler performs, so a deny wider or narrower than the
//! action it names fails here.
//!
//! The rows of the third test hold the denied ACTION fixed and differ along the two elements of the
//! statement the second test keeps constant: WHICH spelling of the caller its `Principal` names —
//! the execution role's own ARN, and the assumed-role session ARN the function's own calls are made
//! as — and WHICH resources its `Resource` element covers: every resource, the key carrying the
//! statement, a wildcard over this account's keys in any Region, and ANOTHER key of the account,
//! which the request's key is not. Beside every row the same invocation probes an untouched control
//! key, and the account's own credential asks both operations on each denied key afterwards: a deny
//! reaches one principal, one action and one resource, so a key whose policy denies the role's
//! decrypt still serves that role's encrypt on the same key, the control key, and the account the
//! policy delegates to.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};
// The three pieces a neighbour already states once: the trust policy an execution role has to
// carry for Lambda to be handed it, the ARN the account holds a role under, and the trailing
// clause of the refusal that found no `Allow` — taken from them rather than written again here.
use crate::sns_lambda_subscription_permission_boundary::trust_policy;
use crate::sts_session_credential_refusals::{no_grant, role_arn};

/// The Region every call below is signed for, and so the Region the keys, the grants and the
/// functions belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every session ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// An access key id neither IAM nor the token service holds a record for: it resolves to the
/// account's own identity, which no identity-based document bounds, so every call this suite makes
/// on its own behalf — creating the key, sealing the plaintext, writing the grant — is the
/// account's and not any role's.
const ACCOUNT_KEY: &str = "EMULATORKMSSETUPKEY1";

/// Where Lambda binds the function collection and one invocation.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and attribute CloudFormation's inline source is archived under.
const HANDLER: &str = "index.handler";

/// The `X-Amz-Target` prefix KMS binds its operations under: its service shape's own name.
const KMS_TARGET_PREFIX: &str = "TrentService";

/// The two IAM actions the handler's own calls perform, as a grant, a statement and a refusal name
/// them.
const DECRYPT_ACTION: &str = "kms:Decrypt";
const ENCRYPT_ACTION: &str = "kms:Encrypt";

/// The code and status KMS refuses a cryptographic operation no authority in force permits with:
/// the AWS-common `AccessDeniedException`, which this service's model declares no shape for.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u64 = 400;

/// The `State` a function serves invocations in, and the status its stack settles at.
const FUNCTION_ACTIVE: &str = "Active";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long this suite waits for work the clock owes before giving up, and how often it looks.
/// Nothing here tests how long anything takes, so the deadline is generous: the condition is
/// waited FOR rather than slept past.
const WAIT: Duration = Duration::from_secs(120);
const LOOK: Duration = Duration::from_millis(50);

/// The timeout every function below is created with. Nothing here tests a function's own timeout,
/// so it is given generous room: a run shares this process with every other test of this target.
const FUNCTION_TIMEOUT: u64 = 120;

/// The plaintext the account seals under each key, which a served decrypt must give back.
const PLAINTEXT: &str = "the-sealed-material";

/// The plaintext a handler's own encrypt seals, which the account then opens again.
const HANDLER_PLAINTEXT: &str = "sealed-from-inside-the-function";

/// The encryption context the sealing call carries, and so the one every decrypt must repeat: "the
/// encryption context in the decryption operation must be an exact, case-sensitive match for the
/// keys and values in the encryption context of the encryption operation"
/// (`smithy_query(query="shape",shape="GrantConstraints")`). Two entries, so a constraint over one
/// of them is a strict subset of it.
const CONTEXT: &[(&str, &str)] = &[("tenant", "alpha"), ("stage", "prod")];

/// [`CONTEXT`] as the map an `EncryptionContext` member carries.
fn context() -> Value {
    Value::Object(
        CONTEXT
            .iter()
            .map(|(name, value)| ((*name).to_string(), json!(value)))
            .collect(),
    )
}

/// The handler every function below runs.
///
/// The client is built at MODULE scope — where a runtime whose SDK cannot resolve the service
/// token fails the whole initialization rather than one call — and each probe performs the
/// operations the event names, reporting the service's own answer or the refusal it raised.
const PROBE_HANDLER: &str = r#"
import base64

import boto3
import botocore.exceptions

KMS = boto3.client("kms")


def _refusal(refused):
    error = refused.response["Error"]
    return {
        "denied": True,
        "code": error["Code"],
        "message": error["Message"],
        "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
    }


def _encrypt(probe):
    try:
        answered = KMS.encrypt(
            KeyId=probe["key_arn"],
            Plaintext=probe["plaintext"].encode("utf-8"),
            EncryptionContext=probe["encryption_context"],
        )
    except botocore.exceptions.ClientError as refused:
        return _refusal(refused)
    return {
        "denied": False,
        "key_id": answered["KeyId"],
        "ciphertext_b64": base64.b64encode(answered["CiphertextBlob"]).decode("ascii"),
    }


def _decrypt(probe):
    try:
        answered = KMS.decrypt(
            CiphertextBlob=base64.b64decode(probe["ciphertext_b64"]),
            KeyId=probe["key_arn"],
            EncryptionContext=probe["encryption_context"],
        )
    except botocore.exceptions.ClientError as refused:
        return _refusal(refused)
    return {
        "denied": False,
        "key_id": answered["KeyId"],
        "plaintext": answered["Plaintext"].decode("utf-8"),
    }


def handler(event, context):
    reports = []
    for probe in event["probes"]:
        report = {
            "case": probe["case"],
            "service": KMS.meta.service_model.service_name,
        }
        if probe.get("encrypt"):
            report["encrypt"] = _encrypt(probe)
        if probe.get("decrypt"):
            report["decrypt"] = _decrypt(probe)
        reports.append(report)
    return {"probes": reports}
"#;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back into
/// this same router, so the key it reaches is the key this suite's KMS holds.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite signs nothing; the credential scope is what routes a request. The handler's OWN
    // call is signed by the runtime, with the session it was given.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// What one Lambda call answered.
struct Answer {
    status: u16,
    function_error: Option<String>,
    body: Vec<u8>,
}

impl Answer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or_else(|_| Value::String(self.text()))
        }
    }
}

/// One Lambda `restJson1` call, made with the account's own credential.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = body.map_or_else(Vec::new, |document| document.to_string().into_bytes());
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, answered) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        // "X-Amz-Function-Error — If present, indicates that an error occurred during function
        // execution" (`Invoke`'s own output).
        function_error: parts
            .headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body: answered.to_vec(),
    }
}

/// One KMS `awsJson1_1` call, as the account's own credential, with its status and parsed body.
async fn kms_any(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("kms.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{KMS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("kms"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One KMS call that must be served.
async fn kms(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = kms_any(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: KMS {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call against `signing_name` that must answer 200; answers its XML body.
async fn query(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from {signing_name}, got {status} with {text}"
    );
    text
}

/// How many looks fit in [`WAIT`] at [`LOOK`] apart.
fn looks() -> u32 {
    u32::try_from(WAIT.as_millis() / LOOK.as_millis().max(1)).unwrap_or(u32::MAX)
}

/// The statements every role below carries whatever else it does: the log writes a FUNCTION makes
/// (the actions `AWSLambdaBasicExecutionRole` names), which say nothing about any key.
///
/// Written here rather than taken from `sfn_task_target_authorization::logs_only`: that one is a
/// state machine's delivery actions, which are not these.
fn logs_only() -> Value {
    json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })
}

/// Create the role `name` with the Lambda trust policy and `statements` as its one inline document,
/// and answer the ARN IAM minted for it.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    statements: Value,
) -> String {
    let created = query(
        router,
        case,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    let document = json!({"Version": "2012-10-17", "Statement": statements}).to_string();
    query(
        router,
        case,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "in-force"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    element(&created, "Arn")
}

/// Deploy `function` running [`PROBE_HANDLER`] as `role_arn` through `stack`, and wait until Lambda
/// reports it active.
///
/// The handler is declared as the stack's inline source, which CloudFormation archives into the
/// deployment package — the way a function whose handler is written out in a suite comes to exist
/// at all.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    function: &str,
    role_arn: &str,
) {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": "python3.12",
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            },
        },
    })
    .to_string();
    query(
        router,
        case,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    // The stack's completion and the function's provisioning window are owed by the CLOCK, which
    // no fixed sleep names: this looks until they hold.
    let mut last = String::new();
    for _ in 0..looks() {
        last = query(
            router,
            case,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", stack)],
        )
        .await;
        if element(&last, "StackStatus") == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        element(&last, "StackStatus"),
        CREATE_COMPLETE,
        "case {case:?}: the stack {stack:?} must reach {CREATE_COMPLETE}, last saw {last}"
    );
    let mut configuration = Value::Null;
    let mut active = false;
    for _ in 0..looks() {
        configuration = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            None,
        )
        .await
        .json();
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            active = true;
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert!(
        active,
        "case {case:?}: {function:?} must reach {FUNCTION_ACTIVE}, last saw {configuration}"
    );
}

/// Invoke `function` with `probes` and answer the report it made for each, in order.
///
/// A run that never imported its module, or one whose handler raised, is a FUNCTION error rather
/// than a report, so that is where this stops: the suite's assertions are about what KMS answered
/// the handler, and that only exists for a run that completed.
async fn probe(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    probes: Vec<Value>,
) -> Vec<Value> {
    let expected = probes.len();
    let answered = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
        Some(&json!({"probes": probes})),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {} with {}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.function_error,
        None,
        "case {case:?}: the handler must run to completion — a client the runtime's SDK could not \
         build is a function error — got {:?} with {}",
        answered.function_error,
        answered.text()
    );
    let payload = answered.json();
    let reports = payload["probes"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        reports.len(),
        expected,
        "case {case:?}: expected one report per probe, got {payload}"
    );
    for report in &reports {
        assert_eq!(
            report["service"],
            json!("kms"),
            "case {case:?}: the client the handler called through must be the key service's, got \
             {report}"
        );
    }
    reports
}

/// The refusal sentence, with `reason` as its trailing clause.
///
/// The clauses come from the tree's one spelling of them ([`no_grant`] for the no-`Allow`
/// diagnosis), but the SENTENCE is spelled here rather than taken from
/// `sts_session_credential_refusals::not_authorized`, which quotes the resource (`on resource:
/// "<arn>"`): the key service words it UNQUOTED, which is what its own refusals carry, so the two
/// are different fixed texts rather than one drifting.
fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// How AWS words the refusal of a principal whose own identity-based documents are the permission
/// it is missing — the sentence for a key whose policy DELEGATES the decision to the caller's
/// account.
fn no_identity_policy_message(principal: &str, action: &str, resource: &str) -> String {
    not_authorized(principal, action, resource, &no_grant(action))
}

/// How AWS words the refusal a resource's OWN policy explicitly denied.
fn explicit_resource_deny_message(principal: &str, action: &str, resource: &str) -> String {
    not_authorized(
        principal,
        action,
        resource,
        "with an explicit deny in a resource-based policy",
    )
}

/// The session a function's own calls are signed as: "the execution role grants the function
/// permission to use Amazon Web Services services" (`FunctionConfiguration$Role`), and the identity
/// those calls speak for is a session of that role named after the function.
fn session_arn(role: &str, function: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{function}")
}

/// Assert that a probe's `half` (`"encrypt"` or `"decrypt"`) was refused exactly the way the
/// document that withheld it is worded.
fn assert_refused(case: &str, report: &Value, half: &str, message: &str) {
    let answered = &report[half];
    assert_eq!(
        answered["denied"],
        json!(true),
        "case {case:?}: expected the {half} to be refused, got {report}"
    );
    assert_eq!(
        answered["code"],
        json!(ACCESS_DENIED),
        "case {case:?}: expected the AWS-common authorization refusal, got {report}"
    );
    assert_eq!(
        answered["status"],
        json!(ACCESS_DENIED_STATUS),
        "case {case:?}: expected the status that refusal carries, got {report}"
    );
    assert_eq!(
        answered["message"],
        json!(message),
        "case {case:?}: expected the sentence the deciding document earns, got {report}"
    );
}

/// Mint one symmetric encryption key under `policy` — the document the row is about — and answer
/// `(key id, key ARN)`.
///
/// A row's document names whichever principal the row is about, so the policy-lockout safety check
/// is bypassed rather than written around: that check is the KMS crate's own suite's subject, not
/// this one's.
async fn given_key(router: &MultiServiceRouter, case: &str, policy: Option<String>) -> String {
    let mut body = json!({"Description": case});
    if let Some(policy) = policy {
        let members = body.as_object_mut().expect("the body is an object");
        members.insert("Policy".to_string(), json!(policy));
        members.insert("BypassPolicyLockoutSafetyCheck".to_string(), json!(true));
    }
    let created = kms(router, case, "CreateKey", &body).await;
    created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: CreateKey must answer the key's Arn, got {created}")
        })
        .to_string()
}

/// Seal `plaintext` under `key_arn` with [`CONTEXT`], as the account's own credential, and answer
/// the ciphertext in the base64 an AWS JSON protocol carries a blob as — which is the form the
/// handler is handed it in.
async fn given_ciphertext(
    router: &MultiServiceRouter,
    case: &str,
    key_arn: &str,
    plaintext: &str,
) -> String {
    let sealed = kms(
        router,
        case,
        "Encrypt",
        &json!({
            "KeyId": key_arn,
            "Plaintext": ::aws_smithy_types::base64::encode(plaintext.as_bytes()),
            "EncryptionContext": context(),
        }),
    )
    .await;
    sealed["CiphertextBlob"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: Encrypt must answer a CiphertextBlob, got {sealed}")
        })
        .to_string()
}

/// The encryption-context constraint one grant carries.
enum Constraint {
    /// No constraint at all, so the grant covers the operation under any context.
    Unconstrained,
    /// `EncryptionContextSubset`: the pairs the request's context must INCLUDE.
    Subset(&'static [(&'static str, &'static str)]),
    /// `EncryptionContextEquals`: the pairs the request's context must MATCH exactly.
    Equals(&'static [(&'static str, &'static str)]),
}

impl Constraint {
    /// The `Constraints` member this constraint travels in, if any.
    fn wire(&self) -> Option<Value> {
        let (member, pairs) = match self {
            Self::Unconstrained => return None,
            Self::Subset(pairs) => ("EncryptionContextSubset", pairs),
            Self::Equals(pairs) => ("EncryptionContextEquals", pairs),
        };
        let map: serde_json::Map<String, Value> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), json!(value)))
            .collect();
        Some(json!({member: Value::Object(map)}))
    }
}

/// Whose grant a row writes: a grant gives its permissions to "the grantee principal" and to
/// nobody else, and a session of a role is the role it is a session of.
enum Grantee {
    /// The role the probe function runs as.
    TheFunctionsRole,
    /// Another role of the same account, which the function never assumes.
    AnotherRole,
}

/// One row of the grant table: the grant written on the key while the probe runs, and whether the
/// handler's decrypt must be served while it is live.
struct GrantRow {
    case: &'static str,
    /// The `Operations` the grant lists.
    operations: &'static [&'static str],
    /// The `Constraints` it carries.
    constraint: Constraint,
    /// Whose grant it is.
    grantee: Grantee,
    /// Whether the decrypt made while this grant is live must be served.
    authorizes: bool,
}

/// The grant rows, each written and then revoked in turn on ONE key: what varies is which
/// operations the grant lists, which encryption-context constraint it carries, and whose grant it
/// is. The operation names are members of the model's own `GrantOperation` enum
/// (`smithy_query(query="shape",shape="GrantOperation")`).
fn grant_rows() -> Vec<GrantRow> {
    vec![
        GrantRow {
            case: "a grant naming the operation asked for, under any context",
            operations: &["Decrypt"],
            constraint: Constraint::Unconstrained,
            grantee: Grantee::TheFunctionsRole,
            authorizes: true,
        },
        GrantRow {
            case: "a grant constrained to a subset of the context the request carries",
            operations: &["Decrypt"],
            constraint: Constraint::Subset(&[("tenant", "alpha")]),
            grantee: Grantee::TheFunctionsRole,
            authorizes: true,
        },
        GrantRow {
            case: "a grant constrained to a subset the request does not carry",
            operations: &["Decrypt"],
            constraint: Constraint::Subset(&[("tenant", "beta")]),
            grantee: Grantee::TheFunctionsRole,
            authorizes: false,
        },
        GrantRow {
            case: "a grant constrained to equal the whole context the request carries",
            operations: &["Decrypt"],
            constraint: Constraint::Equals(CONTEXT),
            grantee: Grantee::TheFunctionsRole,
            authorizes: true,
        },
        GrantRow {
            case: "a grant constrained to equal only part of that context",
            operations: &["Decrypt"],
            constraint: Constraint::Equals(&[("tenant", "alpha")]),
            grantee: Grantee::TheFunctionsRole,
            authorizes: false,
        },
        GrantRow {
            case: "a grant naming the operation among several",
            operations: &["Encrypt", "Decrypt", "DescribeKey"],
            constraint: Constraint::Unconstrained,
            grantee: Grantee::TheFunctionsRole,
            authorizes: true,
        },
        GrantRow {
            case: "a grant naming another operation only",
            operations: &["Encrypt"],
            constraint: Constraint::Unconstrained,
            grantee: Grantee::TheFunctionsRole,
            authorizes: false,
        },
        GrantRow {
            case: "a grant of the same operation to another role of the account",
            operations: &["Decrypt"],
            constraint: Constraint::Unconstrained,
            grantee: Grantee::AnotherRole,
            authorizes: false,
        },
    ]
}

/// RULE, grant half: a handler's own `kms:Decrypt` is decided afresh per call against the grants on
/// the key issued to the session's role — so with no grant at all it is refused the AWS-common
/// `AccessDeniedException` naming the session, the action and the key, a grant that lists the
/// operation and whose encryption-context constraint the request meets serves it, a grant that
/// lists another operation, constrains it to a context the request does not carry or names another
/// principal serves nothing, and revoking a grant that was serving it refuses the very next call.
#[tokio::test]
async fn a_grant_authorizes_a_handlers_own_decrypt_until_it_is_revoked() {
    let router = build_router();
    let setup = "the grant table's key, roles and function";
    let role = "xsvc-in-fn-kms-grantee";
    let bystander = "xsvc-in-fn-kms-bystander";
    let function = "xsvc-in-fn-kms-grant-probe";

    // The role may write its own logs and NOTHING of the key service, so what decides each call is
    // the grant on the key and nothing the role carries.
    let role_arn_minted = given_role(&router, setup, role, json!([logs_only()])).await;
    given_role(&router, setup, bystander, json!([logs_only()])).await;
    assert_eq!(
        role_arn_minted,
        role_arn(role),
        "setup: IAM must mint the role ARN this suite names its principals by"
    );
    let key_arn = given_key(&router, setup, None).await;
    let ciphertext = given_ciphertext(&router, setup, &key_arn, PLAINTEXT).await;
    given_function(
        &router,
        setup,
        "xsvc-in-fn-kms-grant-stack",
        function,
        &role_arn_minted,
    )
    .await;

    let session = session_arn(role, function);
    let refused_message = no_identity_policy_message(&session, DECRYPT_ACTION, &key_arn);
    let decrypt_probe = |case: &str| {
        json!({
            "case": case,
            "decrypt": true,
            "key_arn": key_arn,
            "ciphertext_b64": ciphertext,
            "encryption_context": context(),
        })
    };

    // With no grant and no identity statement, the key policy delegates the decision to the
    // account's own documents — so the refusal blames the role's identity policies.
    let case = "no grant on the key at all";
    let reports = probe(&router, case, function, vec![decrypt_probe(case)]).await;
    assert_refused(case, &reports[0], "decrypt", &refused_message);

    for row in grant_rows() {
        let case = row.case;
        let grantee = match row.grantee {
            Grantee::TheFunctionsRole => role_arn(role),
            Grantee::AnotherRole => role_arn(bystander),
        };
        let mut request = json!({
            "KeyId": key_arn,
            "GranteePrincipal": grantee,
            "Operations": row.operations,
        });
        if let Some(constraints) = row.constraint.wire() {
            request["Constraints"] = constraints;
        }
        let created = kms(&router, case, "CreateGrant", &request).await;
        let grant_id = created["GrantId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: CreateGrant must answer a GrantId, got {created}")
            })
            .to_string();

        let reports = probe(&router, case, function, vec![decrypt_probe(case)]).await;
        let report = &reports[0];
        if row.authorizes {
            assert_eq!(
                report["decrypt"]["denied"],
                json!(false),
                "case {case:?}: expected the live grant to authorize the decrypt, got {report}"
            );
            assert_eq!(
                report["decrypt"]["plaintext"],
                json!(PLAINTEXT),
                "case {case:?}: expected the material the key holds, got {report}"
            );
            assert_eq!(
                report["decrypt"]["key_id"],
                json!(key_arn),
                "case {case:?}: expected the key the service opened the ciphertext with, got \
                 {report}"
            );
        } else {
            assert_refused(case, report, "decrypt", &refused_message);
        }

        kms(
            &router,
            case,
            "RevokeGrant",
            &json!({"KeyId": key_arn, "GrantId": grant_id}),
        )
        .await;
        // Withdrawn, the authority is gone from the very next call — asserted for the rows whose
        // grant was serving the decrypt, which are the ones that had an authority to lose.
        if row.authorizes {
            let case = row.case;
            let reports = probe(&router, case, function, vec![decrypt_probe(case)]).await;
            assert_refused(case, &reports[0], "decrypt", &refused_message);
        }
    }
}

/// One row of the key-policy table: the statement the key's own policy carries beside the
/// account's delegation, and whether each half of the probe must be served under it.
struct PolicyRow {
    case: &'static str,
    /// The action the explicit `Deny` names, if the row's policy carries one.
    denied_action: Option<&'static str>,
    /// Whether that deny names the role the function runs as, rather than another role.
    denies_the_functions_role: bool,
    /// Whether the handler's encrypt and its decrypt must be served under this policy.
    encrypt_served: bool,
    decrypt_served: bool,
}

/// The key-policy rows: what varies is WHICH action the key policy denies and WHOSE access it
/// denies. Every row's role carries the same identity statement allowing both actions, so the only
/// thing that can refuse a call is the key's own document.
const POLICY_ROWS: &[PolicyRow] = &[
    PolicyRow {
        case: "a policy that only delegates to the account",
        denied_action: None,
        denies_the_functions_role: false,
        encrypt_served: true,
        decrypt_served: true,
    },
    PolicyRow {
        case: "an explicit deny of the decrypt for the function's own role",
        denied_action: Some(DECRYPT_ACTION),
        denies_the_functions_role: true,
        encrypt_served: true,
        decrypt_served: false,
    },
    PolicyRow {
        case: "an explicit deny of the encrypt for the function's own role",
        denied_action: Some(ENCRYPT_ACTION),
        denies_the_functions_role: true,
        encrypt_served: false,
        decrypt_served: true,
    },
    PolicyRow {
        case: "an explicit deny of every action of the service for that role",
        denied_action: Some("kms:*"),
        denies_the_functions_role: true,
        encrypt_served: false,
        decrypt_served: false,
    },
    PolicyRow {
        case: "an explicit deny of the decrypt for another role of the account",
        denied_action: Some(DECRYPT_ACTION),
        denies_the_functions_role: false,
        encrypt_served: true,
        decrypt_served: true,
    },
];

/// The statement the DEFAULT key policy carries: the account's root user, which delegates the
/// decision to that account's own identity-based policies ("gives the Amazon Web Services account
/// that owns the KMS key permission to use it", `CreateKeyRequest$Policy`).
fn delegates_to_the_account() -> Value {
    json!({
        "Effect": "Allow",
        "Principal": {"AWS": format!("arn:aws:iam::{ACCOUNT}:root")},
        "Action": "kms:*",
        "Resource": "*",
    })
}

/// The document one key-policy row's key carries.
fn policy_document(row: &PolicyRow, role: &str, bystander: &str) -> String {
    let mut statements = vec![delegates_to_the_account()];
    if let Some(action) = row.denied_action {
        let principal = if row.denies_the_functions_role {
            role_arn(role)
        } else {
            role_arn(bystander)
        };
        statements.push(json!({
            "Effect": "Deny",
            "Principal": {"AWS": principal},
            "Action": action,
            "Resource": "*",
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// RULE, key-policy half: an explicit `Deny` in the key's own policy overrides the identity-based
/// allow the calling role carries — so under a policy denying the decrypt for that role the
/// handler's decrypt is refused the AWS-common `AccessDeniedException` whose sentence blames the
/// resource-based policy while its encrypt on the SAME key is still served, a deny of the encrypt
/// refuses the other half instead, a deny of every action of the service refuses both, a deny
/// naming another role refuses neither — and lifting the deny with the next `PutKeyPolicy` serves
/// the very next call.
#[tokio::test]
async fn an_explicit_key_policy_deny_overrides_a_handlers_identity_allow() {
    let router = build_router();
    let setup = "the key-policy table's keys, roles and function";
    let role = "xsvc-in-fn-kms-policy-caller";
    let bystander = "xsvc-in-fn-kms-policy-bystander";
    let function = "xsvc-in-fn-kms-policy-probe";

    // The role may perform BOTH operations on every key, so nothing it carries can refuse a call:
    // what decides each one is the key's own document.
    let role_arn_minted = given_role(
        &router,
        setup,
        role,
        json!([
            logs_only(),
            {
                "Effect": "Allow",
                "Action": [DECRYPT_ACTION, ENCRYPT_ACTION],
                "Resource": "*",
            },
        ]),
    )
    .await;
    given_role(&router, setup, bystander, json!([logs_only()])).await;
    given_function(
        &router,
        setup,
        "xsvc-in-fn-kms-policy-stack",
        function,
        &role_arn_minted,
    )
    .await;

    let session = session_arn(role, function);
    let mut keys = Vec::new();
    let mut probes = Vec::new();
    for row in POLICY_ROWS {
        let key_arn = given_key(
            &router,
            row.case,
            Some(policy_document(row, role, bystander)),
        )
        .await;
        let ciphertext = given_ciphertext(&router, row.case, &key_arn, PLAINTEXT).await;
        probes.push(json!({
            "case": row.case,
            "encrypt": true,
            "decrypt": true,
            "key_arn": key_arn,
            "plaintext": HANDLER_PLAINTEXT,
            "ciphertext_b64": ciphertext,
            "encryption_context": context(),
        }));
        keys.push(key_arn);
    }

    let reports = probe(&router, "the key-policy table", function, probes).await;
    for ((row, report), key_arn) in POLICY_ROWS.iter().zip(&reports).zip(&keys) {
        let case = row.case;
        assert_eq!(
            report["case"],
            json!(case),
            "case {case:?}: expected the handler to report this row, got {report}"
        );
        if row.decrypt_served {
            assert_eq!(
                report["decrypt"]["denied"],
                json!(false),
                "case {case:?}: expected the decrypt to be served, got {report}"
            );
            assert_eq!(
                report["decrypt"]["plaintext"],
                json!(PLAINTEXT),
                "case {case:?}: expected the material the key holds, got {report}"
            );
        } else {
            assert_refused(
                case,
                report,
                "decrypt",
                &explicit_resource_deny_message(&session, DECRYPT_ACTION, key_arn),
            );
        }
        if !row.encrypt_served {
            assert_refused(
                case,
                report,
                "encrypt",
                &explicit_resource_deny_message(&session, ENCRYPT_ACTION, key_arn),
            );
            continue;
        }
        assert_eq!(
            report["encrypt"]["denied"],
            json!(false),
            "case {case:?}: expected the encrypt to be served, got {report}"
        );
        // Round trip: what the handler sealed is material the key really holds, which the account's
        // own read of the SAME key opens again.
        let sealed = report["encrypt"]["ciphertext_b64"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the served encrypt must answer bytes, got {report}")
            });
        let opened = kms(
            &router,
            case,
            "Decrypt",
            &json!({
                "KeyId": key_arn,
                "CiphertextBlob": sealed,
                "EncryptionContext": context(),
            }),
        )
        .await;
        assert_eq!(
            opened["Plaintext"],
            json!(::aws_smithy_types::base64::encode(HANDLER_PLAINTEXT.as_bytes())),
            "case {case:?}: expected the key to hold what the handler sealed, got {opened}"
        );
    }

    // The policy in force is the one the LAST `PutKeyPolicy` wrote: with the deny lifted, the key
    // whose decrypt was refused serves the very next call.
    let lifted = POLICY_ROWS
        .iter()
        .zip(&keys)
        .find(|(row, _)| !row.decrypt_served)
        .map(|(row, key_arn)| (row.case, key_arn.clone()))
        .expect("a row whose decrypt the key policy refuses");
    let (case, key_arn) = lifted;
    let delegated = json!({
        "Version": "2012-10-17",
        "Statement": [delegates_to_the_account()],
    })
    .to_string();
    kms(
        &router,
        case,
        "PutKeyPolicy",
        &json!({
            "KeyId": key_arn,
            "PolicyName": "default",
            "Policy": delegated,
            "BypassPolicyLockoutSafetyCheck": true,
        }),
    )
    .await;
    let ciphertext = given_ciphertext(&router, case, &key_arn, PLAINTEXT).await;
    let case = "the deny lifted by the next PutKeyPolicy";
    let reports = probe(
        &router,
        case,
        function,
        vec![json!({
            "case": case,
            "decrypt": true,
            "key_arn": key_arn,
            "ciphertext_b64": ciphertext,
            "encryption_context": context(),
        })],
    )
    .await;
    assert_eq!(
        reports[0]["decrypt"]["denied"],
        json!(false),
        "case {case:?}: expected the lifted deny to serve the decrypt, got {:?}",
        reports[0]
    );
    assert_eq!(
        reports[0]["decrypt"]["plaintext"],
        json!(PLAINTEXT),
        "case {case:?}: expected the material the key holds, got {:?}",
        reports[0]
    );
}

/// Which resources one deny row's `Resource` element covers.
enum DenyScope {
    /// `"Resource": "*"` — every resource.
    EveryResource,
    /// The ARN of the key whose own policy carries the statement.
    TheKeyItself,
    /// A wildcard ARN over every key of this account, in any Region.
    EveryKeyOfTheAccount,
    /// The ARN of the control key, which the key carrying the statement is not — so the statement
    /// does not cover the request at all.
    AnotherKeyOfTheAccount,
}

/// How one deny row spells the principal it denies. Both spellings name the same caller: a call a
/// function makes is made by a session of its execution role, and a session of a role is the role
/// it is a session of.
enum DenyPrincipal {
    /// The execution role's own ARN, the spelling an operator copies out of IAM.
    TheExecutionRole,
    /// The assumed-role session ARN the function's own calls are made as.
    TheSessionItself,
}

/// One row of the deny-scope table: how the statement on this row's key names the caller and which
/// resources it covers, and so whether the handler's decrypt under that key is refused.
struct ScopeRow {
    case: &'static str,
    principal: DenyPrincipal,
    scope: DenyScope,
    /// Whether the handler's decrypt under this row's key must be served.
    decrypt_served: bool,
}

/// The deny-scope rows. Every row denies the SAME action to the SAME caller under a different
/// spelling or resource scope, so what each one shows is how far that statement reaches.
const SCOPE_ROWS: &[ScopeRow] = &[
    ScopeRow {
        case: "the execution role's own ARN, over every resource",
        principal: DenyPrincipal::TheExecutionRole,
        scope: DenyScope::EveryResource,
        decrypt_served: false,
    },
    ScopeRow {
        case: "the assumed-role session ARN the function calls as, over every resource",
        principal: DenyPrincipal::TheSessionItself,
        scope: DenyScope::EveryResource,
        decrypt_served: false,
    },
    ScopeRow {
        case: "the execution role's ARN, scoped to the key carrying the statement",
        principal: DenyPrincipal::TheExecutionRole,
        scope: DenyScope::TheKeyItself,
        decrypt_served: false,
    },
    ScopeRow {
        case: "the session ARN, scoped by a wildcard over the account's keys",
        principal: DenyPrincipal::TheSessionItself,
        scope: DenyScope::EveryKeyOfTheAccount,
        decrypt_served: false,
    },
    ScopeRow {
        case: "the execution role's ARN, scoped to another key of the account",
        principal: DenyPrincipal::TheExecutionRole,
        scope: DenyScope::AnotherKeyOfTheAccount,
        decrypt_served: true,
    },
];

/// RULE, the reach of the deny: an explicit `Deny` in a key's policy withholds the action it names
/// from the principal it names on the resources it covers, and nothing else — so the handler's
/// decrypt is refused under either spelling of its own identity and under every resource element
/// that covers the key, served under one that covers only another key, and in every row its encrypt
/// on that same key, its decrypt of an untouched control key, and the account's own two operations
/// on the denied key are all still served.
#[tokio::test]
async fn a_key_policy_deny_reaches_only_the_principal_action_and_keys_it_names() {
    let router = build_router();
    let setup = "the deny-scope table's keys, role and function";
    let role = "xsvc-in-fn-kms-scope-caller";
    let function = "xsvc-in-fn-kms-scope-probe";

    // The identity-based half allows both operations on every resource, so only a key's own
    // document can refuse a call the handler makes.
    let role_arn_minted = given_role(
        &router,
        setup,
        role,
        json!([
            logs_only(),
            {
                "Effect": "Allow",
                "Action": [DECRYPT_ACTION, ENCRYPT_ACTION],
                "Resource": "*",
            },
        ]),
    )
    .await;
    given_function(
        &router,
        setup,
        "xsvc-in-fn-kms-scope-stack",
        function,
        &role_arn_minted,
    )
    .await;
    let session = session_arn(role, function);

    // The control key: no row ever writes a statement to its policy, so what decides a call on it
    // is the delegation it was created with plus the role's identity allow.
    let control_case = "the untouched control key";
    let control_key = given_key(&router, control_case, None).await;
    let control_ciphertext = given_ciphertext(&router, control_case, &control_key, PLAINTEXT).await;

    // Each row's key is created with the default policy and then has the row's document written to
    // it by `PutKeyPolicy` — the call an operator makes to revoke in bulk — which is also what lets
    // a row scope its statement to the key's OWN ARN.
    let mut keys = Vec::new();
    let mut probes = Vec::new();
    for row in SCOPE_ROWS {
        let key_arn = given_key(&router, row.case, None).await;
        let resource = match row.scope {
            DenyScope::EveryResource => "*".to_string(),
            DenyScope::TheKeyItself => key_arn.clone(),
            DenyScope::EveryKeyOfTheAccount => format!("arn:aws:kms:*:{ACCOUNT}:key/*"),
            DenyScope::AnotherKeyOfTheAccount => control_key.clone(),
        };
        let principal = match row.principal {
            DenyPrincipal::TheExecutionRole => role_arn(role),
            DenyPrincipal::TheSessionItself => session.clone(),
        };
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [
                delegates_to_the_account(),
                {
                    "Effect": "Deny",
                    "Principal": {"AWS": principal},
                    "Action": DECRYPT_ACTION,
                    "Resource": resource,
                },
            ],
        })
        .to_string();
        kms(
            &router,
            row.case,
            "PutKeyPolicy",
            &json!({
                "KeyId": key_arn,
                "PolicyName": "default",
                "Policy": document,
                "BypassPolicyLockoutSafetyCheck": true,
            }),
        )
        .await;
        let ciphertext = given_ciphertext(&router, row.case, &key_arn, PLAINTEXT).await;
        probes.push(json!({
            "case": row.case,
            "encrypt": true,
            "decrypt": true,
            "key_arn": key_arn,
            "plaintext": HANDLER_PLAINTEXT,
            "ciphertext_b64": ciphertext,
            "encryption_context": context(),
        }));
        keys.push(key_arn);
    }
    // The control key travels in the SAME invocation as the denied ones: one run of the handler, so
    // what separates the rows is the policy each key carries and nothing about the call.
    probes.push(json!({
        "case": control_case,
        "decrypt": true,
        "key_arn": control_key,
        "ciphertext_b64": control_ciphertext,
        "encryption_context": context(),
    }));

    let reports = probe(&router, "the deny-scope table", function, probes).await;
    for ((row, report), key_arn) in SCOPE_ROWS.iter().zip(&reports).zip(&keys) {
        let case = row.case;
        assert_eq!(
            report["case"],
            json!(case),
            "case {case:?}: expected the handler to report this row, got {report}"
        );
        if row.decrypt_served {
            assert_eq!(
                report["decrypt"]["denied"],
                json!(false),
                "case {case:?}: the statement covers another key, so this decrypt must be served, \
                 got {report}"
            );
            assert_eq!(
                report["decrypt"]["plaintext"],
                json!(PLAINTEXT),
                "case {case:?}: expected the material the key holds, got {report}"
            );
        } else {
            assert_refused(
                case,
                report,
                "decrypt",
                &explicit_resource_deny_message(&session, DECRYPT_ACTION, key_arn),
            );
        }
        // The statement names ONE action, so the other operation on the very same key is served,
        // whatever the row's spelling and scope.
        assert_eq!(
            report["encrypt"]["denied"],
            json!(false),
            "case {case:?}: a deny of the decrypt must leave the encrypt on that key served, got \
             {report}"
        );
    }

    // The control key, asked in the same run by the same session: a deny lives in one key's policy
    // and withholds nothing under another.
    let control = reports
        .last()
        .expect("the invocation reports the control probe");
    assert_eq!(
        control["case"],
        json!(control_case),
        "case {control_case:?}: expected the handler to report the control probe last, got \
         {control}"
    );
    assert_eq!(
        control["decrypt"]["denied"],
        json!(false),
        "case {control_case:?}: the control key carries no deny, so its decrypt must be served, \
         got {control}"
    );
    assert_eq!(
        control["decrypt"]["plaintext"],
        json!(PLAINTEXT),
        "case {control_case:?}: expected the material the control key holds, got {control}"
    );

    // The deny names a principal, so the account the policy still delegates to keeps both
    // operations on every denied key — asked with the account's own credential while every row's
    // statement is in force.
    for (row, key_arn) in SCOPE_ROWS.iter().zip(&keys) {
        let case = row.case;
        let sealed = given_ciphertext(&router, case, key_arn, PLAINTEXT).await;
        let opened = kms(
            &router,
            case,
            "Decrypt",
            &json!({
                "KeyId": key_arn,
                "CiphertextBlob": sealed,
                "EncryptionContext": context(),
            }),
        )
        .await;
        assert_eq!(
            opened["Plaintext"],
            json!(::aws_smithy_types::base64::encode(PLAINTEXT.as_bytes())),
            "case {case:?}: the deny names a principal the account is not, so the account's own \
             decrypt must open the key, got {opened}"
        );
        assert_eq!(
            opened["KeyId"],
            json!(key_arn),
            "case {case:?}: the plaintext is answered under the key that sealed it, got {opened}"
        );
    }
}
