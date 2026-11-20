// SPDX-License-Identifier: CC0-1.0

//! How far an authorization refusal of the identity service goes, end to end over two
//! services and two wire protocols.
//!
//! RULE: every `AccessDenied` the identity service answers an authorization decision with
//! is ONE sentence — `User: ` + the principal that signed the request + ` is not authorized
//! to perform: ` + the action + ` on resource: ` + the resource the decision was made on +
//! ` ` + the clause saying why the decision went that way — CLOSED by the pointer at that
//! decision's own authorization details: `. Go to <the console address of those details> for
//! complete details, or call the GetRequestAuthorizationDetails API with the following
//! authorization id: <id>`, naming an id the service mints for that one refused request and
//! gives twice. The message ends there, and nothing selects the pointer away — not which
//! action the call asked for, not whether the request addresses one entity, a collection or
//! the account itself, not whether it reads or writes, not which document decided (an
//! absent grant, an explicit `Deny`), not the Region it was signed for, and not how often
//! the same call is made. A service that is NOT the identity service words its own refusal
//! with the sentence alone.
//!
//! It takes TWO services to state the rule, which is why the suite lives here: one principal
//! — created and granted through the identity service's own API — is refused by the identity
//! service over awsQuery and by the secret store over awsJson1_1, and the two refusals
//! differ in the action they name, the envelope their protocol carries them in, and whether
//! the identity service's authorization-details pointer closes them.
//!
//! The per-service halves live beside their services: the identity service's wording table
//! is its crate's `tests/all/access_denied_message_wording.rs`, and the envelope each
//! protocol gives a denial is `iam_secretsmanager_denial_envelope_by_protocol.rs` beside
//! this file. This file is where the refusal's TAIL is pinned: a reader looking for an
//! authorization-details pointer in a refusal of the identity service finds here that it is
//! there, and that the id it names is a fresh one per refused request.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element};

/// The account every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// The Region the fixture calls are signed for, and the one the rows sign for unless they
/// name another.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which is the account's own administrative
/// credential: what the estate below is created with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The identity service's signing name.
const IDENTITY_SERVICE: &str = "iam";

/// The secret store's signing name: the second service the one principal is refused by.
const SECRET_STORE: &str = "secretsmanager";

/// The two services over ONE engine, as `core/router/src/lib.rs` mounts them: the user, its
/// documents and its access key are the ones the identity service stores, and the secret
/// store decides its own calls against those same documents.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signatures below are not real ones; what names the caller is the access key id in
    // the credential scope, which is what this suite varies.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and `region`,
/// and whose credential is `access_key_id` — the key a service reads the caller's identity
/// from.
fn authorization(access_key_id: &str, signing_name: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Post one awsQuery form body to the identity service, presenting `access_key_id`, signed
/// for `region`.
async fn iam_in_region(
    router: &MultiServiceRouter,
    access_key_id: &str,
    region: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Version=2010-05-08&Action={action}");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{IDENTITY_SERVICE}.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            authorization(access_key_id, IDENTITY_SERVICE, region),
        )
        .body(Bytes::from(body))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// [`iam_in_region`] in the harness Region.
async fn iam(
    router: &MultiServiceRouter,
    access_key_id: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> (u16, String) {
    iam_in_region(router, access_key_id, REGION, action, parameters).await
}

/// One identity-service call that is part of the fixture, which must be served.
async fn ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    parameters: &[(&str, &str)],
) -> String {
    let (status, body) = iam(router, AMBIENT_KEY, action, parameters).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture {action} must be served, got {status}: {body}"
    );
    body
}

/// Mint one long-term access key for `user_name` and answer its id.
///
/// This one fixture's answer CARRIES CREDENTIAL MATERIAL — a minted secret access key — so
/// nothing here quotes the body, not even on a failure.
async fn mint_access_key(router: &MultiServiceRouter, case: &str, user_name: &str) -> String {
    let (status, body) = iam(router, AMBIENT_KEY, "CreateAccessKey", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture CreateAccessKey must be served, got {status}"
    );
    opt_element(&body, "AccessKeyId").unwrap_or_else(|| {
        panic!("case {case:?}: the minted-credential answer carries no <AccessKeyId> element")
    })
}

/// Post one awsJson1_1 call to the secret store, presenting `access_key_id`.
async fn secrets(
    router: &MultiServiceRouter,
    access_key_id: &str,
    operation: &str,
    body: String,
) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{SECRET_STORE}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{SECRET_STORE}.{operation}"))
        .header(
            "authorization",
            authorization(access_key_id, SECRET_STORE, REGION),
        )
        .body(Bytes::from(body))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The `Message` of an awsQuery refusal.
