// SPDX-License-Identifier: CC0-1.0

//! WHO may call an operation of this control plane whose RESOURCE ARN this service cannot
//! name from the request line, driven end to end over the wire path (a `restJson1` request
//! through the multi-service router into the mounted service), so what is asserted is what a
//! client holding those credentials sees.
//!
//! THE RULE UNDER TEST: every call to this endpoint is decided against the calling
//! principal's identity-based IAM policies under the action
//! `bedrock-agentcore:<OperationName>` before anything is read or written — no family of it
//! is served unjudged. Where the resource the request addresses has no single ARN this
//! service can name (a family addressed under two ARN types, one whose resources hang off a
//! parent the route does not name, one this emulator builds no ARN type for), the question
//! asked is the weaker one IAM states for a decision taken without the resource ARN: a
//! principal whose readable documents hold no `Allow` of the action AT ALL is refused HTTP
//! 403 `AccessDeniedException` naming the principal, the action and which of IAM's rules
//! withheld the grant — with no resource clause, since the decision was not taken on one —
//! and the call performs nothing, while an `Allow` of the action reaches the operation
//! whatever resources that statement is scoped to. An explicit `Deny` of the action refuses
//! the call whatever else the principal carries.
//!
//! This is the complement of `tests/all/collection_listing_authorization.rs`, which pins the
//! same decision where the resource IS named (a family's collection ARN), and whose harness
//! this suite drives rather than a second copy of it: the role, its inline policy and the
//! assumed-role session live in the IAM and STS stores of the SAME shared engine — the stores
//! those services' own handlers write — so the documents under test are the ones
//! `iam:PutRolePolicy` stores, and the principal a refusal blames is the one
//! `sts:GetCallerIdentity` answers for the session the request signed with.
//!
//! The tables span what the rule generalises over: the FAMILY called (browsers, code
//! interpreters, evaluators and four planes of the identity family), whether the route names
//! one MEMBER or the whole family, the method it is bound to, the document in force (none,
//! another action namespace, another operation of this namespace, this action exactly, the
//! whole namespace, this action scoped to one resource ARN, an explicit deny), and the kind
//! of principal (an assumed-role session, the account's own published credential).

use serde_json::json;

use crate::collection_listing_authorization::{
    action_for, allow, another_namespace_only, no_policy_allows, Answer, Harness, ACCESS_DENIED,
    ACCESS_DENIED_STATUS, AMBIENT_KEY_ID, EXPLICIT_DENY, REGION, ROLE, SESSION, SESSION_KEY_ID,
    TWIN_KEY_ID, TWIN_ROLE,
};

/// The workload identity the "performs nothing" rows create, inside
/// `WorkloadIdentityNameType` (len 3..255, pattern `^[A-Za-z0-9_.-]+$`, from
/// `smithy_query(query="operation",operation="CreateWorkloadIdentity")`).
const IDENTITY_NAME: &str = "probe-workload-identity";

/// One call of this endpoint whose resource ARN the service cannot name from the request
/// line, with the route the model binds it to.
///
/// Source: `smithy_query(query="operation",operation="…")` on this crate's model for each
/// `@http` binding below, and the families are the ones this service names no single ARN
/// type for (a browser and a code interpreter are each addressed under two, the identity
/// plane's resources hang off a token vault the route does not name, and an evaluator has no
/// ARN type in this crate).
struct Call {
    operation: &'static str,
    method: &'static str,
    uri: &'static str,
    /// Whether the route's input members are bound to the payload rather than to the query
    /// or a label, so the request carries a JSON body.
    body: bool,
}

/// The calls the table below spans: four families, each with a route addressing the whole
/// family and — where the model binds one — a route naming ONE member.
const CALLS: &[Call] = &[
    Call {
        operation: "ListBrowsers",
        method: "POST",
        uri: "/browsers",
        body: false,
    },
    Call {
        // `aws.browser.v1` is the built-in browser `BrowserId`'s pattern admits, one of the
        // two ARN types that keep this family unnamed.
        operation: "GetBrowser",
        method: "GET",
        uri: "/browsers/aws.browser.v1",
        body: false,
    },
    Call {
        operation: "ListCodeInterpreters",
        method: "POST",
        uri: "/code-interpreters",
        body: false,
    },
    Call {
        operation: "GetCodeInterpreter",
        method: "GET",
        uri: "/code-interpreters/aws.codeinterpreter.v1",
        body: false,
    },
    Call {
        operation: "ListEvaluators",
        method: "POST",
        uri: "/evaluators",
        body: false,
    },
    Call {
        // Inside `EvaluatorId`'s pattern, which admits a `Builtin.` evaluator.
        operation: "GetEvaluator",
        method: "GET",
        uri: "/evaluators/Builtin.Faithfulness",
        body: false,
    },
    Call {
        operation: "ListWorkloadIdentities",
        method: "POST",
        uri: "/identities/ListWorkloadIdentities",
        body: true,
    },
    Call {
        operation: "GetTokenVault",
        method: "POST",
        uri: "/identities/get-token-vault",
        body: true,
    },
    Call {
        operation: "ListApiKeyCredentialProviders",
        method: "POST",
        uri: "/identities/ListApiKeyCredentialProviders",
        body: true,
    },
    Call {
        operation: "ListConsentPortals",
        method: "POST",
        uri: "/identities/ListConsentPortals",
        body: true,
    },
];

