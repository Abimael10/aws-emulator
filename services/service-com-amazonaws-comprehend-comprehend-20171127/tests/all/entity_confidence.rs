// SPDX-License-Identifier: CC0-1.0

//! What `BatchDetectEntities` reports for a detection's `Score`, as a convention
//! rather than as one recorded answer.
//!
//! The modelled `Entity$Score` is "the level of confidence that Amazon Comprehend has
//! in the accuracy of the detection", so it belongs to THAT detection: it is weighed
//! over the characters of the mention the recogniser's evidence accounts for and over
//! what the rest of the document says about the same mention, never read out of a
//! table keyed by the category or the rule class the detection ended up in. Four
//! consequences, each asserted over a table whose rows differ along what the rule
//! generalises over:
//!
//! 1. Two mentions of the SAME type in ONE document, found by the SAME rule class,
//!    come back with two confidences — a score belonging to the type or to the rule
//!    could not tell them apart.
//! 2. The more of the mention's characters the evidence accounts for, the higher the
//!    confidence: a name the lexicon spells out at length, a calendar form that fixes
//!    more fields, a designator written out rather than abbreviated.
//! 3. A mention the document writes again, read the same way, is corroborated by that
//!    second writing, so it is reported above the same mention written once.
//! 4. The confidence is a function of the document alone: a document answered inside
//!    a batch is answered exactly as the single-document operation answers it, and
//!    repeating either call returns the same values.
//! 5. It is reported on the SCALE a confidence is read on: a clearly delimited
//!    mention whose evidence accounts for its own characters is an unambiguous
//!    detection, so it comes back near certainty — past the threshold a client gates
//!    a detection at, with the residual well under a percent — while a span the
//!    evidence only partly accounts for, or one nothing places at all, stays
//!    materially below that gate.
//! 6. A closed lexical form's confidence follows the characters that form fixes and
//!    how tightly it fixes one, so two forms of one category are told apart.
//! 7. It is a reading of the mention IN ITS DOCUMENT: one surface form, one type and
//!    one rule class come back with two confidences in two documents that frame the
//!    mention differently — the cue words the document writes beside it, the mentions
//!    it coordinates with it, and the place it puts it in (the head of a sentence,
//!    where orthography accounts for a capital, against inside one).
//!
//! Every row rides in one batch, so each is answered as its own `ResultList` entry
//! beside the others, exactly as the recorded three-document request's were.

use serde_json::{json, Value};

use crate::batch_inference_calibration::{batch_results, distinct};
use crate::comprehend_client::Comprehend;

/// Every entity each document of a batch was answered with, in document order.
async fn batched_entities(comprehend: &Comprehend, documents: &[&str]) -> Vec<Vec<Value>> {
    let results = batch_results(
        comprehend,
        "BatchDetectEntities",
        &json!({ "TextList": documents, "LanguageCode": "en" }),
    )
    .await;
    results
        .iter()
        .map(|result| result["Entities"].as_array().cloned().unwrap_or_default())
        .collect()
}

/// The `Score` the answer gives the entity whose `Text` is `mention`, asserted to be
/// a confidence (strictly inside `(0, 1)`: a rule-and-lexicon recogniser never
/// excludes the reading in which the span names nothing) and to carry `entity_type`.
fn score_of(case: &str, found: &[Value], mention: &str, entity_type: &str) -> f64 {
    let matching: Vec<&Value> = found
        .iter()
        .filter(|entity| entity["Text"].as_str() == Some(mention))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {case:?}: expected exactly one entity reading {mention:?}, got {found:?}"
    );
    let entity = matching[0];
    assert_eq!(
        entity["Type"].as_str(),
        Some(entity_type),
        "case {case:?}: expected {mention:?} to be typed {entity_type}, got {entity}"
    );
    let score = entity["Score"].as_f64().unwrap_or(f64::NAN);
    assert!(
        score > 0.0 && score < 1.0,
        "case {case:?}: expected a Score strictly inside (0, 1) for {mention:?}, got {score} \
         in {entity}"
    );
    score
}