fn message_of(body: &str) -> String {
    elements(body, "Message")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no <Message> in {body}"))
}

/// The awsQuery error code of a refusal.
fn error_code(body: &str) -> String {
    elements(body, "Code")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no <Code> in {body}"))
}

/// One identity-based statement over one action and one resource.
fn statement(effect: &str, action: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"{effect}","Action":"{action}","Resource":"{resource}"}}]}}"#
    )
}

/// A grant for every action over every resource, with an explicit `Deny` of `action`: the
/// document a row about the explicit deny is refused under.
fn allow_everything_but(action: &str) -> String {
    format!(
        concat!(
            r#"{{"Version":"2012-10-17","Statement":["#,
            r#"{{"Effect":"Allow","Action":"*","Resource":"*"}},"#,
            r#"{{"Effect":"Deny","Action":"{}","Resource":"*"}}]}}"#
        ),
        action
    )
}

/// The ARN of an IAM user of this account at the default path.
fn user_arn(user_name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:user/{user_name}")
}

/// Assert that `message` is exactly `sentence`: the refusal names the decision and ends at
/// the clause saying why.
///
/// The ONE reader of such a refusal's wording in this test crate: a suite about something
/// else — which protocol carried the denial, which document narrowed it — states the
/// sentence it set up and calls this, rather than writing the rule out again. A refusal of
/// the IDENTITY service carries one clause more ([`assert_identity_refusal`]).
pub(crate) fn assert_refusal_sentence(case: &str, message: &str, sentence: &str) {
    assert_eq!(
        message, sentence,
        "case {case:?}: expected the refusal to be worded {sentence:?} and to end there, got \
         {message:?}"
    );
}

/// What a refusal of the IDENTITY service closes with, before the authorization id it names.
///
/// Written out here rather than taken from the service under test, so the suites assert
/// AWS's text; the identity service is global, so the console Region is the partition's own
/// whichever Region the refused request addressed.
const POINTER_PREFIX: &str =
    ". Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1\
     #/authorization-details/";

/// What separates that console address from the authorization id given a second time.
const POINTER_API: &str =
    " for complete details, or call the GetRequestAuthorizationDetails API with the \
     following authorization id: ";

/// `message` with the identity service's authorization-details pointer taken off: the
/// sentence naming the decision, which is the part two refusals of the SAME call share — the
/// pointer names an id minted per refused request, so it is what they differ in by design.
pub(crate) fn sentence_of(message: &str) -> String {
    message
        .split_once(POINTER_PREFIX)
        .map_or_else(|| message.to_string(), |(sentence, _)| sentence.to_string())
}

/// Assert that `message` is the refusal the IDENTITY service answers: `sentence`, closed by
/// the pointer at that decision's authorization details — and answer the authorization id
/// the pointer names.
///
/// The id is minted per refused request, so it is read out of the message rather than
/// predicted; it is asserted to be spelled in the alphabet the authorization-details read
/// admits and to be the SAME id in the console address and in the API pointer. Every other
/// character of the message is asserted.
///
/// The ONE reader of an identity-service refusal in this test crate, for the same reason
/// [`assert_refusal_sentence`] is the one reader of a refusal without that clause.
pub(crate) fn assert_identity_refusal(case: &str, message: &str, sentence: &str) -> String {
    let (before, id) = message.split_once(POINTER_API).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the refusal to point at its authorization details, got \
             {message:?}"
        )
    });
    assert!(
        !id.is_empty()
            && id
                .chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit()),
        "case {case:?}: expected the authorization id to be spelled in lowercase \
         alphanumerics, got {id:?} in {message:?}"
    );
    assert_eq!(
        before,
        format!("{sentence}{POINTER_PREFIX}{id}"),
        "case {case:?}: expected the refusal to be worded {sentence:?} and to close with the \
         pointer at its authorization details, got {message:?}"
    );
    id.to_string()
}

