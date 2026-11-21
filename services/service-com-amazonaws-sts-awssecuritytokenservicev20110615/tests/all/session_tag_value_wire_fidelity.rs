// SPDX-License-Identifier: CC0-1.0

//! What a session tag is once it has crossed the wire: the text the request sent,
//! codepoint for codepoint.
//!
//! RULE: a session tag's key and value arrive as one form component each, percent-encoded
//! byte-wise over their UTF-8 ("plain text key-value pairs", `AssumeRoleRequest$Tags`), and
//! the mint records them as that text decoded and nothing else — no Unicode normalization,
//! no compatibility or case folding, no re-encoding through a narrower character set — so
//! every record the session is later read through carries the submitted text: the session
//! registry this service keeps, the role-session record an authorization decision about the
//! credential is made from, and the session a chained assume mints from a transitive key.
//! Two values a Unicode folding would equate are therefore two values here.
//!
//! The rows differ along what the rule generalises over: the character classes the value is
//! spelled in — `Tag$Value`'s `@pattern ^[\p{L}\p{Z}\p{N}_.:/=+\-@]*$` admits every Unicode
//! letter, separator and number plus that punctuation set — whether its letters live inside
//! the basic multilingual plane, whether a compatibility or case folding would equate it
//! with another row's value, and its length up to the modelled maximum of 256 characters.
//!
//! The comparison a policy clause then makes over `aws:PrincipalTag/<key>` is the other half
//! of the round trip, and it takes two services to state: it is pinned by
//! `core/router/tests/all/iam_sts_unicode_tag_clause_spelling.rs` and, for this service's own
//! trust-policy decision, by `session_tag_conditions.rs` beside this file.

use std::collections::BTreeSet;

use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;

use super::sts_sessions::{
    call, emulator, member, now_secs, seed_role, trust_policy, LONG_TERM_KEY,
};

/// The tag key every row writes, itself spelled outside ASCII so the key's own decoding is
/// asserted beside the value's — `Tag$Key`'s pattern admits the same character classes.
const TAG_KEY: &str = "équipe";

/// The session tag values, one row each: `(case, the value the request sends)`.
///
/// Every one of them is a value `Tag$Value` admits
/// (`smithy_query(query="shape",shape="Tag")`): Unicode letters (`\p{L}`), separators
/// (`\p{Z}`), numbers (`\p{N}`) and the punctuation set `_.:/=+\-@`.
fn rows() -> Vec<(&'static str, String)> {
    vec![
        (
            "accented Latin and CJK letters",
            "Café-Zürich-北京".to_string(),
        ),
        (
            "the same letters with the diacritics stripped",
            "Cafe-Zurich-北京".to_string(),
        ),
        ("Greek and Cyrillic letters", "Ωμέγα-команда".to_string()),
        (
            // U+FB01, a letter whose compatibility decomposition is the two ASCII letters
            // of the row below it.
            "a ligature a compatibility folding would spell as two letters",
            "ﬁ-équipe".to_string(),
        ),
        (
            "those two letters themselves",
            "fi-équipe".to_string(),
        ),
        (
            // U+FF21, a letter whose compatibility decomposition is the ASCII letter of
            // the row below it.
            "a full-width letter",
            "Ａ-team".to_string(),
        ),
        ("its ASCII counterpart", "A-team".to_string()),
        (
            // U+216B, category Nl, which `\p{N}` admits; folds to three ASCII letters.
            "a Roman numeral",
            "Ⅻ-team".to_string(),
        ),
        ("the letters that spell it", "XII-team".to_string()),
        (
            "an upper-cased accented value",
            "ÉQUIPE".to_string(),
        ),
        ("its lower-cased form", "équipe".to_string()),
        (
            // U+20000, a letter outside the basic multilingual plane, so its UTF-8 is four
            // bytes and its UTF-16 a surrogate pair.
            "a letter outside the basic multilingual plane",
            "Zürich-𠀀".to_string(),
        ),
        (
            "letters, a separator space and digits",
            "équipe 42".to_string(),
        ),
        (
            "every punctuation character the pattern admits",
            "a_b.c:d/e=f+g-h@i".to_string(),
        ),
        (
            // The modelled maximum, counted in CHARACTERS: 256 of them, 512 bytes of UTF-8.
            "the modelled maximum of 256 characters, each outside ASCII",
            "é".repeat(256),
        ),
    ]
}

