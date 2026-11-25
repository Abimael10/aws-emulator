// SPDX-License-Identifier: CC0-1.0

//! WHICH SLACK IDS the `\S`-based `@pattern` `teamId`, `channelId` and the listings'
//! `nextToken` share admits, over the wire.
//!
//! THE RULE UNDER TEST: a modelled `@pattern` is an ECMA-262 regular expression, and there
//! `\s` matches every WhiteSpace and every LineTerminator — the tab, the vertical tab, the
//! form feed, the space, the no-break space, every character of the Unicode
//! `Space_Separator` category, the ZERO WIDTH NO-BREAK SPACE U+FEFF, and the four line
//! terminators — so `\S` matches none of them. `teamId`, `channelId` and `paginationToken`
//! each declare `^\S+$` (`smithy_query(query="shape",shape="teamId,channelId,paginationToken")`),
//! so a value carrying one of those characters is outside the member's declared expression
//! however long it is, and every operation that CARRIES the member refuses it with the
//! modelled `ValidationException` (HTTP 400), in the standard validation envelope this
//! service states a declared constraint in, and records, rewrites and removes nothing for
//! it. Every other character is one `\S` matches, and so one the member admits.
//!
//! Why a handler has to be a layer that says so: the request boundary's constraint walk
//! compiles a `@pattern` with an engine whose `\s` is the Unicode `White_Space` property,
//! which holds every one of those characters except U+FEFF, so a value carrying U+FEFF would
//! be admitted there whatever the modelled expression says — and the SAME shapes, with the
//! same `@pattern`, are OUTPUT members: `SlackChannelConfiguration$teamId` and `$channelId`
//! are `@required` members of what `ListSlackChannelConfigurations` reports,
//! `SlackWorkspaceConfiguration$teamId` of what `ListSlackWorkspaceConfigurations` reports,
//! and `UpdateSlackChannelConfiguration` answers with both — so a value the expression does
//! not admit could not be reported back through the shape it was stored under. Both read
//! paths are therefore driven here on admitted values, and are shown to report nothing at
//! all for a refused one.
//!
//! The tables are that rule, not one recorded request. The refused rows are the cross
//! product of every character the expression's `\S` excludes with every place one can sit in
//! an otherwise well-formed id, driven through every operation that carries one of the
//! members, so the refusal cannot come from one layer's reading of the expression: the rows
//! for the characters Unicode also calls `White_Space` are what the boundary's own engine
//! already catches, and every row is required to be answered the same way byte for byte. The
//! admitted rows are the boundary of the rule: ids carrying the zero-width characters a
//! reader might take for spaces but ECMA-262 does not call WhiteSpace (U+200B, U+2060), a
//! non-ASCII id, and ids at both published `@length` bounds, each served and reported back
//! verbatim.
//!
//! The REST of the members' rule stays where it already was — the request boundary applies
//! the `+`'s at-least-one and each member's declared `@length` 1..256 — which the last tests
//! pin from the other side, on each published bound and one past it.

use serde_json::{json, Value};
use service_com_amazonaws_supportapp_supportapp::state::SLACK_ID_PATTERN;

use crate::error_message_templates::{not_found, pattern_refusal, refusal};
use crate::slack_channel_configurations::{role_arn, Emulator, Reply};
use crate::slack_workspace_configurations::{list_workspaces, reported, reported_team_ids};

/// The `(teamId, channelId)` pair a fixture configuration is created under, both values ones
/// the members' expression admits, so what a row is refused for is the value it submits.
const TEAM_ID: &str = "T012ABCDEFG";
const CHANNEL_ID: &str = "C01234A5BCD";

/// The `@pattern` `teamId`, `channelId` and `paginationToken` declare, as the model spells it
/// — the ANCHORED text (`smithy_query(query="shape",shape="teamId")`), written out here so
/// the expression this suite expects a clause to state is not read back off the code under
/// test.
const SLACK_ID_DECLARED_PATTERN: &str = r"^\S+$";

/// The members' declared `@length` bounds, from the same shapes.
const SLACK_ID_MIN_LEN: usize = 1;
const SLACK_ID_MAX_LEN: usize = 256;

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