/// One refused call: what the caller holds, the awsQuery call it makes, and the sentence
/// that refusal is worded in.
struct Row {
    case: &'static str,
    /// The document put in force on the caller, or `None` for a caller holding none.
    document: Option<String>,
    /// The awsQuery `Action` and its form parameters.
    action: &'static str,
    params: Vec<(&'static str, String)>,
    /// The action a policy statement names the call by.
    iam_action: &'static str,
    /// The resource as the refusal NAMES it.
    named_as: String,
    /// The clause closing the sentence.
    reason: String,
}

/// The reason clause for a call no statement in force allows.
fn no_allow(iam_action: &str) -> String {
    format!("because no identity-based policy allows the {iam_action} action")
}

/// The reason clause for a call a statement about it explicitly denies.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// The rows: one refused call each, differing along what the rule generalises over — which
/// action is asked for, whether the request addresses a collection, a path-scoped
/// collection, one entity by name or the account itself, whether the call reads or writes,
/// and which document decided the refusal.
fn rows() -> Vec<Row> {
    vec![
        // A collection read, no document in force at all: the account's users at the root
        // path, which is the call the recorded cross-protocol check makes.
        Row {
            case: "a withheld user listing",
            document: None,
            action: "ListUsers",
            params: Vec::new(),
            iam_action: "iam:ListUsers",
            named_as: format!("arn:aws:iam::{ACCOUNT}:user/"),
            reason: no_allow("iam:ListUsers"),
        },
        // A path-scoped collection: the resource the refusal names is the prefix the
        // request addressed.
        Row {
            case: "a withheld path-scoped group listing",
            document: None,
            action: "ListGroups",
            params: vec![("PathPrefix", "/eng/".to_string())],
            iam_action: "iam:ListGroups",
            named_as: format!("arn:aws:iam::{ACCOUNT}:group/eng/"),
            reason: no_allow("iam:ListGroups"),
        },
        // A request that addresses ONE entity by name, the caller holding a grant for
        // another action: a document was read and does not cover the call.
        Row {
            case: "a withheld read of one group",
            document: Some(statement("Allow", "iam:ListRoles", "*")),
            action: "GetGroup",
            params: vec![("GroupName", "payroll".to_string())],
            iam_action: "iam:GetGroup",
            named_as: "group payroll".to_string(),
            reason: no_allow("iam:GetGroup"),
        },
        // A request that addresses the account's whole configuration: no ARN in the
        // message at all.
        Row {
            case: "a withheld account-wide read",
            document: None,
            action: "GetAccountAuthorizationDetails",
            params: Vec::new(),
            iam_action: "iam:GetAccountAuthorizationDetails",
            named_as: "*".to_string(),
            reason: no_allow("iam:GetAccountAuthorizationDetails"),
        },
        // A WRITE on one entity: what the call does moves nothing.
        Row {
            case: "a withheld tag write",
            document: None,
            action: "TagRole",
            params: vec![
                ("RoleName", "payroll".to_string()),
                ("Tags.member.1.Key", "env".to_string()),
                ("Tags.member.1.Value", "prod".to_string()),
            ],
            iam_action: "iam:TagRole",
            named_as: "role payroll".to_string(),
            reason: no_allow("iam:TagRole"),
        },
        // The OTHER decision: a statement about the request says `Deny` over a grant for
        // everything, so the reason clause is the explicit one.
        Row {
            case: "a user listing denied by a statement about it",
            document: Some(allow_everything_but("iam:ListUsers")),
            action: "ListUsers",
            params: Vec::new(),
            iam_action: "iam:ListUsers",
            named_as: format!("arn:aws:iam::{ACCOUNT}:user/"),
            reason: EXPLICIT_DENY.to_string(),
        },
        // The same `Deny`, on a write of one entity.
        Row {
            case: "a tag write denied by a statement about it",
            document: Some(allow_everything_but("iam:TagRole")),
            action: "TagRole",
            params: vec![
                ("RoleName", "payroll".to_string()),
                ("Tags.member.1.Key", "env".to_string()),
                ("Tags.member.1.Value", "prod".to_string()),
            ],
            iam_action: "iam:TagRole",
            named_as: "role payroll".to_string(),
            reason: EXPLICIT_DENY.to_string(),
        },
    ]
}

/// Create the caller a row is about and answer its access key id.
async fn caller(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
    document: Option<&str>,
) -> String {
    ok(router, case, "CreateUser", &[("UserName", user_name)]).await;
    if let Some(document) = document {
        ok(
            router,
            case,
            "PutUserPolicy",
            &[
                ("UserName", user_name),
                ("PolicyName", "in-force"),
                ("PolicyDocument", document),
            ],
        )
        .await;
    }
    mint_access_key(router, case, user_name).await
}

/// RULE: every authorization refusal the identity service answers names the decision, says
/// why it went that way and then points at that decision's authorization details — whichever
/// action was asked for, on whichever resource form, reading or writing, and whichever
/// document decided — and every refusal names an authorization id of its own.
#[tokio::test]
async fn every_identity_refusal_names_the_decision_the_reason_and_its_details() {
    let router = build_router();
    // The role the tag-write rows address: a fixture, never the caller, so the refusals
    // are about the caller's own permissions.
    let trust = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":"sts:AssumeRole"}}]}}"#
    );
    ok(
        &router,
        "the fixture role",
        "CreateRole",
        &[("RoleName", "payroll"), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    ok(
        &router,
        "the fixture group",
        "CreateGroup",
        &[("GroupName", "payroll")],
    )
    .await;

    let mut minted: Vec<String> = Vec::new();
    for (row, case) in rows().into_iter().enumerate() {
        let user_name = format!("worded{row}");
        let access_key_id = caller(
            &router,
            case.case,
            &user_name,
            case.document.as_deref(),
        )
        .await;
        let params: Vec<(&str, &str)> = case
            .params
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();

        let (status, body) = iam(&router, &access_key_id, case.action, &params).await;
        assert_eq!(
            status, 403,
            "case {:?}: expected HTTP 403, got {status}: {body}",
            case.case
        );
        let code = error_code(&body);
        assert_eq!(
            code, "AccessDenied",
            "case {:?}: expected the error code AccessDenied, got {code:?}: {body}",
            case.case
        );

        let message = message_of(&body);
        let expected = format!(
            "User: {} is not authorized to perform: {} on resource: {} {}",
            user_arn(&user_name),
            case.iam_action,
            case.named_as,
            case.reason,
        );
        minted.push(assert_identity_refusal(case.case, &message, &expected));
    }

    let distinct: std::collections::BTreeSet<&String> = minted.iter().collect();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "expected each refusal to point at its own authorization details, got {minted:?}"
    );
}

/// RULE: every part of the sentence is a fact of the request and of the documents in force,
/// so the SAME refused call made again — and made in another Region, since the identity
/// service is global — answers the identical sentence and pointer clause, with a FRESH
/// authorization id each time: the id names the details of one refused request.
#[tokio::test]
async fn the_same_refused_call_is_worded_identically_in_every_region() {
    let router = build_router();
    let case = "the repeated listing";
    let access_key_id = caller(&router, case, "repeated", None).await;

    let expected = format!(
        "User: {} is not authorized to perform: iam:ListUsers on resource: \
         arn:aws:iam::{ACCOUNT}:user/ {}",
        user_arn("repeated"),
        no_allow("iam:ListUsers"),
    );
    let mut minted: Vec<String> = Vec::new();
    for region in [REGION, REGION, "eu-west-1", "ap-southeast-2"] {
        let (status, body) =
            iam_in_region(&router, &access_key_id, region, "ListUsers", &[]).await;
        assert_eq!(
            status, 403,
            "case {case:?} in {region}: expected HTTP 403, got {status}: {body}"
        );
        minted.push(assert_identity_refusal(
            &format!("{case} in {region}"),
            &message_of(&body),
            &expected,
        ));
    }
    let distinct: std::collections::BTreeSet<&String> = minted.iter().collect();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "expected every refusal of the same call to name its own authorization id, got \
         {minted:?}"
    );
}

