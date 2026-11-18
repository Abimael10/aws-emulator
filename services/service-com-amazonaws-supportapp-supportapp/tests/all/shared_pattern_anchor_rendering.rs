// SPDX-License-Identifier: CC0-1.0

//! Where the `@pattern` a boundary refusal quotes is rendered: the ONE shared
//! anchor-stripping rule in `service_common::validation`.
//!
//! The service seam (`src/service.rs`, `as_stated`) states a `@pattern` violation with
//! the `^` / `$` pair the model brackets the expression with dropped, which is the
//! rendering the shared validation module owns and every service that words a pattern
//! this way calls. This suite drives that one path over the WIRE, through every
//! operation whose request members the boundary judges — the alias put, both channel
//! configuration writes, the channel delete, the workspace registration and delete, and
//! the listing's pagination token — so the clause cannot come from a rule a single
//! operation carries on its own.
//!
//! Each row asserts the clause twice, against two independent statements of the
//! expected expression: the one the shared function renders for that member's declared
//! pattern, and the model's pattern text with its anchor pair removed here in the test.
//! A clause that quoted anything else — an anchored expression, or an expression some
//! other rule produced — fails both.

use serde_json::{json, Value};

use crate::error_message_templates::{pattern_refusal, refusal, valid_create};
use crate::slack_channel_configurations::Emulator;

/// `awsAccountAlias`'s declared `@pattern`, as the model spells it — the ANCHORED text,
/// which is what the shared rendering is handed
/// (`smithy_query(query="shape", shape="awsAccountAlias")`).
const ACCOUNT_ALIAS_DECLARED_PATTERN: &str = r"^[\w\- ]+$";

/// `teamId`'s and `channelId`'s declared `@pattern`, which `paginationToken` shares
/// (`smithy_query(query="shape", shape="teamId,channelId,paginationToken")`).
const SLACK_ID_DECLARED_PATTERN: &str = r"^\S+$";

/// `channelName`'s declared `@pattern`
/// (`smithy_query(query="shape", shape="channelName")`).
const CHANNEL_NAME_DECLARED_PATTERN: &str = r"^.+$";

/// The expression a clause states for `declared`: the model's text with the anchor pair
/// the model brackets it with removed, stated here independently of the function under
/// test so the assertion below is not the renderer compared with itself.
fn without_the_anchor_pair(declared: &str) -> &str {
    declared
        .strip_prefix('^')
        .and_then(|body| body.strip_suffix('$'))
        .unwrap_or_else(|| {
            panic!("every @pattern this service declares is bracketed by ^ and $: {declared:?}")
        })
}

/// A create body whose `member` carries `value`, starting from the valid body the
/// sibling suite states (`crate::error_message_templates::valid_create`), so only the
/// member under test is refused.
fn create_with(member: &str, value: &str) -> Value {
    let mut body = valid_create();
    body[member] = json!(value);
    body
}

