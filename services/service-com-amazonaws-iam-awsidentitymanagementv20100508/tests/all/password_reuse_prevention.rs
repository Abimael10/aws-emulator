// SPDX-License-Identifier: CC0-1.0

//! The account's `PasswordReusePrevention` window, enforced on an IAM user changing its OWN
//! console password.
//!
//! RULE: the account's password policy states "the number of previous passwords that IAM
//! users are prevented from reusing" (`PasswordPolicy$PasswordReusePrevention`), and a
//! `ChangePassword` whose `NewPassword` is one of the user's most recent passwords inside
//! that window — the password in force among them — is refused with the modelled
//! `PasswordPolicyViolationException` (HTTP 400, awsQuery code `PasswordPolicyViolation`,
//! `smithy_query(query="operation", operation="ChangePassword")`), leaving the password in
//! force exactly as it was. A password the window no longer covers, and any password at all
//! for an account whose policy set no window (the documented default, under which users "are
//! not prevented from reusing previous passwords"), is accepted.
//!
//! The first test is a TABLE whose rows differ along what the rule generalises over: the
//! window the account set (both published bounds of `passwordReusePreventionType`, range
//! 1..24, a value between them, a policy that sets none and no policy at all) and how far
//! back the password the user returns to was — the one in force, one the window still covers,
//! the oldest it covers, and the first one outside it. The remaining tests pin the two things
//! the window is decided FROM: it is one user's own history, and a password an administrative
//! `UpdateLoginProfile` superseded is in it.
//!
//! The awsQuery harness (router, call, element, error_code) is the one
//! `tests/all/user_lifecycle.rs` owns and the `iam:ChangePassword` fixture grant the one
//! `tests/all/identity_policy_enforcement.rs` owns, so every suite in this crate builds and
//! reads a request the same way.

use smithy_server_runtime::MultiServiceRouter;

use crate::user_lifecycle::{call, call_signed_by, element, error_code, router};

/// The awsQuery error code of `PasswordPolicyViolationException`, and the status it carries
/// (HTTP 400).
const VIOLATION_CODE: &str = "PasswordPolicyViolation";

/// The WHOLE message that refusal carries, spelled out here rather than imported from the
/// service, so a change of the wording fails this suite instead of following it. Every
/// character is fixed: it names the rule and interpolates nothing — not the user, not the
/// window, and no password.
const VIOLATION_MESSAGE: &str = "Password cannot be the same as previously used password";

/// The password the user walks to after the attempt, to show which password is in force: a
/// value no row ever sets, so a change presenting it could not be served by accident.
const FRESH_PASSWORD: &str = "Fresh-passw0rd-after-the-attempt!";

/// One row: the window the account sets, how long a history the user walks, how far back the
/// password it then returns to was, and whether the account admits that.
struct Row {
    case: &'static str,
    /// The `PasswordReusePrevention` the policy sets, `Some(None)` for a policy that sets
    /// none, and `None` for an account holding no password policy at all.
    window: Option<Option<&'static str>>,
    /// How many changes the user makes before the attempt, so the attempt has a history to
    /// reach back into.
    changes: usize,
    /// Which password the attempt returns to, counted back from the one in force: `0` is the
    /// password in force, `1` the one it superseded, and so on.
    depth: usize,
    /// Whether the window bars that password.
    refused: bool,
}

