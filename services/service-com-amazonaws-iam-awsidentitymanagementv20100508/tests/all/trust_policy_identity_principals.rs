// SPDX-License-Identifier: CC0-1.0

//! The ACCOUNT IDENTITIES a role's trust policy may name, end to end over the awsQuery wire.
//!
//! RULE: IAM resolves every identity of its own account that a trust policy names — the
//! values under the `AWS` type of a statement's `Principal` or `NotPrincipal` element that
//! are an IAM user or role ARN of this account — and refuses the write when the account holds
//! no such entity: the modelled `MalformedPolicyDocumentException` (HTTP 400, awsQuery code
//! `MalformedPolicyDocument`) with `Invalid principal in policy: "AWS":"<the rejected
//! value>"`, and nothing is stored — a refused `CreateRole` mints no role and a refused
//! `UpdateAssumeRolePolicy` leaves the document the role already held. Every value that names
//! no identity of THIS account is resolved by nothing and stored as it was written: a whole
//! account, the wildcard, another account's identity, a session, a service or federated
//! principal.
//!
//! No single recorded request is the specification, so each suite is a TABLE whose rows differ
//! along what the rule generalises over: the entity TYPE the ARN names (a user, a role),
//! whether the account holds it, the path the ARN carries, the form the value takes (one
//! string, a list, the second entry of a list), the element it sits under, which statement
//! names it, the form `Statement` takes, the casing of the name, the account the ARN names,
//! and the operation that writes the member. The awsQuery harness (router, call, element,
//! error_code) is the one `tests/all/user_lifecycle.rs` owns and every suite in this crate
//! shares.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;

use crate::user_lifecycle::{account_id, call, document_element, element, error_code, router};

/// The role this account holds, and the path it is held under: a path either side of the
/// default, so a rule about paths is exercised on more than the root.
const HELD_ROLE: (&str, &str) = ("Deployer", "/team/");

/// The user this account holds, at the account root.
const HELD_USER: (&str, &str) = ("builder", "/");

/// An account that is not this emulator's own, whose IAM this emulator is not the one to ask.
const OTHER_ACCOUNT: &str = "210987654321";

/// The trust policy every role this suite creates starts out with: a service principal, which
/// this reading resolves nothing for.
const HELD_TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// A trust policy whose single statement names `principal` under the account-principal type —
/// written as the JSON the row supplies, so a row can state one string or a list of them.
fn trust(principal: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":{principal}}},"Action":"sts:AssumeRole"}}]}}"#
    )
}

/// The ARN of a role of this account at `path`, as the policy language writes a principal.
fn role_arn(path: &str, role_name: &str) -> String {
    state::role_arn(path, role_name)
}

/// The ARN of a user of this account at `path`.
fn user_arn(path: &str, user_name: &str) -> String {
    state::user_arn(path, user_name)
}

/// One document the rule refuses: the JSON the request carries, and the value IAM must name in
/// its refusal.
struct Refused {
    case: &'static str,
    document: String,
    rejected: String,
}

impl Refused {
    /// A row whose single `AWS` principal is `rejected`, written as one string.
    fn named(case: &'static str, rejected: String) -> Self {
        Self {
            case,
            document: trust(&format!("\"{rejected}\"")),
            rejected,
        }
    }
}

