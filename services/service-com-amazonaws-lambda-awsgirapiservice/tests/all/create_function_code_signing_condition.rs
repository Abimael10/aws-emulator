// SPDX-License-Identifier: CC0-1.0

//! IAM authorization of the calls that NAME a code-signing configuration, over the restJson1
//! wire.
//!
//! RULE under test: a request whose input binds a code-signing configuration ARN as a value
//! ABOUT the request — `CreateFunction`'s optional `CodeSigningConfigArn` member and
//! `PutFunctionCodeSigningConfig`'s required one
//! (`smithy_query(query="operation", operation="CreateFunction,PutFunctionCodeSigningConfig")`)
//! — is authorized with that value published as this service's `lambda:CodeSigningConfigArn`
//! request condition key, the key being MODELLED whether or not the request named a value for
//! it. So a statement whose `Condition` is written over that key decides the call on the value
//! it carries, including on the ABSENCE of one where an optional member was omitted, and a
//! grant narrowed to one approved configuration covers only the calls that name it; every
//! other caller is refused `AccessDeniedException` (HTTP 403) carrying the one sentence IAM
//! words an authorization failure with, BEFORE the operation reads or writes anything.
//!
//! The decision is the real one: each row's role and its inline policy are made with IAM's and
//! STS's OWN operations over the same engine (`CreateRole`, `PutRolePolicy`, `AssumeRole`), so
//! the documents the gate reads are the ones those calls wrote, and the caller signs with the
//! access key id `AssumeRole` answered. Every configuration a row names is one
//! `CreateCodeSigningConfig` persisted, and every assertion about what the store holds is read
//! back through `GetFunction` / `GetFunctionCodeSigningConfig`, so an answer that was never
//! persisted — or a write that landed in spite of a refusal — fails here.
//!
//! The rows differ along what the rule generalises over: the OPERATION asked for (the create
//! that states a configuration for a function that does not exist yet, and the put that
//! attaches one to a function that does), WHICH value the request publishes (the approved
//! configuration, another configuration this account holds, and none at all), and the
//! CONDITION OPERATOR the grant is written under (an equality, its `IfExists` spelling, a
//! wildcard match, the existence operator asking for the key's absence, and no condition at
//! all).

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

// The `awsQuery` form encoder and the XML element reader this test target already carries,
// so the IAM and STS fixtures below are written on the same two readings every other suite
// of this crate uses rather than on fresh copies of them.
use crate::capacity_provider_authorization::xml_element;
use crate::event_source_mapping_role_access::form_encode;

/// Where `CreateFunction` is bound
/// (`smithy_query(query="operation", operation="CreateFunction")`: `POST
/// /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Where `CreateCodeSigningConfig` is bound
/// (`smithy_query(query="operation", operation="CreateCodeSigningConfig")`: `POST
/// /2020-04-22/code-signing-configs`).
const CODE_SIGNING_CONFIGS_URI: &str = "/2020-04-22/code-signing-configs";

/// The Region every request below signs for and addresses.
const REGION: &str = "us-east-1";

/// The access key id standing for the account's own published credential: neither STS nor IAM
/// holds a record for it, so it resolves to the account's own identity, which holds no IAM
/// policies — the position AWS account root credentials are in. The fixtures are built with
/// it, because this gate serves it.
const ACCOUNT_KEY_ID: &str = "AKIDTESTONLYACCOUNT1";

/// The name of the inline policy the caller's role carries. `iam:PutRolePolicy` REPLACES the
/// policy of that name, so one role can be re-judged row by row.
const POLICY_NAME: &str = "inline";

/// The session name the caller's role is assumed under.
const SESSION: &str = "probe";

/// The role the rows below sign as, and the execution role every function is created with.
const CALLER_ROLE: &str = "deployer";
const EXECUTION_ROLE: &str = "lambda-exec";

/// The service principal a function's execution role is assumed by.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The condition key a call that names a code-signing configuration publishes a value for, as
/// the IAM Service Authorization Reference spells this service's own keys
/// (`lambda:<MemberName>`).
const CODE_SIGNING_CONFIG_ARN_KEY: &str = "lambda:CodeSigningConfigArn";