/// The rows, spanning the window and the position in the history.
fn rows() -> Vec<Row> {
    vec![
        // The published minimum window (1) covers exactly the password in force.
        Row {
            case: "a window of one bars the password in force",
            window: Some(Some("1")),
            changes: 1,
            depth: 0,
            refused: true,
        },
        Row {
            case: "a window of one admits the password it superseded",
            window: Some(Some("1")),
            changes: 1,
            depth: 1,
            refused: false,
        },
        // A window between the bounds, at its far edge and one past it.
        Row {
            case: "a window of three bars the oldest password it covers",
            window: Some(Some("3")),
            changes: 2,
            depth: 2,
            refused: true,
        },
        Row {
            case: "a window of three admits the first password outside it",
            window: Some(Some("3")),
            changes: 3,
            depth: 3,
            refused: false,
        },
        // The published maximum window (24) reaches every password this user ever had.
        Row {
            case: "the published maximum window bars a password five changes back",
            window: Some(Some("24")),
            changes: 5,
            depth: 5,
            refused: true,
        },
        // No window at all: the documented default, under which nothing is barred — not even
        // the password in force.
        Row {
            case: "a policy that sets no window admits the password in force",
            window: Some(None),
            changes: 1,
            depth: 0,
            refused: false,
        },
        Row {
            case: "an account holding no password policy admits the password in force",
            window: None,
            changes: 1,
            depth: 0,
            refused: false,
        },
    ]
}

/// The password a row holds at step `index` of its walk: a value conforming to the length and
/// character rules every row's policy leaves at its default.
fn password(index: usize) -> String {
    format!("Passw0rd-step-{index}!")
}

/// Put the account's password policy in force, or leave the account without one.
async fn set_window(router: &MultiServiceRouter, case: &str, window: Option<Option<&str>>) {
    let Some(window) = window else {
        return;
    };
    let mut params: Vec<(&str, &str)> = Vec::new();
    if let Some(window) = window {
        params.push(("PasswordReusePrevention", window));
    } else {
        // A policy that sets no reuse window at all still has to be a policy, so it names
        // another member: the write "does not support partial updates", so every member it
        // omits — `PasswordReusePrevention` among them — takes its documented default.
        params.push(("MaxPasswordAge", "90"));
    }
    let (status, body) = call(router, "UpdateAccountPasswordPolicy", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture UpdateAccountPasswordPolicy must succeed, got {status}: \
         {body}"
    );
}

/// Create `user` with the console password `password` and the one grant a signed
/// `ChangePassword` needs, answering the access key id its own requests are signed with.
async fn user_with_password(
    router: &MultiServiceRouter,
    case: &str,
    user: &str,
    password: &str,
) -> String {
    let (status, body) = call(router, "CreateUser", &[("UserName", user)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture CreateUser must succeed, got {status}: {body}"
    );
    let (status, body) = call(
        router,
        "CreateLoginProfile",
        &[("UserName", user), ("Password", password)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture CreateLoginProfile must succeed, got {status}: {body}"
    );
    crate::identity_policy_enforcement::grant_action(router, case, user, "iam:ChangePassword")
        .await;
    let (status, minted) = call(router, "CreateAccessKey", &[("UserName", user)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture CreateAccessKey must succeed, got {status}: {minted}"
    );
    element(&minted, "AccessKeyId")
}

/// One `ChangePassword` signed by `akid`.
async fn change(
    router: &MultiServiceRouter,
    akid: &str,
    old_password: &str,
    new_password: &str,
) -> (u16, String) {
    call_signed_by(
        router,
        Some(akid),
        "ChangePassword",
        &[
            ("OldPassword", old_password),
            ("NewPassword", new_password),
        ],
    )
    .await
}

/// Assert that a change was refused as a password-policy violation, with the status, the code
/// and the whole sentence AWS words it with — and nothing of the account or of the request in
/// that sentence.
fn assert_reuse_refusal(case: &str, status: u16, body: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status}: {body}"
    );
    assert_eq!(
        error_code(body),
        VIOLATION_CODE,
        "case {case:?}: expected the error code {VIOLATION_CODE}, got {:?}: {body}",
        error_code(body)
    );
    assert_eq!(
        element(body, "Message"),
        VIOLATION_MESSAGE,
        "case {case:?}: expected the fixed refusal sentence {VIOLATION_MESSAGE:?}, got {:?}",
        element(body, "Message")
    );
}

/// Walk `user` through `changes` changes from the password the create set, answering every
/// password the account held, oldest first.
async fn walk(
    router: &MultiServiceRouter,
    case: &str,
    akid: &str,
    changes: usize,
) -> Vec<String> {
    let mut held = vec![password(0)];
    for step in 1..=changes {
        let next = password(step);
        let (status, body) = change(router, akid, held.last().expect("a password in force"), &next)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture change {step} must be served, got {status}: {body}"
        );
        held.push(next);
    }
    held
}

