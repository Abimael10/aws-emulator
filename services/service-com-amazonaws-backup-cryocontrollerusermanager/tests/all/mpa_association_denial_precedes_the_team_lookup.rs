// SPDX-License-Identifier: CC0-1.0

//! `AssociateBackupVaultMpaApprovalTeam` settles the VAULT before it judges `MpaApprovalTeamArn`
//! — including when that member carries a team the approval service really holds.
//!
//! The rule under test: **a vault-scoped call is authorized against the vault it names before
//! any member it carries is resolved or judged, so a call whose vault is outside the caller's
//! visibility is refused HTTP 403 `AccessDeniedException` with the fixed sentence "Insufficient
//! privileges to perform this action." whatever the rest of the request carries, writes nothing
//! anywhere, and a member-value refusal is only ever reachable for a vault the caller may act
//! on.** A vault is invisible when the account and Region the request is scoped to does not hold
//! that name — it was never created, it was deleted, or it belongs to another Region.
//!
//! The sibling suite `mpa_association_decision_order.rs` pins the order for team ARNs that name
//! no team, in one Region and with no `RequesterComment`. The rows here widen it along the rest
//! of what the rule generalises over, and add the value that shows the answer does not depend on
//! the member at all: a team ARN that WOULD resolve. Multi-party approval and AWS Organizations
//! are therefore mounted beside AWS Backup on one engine (the harness
//! `mpa_approval_team_association.rs` publishes), and the team every row names is one created
//! through the approval service's own wire — never a value made up here.
//!
//! The rows span the reason the vault is invisible (never created, deleted, a vault of another
//! Region addressed from this one), the `MpaApprovalTeamArn` carried (a team the approval service
//! holds, a well-formed ARN naming none, the empty value) and whether the optional
//! `RequesterComment` is sent. Every row must be the SAME answer, down to the body, because none
//! of what it carries is reached — and the Region that DOES hold a vault must report no team
//! afterwards. The control keeps it honest: the same held team on a vault the caller CAN see is
//! the modelled 204, read back through `DescribeBackupVault`, and only there is a team ARN naming
//! no team refused for the member.
//!
//! The harness is shared, not restated: the three mounted services come from
//! `mpa_approval_team_association.rs`, and the setup ladder and the refusal assertion (status,
//! error shape, `__type`, the fixed sentence and the silence about the vault) from
//! `vault_read_visibility.rs`, which owns the access-before-existence rule for vault-scoped
//! reads. Only the Region-scoped setup and the two things this operation adds — that the refusal
//! names neither the member nor the value it carried — live here.

use serde_json::{json, Value};

use crate::mpa_approval_team_association::{Emulator, WINDOW};
use crate::vault_and_backup_job::{
    call_reported_in_region, error_code, string_at, ACCOUNT, REGION,
};
use crate::vault_read_visibility::{assert_denied, prepare, Answer, Setup};

/// The error shape, and the `__type`, every authorization refusal carries.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The error the model declares for a member whose value this service cannot accept.
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The member a parameter-value refusal of this operation blames — and the one an
/// authorization refusal must NOT mention, since it is never reached.
const MEMBER: &str = "MpaApprovalTeamArn";

/// A Region that holds none of the vaults a case addresses from [`REGION`].
const OTHER_REGION: &str = "eu-central-1";

/// Why a vault name is outside the caller's visibility in [`REGION`].
#[derive(Debug, Clone, Copy)]
enum Invisible {
    /// [`REGION`] does not hold that name at all: the vault-read suite's own setup ladder
    /// puts the store in that state (never created, or created and then deleted).
    NotHeldHere(Setup),
    /// The name is an air-gapped vault of [`OTHER_REGION`], which the call is not made in.
    AnotherRegion,
}