/// Documents naming an identity of this account the account does not hold, spanning the forms
/// the rule generalises over.
fn refused_documents() -> Vec<Refused> {
    let held_role = role_arn(HELD_ROLE.1, HELD_ROLE.0);
    let absent_role = role_arn("/", "ghost-role");
    let absent_user = user_arn("/", "ghost-user");
    vec![
        Refused::named("a role of this account that nothing holds", absent_role.clone()),
        Refused::named(
            "a role under a path, which nothing holds either",
            role_arn("/team/tools/", "ghost-role"),
        ),
        Refused::named("a user of this account that nothing holds", absent_user.clone()),
        Refused::named(
            "a user ARN carrying the name of a role the account holds, a different entity",
            user_arn(HELD_ROLE.1, HELD_ROLE.0),
        ),
        Refused::named(
            "a role ARN carrying the name of a user the account holds",
            role_arn(HELD_USER.1, HELD_USER.0),
        ),
        Refused::named(
            "an ARN whose account field is empty, which names this account",
            "arn:aws:iam:::role/ghost-role".to_string(),
        ),
        Refused::named(
            "a principal ARN carrying a wildcard, which names no one entity",
            role_arn("/", "*"),
        ),
        Refused {
            case: "the second value of a list, beside a role the account holds",
            document: trust(&format!("[\"{held_role}\",\"{absent_role}\"]")),
            rejected: absent_role.clone(),
        },
        Refused {
            case: "the complement element names it",
            document: format!(
                r#"{{"Statement":[{{"Effect":"Deny","NotPrincipal":{{"AWS":"{absent_role}"}},"Action":"sts:AssumeRole"}}]}}"#
            ),
            rejected: absent_role.clone(),
        },
        Refused {
            case: "the second statement names it, after one IAM accepts",
            document: format!(
                r#"{{"Statement":[{{"Effect":"Allow","Principal":{{"AWS":"{held_role}"}},"Action":"sts:AssumeRole"}},{{"Effect":"Allow","Principal":{{"AWS":"{absent_user}"}},"Action":"sts:AssumeRole"}}]}}"#
            ),
            rejected: absent_user.clone(),
        },
        Refused {
            case: "the single statement object the grammar admits names it",
            document: format!(
                r#"{{"Statement":{{"Effect":"Allow","Principal":{{"AWS":"{absent_role}"}},"Action":"sts:AssumeRole"}}}}"#
            ),
            rejected: absent_role.clone(),
        },
        Refused {
            case: "beside a service principal under the same element",
            document: format!(
                r#"{{"Statement":[{{"Effect":"Allow","Principal":{{"AWS":"{absent_role}","Service":"ec2.amazonaws.com"}},"Action":"sts:AssumeRole"}}]}}"#
            ),
            rejected: absent_role,
        },
    ]
}

/// Documents the rule stores: the identities this account DOES hold, and every value that
/// names no identity of this account at all. Each row carries the document the request sends
/// and the document a later read reports, which differ only where the account-principal
/// spelling rule rewrites a bare account id into that account's root ARN
/// (`tests/all/role_trust_policy_account_principals.rs` owns that rule).
fn accepted_documents() -> Vec<(String, String, String)> {
    let account = account_id();
    let held_role = role_arn(HELD_ROLE.1, HELD_ROLE.0);
    let held_user = user_arn(HELD_USER.1, HELD_USER.0);
    // A role the OWNING SERVICE keeps provisioned under a name AWS publishes is a role that
    // resolves, although `iam:CreateRole` never created it. The catalog is read here rather
    // than retyped (`state::PUBLISHED_SERVICE_LINKED_ROLE_NAMES`), and the row is its first
    // entry, which no request of this suite cites.
    let (provisioned_principal, provisioned_name) = *state::PUBLISHED_SERVICE_LINKED_ROLE_NAMES
        .first()
        .expect("the published service-linked role catalog carries at least one row");
    let provisioned = role_arn(
        &state::service_linked_role_path(provisioned_principal),
        provisioned_name,
    );
    let mut rows: Vec<(String, String, String)> = vec![
        (
            "the role this account holds".to_string(),
            trust(&format!("\"{held_role}\"")),
            trust(&format!("\"{held_role}\"")),
        ),
        (
            "the same role in another casing, since a name is not distinguished by case"
                .to_string(),
            trust(&format!("\"{}\"", role_arn(HELD_ROLE.1, "DEPLOYER"))),
            trust(&format!("\"{}\"", role_arn(HELD_ROLE.1, "DEPLOYER"))),
        ),
        (
            "the user this account holds".to_string(),
            trust(&format!("\"{held_user}\"")),
            trust(&format!("\"{held_user}\"")),
        ),
        (
            format!("the role {provisioned_principal} keeps provisioned"),
            trust(&format!("\"{provisioned}\"")),
            trust(&format!("\"{provisioned}\"")),
        ),
        (
            "both identities the account holds, as a list".to_string(),
            trust(&format!("[\"{held_role}\",\"{held_user}\"]")),
            trust(&format!("[\"{held_role}\",\"{held_user}\"]")),
        ),
        (
            "every principal".to_string(),
            trust("\"*\""),
            trust("\"*\""),
        ),
        (
            "this account's root".to_string(),
            trust(&format!("\"arn:aws:iam::{account}:root\"")),
            trust(&format!("\"arn:aws:iam::{account}:root\"")),
        ),
        (
            "this whole account by its id".to_string(),
            trust(&format!("\"{account}\"")),
            trust(&format!("\"arn:aws:iam::{account}:root\"")),
        ),
        (
            "a role of another account, whose IAM this emulator is not".to_string(),
            trust(&format!("\"arn:aws:iam::{OTHER_ACCOUNT}:role/ghost-role\"")),
            trust(&format!("\"arn:aws:iam::{OTHER_ACCOUNT}:role/ghost-role\"")),
        ),
        (
            "a session of this account, which is no record of an entity".to_string(),
            trust(&format!(
                "\"arn:aws:sts::{account}:assumed-role/{}/build\"",
                HELD_ROLE.0
            )),
            trust(&format!(
                "\"arn:aws:sts::{account}:assumed-role/{}/build\"",
                HELD_ROLE.0
            )),
        ),
    ];
    rows.push((
        "a service principal and a federated one, naming no account identity".to_string(),
        r#"{"Statement":[{"Effect":"Allow","Principal":{"Service":"lambda.amazonaws.com","Federated":"accounts.google.com"},"Action":"sts:AssumeRole"}]}"#
            .to_string(),
        r#"{"Statement":[{"Effect":"Allow","Principal":{"Service":"lambda.amazonaws.com","Federated":"accounts.google.com"},"Action":"sts:AssumeRole"}]}"#
            .to_string(),
    ));
    rows
}