/// RULE: a change to a password the account's reuse window still covers is refused with the
/// modelled violation and changes nothing; a change to a password outside that window — or
/// any change at all under an account that set no window — is served and the password it
/// names is the one in force afterwards.
#[tokio::test]
async fn the_reuse_window_decides_which_earlier_password_a_user_may_return_to() {
    for row in rows() {
        let router = router();
        set_window(&router, row.case, row.window).await;
        let akid = user_with_password(&router, row.case, "walker", &password(0)).await;
        let held = walk(&router, row.case, &akid, row.changes).await;
        let in_force = held.last().expect("a password in force").clone();
        let returning_to = held[held.len() - 1 - row.depth].clone();

        let (status, body) = change(&router, &akid, &in_force, &returning_to).await;

        // Which password is in force after the attempt: the one the row returned to when the
        // account admitted it, and the one that was in force when it did not. Proved by a
        // further change that presents it, which is served only for the password in force.
        let expected_in_force = if row.refused {
            assert_reuse_refusal(row.case, status, &body);
            in_force
        } else {
            assert_eq!(
                status, 200,
                "case {:?}: expected the change to be served, got {status}: {body}",
                row.case
            );
            returning_to
        };
        let (status, body) = change(&router, &akid, &expected_in_force, FRESH_PASSWORD).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected {} to be the password in force after the attempt, got \
             {status}: {body}",
            row.case,
            if row.refused {
                "the password the refused change did not replace"
            } else {
                "the password the served change set"
            }
        );
    }
}

/// RULE: the window is decided against ONE user's own passwords, so a password another user
/// of the same account used is not in it.
#[tokio::test]
async fn the_reuse_window_covers_only_the_calling_user_s_own_passwords() {
    let router = router();
    set_window(&router, "per user", Some(Some("24"))).await;
    let first = user_with_password(&router, "per user", "walker", &password(0)).await;
    let second = user_with_password(&router, "per user", "neighbour", FRESH_PASSWORD).await;
    walk(&router, "per user", &first, 2).await;

    // The neighbour takes a password the first user had two changes ago, which the first
    // user's own window covers.
    let (status, body) = change(&router, &second, FRESH_PASSWORD, &password(0)).await;
    assert_eq!(
        status, 200,
        "expected another user's history to bar nothing, got {status}: {body}"
    );
    // And the first user is still barred from it, so the window did not move with the
    // neighbour's change either.
    let (status, body) = change(&router, &first, &password(2), &password(0)).await;
    assert_reuse_refusal("per user", status, &body);
}

/// RULE: the window covers every password the account held for the user, whichever write
/// superseded it — so a password an administrative `UpdateLoginProfile` replaced is one the
/// user may not return to.
#[tokio::test]
async fn a_password_an_administrative_update_superseded_is_inside_the_window() {
    for (case, window, refused) in [
        ("a window of two covers the superseded password", Some("2"), true),
        ("no window covers nothing", None, false),
    ] {
        let router = router();
        set_window(&router, case, Some(window)).await;
        let akid = user_with_password(&router, case, "walker", &password(0)).await;
        let (status, body) = call(
            &router,
            "UpdateLoginProfile",
            &[("UserName", "walker"), ("Password", &password(1))],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the fixture UpdateLoginProfile must succeed, got {status}: {body}"
        );

        let (status, body) = change(&router, &akid, &password(1), &password(0)).await;

        if refused {
            assert_reuse_refusal(case, status, &body);
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the change to be served, got {status}: {body}"
            );
        }
    }
}
