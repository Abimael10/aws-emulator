// SPDX-License-Identifier: CC0-1.0

//! WHEN `CreateUser` applies the `UserId` bound the service adds to the modelled
//! constraints, over the wire.
//!
//! The rule is the one [`crate::user_group_id_length`] states for the sibling member of this
//! service's other RBAC identifier: a bound this service enforces that the API does not state
//! — neither as a constraint trait nor in the member's own documentation — is judged by a
//! CREATE LAST, after every rule the API models for that request, so a request that breaks a
//! modelled rule is refused with that rule's own error class. An operation that merely
//! RESOLVES such an identifier keeps applying the bound first.
//!
//! `UserId` is such a member: it is documented only as "The ID of the user. This value is
//! stored as a lowercase string."
//! (`smithy_query(query="doc",shape="CreateUserMessage",member="UserId")`) and is constrained
//! with `@length(min: 1)` and a pattern only, so the 40-character maximum the service refuses
//! a longer id with is the service's own.
//!
//! The axis is the shared one — how long the id is relative to that bound, and which
//! characters it is made of — crossed with whether the request also breaks a rule the model
//! states as its own error class: here the mutually exclusive ways of saying how a user
//! authenticates, which the model gives its own `InvalidParameterCombinationException`.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{assert_refusal, params, Emulator, Reply};
use crate::user_group_id_length::{assert_states_only_the_rule, identifiers, ENGINE};

/// The sentence this service refuses a request that says two different things about how a
/// user authenticates with.
const CONFLICTING_AUTHENTICATION: &str =
    "Passwords cannot be supplied when NoPasswordRequired is true.";

/// A password within the published rule (16 to 128 characters), so a row that supplies one
/// is refused for the combination it names rather than for the password itself.
const PASSWORD: &str = "aCorrectHorseBattery1";

/// The ACL rule list every row below asks for, which the engine publishes.
const ACCESS_STRING: &str = "on ~* +@all";

/// `CreateUser` for `id`, saying how it authenticates the way `authentication` asks.
async fn create_user(
    emulator: &Emulator,
    id: &str,
    authentication: &[(&str, &str)],
) -> Reply {
    let mut request = params(&[
        ("UserId", id),
        ("UserName", id),
        ("Engine", ENGINE),
        ("AccessString", ACCESS_STRING),
    ]);
    request.extend(params(authentication));
    emulator.call("CreateUser", &request).await
}

#[tokio::test]
async fn the_modelled_combination_rule_answers_a_create_whatever_the_user_ids_length() {
    let bound = state::MAX_USER_ID_LEN;
    for (case, id) in identifiers(bound) {
        let emulator = Emulator::new();

        // Two statements of how the user authenticates — a rule the model gives its own
        // error class — answered whatever the id's length, so the service's own bound does
        // not hand the caller an error code its request did not earn.
        let refused = create_user(
            &emulator,
            &id,
            &[("NoPasswordRequired", "true"), ("Passwords.member.1", PASSWORD)],
        )
        .await;
        assert_refusal(
            &case,
            &refused,
            400,
            "InvalidParameterCombination",
            CONFLICTING_AUTHENTICATION,
        );
        assert_states_only_the_rule(&case, &refused, &id);

        // The same request with the modelled rules satisfied: the service's OWN bound is all
        // that is left to judge.
        let answered = create_user(&emulator, &id, &[("NoPasswordRequired", "true")]).await;
        if id.chars().count() > bound {
            assert_refusal(
                &case,
                &answered,
                400,
                "InvalidParameterValue",
                &format!("User id length must be less than {bound}."),
            );
            assert_states_only_the_rule(&case, &answered, &id);
            continue;
        }
        assert_eq!(
            answered.status, 200,
            "case {case:?}: an id within the bound must be stored, got {} {}",
            answered.status, answered.body
        );
        // Read back through the implemented reader: the create's answer is the stored user,
        // and the refused request above left no second user holding the id.
        let stored = emulator
            .call("DescribeUsers", &params(&[("UserId", id.as_str())]))
            .await;
        assert_eq!(
            (stored.status, stored.text("UserId")),
            (200, Some(id.as_str())),
            "case {case:?}: the stored user must be readable, got {} {}",
            stored.status,
            stored.body
        );
        assert_eq!(
            stored.all("UserId").len(),
            1,
            "case {case:?}: exactly one user must hold the id, got {}",
            stored.body
        );
    }
}

#[tokio::test]
async fn the_operation_that_only_resolves_a_user_id_still_judges_it_first() {
    // The other half of the rule, on this member: a delete whose request is the identifier
    // and nothing else applies the bound BEFORE the lookup, so an id the service would never
    // have stored is refused as the bad parameter it is rather than reported already gone.
    let bound = state::MAX_USER_ID_LEN;
    let refusal = format!("User id length must be less than {bound}.");
    for (case, id) in identifiers(bound) {
        let emulator = Emulator::new();
        let answered = emulator
            .call("DeleteUser", &params(&[("UserId", id.as_str())]))
            .await;
        if id.chars().count() > bound {
            assert_refusal(&case, &answered, 400, "InvalidParameterValue", &refusal);
            assert_states_only_the_rule(&case, &answered, &id);
            continue;
        }
        // Within the bound the lookup runs, and the user this account does not hold is
        // reported absent.
        assert_eq!(
            (answered.status, answered.error_code()),
            (404, "UserNotFound"),
            "case {case:?}: the lookup must run for an id within the bound, got {} {}",
            answered.status,
            answered.body
        );
    }
}