/// The wire name and status of the refusal, which no operation of this service models: the
/// service answers the AWS common `AccessDeniedException` at HTTP 403.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The `CodeSigningPolicy` the configurations below carry, from
/// `smithy_query(query="shape", shape="CodeSigningPolicy")` (`Warn | Enforce`): the value that
/// ADMITS a deployment whose signature checks fail, since these rows deploy an unsigned
/// package under the configuration they name and the other value blocks that outright.
const ADMITTING_POLICY: &str = "Warn";

/// The account id every ARN of this emulator carries.
fn account() -> &'static str {
    ::engine::emulator_account_id()
}

/// The ARN the account holds the role `role` under.
fn role_arn(role: &str) -> String {
    format!("arn:aws:iam::{}:role/{role}", account())
}

/// The principal ARN the session assumed from `role` speaks for — what a refusal must name.
fn session_arn(role: &str) -> String {
    format!("arn:aws:sts::{}:assumed-role/{role}/{SESSION}", account())
}

/// The ARN of the function `name`, as the model's `FunctionArn` pattern spells it — the
/// resource a call addressed at that function is judged against.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{}:function:{name}", account())
}

/// What a dispatched request answered.
struct Answer {
    status: u16,
    body: bytes::Bytes,
    error_type: Option<String>,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn error_code(&self) -> String {
        self.error_type
            .clone()
            .or_else(|| {
                self.json()
                    .get("__type")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| self.text())
    }

    /// The `Message` an error body carries — this service spells the member with a capital, as
    /// every error structure it models does.
    fn message(&self) -> String {
        let document = self.json();
        document
            .get("Message")
            .or_else(|| document.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// Lambda beside the IAM and STS services over ONE shared engine, which is what `core/router`
/// assembles.
fn router_with_identity() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// Move every stored function's activation instant into the past, so the next call sees the
/// `Pending` -> `Active` transition the clock owes. This is what waiting would do, without
/// waiting.
fn make_activation_due(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        function.active_due = past;
    }
}

/// One Lambda request signed with `access_key_id`.
async fn lambda(
    router: &MultiServiceRouter,
    access_key_id: &str,
    method: &str,
    uri: &str,
    body: &Value,
) -> Answer {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the test request body must serialize")
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/lambda/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        error_type: parts
            .headers
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        body,
    }
}

/// One `awsQuery` call to IAM or STS, signed with the account's own credential.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.amazonaws.com/"))
        .header("host", format!("{signing_name}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY_ID}/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Create the role `role` with a trust policy admitting the service principal `principal`,
/// through `iam:CreateRole`.
async fn given_service_role(router: &MultiServiceRouter, role: &str, principal: &str) {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, body) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", role.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole {role:?} must succeed, got {status} {body}"
    );
}

/// Put `document` on `role` as its inline policy through `iam:PutRolePolicy`, which REPLACES
/// the policy of that name — so the next request made by a session of that role is judged on
/// the document this call wrote.
async fn given_role_policy(
    router: &MultiServiceRouter,
    case: &str,
    role: &str,
    document: &Value,
) {
    let (status, body) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", role.to_string()),
            ("PolicyName", POLICY_NAME.to_string()),
            ("PolicyDocument", document.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy must succeed, got {status} {body}"
    );
}

/// Create the role `role` with a trust policy the account itself satisfies and assume it
/// through `sts:AssumeRole`: the answer is the access key id of the session, which is the
/// credential every row signs its own call with.
async fn given_assumed_role(router: &MultiServiceRouter, role: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{}:root", account()) },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, body) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", role.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole {role:?} must succeed, got {status} {body}"
    );
    let (status, body) = query(
        router,
        "sts",
        "2011-06-15",
        "AssumeRole",
        &[
            ("RoleArn", role_arn(role)),
            ("RoleSessionName", SESSION.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: AssumeRole on {role:?} must succeed, got {status} {body}"
    );
    xml_element(&body, "AccessKeyId")
        .unwrap_or_else(|| panic!("setup: AssumeRole must answer an AccessKeyId, got {body}"))
}

/// A signing profile version ARN satisfying the model's `Arn` pattern, in the form AWS Signer
/// vends one.
fn signing_profile(index: usize) -> String {
    format!(
        "arn:aws:signer:{REGION}:{}:/signing-profiles/Profile{index}/{index:08x}",
        account()
    )
}

/// `CreateCodeSigningConfig` with the account's own credential, answering the ARN of the
/// configuration it persisted.
async fn given_code_signing_config(router: &MultiServiceRouter, index: usize) -> String {
    let answer = lambda(
        router,
        ACCOUNT_KEY_ID,
        "POST",
        CODE_SIGNING_CONFIGS_URI,
        &json!({
            "AllowedPublishers": { "SigningProfileVersionArns": [signing_profile(index)] },
            "CodeSigningPolicies": { "UntrustedArtifactOnDeployment": ADMITTING_POLICY },
            "Description": format!("configuration {index}"),
        }),
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "setup: CreateCodeSigningConfig {index} must succeed, got {} {}",
        answer.status,
        answer.text()
    );
    answer.json()["CodeSigningConfig"]["CodeSigningConfigArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "setup: CreateCodeSigningConfig must answer an ARN, got {}",
                answer.text()
            )
        })
        .to_string()
}

/// The `CreateFunction` body for `name`, naming the code-signing configuration `config_arn`
/// when the row supplies one and carrying no such member at all when it does not.
fn create_body(name: &str, config_arn: Option<&str>) -> Value {
    let package: Vec<u8> = (0..64u16)
        .map(|byte| u8::try_from(byte % 251).unwrap_or(0))
        .collect();
    let mut body = json!({
        "FunctionName": name,
        "Role": role_arn(EXECUTION_ROLE),
        "Runtime": "python3.12",
        "Handler": "index.handler",
        "Code": { "ZipFile": base64::engine::general_purpose::STANDARD.encode(&package) },
    });
    if let Some(arn) = config_arn {
        body["CodeSigningConfigArn"] = json!(arn);
    }
    body
}

/// `GetFunction` with the account's own credential: the stored function, or `None` when this
/// account holds none under that name.
async fn stored_function(router: &MultiServiceRouter, name: &str) -> Option<Value> {
    let read = lambda(
        router,
        ACCOUNT_KEY_ID,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}"),
        &Value::Null,
    )
    .await;
    if read.status == 404 {
        return None;
    }
    assert_eq!(
        read.status,
        200,
        "GetFunction {name:?} as the account must be served, got {} {}",
        read.status,
        read.text()
    );
    Some(read.json())
}