/// Every character a modelled `@pattern`'s `\S` does NOT match, from the ECMA-262 WhiteSpace
/// and LineTerminator productions: the tab, line feed, vertical tab, form feed and carriage
/// return, the space and the no-break space, every character of the Unicode
/// `Space_Separator` category (U+1680, U+2000 through U+200A, U+202F, U+205F and U+3000), the
/// line and paragraph separators, and the zero width no-break space U+FEFF. An id carrying
/// one of them is outside `^\S+$` however long it is.
fn characters_the_pattern_excludes() -> Vec<(String, char)> {
    let named: Vec<(String, char)> = [
        ("a tab", '\u{0009}'),
        ("a line feed", '\u{000A}'),
        ("a vertical tab", '\u{000B}'),
        ("a form feed", '\u{000C}'),
        ("a carriage return", '\u{000D}'),
        ("a space", '\u{0020}'),
        ("a no-break space", '\u{00A0}'),
        ("the Ogham space mark U+1680", '\u{1680}'),
        ("the line separator U+2028", '\u{2028}'),
        ("the paragraph separator U+2029", '\u{2029}'),
        ("the narrow no-break space U+202F", '\u{202F}'),
        ("the medium mathematical space U+205F", '\u{205F}'),
        ("the ideographic space U+3000", '\u{3000}'),
        ("the zero width no-break space U+FEFF", '\u{FEFF}'),
    ]
    .into_iter()
    .map(|(label, character)| (label.to_string(), character))
    .collect();
    // The rest of the `Space_Separator` category the production folds in as <USP>, a run the
    // table states by its bounds rather than one row at a time.
    let general_punctuation_spaces = ('\u{2000}'..='\u{200A}').map(|character| {
        (
            format!("the general punctuation space U+{:04X}", character as u32),
            character,
        )
    });
    named.into_iter().chain(general_punctuation_spaces).collect()
}

/// Where in an otherwise well-formed id an excluded character sits. Each entry turns the
/// character into a whole value too, so the cross product below spans both axes the rule
/// generalises over: WHICH character, and WHERE it is.
fn placements(character: char, well_formed: &str) -> Vec<(&'static str, String)> {
    let (head, tail) = well_formed.split_at(well_formed.len() / 2);
    vec![
        ("at the start of the id", format!("{character}{well_formed}")),
        ("inside the id", format!("{head}{character}{tail}")),
        ("at the end of the id", format!("{well_formed}{character}")),
        ("as the whole id", character.to_string()),
        (
            "twice over in the id",
            format!("{head}{character}{character}{tail}"),
        ),
    ]
}

/// Every refused value for `well_formed`: each excluded character in each placement.
fn ids_outside_the_members_pattern(well_formed: &str) -> Vec<(String, String)> {
    characters_the_pattern_excludes()
        .into_iter()
        .flat_map(|(character_case, character)| {
            placements(character, well_formed)
                .into_iter()
                .map(move |(placement, submitted)| {
                    (format!("{character_case} {placement}"), submitted)
                })
        })
        .collect()
}

/// One refused value per excluded character — the same cross product with the placement axis
/// collapsed, for the tables that drive every operation per row.
fn one_id_per_excluded_character(well_formed: &str) -> Vec<(String, String)> {
    characters_the_pattern_excludes()
        .into_iter()
        .map(|(character_case, character)| {
            let (head, tail) = well_formed.split_at(well_formed.len() / 2);
            (
                format!("{character_case} inside the id"),
                format!("{head}{character}{tail}"),
            )
        })
        .collect()
}

/// Ids made of characters the expression's `\S` DOES match, including the zero-width
/// characters ECMA-262 does not call WhiteSpace (U+200B ZERO WIDTH SPACE and U+2060 WORD
/// JOINER are `Format` characters, not `Space_Separator`), a non-ASCII id, and ids at both
/// published `@length` bounds.
fn ids_the_pattern_admits() -> Vec<(&'static str, String)> {
    vec![
        ("the documented workspace id", TEAM_ID.to_string()),
        (
            "a zero width space inside the id",
            "C0123\u{200B}45".to_string(),
        ),
        ("a word joiner inside the id", "C0123\u{2060}45".to_string()),
        ("a non-ASCII id", "Cñ0123456\u{1F600}".to_string()),
        ("an id at the published minimum", "C".repeat(SLACK_ID_MIN_LEN)),
        ("an id at the published maximum", "C".repeat(SLACK_ID_MAX_LEN)),
    ]
}

/// A create body for `(team_id, channel_id)`, with everything else the model requires.
fn create_body(team_id: &str, channel_id: &str) -> Value {
    json!({
        "teamId": team_id,
        "channelId": channel_id,
        "notifyOnCaseSeverity": "none",
        "channelRoleArn": role_arn("SupportAppRole"),
    })
}

/// An update body naming `(team_id, channel_id)` and nothing else the rule under test covers.
fn update_body(team_id: &str, channel_id: &str) -> Value {
    json!({
        "teamId": team_id,
        "channelId": channel_id,
        "channelName": "aws-support",
    })
}

