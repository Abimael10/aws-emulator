// SPDX-License-Identifier: CC0-1.0

//! What a confidence the `BatchDetect*` analysers report MEANS: one value per
//! detection, computed from that detection's own evidence in its own document.
//!
//! Four rules, one per analyser of the batch family, each asserted over a SET of
//! documents rather than over one recorded answer:
//!
//! 1. `BatchDetectDominantLanguage`: the leading `Score` is the engine's own
//!    confidence in that language weighed against what the other candidates claim of
//!    the same material and the standing possibility of a language the engine does not
//!    model — so it rises with the text the identification was made from, falls when
//!    the engine's margin narrows, and never reaches 1. Two documents of comparable
//!    length the engine is differently sure about are therefore ordered by the margin
//!    and not by which is longer.
//! 2. `BatchDetectKeyPhrases`: every phrase carries its OWN confidence, tallied over
//!    that phrase's characters as the analysis read them, so two phrases of one
//!    document are not reported with one value and a phrase resting on a word the
//!    analysis left two readings open on is reported materially below one it settled.
//! 3. `BatchDetectSentiment`: the members the document does not support keep the room
//!    the DOCUMENT leaves them rather than one shared floor, so the residual members of
//!    two documents are not the same number.
//! 4. `BatchDetectSyntax`: a degree adverb grades an adjective and never a verb phrase,
//!    so the participle one grades in a copula's predicate is that predicate's
//!    ADJECTIVE and the copula is then the clause's own VERB — while the same
//!    participle ungraded is the predicate's passive verb and the copula its auxiliary.
//!    A token whose class its evidence and its position settle between them leaves no
//!    competing reading open and is reported at full confidence; one whose other
//!    reading stays live is reported below it.
//!
//! The rows differ along what the rules generalise over: language, document length and
//! clarity, phrase length and the construction a phrase stands in, polarity and how
//! much of the document carries it, and the position a participle, a copula and an
//! ambiguous word stand in.

use serde_json::{json, Value};

use crate::batch_inference_calibration::{batch_results, distinct};
use crate::comprehend_client::Comprehend;

// ---------------------------------------------------------------------------
// 1. BatchDetectDominantLanguage
// ---------------------------------------------------------------------------

