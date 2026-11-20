// SPDX-License-Identifier: CC0-1.0

//! `AssociateBackupVaultMpaApprovalTeam` decides ACCESS to the vault before it judges the
//! value of `MpaApprovalTeamArn`.
//!
//! The rule under test: **a vault-scoped call is authorized against the vault it names BEFORE
//! any member it carries is judged, so a call whose vault is outside the caller's visibility —
//! the account the request is scoped to does not hold it, never created or deleted — is refused
//! HTTP 403 `AccessDeniedException` with the fixed sentence "Insufficient privileges to perform
//! this action.", whatever the rest of the request carries, and a parameter-value refusal is
//! only ever reachable for a vault the caller may act on.** The decision order is what makes
//! the two refusals distinguishable: a client branching on the authorization answer must not be
//! told its parameter is wrong instead.
//!
//! The rows span what the rule generalises over: the reason the vault is invisible (never
//! created, deleted), the vault-name spelling (both punctuation characters
//! `^[a-zA-Z0-9\-\_]{2,50}$` admits and both ends of its length bound), and the
//! `MpaApprovalTeamArn` the request carries — the empty value the visible-vault control is
//! refused for, a well-formed team ARN naming no team, and a value that is no ARN at all. Each
//! is answered the same way, because none of them is reached.
//!
//! The control keeps it honest: the SAME call against a vault the caller CAN see is refused for
//! the parameter instead, 400 `InvalidParameterValueException` naming the member — so no row
//! passes against a handler that answers 403 for everything.
//!
//! The harness (`router`, `call`, `prepare`, `assert_denied`) is the one the suite owning the
//! access-before-existence rule for vault-scoped reads publishes, so every request here goes
//! through the same wire path.

use serde_json::json;

use crate::vault_read_visibility::{
    assert_denied, call, error_code, message, prepare, router, Setup,
};

/// Where the model binds the association:
/// `PUT /backup-vaults/{BackupVaultName}/mpaApprovalTeam`.
fn association_path(vault: &str) -> String {
    format!("/backup-vaults/{vault}/mpaApprovalTeam")
}

/// The member every parameter-value refusal of this operation blames.
const MEMBER: &str = "MpaApprovalTeamArn";

/// The error the model declares for a member whose value this service cannot accept.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The `MpaApprovalTeamArn` values a row may carry: none of them is reached for a vault the
/// caller cannot see, so the answer must not depend on which one it is.
fn team_arns() -> Vec<(&'static str, String)> {
    vec![
        ("an empty team ARN", String::new()),
        (
            "a well-formed team ARN naming no team",
            "arn:aws:mpa:us-west-2:000000000000:approval-team/does-not-exist".to_string(),
        ),
        ("a value that is no ARN at all", "not-an-arn".to_string()),
    ]
}

/// RULE: the vault's own authorization is settled first, so an invisible vault is 403
/// `AccessDeniedException` whatever `MpaApprovalTeamArn` carries.
#[tokio::test]
async fn an_association_on_an_invisible_vault_is_denied_before_the_team_arn_is_judged() {
    let longest: String = std::iter::repeat_n('v', 50).collect();
    let invisible: Vec<(&str, Setup, String)> = vec![
        (
            "a vault that was never created",
            Setup::Absent,
            "never-created".to_string(),
        ),
        (
            "a two-character name that was never created",
            Setup::Absent,
            "ab".to_string(),
        ),
        (
            "a fifty-character name that was never created",
            Setup::Absent,
            longest,
        ),
        (
            "a vault that was deleted",
            Setup::Deleted,
            "deleted-vault".to_string(),
        ),
        (
            "an under_scored name that was deleted",
            Setup::Deleted,
            "deleted_vault_1".to_string(),
        ),
    ];

    for (reason, setup, vault) in &invisible {
        for (arn_case, team_arn) in team_arns() {
            let case = format!("{reason}, with {arn_case}");
            let router = router();
            prepare(&router, *setup, vault).await;
            let answer = call(
                &router,
                "PUT",
                &association_path(vault),
                Some(&json!({ MEMBER: team_arn }).to_string()),
            )
            .await;
            assert_denied(&case, &answer, vault);
            // The refusal is about the vault, so it must not blame the member either.
            let rendered = answer.body.to_string();
            assert!(
                !rendered.contains(MEMBER),
                "case {case:?}: the authorization refusal must not blame {MEMBER}, got {rendered}"
            );
        }
    }
}

/// The CONTROL: a vault the caller CAN see reaches the member's own judgement, so the same call
/// is the parameter-value refusal naming the member.
#[tokio::test]
async fn an_association_on_a_visible_vault_reaches_the_team_arn_refusal() {
    let router = router();
    prepare(&router, Setup::Created, "visible-vault").await;

    let answer = call(
        &router,
        "PUT",
        &association_path("visible-vault"),
        Some(&json!({ MEMBER: "" }).to_string()),
    )
    .await;
    assert_eq!(
        answer.status, 400,
        "a visible vault must reach the member's judgement, got {answer:?}"
    );
    assert_eq!(
        error_code(&answer.body),
        INVALID_PARAMETER,
        "expected {INVALID_PARAMETER} for the empty team ARN, got {:?}",
        answer.body
    );
    assert_eq!(
        message(&answer.body),
        format!("Parameter {MEMBER} is invalid."),
        "expected the member's own refusal sentence, got {:?}",
        answer.body
    );
}
