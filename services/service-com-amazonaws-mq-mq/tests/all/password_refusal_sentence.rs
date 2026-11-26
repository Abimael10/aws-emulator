// SPDX-License-Identifier: CC0-1.0

//! HOW Amazon MQ words a refusal of a `User$Password`, driven end to end through the
//! router.
//!
//! RULE: where Amazon MQ states a refusal in a sentence of its OWN — the fixed text the
//! service answers for that class of refusal — the emulator answers that sentence
//! verbatim: nothing from the request is added to it or interpolated into it, neither the
//! rejected value, the character at fault, nor the element path of the list member the
//! value was sent in. A password is refused for exactly one of the three rules
//! `User$Password` documents ("must be at least 12 characters long, must contain at least
//! 4 unique characters, and must not contain commas, colons, or equal signs (,:=)" —
//! `smithy_query(query="doc", shape="User", member="Password")`), each with its own
//! sentence, at HTTP 400 `BadRequestException`, and nothing is created.
//!
//! The tables span what the rule generalises over: each of the three documented rules;
//! each forbidden character (`state::FORBIDDEN_PASSWORD_CHARACTERS`, the set the member's
//! documentation names) at the start, in the middle and at the end of the password; the
//! position of the offending user in `CreateBroker`'s `Users` list (first and later, so a
//! message carrying the element path would differ between rows); every engine the model
//! enumerates (`ENGINE_TYPE_VALUES`), the wording being the service's and not an
//! engine's; and both ways a password reaches the service — inline in `CreateBroker` and
//! on its own in `CreateUser`.
//!
//! The wire harness is shared with [`crate::broker_lifecycle`], so every call goes
//! through the same `MultiServiceRouter::dispatch` the front door uses.

use serde_json::{json, Value};

use service_com_amazonaws_mq_mq::ops::types::ENGINE_TYPE_VALUES;
use service_com_amazonaws_mq_mq::state::{
    FORBIDDEN_PASSWORD_CHARACTERS, MIN_PASSWORD_LENGTH, MIN_UNIQUE_PASSWORD_CHARACTERS,
    PASSWORD_FORBIDDEN_CHARACTERS_MESSAGE, PASSWORD_LENGTH_MESSAGE,
    PASSWORD_UNIQUE_CHARACTERS_MESSAGE,
};

use crate::broker_lifecycle::{create_body, string_at, Mq};

/// A password satisfying every documented rule, for the users a case is not about.
const GOOD_PASSWORD: &str = "GoodPassword12";

/// The passwords each documented rule rejects, with the sentence Amazon MQ states that
/// rule in. The forbidden-character rows are built from the member's own documented set
/// rather than from a list spelled out here.
fn refused_passwords() -> Vec<(String, String, &'static str)> {
    let mut rows: Vec<(String, String, &'static str)> = vec![
        // The length rule, at one character short of the published bound and at nothing
        // at all.
        (
            "one short of the length bound".to_string(),
            "A".repeat(MIN_PASSWORD_LENGTH - 1),
            PASSWORD_LENGTH_MESSAGE,
        ),
        (
            "an empty password".to_string(),
            String::new(),
            PASSWORD_LENGTH_MESSAGE,
        ),
        // The unique-character rule, at one character short of the published bound: long
        // enough for the length rule, so this is the rule it breaks.
        (
            "one short of the unique-character bound".to_string(),
            "abc".repeat(MIN_PASSWORD_LENGTH),
            PASSWORD_UNIQUE_CHARACTERS_MESSAGE,
        ),
    ];
    // The forbidden-character rule: every character of the documented set, at each
    // position of the password a client might put one in.
    for forbidden in FORBIDDEN_PASSWORD_CHARACTERS {
        for (where_it_is, password) in [
            ("at the start", format!("{forbidden}GoodPassword12")),
            ("in the middle", format!("Good{forbidden}Password12")),
            ("at the end", format!("GoodPassword12{forbidden}")),
        ] {
            rows.push((
                format!("{forbidden:?} {where_it_is}"),
                password,
                PASSWORD_FORBIDDEN_CHARACTERS_MESSAGE,
            ));
        }
    }
    rows
}

/// Assert one refusal: the status, the error code a restJson1 client branches on, and the
/// message, which must be the service's sentence and nothing more.
fn assert_refusal(case: &str, expected: &str, answer: &(u16, String, Value)) {
    let (status, code, body) = answer;
    assert_eq!(
        *status, 400,
        "case {case:?}: expected 400, got {status} {code:?} {body:?}"
    );
    assert_eq!(
        code, "BadRequestException",
        "case {case:?}: expected BadRequestException, got {code:?} {body:?}"
    );
    let message = string_at(body, "/message");
    assert_eq!(
        message, expected,
        "case {case:?}: expected the service's own sentence {expected:?}, got {message:?}"
    );
}

