// SPDX-License-Identifier: CC0-1.0

//! WHICH CHANNEL NAMES the member's own `@pattern` admits, over the wire.
//!
//! THE RULE UNDER TEST: a modelled `@pattern` is an ECMA-262 regular expression, and there
//! `.` matches every character EXCEPT a line terminator — line feed, carriage return, line
//! separator U+2028 and paragraph separator U+2029. `channelName` declares `^.+$`
//! (`smithy_query(query="shape",shape="channelName")`), so a value carrying one of those
//! four characters is outside the member's declared expression however long it is, and
//! every operation that WRITES the member refuses it with the modelled
//! `ValidationException` (HTTP 400), in the standard validation envelope this service states
//! a declared constraint in, and records nothing for it. Every other character is one `.`
//! matches, and so one the member admits.
//!
//! Why a handler has to be a layer that says so: the request boundary's constraint walk
//! compiles a `@pattern` with an engine whose `.` excludes a line FEED alone, so three of
//! the four would be admitted there whatever the modelled expression says — and the SAME
//! shape, with the same `@pattern`, is an OUTPUT member:
//! `UpdateSlackChannelConfiguration` answers with `channelName` and
//! `ListSlackChannelConfigurations` reports it inside every `SlackChannelConfiguration`, so
//! a value the expression does not admit could not be reported back through the shape it was
//! stored in. Both read paths are therefore driven here on an admitted value, and both are
//! shown to report nothing at all for a refused one.
//!
//! The tables are that rule, not one recorded request. The refused rows are the cross
//! product of every line terminator the expression's `.` excludes with every place one can
//! sit in an otherwise well-formed name — at its start, inside it, at its end, as the whole
//! of it, twice over, and inside a name at the member's published `@length` maximum — and
//! are driven on BOTH writes, so the refusal cannot come from one layer's reading of the
//! expression: the line-feed rows are what the boundary's own engine already catches, and
//! every row is required to be answered the same way byte for byte. The admitted rows are
//! the boundary of the rule: names carrying the characters that are whitespace or control to
//! a reader but are NOT line terminators (tab, vertical tab, form feed, the next-line
//! character U+0085, a plain space), a non-ASCII name, and names at both published `@length`
//! bounds, each served and reported back verbatim.
//!
//! The REST of the member's rule stays where it already was — the request boundary applies
//! the `+`'s at-least-one and the member's declared `@length` 1..256 — which the last test
//! pins from the other side.

use serde_json::{json, Value};
use service_com_amazonaws_supportapp_supportapp::state::CHANNEL_NAME_PATTERN;

use crate::error_message_templates::pattern_refusal;
use crate::slack_channel_configurations::{role_arn, Emulator, Reply};

/// The `(teamId, channelId)` every row below configures.
const TEAM_ID: &str = "T012ABCDEFG";
const CHANNEL_ID: &str = "C01234A5BCD";

/// `channelName`'s declared `@pattern`, as the model spells it — the ANCHORED text
/// (`smithy_query(query="shape",shape="channelName")`), written out here so the expression
/// this suite expects a clause to state is not read back off the code under test.
const CHANNEL_NAME_DECLARED_PATTERN: &str = r"^.+$";

/// `channelName`'s declared `@length` bounds, from the same shape.
const CHANNEL_NAME_MIN_LEN: usize = 1;
const CHANNEL_NAME_MAX_LEN: usize = 256;

/// The channel name the fixture configuration is created with, so an update row acts on a
/// stored configuration whose name the member's expression admits.
const STORED_CHANNEL_NAME: &str = "aws-support";

/// The characters a modelled `@pattern`'s `.` does NOT match, from ECMA-262: line feed,
/// carriage return, line separator (U+2028) and paragraph separator (U+2029). A channel name
/// carrying one of them is outside `^.+$` however long it is.
const LINE_TERMINATORS: [(&str, char); 4] = [
    ("a line feed", '\n'),
    ("a carriage return", '\r'),
    ("the line separator U+2028", '\u{2028}'),
    ("the paragraph separator U+2029", '\u{2029}'),
];