/// `PUT /backup-vaults/{BackupVaultName}/mpaApprovalTeam`, as the model binds the association,
/// from a client bound to `region`.
async fn associate_in(
    emulator: &Emulator,
    region: &str,
    vault: &str,
    team_arn: &str,
    comment: Option<&str>,
) -> (u16, String, Value) {
    let mut body = json!({ MEMBER: team_arn });
    if let Some(comment) = comment {
        body["RequesterComment"] = json!(comment);
    }
    call_reported_in_region(
        &emulator.router,
        region,
        "PUT",
        &format!("/backup-vaults/{vault}/mpaApprovalTeam"),
        Some(&body.to_string()),
    )
    .await
}

/// `DescribeBackupVault` from a client bound to `region`.
async fn describe_in(emulator: &Emulator, region: &str, vault: &str) -> (u16, Value) {
    let (status, _errortype, body) = call_reported_in_region(
        &emulator.router,
        region,
        "GET",
        &format!("/backup-vaults/{vault}"),
        None,
    )
    .await;
    (status, body)
}

/// Put the store into the state `setup` names for `vault`.
async fn prepare_invisible(emulator: &Emulator, setup: Invisible, vault: &str) {
    match setup {
        Invisible::NotHeldHere(ladder) => prepare(&emulator.router, ladder, vault).await,
        Invisible::AnotherRegion => {
            // Created as the one vault type multi-party approval is a facility of, so the row
            // stands on the Region alone and not on the vault's type.
            let (status, _errortype, created) = call_reported_in_region(
                &emulator.router,
                OTHER_REGION,
                "PUT",
                &format!("/logically-air-gapped-backup-vaults/{vault}"),
                Some(WINDOW),
            )
            .await;
            assert_eq!(
                status, 200,
                "setup for {vault:?}: the vault must be created in {OTHER_REGION}, got {status} \
                 {created:?}"
            );
        }
    }
}

/// Assert one answer is the authorization refusal, and that it discloses neither the vault nor
/// the member it never reached.
///
/// The status, the error shape the response names, the body's `__type`, the fixed sentence and
/// the silence about the vault are the vault-read suite's own assertion, shared rather than
/// restated here; what this operation adds is that the refusal names neither the member whose
/// value it never judged nor the value that member carried.
fn assert_denied_association(
    case: &str,
    answered: &(u16, String, Value),
    vault: &str,
    team_arn: &str,
) {
    let (status, errortype, body) = answered;
    assert_denied(
        case,
        &Answer {
            status: *status,
            error_type: Some(errortype.clone()),
            body: body.clone(),
        },
        vault,
    );
    let rendered = body.to_string();
    assert!(
        !rendered.contains(MEMBER),
        "case {case:?}: the authorization refusal must not blame {MEMBER}, got {rendered}"
    );
    assert!(
        team_arn.is_empty() || !rendered.contains(team_arn),
        "case {case:?}: the authorization refusal must not carry the team ARN {team_arn:?}, \
         got {rendered}"
    );
}