// ---------------------------------------------------------------------------
// 1 and 2. Two mentions of one type in one document
// ---------------------------------------------------------------------------

/// `(case, document, Type, the mention whose evidence accounts for more of it, the
/// other mention, why the first one leaves less open)`.
///
/// Each row holds TWO mentions of one type, found by one rule class, in one document,
/// so a confidence read out of the type or out of the rule would answer both the
/// same. The rows differ along the KIND of evidence behind the pair — a lexicon entry
/// spelling the name out, two closed calendar forms, a legal-form designator inside
/// the name, a unit carrying a measurement, an honorific standing in front of a name —
/// and inside each pair along how much of the mention that evidence accounts for.
type Pair = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

const PAIRS: &[Pair] = &[
    (
        "two names the place lexicon spells out",
        "They expanded into Portugal and Chile.",
        "LOCATION",
        "Portugal",
        "Chile",
        "a longer spelling the lexicon names is less likely to be a coincidence than a \
         shorter one",
    ),
    (
        "two closed calendar forms",
        "The outage ran from 2024-01-15 to January 20, 2024.",
        "DATE",
        "January 20, 2024",
        "2024-01-15",
        "the form written with its month name fixes more characters than the all-numeric \
         one",
    ),
    (
        "a legal-form designator written out against its abbreviation",
        "Globex Corporation filed the papers. Acme Widgets LLC paid the fee.",
        "ORGANIZATION",
        "Globex Corporation",
        "Acme Widgets LLC",
        "a designator spelled out accounts for more of the name than a three-character \
         abbreviation of it",
    ),
    (
        "two measurements carrying their units",
        "The parcel weighs 12 kg and the crate weighs 1,250 kilograms.",
        "QUANTITY",
        "1,250 kilograms",
        "12 kg",
        "the amount written with its scale and its unit spelled out fixes more characters \
         than the short form",
    ),
    (
        "two names an honorific announces",
        "Dr. Alexandra Rutherford and Dr. Jane Doe reviewed the report.",
        "PERSON",
        "Jane Doe",
        "Alexandra Rutherford",
        "the honorific names the head of the name, which is more of the shorter mention \
         than of the longer one",
    ),
];