/// RULE: a `CreateBroker` whose `Users` carries a password one of the documented rules
/// rejects is refused with that rule's sentence exactly — the element of `Users` the
/// password sat in is not part of the message — and no broker is minted.
#[tokio::test]
async fn create_broker_refuses_a_password_in_amazon_mqs_own_words() {
    let mq = Mq::new("us-east-1");

    let mut rows = 0usize;
    for engine_type in ENGINE_TYPE_VALUES {
        for (case, password, expected) in refused_passwords() {
            // The offending user is sent as the FIRST and as a LATER element of `Users`,
            // so a message naming the element path could not read the same for both.
            for position in [0usize, 1] {
                let case = format!("{engine_type}/{case}/users[{position}]");
                let broker_name = format!("pwrefused-{rows}");
                let mut users: Vec<Value> = (0..=position)
                    .map(|index| {
                        json!({"username": format!("user{index}"), "password": GOOD_PASSWORD})
                    })
                    .collect();
                users[position] =
                    json!({"username": format!("user{position}"), "password": password});
                let mut body = create_body(&broker_name);
                body["engineType"] = json!(engine_type);
                body["users"] = json!(users);

                let answer = mq.call_with_code("POST", "/v1/brokers", Some(body)).await;
                assert_refusal(&case, expected, &answer);

                // Nothing was minted: the name the refused create asked for belongs to
                // no broker.
                let (status, listed) = mq.list_brokers("").await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected 200 from list-brokers, got {status} {listed:?}"
                );
                let names: Vec<String> = listed
                    .pointer("/brokerSummaries")
                    .and_then(Value::as_array)
                    .map(|summaries| {
                        summaries
                            .iter()
                            .map(|summary| string_at(summary, "/brokerName"))
                            .collect()
                    })
                    .unwrap_or_default();
                assert!(
                    !names.contains(&broker_name),
                    "case {case:?}: a refused create mints no broker, got {names:?}"
                );
                rows += 1;
            }
        }
    }
    assert_eq!(
        rows,
        ENGINE_TYPE_VALUES.len() * refused_passwords().len() * 2,
        "every rule, character, position and engine must be covered, covered {rows}"
    );
}

/// The same rule on the other way a password reaches the service: `CreateUser`, where the
/// password is the request's own member rather than an element of a list. One sentence per
/// rule, whichever operation was called.
#[tokio::test]
async fn create_user_refuses_a_password_in_the_same_words() {
    let mq = Mq::new("eu-west-1");
    let (status, created) = mq.create_broker(create_body("pwrefused-user")).await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from create-broker, got {status} {created:?}"
    );
    let broker_id = string_at(&created, "/brokerId");

    for (index, (case, password, expected)) in refused_passwords().into_iter().enumerate() {
        let username = format!("refused{index}");
        let answer = mq
            .call_with_code(
                "POST",
                &format!("/v1/brokers/{broker_id}/users/{username}"),
                Some(json!({"password": password})),
            )
            .await;
        assert_refusal(&case, expected, &answer);

        // And the refused create stored nothing.
        let (status, user) = mq
            .call(
                "GET",
                &format!("/v1/brokers/{broker_id}/users/{username}"),
                None,
            )
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: a refused create stores no user, got {status} {user:?}"
        );
    }
}

/// The accepted side of the same three rules: a password that meets every one of them is
/// taken, at each published bound exactly and above it, with the punctuation the
/// forbidden set does not name.
#[tokio::test]
async fn a_password_that_meets_every_rule_is_accepted() {
    let mq = Mq::new("us-west-2");
    let (status, created) = mq.create_broker(create_body("pwaccepted")).await;
    assert_eq!(
        status, 200,
        "setup: expected 200 from create-broker, got {status} {created:?}"
    );
    let broker_id = string_at(&created, "/brokerId");

    // Rows: exactly the length bound with exactly the unique-character bound, a longer
    // password, and one carrying punctuation that is not in the forbidden set.
    let accepted: Vec<(String, String)> = vec![
        (
            "exactly both published bounds".to_string(),
            "abcd".repeat(MIN_PASSWORD_LENGTH / MIN_UNIQUE_PASSWORD_CHARACTERS),
        ),
        (
            "longer than the length bound".to_string(),
            format!("{GOOD_PASSWORD}{GOOD_PASSWORD}"),
        ),
        (
            "punctuation the rule does not forbid".to_string(),
            "Abcd-efgh.ijkl~1".to_string(),
        ),
    ];

    for (index, (case, password)) in accepted.into_iter().enumerate() {
        assert!(
            password.chars().count() >= MIN_PASSWORD_LENGTH,
            "case {case:?}: the row must meet the length bound, got {password:?}"
        );
        let username = format!("accepted{index}");
        let (status, code, answer) = mq
            .call_with_code(
                "POST",
                &format!("/v1/brokers/{broker_id}/users/{username}"),
                Some(json!({"password": password})),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} {code:?} {answer:?}"
        );
        let (status, user) = mq
            .call(
                "GET",
                &format!("/v1/brokers/{broker_id}/users/{username}"),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the user must read back, got {status} {user:?}"
        );
    }
}
