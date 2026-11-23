// SPDX-License-Identifier: CC0-1.0

//! The two CLASSES a refused pass of a caller-supplied role comes in, over the one
//! operation that is handed one.
//!
//! RULE: a role an identity pool is configured with is a role the caller PASSES to Amazon
//! Cognito, so it has to name an IAM role of the account the service acts for — and WHICH
//! refusal a value that does not is follows what the value NAMES, because the two are fixed
//! in different places:
//!
//! * a WELL-FORMED IAM role ARN whose account is not this one is a pass this account may not
//!   make — `AccessDeniedException`, the AWS-common code, with the fixed pass-role sentence,
//!   which a caller acts on in IAM;
//! * a value that names NO IAM role at all reaches no role to pass — `NotAuthorizedException`
//!   stating that access to the role the request named is forbidden, quoting the value back,
//!   which a caller acts on in the request.
//!
//! Either way the write is refused before anything is stored, so the pool keeps the role
//! configuration it had — read back through `GetIdentityPoolRoles`, which is what a client
//! sees afterwards.
//!
//! The rows span what the rule generalises over: WHICH of the two classes the value falls
//! in, HOW it falls in it (another account's role at the account root and under a path; a
//! value that is no ARN, an ARN of another vendor, an IAM ARN naming another entity type, an
//! IAM ARN carrying a Region, a role ARN with no role name, an empty value), WHICH key of
//! the `Roles` map carries it, and the state the pool is in when the write arrives (one with
//! no roles configured, and one already carrying both).

use serde_json::{json, Value};
use service_com_amazonaws_cognitoidentity_awscognitoidentityservice::scaffolded::ACCESS_DENIED_STATUS;
use service_com_amazonaws_cognitoidentity_awscognitoidentityservice::state::{
    forbidden_role_message, CROSS_ACCOUNT_PASS_ROLE_MESSAGE,
};

use crate::identity_pool_flow::{error_code, Emulator};

/// The status this service answers its modelled client errors with: the error class travels
/// in the body and in the error-type header rather than in the status
/// (`crate::service::modeled_error_status`).
const CLIENT_ERROR_STATUS: u16 = 400;

/// The message an error response carries.
fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .unwrap_or_else(|| panic!("an error response carries a message: {body}"))
        .to_string()
}

