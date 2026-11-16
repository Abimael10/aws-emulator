// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a `CreateUser` of a username the user pool already holds
//! answers, whatever else the pool holds and whatever has happened to the held user.
//!
//! Every case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rule under test: a `(UserName, AuthenticationType)` identity is allocated AT MOST
//! ONCE, so a `CreateUser` naming an identity the pool already holds a record for is
//! refused `ResourceAlreadyExistsException` (HTTP 400) with AWS's sentence for a username
//! that is taken, and leaves the pool exactly as it was.
//!
//! The sibling suite [`crate::user_identity_allocation`] spans that rule over the FORM of
//! the name and over what the repeat create carries, always against a pool that holds
//! exactly the one freshly created user — and this suite reuses its request builder, its
//! pool read and its sentence rather than restating them. What it adds are the two
//! dimensions that suite does not span: what ELSE the pool holds (so the lookup finds the
//! identity wherever it sits in the stored order, and refusing it touches no other user)
//! and what has happened to the held user since it was created (created, disabled,
//! re-enabled, entitled to stacks), since none of those histories frees the username. It
//! also pins that the lookup is an EXACT identity match: a name that merely resembles a
//! held one is still free.

use serde_json::{json, Value};

use crate::refusal_messages::refusal;
use crate::stack_fleet_lifecycle::Emulator;
use crate::user_identity_allocation::{create_request, user_pool, ALREADY_EXISTS, USER_POOL};

/// The wire error code a create of a taken username answers, one of `CreateUser`'s
/// modelled errors (`smithy_query(query="operation", operation="CreateUser")`).
const ALREADY_EXISTS_CODE: &str = "ResourceAlreadyExistsException";

/// AWS's sentence for a username that is taken, spelled out once here. The constant the
/// assertions compare a refusal against is the one the handler writes, so stating the
/// words in the test tree is what pins the WORDING rather than the constant.
const ALREADY_EXISTS_WORDING: &str = "A user with that username already exists.";

#[test]
fn the_sentence_a_taken_username_is_refused_with_is_the_one_aws_words() {
    assert_eq!(
        ALREADY_EXISTS, ALREADY_EXISTS_WORDING,
        "case \"the taken-username sentence\": expected AWS's wording \
         {ALREADY_EXISTS_WORDING:?}, got {ALREADY_EXISTS:?}"
    );
}

/// The users the pool holds BESIDE the one under test. The pool is keyed by
/// `(AuthenticationType, UserName)`, so these names sort around the held names below and
/// put the held identity at the start, in the middle and at the end of the stored order.
const BYSTANDERS: &[&str] = &[
    "ada@example.com",
    "nikola@example.com",
    "zuzana@example.com",
];

/// What has happened to the held user by the time the duplicate create arrives: the calls
/// to make after creating it, none of which gives the username back.
struct History {
    case: &'static str,
    /// Operations taking only the identity, applied in order.
    transitions: &'static [&'static str],
    /// Stacks to entitle the held user to, so the identity is also referenced elsewhere.
    entitled_to: &'static [&'static str],
}

/// The histories a held identity can have. They differ along what the rule generalises
/// over: nothing done at all, the sign-in flag moved away, moved away and back, and the
/// identity referenced by other records of the account.
const HISTORIES: &[History] = &[
    History {
        case: "as created",
        transitions: &[],
        entitled_to: &[],
    },
    History {
        case: "disabled",
        transitions: &["DisableUser"],
        entitled_to: &[],
    },
    History {
        case: "disabled then enabled again",
        transitions: &["DisableUser", "EnableUser"],
        entitled_to: &[],
    },
    History {
        case: "entitled to two stacks",
        transitions: &[],
        entitled_to: &["duplicate-stack-a", "duplicate-stack-b"],
    },
];

/// One row of the pool table: the username under test and whether the pool holds the
/// bystanders too, which is what moves the held identity through the stored order.
struct PoolShape {
    case: &'static str,
    held: &'static str,
    with_bystanders: bool,
}

/// The pool table: the held identity alone, then first, in the middle and last in the
/// stored order of a pool that holds other users as well.
const POOL_SHAPES: &[PoolShape] = &[
    PoolShape {
        case: "the only user in the pool",
        held: "solo@example.com",
        with_bystanders: false,
    },
    PoolShape {
        case: "first in the stored order",
        held: "aaron@example.com",
        with_bystanders: true,
    },
    PoolShape {
        case: "in the middle of the stored order",
        held: "marie@example.com",
        with_bystanders: true,
    },
    PoolShape {
        case: "last in the stored order",
        held: "zz-last@example.com",
        with_bystanders: true,
    },
];