/// RULE: the mint stores a session tag as the text the request sent, codepoint for
/// codepoint, in every record the session is read through — this service's registry, the
/// role-session record an authorization decision is made from, and the session a chained
/// assume mints from a transitive key — and two values a Unicode folding would equate stay
/// two values.
#[tokio::test]
async fn a_session_tag_crosses_the_wire_codepoint_for_codepoint() {
    // Every stored value, so the rows that a folding would equate can be asserted distinct
    // once every row has been minted.
    let mut stored_values: Vec<String> = Vec::new();
    for (index, (case, value)) in rows().into_iter().enumerate() {
        let (router, engine) = emulator();
        let (first_arn, _) = seed_role(
            &engine,
            &format!("AROASEEDEDWIREFIDEL{index:03}"),
            &format!("wire-fidelity-first-{index}"),
            "/",
            Some(&trust_policy(&["sts:AssumeRole", "sts:TagSession"])),
            Some(43_200),
        );
        let (second_arn, _) = seed_role(
            &engine,
            &format!("AROASEEDEDWIRECHAIN{index:03}"),
            &format!("wire-fidelity-second-{index}"),
            "/",
            Some(&trust_policy(&["sts:AssumeRole", "sts:TagSession"])),
            Some(43_200),
        );

        let (status, body) = call(
            &router,
            LONG_TERM_KEY,
            "AssumeRole",
            &[
                ("RoleArn", first_arn),
                ("RoleSessionName", format!("wire-{index}")),
                ("Tags.member.1.Key", TAG_KEY.to_string()),
                ("Tags.member.1.Value", value.clone()),
                ("TransitiveTagKeys.member.1", TAG_KEY.to_string()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the tagged mint to be served, got {status} {body}"
        );
        let access_key_id = member(case, &body, "AccessKeyId");

        let expected = vec![(TAG_KEY.to_string(), value.clone())];
        let held = session_tags(&engine, &access_key_id);
        assert_eq!(
            held, expected,
            "case {case:?}: expected the minted session to hold {expected:?}, got {held:?}"
        );
        // Stated again over the CODEPOINTS, so a value that merely renders the same — a
        // normalized or folded form of the submitted text — fails here rather than passing
        // a comparison of two mangled strings.
        let submitted: Vec<char> = value.chars().collect();
        let recorded: Vec<char> = held
            .first()
            .map(|(_, held)| held.chars().collect())
            .unwrap_or_default();
        assert_eq!(
            recorded, submitted,
            "case {case:?}: expected the stored value's codepoints to be the submitted ones, \
             got {recorded:?}"
        );

        // The record an authorization decision about this credential is made from: the tags
        // travel into it, so what `aws:PrincipalTag/<key>` answers is this same text.
        let published = published_tags(&engine, &access_key_id);
        assert_eq!(
            published, expected,
            "case {case:?}: expected the published role-session record to carry {expected:?}, \
             got {published:?}"
        );

        // And the session a chained assume mints from the transitive key: the copy carries
        // the text forward unchanged.
        let (status, body) = call(
            &router,
            &access_key_id,
            "AssumeRole",
            &[
                ("RoleArn", second_arn),
                ("RoleSessionName", format!("chained-{index}")),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the chained mint to be served, got {status} {body}"
        );
        let chained_key = member(case, &body, "AccessKeyId");
        let chained = session_tags(&engine, &chained_key);
        assert_eq!(
            chained, expected,
            "case {case:?}: expected the chained session to carry {expected:?}, got {chained:?}"
        );

        stored_values.push(held.first().map(|(_, held)| held.clone()).unwrap_or_default());
    }

    let distinct: BTreeSet<&String> = stored_values.iter().collect();
    assert_eq!(
        distinct.len(),
        stored_values.len(),
        "expected every row's value to be stored as its own text, got {stored_values:?}"
    );
}

/// The session tags this service's registry holds for the credential `access_key_id`.
fn session_tags(engine: &engine::InMemoryEngine, access_key_id: &str) -> Vec<(String, String)> {
    engine
        .state::<sts::ComAmazonawsStsAwssecuritytokenservicev20110615State>()
        .lock()
        .session(access_key_id)
        .map(|session| session.tags.clone())
        .unwrap_or_default()
}

/// The session tags the role-session record published for `access_key_id` carries — the
/// record a service deciding a request signed with that credential reads the principal's
/// tags out of.
fn published_tags(engine: &engine::InMemoryEngine, access_key_id: &str) -> Vec<(String, String)> {
    engine
        .state::<iam::ComAmazonawsIamAwsidentitymanagementv20100508State>()
        .lock()
        .role_session(
            access_key_id,
            aws_smithy_types::DateTime::from_secs(now_secs()),
        )
        .map(|session| session.tags.clone())
        .unwrap_or_default()
}
