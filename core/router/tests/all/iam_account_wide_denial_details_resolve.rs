// SPDX-License-Identifier: CC0-1.0

//! An IAM refusal decided on the ACCOUNT-WIDE resource points at authorization details that
//! resolve, exactly as a refusal decided on one entity or one collection does.
//!
//! The rule under test: every `AccessDenied` the identity service answers an authorization
//! decision with is ONE sentence — `User: ` + the principal that signed + ` is not authorized
//! to perform: ` + the action + ` on resource: ` + the resource the decision was made on +
//! ` ` + the clause saying why — CLOSED by the pointer at that decision's own authorization
//! details, `. Go to <console address> for complete details, or call the
//! GetRequestAuthorizationDetails API with the following authorization id: <id>`, naming one
//! id minted for that one refused request and given twice; and that id names a record the
//! service owning authorization details serves. Nothing about the operation selects the
//! pointer away — in particular not the RESOURCE FORM the decision was made on: an operation
//! that reports or changes the account's own configuration rather than an entity the account
//! holds is decided on the account-wide resource `*`, so its refusal carries no ARN at all,
//! and it carries the pointer and resolves just the same.
//!
//! `iam_denial_authorization_details_resolve.rs` beside this file pins the resolution for the
//! resource forms that name an entity or a collection; this suite is the account-wide half of
//! the same rule, over the family of operations decided on `*`. It takes TWO services to
//! state, which is why it lives here: the denial is the identity service's
//! (`service-com-amazonaws-iam-awsidentitymanagementv20100508`, awsQuery) while the record it
//! points at belongs to IAM Toolbox
//! (`service-com-amazonaws-iamtoolbox-authrequestservice`, restJson1, `GET
//! /authorization-details/{authorizationId}`), and the two meet only over the one engine the
//! assembled emulator mounts them on.
//!
//! The rows are a TABLE spanning what the rule generalises over: which member of the
//! account-wide family is refused, whether it reports or changes the account's configuration,
//! whether the call carries parameters of its own, and which of the two decisions refused it
//! (nothing allowing the call, or a statement about it saying `Deny`).

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every call below is signed for. The identity service is global, so this is the
/// Region its endpoint lives in.
const REGION: &str = "us-east-1";

/// The access-key id standing for the account's own published credential: the identity
/// service holds no user record for it, so it resolves to the account's ambient identity,
/// which carries no IAM policies — the position account root credentials are in, and what the
/// fixture calls below are signed as.
const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// The signing name the identity service and IAM Toolbox share.
const SIGNING_NAME: &str = "iam";

/// The identity service's API version on the awsQuery wire.
const VERSION: &str = "2010-05-08";

/// The resource an account-wide call is decided against: the Service Authorization Reference
/// lists no resource type for an IAM action that acts on the account itself, so the decision
/// is made against every resource and the refusal names exactly that.
const ACCOUNT_WIDE: &str = "*";

/// What the pointer clause of a refusal names before the authorization id, and what it names
/// after it. Written out here rather than taken from the service under test, so this suite
/// asserts AWS's text; the identity service is global, so the console Region is the
/// partition's own whichever Region the refused request addressed.
const POINTER_PREFIX: &str =
    ". Go to https://us-east-1.console.aws.amazon.com/iam/home?region=us-east-1\
     #/authorization-details/";
const POINTER_API: &str =
    " for complete details, or call the GetRequestAuthorizationDetails API with the \
     following authorization id: ";

/// The `EvaluatedEffect` the record of a call nothing allowed carries.
const IMPLICIT_DENY: &str = "IMPLICIT_DENY";

/// The `EvaluatedEffect` the record of a call a statement about it explicitly denied carries.
const EXPLICIT_DENY: &str = "EXPLICIT_DENY";

/// A SigV4 `Authorization` header whose credential scope names the identity service's signing
/// name. The signature itself is never checked — the router here runs with request
/// authentication off — and the access key id is what resolves the caller's identity.
fn authorization(access_key_id: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{SIGNING_NAME}/\
         aws4_request, SignedHeaders=host, Signature=0"
    )
}

/// The emulator these rows drive: the identity service beside the service that owns the
/// authorization details of a refused request, on ONE engine, as `core/router/src/lib.rs`
/// mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iamtoolbox_authrequestservice::EmulatedService::new(engine),
    );
    router
}

/// One awsQuery call to the identity service, signed with `access_key_id`.
async fn iam(
    router: &MultiServiceRouter,
    access_key_id: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(access_key_id))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    (status, text)
}