/// Drive `call` as the principal `access_key_id` speaks for.
async fn call_as(harness: &Harness, access_key_id: &str, call: &Call) -> Answer {
    let body = json!({});
    harness
        .send_as(
            access_key_id,
            REGION,
            call.method,
            call.uri,
            call.body.then_some(&body),
        )
        .await
}

/// Whether `answer` is this endpoint's authorization refusal.
fn is_access_denied(answer: &Answer) -> bool {
    (answer.status, answer.code.as_str()) == (ACCESS_DENIED_STATUS, ACCESS_DENIED)
}

/// The sentence a refused call whose resource was not named states: the principal, the
/// action and the rule that withheld the grant, and no resource clause — unlike the
/// collection sentence `collection_listing_authorization` asserts, which names the ARN the
/// decision was taken against.
fn not_authorized(principal: &str, action: &str, reason: &str) -> String {
    format!("User: {principal} is not authorized to perform: {action} {reason}")
}

/// Assert `answer` is this endpoint's authorization refusal of `operation`, blaming
/// `principal` with `reason` and naming no resource.
fn assert_denied(answer: &Answer, case: &str, principal: &str, operation: &str, reason: &str) {
    assert!(
        is_access_denied(answer),
        "case {case:?}: expected {ACCESS_DENIED_STATUS} {ACCESS_DENIED}, got {} {:?} with {:?}",
        answer.status,
        answer.code,
        answer.message
    );
    let expected = not_authorized(principal, &action_for(operation), reason);
    assert_eq!(
        answer.message, expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        answer.message
    );
}

