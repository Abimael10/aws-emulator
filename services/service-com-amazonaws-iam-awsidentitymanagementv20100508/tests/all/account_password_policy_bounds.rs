// SPDX-License-Identifier: CC0-1.0

//! The published ranges of the account password policy's numeric settings, end to end over
//! the awsQuery wire.
//!
//! RULE: every numeric member `UpdateAccountPasswordPolicy` takes carries the range its
//! modelled type publishes, so a value at either published bound is accepted and the policy
//! the account then holds reports exactly that value, while a value one past either bound is
//! refused HTTP 400 `ValidationError` — naming the member in its wire (lower-camel) spelling
//! and the bound it broke — and the write is REJECTED WHOLE, leaving the policy already in
//! force untouched because this operation "does not support partial updates".
//!
//! The ranges are read from the model, not from one request:
//! `smithy_query(query="shape", shape="minimumPasswordLengthType,maxPasswordAgeType,\
//! passwordReusePreventionType")` publishes 6..128, 1..1095 and 1..24 respectively, and
//! `smithy_query(query="operation", operation="UpdateAccountPasswordPolicy")` is what binds
//! each of them to its member. Each row below is one of those members, taken at each of its
//! bounds and one past each; no single value is the specification.
//!
//! What the policy holds and what the read reports member for member is
//! `tests/all/account_password_policy.rs`, whose read and reader this suite reuses; the
//! clause a refused bound is worded with belongs to `tests/all/range_refusal_wording.rs`,
//! whose builders state it here too, so a change of that wording is made in one place.
//!
//! The awsQuery harness (`router`, `call`, `element`, `error_code`) is the one
//! `tests/all/user_lifecycle.rs` owns and every suite in this crate shares.

use smithy_server_runtime::MultiServiceRouter;

use crate::account_password_policy::{
    MAX_MINIMUM_PASSWORD_LENGTH, MIN_MINIMUM_PASSWORD_LENGTH, member, read_policy,
};
use crate::range_refusal_wording::{above_value, below_value};
use crate::user_lifecycle::{call, element, error_code, router};

/// The awsQuery error code the framework refuses a value outside a published range with:
/// `ValidationError`, carried with HTTP 400.
const VALUE_REFUSAL_CODE: &str = "ValidationError";

/// One numeric member of the password policy, with the range its modelled type publishes.
struct Ranged {
    /// The member as `UpdateAccountPasswordPolicy` names it on the wire.
    member: &'static str,
    /// The same member in the lower-camel spelling the refusal names it in.
    wire_member: &'static str,
    /// The type the member targets, for the case names.
    modelled_type: &'static str,
    /// The published floor of that type's range.
    min: i64,
    /// The published ceiling of that type's range.
    max: i64,
}

/// The rows: every member of `UpdateAccountPasswordPolicyRequest` whose target publishes a
/// range, with the bounds that model states for it.
const RANGED_MEMBERS: &[Ranged] = &[
    Ranged {
        member: "MinimumPasswordLength",
        wire_member: "minimumPasswordLength",
        modelled_type: "minimumPasswordLengthType",
        // The bounds of this member's range as the sibling suite reads them from the model.
        min: MIN_MINIMUM_PASSWORD_LENGTH as i64,
        max: MAX_MINIMUM_PASSWORD_LENGTH as i64,
    },
    Ranged {
        member: "MaxPasswordAge",
        wire_member: "maxPasswordAge",
        modelled_type: "maxPasswordAgeType",
        min: 1,
        max: 1095,
    },
    Ranged {
        member: "PasswordReusePrevention",
        wire_member: "passwordReusePrevention",
        modelled_type: "passwordReusePreventionType",
        min: 1,
        max: 24,
    },
];

/// One `UpdateAccountPasswordPolicy` setting `member` to `value` and nothing else.
async fn write_one(router: &MultiServiceRouter, member: &str, value: &str) -> (u16, String) {
    call(router, "UpdateAccountPasswordPolicy", &[(member, value)]).await
}

