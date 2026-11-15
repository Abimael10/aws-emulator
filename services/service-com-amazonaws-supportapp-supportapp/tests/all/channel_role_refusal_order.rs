// SPDX-License-Identifier: CC0-1.0

//! ONE answer for one `channelRoleArn`, on every operation that takes the member.
//!
//! THE RULE UNDER TEST: `channelRoleArn` has two rules, and they are judged in one order —
//! the member's declared `@length` maximum is a caller-input mistake and is settled first
//! (400 `ValidationException`, in the standard validation envelope), and only a value this
//! API can carry reaches the pass-role attempt that judges which role it names (403
//! `AccessDeniedException`). Both operations that take the member therefore answer the SAME
//! status, the same modelled error class and the same message byte for byte for the same
//! value, including a value that breaks BOTH rules, where the order is what decides the
//! class.
//!
//! That is what makes this suite's shape a COMPARISON rather than two tables: each row is
//! submitted to `CreateSlackChannelConfiguration` and to
//! `UpdateSlackChannelConfiguration` and the two answers are required to agree, so an
//! operation that judged the member's rules in its own order — or skipped one of them —
//! fails here however self-consistent it is. The rows span the value's size (at the
//! published maximum, one character past it), whose account it names (the caller's, a
//! foreign one), and its form (an IAM role ARN, no ARN at all, a role name carrying a
//! character the member's `@pattern` cannot match), so each of the two rules is the
//! deciding one on some row and both are broken on others.
//!
//! What each class means on its own, and the whole span of forms the pass-role step
//! refuses, are covered in [`crate::channel_role_pass_role_form`],
//! [`crate::channel_role_account`] and [`crate::channel_role_name_pattern`]; this suite is
//! about the two operations answering alike.

use serde_json::{json, Value};
use service_com_amazonaws_supportapp_supportapp::state::{
    ChannelRoleArnRefusal, CROSS_ACCOUNT_PASS_ROLE_MESSAGE, MAX_CHANNEL_ROLE_ARN_LEN,
};

use crate::slack_channel_configurations::{role_arn, Emulator, Reply};

/// The `(teamId, channelId)` every row below configures.
const TEAM_ID: &str = "T012ABCDEFG";
const CHANNEL_ID: &str = "C01234A5BCD";

/// A twelve-digit account id that is not the caller's, which `channelRoleArn`'s
/// `@pattern ^arn:aws:iam::[0-9]{12}:role/(.+)$` admits all the same.
const FOREIGN_ACCOUNT: &str = "999999999999";

/// The role ARN the fixture configuration is created with, so an update row acts on a
/// stored configuration whose role is already a role of the caller's account.
fn fixture_role_arn() -> String {
    role_arn("SupportAppRole")
}

/// A `channelRoleArn` of `account` whose whole value is `length` characters long, padded in
/// the role name — the one part of the member's form that is free.
fn role_arn_of_length(account: &str, length: usize) -> String {
    let prefix = format!("arn:aws:iam::{account}:role/");
    let padding = length
        .checked_sub(prefix.chars().count())
        .expect("a row longer than the ARN prefix it pads");
    format!("{prefix}{}", "R".repeat(padding))
}

/// What a row expects BOTH operations to answer.
#[derive(Debug)]
enum Expected {
    /// The value names a role this account can pass: served, and the value is persisted.
    Served,
    /// The refusal both operations must answer, as the shared rule names it.
    Refused(ChannelRoleArnRefusal),
}

/// One row: a `channelRoleArn` and the single answer both operations owe it.
#[derive(Debug)]
struct Case {
    label: &'static str,
    submitted: String,
    expected: Expected,
}

/// The standard validation envelope's clause for a value past the member's declared
/// `@length` maximum: the value submitted, the member path in its lowerCamel spelling, and
/// the bound the crate states from that member.
fn length_clause(submitted: &str) -> String {
    format!(
        "1 validation error detected: Value '{submitted}' at 'channelRoleArn' failed to satisfy \
         constraint: Member must have length less than or equal to {MAX_CHANNEL_ROLE_ARN_LEN}"
    )
}