/// One awsQuery call signed as the account itself, which must succeed: the fixture legs.
async fn fixture(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = iam(router, ACCOUNT_KEY, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture {action} must succeed, got {status}: {body}"
    );
    body
}

/// `GET /authorization-details/{id}`: the read IAM Toolbox serves, signed as the account.
async fn authorization_details(router: &MultiServiceRouter, authorization_id: &str) -> (u16, Value) {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!(
            "https://iam.amazonaws.com/authorization-details/{authorization_id}"
        ))
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization(ACCOUNT_KEY))
        .body(Bytes::new())
        .expect("a well-formed GetRequestAuthorizationDetails request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.into_body();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// An IAM user carrying `policies` as its inline policies, with a long-term access key:
/// answers that key id.
///
/// The answer of the key mint CARRIES CREDENTIAL MATERIAL, so nothing here quotes its body.
async fn principal(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
    policies: &[(String, String)],
) -> String {
    fixture(router, case, "CreateUser", &[("UserName", user_name)]).await;
    for (policy_name, document) in policies {
        fixture(
            router,
            case,
            "PutUserPolicy",
            &[
                ("UserName", user_name),
                ("PolicyName", policy_name),
                ("PolicyDocument", document),
            ],
        )
        .await;
    }
    let (status, minted) = iam(
        router,
        ACCOUNT_KEY,
        "CreateAccessKey",
        &[("UserName", user_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture CreateAccessKey must succeed, got {status}"
    );
    element(&minted, "AccessKeyId")
}

/// A one-statement document over one action and every resource.
fn statement(effect: &str, action: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"{effect}","Action":"{action}","Resource":"*"}}]}}"#
    )
}

/// The authorization id the pointer clause of `message` names, asserted to be there, to be
/// spelled in the alphabet the authorization-details read admits, and to be the SAME id in the
/// console address and in the API pointer.
fn pointed_at(case: &str, message: &str) -> String {
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
    assert!(
        before.ends_with(&format!("{POINTER_PREFIX}{id}")),
        "case {case:?}: expected the console address to name the same authorization id {id:?}, \
         got {message:?}"
    );
    id.to_string()
}