/// The leading candidate's `LanguageCode` and `Score` for every document of one batch.
async fn leading_languages(comprehend: &Comprehend, texts: &[&str]) -> Vec<(String, f64)> {
    let results = batch_results(
        comprehend,
        "BatchDetectDominantLanguage",
        &json!({ "TextList": texts }),
    )
    .await;
    results
        .iter()
        .map(|entry| {
            let leading = entry["Languages"]
                .as_array()
                .and_then(|languages| languages.first().cloned())
                .unwrap_or_else(|| panic!("expected a leading candidate, got {entry}"));
            (
                leading["LanguageCode"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                leading["Score"].as_f64().unwrap_or(f64::NAN),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_leading_confidence_rises_with_the_text_the_identification_read() {
    let comprehend = Comprehend::new();

    // One language, one clause repeated: every row carries the same evidence per
    // character and more of it than the row above, so the confidence must rise with
    // the material and stop short of a certainty.
    let clause = "The weather in London is rather pleasant today. ";
    let documents: Vec<String> = (1..=5).map(|copies| clause.repeat(copies)).collect();
    let texts: Vec<&str> = documents.iter().map(String::as_str).collect();
    let leading = leading_languages(&comprehend, &texts).await;

    let mut previous = 0.0;
    for (copies, (code, score)) in leading.iter().enumerate() {
        let case = format!("{} copies of the clause", copies + 1);
        assert_eq!(
            code, "en",
            "case {case:?}: expected the document's own language, got {code:?}"
        );
        assert!(
            *score > previous,
            "case {case:?}: expected a confidence above the shorter document's \
             {previous}, got {score}"
        );
        assert!(
            *score < 1.0,
            "case {case:?}: an identification is a confidence and never a certainty, \
             got {score}"
        );
        previous = *score;
    }
}

#[tokio::test]
async fn a_document_two_languages_share_is_reported_below_one_of_the_same_length() {
    let comprehend = Comprehend::new();

    // Two documents of comparable length: one plainly in a single language, one whose
    // material two languages both write. A confidence that followed the amount of text
    // alone would order these by length and report them within a rounding of each
    // other; the margin the engine measured is what separates them.
    let plain = "Good afternoon, how are you today? The weather here is rather pleasant.";
    let shared = "Hola Ciao Hola Ciao Hola Ciao Hola Ciao Hola Ciao Hola Ciao Hola Ciao.";
    let leading = leading_languages(&comprehend, &[plain, shared]).await;

    let (_, plain_score) = leading[0].clone();
    let (_, shared_score) = leading[1].clone();
    assert!(
        plain_score < 1.0 && shared_score > 0.0,
        "case \"margin\": expected both confidences strictly inside (0, 1), got \
         {plain_score} and {shared_score}"
    );
    assert!(
        shared_score < plain_score,
        "case \"margin\": expected the document two languages share to be reported \
         below the one plainly in a single language, got {shared_score} against \
         {plain_score}"
    );
    // Not merely below: a document the engine cannot place leaves the fallback branch a
    // client takes reachable, which a value a rounding apart from the clear document's
    // would not.
    assert!(
        plain_score - shared_score > 0.05,
        "case \"margin\": expected the two to be reported materially apart, got \
         {shared_score} against {plain_score}"
    );
}

// ---------------------------------------------------------------------------
// 2. BatchDetectKeyPhrases
// ---------------------------------------------------------------------------

/// Every key phrase of every document of one batch, as `(Text, Score)`.
async fn key_phrases(comprehend: &Comprehend, texts: &[&str]) -> Vec<Vec<(String, f64)>> {
    let results = batch_results(
        comprehend,
        "BatchDetectKeyPhrases",
        &json!({ "TextList": texts, "LanguageCode": "en" }),
    )
    .await;
    results
        .iter()
        .map(|entry| {
            entry["KeyPhrases"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|phrase| {
                    (
                        phrase["Text"].as_str().unwrap_or_default().to_string(),
                        phrase["Score"].as_f64().unwrap_or(f64::NAN),
                    )
                })
                .collect()
        })
        .collect()
}

/// `(case, document)` — the rows differ along how many phrases the document holds, how
/// long each phrase is, and whether a phrase rests on a word the analysis settles or on
/// one whose other reading stays live.
const KEY_PHRASE_DOCUMENTS: &[(&str, &str)] = &[
    (
        "two determiner-led phrases",
        "The flight was delayed and the staff were rude.",
    ),
    (
        "a subject phrase, a date expression and a place name",
        "The quarterly report is due next Friday in Boston.",
    ),
    (
        "a phrase resting on a gerund inside a nominal compound",
        "The language processing service works.",
    ),
    ("one bare phrase", "Boston."),
    (
        "a long phrase beside a short one",
        "The departure lounge and the gate were closed.",
    ),
];

#[tokio::test]
async fn every_key_phrase_carries_its_own_confidence() {
    let comprehend = Comprehend::new();
    let texts: Vec<&str> = KEY_PHRASE_DOCUMENTS
        .iter()
        .map(|(_, text)| *text)
        .collect();
    let per_document = key_phrases(&comprehend, &texts).await;

    for (position, (case, _)) in KEY_PHRASE_DOCUMENTS.iter().enumerate() {
        let phrases = &per_document[position];
        assert!(
            !phrases.is_empty(),
            "case {case:?}: expected the document's key phrases, got none"
        );
        for (text, score) in phrases {
            assert!(
                *score > 0.0 && *score < 1.0,
                "case {case:?}: expected the confidence of {text:?} strictly inside \
                 (0, 1) — a key phrase is a detection, not a certainty — got {score}"
            );
        }
        // Two phrases of one document are two detections, each with its own evidence,
        // so they are not reported with one literal.
        let spread = distinct(phrases.iter().map(|(_, score)| *score));
        assert_eq!(
            spread,
            phrases.len(),
            "case {case:?}: expected one confidence per phrase, got {spread} distinct \
             values across {phrases:?}"
        );
    }

    // The evidence a phrase carries is its own characters as the analysis read them, so
    // a longer phrase the analysis settles whole is reported above a shorter one.
    let score_of = |case: &str, wanted: &str| -> f64 {
        let position = KEY_PHRASE_DOCUMENTS
            .iter()
            .position(|(name, _)| *name == case)
            .unwrap_or_else(|| panic!("no document for case {case:?}"));
        per_document[position]
            .iter()
            .find(|(text, _)| text == wanted)
            .map(|(_, score)| *score)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: no phrase {wanted:?} in {:?}",
                    per_document[position]
                )
            })
    };
    const LONG_CASE: &str = "a long phrase beside a short one";
    let long = score_of(LONG_CASE, "The departure lounge");
    let short = score_of(LONG_CASE, "the gate");
    assert!(
        short < long,
        "case {LONG_CASE:?}: expected the phrase the document spells out at length to \
         be reported above the short one, got {short} against {long}"
    );

    // A phrase resting on a word whose other reading stays live keeps only the share
    // that reading leaves it, so it is reported materially below a phrase of comparable
    // length the analysis settles whole.
    const GERUND_CASE: &str = "a phrase resting on a gerund inside a nominal compound";
    let gerund = score_of(GERUND_CASE, "The language processing service");
    assert!(
        gerund < 0.999,
        "case {GERUND_CASE:?}: expected a phrase resting on a reading the analysis did \
         not settle to be reported below the near-certainty a settled phrase carries, \
         got {gerund}"
    );
    assert!(
        gerund < long - 0.001,
        "case {GERUND_CASE:?}: expected it below the settled phrase of comparable \
         length, got {gerund} against {long}"
    );
}

#[tokio::test]
async fn a_batched_document_is_scored_as_it_is_on_its_own() {
    let comprehend = Comprehend::new();
    let texts: Vec<&str> = KEY_PHRASE_DOCUMENTS
        .iter()
        .map(|(_, text)| *text)
        .collect();
    let batched = key_phrases(&comprehend, &texts).await;

    for (position, (case, text)) in KEY_PHRASE_DOCUMENTS.iter().enumerate() {
        let alone = key_phrases(&comprehend, &[text]).await;
        assert_eq!(
            batched[position], alone[0],
            "case {case:?}: batching changes how many documents one request carries, \
             never how a document is scored, got {:?} against {:?}",
            batched[position], alone[0]
        );
    }
}

// ---------------------------------------------------------------------------
// 3. BatchDetectSentiment
// ---------------------------------------------------------------------------

/// The four modelled members of `SentimentScore`.
const SENTIMENT_MEMBERS: &[&str] = &["Positive", "Negative", "Neutral", "Mixed"];

/// `(case, document)` — the rows differ along polarity, how much of the document
/// carries it, and how much material the engine reads no polarity on at all.
const SENTIMENT_DOCUMENTS: &[(&str, &str)] = &[
    ("a short complaint", "The seat was uncomfortable."),
    (
        "the same complaint inside a long factual report",
        "The seat was uncomfortable. The flight left Boston at three in the afternoon \
         and the quarterly report is due next Friday.",
    ),
    ("praise", "I love this product, it is wonderful."),
    (
        "plain fact",
        "The meeting is scheduled for three in the afternoon.",
    ),
    (
        "both polarities",
        "The food was delicious but the service was terrible.",
    ),
];

#[tokio::test]
async fn the_members_a_document_does_not_support_keep_the_room_that_document_leaves() {
    let comprehend = Comprehend::new();
    let texts: Vec<&str> = SENTIMENT_DOCUMENTS.iter().map(|(_, text)| *text).collect();
    let results = batch_results(
        &comprehend,
        "BatchDetectSentiment",
        &json!({ "TextList": texts, "LanguageCode": "en" }),
    )
    .await;

    // Every residual value the table reports: the members of each document that are not
    // its verdict and that the document leaves open (`MIXED` is measured from the two
    // signed readings, so a document carrying at most one polarity reports it as the
    // zero it measures and it is not a residual this rule is about).
    let mut residuals: Vec<(&str, &str, f64)> = Vec::new();
    for (position, (case, _)) in SENTIMENT_DOCUMENTS.iter().enumerate() {
        let entry = &results[position];
        let verdict = entry["Sentiment"].as_str().unwrap_or_default().to_string();
        let mut total = 0.0;
        for member in SENTIMENT_MEMBERS {
            let score = entry["SentimentScore"][member]
                .as_f64()
                .unwrap_or_else(|| panic!("case {case:?}: expected {member}, got {entry}"));
            total += score;
            assert!(
                (0.0..1.0).contains(&score),
                "case {case:?}: expected {member} inside [0, 1), got {score}"
            );
            if !verdict.eq_ignore_ascii_case(member) && score > 0.0 {
                residuals.push((case, member, score));
            }
        }
        assert!(
            (total - 1.0).abs() < 1e-5,
            "case {case:?}: the four confidences are a distribution, so they sum to 1, \
             got {total} from {entry}"
        );
    }

    // The room a reading the engine found nothing for keeps is a function of the
    // document, so the residual members of the table are not one shared literal: a
    // single constant would report one value here however many documents were read.
    let spread = distinct(residuals.iter().map(|(_, _, score)| *score));
    assert!(
        spread >= SENTIMENT_DOCUMENTS.len(),
        "expected the residual confidences to follow each document's own material, got \
         {spread} distinct values across {residuals:?}"
    );

    // The same class, read on two documents the engine reads the same way but that
    // leave it different amounts of material to have missed a word in.
    let residual_of = |case: &str, member: &str| -> f64 {
        residuals
            .iter()
            .find(|(name, class, _)| *name == case && *class == member)
            .map(|(_, _, score)| *score)
            .unwrap_or_else(|| panic!("no {member} residual for case {case:?}: {residuals:?}"))
    };
    let short = residual_of("a short complaint", "Positive");
    let padded = residual_of("the same complaint inside a long factual report", "Positive");
    assert_ne!(
        short, padded,
        "expected the room a missed positive word leaves to follow the material each \
         document holds, got {short} for both"
    );
}

// ---------------------------------------------------------------------------
// 4. BatchDetectSyntax
// ---------------------------------------------------------------------------

/// Every syntax token of every document of one batch, as `(Text, Tag, Score)`.
async fn syntax_tokens(comprehend: &Comprehend, texts: &[&str]) -> Vec<Vec<(String, String, f64)>> {
    let results = batch_results(
        comprehend,
        "BatchDetectSyntax",
        &json!({ "TextList": texts, "LanguageCode": "en" }),
    )
    .await;
    results
        .iter()
        .map(|entry| {
            entry["SyntaxTokens"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|token: &Value| {
                    (
                        token["Text"].as_str().unwrap_or_default().to_string(),
                        token["PartOfSpeech"]["Tag"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        token["PartOfSpeech"]["Score"].as_f64().unwrap_or(f64::NAN),
                    )
                })
                .collect()
        })
        .collect()
}

/// `(case, document, [(the token, the tag it must come back with)])`.
///
/// The rows differ along the construction the SAME word stands in rather than along the
/// word: the same forms of `be` and the same participles appear graded and ungraded, in
/// a predicate and in front of an agent phrase, so a tagger reading a word out of a
/// list without its position would disagree with at least one row.
type SyntaxRow = (&'static str, &'static str, &'static [(&'static str, &'static str)]);

const GRADED_DOCUMENTS: &[SyntaxRow] = &[
    (
        "an ungraded participle in a copula's predicate is that clause's verb",
        "The flight was delayed.",
        &[("was", "AUX"), ("delayed", "VERB")],
    ),
    (
        "the same participle graded by a degree adverb is the predicate's adjective",
        "The flight was very delayed.",
        &[("was", "VERB"), ("very", "ADV"), ("delayed", "ADJ")],
    ),
    (
        "a graded participle with an adjunct behind it",
        "I was very disappointed with the late delivery.",
        &[("was", "VERB"), ("disappointed", "ADJ")],
    ),
    (
        "a graded participle in front of an agent phrase",
        "I was quite annoyed by the delay.",
        &[("was", "VERB"), ("annoyed", "ADJ"), ("by", "ADP")],
    ),
    (
        "a graded gerund predicate",
        "The process is extremely frustrating.",
        &[("is", "VERB"), ("frustrating", "ADJ")],
    ),
    (
        "an ungraded passive with an agent keeps its verb reading",
        "The window was broken by a storm.",
        &[("was", "AUX"), ("broken", "VERB")],
    ),
    (
        "a word with no verbal reading is the predicate's adjective, graded or not",
        "The staff were rather rude.",
        &[("were", "VERB"), ("rude", "ADJ")],
    ),
];

#[tokio::test]
async fn a_participle_a_degree_adverb_grades_is_the_predicates_adjective() {
    let comprehend = Comprehend::new();
    let texts: Vec<&str> = GRADED_DOCUMENTS.iter().map(|(_, text, _)| *text).collect();
    let per_document = syntax_tokens(&comprehend, &texts).await;

    for (position, (case, _, expected)) in GRADED_DOCUMENTS.iter().enumerate() {
        let tokens = &per_document[position];
        for (word, tag) in *expected {
            let reported = tokens
                .iter()
                .find(|(text, _, _)| text == word)
                .map(|(_, tag, _)| tag.as_str())
                .unwrap_or_else(|| panic!("case {case:?}: no token {word:?} in {tokens:?}"));
            assert_eq!(
                reported, *tag,
                "case {case:?}: expected {word:?} to come back {tag:?}, got \
                 {reported:?} in {tokens:?}"
            );
        }
    }
}

/// `(case, document, the tokens whose reading nothing leaves open, the tokens whose
/// other reading stays live)`.
type ConfidenceRow = (
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

const CONFIDENCE_DOCUMENTS: &[ConfidenceRow] = &[
    (
        // Every word of it is claimed by one class, or stands where its position
        // settles which class it is: the whole document is reported at full
        // confidence, as the recorded responses report such a document.
        "a document every word of which the analysis settles",
        "The flight was delayed and the staff were rude.",
        &[
            "The", "flight", "was", "delayed", "and", "the", "staff", "were", "rude",
        ],
        &[],
    ),
    (
        // "after" is a preposition and a subordinating conjunction, and nothing in the
        // position it stands in settles which.
        "a word two classes claim that no position settles",
        "The dog barks after dark.",
        &["The", "dog", "barks"],
        &["after"],
    ),
    (
        // The position PREFERS the nominal reading of the gerund without excluding the
        // verbal one, which is still a verb form.
        "a gerund the position prefers one reading of",
        "The language processing service works.",
        &["The", "service", "works"],
        &["processing"],
    ),
];

#[tokio::test]
async fn a_token_nothing_contests_is_reported_at_full_confidence_and_one_contested_below() {
    let comprehend = Comprehend::new();
    let texts: Vec<&str> = CONFIDENCE_DOCUMENTS
        .iter()
        .map(|(_, text, _, _)| *text)
        .collect();
    let per_document = syntax_tokens(&comprehend, &texts).await;

    for (position, (case, _, settled, live)) in CONFIDENCE_DOCUMENTS.iter().enumerate() {
        let tokens = &per_document[position];
        let score_of = |word: &str| -> f64 {
            tokens
                .iter()
                .find(|(text, _, _)| text == word)
                .map(|(_, _, score)| *score)
                .unwrap_or_else(|| panic!("case {case:?}: no token {word:?} in {tokens:?}"))
        };
        for word in *settled {
            let score = score_of(word);
            assert_eq!(
                score, 1.0,
                "case {case:?}: expected {word:?}, whose reading nothing leaves open, \
                 to be reported at full confidence, got {score} in {tokens:?}"
            );
        }
        for word in *live {
            let score = score_of(word);
            assert!(
                score > 0.0 && score < 1.0,
                "case {case:?}: expected {word:?}, whose other reading stays live, to \
                 be reported strictly inside (0, 1), got {score} in {tokens:?}"
            );
        }
        // Over EVERY word of the document rather than the named ones alone: a token the
        // row does not name as contested carries no reading the analysis left open, so
        // it is reported at full confidence and not a rounding below it.
        for (text, _, score) in tokens
            .iter()
            .filter(|(text, _, _)| text.chars().any(char::is_alphabetic))
            .filter(|(text, _, _)| !live.contains(&text.as_str()))
        {
            assert_eq!(
                *score, 1.0,
                "case {case:?}: expected {text:?}, which the row does not name as \
                 contested, to be reported at full confidence, got {score} in \
                 {tokens:?}"
            );
        }
    }
}