/// The table both operations are driven over. The rows differ in the value's size, in whose
/// account it names and in its form, so the length rule decides some rows, the pass-role
/// rule decides others, and on the rows that break both the ORDER between them is what
/// decides the class.
fn cases() -> Vec<Case> {
    let one_past = MAX_CHANNEL_ROLE_ARN_LEN + 1;
    let caller = engine::EMULATOR_ACCOUNT_ID;
    let caller_at_the_maximum = role_arn_of_length(caller, MAX_CHANNEL_ROLE_ARN_LEN);
    let caller_one_past = role_arn_of_length(caller, one_past);
    let foreign_at_the_maximum = role_arn_of_length(FOREIGN_ACCOUNT, MAX_CHANNEL_ROLE_ARN_LEN);
    let foreign_one_past = role_arn_of_length(FOREIGN_ACCOUNT, one_past);
    let not_an_arn_one_past = "n".repeat(one_past);
    vec![
        Case {
            label: "a role of the caller's account",
            submitted: role_arn("SupportAppOtherRole"),
            expected: Expected::Served,
        },
        Case {
            label: "a role of the caller's account at the published maximum",
            submitted: caller_at_the_maximum,
            expected: Expected::Served,
        },
        Case {
            label: "a foreign account's role",
            submitted: format!("arn:aws:iam::{FOREIGN_ACCOUNT}:role/SupportAppRole"),
            expected: Expected::Refused(ChannelRoleArnRefusal::AccessDenied(
                CROSS_ACCOUNT_PASS_ROLE_MESSAGE.to_string(),
            )),
        },
        Case {
            label: "a foreign account's role at the published maximum",
            submitted: foreign_at_the_maximum,
            expected: Expected::Refused(ChannelRoleArnRefusal::AccessDenied(
                CROSS_ACCOUNT_PASS_ROLE_MESSAGE.to_string(),
            )),
        },
        Case {
            // A role name carrying a line terminator, which the member's `@pattern` `.`
            // cannot match: a form the pass-role step refuses, inside the caller's own
            // account and well within the bound.
            label: "the caller's account with a role name the member's pattern cannot match",
            submitted: role_arn("Support\nAppRole"),
            expected: Expected::Refused(ChannelRoleArnRefusal::AccessDenied(
                CROSS_ACCOUNT_PASS_ROLE_MESSAGE.to_string(),
            )),
        },
        Case {
            label: "the caller's own role, one character past the maximum",
            submitted: caller_one_past.clone(),
            expected: Expected::Refused(ChannelRoleArnRefusal::Validation(length_clause(
                &caller_one_past,
            ))),
        },
        Case {
            // Both rules are broken: over-long AND naming another account. The length rule
            // is settled first, so this is the 400 and not the 403.
            label: "a foreign account's role, one character past the maximum",
            submitted: foreign_one_past.clone(),
            expected: Expected::Refused(ChannelRoleArnRefusal::Validation(length_clause(
                &foreign_one_past,
            ))),
        },
        Case {
            // Over-long and no ARN at all: the length rule is settled before the form is
            // judged, so this too is the 400.
            label: "no ARN at all, one character past the maximum",
            submitted: not_an_arn_one_past.clone(),
            expected: Expected::Refused(ChannelRoleArnRefusal::Validation(length_clause(
                &not_an_arn_one_past,
            ))),
        },
    ]
}

/// A create body carrying `channel_role_arn`, with everything else the model requires.
fn create_body(channel_role_arn: &str) -> Value {
    json!({
        "teamId": TEAM_ID,
        "channelId": CHANNEL_ID,
        "channelName": "aws-support",
        "notifyOnCaseSeverity": "none",
        "channelRoleArn": channel_role_arn,
    })
}

/// The status, modelled error class and message a reply carries, which is the whole of what
/// a client branches on — and therefore the whole of what the two operations must agree on.
fn answer(reply: &Reply) -> (u16, String, Option<String>) {
    (
        reply.status,
        reply.error_type.clone(),
        reply.string("message"),
    )
}

/// What `expected` says that triple is.
fn expected_answer(case: &Case) -> (u16, String, Option<String>) {
    match &case.expected {
        Expected::Served => (200, String::new(), None),
        Expected::Refused(ChannelRoleArnRefusal::Validation(message)) => (
            400,
            "ValidationException".to_string(),
            Some(message.clone()),
        ),
        Expected::Refused(ChannelRoleArnRefusal::AccessDenied(message)) => (
            403,
            "AccessDeniedException".to_string(),
            Some(message.clone()),
        ),
    }
}

/// The role ARN the listing reports for the fixture pair, or `None` when nothing is stored.
async fn stored_role_arn(emulator: &Emulator) -> Option<String> {
    emulator.listed(TEAM_ID, CHANNEL_ID).await.map(|listed| {
        listed
            .get("channelRoleArn")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    })
}

