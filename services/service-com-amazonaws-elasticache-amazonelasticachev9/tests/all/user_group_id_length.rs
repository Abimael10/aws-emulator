// SPDX-License-Identifier: CC0-1.0

//! WHEN `CreateUserGroup` applies the `UserGroupId` bound the service adds to the modelled
//! constraints, over the wire.
//!
//! The rule under test: a bound this service enforces that the API does not state — neither
//! as a constraint trait nor in the member's own documentation — is judged by a CREATE LAST,
//! after every rule the API models for that request, so a request that breaks a modelled rule
//! is refused with that rule's own error class and the caller is told which rule it broke. An
//! operation that merely RESOLVES such an identifier keeps applying the bound first: there is
//! no other rule for it to reach, and the identifier is all it was given to judge.
//!
//! `UserGroupId` is such a member: it is documented only as "The ID of the user group. This
//! value is stored as a lowercase string."
//! (`smithy_query(query="doc",shape="CreateUserGroupMessage",member="UserGroupId")`) and
//! carries neither a `@length` maximum nor a `@pattern`, so the 40-character bound the
//! service refuses a longer id with is the service's own. Contrast `ReplicationGroupId`,
//! whose constraints the member's documentation states ("A name must contain from 1 to 40
//! alphanumeric characters or hyphens"): that one is part of the published contract, and the
//! creates that store it judge it up front — which is why no row here is about it. The same
//! rule on this service's other such member, `UserId`, is
//! [`crate::rbac_id_bound_precedence`].
//!
//! The table is two-dimensional. One axis is how long the id is relative to the bound (well
//! under it, one under it, at it, one past it, well past it, several times past it, and a
//! form carrying digits and hyphens); the other is whether the request also breaks the rule
//! the model states as its own error class for this operation — the membership rule
//! `DefaultUserRequired` states. A request that breaks it must get that error class whatever
//! the id's length; a request that breaks none must get the service's own bound once past it,
//! and must round-trip when within it.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::harness::{assert_refusal, params, Emulator, Reply};
use crate::user_group_membership::create_user;

/// The sentence ElastiCache refuses a user group without the built-in user with, spelled out
/// rather than read from the constant the handlers share: fixed text for the whole error
/// class.
const DEFAULT_USER_REQUIRED: &str =
    "Redis user group needs to contain a user with the user name default.";

/// The engine the groups below are listed for, from the published set (`state::USER_ENGINES`).
pub const ENGINE: &str = "redis";

/// A member the rows put in a group beside the built-in user.
const MEMBER: &str = "member-one";

/// An identifier of exactly `length` characters that begins with a letter and carries only
/// letters, so the only thing that distinguishes one row from another is how long it is.
fn letters(length: usize) -> String {
    format!("g{}", "a".repeat(length - 1))
}

/// The axis both this suite and [`crate::rbac_id_bound_precedence`] walk: how long the
/// identifier is relative to `bound`, the bound the service adds to the modelled
/// constraints, and which characters it is made of.
pub fn identifiers(bound: usize) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = [
        ("well under the bound", letters(12)),
        ("one character under the bound", letters(bound - 1)),
        ("at the bound", letters(bound)),
        ("one character past the bound", letters(bound + 1)),
        ("well past the bound", letters(bound + 9)),
        ("three times the bound", letters(bound * 3)),
    ]
    .into_iter()
    .map(|(case, id)| (case.to_string(), id))
    .collect();
    // A form rather than a length: letters, digits and hyphens, past the bound.
    rows.push((
        "past the bound, carrying digits and hyphens".to_string(),
        format!("group-{}-9", "a-1".repeat(14)),
    ));
    rows
}

/// Assert `reply`'s message says nothing about `value`: a sentence that states a rule is
/// fixed text, so no part of the request appears in it.
pub fn assert_states_only_the_rule(case: &str, reply: &Reply, value: &str) {
    assert!(
        !reply.error_message().contains(value),
        "case {case:?}: the sentence states the rule, so it carries no request value, got {:?}",
        reply.error_message()
    );
}

/// `CreateUserGroup` for `id` out of `members`, whatever it answers.
async fn create_user_group(emulator: &Emulator, id: &str, members: &[&str]) -> Reply {
    let mut request = params(&[("UserGroupId", id), ("Engine", ENGINE)]);
    for (index, member) in members.iter().enumerate() {
        request.push((format!("UserIds.member.{}", index + 1), (*member).to_string()));
    }
    emulator.call("CreateUserGroup", &request).await
}