#[tokio::test]
async fn every_operation_states_a_pattern_through_the_shared_anchor_rendering() {
    let emulator = Emulator::new();

    // The rows differ along what the rule generalises over: which operation is called,
    // which member carries the bad value, which `@pattern` that member declares, and
    // the form of the value that leaves it (a space, a line break, punctuation).
    let rows: [(&str, &str, Value, &str, &str, &str); 8] = [
        (
            "an alias outside its charset",
            "put-account-alias",
            json!({ "accountAlias": "bad alias!" }),
            "bad alias!",
            "accountAlias",
            ACCOUNT_ALIAS_DECLARED_PATTERN,
        ),
        (
            "a create whose teamId carries a space",
            "create-slack-channel-configuration",
            create_with("teamId", "T012 ABCDEFG"),
            "T012 ABCDEFG",
            "teamId",
            SLACK_ID_DECLARED_PATTERN,
        ),
        (
            "a create whose channelId carries a tab",
            "create-slack-channel-configuration",
            create_with("channelId", "C01234\tA5BCD"),
            "C01234\tA5BCD",
            "channelId",
            SLACK_ID_DECLARED_PATTERN,
        ),
        (
            "a create whose channelName spans a line break",
            "create-slack-channel-configuration",
            create_with("channelName", "first\nsecond"),
            "first\nsecond",
            "channelName",
            CHANNEL_NAME_DECLARED_PATTERN,
        ),
        (
            "an update whose teamId carries a space",
            "update-slack-channel-configuration",
            json!({ "teamId": "T012 ABCDEFG", "channelId": "C01234A5BCD" }),
            "T012 ABCDEFG",
            "teamId",
            SLACK_ID_DECLARED_PATTERN,
        ),
        (
            "a delete whose channelId carries a space",
            "delete-slack-channel-configuration",
            json!({ "teamId": "T012ABCDEFG", "channelId": "C0123 45" }),
            "C0123 45",
            "channelId",
            SLACK_ID_DECLARED_PATTERN,
        ),
        (
            "a workspace registration whose teamId carries a space",
            "register-slack-workspace-for-organization",
            json!({ "teamId": "T012 ABCDEFG" }),
            "T012 ABCDEFG",
            "teamId",
            SLACK_ID_DECLARED_PATTERN,
        ),
        (
            "a workspace delete whose teamId carries a space",
            "delete-slack-workspace-configuration",
            json!({ "teamId": "T012 ABCDEFG" }),
            "T012 ABCDEFG",
            "teamId",
            SLACK_ID_DECLARED_PATTERN,
        ),
    ];

    for (label, path, body, value, member, declared) in rows {
        let stated = ::service_common::validation::unanchored(declared);
        let expected_expression = without_the_anchor_pair(declared);
        assert_eq!(
            stated, expected_expression,
            "case {label:?}: expected the shared rendering of {declared:?} to be \
             {expected_expression:?}, got {stated:?}"
        );

        let reply = emulator.call(path, body).await;
        let message = refusal(label, &reply, 400, "ValidationException");
        let expected = pattern_refusal(value, member, &stated);
        assert_eq!(
            message, expected,
            "case {label:?}: expected {expected:?}, got {message:?}"
        );
        assert_eq!(
            message,
            pattern_refusal(value, member, expected_expression),
            "case {label:?}: expected the clause to quote {expected_expression:?}, got {message:?}"
        );
    }
}

/// The listing's pagination token is judged by the same boundary, so its clause is
/// rendered by the same shared rule — and a token that IS within the pattern is refused
/// by the handler instead, with no expression quoted at all.
#[tokio::test]
async fn the_listing_token_states_its_pattern_through_the_same_rendering() {
    let emulator = Emulator::new();
    let stated = ::service_common::validation::unanchored(SLACK_ID_DECLARED_PATTERN);
    assert_eq!(
        stated,
        without_the_anchor_pair(SLACK_ID_DECLARED_PATTERN),
        "expected the shared rendering of {SLACK_ID_DECLARED_PATTERN:?} to drop the anchor pair, \
         got {stated:?}"
    );

    for (label, token) in [
        ("a token carrying a space", "next token"),
        ("a token carrying a line break", "next\ntoken"),
    ] {
        let reply = emulator.list(Some(token)).await;
        let message = refusal(label, &reply, 400, "ValidationException");
        let expected = pattern_refusal(token, "nextToken", &stated);
        assert_eq!(
            message, expected,
            "case {label:?}: expected {expected:?}, got {message:?}"
        );
    }

    // A token the boundary accepts never reaches this rendering: the handler refuses a
    // token no listing of this service returned, in its own prose.
    let reply = emulator.list(Some("Zm9yZ2Vk")).await;
    let message = refusal("a well-formed token", &reply, 400, "ValidationException");
    assert!(
        !message.contains(&stated) && !message.contains("regular expression pattern"),
        "case \"a well-formed token\": expected no pattern clause, got {message:?}"
    );
}