/// An IAM role ARN of the account this emulator serves.
fn own_role(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// The refusal one role value draws: the status, the error code and the message.
fn refusal_for(role_arn: &str, cross_account: bool) -> (u16, &'static str, String) {
    if cross_account {
        (
            ACCESS_DENIED_STATUS,
            "AccessDeniedException",
            CROSS_ACCOUNT_PASS_ROLE_MESSAGE.to_string(),
        )
    } else {
        (
            CLIENT_ERROR_STATUS,
            "NotAuthorizedException",
            forbidden_role_message(role_arn),
        )
    }
}

/// Every value the rule classifies: the case, the value, and whether it is a well-formed
/// role ARN of ANOTHER account (the pass-role class) rather than a value naming no role.
///
/// The account id the cross-account rows name is one of the reserved documentation
/// accounts, so it can never be the account this emulator serves.
fn rows() -> Vec<(&'static str, String, bool)> {
    vec![
        (
            "another account's role",
            "arn:aws:iam::123456789012:role/pool-auth".to_string(),
            true,
        ),
        (
            "another account's role under a path",
            "arn:aws:iam::123456789012:role/service-role/pool-auth".to_string(),
            true,
        ),
        (
            "another account's role in another partition",
            "arn:aws-us-gov:iam::123456789012:role/pool-auth".to_string(),
            true,
        ),
        (
            "a value that is no ARN at all",
            "not-a-real-arn-just-a-long-garbage-string-1234567890".to_string(),
            false,
        ),
        (
            "an ARN of another vendor",
            "arn:aws:s3:::a-bucket-named-like-this".to_string(),
            false,
        ),
        (
            "an IAM ARN naming another entity type",
            format!("arn:aws:iam::{}:user/alice", engine::emulator_account_id()),
            false,
        ),
        (
            // An IAM ARN is partition-global: a value carrying a Region names no IAM
            // resource, in this account or any other.
            "an IAM role ARN carrying a Region",
            "arn:aws:iam:us-east-1:123456789012:role/pool-auth".to_string(),
            false,
        ),
        (
            "an account field that is not an account id",
            "arn:aws:iam::1234:role/pool-auth".to_string(),
            false,
        ),
        (
            "a role ARN with no role name",
            "arn:aws:iam::123456789012:role/".to_string(),
            false,
        ),
        (
            // A value shorter than `ARNString`'s modelled minimum never reaches this rule:
            // the constraint walk ahead of the handler refuses it. The shortest value the
            // rule itself decides is therefore one of twenty characters.
            "a value at the shortest length the shape admits",
            "xxxxxxxxxxxxxxxxxxxx".to_string(),
            false,
        ),
    ]
}

/// RULE, first half: each class of value draws its own refusal, under either `Roles` key and
/// whatever the pool already holds.
#[tokio::test]
async fn a_role_value_is_refused_in_the_class_of_what_it_names() {
    let emulator = Emulator::new();
    // Two pools: one carrying no role configuration, and one already carrying both roles,
    // so the classification does not depend on what the pool holds.
    let empty_pool = emulator.pool("pass-role-class-empty", true).await;
    let configured_pool = emulator.pool("pass-role-class-configured", true).await;
    emulator
        .ok(
            "SetIdentityPoolRoles",
            &json!({
                "IdentityPoolId": &configured_pool,
                "Roles": {
                    "authenticated": own_role("stored-auth"),
                    "unauthenticated": own_role("stored-unauth"),
                },
            }),
        )
        .await;

    for (case, role_arn, cross_account) in rows() {
        let (expected_status, expected_code, expected_message) =
            refusal_for(&role_arn, cross_account);

        for role_type in ["authenticated", "unauthenticated"] {
            for (pool_named, identity_pool_id) in [
                ("a pool with no roles", &empty_pool),
                ("a pool already carrying both roles", &configured_pool),
            ] {
                let request = json!({
                    "IdentityPoolId": identity_pool_id,
                    "Roles": { role_type: &role_arn },
                });
                let (status, body) = emulator.call("SetIdentityPoolRoles", &request).await;
                assert_eq!(
                    (status, error_code(&body).as_str()),
                    (expected_status, expected_code),
                    "case {case:?} at {role_type} on {pool_named}: expected \
                     {expected_status} {expected_code}, got {status} with {body}"
                );
                assert_eq!(
                    error_message(&body),
                    expected_message,
                    "case {case:?} at {role_type} on {pool_named}: expected \
                     {expected_message:?}, got {body}"
                );
            }
        }
    }
}

/// RULE, second half: a refused pass stores nothing, so the pool answers afterwards with
/// exactly the role configuration it had before the refusal.
#[tokio::test]
async fn a_refused_pass_leaves_the_stored_role_configuration_alone() {
    let emulator = Emulator::new();
    let empty_pool = emulator.pool("pass-role-keeps-empty", true).await;
    let configured_pool = emulator.pool("pass-role-keeps-configured", true).await;
    let stored = json!({
        "authenticated": own_role("kept-auth"),
        "unauthenticated": own_role("kept-unauth"),
    });
    emulator
        .ok(
            "SetIdentityPoolRoles",
            &json!({ "IdentityPoolId": &configured_pool, "Roles": &stored }),
        )
        .await;

    // (the pool, what it must still report afterwards)
    let pools: [(&str, &String, Value); 2] = [
        ("a pool with no roles", &empty_pool, json!({})),
        (
            "a pool already carrying both roles",
            &configured_pool,
            stored.clone(),
        ),
    ];

    for (case, role_arn, _cross_account) in rows() {
        for (pool_named, identity_pool_id, expected) in &pools {
            let (status, _body) = emulator
                .call(
                    "SetIdentityPoolRoles",
                    &json!({
                        "IdentityPoolId": identity_pool_id,
                        "Roles": { "authenticated": &role_arn },
                    }),
                )
                .await;
            assert_ne!(
                status, 200,
                "case {case:?} on {pool_named}: the pass must be refused"
            );
            let read = emulator
                .ok(
                    "GetIdentityPoolRoles",
                    &json!({ "IdentityPoolId": identity_pool_id }),
                )
                .await;
            assert_eq!(
                read["IdentityPoolId"].as_str(),
                Some(identity_pool_id.as_str()),
                "case {case:?} on {pool_named}: the read must answer for the pool asked \
                 about, got {read}"
            );
            let reported = read["Roles"].clone();
            let reported = if reported.is_null() {
                json!({})
            } else {
                reported
            };
            assert_eq!(
                reported, *expected,
                "case {case:?} on {pool_named}: a refused write must leave the stored roles \
                 alone, expected {expected}, got {read}"
            );
        }
    }
}

/// RULE, the admitted side: a value that DOES name an IAM role of this account is stored and
/// read back, so the rule refuses only what it must.
#[tokio::test]
async fn a_role_of_this_account_is_stored_and_read_back() {
    let emulator = Emulator::new();
    let identity_pool_id = emulator.pool("pass-role-admitted", true).await;

    // (case, the role value the write sends)
    let admitted: &[(&str, String)] = &[
        ("a role at the account root", own_role("pool-auth")),
        (
            "a role under a path",
            own_role("service-role/pool-auth"),
        ),
        (
            // An IAM ARN's account field is optional in several modelled patterns, and a
            // value naming no account names no OTHER account either.
            "a role ARN naming no account",
            "arn:aws:iam:::role/pool-auth".to_string(),
        ),
    ];

    for (case, role_arn) in admitted {
        emulator
            .ok(
                "SetIdentityPoolRoles",
                &json!({
                    "IdentityPoolId": &identity_pool_id,
                    "Roles": { "authenticated": role_arn },
                }),
            )
            .await;
        let read = emulator
            .ok(
                "GetIdentityPoolRoles",
                &json!({ "IdentityPoolId": &identity_pool_id }),
            )
            .await;
        assert_eq!(
            read["Roles"]["authenticated"].as_str(),
            Some(role_arn.as_str()),
            "case {case:?}: the stored role must be the one the write named, got {read}"
        );
    }
}