/// RULE: the vault's own authorization is settled first, so an invisible vault is 403
/// `AccessDeniedException` even when `MpaApprovalTeamArn` names a team the approval service
/// holds — and the refusal records nothing, in that Region or any other.
#[tokio::test]
async fn an_invisible_vault_is_denied_even_for_a_team_the_approval_service_holds() {
    let emulator = Emulator::new().await;
    let held = emulator.create_approval_team("ordering-team").await;

    let invisible: Vec<(&str, Invisible, &str)> = vec![
        (
            "a vault that was never created",
            Invisible::NotHeldHere(Setup::Absent),
            "never-created",
        ),
        (
            "a vault that was deleted",
            Invisible::NotHeldHere(Setup::Deleted),
            "deleted_vault_1",
        ),
        (
            "an air-gapped vault of another Region",
            Invisible::AnotherRegion,
            "far-vault",
        ),
    ];
    // None of these is reached for an invisible vault, so the answer must not depend on which
    // one the request carries: one the approval service holds, one of the same form naming no
    // team, and the empty value the visible-vault control is refused for.
    let team_arns: Vec<(&str, String)> = vec![
        ("a team the approval service holds", held.clone()),
        (
            "a well-formed team ARN naming no team",
            format!("arn:aws:mpa:{REGION}:{ACCOUNT}:approval-team/does-not-exist"),
        ),
        ("an empty team ARN", String::new()),
    ];
    let comments = [
        ("no RequesterComment", None),
        ("a RequesterComment", Some("please put this team in charge")),
    ];

    for (_reason, setup, vault) in &invisible {
        prepare_invisible(&emulator, *setup, vault).await;
    }

    let mut answers: Vec<(String, (u16, String, Value))> = Vec::new();
    for (reason, _setup, vault) in &invisible {
        for (arn_case, team_arn) in &team_arns {
            for (comment_case, comment) in comments {
                let case = format!("{reason}, with {arn_case} and {comment_case}");
                let answered = associate_in(&emulator, REGION, vault, team_arn, comment).await;
                assert_denied_association(&case, &answered, vault, team_arn);
                answers.push((case, answered));
            }
        }
    }

    // Every row is the SAME answer: a client cannot tell from it which of the three reasons
    // the vault was invisible for, nor what the request carried.
    let (first_case, first) = answers
        .first()
        .cloned()
        .expect("the table drives at least one association");
    for (case, answered) in &answers {
        assert_eq!(
            answered, &first,
            "case {case:?}: must answer exactly as case {first_case:?}, got {answered:?} \
             against {first:?}"
        );
    }

    // And the refusals wrote nothing: the vault that does exist — in the Region that holds it —
    // still reports no team, and the names that never did are still invisible.
    let (status, described) = describe_in(&emulator, OTHER_REGION, "far-vault").await;
    assert_eq!(
        status, 200,
        "the vault of {OTHER_REGION} must still read back there, got {status} {described:?}"
    );
    assert_eq!(
        described.get(MEMBER),
        None,
        "a denied association must record no team on the other Region's vault, got {described:?}"
    );
    for vault in ["never-created", "deleted_vault_1", "far-vault"] {
        let (status, described) = describe_in(&emulator, REGION, vault).await;
        assert_eq!(
            (status, error_code(&described).as_str()),
            (403, ACCESS_DENIED),
            "case {vault:?}: a denied association must not have created a vault in {REGION}, \
             got {status} {described:?}"
        );
    }
}

/// The CONTROL: a vault the caller CAN see reaches the member, so the SAME held team is
/// recorded and read back, and only then is a team ARN naming no team refused for the member.
/// Without it, a handler that answered 403 for everything would pass the table above.
#[tokio::test]
async fn a_visible_vault_reaches_the_team_arn_and_records_a_held_team() {
    let emulator = Emulator::new().await;
    emulator.create_vault("visible-vault").await;
    let held = emulator.create_approval_team("ordering-team").await;

    let (status, errortype, associated) = associate_in(
        &emulator,
        REGION,
        "visible-vault",
        &held,
        Some("please put this team in charge"),
    )
    .await;
    assert_eq!(
        (status, errortype.as_str()),
        (204, "<unset>"),
        "a visible vault and a held team must answer the modelled 204, got {status} \
         {associated:?}"
    );
    assert_eq!(
        emulator.reported_team("visible-vault").await,
        held,
        "the describe must report the team the association stored"
    );

    let unheld = format!("arn:aws:mpa:{REGION}:{ACCOUNT}:approval-team/does-not-exist");
    let (status, _errortype, refused) =
        associate_in(&emulator, REGION, "visible-vault", &unheld, None).await;
    assert_eq!(
        (
            status,
            error_code(&refused),
            string_at(&refused, "Message"),
            string_at(&refused, "Context"),
        ),
        (
            400,
            INVALID_PARAMETER.to_string(),
            format!("Parameter {MEMBER} is invalid."),
            MEMBER.to_string(),
        ),
        "a visible vault must reach the member's own refusal, got {status} {refused:?}"
    );
    assert_eq!(
        emulator.reported_team("visible-vault").await,
        held,
        "a refused association must leave the stored team alone"
    );
}
