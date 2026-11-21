// SPDX-License-Identifier: CC0-1.0

//! Stop sequences on the inference surfaces, end to end over HTTP: what a
//! configured stop sequence does to the text a turn serves, on `Converse`,
//! `ConverseStream` and `InvokeModel`.
//!
//! Every case drives the real router through the harness the other suites of
//! this target use, so what is asserted is the wire behaviour a client sees.
//!
//! The rule is asserted over a SET of inputs: the tables differ along what it
//! generalises over (the sequence's characters, its length, how many sequences
//! are configured, whether they overlap, whether the text carries one at all,
//! how much of the generation budget is left, which model family serves the
//! turn and which request member carries the sequences), so no row is the
//! specification on its own. Each halted answer is checked against the SAME
//! request served without stop sequences, so what is asserted is a cut of the
//! text the emulator itself chose to serve, never a text written into the test.

use serde_json::{json, Value};

use crate::bedrock_runtime_inference::{emulator, post_json, send};
use crate::bedrock_runtime_streaming::decode_frames;
use service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state;

/// One `Converse` case: the model it is sent to, the stop sequences it
/// configures, and the generation cap it asks for when it asks for one.
struct StopRow {
    case: &'static str,
    /// A model the registry serves and the Converse API supports — the rows span
    /// several response families, which inference generalises over.
    model_id: &'static str,
    /// What the turn asks for; a longer ask is a longer prompt, hence a
    /// different synthesized completion to cut.
    ask: &'static str,
    /// `inferenceConfig.stopSequences`, whose member is a `NonEmptyString`
    /// (`smithy_query(query="shape", shape="InferenceConfiguration,
    /// NonEmptyStringList")`).
    stop_sequences: &'static [&'static str],
    /// `inferenceConfig.maxTokens`, where the row also constrains the budget.
    max_tokens: Option<i64>,
    /// The modelled `StopReason` the model's own family reports for a turn
    /// halted on a configured stop sequence (`halt_stop_reason` of
    /// [`HALT_REASON_ROWS`], which carries the per-family sources).
    halt_stop_reason: &'static str,
}

/// The stop-sequence table: one character and several, a word, punctuation, a
/// repeated character, overlapping sequences, several sequences at once, a
/// sequence the text does not carry, and sequences under a budget that is
/// generous, modest and below the shortest completion this emulator serves.
fn stop_rows() -> &'static [StopRow] {
    &[
        StopRow {
            case: "a single digit, as a delimiter-style client configures one",
            model_id: "amazon.nova-lite-v1:0",
            ask: "Output exactly the following, separated by single spaces and nothing \
                  else, no punctuation, no preamble: count from 1 to 100",
            stop_sequences: &["7"],
            max_tokens: Some(400),
            halt_stop_reason: "end_turn",
        },
        StopRow {
            case: "one punctuation character",
            model_id: "anthropic.claude-3-haiku-20240307-v1:0",
            ask: "Name one planet.",
            stop_sequences: &["."],
            max_tokens: None,
            halt_stop_reason: "stop_sequence",
        },
        StopRow {
            case: "a whole word",
            model_id: "amazon.titan-text-premier-v1:0",
            ask: "Explain idempotency in one sentence.",
            stop_sequences: &["runs"],
            max_tokens: Some(512),
            halt_stop_reason: "end_turn",
        },
        StopRow {
            case: "a character the text repeats, which stops at the first one",
            model_id: "mistral.mistral-7b-instruct-v0:2",
            ask: "List three primes.",
            stop_sequences: &["e"],
            max_tokens: None,
            halt_stop_reason: "end_turn",
        },
        StopRow {
            case: "two sequences, one of them a prefix of the other",
            model_id: "cohere.command-r-plus-v1:0",
            ask: "Answer with one sentence about state.",
            stop_sequences: &["language", "lang"],
            max_tokens: Some(64),
            halt_stop_reason: "end_turn",
        },
        StopRow {
            case: "several sequences, stopping at whichever completes first",
            model_id: "amazon.nova-pro-v1:0",
            ask: "Name two planets.",
            stop_sequences: &["emulator", "model", "request"],
            max_tokens: Some(200),
            halt_stop_reason: "end_turn",
        },
        StopRow {
            case: "a multi-word sequence spanning a space",
            model_id: "meta.llama3-8b-instruct-v1:0",
            ask: "Explain idempotency to a cat.",
            stop_sequences: &["in this emulator"],
            max_tokens: None,
            halt_stop_reason: "end_turn",
        },
        StopRow {
            case: "a sequence the served text does not carry",
            model_id: "amazon.nova-lite-v1:0",
            ask: "Reply with only the lowercase English word for 2+3.",
            stop_sequences: &["Ω-никогда-не-встретится"],
            max_tokens: Some(128),
            halt_stop_reason: "end_turn",
        },
        StopRow {
            case: "a sequence inside a budget below the shortest completion",
            model_id: "amazon.nova-lite-v1:0",
            ask: "Reply with only the lowercase English word for 2+3.",
            stop_sequences: &["lang"],
            max_tokens: Some(4),
            halt_stop_reason: "end_turn",
        },
        StopRow {
            case: "an empty list of sequences",
            model_id: "anthropic.claude-3-5-sonnet-20241022-v2:0",
            ask: "Name one moon.",
            stop_sequences: &[],
            max_tokens: Some(100),
            halt_stop_reason: "stop_sequence",
        },
    ]
}