/// RULE (the served half): a value at either published bound of a member's range is
/// accepted, and the policy the account then holds reports that exact value.
#[tokio::test]
async fn each_published_bound_of_every_ranged_member_is_served_and_read_back() {
    for ranged in RANGED_MEMBERS {
        for (edge, value) in [
            ("the published floor", ranged.min),
            ("the published ceiling", ranged.max),
        ] {
            let case = format!(
                "{} at {edge} of {} ({value})",
                ranged.member, ranged.modelled_type
            );
            // One router per value, so the policy read back is only what this write left.
            let router = router();
            let value = value.to_string();
            let (status, body) = write_one(&router, ranged.member, &value).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the write to be served, got {status}: {body}"
            );

            let (read_status, read_body) = read_policy(&router).await;
            assert_eq!(
                read_status, 200,
                "case {case:?}: expected the configured policy to be served, got \
                 {read_status}: {read_body}"
            );
            assert_eq!(
                member(&read_body, ranged.member).as_deref(),
                Some(value.as_str()),
                "case {case:?}: expected <{}> to be {value:?}, got {read_body}",
                ranged.member
            );
        }
    }
}

/// RULE (the refused half): a value one past either published bound is refused HTTP 400
/// `ValidationError`, naming the member in its wire spelling and the bound it broke.
#[tokio::test]
async fn one_past_either_published_bound_is_refused_naming_the_member_and_the_bound() {
    for ranged in RANGED_MEMBERS {
        for (edge, value, requirement) in [
            (
                "one below the published floor",
                ranged.min - 1,
                below_value(ranged.min),
            ),
            (
                "one above the published ceiling",
                ranged.max + 1,
                above_value(ranged.max),
            ),
        ] {
            let case = format!(
                "{} at {edge} of {} ({value})",
                ranged.member, ranged.modelled_type
            );
            let router = router();
            let (status, body) = write_one(&router, ranged.member, &value.to_string()).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status}: {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, VALUE_REFUSAL_CODE,
                "case {case:?}: expected the code {VALUE_REFUSAL_CODE}, got {code:?}: {body}"
            );
            let expected_message = format!(
                "1 validation error detected: Value at '{}' failed to satisfy constraint: \
                 {requirement}",
                ranged.wire_member
            );
            let message = element(&body, "Message");
            assert_eq!(
                message, expected_message,
                "case {case:?}: expected {expected_message:?}, got {message:?}"
            );

            // Nothing was written: the account that never set a policy still holds none, so
            // the read is refused rather than answering a policy this refusal left behind.
            let (read_status, read_body) = read_policy(&router).await;
            assert_eq!(
                read_status, 404,
                "case {case:?}: expected the account to hold no policy after a refused \
                 write, got {read_status}: {read_body}"
            );
        }
    }
}

/// RULE (the whole-request half): a write carrying one value outside its member's range is
/// rejected WHOLE — the members it carried alongside that value are not applied either, and
/// the policy already in force is left exactly as the last served write left it.
#[tokio::test]
async fn a_refused_write_leaves_the_policy_already_in_force_untouched() {
    for ranged in RANGED_MEMBERS {
        for (edge, bad) in [
            ("one below the published floor", ranged.min - 1),
            ("one above the published ceiling", ranged.max + 1),
        ] {
            let case = format!("{} at {edge} ({bad})", ranged.member);
            let router = router();

            // The policy in force: every ranged member inside its own range, plus a
            // character class, so a partially applied write would show.
            let in_force = [
                ("MinimumPasswordLength", "24"),
                ("MaxPasswordAge", "90"),
                ("PasswordReusePrevention", "12"),
                ("RequireSymbols", "true"),
            ];
            let (status, body) = call(&router, "UpdateAccountPasswordPolicy", &in_force).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the fixture write must be served, got {status}: {body}"
            );
            let (status, before) = read_policy(&router).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the fixture read must be served, got {status}: {before}"
            );

            // The refused write: the out-of-range value, carried alongside settings that
            // are themselves admissible and differ from what is in force.
            let bad = bad.to_string();
            let (status, body) = call(
                &router,
                "UpdateAccountPasswordPolicy",
                &[
                    (ranged.member, bad.as_str()),
                    ("RequireNumbers", "true"),
                    ("AllowUsersToChangePassword", "true"),
                ],
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status}: {body}"
            );

            let (read_status, after) = read_policy(&router).await;
            assert_eq!(
                read_status, 200,
                "case {case:?}: expected the policy in force to still be served, got \
                 {read_status}: {after}"
            );
            for tag in [
                "MinimumPasswordLength",
                "MaxPasswordAge",
                "PasswordReusePrevention",
                "RequireSymbols",
                "RequireNumbers",
                "AllowUsersToChangePassword",
                "ExpirePasswords",
            ] {
                assert_eq!(
                    member(&after, tag),
                    member(&before, tag),
                    "case {case:?}: expected <{tag}> to be left as the served write left \
                     it ({:?}), got {:?}",
                    member(&before, tag),
                    member(&after, tag)
                );
            }
        }
    }
}
