// SPDX-License-Identifier: CC0-1.0

//! Where the `AccessDenied` message of a refused IAM call STOPS.
//!
//! The rule under test: the message is one sentence — `User: ` + the ARN of the principal
//! that signed the request + ` is not authorized to perform: ` + the IAM action the call was
//! decided on + ` on resource: ` + the resource it was decided against + ` ` + the clause
//! saying why the decision went that way — and it ENDS at that clause. Nothing follows it:
//! no further sentence, no page to read the decision on, no handle for the request, nothing
//! about the evaluation that made it. The resource the sentence names is the one the request
//! addressed, in the form that operation's decision is made on (the ARN of a collection, the
//! account-wide `*`, an entity by type and name, or the ARN the request carried).
//!
//! The rows are a TABLE spanning what the rule generalises over: which operation is refused,
//! whether the request addresses a collection, the account itself, one entity or an ARN it
//! carries, whether the call reads or writes, and which of the two decisions refused it (no
//! statement allowing it, or a statement about it saying `Deny`). `access_denied_message_wording.rs`
//! is the broader wording table; this suite pins the END of the message, over the operations
//! a client reads a denial from most.
//!
//! The awsQuery harness and the fixture principal are the ones `tests/all/user_lifecycle.rs`
//! and `tests/all/identity_policy_enforcement.rs` own, so a request is built and read exactly
//! one way.

use crate::identity_policy_enforcement::{principal, statement};
use crate::role_lifecycle::borrowed;
use crate::user_lifecycle::{
    account_id, assert_access_denied_message, call, call_signed_by, element, error_code, router,
};

/// A grant for every IAM action on every resource: what a row about an explicit `Deny` is
/// layered on, so the row demonstrates the `Deny` and not a missing grant.
const ALLOW_EVERYTHING: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iam:*","Resource":"*"}]}"#;

/// The document a simulation row hands `SimulateCustomPolicy` to simulate: the request is
/// refused before anything is simulated, so what it carries only has to be a document.
const SIMULATED_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#;

/// The clause closing the sentence of a call no statement in force allows.
fn no_allow(iam_action: &str) -> String {
    format!("because no identity-based policy allows the {iam_action} action")
}

/// The clause closing the sentence of a call a statement about it explicitly denies.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// One row: the awsQuery call a zero-permission caller makes, the action its decision is
/// made on, and the resource the refusal names.
struct Row {
    case: &'static str,
    action: &'static str,
    params: Vec<(String, String)>,
    iam_action: &'static str,
    named_as: String,
}

/// The rows. `policy_arn` is a managed policy of the account, which the rows that address a
/// policy by ARN name.
fn rows(policy_arn: &str) -> Vec<Row> {
    let account = account_id();
    vec![
        // Collection reads, each decided on the ARN of the collection the request
        // addressed: the four entity types whose listings a client meets a denial on most.
        Row {
            case: "a user listing",
            action: "ListUsers",
            params: Vec::new(),
            iam_action: "iam:ListUsers",
            named_as: format!("arn:aws:iam::{account}:user/"),
        },
        Row {
            case: "a group listing",
            action: "ListGroups",
            params: Vec::new(),
            iam_action: "iam:ListGroups",
            named_as: format!("arn:aws:iam::{account}:group/"),
        },
        Row {
            case: "a role listing",
            action: "ListRoles",
            params: Vec::new(),
            iam_action: "iam:ListRoles",
            named_as: format!("arn:aws:iam::{account}:role/"),
        },
        Row {
            case: "an instance-profile listing",
            action: "ListInstanceProfiles",
            params: Vec::new(),
            iam_action: "iam:ListInstanceProfiles",
            named_as: format!("arn:aws:iam::{account}:instance-profile/"),
        },
        // The same listing under a path prefix: the resource is the prefix the request
        // addressed, so the sentence's resource moves with the request and its end does not.
        Row {
            case: "a path-scoped group listing",
            action: "ListGroups",
            params: vec![("PathPrefix".to_string(), "/eng/".to_string())],
            iam_action: "iam:ListGroups",
            named_as: format!("arn:aws:iam::{account}:group/eng/"),
        },
        // A call that names no entity of the account at all: decided on the account-wide
        // resource, so the sentence carries no ARN.
        Row {
            case: "a custom-policy simulation",
            action: "SimulateCustomPolicy",
            params: vec![
                (
                    "PolicyInputList.member.1".to_string(),
                    SIMULATED_DOCUMENT.to_string(),
                ),
                ("ActionNames.member.1".to_string(), "s3:GetObject".to_string()),
            ],
            iam_action: "iam:SimulateCustomPolicy",
            named_as: "*".to_string(),
        },
        // A WRITE on one policy, named by the ARN the request carried, prefixed by its
        // entity type.
        Row {
            case: "a tag removal from one policy",
            action: "UntagPolicy",
            params: vec![
                ("PolicyArn".to_string(), policy_arn.to_string()),
                ("TagKeys.member.1".to_string(), "env".to_string()),
            ],
            iam_action: "iam:UntagPolicy",
            named_as: format!("policy {policy_arn}"),
        },
        // A READ decided on the identity ARN the request carried, named without a type
        // prefix because that is the resource the request addressed.
        Row {
            case: "a policy-grant report on one user",
            action: "ListPoliciesGrantingServiceAccess",
            params: vec![
                (
                    "Arn".to_string(),
                    format!("arn:aws:iam::{account}:user/reported"),
                ),
                ("ServiceNamespaces.member.1".to_string(), "s3".to_string()),
            ],
            iam_action: "iam:ListPoliciesGrantingServiceAccess",
            named_as: format!("arn:aws:iam::{account}:user/reported"),
        },
    ]
}