/// RULE: the sentence naming the decision is the refusal whichever service answers it, and
/// the identity service closes its own with the pointer at that decision's authorization
/// details — so one principal refused by the identity service and by the secret store, over
/// two protocols and against the very same documents, is answered two messages differing in
/// the action they name and in that one clause.
#[tokio::test]
async fn both_services_word_the_same_principals_refusal_the_same_way() {
    let router = build_router();
    let case = "one principal refused by both services";
    let access_key_id = caller(&router, case, "both", None).await;

    let (status, body) = iam(&router, &access_key_id, "ListUsers", &[]).await;
    assert_eq!(
        status, 403,
        "case {case:?}: expected the identity service to refuse, got {status}: {body}"
    );
    let expected = format!(
        "User: {} is not authorized to perform: iam:ListUsers on resource: \
         arn:aws:iam::{ACCOUNT}:user/ {}",
        user_arn("both"),
        no_allow("iam:ListUsers"),
    );
    assert_identity_refusal(case, &message_of(&body), &expected);

    // The secret store's refusal of the same principal, over its own protocol: the same
    // sentence shape, and nothing after the reason — the authorization-details pointer is
    // the identity service's own.
    let (status, body) = secrets(
        &router,
        &access_key_id,
        "ListSecrets",
        json!({ "MaxResults": 1 }).to_string(),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the secret store to refuse, got {status}: {body}"
    );
    let parsed: Value = serde_json::from_str(&body)
        .unwrap_or_else(|_| panic!("case {case:?}: the refusal is not JSON: {body}"));
    let store_message = parsed
        .get("message")
        .or_else(|| parsed.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no message in {body}"))
        .to_string();
    let expected = format!(
        "User: {} is not authorized to perform: secretsmanager:ListSecrets {}",
        user_arn("both"),
        no_allow("secretsmanager:ListSecrets"),
    );
    assert_refusal_sentence(case, &store_message, &expected);
}