#[tokio::test]
async fn two_mentions_of_one_type_are_scored_from_their_own_evidence() {
    let comprehend = Comprehend::new();
    let documents: Vec<&str> = PAIRS.iter().map(|(_, document, ..)| *document).collect();
    let answers = batched_entities(&comprehend, &documents).await;

    let mut every_score: Vec<f64> = Vec::new();
    for (position, (case, _, entity_type, stronger, weaker, reason)) in PAIRS.iter().enumerate() {
        let found = &answers[position];
        let stronger_score = score_of(case, found, stronger, entity_type);
        let weaker_score = score_of(case, found, weaker, entity_type);

        // 1. Two detections of one type in one document, two confidences.
        assert!(
            (stronger_score - weaker_score).abs() > 1e-6,
            "case {case:?}: expected {stronger:?} and {weaker:?} to be scored from their own \
             evidence, got {stronger_score} and {weaker_score}"
        );
        // 2. More of the mention accounted for, more confidence.
        assert!(
            stronger_score > weaker_score,
            "case {case:?}: expected {stronger:?} to be scored above {weaker:?} ({reason}), \
             got {stronger_score} against {weaker_score}"
        );
        every_score.push(stronger_score);
        every_score.push(weaker_score);
    }

    // No ladder: ten detections spanning five kinds of evidence take ten values.
    let spread = distinct(every_score.iter().copied());
    assert_eq!(
        spread,
        every_score.len(),
        "expected every detection to carry its own confidence, got {spread} distinct values \
         across {every_score:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. The document's own corroboration of the mention
// ---------------------------------------------------------------------------

/// `(case, the document writing the mention once, the document writing it again,
/// Type, the mention, whether the second document FRAMES both writings alike)`.
///
/// The rows differ along the kind of evidence that found the mention — a lexicon
/// entry, a closed form, a designator inside the name — so the corroboration is not a
/// property of one rule class. The last field says whether the document puts both
/// writings in the same kind of place: the corroboration itself belongs to the
/// document, so where it does, both writings carry one confidence, and where it does
/// not (one writing opening a sentence, where orthography accounts for its capital,
/// the other standing inside one) the writing the document frames more weakly is
/// reported below the other.
const REPEATED: &[(&str, &str, &str, &str, &str, bool)] = &[
    (
        "a name the place lexicon spells out",
        "They expanded into Portugal.",
        "They expanded into Portugal and later left Portugal.",
        "LOCATION",
        "Portugal",
        true,
    ),
    (
        "a closed calendar form",
        "The outage began on 2024-01-15.",
        "The outage began on 2024-01-15 and the report is dated 2024-01-15.",
        "DATE",
        "2024-01-15",
        true,
    ),
    (
        "a name a legal-form designator types",
        "Acme Widgets LLC filed the papers.",
        "Acme Widgets LLC filed the papers, and Acme Widgets LLC paid the fee.",
        "ORGANIZATION",
        "Acme Widgets LLC",
        false,
    ),
];

#[tokio::test]
async fn a_mention_the_document_writes_again_is_scored_above_one_written_once() {
    let comprehend = Comprehend::new();
    let documents: Vec<&str> = REPEATED
        .iter()
        .flat_map(|(_, once, again, ..)| [*once, *again])
        .collect();
    let answers = batched_entities(&comprehend, &documents).await;

    for (position, (case, _, _, entity_type, mention, framed_alike)) in REPEATED.iter().enumerate() {
        let written_once = score_of(case, &answers[position * 2], mention, entity_type);
        // The repeated mention is reported twice, and the corroboration belongs to the
        // document, so every writing carries it.
        let repeated = &answers[position * 2 + 1];
        let writings: Vec<f64> = repeated
            .iter()
            .filter(|entity| entity["Text"].as_str() == Some(*mention))
            .map(|entity| entity["Score"].as_f64().unwrap_or(f64::NAN))
            .collect();
        assert_eq!(
            writings.len(),
            2,
            "case {case:?}: expected {mention:?} to be reported at both of its writings, got \
             {repeated:?}"
        );
        for (which, writing) in writings.iter().enumerate() {
            assert!(
                *writing > written_once,
                "case {case:?}: expected writing {which} of {mention:?} in a document that \
                 writes it twice to be scored above the same mention written once, got \
                 {writing} against {written_once}"
            );
            assert!(
                *writing < 1.0,
                "case {case:?}: a corroborated reading is still a confidence, so expected \
                 writing {which} to be scored below 1, got {writing}"
            );
        }
        if *framed_alike {
            assert_eq!(
                distinct(writings.iter().copied()),
                1,
                "case {case:?}: the corroboration belongs to the document and this document \
                 frames both writings of {mention:?} alike, so expected one confidence, got \
                 {writings:?}"
            );
        } else {
            // The first writing opens a sentence, where English's orthography accounts
            // for the capital it is written with, so its reading rests on less than the
            // reading of the writing standing inside a sentence.
            assert!(
                writings[1] > writings[0],
                "case {case:?}: expected the writing of {mention:?} standing inside a \
                 sentence to be scored above the one opening it, got {:?}",
                writings
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 4. The confidence is a function of the document alone
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_batched_document_is_scored_exactly_as_the_single_document_operation_scores_it() {
    let comprehend = Comprehend::new();
    let documents: Vec<&str> = PAIRS
        .iter()
        .map(|(_, document, ..)| *document)
        .chain(REPEATED.iter().map(|(_, _, again, ..)| *again))
        .collect();
    let batched = batched_entities(&comprehend, &documents).await;
    // Repeating the batch returns the same answer: nothing about a detection depends
    // on how often it has been asked for.
    let repeated = batched_entities(&comprehend, &documents).await;

    for (position, document) in documents.iter().enumerate() {
        let case = format!("document at index {position}");
        assert_eq!(
            batched[position], repeated[position],
            "case {case:?}: expected a repeated batch to answer {document:?} identically, got \
             {:?} then {:?}",
            batched[position], repeated[position]
        );

        let alone = comprehend
            .ok(
                "DetectEntities",
                &json!({ "Text": document, "LanguageCode": "en" }),
            )
            .await;
        let alone = alone["Entities"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            batched[position], alone,
            "case {case:?}: a batch answers each document on its own, so expected {document:?} \
             to come back exactly as DetectEntities answers it, got {:?} against {alone:?}",
            batched[position]
        );
    }
}

// ---------------------------------------------------------------------------
// 5. The scale the confidence is reported on
// ---------------------------------------------------------------------------

/// The confidence a client gates a detection at: above it the mention is written to a
/// record, redacted or routed for review, below it it is discarded. A detection whose
/// evidence accounts for the mention's own characters is an unambiguous one and has to
/// come back past this gate, with the residual it leaves well under a percent — not a
/// borderline reading a percentage point or two short of it.
const DECISION_GATE: f64 = 0.99;

/// `(case, document, Type, the mention the evidence accounts for whole)`.
///
/// The rows differ along the kind of evidence that accounts for the whole mention,
/// which is what the rule generalises over — a lexicon entry spelling a place name
/// out, a shorter spelling of one, a lexicon entry naming a company, a calendar form
/// fixing separators and field widths, an amount written with its currency, one
/// written with its unit, one written as a proportion — and so along the Type as well,
/// so a gate that passes only one category or one rule class disagrees with the table.
const ACCOUNTED_WHOLE: &[(&str, &str, &str, &str)] = &[
    (
        "a place name the lexicon spells out",
        "They expanded into Portugal.",
        "LOCATION",
        "Portugal",
    ),
    (
        "a shorter place name the lexicon spells out",
        "The shipment cleared customs in Chile.",
        "LOCATION",
        "Chile",
    ),
    (
        "a company the lexicon names",
        "The filing names Amazon as the buyer.",
        "ORGANIZATION",
        "Amazon",
    ),
    (
        "a calendar form that fixes its separators and field widths",
        "The outage began on 2024-01-15.",
        "DATE",
        "2024-01-15",
    ),
    (
        "an amount written with its currency",
        "The grant came to $100,000 over three years.",
        "QUANTITY",
        "$100,000",
    ),
    (
        "an amount written with its unit",
        "The parcel weighs 12 kg.",
        "QUANTITY",
        "12 kg",
    ),
    (
        "an amount written as a proportion",
        "Revenue grew 12 percent over the quarter.",
        "QUANTITY",
        "12 percent",
    ),
];

/// `(case, document, Type, the mention, why its evidence leaves more open)`.
///
/// The counterweight to the table above: the near-certainty of a whole-mention
/// reading is a consequence of the evidence and not a constant the recogniser prints,
/// so a span whose characters the evidence only partly names, and one nothing places
/// at all, must stay materially below the gate.
const ACCOUNTED_IN_PART: &[(&str, &str, &str, &str, &str)] = &[
    (
        "a name only a three-character designator places",
        "Acme Widgets LLC filed the papers.",
        "ORGANIZATION",
        "Acme Widgets LLC",
        "the designator names three of the name's fourteen characters, so the rest of \
         the span is left unaccounted for",
    ),
    (
        "a capitalized phrase no rule places",
        "The Zorblax Initiative published its findings.",
        "OTHER",
        "Zorblax Initiative",
        "the orthography says the span is a name and nothing says which kind, so the \
         reading it is reported with is the one the rules could not narrow",
    ),
];

/// How far below the gate a detection the evidence only partly accounts for has to
/// sit for the gate to mean anything: a client reading the two tables apart must not
/// need a threshold finer than this.
const MATERIAL_MARGIN: f64 = 0.05;

#[tokio::test]
async fn a_mention_the_evidence_accounts_for_whole_is_reported_near_certainty() {
    let comprehend = Comprehend::new();
    let documents: Vec<&str> = ACCOUNTED_WHOLE
        .iter()
        .map(|(_, document, _, _)| *document)
        .chain(ACCOUNTED_IN_PART.iter().map(|(_, document, ..)| *document))
        .collect();
    let answers = batched_entities(&comprehend, &documents).await;

    let mut near_certain: Vec<f64> = Vec::new();
    for (position, (case, _, entity_type, mention)) in ACCOUNTED_WHOLE.iter().enumerate() {
        let score = score_of(case, &answers[position], mention, entity_type);
        assert!(
            score > DECISION_GATE,
            "case {case:?}: the evidence accounts for every character of {mention:?}, so \
             expected a confidence past {DECISION_GATE}, got {score}"
        );
        assert!(
            1.0 - score < 0.01,
            "case {case:?}: expected the residual of an unambiguous detection of {mention:?} \
             to be well under a percent, got {}",
            1.0 - score
        );
        near_certain.push(score);
    }

    // Near certainty is where the evidence puts these detections, not a constant the
    // recogniser prints past the gate: every row of the table takes its own value.
    let spread = distinct(near_certain.iter().copied());
    assert_eq!(
        spread,
        near_certain.len(),
        "expected each detection past the gate to carry its own confidence, got {spread} \
         distinct values across {near_certain:?}"
    );
    let floor = near_certain
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);

    for (offset, (case, _, entity_type, mention, reason)) in ACCOUNTED_IN_PART.iter().enumerate() {
        let position = ACCOUNTED_WHOLE.len() + offset;
        let score = score_of(case, &answers[position], mention, entity_type);
        assert!(
            score < DECISION_GATE - MATERIAL_MARGIN,
            "case {case:?}: expected {mention:?} to stay materially below {DECISION_GATE} \
             ({reason}), got {score}"
        );
        assert!(
            score < floor - MATERIAL_MARGIN,
            "case {case:?}: expected {mention:?} to stay materially below every mention the \
             evidence accounts for whole ({floor}), got {score}"
        );
    }
}

// ---------------------------------------------------------------------------
// 6. What a closed lexical form's evidence is
// ---------------------------------------------------------------------------

/// `(case, document, Type, the mention, how much of it the form fixes and how
/// tightly, whether a name a lexicon spells out must outrank it)` — in ascending
/// order of the evidence the form carries.
///
/// A lexical rule says one thing about a span: it fixes CHARACTERS. So its evidence
/// is the characters of the mention it accounts for, each weighed by how tightly that
/// form fixes one — which is what orders the rows here, over three categories and
/// five forms, so neither the category nor the rule class can be what the confidence
/// is read out of. The products in each row are `characters fixed` × `how tightly`,
/// on the ladder that runs from a bare calendar word (which fixes only that the
/// characters spell one of seven weekday names) up to a form that fixes its
/// separators and its field widths. The last field marks the rows at the LOOSE end of
/// that ladder, which say so much less about a span than a lexicon entry naming it
/// outright that they may not be reported above one — a property of the row's own
/// form, not of its position in this table.
const ASCENDING_FORM_EVIDENCE: &[(&str, &str, &str, &str, &str, bool)] = &[
    (
        "a bare calendar word",
        "We ship on Friday.",
        "DATE",
        "Friday",
        "six characters, loosely fixed",
        true,
    ),
    (
        "a measurement, fewer characters fixed more tightly",
        "The parcel weighs 12 kg.",
        "QUANTITY",
        "12 kg",
        "four characters, fixed more tightly than a calendar word",
        true,
    ),
    (
        "the same calendar word with the modifier that places it",
        "The report is due next Friday at noon.",
        "DATE",
        "next Friday",
        "ten characters, loosely fixed",
        true,
    ),
    (
        "an amount written as a proportion",
        "Revenue grew 12 percent over the quarter.",
        "QUANTITY",
        "12 percent",
        "nine characters, fixed tightly",
        false,
    ),
    (
        "a calendar form that fixes its separators and field widths",
        "The outage began on 2024-01-15.",
        "DATE",
        "2024-01-15",
        "ten characters, fixed tightly",
        false,
    ),
];

/// A mention a lexicon spells out character by character: the strongest evidence this
/// recogniser has, and so not something the LOOSEST closed forms above may outweigh.
const SPELLED_OUT_BY_A_LEXICON: (&str, &str, &str, &str) = (
    "a place name the lexicon spells out",
    "The shipment cleared customs in Chile.",
    "LOCATION",
    "Chile",
);

#[tokio::test]
async fn a_closed_forms_confidence_follows_the_characters_it_fixes_and_how_tightly() {
    let comprehend = Comprehend::new();
    let (lexicon_case, lexicon_document, lexicon_type, lexicon_mention) = SPELLED_OUT_BY_A_LEXICON;
    let documents: Vec<&str> = ASCENDING_FORM_EVIDENCE
        .iter()
        .map(|(_, document, ..)| *document)
        .chain(std::iter::once(lexicon_document))
        .collect();
    let answers = batched_entities(&comprehend, &documents).await;

    let mut ladder: Vec<(&str, f64, bool)> = Vec::new();
    for (position, (case, _, entity_type, mention, evidence, loosely_fixed)) in
        ASCENDING_FORM_EVIDENCE.iter().enumerate()
    {
        let score = score_of(case, &answers[position], mention, entity_type);
        if let Some((previous_case, previous, _)) = ladder.last() {
            assert!(
                score > *previous,
                "case {case:?}: {mention:?} carries more of its form's evidence \
                 ({evidence}) than {previous_case:?}, so expected a higher confidence, got \
                 {score} against {previous}"
            );
        }
        ladder.push((case, score, *loosely_fixed));
    }

    let spelled_out = score_of(
        lexicon_case,
        &answers[ASCENDING_FORM_EVIDENCE.len()],
        lexicon_mention,
        lexicon_type,
    );
    // The loosely fixed forms say far less about a span than a lexicon entry naming it
    // outright, so they cannot be reported above one.
    for (case, score, _) in ladder.iter().filter(|(_, _, loosely_fixed)| *loosely_fixed) {
        assert!(
            spelled_out > *score,
            "case {case:?}: expected {lexicon_mention:?}, which the lexicon spells out, to be \
             scored above a loosely fixed form, got {spelled_out} against {score}"
        );
    }
}

// ---------------------------------------------------------------------------
// 7. The confidence is a reading of the mention IN ITS DOCUMENT
// ---------------------------------------------------------------------------

/// `(case, the document that frames the mention with nothing, the document that frames
/// it, Type, the mention, what the framing document writes around it)`.
///
/// ONE surface form, ONE type, ONE rule class, TWO documents: the characters of the
/// mention carry the same evidence in both, so a confidence that is a function of the
/// span — of its length, of the rule that fired on it, of its category — answers both
/// documents the same. It is not: what the document writes around the mention bears on
/// the reading, so the framed writing is reported above the bare one.
///
/// The rows differ along the KIND of framing, which is what the rule generalises over —
/// an honorific and a verb that takes a human argument standing beside a personal
/// name, a legal-form designator beside a company's name, a geographic head noun
/// beside a place name, and a place name the document coordinates another with — and
/// so along the Type as well, so no single lexicon or category is what the rule rests
/// on.
const FRAMED: &[(&str, &str, &str, &str, &str, &str)] = &[
    (
        "a personal name an honorific and a verb of saying frame",
        "The claim lists Jane Doe as the buyer.",
        "The claim was filed by Dr. Jane Doe, who told the court the same.",
        "PERSON",
        "Jane Doe",
        "an honorific, the agent preposition of a passive and a verb English fills the \
         human argument of",
    ),
    (
        "a company's name a second designator frames",
        "The fee was paid by Acme Widgets LLC.",
        "The fee was paid by Acme Widgets LLC, a subsidiary of the Globex holdings group.",
        "ORGANIZATION",
        "Acme Widgets LLC",
        "two further institution words standing outside the name",
    ),
    (
        "a place name a geographic head noun frames",
        "The shipment cleared customs in Chile.",
        "The shipment cleared customs in Chile, a republic on the coast.",
        "LOCATION",
        "Chile",
        "two generic geographic feature terms standing outside the name",
    ),
    (
        "a place name the document coordinates another place with",
        "They expanded into Chile last year.",
        "They expanded into Portugal and Chile last year.",
        "LOCATION",
        "Chile",
        "a coordinated place name, whose own characters bear on the reading of its \
         conjunct",
    ),
];

#[tokio::test]
async fn one_mention_is_scored_from_the_framing_its_own_document_gives_it() {
    let comprehend = Comprehend::new();
    let documents: Vec<&str> = FRAMED
        .iter()
        .flat_map(|(_, bare, framed, ..)| [*bare, *framed])
        .collect();
    let answers = batched_entities(&comprehend, &documents).await;

    for (position, (case, _, _, entity_type, mention, framing)) in FRAMED.iter().enumerate() {
        let bare = score_of(case, &answers[position * 2], mention, entity_type);
        let framed = score_of(case, &answers[position * 2 + 1], mention, entity_type);
        assert!(
            (framed - bare).abs() > 1e-6,
            "case {case:?}: expected {mention:?} to be scored from the evidence its own \
             document leaves it, got the same value {framed} in both documents"
        );
        assert!(
            framed > bare,
            "case {case:?}: expected {mention:?} to be scored above the unframed writing \
             ({framing}), got {framed} against {bare}"
        );
    }
}

/// `(case, the document opening a sentence with the mention, the document writing it
/// inside one, Type, the mention)`.
///
/// English's orthography puts a capital on a sentence's first word whatever the word
/// is, so that capital is no evidence that the characters name anything: the same
/// mention therefore rests on less at the head of a sentence than inside one. The rows
/// differ along the rule class that typed the span — a lexicon entry naming every
/// token, a legal-form designator naming one token of three, and the fallback that
/// places a capitalized phrase in no category at all — so the reading of a capital is
/// not a property of one rule.
const SENTENCE_POSITION: &[(&str, &str, &str, &str, &str)] = &[
    (
        "a name the place lexicon spells out",
        "Portugal raised the quota again.",
        "The quota Portugal raised was published.",
        "LOCATION",
        "Portugal",
    ),
    (
        "a name a legal-form designator types",
        "Acme Widgets LLC paid the fee.",
        "The fee was paid by Acme Widgets LLC.",
        "ORGANIZATION",
        "Acme Widgets LLC",
    ),
    (
        "a capitalized phrase no rule places",
        "Zorblax Initiative published its findings.",
        "The findings came from Zorblax Initiative.",
        "OTHER",
        "Zorblax Initiative",
    ),
];

#[tokio::test]
async fn a_capital_a_sentence_boundary_explains_is_not_evidence_of_a_name() {
    let comprehend = Comprehend::new();
    let documents: Vec<&str> = SENTENCE_POSITION
        .iter()
        .flat_map(|(_, opening, inside, ..)| [*opening, *inside])
        .collect();
    let answers = batched_entities(&comprehend, &documents).await;

    for (position, (case, _, _, entity_type, mention)) in SENTENCE_POSITION.iter().enumerate() {
        let opening = score_of(case, &answers[position * 2], mention, entity_type);
        let inside = score_of(case, &answers[position * 2 + 1], mention, entity_type);
        assert!(
            inside > opening,
            "case {case:?}: the capital of a sentence's first word is orthography rather than \
             evidence, so expected {mention:?} written inside a sentence to be scored above \
             the same mention opening one, got {inside} against {opening}"
        );
    }
}
