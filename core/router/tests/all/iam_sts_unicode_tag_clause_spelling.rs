// SPDX-License-Identifier: CC0-1.0

//! How a `StringEquals` clause over `aws:PrincipalTag/<key>` is compared with the tag a
//! session carries, when either side is spelled outside ASCII: two services, one decision.
//!
//! RULE: the two sides of the comparison are each DECODED from the wire spelling that
//! carried them — the session tag from the form component's percent-encoded UTF-8, the
//! clause's string from the policy document's JSON text, where a character may stand
//! literally or as a `\uXXXX` escape (a surrogate pair outside the basic multilingual
//! plane) — and are then compared codepoint for codepoint. So a clause holds for exactly
//! those sessions whose decoded tag value IS the clause's decoded string, however each side
//! was spelled: the admitted call is answered (HTTP 200) and every other session is refused
//! `AccessDenied` / HTTP 403. Nothing is folded before the comparison: a letter is not its
//! compatibility decomposition, a full-width letter is not its ASCII counterpart, a numeral
//! is not the letters that spell it, and an upper-cased value is not its lower-cased form.
//!
//! IAM holds the role and its conditioned document while STS mints the tagged session, so
//! neither crate's own test target can tag a session and then make a decision about it;
//! this is the suite that can pin it. `iam_session_tag_authorization.rs` beside this file
//! states the rule over the VALUE a session carries, with every document written in one
//! spelling; this suite varies the SPELLING of the document and the folding that must not be
//! applied, and reuses that suite's estate helpers rather than building a second copy of
//! them. The storage half of the round trip — the tag as the mint records it — is pinned by
//! the STS crate's own `session_tag_value_wire_fidelity.rs`.

use crate::iam_role_id_agrees_with_sts::{build_router, LONG_TERM_KEY};
use crate::iam_session_tag_authorization::{
    ascii_json, assert_lists, assert_withheld, assume_role, create_role, list_roles,
    put_role_policy, ACTION, TAG_KEY,
};

/// One identity-based statement granting [`ACTION`] everywhere under a `StringEquals` clause
/// over `aws:PrincipalTag/<key>`, with `written` placed inside the clause's JSON string
/// exactly as given: the SPELLING of the value is what this suite varies, so the document is
/// written out here rather than serialized from a value.
fn conditioned(written: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Action\":\"{ACTION}\",\"Resource\":\"*\",\"Condition\":{{\"StringEquals\":\
         {{\"aws:PrincipalTag/{TAG_KEY}\":\"{written}\"}}}}}}]}}"
    )
}

/// `text` with every character the policy document's own character set does not carry
/// written as a JSON `\uXXXX` escape, and every other character left literal.
///
/// This is the spelling a client sends when it serializes a document without asking for
/// ASCII output: `policyDocumentType`'s `@pattern
/// ^[\u0009\u000A\u000D\u0020-\u00FF]+$` admits the Basic Latin and Latin-1 Supplement
/// characters literally — "the printable characters in the Basic Latin and Latin-1
/// Supplement character set (through \u00FF)" (`PutRolePolicyRequest$PolicyDocument`) — so
/// an accented Latin letter travels as itself and anything beyond that range travels as an
/// escape.
fn latin1_json(text: &str) -> String {
    let mut written = String::with_capacity(text.len());
    for character in text.chars() {
        if character <= '\u{00FF}' {
            written.push(character);
        } else {
            written.push_str(&ascii_json(&character.to_string()));
        }
    }
    written
}

/// One row about the SPELLING of the clause: the value the session is tagged with, how the
/// document spells that value, and a value differing from it by one codepoint that the same
/// clause must not hold for.
struct Spelling {
    case: &'static str,
    /// The tag value, which always travels as percent-encoded UTF-8 — what an SDK's form
    /// encoder sends.
    value: &'static str,
    /// The same value as the document spells it inside the clause's JSON string.
    written: String,
    /// The value the control session carries.
    other: &'static str,
}

/// The rows: one document spelling each, differing along what the rule generalises over —
/// whether a character stands literally or as an escape, whether it is inside the character
/// set the document admits literally, and whether it is inside the basic multilingual plane.
fn spellings() -> Vec<Spelling> {
    vec![
        Spelling {
            case: "every character outside ASCII escaped, as a serializer asked for ASCII \
                   output writes it",
            value: "Café-Zürich-北京",
            written: ascii_json("Café-Zürich-北京"),
            other: "Cafe-Zurich-北京",
        },
        Spelling {
            case: "the characters standing literally, inside the set the document admits",
            value: "Café-Zürich",
            written: "Café-Zürich".to_string(),
            other: "Cafe-Zurich",
        },
        Spelling {
            case: "literal where the document admits the character and escaped beyond it",
            value: "Café-Zürich-北京",
            written: latin1_json("Café-Zürich-北京"),
            other: "Café-Zürich-東京",
        },
        Spelling {
            case: "a letter outside the basic multilingual plane, escaped as a surrogate pair",
            value: "Zürich-𠀀",
            written: ascii_json("Zürich-𠀀"),
            other: "Zürich-𠀁",
        },
        Spelling {
            case: "a separator space and digits beside the accented letters",
            value: "équipe 42",
            written: latin1_json("équipe 42"),
            other: "équipe 43",
        },
    ]
}