/// One member of the account-wide family: the awsQuery `Action`, the IAM action its decision
/// is made on, and the form parameters the call carries.
struct Member {
    action: &'static str,
    iam_action: &'static str,
    params: Vec<(&'static str, String)>,
}

/// The family. Every one of these reports or changes the ACCOUNT's own configuration rather
/// than an entity the account holds, so every one is decided on [`ACCOUNT_WIDE`] — the rows
/// span both directions (reports and changes), both input forms (no parameter of its own and
/// one it carries) and five different parts of that configuration, so no single operation is
/// the specification.
fn family() -> Vec<Member> {
    vec![
        // The feature whose read the recorded check made: an account-level record, reported
        // through an operation that takes no input at all.
        Member {
            action: "GetOutboundWebIdentityFederationInfo",
            iam_action: "iam:GetOutboundWebIdentityFederationInfo",
            params: Vec::new(),
        },
        // The two writes of that same record.
        Member {
            action: "EnableOutboundWebIdentityFederation",
            iam_action: "iam:EnableOutboundWebIdentityFederation",
            params: Vec::new(),
        },
        Member {
            action: "DisableOutboundWebIdentityFederation",
            iam_action: "iam:DisableOutboundWebIdentityFederation",
            params: Vec::new(),
        },
        // The account's whole authorization estate, and its summary: account-wide reports
        // whose refusals a client meets while diagnosing permissions.
        Member {
            action: "GetAccountAuthorizationDetails",
            iam_action: "iam:GetAccountAuthorizationDetails",
            params: Vec::new(),
        },
        Member {
            action: "GetAccountSummary",
            iam_action: "iam:GetAccountSummary",
            params: Vec::new(),
        },
        // The account's alias: a report and a change of one key, the change carrying a
        // parameter of its own so the row is not only about parameterless operations.
        Member {
            action: "ListAccountAliases",
            iam_action: "iam:ListAccountAliases",
            params: Vec::new(),
        },
        Member {
            action: "CreateAccountAlias",
            iam_action: "iam:CreateAccountAlias",
            params: vec![("AccountAlias", "payroll-sandbox".to_string())],
        },
        // The account's password policy: a change carrying a numeric parameter.
        Member {
            action: "UpdateAccountPasswordPolicy",
            iam_action: "iam:UpdateAccountPasswordPolicy",
            params: vec![("MinimumPasswordLength", "14".to_string())],
        },
    ]
}

/// Which decision refused the call, and what the sentence's closing clause and the record's
/// effect are for it.
#[derive(Debug, Clone, Copy)]
struct Decision {
    case: &'static str,
    /// Whether the caller holds a grant for everything that the `Deny` is layered on.
    explicit: bool,
    effect: &'static str,
}

/// The two decisions a refusal can be reached by, so the rule is asserted over both and not
/// only over the one the recorded check made.
const DECISIONS: &[Decision] = &[
    Decision {
        case: "nothing allows the call",
        explicit: false,
        effect: IMPLICIT_DENY,
    },
    Decision {
        case: "a statement about the call says Deny",
        explicit: true,
        effect: EXPLICIT_DENY,
    },
];

/// The documents the caller holds for `decision`, relative to the action its row calls.
fn documents(decision: Decision, iam_action: &str) -> Vec<(String, String)> {
    if decision.explicit {
        vec![
            (
                "allow-everything".to_string(),
                statement("Allow", "iam:*"),
            ),
            (
                "deny-the-action".to_string(),
                statement("Deny", iam_action),
            ),
        ]
    } else {
        // A document IS in force and it names another action, so the refusal is the absence
        // of a grant for THIS action rather than the absence of any document at all.
        vec![(
            "allow-another-action".to_string(),
            statement("Allow", "iam:ListUsers"),
        )]
    }
}

/// The clause closing the sentence of a refusal reached by `decision`.
fn reason(decision: Decision, iam_action: &str) -> String {
    if decision.explicit {
        "with an explicit deny in an identity-based policy".to_string()
    } else {
        format!("because no identity-based policy allows the {iam_action} action")
    }
}

/// RULE: a refusal of a call decided on the account-wide resource is the sentence naming the
/// decision — with `*` as the resource, so no ARN appears in it — closed by the pointer at
/// that decision's authorization details, and the id that pointer names resolves in the
/// service owning those details to a record of THIS request: the action, the account-wide
/// resource and the effect that decided it.
#[tokio::test]
async fn an_account_wide_denial_points_at_a_record_that_resolves() {
    let router = build_router();
    let account = ::engine::emulator_account_id();

    let mut minted: Vec<String> = Vec::new();
    for (index, member) in family().into_iter().enumerate() {
        for (attempt, decision) in DECISIONS.iter().enumerate() {
            let case = format!("{} / {}", member.action, decision.case);
            let user_name = format!("accountwide{index}{attempt}");
            let held = documents(*decision, member.iam_action);
            let access_key_id = principal(&router, &case, &user_name, &held).await;
            let params: Vec<(&str, &str)> = member
                .params
                .iter()
                .map(|(name, value)| (*name, value.as_str()))
                .collect();

            let (status, body) = iam(&router, &access_key_id, member.action, &params).await;
            assert_eq!(
                status, 403,
                "case {case:?}: expected the refused call to answer HTTP 403, got {status}: \
                 {body}"
            );
            assert_eq!(
                element(&body, "Code"),
                "AccessDenied",
                "case {case:?}: expected the error code AccessDenied, got {:?}: {body}",
                element(&body, "Code")
            );

            // The whole message: the sentence this decision words, then the pointer. The
            // authorization id is minted per refused request, so it is read out of the
            // message rather than predicted; every other character is asserted.
            let message = element(&body, "Message");
            let authorization_id = pointed_at(&case, &message);
            let expected = format!(
                "User: arn:aws:iam::{account}:user/{user_name} is not authorized to perform: \
                 {} on resource: {ACCOUNT_WIDE} {}{POINTER_PREFIX}{authorization_id}\
                 {POINTER_API}{authorization_id}",
                member.iam_action,
                reason(*decision, member.iam_action),
            );
            assert_eq!(
                message, expected,
                "case {case:?}: expected the refusal to be worded {expected:?}, got {message:?}"
            );
            minted.push(authorization_id.clone());

            // The id resolves — in the service that owns the record, over its own wire.
            let (detail_status, detail) = authorization_details(&router, &authorization_id).await;
            assert_eq!(
                detail_status, 200,
                "case {case:?}: expected the authorization id {authorization_id:?} to resolve, \
                 got {detail_status}: {detail:?}"
            );

            // The record is about THIS request: one evaluation, of the action and the
            // account-wide resource the decision was made on, with the effect that decided it.
            let evaluations = detail["evaluations"].as_array().cloned().unwrap_or_default();
            assert_eq!(
                evaluations.len(),
                1,
                "case {case:?}: expected one evaluation, got {detail:?}"
            );
            let evaluation = &evaluations[0];
            assert_eq!(
                evaluation["action"].as_str(),
                Some(member.iam_action),
                "case {case:?}: expected the evaluation to name the action decided, got \
                 {evaluation:?}"
            );
            assert_eq!(
                evaluation["resource"].as_str(),
                Some(ACCOUNT_WIDE),
                "case {case:?}: expected the evaluation to name the account-wide resource, got \
                 {evaluation:?}"
            );
            assert_eq!(
                evaluation["evaluatedEffect"].as_str(),
                Some(decision.effect),
                "case {case:?}: expected the effect {:?}, got {evaluation:?}",
                decision.effect
            );

            // The context the decision was made in, which names the principal that signed.
            assert_eq!(
                detail["requestContext"]["aws:username"].as_str(),
                Some(user_name.as_str()),
                "case {case:?}: expected the request context to name the signing user, got \
                 {detail:?}"
            );

            // The policies that were evaluated: one entry per document in force on the caller.
            let policies = detail["policies"].as_array().cloned().unwrap_or_default();
            assert_eq!(
                policies.len(),
                held.len(),
                "case {case:?}: expected one entry per document in force, got {policies:?}"
            );
            for (policy_name, _) in &held {
                assert!(
                    policies.iter().any(|policy| {
                        policy["type"].as_str() == Some("IDENTITY_BASED_POLICY")
                            && policy["inline"].as_bool() == Some(true)
                            && policy["uri"]
                                .as_str()
                                .is_some_and(|uri| uri.ends_with(policy_name.as_str()))
                    }),
                    "case {case:?}: expected the evaluated policies to carry {policy_name:?}, \
                     got {policies:?}"
                );
            }
        }
    }

    // Every refusal is pointed at a record of its own: the id names the details of ONE
    // refused request, so no two of these share one.
    let distinct: BTreeSet<&String> = minted.iter().collect();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "expected every refusal to mint its own authorization id, got {minted:?}"
    );
}