#[tokio::test]
async fn a_held_username_is_taken_wherever_it_sits_and_whatever_happened_to_it() {
    for shape in POOL_SHAPES {
        for history in HISTORIES {
            let case = format!("{}, {}", shape.case, history.case);
            let emulator = Emulator::new();

            if shape.with_bystanders {
                for bystander in BYSTANDERS {
                    emulator
                        .ok(
                            &case,
                            "CreateUser",
                            create_request(bystander, &json!({ "FirstName": "Bystander" })),
                        )
                        .await;
                }
            }
            emulator
                .ok(
                    &case,
                    "CreateUser",
                    create_request(shape.held, &json!({ "FirstName": "Held" })),
                )
                .await;

            for operation in history.transitions {
                emulator
                    .ok(
                        &case,
                        operation,
                        json!({ "UserName": shape.held, "AuthenticationType": USER_POOL }),
                    )
                    .await;
            }
            for stack in history.entitled_to {
                emulator
                    .ok(&case, "CreateStack", json!({ "Name": stack }))
                    .await;
                emulator
                    .ok(
                        &case,
                        "BatchAssociateUserStack",
                        json!({
                            "UserStackAssociations": [{
                                "UserName": shape.held,
                                "AuthenticationType": USER_POOL,
                                "StackName": stack,
                                "SendEmailNotification": false,
                            }]
                        }),
                    )
                    .await;
            }

            let before = user_pool(&emulator, &case).await;
            let expected_users = if shape.with_bystanders {
                BYSTANDERS.len() + 1
            } else {
                1
            };
            assert_eq!(
                before.len(),
                expected_users,
                "case {case:?}: the setup must have allocated {expected_users} identities, \
                 got {before:?}"
            );

            // Claiming the held username is refused, twice over: the refusal is the state
            // of the pool, not a one-shot answer to the first repeat. What such a repeat
            // may carry beside the identity is spanned by `user_identity_allocation`, so
            // here it carries the identity alone.
            for attempt in 1..=2 {
                let message = refusal(
                    &emulator,
                    &case,
                    "CreateUser",
                    create_request(shape.held, &json!({})),
                    400,
                    ALREADY_EXISTS_CODE,
                )
                .await;
                assert_eq!(
                    message, ALREADY_EXISTS,
                    "case {case:?}, attempt {attempt}: expected AWS's sentence \
                     {ALREADY_EXISTS:?}, got {message:?}"
                );
                assert!(
                    !message.contains(shape.held),
                    "case {case:?}, attempt {attempt}: the sentence states that the \
                     username is taken without echoing it, got {message:?}"
                );
            }

            let after = user_pool(&emulator, &case).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused create must leave every stored user exactly as \
                 it was, got {after:?}"
            );

            // The refusal is about the one identity it names: a username the pool does
            // not hold is still allocatable afterwards.
            emulator
                .ok(
                    &case,
                    "CreateUser",
                    create_request("free@example.com", &json!({})),
                )
                .await;
            let grown = user_pool(&emulator, &case).await;
            assert_eq!(
                grown.len(),
                before.len() + 1,
                "case {case:?}: a free username must still be allocatable after the \
                 refusal, got {grown:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_username_that_merely_resembles_a_held_one_is_still_free() {
    // Rows: names that are NOT the held identity but share text with it — a prefix, a
    // suffix, the name with a character appended, and a name the held one contains.
    // (Another capitalisation of a held name is the same rule, asserted in
    // `user_identity_allocation`.) The identity is the whole name, so each of these is its
    // own free username.
    const HELD: &str = "stored.user@example.com";
    for (row, candidate) in [
        ("a prefix of the held name", "stored.user@example.co"),
        ("a suffix of the held name", "user@example.com"),
        (
            "the held name with a character appended",
            "stored.user@example.comx",
        ),
        ("a name the held one contains", "stored.user"),
    ] {
        let case = format!("{row}: {candidate}");
        let emulator = Emulator::new();
        emulator
            .ok(&case, "CreateUser", create_request(HELD, &json!({})))
            .await;

        let answer = emulator
            .ok(&case, "CreateUser", create_request(candidate, &json!({})))
            .await;
        assert_eq!(
            answer,
            json!({}),
            "case {case:?}: the modelled result carries no members, got {answer}"
        );

        let stored = user_pool(&emulator, &case).await;
        let names: Vec<&str> = stored
            .iter()
            .filter_map(|user| user.get("UserName").and_then(Value::as_str))
            .collect();
        assert_eq!(
            names.len(),
            2,
            "case {case:?}: each name is its own identity, got {stored:?}"
        );
        assert!(
            names.contains(&HELD) && names.contains(&candidate),
            "case {case:?}: the pool must hold both {HELD:?} and {candidate:?}, got {names:?}"
        );

        // And the held name itself is still taken.
        let message = refusal(
            &emulator,
            &case,
            "CreateUser",
            create_request(HELD, &json!({})),
            400,
            ALREADY_EXISTS_CODE,
        )
        .await;
        assert_eq!(
            message, ALREADY_EXISTS,
            "case {case:?}: expected AWS's sentence {ALREADY_EXISTS:?}, got {message:?}"
        );
    }
}