/// `DescribeUserGroups` for `id`, whatever it answers.
async fn read_user_group(emulator: &Emulator, id: &str) -> Reply {
    emulator
        .call("DescribeUserGroups", &params(&[("UserGroupId", id)]))
        .await
}

#[tokio::test]
async fn the_membership_rule_answers_a_create_whatever_the_group_ids_length() {
    let bound = state::MAX_USER_ID_LEN;
    for (case, id) in identifiers(bound) {
        let emulator = Emulator::new();
        create_user(&emulator, &case, MEMBER).await;

        // The modelled rule the request breaks, answered whatever the id's length: the
        // service's own bound does not pre-empt it, so a client branching on the error code
        // sees the refusal it has to handle.
        let refused = create_user_group(&emulator, &id, &[MEMBER]).await;
        assert_refusal(
            &case,
            &refused,
            400,
            "DefaultUserRequired",
            DEFAULT_USER_REQUIRED,
        );
        assert_states_only_the_rule(&case, &refused, &id);
        // The refusal stored nothing, so the id is still free.
        let described = read_user_group(&emulator, &id).await;
        assert_eq!(
            (described.status, described.error_code()),
            (404, "UserGroupNotFound"),
            "case {case:?}: the refused create must store nothing, got {} {}",
            described.status,
            described.body
        );

        // The same request with the modelled rule satisfied: now the service's OWN bound is
        // all that is left to judge, so an id within it is stored and one past it is refused
        // with the sentence that states the bound.
        let answered = create_user_group(&emulator, &id, &[state::DEFAULT_USER_ID, MEMBER]).await;
        if id.chars().count() > bound {
            assert_refusal(
                &case,
                &answered,
                400,
                "InvalidParameterValue",
                &format!("User group id length must be less than {bound}."),
            );
            assert_states_only_the_rule(&case, &answered, &id);
            let missing = read_user_group(&emulator, &id).await;
            assert_eq!(
                (missing.status, missing.error_code()),
                (404, "UserGroupNotFound"),
                "case {case:?}: the refused create must store nothing, got {} {}",
                missing.status,
                missing.body
            );
            continue;
        }
        assert_eq!(
            answered.status, 200,
            "case {case:?}: an id within the bound must be stored, got {} {}",
            answered.status, answered.body
        );
        // Read back through the implemented reader: the create's answer is the stored group.
        let stored = read_user_group(&emulator, &id).await;
        assert_eq!(
            (stored.status, stored.text("UserGroupId")),
            (200, Some(id.as_str())),
            "case {case:?}: the stored group must be readable, got {} {}",
            stored.status,
            stored.body
        );
        assert_eq!(
            stored.all("UserIds/member"),
            vec![state::DEFAULT_USER_ID, MEMBER],
            "case {case:?}: the read must report the stored membership, got {}",
            stored.body
        );
    }
}

#[tokio::test]
async fn the_operations_that_only_resolve_a_group_id_still_judge_it_first() {
    // The other half of the rule: an operation whose request is the identifier and nothing
    // else applies the bound BEFORE the lookup, so a caller that sent an id the service
    // would never have stored is told which member is wrong rather than that the group is
    // absent — which is the opposite conclusion for create-if-missing and cleanup logic.
    let bound = state::MAX_USER_ID_LEN;
    let refusal = format!("User group id length must be less than {bound}.");
    for (case, id) in identifiers(bound) {
        let emulator = Emulator::new();
        for (action, extra) in [
            ("ModifyUserGroup", &[("UserIdsToAdd.member.1", "default")][..]),
            ("DeleteUserGroup", &[][..]),
        ] {
            let mut request = params(&[("UserGroupId", id.as_str())]);
            request.extend(params(extra));
            let answered = emulator.call(action, &request).await;
            let row = format!("{case} / {action}");
            if id.chars().count() > bound {
                assert_refusal(&row, &answered, 400, "InvalidParameterValue", &refusal);
                assert_states_only_the_rule(&row, &answered, &id);
                continue;
            }
            // Within the bound the lookup runs, and the group this account does not hold is
            // reported absent, with the id the request named.
            assert_refusal(
                &row,
                &answered,
                404,
                "UserGroupNotFound",
                &format!("User group {id} not found."),
            );
        }
    }
}