#[tokio::test]
async fn both_writes_answer_one_channel_role_arn_the_same_way() {
    for case in cases() {
        let expected = expected_answer(&case);
        let label = case.label;

        // The create path: the member's rules decide the answer before anything is stored.
        let emulator = Emulator::new();
        let created = emulator
            .create_in_registered_workspace(create_body(&case.submitted))
            .await;
        let create_answer = answer(&created);
        assert_eq!(
            create_answer, expected,
            "case {label:?} (create, {submitted:?}): expected {expected:?}, got {create_answer:?} \
             / {body}",
            submitted = case.submitted,
            body = created.body
        );
        let stored = stored_role_arn(&emulator).await;
        match &case.expected {
            Expected::Served => assert_eq!(
                stored.as_deref(),
                Some(case.submitted.as_str()),
                "case {label:?} (create, {submitted:?}): expected the served value to be \
                 persisted, got {stored:?}",
                submitted = case.submitted
            ),
            Expected::Refused(_) => assert!(
                stored.is_none(),
                "case {label:?} (create, {submitted:?}): expected a refused create to record \
                 nothing, got {stored:?}",
                submitted = case.submitted
            ),
        }

        // The update path, over a configuration whose stored role is already a role of the
        // caller's account: the same value, the same answer — and a refused update leaves
        // the stored role exactly as it stands.
        let emulator = Emulator::new();
        let fixture = emulator
            .create_in_registered_workspace(create_body(&fixture_role_arn()))
            .await;
        assert_eq!(
            fixture.status, 200,
            "case {label:?}: expected the fixture create to answer HTTP 200, got {} / {}",
            fixture.status, fixture.body
        );
        let updated = emulator
            .update(json!({
                "teamId": TEAM_ID,
                "channelId": CHANNEL_ID,
                "channelRoleArn": case.submitted,
            }))
            .await;
        let update_answer = answer(&updated);
        assert_eq!(
            update_answer, expected,
            "case {label:?} (update, {submitted:?}): expected {expected:?}, got {update_answer:?} \
             / {body}",
            submitted = case.submitted,
            body = updated.body
        );
        assert_eq!(
            update_answer, create_answer,
            "case {label:?} ({submitted:?}): both operations must answer alike, got \
             {create_answer:?} on create and {update_answer:?} on update",
            submitted = case.submitted
        );
        let stored = stored_role_arn(&emulator).await;
        let expected_stored = match &case.expected {
            Expected::Served => case.submitted.clone(),
            Expected::Refused(_) => fixture_role_arn(),
        };
        assert_eq!(
            stored.as_deref(),
            Some(expected_stored.as_str()),
            "case {label:?} (update, {submitted:?}): expected the stored role ARN to be \
             {expected_stored:?}, got {stored:?}",
            submitted = case.submitted
        );
    }
}

#[tokio::test]
async fn an_update_naming_no_role_keeps_the_stored_one() {
    // The member is optional on the update, and the rules judge the value a request CARRIES:
    // an update that names no role at all reaches neither of them and leaves the stored role
    // alone. This is the shared rule's `None` arm on the one operation that can take it.
    let emulator = Emulator::new();
    let created = emulator
        .create_in_registered_workspace(create_body(&fixture_role_arn()))
        .await;
    assert_eq!(
        created.status, 200,
        "case \"no channelRoleArn\": expected the fixture create to answer HTTP 200, got {} / {}",
        created.status, created.body
    );
    let updated = emulator
        .update(json!({
            "teamId": TEAM_ID,
            "channelId": CHANNEL_ID,
            "channelName": "aws-support-renamed",
        }))
        .await;
    assert_eq!(
        updated.status, 200,
        "case \"no channelRoleArn\": expected HTTP 200, got {} / {}",
        updated.status, updated.body
    );
    let stored = stored_role_arn(&emulator).await;
    assert_eq!(
        stored.as_deref(),
        Some(fixture_role_arn().as_str()),
        "case \"no channelRoleArn\": expected the stored role ARN to be unchanged, got {stored:?}"
    );
}

#[tokio::test]
async fn the_shared_rule_names_the_class_each_refusal_travels_in() {
    // The classes the wire rows above assert are the ones the shared rule itself reports, so
    // the order and the class live in ONE place: a handler that lifted the wrong class, or
    // judged the two rules in its own order, could not agree with both.
    for case in cases() {
        let label = case.label;
        let decided = service_com_amazonaws_supportapp_supportapp::state::channel_role_arn_refusal(
            &case.submitted,
        );
        match &case.expected {
            Expected::Served => assert!(
                decided.is_none(),
                "case {label:?} ({submitted:?}): expected no refusal, got {decided:?}",
                submitted = case.submitted
            ),
            Expected::Refused(expected) => assert_eq!(
                decided.as_ref(),
                Some(expected),
                "case {label:?} ({submitted:?}): expected {expected:?}, got {decided:?}",
                submitted = case.submitted
            ),
        }
    }
}