/// The expression a clause states for `declared`: the model's text with the anchor pair the
/// model brackets it with removed, stated here independently of the code under test.
fn without_the_anchor_pair(declared: &str) -> &str {
    declared
        .strip_prefix('^')
        .and_then(|body| body.strip_suffix('$'))
        .unwrap_or_else(|| {
            panic!("every @pattern this service declares is bracketed by ^ and $: {declared:?}")
        })
}

/// Where in an otherwise well-formed channel name a line terminator sits. Each entry turns
/// the terminator into a whole value, so the cross product below spans both axes the rule
/// generalises over: WHICH terminator, and WHERE it is — including a name at the member's
/// published `@length` maximum, where the only thing wrong with the value is the character
/// the expression cannot match.
fn placements(terminator: char) -> Vec<(&'static str, String)> {
    let at_the_maximum: String = "n"
        .repeat(CHANNEL_NAME_MAX_LEN - 1)
        .chars()
        .chain(std::iter::once(terminator))
        .collect();
    vec![
        (
            "at the start of the channel name",
            format!("{terminator}aws-support"),
        ),
        (
            "inside the channel name",
            format!("aws{terminator}support"),
        ),
        (
            "at the end of the channel name",
            format!("aws-support{terminator}"),
        ),
        ("as the whole channel name", terminator.to_string()),
        (
            "twice over in the channel name",
            format!("aws{terminator}{terminator}support"),
        ),
        ("inside a name at the published maximum", at_the_maximum),
    ]
}

/// Every refused value: each line terminator in each placement.
fn names_outside_the_members_pattern() -> Vec<(String, String)> {
    LINE_TERMINATORS
        .iter()
        .flat_map(|(terminator_case, terminator)| {
            placements(*terminator)
                .into_iter()
                .map(move |(placement, submitted)| {
                    (format!("{terminator_case} {placement}"), submitted)
                })
        })
        .collect()
}

/// Channel names made of characters the expression's `.` DOES match, including the ones a
/// reader would call whitespace or control but ECMA-262 does not call line terminators, and
/// names at both published `@length` bounds. Each is served and reported back verbatim.
fn names_the_pattern_admits() -> Vec<(&'static str, String)> {
    vec![
        ("a tab inside the name", "aws\tsupport".to_string()),
        (
            "a vertical tab inside the name",
            "aws\u{000B}support".to_string(),
        ),
        (
            "a form feed inside the name",
            "aws\u{000C}support".to_string(),
        ),
        (
            "the next-line character U+0085 inside the name",
            "aws\u{0085}support".to_string(),
        ),
        ("a space inside the name", "aws support".to_string()),
        (
            "a non-ASCII name",
            "unterstützung\u{1F600}".to_string(),
        ),
        (
            "a name at the published minimum",
            "n".repeat(CHANNEL_NAME_MIN_LEN),
        ),
        (
            "a name at the published maximum",
            "n".repeat(CHANNEL_NAME_MAX_LEN),
        ),
    ]
}

/// A create body carrying `channel_name`, with everything else the model requires.
fn create_body(channel_name: &str) -> Value {
    json!({
        "teamId": TEAM_ID,
        "channelId": CHANNEL_ID,
        "channelName": channel_name,
        "notifyOnCaseSeverity": "none",
        "channelRoleArn": role_arn("SupportAppRole"),
    })
}

/// An update body carrying `channel_name` for the pair every row configures.
fn update_body(channel_name: &str) -> Value {
    json!({
        "teamId": TEAM_ID,
        "channelId": CHANNEL_ID,
        "channelName": channel_name,
    })
}