/// The clause a value outside its member's declared `@pattern` is refused with: the standard
/// single-violation envelope, naming the submitted value, the member path in the spelling the
/// envelope states a member with, and the expression WITHOUT the anchors the model brackets
/// it with.
fn expected_refusal(member: &str, submitted: &str) -> String {
    pattern_refusal(
        submitted,
        member,
        without_the_anchor_pair(SLACK_ID_DECLARED_PATTERN),
    )
}

/// Require `reply` to be the modelled refusal of the declared `@pattern` on `member`.
fn assert_pattern_refusal(case: &str, member: &str, submitted: &str, reply: &Reply) {
    let message = refusal(case, reply, 400, "ValidationException");
    let expected = expected_refusal(member, submitted);
    assert_eq!(
        message, expected,
        "case {case:?} ({submitted:?}): expected {expected:?}, got {message:?}"
    );
}

/// A fixture configuration under the well-formed pair, so a row that acts on a stored
/// configuration is refused for the value it submits rather than for a missing resource.
async fn with_fixture_configuration(case: &str) -> Emulator {
    let emulator = Emulator::new();
    let created = emulator
        .create_in_registered_workspace(create_body(TEAM_ID, CHANNEL_ID))
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected the fixture create to answer HTTP 200, got {} / {}",
        created.status, created.body
    );
    emulator
}

#[tokio::test]
async fn the_crate_states_the_members_declared_pattern() {
    // The expression the crate applies is the one the model declares, so a drift in it fails
    // here rather than passing unnoticed through the rows below.
    assert_eq!(
        SLACK_ID_PATTERN, SLACK_ID_DECLARED_PATTERN,
        "case \"the declared expression\": expected {SLACK_ID_DECLARED_PATTERN:?}, got \
         {SLACK_ID_PATTERN:?}"
    );
    // And the clause states it with the anchor pair dropped, which is this service's shared
    // rendering of a reported `@pattern`.
    let stated = ::service_common::validation::unanchored(SLACK_ID_PATTERN);
    let expected = without_the_anchor_pair(SLACK_ID_DECLARED_PATTERN);
    assert_eq!(
        stated, expected,
        "case \"the stated expression\": expected {expected:?}, got {stated:?}"
    );
}

#[tokio::test]
async fn a_create_whose_channel_id_the_pattern_rejects_records_nothing() {
    for (case, submitted) in ids_outside_the_members_pattern(CHANNEL_ID) {
        // The workspace the create names is registered first, so the only thing wrong with
        // the request is the character the expression cannot match.
        let emulator = Emulator::new();
        let reply = emulator
            .create_in_registered_workspace(create_body(TEAM_ID, &submitted))
            .await;
        assert_pattern_refusal(&case, "channelId", &submitted, &reply);

        let listed = emulator.listed(TEAM_ID, &submitted).await;
        assert!(
            listed.is_none(),
            "case {case:?} ({submitted:?}): expected no configuration to be recorded, got \
             {listed:?}"
        );
        let whole_listing = emulator.list(None).await.configurations();
        assert!(
            whole_listing.is_empty(),
            "case {case:?} ({submitted:?}): expected the listing to report nothing, got \
             {whole_listing:?}"
        );
    }
}

#[tokio::test]
async fn a_create_whose_team_id_the_pattern_rejects_records_nothing() {
    for (case, submitted) in ids_outside_the_members_pattern(TEAM_ID) {
        let emulator = Emulator::new();
        let reply = emulator.create(create_body(&submitted, CHANNEL_ID)).await;
        assert_pattern_refusal(&case, "teamId", &submitted, &reply);

        let whole_listing = emulator.list(None).await.configurations();
        assert!(
            whole_listing.is_empty(),
            "case {case:?} ({submitted:?}): expected the listing to report nothing, got \
             {whole_listing:?}"
        );
    }
}