/// `GetFunctionCodeSigningConfig` with the account's own credential: the ARN of the
/// configuration the function is associated with, or `None` when it carries none — the body of
/// an unassociated function names no ARN at all.
async fn stored_association(router: &MultiServiceRouter, name: &str) -> Option<String> {
    let read = lambda(
        router,
        ACCOUNT_KEY_ID,
        "GET",
        &format!("/2020-06-30/functions/{name}/code-signing-config"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "GetFunctionCodeSigningConfig {name:?} as the account must be served, got {} {}",
        read.status,
        read.text()
    );
    read.json()
        .get("CodeSigningConfigArn")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// An identity-based document allowing `action` on every resource, under the `Condition` block
/// `condition` (no block at all for `Value::Null`).
fn grant(action: &str, condition: Value) -> Value {
    let mut statement = json!({
        "Effect": "Allow",
        "Action": action,
        "Resource": "*",
    });
    if !condition.is_null() {
        statement["Condition"] = condition;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// The sentence IAM refuses a call no readable identity-based document allows with, as
/// `service_common::iam_authorization` words it: the principal that signed, the action, the
/// resource and IAM's default-deny clause.
fn default_deny_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// RULE: `CreateFunction` publishes the code-signing configuration its own member names — and
/// the ABSENCE of one where the optional member was omitted — as the
/// `lambda:CodeSigningConfigArn` request condition key, so a grant conditioned on that key
/// serves only the creates whose value satisfies the condition and refuses every other one
/// before anything is created.
#[tokio::test]
async fn a_create_is_decided_by_the_code_signing_configuration_it_names() {
    let (router, _engine) = router_with_identity();
    given_service_role(&router, EXECUTION_ROLE, LAMBDA_PRINCIPAL).await;
    let caller_key = given_assumed_role(&router, CALLER_ROLE).await;
    let approved = given_code_signing_config(&router, 1).await;
    let other = given_code_signing_config(&router, 2).await;
    // Both configurations live under the same ARN prefix, so a wildcard over it covers each of
    // them while the equality below covers only the approved one.
    let any_config = format!("arn:aws:lambda:{REGION}:{}:code-signing-config:*", account());
    let action = "lambda:CreateFunction";

    // (case, the Condition the grant is written under, the ARN the create names, served)
    let rows: Vec<(&str, Value, Option<String>, bool)> = vec![
        (
            "an equality over the approved configuration, named",
            json!({ "StringEquals": { CODE_SIGNING_CONFIG_ARN_KEY: approved.clone() } }),
            Some(approved.clone()),
            true,
        ),
        (
            "an equality over the approved configuration, another named",
            json!({ "StringEquals": { CODE_SIGNING_CONFIG_ARN_KEY: approved.clone() } }),
            Some(other.clone()),
            false,
        ),
        (
            "an equality over the approved configuration, none named",
            json!({ "StringEquals": { CODE_SIGNING_CONFIG_ARN_KEY: approved.clone() } }),
            None,
            false,
        ),
        (
            "the IfExists spelling of that equality, named",
            json!({ "StringEqualsIfExists": { CODE_SIGNING_CONFIG_ARN_KEY: approved.clone() } }),
            Some(approved.clone()),
            true,
        ),
        (
            "the IfExists spelling of that equality, another named",
            json!({ "StringEqualsIfExists": { CODE_SIGNING_CONFIG_ARN_KEY: approved.clone() } }),
            Some(other.clone()),
            false,
        ),
        (
            "the IfExists spelling of that equality, none named",
            json!({ "StringEqualsIfExists": { CODE_SIGNING_CONFIG_ARN_KEY: approved.clone() } }),
            None,
            true,
        ),
        (
            "a wildcard over every configuration of this account, another named",
            json!({ "StringLike": { CODE_SIGNING_CONFIG_ARN_KEY: any_config.clone() } }),
            Some(other.clone()),
            true,
        ),
        (
            "a wildcard over every configuration of this account, none named",
            json!({ "StringLike": { CODE_SIGNING_CONFIG_ARN_KEY: any_config.clone() } }),
            None,
            false,
        ),
        (
            "the existence operator asking for no configuration, none named",
            json!({ "Null": { CODE_SIGNING_CONFIG_ARN_KEY: "true" } }),
            None,
            true,
        ),
        (
            "the existence operator asking for no configuration, one named",
            json!({ "Null": { CODE_SIGNING_CONFIG_ARN_KEY: "true" } }),
            Some(approved.clone()),
            false,
        ),
        ("no condition at all, none named", Value::Null, None, true),
        (
            "no condition at all, one named",
            Value::Null,
            Some(approved.clone()),
            true,
        ),
    ];

    for (index, (case, condition, named, served)) in rows.iter().enumerate() {
        given_role_policy(
            &router,
            case,
            CALLER_ROLE,
            &grant(action, condition.clone()),
        )
        .await;
        let name = format!("created-{index:02}");
        let answered = lambda(
            &router,
            &caller_key,
            "POST",
            FUNCTIONS_URI,
            &create_body(&name, named.as_deref()),
        )
        .await;

        if *served {
            assert_eq!(
                answered.status,
                201,
                "case {case:?}: the grant covers this create, so it must be served, got {} {}",
                answered.status,
                answered.text()
            );
            let stored = stored_function(&router, &name).await.unwrap_or_else(|| {
                panic!("case {case:?}: a served create must leave the function readable")
            });
            assert_eq!(
                stored["Configuration"]["FunctionArn"].as_str(),
                Some(function_arn(&name).as_str()),
                "case {case:?}: expected the stored function {}, got {stored}",
                function_arn(&name)
            );
            assert_eq!(
                stored_association(&router, &name).await,
                named.clone(),
                "case {case:?}: the association must be the configuration the create named \
                 ({named:?})"
            );
            continue;
        }

        assert_eq!(
            answered.status, ACCESS_DENIED_STATUS,
            "case {case:?}: the grant does not cover this create, so it must be refused, got \
             {} {}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.error_code(),
            ACCESS_DENIED,
            "case {case:?}: expected {ACCESS_DENIED}, got {}",
            answered.text()
        );
        assert_eq!(
            answered.message(),
            default_deny_message(&session_arn(CALLER_ROLE), action, &function_arn(&name)),
            "case {case:?}: expected IAM's default-deny sentence, got {:?}",
            answered.message()
        );
        assert!(
            stored_function(&router, &name).await.is_none(),
            "case {case:?}: a refused create must leave nothing a later caller can read"
        );
    }
}

/// RULE: the same key is published by `PutFunctionCodeSigningConfig`, whose required member
/// names the configuration it attaches — so a grant conditioned on the key serves only the
/// calls that attach the approved configuration, and a refused call leaves the function's
/// association as it was.
#[tokio::test]
async fn an_association_is_decided_by_the_code_signing_configuration_it_attaches() {
    let (router, engine) = router_with_identity();
    given_service_role(&router, EXECUTION_ROLE, LAMBDA_PRINCIPAL).await;
    let caller_key = given_assumed_role(&router, CALLER_ROLE).await;
    let approved = given_code_signing_config(&router, 3).await;
    let other = given_code_signing_config(&router, 4).await;
    let action = "lambda:PutFunctionCodeSigningConfig";
    // The grant is narrowed to the approved configuration for every row: what differs is the
    // configuration the call attaches.
    given_role_policy(
        &router,
        "the approved configuration alone",
        CALLER_ROLE,
        &grant(
            action,
            json!({ "StringEquals": { CODE_SIGNING_CONFIG_ARN_KEY: approved.clone() } }),
        ),
    )
    .await;

    // (case, the configuration the put attaches, served)
    let rows: Vec<(&str, String, bool)> = vec![
        ("another configuration this account holds", other.clone(), false),
        ("the approved configuration", approved.clone(), true),
        // Asked again, so a served call is not a one-off and the refusal below is decided on
        // the value rather than on the order the rows ran in.
        ("another configuration, after the approved one", other.clone(), false),
    ];

    let name = "associated";
    let created = lambda(
        &router,
        ACCOUNT_KEY_ID,
        "POST",
        FUNCTIONS_URI,
        &create_body(name, None),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup: CreateFunction as the account must succeed, got {} {}",
        created.status,
        created.text()
    );
    // A configuration is attached to the function as a whole, which a function still coming up
    // is not ready to take: the transition the clock owes is made due rather than waited for.
    make_activation_due(&engine);
    let mut held: Option<String> = None;

    for (case, attached, served) in &rows {
        let answered = lambda(
            &router,
            &caller_key,
            "PUT",
            &format!("/2020-06-30/functions/{name}/code-signing-config"),
            &json!({ "CodeSigningConfigArn": attached }),
        )
        .await;

        if *served {
            assert_eq!(
                answered.status,
                200,
                "case {case:?}: the grant covers this call, so it must be served, got {} {}",
                answered.status,
                answered.text()
            );
            held = Some(attached.clone());
        } else {
            assert_eq!(
                answered.status, ACCESS_DENIED_STATUS,
                "case {case:?}: the grant does not cover this call, so it must be refused, got \
                 {} {}",
                answered.status,
                answered.text()
            );
            assert_eq!(
                answered.error_code(),
                ACCESS_DENIED,
                "case {case:?}: expected {ACCESS_DENIED}, got {}",
                answered.text()
            );
            assert_eq!(
                answered.message(),
                default_deny_message(&session_arn(CALLER_ROLE), action, &function_arn(name)),
                "case {case:?}: expected IAM's default-deny sentence, got {:?}",
                answered.message()
            );
        }

        assert_eq!(
            stored_association(&router, name).await,
            held,
            "case {case:?}: the stored association must be {held:?}"
        );
    }
}