/// The clause a value outside the member's declared `@pattern` is refused with: the standard
/// single-violation envelope, naming the submitted value, the member path in the spelling the
/// envelope states a member with, and the expression WITHOUT the anchors the model brackets
/// it with.
fn expected_refusal(submitted: &str) -> String {
    pattern_refusal(
        submitted,
        "channelName",
        without_the_anchor_pair(CHANNEL_NAME_DECLARED_PATTERN),
    )
}

/// Require `reply` to be the modelled refusal of a declared constraint on `channelName`.
fn assert_pattern_refusal(case: &str, submitted: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 400,
        "case {case:?} ({submitted:?}): expected HTTP 400, got {} / {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_type, "ValidationException",
        "case {case:?} ({submitted:?}): expected ValidationException, got {:?} / {}",
        reply.error_type, reply.body
    );
    let message = reply.string("message").unwrap_or_else(|| {
        panic!(
            "case {case:?} ({submitted:?}): the refusal carries no message: {}",
            reply.body
        )
    });
    let expected = expected_refusal(submitted);
    assert_eq!(
        message, expected,
        "case {case:?} ({submitted:?}): expected {expected:?}, got {message:?}"
    );
}

/// The channel name the listing reports for the fixture pair, or `None` when nothing is
/// stored for it.
async fn listed_channel_name(emulator: &Emulator) -> Option<String> {
    emulator
        .listed(TEAM_ID, CHANNEL_ID)
        .await
        .map(|listed| {
            listed
                .get("channelName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
}

#[tokio::test]
async fn the_crate_states_the_members_declared_pattern() {
    // The expression the crate applies is the one the model declares, so a drift in it fails
    // here rather than passing unnoticed through the rows below.
    assert_eq!(
        CHANNEL_NAME_PATTERN, CHANNEL_NAME_DECLARED_PATTERN,
        "case \"the declared expression\": expected {CHANNEL_NAME_DECLARED_PATTERN:?}, got \
         {CHANNEL_NAME_PATTERN:?}"
    );
    // And the clause states it with the anchor pair dropped, which is this service's shared
    // rendering of a reported `@pattern`.
    let stated = ::service_common::validation::unanchored(CHANNEL_NAME_PATTERN);
    let expected = without_the_anchor_pair(CHANNEL_NAME_DECLARED_PATTERN);
    assert_eq!(
        stated, expected,
        "case \"the stated expression\": expected {expected:?}, got {stated:?}"
    );
}

#[tokio::test]
async fn a_create_whose_channel_name_the_pattern_rejects_is_refused() {
    for (case, submitted) in names_outside_the_members_pattern() {
        let emulator = Emulator::new();
        let reply = emulator
            .create_in_registered_workspace(create_body(&submitted))
            .await;
        assert_pattern_refusal(&case, &submitted, &reply);

        // Nothing was recorded, so neither read path can report a value the member's own
        // `@pattern` does not admit.
        let listed = emulator.listed(TEAM_ID, CHANNEL_ID).await;
        assert!(
            listed.is_none(),
            "case {case:?} ({submitted:?}): expected no configuration to be recorded, got \
             {listed:?}"
        );
    }
}

#[tokio::test]
async fn an_update_onto_a_channel_name_the_pattern_rejects_is_refused() {
    for (case, submitted) in names_outside_the_members_pattern() {
        let emulator = Emulator::new();
        let created = emulator
            .create_in_registered_workspace(create_body(STORED_CHANNEL_NAME))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected the fixture create to answer HTTP 200, got {} / {}",
            created.status, created.body
        );

        let reply = emulator.update(update_body(&submitted)).await;
        assert_pattern_refusal(&case, &submitted, &reply);

        // The refused update left the stored name exactly as the create recorded it, so the
        // operation's own output member cannot report the refused value either.
        let stored = listed_channel_name(&emulator).await;
        assert_eq!(
            stored.as_deref(),
            Some(STORED_CHANNEL_NAME),
            "case {case:?} ({submitted:?}): expected the stored channel name to be unchanged, \
             got {stored:?}"
        );
    }
}

#[tokio::test]
async fn both_writes_refuse_one_channel_name_the_same_way() {
    // One value, one answer: the line-feed rows are the ones the request boundary's own
    // engine catches and the other three are caught by the rule the handlers apply, so a
    // layer that worded its refusal its own way could not agree with the other.
    for (case, submitted) in names_outside_the_members_pattern() {
        let emulator = Emulator::new();
        let created = emulator
            .create_in_registered_workspace(create_body(&submitted))
            .await;
        let update = emulator.update(update_body(&submitted)).await;
        let answers = (
            (created.status, created.error_type.clone(), created.string("message")),
            (update.status, update.error_type.clone(), update.string("message")),
        );
        assert_eq!(
            answers.0, answers.1,
            "case {case:?} ({submitted:?}): both writes must answer alike, got {:?} on create \
             and {:?} on update",
            answers.0, answers.1
        );
        assert_eq!(
            answers.0,
            (
                400,
                "ValidationException".to_string(),
                Some(expected_refusal(&submitted))
            ),
            "case {case:?} ({submitted:?}): expected the declared-constraint refusal, got {:?}",
            answers.0
        );
    }
}

#[tokio::test]
async fn a_channel_name_the_pattern_admits_round_trips_on_both_writes() {
    for (case, channel_name) in names_the_pattern_admits() {
        let emulator = Emulator::new();
        let created = emulator
            .create_in_registered_workspace(create_body(&channel_name))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?} ({channel_name:?}): expected create to answer HTTP 200, got {} / {}",
            created.status, created.body
        );
        let listed = listed_channel_name(&emulator).await;
        assert_eq!(
            listed.as_deref(),
            Some(channel_name.as_str()),
            "case {case:?}: expected the created channel name to be reported, got {listed:?}"
        );

        // And the other read path of the same member: the update answers with the value it
        // persisted, renamed to another name the expression admits — clipped to the member's
        // published maximum, so a row that is already at that bound stays within it.
        let renamed: String = format!("renamed\t{channel_name}")
            .chars()
            .take(CHANNEL_NAME_MAX_LEN)
            .collect();
        let updated = emulator.update(update_body(&renamed)).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?} ({renamed:?}): expected update to answer HTTP 200, got {} / {}",
            updated.status, updated.body
        );
        assert_eq!(
            updated.string("channelName"),
            Some(renamed.clone()),
            "case {case:?}: expected the update to answer with the new channel name, got {}",
            updated.body
        );
        let listed = listed_channel_name(&emulator).await;
        assert_eq!(
            listed.as_deref(),
            Some(renamed.as_str()),
            "case {case:?}: expected the updated channel name to be persisted, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn the_rest_of_the_members_rule_stays_at_the_request_boundary() {
    // The `+` the expression quantifies, and the member's declared `@length` minimum, are
    // the request boundary's: an empty name is short AND outside the expression, so both
    // clauses are reported in one counted envelope — on both writes.
    let empty = String::new();
    let expected = format!(
        "2 validation errors detected: Value '' at 'channelName' failed to satisfy constraint: \
         Member must have length greater than or equal to {CHANNEL_NAME_MIN_LEN}; Value '' at \
         'channelName' failed to satisfy constraint: Member must satisfy regular expression \
         pattern: {}",
        without_the_anchor_pair(CHANNEL_NAME_DECLARED_PATTERN)
    );
    for (case, path, body) in [
        (
            "an empty name on a create",
            "create-slack-channel-configuration",
            create_body(&empty),
        ),
        (
            "an empty name on an update",
            "update-slack-channel-configuration",
            update_body(&empty),
        ),
    ] {
        let emulator = Emulator::new();
        let reply = emulator.call(path, body).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected HTTP 400, got {} / {}",
            reply.status, reply.body
        );
        let message = reply
            .string("message")
            .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no message: {}", reply.body));
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}