/// One `CreateRole` call carrying `document`.
async fn create_role(
    router: &MultiServiceRouter,
    role_name: &str,
    path: Option<&str>,
    document: &str,
) -> (u16, String) {
    let mut params: Vec<(&str, &str)> = vec![
        ("RoleName", role_name),
        ("AssumeRolePolicyDocument", document),
    ];
    if let Some(path) = path {
        params.push(("Path", path));
    }
    call(router, "CreateRole", &params).await
}

/// One `UpdateAssumeRolePolicy` call carrying `document`.
async fn update_trust_policy(
    router: &MultiServiceRouter,
    role_name: &str,
    document: &str,
) -> (u16, String) {
    call(
        router,
        "UpdateAssumeRolePolicy",
        &[("RoleName", role_name), ("PolicyDocument", document)],
    )
    .await
}

/// The trust policy `GetRole` reports, percent-decoded back to plain JSON as IAM's contract
/// tells a caller to read a policy document.
async fn reported_trust_policy(router: &MultiServiceRouter, role_name: &str) -> String {
    let (status, body) = call(router, "GetRole", &[("RoleName", role_name)]).await;
    assert_eq!(
        status, 200,
        "case {role_name:?}: GetRole must succeed, got {status}: {body}"
    );
    document_element(&body, "AssumeRolePolicyDocument")
}

/// A router whose account holds the one role and the one user every row resolves against.
async fn seeded_router() -> MultiServiceRouter {
    let router = router();
    let (status, body) = create_role(
        &router,
        HELD_ROLE.0,
        Some(HELD_ROLE.1),
        HELD_TRUST_POLICY,
    )
    .await;
    assert_eq!(
        status, 200,
        "the held role must be created, got {status}: {body}"
    );
    let (status, body) = call(
        &router,
        "CreateUser",
        &[("UserName", HELD_USER.0), ("Path", HELD_USER.1)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the held user must be created, got {status}: {body}"
    );
    router
}

/// Assert that `body` is IAM's invalid-principal refusal naming `rejected`.
fn assert_invalid_principal(case: &str, status: u16, body: &str, rejected: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400, got {status}: {body}"
    );
    let code = error_code(body);
    assert_eq!(
        code, "MalformedPolicyDocument",
        "case {case:?}: expected the modelled awsQuery code, got {code:?}: {body}"
    );
    // The sentence spelled out here rather than read off the production function, so this
    // wire-level suite fails if its fixed text ever changes: `Invalid principal in policy: `
    // then the principal TYPE in upper case and the rejected value, each quoted and separated
    // by a colon, with the value the only variable part.
    let message = element(body, "Message");
    let expected = format!("Invalid principal in policy: \"AWS\":\"{rejected}\"");
    assert_eq!(
        message, expected,
        "case {case:?}: expected IAM's invalid-principal sentence {expected:?}, got {message:?}"
    );
}