/// RULE: the pointer is a property of the DECISION and not of the call, so repeating one
/// refused account-wide call answers the identical sentence with a fresh authorization id
/// each time, and each of those ids resolves to its own record of that same decision.
#[tokio::test]
async fn repeating_one_account_wide_refusal_mints_a_record_each_time() {
    let router = build_router();
    let account = ::engine::emulator_account_id();
    let case = "the repeated federation read";
    // The operation the recorded check made, refused by the absence of a grant for it.
    let iam_action = "iam:GetOutboundWebIdentityFederationInfo";
    let user_name = "repeatedaccountwide";
    let access_key_id = principal(&router, case, user_name, &[]).await;

    let sentence = format!(
        "User: arn:aws:iam::{account}:user/{user_name} is not authorized to perform: \
         {iam_action} on resource: {ACCOUNT_WIDE} because no identity-based policy allows the \
         {iam_action} action"
    );
    let mut minted: Vec<String> = Vec::new();
    for attempt in 0..3 {
        let row = format!("{case} attempt {attempt}");
        let (status, body) = iam(
            &router,
            &access_key_id,
            "GetOutboundWebIdentityFederationInfo",
            &[],
        )
        .await;
        assert_eq!(
            status, 403,
            "case {row:?}: expected HTTP 403, got {status}: {body}"
        );
        let message = element(&body, "Message");
        let authorization_id = pointed_at(&row, &message);
        assert_eq!(
            message,
            format!("{sentence}{POINTER_PREFIX}{authorization_id}{POINTER_API}{authorization_id}"),
            "case {row:?}: expected the sentence {sentence:?} closed by the pointer, got \
             {message:?}"
        );
        let (detail_status, detail) = authorization_details(&router, &authorization_id).await;
        assert_eq!(
            detail_status, 200,
            "case {row:?}: expected the id {authorization_id:?} to resolve, got \
             {detail_status}: {detail:?}"
        );
        minted.push(authorization_id);
    }
    let distinct: BTreeSet<&String> = minted.iter().collect();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "expected each repetition to name its own authorization id, got {minted:?}"
    );
}