impl StopRow {
    /// The request body, with the stop sequences this row configures or without
    /// any — the second form is the baseline the cut is measured against.
    fn request(&self, with_stop_sequences: bool) -> Value {
        let mut inference = json!({ "temperature": 0.0 });
        if let Some(max_tokens) = self.max_tokens {
            inference["maxTokens"] = json!(max_tokens);
        }
        if with_stop_sequences {
            inference["stopSequences"] = json!(self.stop_sequences);
        }
        json!({
            "messages": [{"role": "user", "content": [{"text": self.ask}]}],
            "inferenceConfig": inference,
        })
    }
}

/// `text` cut where the first of `stop_sequences` completes, computed here
/// independently of the emulator: the earliest offset at which an occurrence of
/// any sequence ENDS, so the cut keeps that sequence and nothing after it.
/// `None` when the text carries none of them.
fn cut_at_first_stop_sequence(text: &str, stop_sequences: &[&str]) -> Option<String> {
    let mut earliest: Option<usize> = None;
    for sequence in stop_sequences {
        if let Some(at) = text.find(*sequence) {
            let end = at + sequence.len();
            if earliest.is_none_or(|previous| end < previous) {
                earliest = Some(end);
            }
        }
    }
    earliest.map(|end| text[..end].to_string())
}