/// A managed policy of the account the rows that address one name by ARN, created with the
/// account's own credentials so no row's refusal is about it.
async fn managed_policy(router: &smithy_server_runtime::MultiServiceRouter) -> String {
    let (status, policy) = call(
        router,
        "CreatePolicy",
        &[
            ("PolicyName", "ends-at-the-reason"),
            ("PolicyDocument", ALLOW_EVERYTHING),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture CreatePolicy must succeed, got {status}: {policy}"
    );
    element(&policy, "Arn")
}

/// Assert the answer to a refused row: HTTP 403, the `AccessDenied` code, and a message that
/// is exactly the sentence and stops there.
fn assert_refusal(
    case: &str,
    status: u16,
    body: &str,
    caller: &str,
    iam_action: &str,
    named_as: &str,
    reason: &str,
) {
    assert_eq!(
        status, 403,
        "case {case:?}: expected HTTP 403, got {status}: {body}"
    );
    assert_eq!(
        error_code(body),
        "AccessDenied",
        "case {case:?}: expected the error code AccessDenied, got {:?}",
        error_code(body)
    );
    let message = element(body, "Message");
    assert_access_denied_message(case, &message, caller, iam_action, named_as, reason);
    // The sentence ends at the reason, so the message holds exactly one sentence: nothing
    // after it points anywhere, and nothing after it names another call to make.
    assert!(
        !message.contains(". "),
        "case {case:?}: expected the refusal to end at the reason, got a continuation in \
         {message:?}"
    );
}

/// RULE: a refusal of a call nothing in force allows is the sentence naming the decision and
/// ends at the clause saying why — whichever operation was refused, on whichever resource
/// form, reading or writing.
#[tokio::test]
async fn a_withheld_call_is_refused_with_the_sentence_and_nothing_after_it() {
    let router = router();
    let policy_arn = managed_policy(&router).await;

    for (row, case) in rows(&policy_arn).into_iter().enumerate() {
        let user_name = format!("nopriv{row}");
        // No identity-based policy at all: every call below is IAM's implicit deny.
        let access_key_id = principal(&router, case.case, &user_name, &[]).await;
        let params = borrowed(&case.params);

        let (status, body) =
            call_signed_by(&router, Some(&access_key_id), case.action, &params).await;
        assert_refusal(
            case.case,
            status,
            &body,
            &format!("arn:aws:iam::{}:user/{user_name}", account_id()),
            case.iam_action,
            &case.named_as,
            &no_allow(case.iam_action),
        );
    }
}

/// RULE: the same holds for the OTHER decision — a statement about the request saying
/// `Deny` over a grant for everything — which moves the reason clause and nothing else.
#[tokio::test]
async fn an_explicitly_denied_call_is_refused_with_the_sentence_and_nothing_after_it() {
    let router = router();
    let policy_arn = managed_policy(&router).await;

    for (row, case) in rows(&policy_arn).into_iter().enumerate() {
        let user_name = format!("denied{row}");
        let access_key_id = principal(
            &router,
            case.case,
            &user_name,
            &[
                ("allow-everything".to_string(), ALLOW_EVERYTHING.to_string()),
                (
                    "deny-the-action".to_string(),
                    statement("Deny", case.iam_action, "*"),
                ),
            ],
        )
        .await;
        let params = borrowed(&case.params);

        let (status, body) =
            call_signed_by(&router, Some(&access_key_id), case.action, &params).await;
        assert_refusal(
            case.case,
            status,
            &body,
            &format!("arn:aws:iam::{}:user/{user_name}", account_id()),
            case.iam_action,
            &case.named_as,
            EXPLICIT_DENY,
        );
    }
}