#[tokio::test]
async fn every_operation_carrying_the_members_refuses_one_value_the_same_way() {
    // One value, one answer: the rows for the characters Unicode also calls `White_Space` are
    // the ones the request boundary's own engine catches, and the U+FEFF row is caught by the
    // rule the handlers apply, so a layer that worded its refusal its own way could not agree
    // with the other.
    for (case, submitted) in one_id_per_excluded_character(CHANNEL_ID) {
        let emulator = with_fixture_configuration(&case).await;

        // Both members, through every operation that carries them: the create, the update and
        // the delete name a channel configuration by its pair; the registration and the
        // workspace delete name a workspace by its `teamId` alone.
        let answers: Vec<(&str, &str, Reply)> = vec![
            (
                "create",
                "channelId",
                emulator.create(create_body(TEAM_ID, &submitted)).await,
            ),
            (
                "update",
                "channelId",
                emulator.update(update_body(TEAM_ID, &submitted)).await,
            ),
            (
                "delete",
                "channelId",
                emulator.delete(TEAM_ID, &submitted).await,
            ),
            (
                "create (teamId)",
                "teamId",
                emulator.create(create_body(&submitted, CHANNEL_ID)).await,
            ),
            (
                "update (teamId)",
                "teamId",
                emulator.update(update_body(&submitted, CHANNEL_ID)).await,
            ),
            (
                "delete (teamId)",
                "teamId",
                emulator.delete(&submitted, CHANNEL_ID).await,
            ),
            (
                "register workspace",
                "teamId",
                emulator
                    .call(
                        "register-slack-workspace-for-organization",
                        json!({ "teamId": &submitted }),
                    )
                    .await,
            ),
            (
                "delete workspace",
                "teamId",
                emulator.delete_workspace(&submitted).await,
            ),
        ];
        for (operation, member, reply) in &answers {
            assert_pattern_refusal(&format!("{case} on the {operation}"), member, &submitted, reply);
        }

        // And none of them touched the stored state: the fixture configuration and its
        // workspace are still exactly as they were created.
        let listed = emulator.listed(TEAM_ID, CHANNEL_ID).await;
        assert!(
            listed.is_some(),
            "case {case:?} ({submitted:?}): expected the fixture configuration to be untouched, \
             got {listed:?}"
        );
        let workspaces = reported_team_ids(&reported(&list_workspaces(&emulator, None).await));
        assert_eq!(
            workspaces,
            vec![TEAM_ID.to_string()],
            "case {case:?} ({submitted:?}): expected the registry to be untouched, got \
             {workspaces:?}"
        );
    }
}

#[tokio::test]
async fn a_create_whose_whole_pair_is_outside_the_pattern_states_both_clauses() {
    // Two members of one request, each outside its own declared expression: one counted
    // envelope naming both clauses, in the order the model declares the members — the same
    // answer whichever layer read the expression, so the boundary's own engine (the space
    // row) and the rule the handler applies (the U+FEFF row) agree on the shape of it.
    for (case, character) in [
        ("a space in both ids", '\u{0020}'),
        ("the zero width no-break space U+FEFF in both ids", '\u{FEFF}'),
        ("a line feed in both ids", '\u{000A}'),
    ] {
        let emulator = Emulator::new();
        let team_id = format!("T012{character}ABCDEFG");
        let channel_id = format!("C0123{character}45");
        let reply = emulator
            .create(create_body(&team_id, &channel_id))
            .await;
        let message = refusal(case, &reply, 400, "ValidationException");
        let stated = without_the_anchor_pair(SLACK_ID_DECLARED_PATTERN);
        let expected = format!(
            "2 validation errors detected: Value '{team_id}' at 'teamId' failed to satisfy \
             constraint: Member must satisfy regular expression pattern: {stated}; Value \
             '{channel_id}' at 'channelId' failed to satisfy constraint: Member must satisfy \
             regular expression pattern: {stated}"
        );
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn ids_the_pattern_admits_round_trip_through_every_path() {
    for (case, id) in ids_the_pattern_admits() {
        let emulator = Emulator::new();

        // The id as a `channelId`, created and reported back verbatim by the listing.
        let created = emulator
            .create_in_registered_workspace(create_body(TEAM_ID, &id))
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?} ({id:?}): expected create to answer HTTP 200, got {} / {}",
            created.status, created.body
        );
        let listed = emulator.listed(TEAM_ID, &id).await;
        assert!(
            listed.is_some(),
            "case {case:?} ({id:?}): expected the created configuration to be reported, got \
             {listed:?}"
        );

        // The other read path of the same members: the update answers with the pair it
        // persisted under.
        let updated = emulator.update(update_body(TEAM_ID, &id)).await;
        assert_eq!(
            (updated.status, updated.string("channelId")),
            (200, Some(id.clone())),
            "case {case:?}: expected the update to answer with the stored channelId, got {} / {}",
            updated.status,
            updated.body
        );

        // And the id as a `teamId`: registered, then reported by the workspace listing.
        let registered = emulator.register_workspace(&id).await;
        assert_eq!(
            registered.status, 200,
            "case {case:?} ({id:?}): expected the registration to answer HTTP 200, got {} / {}",
            registered.status, registered.body
        );
        let workspaces = reported_team_ids(&reported(&list_workspaces(&emulator, None).await));
        assert!(
            workspaces.contains(&id),
            "case {case:?} ({id:?}): expected the registry to report the workspace, got \
             {workspaces:?}"
        );

        // A delete of a pair the members admit is the resource rule, not a refusal of the
        // value: the configuration this account holds goes, and a second delete is the miss.
        let deleted = emulator.delete(TEAM_ID, &id).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?} ({id:?}): expected the delete to answer HTTP 200, got {} / {}",
            deleted.status, deleted.body
        );
        let repeated = emulator.delete(TEAM_ID, &id).await;
        let message = refusal(case, &repeated, 404, "ResourceNotFoundException");
        assert_eq!(
            message,
            not_found(TEAM_ID, &id),
            "case {case:?} ({id:?}): expected the miss template, got {message:?}"
        );
    }
}