/// RULE: a stop sequence a request configures HALTS generation — the served text
/// ends with the first of the configured sequences to complete and carries
/// nothing after it, whatever generation budget is left over — so the turn ends
/// on the sequence, reporting the stop reason the serving model's own family
/// words a halt with rather than the ceiling's; a request that configures none,
/// or whose text carries none, is served the whole completion unchanged.
#[tokio::test]
async fn converse_halts_the_served_text_at_the_first_configured_stop_sequence() {
    let router = emulator();

    for row in stop_rows() {
        let case = row.case;

        // The same request without stop sequences: whatever this emulator
        // chooses to generate for it is what a configured sequence has to cut.
        let baseline = post_json(
            &router,
            &format!("/model/{}/converse", row.model_id),
            &row.request(false),
        )
        .await;
        assert_eq!(
            baseline.status,
            200,
            "case {case:?}: expected the unconfigured turn to answer 200, got {} / {}",
            baseline.status,
            String::from_utf8_lossy(&baseline.body)
        );
        let baseline = baseline.json();
        let full = baseline["output"]["message"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the turn answered no text: {baseline}"))
            .to_string();
        let baseline_stop = baseline["stopReason"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let call = post_json(
            &router,
            &format!("/model/{}/converse", row.model_id),
            &row.request(true),
        )
        .await;
        assert_eq!(
            call.status,
            200,
            "case {case:?}: expected 200, got {} / {}",
            call.status,
            String::from_utf8_lossy(&call.body)
        );
        let body = call.json();
        let served = body["output"]["message"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the turn answered no text: {body}"))
            .to_string();

        match cut_at_first_stop_sequence(&full, row.stop_sequences) {
            Some(expected) => {
                assert_eq!(
                    served, expected,
                    "case {case:?}: expected the text to halt at the first of {:?} \
                     ({expected:?}), got {served:?}",
                    row.stop_sequences
                );
                assert!(
                    row.stop_sequences
                        .iter()
                        .any(|sequence| served.ends_with(*sequence)),
                    "case {case:?}: expected the halted text to end with one of {:?}, got \
                     {served:?}",
                    row.stop_sequences
                );
                assert!(
                    served.len() < full.len(),
                    "case {case:?}: expected the halted text to be shorter than the \
                     unconfigured {full:?}, got {served:?}"
                );
                // Generation ended ON the sequence rather than at the ceiling,
                // and WHICH modelled `StopReason` member names that cause is the
                // serving family's (`smithy_query(query="shape",
                // shape="StopReason")` for the members; [`HALT_REASON_ROWS`] for
                // the per-family sources).
                assert_eq!(
                    body["stopReason"].as_str(),
                    Some(row.halt_stop_reason),
                    "case {case:?}: expected a turn halted at a stop sequence to report \
                     {:?}, got {:?}",
                    row.halt_stop_reason,
                    body["stopReason"]
                );
            }
            None => {
                assert_eq!(
                    served, full,
                    "case {case:?}: a text carrying none of {:?} is served whole, got \
                     {served:?}",
                    row.stop_sequences
                );
                assert_eq!(
                    body["stopReason"].as_str(),
                    Some(baseline_stop.as_str()),
                    "case {case:?}: expected the unconfigured stop reason \
                     {baseline_stop:?}, got {:?}",
                    body["stopReason"]
                );
            }
        }

        // The token accounting reports the text actually served, not the text
        // generation would have reached without the halt.
        assert_eq!(
            body["usage"]["outputTokens"].as_i64(),
            Some(i64::from(state::count_text_tokens(&served))),
            "case {case:?}: expected outputTokens to be the tokens of {served:?}, got {:?}",
            body["usage"]["outputTokens"]
        );
        if let Some(cap) = row.max_tokens {
            let output = body["usage"]["outputTokens"].as_i64().unwrap_or_default();
            assert!(
                output <= cap,
                "case {case:?}: expected at most {cap} output tokens, got {output}"
            );
        }
    }
}

/// RULE (the same halt, on the streaming surface): `ConverseStream` frames the
/// turn `Converse` serves, so its deltas reassemble to the identically halted
/// text and no single delta carries text past the cut.
#[tokio::test]
async fn converse_stream_frames_the_same_halted_text_converse_serves() {
    let router = emulator();

    for row in stop_rows() {
        let case = row.case;
        let request = row.request(true);

        let converse = post_json(
            &router,
            &format!("/model/{}/converse", row.model_id),
            &request,
        )
        .await;
        assert_eq!(
            converse.status,
            200,
            "case {case:?}: expected Converse to answer 200, got {} / {}",
            converse.status,
            String::from_utf8_lossy(&converse.body)
        );
        let converse = converse.json();
        let expected = converse["output"]["message"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: Converse answered no text: {converse}"))
            .to_string();

        let stream = post_json(
            &router,
            &format!("/model/{}/converse-stream", row.model_id),
            &request,
        )
        .await;
        assert_eq!(
            stream.status,
            200,
            "case {case:?}: expected ConverseStream to answer 200, got {} / {}",
            stream.status,
            String::from_utf8_lossy(&stream.body)
        );
        let frames = decode_frames(case, &stream.body);
        let deltas: Vec<String> = frames
            .iter()
            .filter(|(frame, _)| frame == "contentBlockDelta")
            .map(|(_, payload)| {
                payload["delta"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        let streamed: String = deltas.concat();
        assert_eq!(
            streamed, expected,
            "case {case:?}: expected the deltas to reassemble to the halted text \
             {expected:?}, got {streamed:?}"
        );
        // No delta may carry text the halted turn does not serve.
        let mut consumed = 0usize;
        for delta in &deltas {
            assert!(
                expected[consumed..].starts_with(delta.as_str()),
                "case {case:?}: a delta carries text past the cut: {delta:?} at offset \
                 {consumed} of {expected:?}"
            );
            consumed += delta.len();
        }
        let stop_reason = frames
            .iter()
            .find(|(frame, _)| frame == "messageStop")
            .map(|(_, payload)| payload["stopReason"].as_str().unwrap_or_default().to_string())
            .unwrap_or_default();
        assert_eq!(
            stop_reason.as_str(),
            converse["stopReason"].as_str().unwrap_or_default(),
            "case {case:?}: expected messageStop to report the stop reason Converse \
             reports, got {stop_reason:?}"
        );
    }
}

/// One `InvokeModel` case: the member a model family's own request body carries
/// its stop sequences in, as `state::body_stop_sequences` documents per family,
/// and where that family's response envelope reports the generated text and the
/// terminal reason it ended on.
struct FamilyStopRow {
    case: &'static str,
    model_id: &'static str,
    /// The request body with `SEQUENCES` standing in for the configured list,
    /// bound to the member that family documents (none for a family that
    /// documents no stop-sequence parameter).
    template: &'static str,
    text_pointer: &'static str,
    /// `false` for a family whose request format documents no stop-sequence
    /// parameter, so the body configures nothing to halt on.
    honours_stop_sequences: bool,
    /// Where the family's own response envelope reports its terminal reason.
    stop_pointer: &'static str,
    /// The word that envelope carries for a turn halted on a configured stop
    /// sequence — the family's own, which is a reason of its own only for the
    /// family whose response fields also NAME the generated sequence
    /// (`sequence_pointer`). For a family whose body configures no stop sequence
    /// nothing halts, so it is the word a finished turn reports.
    halt_reason: &'static str,
    /// Where the family names the sequence a halted turn was generated, for the
    /// family that names it at all.
    sequence_pointer: Option<&'static str>,
}

/// One row per response family the registry serves, with the stop-sequence
/// member of that family's own request format.
const FAMILY_STOP_ROWS: &[FamilyStopRow] = &[
    FamilyStopRow {
        case: "anthropic messages",
        model_id: "anthropic.claude-3-haiku-20240307-v1:0",
        template: r#"{"anthropic_version":"bedrock-2023-05-31","max_tokens":256,"stop_sequences":SEQUENCES,"messages":[{"role":"user","content":[{"type":"text","text":"Name one planet."}]}]}"#,
        text_pointer: "/content/0/text",
        honours_stop_sequences: true,
        stop_pointer: "/stop_reason",
        halt_reason: "stop_sequence",
        sequence_pointer: Some("/stop_sequence"),
    },
    FamilyStopRow {
        case: "titan text",
        model_id: "amazon.titan-text-premier-v1:0",
        template: r#"{"inputText":"Name one planet.","textGenerationConfig":{"maxTokenCount":512,"stopSequences":SEQUENCES}}"#,
        text_pointer: "/results/0/outputText",
        honours_stop_sequences: true,
        stop_pointer: "/results/0/completionReason",
        halt_reason: "FINISH",
        sequence_pointer: None,
    },
    FamilyStopRow {
        case: "nova",
        model_id: "amazon.nova-lite-v1:0",
        template: r#"{"messages":[{"role":"user","content":[{"text":"Name one planet."}]}],"inferenceConfig":{"max_new_tokens":256,"stopSequences":SEQUENCES}}"#,
        text_pointer: "/output/message/content/0/text",
        honours_stop_sequences: true,
        stop_pointer: "/stopReason",
        halt_reason: "end_turn",
        sequence_pointer: None,
    },
    FamilyStopRow {
        case: "mistral",
        model_id: "mistral.mistral-7b-instruct-v0:2",
        template: r#"{"prompt":"Name one planet.","max_tokens":256,"stop":SEQUENCES}"#,
        text_pointer: "/outputs/0/text",
        honours_stop_sequences: true,
        stop_pointer: "/outputs/0/stop_reason",
        halt_reason: "stop",
        sequence_pointer: None,
    },
    FamilyStopRow {
        case: "cohere command",
        model_id: "cohere.command-text-v14",
        template: r#"{"prompt":"Name one planet.","max_tokens":256,"stop_sequences":SEQUENCES}"#,
        text_pointer: "/generations/0/text",
        honours_stop_sequences: true,
        stop_pointer: "/generations/0/finish_reason",
        halt_reason: "COMPLETE",
        sequence_pointer: None,
    },
    FamilyStopRow {
        case: "llama, whose request format documents no stop sequence",
        model_id: "meta.llama3-8b-instruct-v1:0",
        template: r#"{"prompt":"Name one planet.","max_gen_len":256,"stop":SEQUENCES}"#,
        text_pointer: "/generation",
        honours_stop_sequences: false,
        stop_pointer: "/stop_reason",
        halt_reason: "stop",
        sequence_pointer: None,
    },
];

/// RULE (the same halt, on the `InvokeModel` surface): the stop sequences a
/// model family's own request body configures halt generation too, so the text
/// that family's response envelope reports ends with the first of them to
/// complete and carries nothing after it.
#[tokio::test]
async fn invoke_model_halts_the_served_text_at_the_stop_sequences_the_body_carries() {
    let router = emulator();

    for row in FAMILY_STOP_ROWS {
        for sequences in [
            vec!["."],
            vec!["runs"],
            vec!["emulator", "model"],
            vec!["Ω-никогда-не-встретится"],
        ] {
            let case = format!("{} halting on {sequences:?}", row.case);
            let full = invoke_text(&router, row, "[]", &case).await;
            let encoded = serde_json::to_string(&sequences).expect("a serializable list");
            let served = invoke_text(&router, row, &encoded, &case).await;
            let expected = match (
                row.honours_stop_sequences,
                cut_at_first_stop_sequence(&full, &sequences),
            ) {
                (true, Some(cut)) => cut,
                _ => full.clone(),
            };
            assert_eq!(
                served, expected,
                "case {case:?}: expected {expected:?}, got {served:?}"
            );

            // One sequence on its own is that list with a single entry, so the
            // bare-string spelling of the member halts the text identically.
            if sequences.len() == 1 {
                let case = format!("{case} spelled as a bare string");
                let encoded =
                    serde_json::to_string(&sequences[0]).expect("a serializable sequence");
                let scalar = invoke_text(&router, row, &encoded, &case).await;
                assert_eq!(
                    scalar, expected,
                    "case {case:?}: expected {expected:?}, got {scalar:?}"
                );
            }
        }
    }
}

/// The text one `InvokeModel` call reports in its family's own response
/// envelope, for the body `row` describes with `sequences` — the JSON the
/// family's stop-sequence member is bound to — configured.
async fn invoke_text(
    router: &smithy_server_runtime::MultiServiceRouter,
    row: &FamilyStopRow,
    sequences: &str,
    case: &str,
) -> String {
    let served = invoke_body(router, row, sequences, case).await;
    served
        .pointer(row.text_pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: no text at {} in {served}",
                row.text_pointer
            )
        })
        .to_string()
}

/// The whole response envelope that same call answers with.
async fn invoke_body(
    router: &smithy_server_runtime::MultiServiceRouter,
    row: &FamilyStopRow,
    sequences: &str,
    case: &str,
) -> Value {
    let body = row.template.replace("SEQUENCES", sequences);
    let call = send(
        router,
        "POST",
        &format!("/model/{}/invoke", row.model_id),
        &[
            ("content-type", "application/json"),
            ("accept", "application/json"),
        ],
        body.into_bytes(),
    )
    .await;
    assert_eq!(
        call.status,
        200,
        "case {case:?}: expected 200, got {} / {}",
        call.status,
        String::from_utf8_lossy(&call.body)
    );
    call.json()
}

/// One response family the Converse API serves, and the modelled `StopReason`
/// member a turn of its models reports for a halt on a configured stop sequence.
///
/// The member a halted turn carries is the MODEL's, not the Converse envelope's:
/// a family whose own response fields NAME the stop sequence a turn was
/// generated reports the halt as a reason of its own, and a family that
/// publishes no such field reports the halt as the ordinary end of a turn.
///
/// Source: each provider's response fields in "Inference request parameters and
/// response fields for foundation models" (Amazon Bedrock User Guide) — the
/// Anthropic Claude Messages API's `stop_reason` / `stop_sequence` pair against
/// Amazon Nova's `stopReason`, Amazon Titan's `completionReason`, Meta Llama's
/// and Mistral's `stop_reason`, Cohere's `finish_reason`, and the
/// OpenAI-compatible `finish_reason`, whose `stop` covers a natural stopping
/// point and a supplied stop sequence alike. The members themselves are
/// `smithy_query(query="shape", shape="StopReason")`.
struct HaltReasonRow {
    case: &'static str,
    /// A model of that family the Converse API serves.
    model_id: &'static str,
    halt_stop_reason: &'static str,
}

/// One row per family, on a model of that family the rows of `stop_rows` do not
/// all cite: the Anthropic Claude line that names the sequence, and every family
/// that words a halt as the end of a turn.
const HALT_REASON_ROWS: &[HaltReasonRow] = &[
    HaltReasonRow {
        case: "anthropic claude, whose envelope names the generated sequence",
        model_id: "anthropic.claude-sonnet-4-20250514-v1:0",
        halt_stop_reason: "stop_sequence",
    },
    HaltReasonRow {
        case: "amazon nova",
        model_id: "amazon.nova-premier-v1:0",
        halt_stop_reason: "end_turn",
    },
    HaltReasonRow {
        case: "amazon titan text",
        model_id: "amazon.titan-text-lite-v1",
        halt_stop_reason: "end_turn",
    },
    HaltReasonRow {
        case: "meta llama",
        model_id: "meta.llama3-3-70b-instruct-v1:0",
        halt_stop_reason: "end_turn",
    },
    HaltReasonRow {
        case: "mistral",
        model_id: "mistral.mistral-large-2407-v1:0",
        halt_stop_reason: "end_turn",
    },
    HaltReasonRow {
        case: "cohere command r",
        model_id: "cohere.command-r-v1:0",
        halt_stop_reason: "end_turn",
    },
    HaltReasonRow {
        case: "a model served on the OpenAI-compatible schema",
        model_id: "openai.gpt-oss-20b-1:0",
        halt_stop_reason: "end_turn",
    },
];

/// Sequences that are certain to halt a turn whose whole text is `text`: they
/// are taken OUT of the text the emulator itself served, so each case halts
/// wherever this emulator chose to generate, rather than on a sequence written
/// into the test. One word, a later word, and two words spanning a space.
fn sequences_out_of(text: &str) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut sequences = Vec::new();
    if let Some(word) = words.get(1) {
        sequences.push((*word).to_string());
    }
    if let Some(word) = words.get(3) {
        sequences.push((*word).to_string());
    }
    if let [first, second, ..] = words.as_slice() {
        sequences.push(format!("{first} {second}"));
    }
    sequences
}

/// RULE: the modelled `StopReason` a turn halted on a configured stop sequence
/// reports is the one the serving model's own family words a halt with — a
/// family whose response fields name the generated sequence reports
/// `stop_sequence`, and a family that words a halt as the ordinary end of a turn
/// reports `end_turn` — while the same model reports `end_turn` for a turn that
/// halted on nothing, and `Converse` and `ConverseStream` report one member for
/// one request.
#[tokio::test]
async fn a_halted_turn_reports_the_stop_reason_its_model_family_words_a_halt_with() {
    let router = emulator();

    for row in HALT_REASON_ROWS {
        let case = row.case;
        let request = |sequences: &[String]| {
            let mut inference = json!({});
            if !sequences.is_empty() {
                inference["stopSequences"] = json!(sequences);
            }
            json!({
                "messages": [{"role": "user", "content": [{"text": "Name one planet."}]}],
                "inferenceConfig": inference,
            })
        };

        // The turn this model serves with nothing configured to halt it: it
        // halted on no sequence and no cap, so it is the end of a turn.
        let baseline = post_json(
            &router,
            &format!("/model/{}/converse", row.model_id),
            &request(&[]),
        )
        .await;
        assert_eq!(
            baseline.status,
            200,
            "case {case:?}: expected the unconfigured turn to answer 200, got {} / {}",
            baseline.status,
            String::from_utf8_lossy(&baseline.body)
        );
        let baseline = baseline.json();
        let full = baseline["output"]["message"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the turn answered no text: {baseline}"))
            .to_string();
        assert_eq!(
            baseline["stopReason"].as_str(),
            Some("end_turn"),
            "case {case:?}: expected a turn that halted on nothing to report end_turn, got {:?}",
            baseline["stopReason"]
        );

        for sequence in sequences_out_of(&full) {
            let case = format!("{case} halting on {sequence:?}");
            let request = request(std::slice::from_ref(&sequence));

            let call = post_json(
                &router,
                &format!("/model/{}/converse", row.model_id),
                &request,
            )
            .await;
            assert_eq!(
                call.status,
                200,
                "case {case:?}: expected 200, got {} / {}",
                call.status,
                String::from_utf8_lossy(&call.body)
            );
            let body = call.json();
            let served = body["output"]["message"]["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: the turn answered no text: {body}"))
                .to_string();
            // The turn really halted: this is a stop reason for a halt, not for
            // a turn that ran to its end.
            assert!(
                served.ends_with(sequence.as_str()) && served.len() < full.len(),
                "case {case:?}: expected the text to halt at {sequence:?} inside {full:?}, \
                 got {served:?}"
            );
            assert_eq!(
                body["stopReason"].as_str(),
                Some(row.halt_stop_reason),
                "case {case:?}: expected {:?}, got {:?}",
                row.halt_stop_reason,
                body["stopReason"]
            );

            // The streaming surface frames the same turn, so it reports the same
            // member.
            let stream = post_json(
                &router,
                &format!("/model/{}/converse-stream", row.model_id),
                &request,
            )
            .await;
            assert_eq!(
                stream.status,
                200,
                "case {case:?}: expected ConverseStream to answer 200, got {} / {}",
                stream.status,
                String::from_utf8_lossy(&stream.body)
            );
            let streamed = decode_frames(&case, &stream.body)
                .iter()
                .find(|(frame, _)| frame == "messageStop")
                .map(|(_, payload)| payload["stopReason"].as_str().unwrap_or_default().to_string())
                .unwrap_or_default();
            assert_eq!(
                streamed.as_str(),
                row.halt_stop_reason,
                "case {case:?}: expected messageStop to report {:?}, got {streamed:?}",
                row.halt_stop_reason
            );
        }
    }
}

/// RULE (the same halt on the `InvokeModel` surface): the terminal reason a
/// family's own response envelope reports for a halted turn is that family's own
/// word for it, and the family whose response fields name the generated sequence
/// reports the sequence the turn halted on beside that reason.
#[tokio::test]
async fn invoke_model_reports_the_family_terminal_reason_for_a_halted_turn() {
    let router = emulator();

    for row in FAMILY_STOP_ROWS {
        let full = invoke_text(&router, row, "[]", row.case).await;
        for sequence in sequences_out_of(&full) {
            let case = format!("{} halting on {sequence:?}", row.case);
            let encoded = serde_json::to_string(&sequence).expect("a serializable sequence");
            let served = invoke_body(&router, row, &encoded, &case).await;
            let text = served
                .pointer(row.text_pointer)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: no text in {served}"))
                .to_string();
            match row.honours_stop_sequences {
                true => assert!(
                    text.ends_with(sequence.as_str()) && text.len() < full.len(),
                    "case {case:?}: expected the text to halt at {sequence:?} inside \
                     {full:?}, got {text:?}"
                ),
                // A body that configures no stop sequence halts on nothing, so
                // the turn runs to its end and reports the reason for that.
                false => assert_eq!(
                    text, full,
                    "case {case:?}: expected the whole text {full:?}, got {text:?}"
                ),
            }
            assert_eq!(
                served.pointer(row.stop_pointer).and_then(Value::as_str),
                Some(row.halt_reason),
                "case {case:?}: expected {:?} at {}, got {served}",
                row.halt_reason,
                row.stop_pointer
            );
            if let Some(pointer) = row.sequence_pointer {
                let expected = match row.honours_stop_sequences {
                    true => json!(sequence),
                    false => Value::Null,
                };
                assert_eq!(
                    served.pointer(pointer),
                    Some(&expected),
                    "case {case:?}: expected {expected} at {pointer}, got {served}"
                );
            }
        }
    }
}