/// RULE: `CreateRole` refuses a trust policy naming an identity of this account the account
/// does not hold — `MalformedPolicyDocument`, HTTP 400, IAM's invalid-principal sentence
/// naming the rejected value as written — and mints no role.
#[tokio::test]
async fn creating_a_role_trusting_an_identity_nothing_holds_is_refused() {
    let router = seeded_router().await;
    for (index, row) in refused_documents().into_iter().enumerate() {
        let role_name = format!("refused-{index}");
        let (status, body) = create_role(&router, &role_name, None, &row.document).await;
        assert_invalid_principal(row.case, status, &body, &row.rejected);

        // Nothing was persisted: the name the refused create carried resolves to no role.
        let (get_status, got) = call(&router, "GetRole", &[("RoleName", &role_name)]).await;
        assert_eq!(
            get_status, 404,
            "case {:?}: a refused create must persist nothing, got {get_status}: {got}",
            row.case
        );
    }
}

/// RULE: a trust policy whose account identities the account HOLDS — and one naming no
/// identity of this account at all — is stored, and `GetRole` reports it: the refusal above is
/// about the identity named, not about naming one.
#[tokio::test]
async fn a_trust_policy_naming_a_resolvable_principal_is_stored_and_read_back() {
    let router = seeded_router().await;
    for (index, (case, document, stored)) in accepted_documents().into_iter().enumerate() {
        let role_name = format!("accepted-{index}");
        let (status, body) = create_role(&router, &role_name, None, &document).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to be accepted, got {status}: {body}"
        );
        let reported = reported_trust_policy(&router, &role_name).await;
        assert_eq!(
            reported, stored,
            "case {case:?}: expected the stored document to read back, got {reported:?}"
        );
    }
}

/// RULE: `UpdateAssumeRolePolicy` writes the same member, so it resolves the same identities —
/// and a refused update leaves the role with the document it already held, while a document
/// whose identities resolve replaces it and reads back.
#[tokio::test]
async fn replacing_a_trust_policy_resolves_its_account_identities_too() {
    let router = seeded_router().await;
    let role_name = "TrustTarget";
    let (status, body) = create_role(&router, role_name, None, HELD_TRUST_POLICY).await;
    assert_eq!(status, 200, "the role must be created, got {status}: {body}");

    for row in refused_documents() {
        let (status, body) = update_trust_policy(&router, role_name, &row.document).await;
        assert_invalid_principal(row.case, status, &body, &row.rejected);
        let reported = reported_trust_policy(&router, role_name).await;
        assert_eq!(
            reported, HELD_TRUST_POLICY,
            "case {:?}: a refused update must leave the held policy alone, got {reported:?}",
            row.case
        );
    }

    for (case, document, stored) in accepted_documents() {
        let (status, body) = update_trust_policy(&router, role_name, &document).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to be accepted, got {status}: {body}"
        );
        let reported = reported_trust_policy(&router, role_name).await;
        assert_eq!(
            reported, stored,
            "case {case:?}: expected the stored document to read back, got {reported:?}"
        );
    }
}

/// RULE: the refusal is grounded in the ACCOUNT's own records, so an identity the account
/// holds resolves and the SAME document is refused once that identity is gone: a trust policy
/// naming a role is storable while the role exists and refused after it is deleted.
#[tokio::test]
async fn the_same_document_is_resolved_against_the_account_as_it_stands() {
    let router = seeded_router().await;
    let trusted = "Trusted";
    let (status, body) = create_role(&router, trusted, None, HELD_TRUST_POLICY).await;
    assert_eq!(
        status, 200,
        "the trusted role must be created, got {status}: {body}"
    );
    let document = trust(&format!("\"{}\"", role_arn("/", trusted)));

    let (status, body) = create_role(&router, "TrustsTheRole", None, &document).await;
    assert_eq!(
        status, 200,
        "case \"while the trusted role exists\": expected the create to be accepted, got \
         {status}: {body}"
    );

    let (status, body) = call(&router, "DeleteRole", &[("RoleName", trusted)]).await;
    assert_eq!(
        status, 200,
        "the trusted role must be deletable, got {status}: {body}"
    );

    let (status, body) = create_role(&router, "TrustsTheGoneRole", None, &document).await;
    assert_invalid_principal(
        "once the trusted role is gone",
        status,
        &body,
        &role_arn("/", trusted),
    );
    let (status, body) = update_trust_policy(&router, "TrustsTheRole", &document).await;
    assert_invalid_principal(
        "replacing a trust policy with the same document once the role is gone",
        status,
        &body,
        &role_arn("/", trusted),
    );
}