#[tokio::test]
async fn the_rest_of_the_members_rule_stays_at_the_request_boundary() {
    // The members' declared `@length` is the request boundary's, on both bounds and past the
    // upper one: an id of 256 characters is served (above), one of 257 is refused with the
    // length clause alone, and an empty id is short AND outside the expression, so both
    // clauses are reported in one counted envelope.
    let stated = without_the_anchor_pair(SLACK_ID_DECLARED_PATTERN);
    let past_the_maximum = "C".repeat(SLACK_ID_MAX_LEN + 1);
    let emulator = Emulator::new();

    // One past the upper bound: the boundary's own length clause, and no clause about the
    // expression — the value is made only of characters `\S` matches. The clause quotes the
    // submitted value under the boundary's own echo ceiling, so the row asserts the clause
    // the bound draws rather than how long an echo that layer keeps.
    let case = "a channelId one character past the published maximum";
    let reply = emulator.create(create_body(TEAM_ID, &past_the_maximum)).await;
    let message = refusal(case, &reply, 400, "ValidationException");
    let expected_clause =
        format!("at 'channelId' failed to satisfy constraint: Member must have length less than or \
                 equal to {SLACK_ID_MAX_LEN}");
    assert!(
        message.starts_with("1 validation error detected: ") && message.ends_with(&expected_clause),
        "case {case:?}: expected one {expected_clause:?} clause, got {message:?}"
    );
    assert!(
        !message.contains("regular expression pattern"),
        "case {case:?}: expected no pattern clause, got {message:?}"
    );

    // And the lower bound, where the empty value is short AND outside the expression: both
    // clauses in one counted envelope.
    let case = "an empty channelId";
    let expected = format!(
        "2 validation errors detected: Value '' at 'channelId' failed to satisfy constraint: \
         Member must have length greater than or equal to {SLACK_ID_MIN_LEN}; Value '' at \
         'channelId' failed to satisfy constraint: Member must satisfy regular expression \
         pattern: {stated}"
    );
    let reply = emulator.create(create_body(TEAM_ID, "")).await;
    let message = refusal(case, &reply, 400, "ValidationException");
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );
}

#[tokio::test]
async fn a_listing_token_the_pattern_rejects_is_refused_as_the_declared_constraint() {
    // `paginationToken` declares the same expression, so the same rule holds for the member
    // both listings carry it as: a token outside it is the declared-constraint refusal,
    // whichever layer read the expression, and a token the expression ADMITS is refused by
    // this service's own pagination rule instead, which quotes no expression at all.
    let emulator = Emulator::new();
    for (case, token) in one_id_per_excluded_character("Zm9yZ2Vk") {
        let channel_listing = emulator.list(Some(&token)).await;
        assert_pattern_refusal(
            &format!("{case} on the channel listing"),
            "nextToken",
            &token,
            &channel_listing,
        );
        let workspace_listing = list_workspaces(&emulator, Some(&token)).await;
        assert_pattern_refusal(
            &format!("{case} on the workspace listing"),
            "nextToken",
            &token,
            &workspace_listing,
        );
    }

    let stated = without_the_anchor_pair(SLACK_ID_DECLARED_PATTERN);
    for (case, reply) in [
        ("a well-formed token on the channel listing", emulator.list(Some("Zm9yZ2Vk")).await),
        (
            "a well-formed token on the workspace listing",
            list_workspaces(&emulator, Some("Zm9yZ2Vk")).await,
        ),
    ] {
        let message = refusal(case, &reply, 400, "ValidationException");
        assert!(
            !message.contains(stated) && !message.contains("regular expression pattern"),
            "case {case:?}: expected no pattern clause, got {message:?}"
        );
    }
}
