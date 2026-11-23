// SPDX-License-Identifier: CC0-1.0

//! WHICH ACCOUNT ALIASES the member's own `@pattern` admits, over the wire.
//!
//! THE RULE UNDER TEST: a modelled `@pattern` is an ECMA-262 regular expression, and there
//! `\w` is the ASCII class `[A-Za-z0-9_]` — a letter of another script, a digit of another
//! numbering system, a combining mark and a connector punctuation character are none of its
//! members. `accountAlias` declares `^[\w\- ]+$`
//! (`smithy_query(query="shape",shape="awsAccountAlias")`), so a value carrying any
//! character outside that class, the escaped literal hyphen and the space is outside the
//! member's declared expression however long it is, and the operation that WRITES the member
//! refuses it with the modelled `ValidationException` (HTTP 400), in the standard validation
//! envelope this service states a declared constraint in, and records nothing for it. Every
//! value made only of those characters is one the member admits.
//!
//! Why a handler has to be a layer that says so: the request boundary's constraint walk
//! compiles a `@pattern` with an engine whose `\w` is the UNICODE word class, so every
//! non-ASCII word character would be admitted there whatever the modelled expression says —
//! and the SAME shape, with the same `@pattern`, is an OUTPUT member: `GetAccountAlias`
//! answers the stored alias through `GetAccountAliasResult$accountAlias`, so a value the
//! expression does not admit could not be reported back through the shape it was stored in.
//! The read path is therefore driven here on both sides of the rule: an admitted alias is
//! reported back verbatim, and a refused one leaves the account's alias exactly as it was.
//!
//! The tables are that rule, not one recorded request. The refused rows are the cross
//! product of a non-ASCII word character of each KIND the Unicode class carries (a letter
//! with a diacritic, letters of three other scripts, a digit of another numbering system, a
//! combining mark, a connector punctuation character) with every place one can sit in an
//! otherwise well-formed alias — at its start, inside it, at its end, as the whole of it,
//! twice over, and inside an alias at the member's published `@length` maximum. Characters
//! outside the class that are plain ASCII are rows too, and every row is required to be
//! answered the same way byte for byte, so the refusal cannot come from one layer's reading
//! of the expression. The admitted rows are the boundary of the rule: every kind of
//! character the class does admit, and aliases at both published `@length` bounds.
//!
//! The REST of the member's rule stays where it already was — the request boundary applies
//! the `+`'s at-least-one and the member's declared `@length` 1..30 — which the last test
//! pins from the other side.

use service_com_amazonaws_supportapp_supportapp::state::ACCOUNT_ALIAS_PATTERN;

use crate::error_message_templates::pattern_refusal;
use crate::slack_channel_configurations::{Emulator, Reply};

/// `awsAccountAlias`'s declared `@pattern`, as the model spells it — the ANCHORED text
/// (`smithy_query(query="shape",shape="awsAccountAlias")`), written out here so the
/// expression this suite expects a clause to state is not read back off the code under test.
const ACCOUNT_ALIAS_DECLARED_PATTERN: &str = r"^[\w\- ]+$";

/// `awsAccountAlias`'s declared `@length` bounds, from the same shape.
const ACCOUNT_ALIAS_MIN_LEN: usize = 1;
const ACCOUNT_ALIAS_MAX_LEN: usize = 30;

/// The alias an account already holds when a row submits a refused one, so each refusal is
/// shown to leave the stored value alone rather than an empty store.
const STORED_ALIAS: &str = "keep-me";

/// Word characters of the UNICODE class that the modelled expression's ASCII `\w` does NOT
/// admit, one per KIND that class carries: an ASCII letter with a diacritic, letters of
/// three other scripts, a digit of another numbering system (category Nd), a combining mark
/// (category Mn) and a connector punctuation character (category Pc). Written out here from
/// the ECMA-262 reading of `\w` as `[A-Za-z0-9_]`, which is what
/// `smithy_query(query="shape",shape="awsAccountAlias")`'s `^[\w\- ]+$` means by it; none of
/// them is the `ü` the issue cited alone.
const NON_ASCII_WORD_CHARS: [(&str, char); 7] = [
    ("a Latin letter with a diaeresis", 'ü'),
    ("a Latin letter with a tilde", 'ñ'),
    ("a CJK ideograph", '日'),
    ("a Cyrillic letter", 'ж'),
    ("an Arabic-Indic digit", '٣'),
    ("a combining acute accent U+0301", '\u{0301}'),
    ("the undertie U+203F", '\u{203F}'),
];