/// RULE over the whole class: every call of this endpoint whose resource ARN it cannot name
/// is still judged, under ITS OWN action — a session no document grants that action to is
/// refused 403 `AccessDeniedException` naming the principal, the action and the rule, and the
/// same session granted the namespace reaches each operation.
#[tokio::test]
async fn every_call_whose_resource_is_unnamed_is_judged_under_its_own_action() {
    let harness = Harness::new();
    let principal = harness.given_session(ROLE, SESSION, SESSION_KEY_ID);

    harness.given_role_policy(ROLE, Some(&another_namespace_only()));
    for call in CALLS {
        let answer = call_as(&harness, SESSION_KEY_ID, call).await;
        assert_denied(
            &answer,
            call.operation,
            &principal,
            call.operation,
            &no_policy_allows(&action_for(call.operation)),
        );
    }

    // The same session once the namespace is granted: the gate refuses none of them, and
    // each call answers its own verdict.
    harness.given_role_policy(ROLE, Some(&allow(r#""bedrock-agentcore:*""#, r#""*""#)));
    for call in CALLS {
        let answer = call_as(&harness, SESSION_KEY_ID, call).await;
        assert!(
            !is_access_denied(&answer),
            "case {:?}: a granted call is not refused, got {} with {:?}",
            call.operation,
            answer.status,
            answer.message
        );
    }
}

/// RULE: the account's own published credential holds no IAM policy, so this gate bounds
/// none of its calls — every one of them reaches its operation.
#[tokio::test]
async fn the_accounts_own_credential_reaches_every_one_of_these_calls() {
    let harness = Harness::new();
    for call in CALLS {
        let answer = call_as(&harness, AMBIENT_KEY_ID, call).await;
        assert!(
            !is_access_denied(&answer),
            "case {:?}: the account's own credential is not refused, got {} with {:?}",
            call.operation,
            answer.status,
            answer.message
        );
    }
}

/// RULE: the documents in force decide such a call on its ACTION — a document holding no
/// `Allow` of the action refuses it, an `Allow` of the action reaches the operation whatever
/// resources the statement is scoped to (the decision never read a resource element), and an
/// explicit `Deny` of the action refuses it whatever else the principal carries.
#[tokio::test]
async fn an_allow_of_the_action_reaches_the_operation_whatever_it_is_scoped_to() {
    let harness = Harness::new();
    let principal = harness.given_session(ROLE, SESSION, SESSION_KEY_ID);
    let listing = &CALLS[0];
    let action = action_for(listing.operation);
    // One browser of this account, spelled as `BrowserArn`'s pattern admits a
    // customer-created one: a resource ARN this service's layer did not resolve for the call.
    let one_browser = format!(
        "arn:aws:bedrock-agentcore:{REGION}:{}:browser-custom/probeBrowser-abcdefghij",
        ::engine::emulator_account_id()
    );

    // (case, the document in force on the role, the refusal expected — `None` when allowed)
    let rows: Vec<(&str, Option<String>, Option<String>)> = vec![
        (
            "no document at all: IAM's default deny",
            None,
            Some(no_policy_allows(&action)),
        ),
        (
            "another action namespace entirely",
            Some(another_namespace_only()),
            Some(no_policy_allows(&action)),
        ),
        (
            "another operation of this namespace",
            Some(allow(r#""bedrock-agentcore:ListGateways""#, r#""*""#)),
            Some(no_policy_allows(&action)),
        ),
        (
            "exactly this action, over every resource",
            Some(allow(&format!(r#""{action}""#), r#""*""#)),
            None,
        ),
        (
            "the whole namespace, over every resource",
            Some(allow(r#""bedrock-agentcore:*""#, r#""*""#)),
            None,
        ),
        (
            "this action scoped to ONE resource ARN, which this decision never read",
            Some(allow(
                &format!(r#""{action}""#),
                &format!(r#""{one_browser}""#),
            )),
            None,
        ),
        (
            "an explicit deny of this action beside an allow of the namespace",
            Some(format!(
                r#"{{"Version":"2012-10-17","Statement":[
                 {{"Effect":"Allow","Action":"bedrock-agentcore:*","Resource":"*"}},
                 {{"Effect":"Deny","Action":"{action}","Resource":"*"}}]}}"#
            )),
            Some(EXPLICIT_DENY.to_string()),
        ),
        (
            "an explicit deny of the whole namespace",
            Some(
                r#"{"Version":"2012-10-17","Statement":[
                 {"Effect":"Allow","Action":"bedrock-agentcore:*","Resource":"*"},
                 {"Effect":"Deny","Action":"bedrock-agentcore:*","Resource":"*"}]}"#
                    .to_string(),
            ),
            Some(EXPLICIT_DENY.to_string()),
        ),
    ];

    for (case, document, refusal) in rows {
        harness.given_role_policy(ROLE, document.as_deref());
        let answer = call_as(&harness, SESSION_KEY_ID, listing).await;
        match refusal {
            Some(reason) => assert_denied(&answer, case, &principal, listing.operation, &reason),
            None => {
                assert_eq!(
                    answer.status, 200,
                    "case {case:?}: an allowed listing reaches the operation, got {} {:?} \
                     with {:?}",
                    answer.status, answer.code, answer.message
                );
                assert!(
                    answer.body["browserSummaries"].is_array(),
                    "case {case:?}: expected the modelled listing, got {:?}",
                    answer.body
                );
            }
        }
    }
}

/// RULE's CONSEQUENCE: a refused call performs nothing — the resource a refused create would
/// have minted does not exist afterwards — while the granted twin session, the same policy
/// plus one added allow of that create, mints it and the account owner reads it back.
#[tokio::test]
async fn a_refused_create_mints_nothing_while_the_granted_twin_creates() {
    let harness = Harness::new();
    let principal = harness.given_session(ROLE, SESSION, SESSION_KEY_ID);
    harness.given_session(TWIN_ROLE, SESSION, TWIN_KEY_ID);
    let create = Call {
        operation: "CreateWorkloadIdentity",
        method: "POST",
        uri: "/identities/CreateWorkloadIdentity",
        body: true,
    };
    let body = json!({"name": IDENTITY_NAME});
    let read = json!({"name": IDENTITY_NAME});
    let get = "/identities/GetWorkloadIdentity";

    harness.given_role_policy(ROLE, Some(&another_namespace_only()));
    // The twin carries that same agent policy plus ONE added allow of this create.
    harness.given_role_policy(
        TWIN_ROLE,
        Some(
            r#"{"Version":"2012-10-17","Statement":[
             {"Effect":"Allow","Action":["bedrock-agent:ListAgents","bedrock-agent:GetAgent"],
              "Resource":"*"},
             {"Effect":"Allow","Action":"bedrock-agentcore:CreateWorkloadIdentity",
              "Resource":"*"}]}"#,
        ),
    );

    let denied = harness
        .send_as(
            SESSION_KEY_ID,
            REGION,
            create.method,
            create.uri,
            Some(&body),
        )
        .await;
    assert_denied(
        &denied,
        "the session granted only another namespace",
        &principal,
        create.operation,
        &no_policy_allows(&action_for(create.operation)),
    );
    assert!(
        denied.body.get("workloadIdentityArn").is_none(),
        "case \"the refused create\": expected nothing of the resource disclosed, got {:?}",
        denied.body
    );

    // Nothing was minted: the account owner, which this gate bounds not at all, finds no
    // such identity.
    let absent = harness
        .send_as(AMBIENT_KEY_ID, REGION, "POST", get, Some(&read))
        .await;
    assert_eq!(
        absent.status, 404,
        "case \"after the refused create\": expected the identity to be absent, got {} with \
         {:?}",
        absent.status, absent.body
    );

    let created = harness
        .send_as(TWIN_KEY_ID, REGION, create.method, create.uri, Some(&body))
        .await;
    assert_eq!(
        created.status, 201,
        "case \"the granted twin session\": expected the create's own answer, got {} {:?} \
         with {:?}",
        created.status, created.code, created.message
    );

    let fetched = harness
        .send_as(AMBIENT_KEY_ID, REGION, "POST", get, Some(&read))
        .await;
    assert_eq!(
        (fetched.status, fetched.body["name"].as_str()),
        (200, Some(IDENTITY_NAME)),
        "case \"after the granted create\": expected the stored identity, got {} with {:?}",
        fetched.status,
        fetched.body
    );
}