/// RULE: the clause's string and the session's tag are each decoded from the spelling that
/// carried them and then compared codepoint for codepoint, so a clause written with escapes,
/// with literal characters, or with a mixture of the two is satisfied by the session whose
/// decoded tag value is that same text — and by no other session.
#[tokio::test]
async fn a_principal_tag_clause_holds_however_the_document_spells_its_value() {
    let router = build_router();
    for (index, row) in spellings().into_iter().enumerate() {
        let Spelling {
            case,
            value,
            written,
            other,
        } = row;
        let role_name = format!("spelled-clause-role-{index}");
        let role_arn = create_role(&router, &role_name).await;
        put_role_policy(&router, &role_name, &conditioned(&written)).await;

        let exact = assume_role(
            &router,
            LONG_TERM_KEY,
            &role_arn,
            "exact-session",
            &[(TAG_KEY, value)],
            &[],
        )
        .await;
        let mismatched = assume_role(
            &router,
            LONG_TERM_KEY,
            &role_arn,
            "mismatched-session",
            &[(TAG_KEY, other)],
            &[],
        )
        .await;

        let (status, body) = list_roles(&router, &exact).await;
        assert_lists(case, &role_name, status, &body);
        let (status, body) = list_roles(&router, &mismatched).await;
        assert_withheld(case, &role_name, "mismatched-session", status, &body);
    }
}

/// One row about a folding that must NOT be applied: the value the clause tests for, the
/// value the session is tagged with, and whether the call is answered.
struct Folding {
    case: &'static str,
    /// The clause's value.
    clause: &'static str,
    /// The session's tag value.
    carried: &'static str,
    /// Set when the clause holds for that session.
    admitted: bool,
}

/// The rows: a pair per folding — the session carrying the clause's own text, which is
/// admitted, and the session carrying the text that folding would equate with it, which is
/// not.
///
/// The characters are ones `Tag$Value`'s pattern admits (`\p{L}`, `\p{N}`): U+FB01, whose
/// compatibility decomposition is `fi`; U+FF21, whose compatibility decomposition is `A`;
/// U+216B, whose compatibility decomposition is `XII`; and an accented letter in both cases.
fn foldings() -> Vec<Folding> {
    vec![
        Folding {
            case: "a ligature is not the two letters it decomposes to",
            clause: "ﬁ-équipe",
            carried: "fi-équipe",
            admitted: false,
        },
        Folding {
            case: "the ligature itself",
            clause: "ﬁ-équipe",
            carried: "ﬁ-équipe",
            admitted: true,
        },
        Folding {
            case: "a full-width letter is not its ASCII counterpart",
            clause: "Ａ-team",
            carried: "A-team",
            admitted: false,
        },
        Folding {
            case: "the full-width letter itself",
            clause: "Ａ-team",
            carried: "Ａ-team",
            admitted: true,
        },
        Folding {
            case: "a Roman numeral is not the letters that spell it",
            clause: "Ⅻ-team",
            carried: "XII-team",
            admitted: false,
        },
        Folding {
            case: "the numeral itself",
            clause: "Ⅻ-team",
            carried: "Ⅻ-team",
            admitted: true,
        },
        Folding {
            case: "an accented value's case is not folded away",
            clause: "ÉQUIPE",
            carried: "équipe",
            admitted: false,
        },
        Folding {
            case: "the same casing",
            clause: "ÉQUIPE",
            carried: "ÉQUIPE",
            admitted: true,
        },
    ]
}

/// RULE: the comparison folds nothing — no compatibility decomposition and no case folding
/// over Unicode — so a session tagged with a value a folding would equate with the clause's
/// string is refused `AccessDenied`, while the session carrying the clause's own text is
/// answered.
#[tokio::test]
async fn a_principal_tag_clause_folds_nothing_before_comparing() {
    let router = build_router();
    for (index, row) in foldings().into_iter().enumerate() {
        let Folding {
            case,
            clause,
            carried,
            admitted,
        } = row;
        let role_name = format!("unfolded-clause-role-{index}");
        let role_arn = create_role(&router, &role_name).await;
        put_role_policy(&router, &role_name, &conditioned(&ascii_json(clause))).await;

        let session_name = format!("folding-session-{index}");
        let key = assume_role(
            &router,
            LONG_TERM_KEY,
            &role_arn,
            &session_name,
            &[(TAG_KEY, carried)],
            &[],
        )
        .await;
        let (status, body) = list_roles(&router, &key).await;
        if admitted {
            assert_lists(case, &role_name, status, &body);
        } else {
            assert_withheld(case, &role_name, &session_name, status, &body);
        }
    }
}