/// Characters outside the expression's class that are plain ASCII — the side of the same
/// rule the request boundary's own engine already catches, included so both layers are
/// required to answer one refusal the same way.
const ASCII_CHARS_OUTSIDE_THE_CLASS: [(&str, char); 4] = [
    ("an exclamation mark", '!'),
    ("a slash", '/'),
    ("a tab", '\t'),
    ("a dot", '.'),
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

/// Where in an otherwise well-formed alias a character outside the expression's class sits.
/// Each entry turns the character into a whole value, so the cross product below spans both
/// axes the rule generalises over: WHICH character, and WHERE it is — including an alias at
/// the member's published `@length` maximum, where the only thing wrong with the value is
/// the character the class does not admit.
fn placements(character: char) -> Vec<(&'static str, String)> {
    let at_the_maximum: String = "a"
        .repeat(ACCOUNT_ALIAS_MAX_LEN - 1)
        .chars()
        .chain(std::iter::once(character))
        .collect();
    vec![
        ("at the start of the alias", format!("{character}my-team")),
        ("inside the alias", format!("my{character}team")),
        ("at the end of the alias", format!("my-team{character}")),
        ("as the whole alias", character.to_string()),
        (
            "twice over in the alias",
            format!("my{character}{character}team"),
        ),
        ("inside an alias at the published maximum", at_the_maximum),
    ]
}

/// Every refused value for the characters `outside` names: each of them in each placement.
fn aliases_outside_the_members_pattern(outside: &[(&str, char)]) -> Vec<(String, String)> {
    outside
        .iter()
        .flat_map(|(character_case, character)| {
            placements(*character)
                .into_iter()
                .map(move |(placement, submitted)| {
                    (format!("{character_case} {placement}"), submitted)
                })
        })
        .collect()
}

/// Aliases made only of characters the expression's class admits — an ASCII letter of either
/// case, an ASCII digit, an underscore, the literal hyphen and the space — and aliases at
/// both published `@length` bounds. Each is served and reported back verbatim.
fn aliases_the_pattern_admits() -> Vec<(&'static str, String)> {
    vec![
        ("a hyphenated alias", "my-team".to_string()),
        ("an alias carrying spaces", "Support Team 1".to_string()),
        ("an underscored alias", "under_score".to_string()),
        ("digits only", "0123456789".to_string()),
        ("hyphens only", "---".to_string()),
        ("spaces only", "   ".to_string()),
        (
            "every admitted kind of character at once",
            "aZ_9 -team".to_string(),
        ),
        (
            "an alias at the published minimum",
            "a".repeat(ACCOUNT_ALIAS_MIN_LEN),
        ),
        (
            "an alias at the published maximum",
            "z".repeat(ACCOUNT_ALIAS_MAX_LEN),
        ),
    ]
}

/// The clause a value outside the member's declared `@pattern` is refused with: the standard
/// single-violation envelope, naming the submitted value, the member path in the spelling
/// the envelope states a member with, and the expression WITHOUT the anchors the model
/// brackets it with.
fn expected_refusal(submitted: &str) -> String {
    pattern_refusal(
        submitted,
        "accountAlias",
        without_the_anchor_pair(ACCOUNT_ALIAS_DECLARED_PATTERN),
    )
}

/// Require `reply` to be the modelled refusal of a declared constraint on `accountAlias`.
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

/// The alias `GetAccountAlias` reports, or `None` when the account holds none.
async fn reported_alias(case: &str, emulator: &Emulator) -> Option<String> {
    let got = emulator.get_alias().await;
    assert_eq!(
        got.status, 200,
        "case {case:?}: expected the get to answer HTTP 200, got {} / {}",
        got.status, got.body
    );
    got.string("accountAlias")
}

#[tokio::test]
async fn the_crate_states_the_members_declared_pattern() {
    // The expression the crate applies is the one the model declares, so a drift in it fails
    // here rather than passing unnoticed through the rows below.
    assert_eq!(
        ACCOUNT_ALIAS_PATTERN, ACCOUNT_ALIAS_DECLARED_PATTERN,
        "case \"the declared expression\": expected {ACCOUNT_ALIAS_DECLARED_PATTERN:?}, got \
         {ACCOUNT_ALIAS_PATTERN:?}"
    );
    // And the clause states it with the anchor pair dropped, which is this service's shared
    // rendering of a reported `@pattern`.
    let stated = ::service_common::validation::unanchored(ACCOUNT_ALIAS_PATTERN);
    let expected = without_the_anchor_pair(ACCOUNT_ALIAS_DECLARED_PATTERN);
    assert_eq!(
        stated, expected,
        "case \"the stated expression\": expected {expected:?}, got {stated:?}"
    );
}

#[tokio::test]
async fn a_put_whose_alias_carries_a_non_ascii_word_character_is_refused() {
    for (case, submitted) in aliases_outside_the_members_pattern(&NON_ASCII_WORD_CHARS) {
        let emulator = Emulator::new();
        let stored = emulator.put_alias(STORED_ALIAS).await;
        assert_eq!(
            stored.status, 200,
            "case {case:?}: expected the fixture put to answer HTTP 200, got {} / {}",
            stored.status, stored.body
        );

        let reply = emulator.put_alias(&submitted).await;
        assert_pattern_refusal(&case, &submitted, &reply);

        // Nothing was written, so the read path cannot report a value the member's own
        // `@pattern` does not admit.
        let reported = reported_alias(&case, &emulator).await;
        assert_eq!(
            reported.as_deref(),
            Some(STORED_ALIAS),
            "case {case:?} ({submitted:?}): expected the stored alias to be unchanged, got \
             {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_refused_put_leaves_an_account_with_no_alias_at_all() {
    // The same rule on an account that holds nothing yet: a refused put mints no alias, so
    // the read path reports the absent member rather than a value outside the expression.
    for (case, submitted) in aliases_outside_the_members_pattern(&NON_ASCII_WORD_CHARS) {
        let emulator = Emulator::new();
        let reply = emulator.put_alias(&submitted).await;
        assert_pattern_refusal(&case, &submitted, &reply);

        let reported = reported_alias(&case, &emulator).await;
        assert_eq!(
            reported, None,
            "case {case:?} ({submitted:?}): expected no alias to be recorded, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn every_character_outside_the_class_is_refused_the_same_way() {
    // One template, whichever reading of the expression caught the value: the ASCII rows are
    // what the request boundary's own engine already catches and the non-ASCII rows are
    // caught by the rule the handler applies, so a layer that worded its refusal its own way
    // could not agree with the other.
    let mut rows = aliases_outside_the_members_pattern(&ASCII_CHARS_OUTSIDE_THE_CLASS);
    rows.extend(aliases_outside_the_members_pattern(&NON_ASCII_WORD_CHARS));
    let emulator = Emulator::new();
    for (case, submitted) in rows {
        let reply = emulator.put_alias(&submitted).await;
        let answered = (
            reply.status,
            reply.error_type.clone(),
            reply.string("message"),
        );
        let expected = (
            400,
            "ValidationException".to_string(),
            Some(expected_refusal(&submitted)),
        );
        assert_eq!(
            answered, expected,
            "case {case:?} ({submitted:?}): expected {expected:?}, got {answered:?}"
        );
    }
}

#[tokio::test]
async fn an_alias_the_pattern_admits_round_trips_through_put_and_get() {
    for (case, alias) in aliases_the_pattern_admits() {
        let emulator = Emulator::new();
        let put = emulator.put_alias(&alias).await;
        assert_eq!(
            put.status, 200,
            "case {case:?} ({alias:?}): expected the put to answer HTTP 200, got {} / {}",
            put.status, put.body
        );

        let reported = reported_alias(case, &emulator).await;
        assert_eq!(
            reported.as_deref(),
            Some(alias.as_str()),
            "case {case:?}: expected the persisted alias to be reported, got {reported:?}"
        );

        // And a second put replaces it with another alias the expression admits, which the
        // same read path reports: the rule refuses nothing it should admit on either write.
        let replacement: String = format!("{alias} 2")
            .chars()
            .take(ACCOUNT_ALIAS_MAX_LEN)
            .collect();
        let put = emulator.put_alias(&replacement).await;
        assert_eq!(
            put.status, 200,
            "case {case:?} ({replacement:?}): expected the replacing put to answer HTTP 200, got \
             {} / {}",
            put.status, put.body
        );
        let reported = reported_alias(case, &emulator).await;
        assert_eq!(
            reported.as_deref(),
            Some(replacement.as_str()),
            "case {case:?}: expected the replacing alias to be reported, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn the_rest_of_the_members_rule_stays_at_the_request_boundary() {
    // The member is still judged by the request boundary for everything else it declares:
    // its `@length` maximum is reported in the standard envelope with no expression quoted
    // at all, so the rule above took nothing off that boundary.
    let emulator = Emulator::new();
    let past_the_maximum = "z".repeat(ACCOUNT_ALIAS_MAX_LEN + 1);
    let reply = emulator.put_alias(&past_the_maximum).await;
    assert_eq!(
        reply.status, 400,
        "case \"an alias one past the published maximum\": expected HTTP 400, got {} / {}",
        reply.status, reply.body
    );
    let message = reply.string("message").unwrap_or_else(|| {
        panic!(
            "case \"an alias one past the published maximum\": the refusal carries no message: {}",
            reply.body
        )
    });
    let expected = format!(
        "1 validation error detected: Value '{past_the_maximum}' at 'accountAlias' failed to \
         satisfy constraint: Member must have length less than or equal to {ACCOUNT_ALIAS_MAX_LEN}"
    );
    assert_eq!(
        message, expected,
        "case \"an alias one past the published maximum\": expected {expected:?}, got {message:?}"
    );
}
