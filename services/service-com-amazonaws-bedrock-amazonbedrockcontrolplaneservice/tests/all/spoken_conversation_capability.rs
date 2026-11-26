// SPDX-License-Identifier: CC0-1.0

//! What the published catalog reports for a model that carries a SPOKEN conversation, and
//! how that differs from a model whose turn is written.
//!
//! The rule under test, asserted over a SET of rows rather than over one recorded request:
//! **the modality a model answers with decides which conversation surfaces reach it.** A
//! turn of a conversation is TEXT or SPEECH, and a model that answers a turn at all carries
//! the members describing a conversation with it or a job built over it — `modelFamily`,
//! `batchSupported`, `latencyOptimizationSupported` — while the surfaces that act on a
//! WRITTEN turn (the `converse` record, `guardrailsSupported`, and Converse's own
//! synchronous mode) reach a
//! model whose turn is TEXT and no other. A spoken answer arrives as it is spoken rather
//! than in one piece, so a model that SPEAKS offers no synchronous calling mode on any API
//! and is carried by a stream in both directions when it takes a spoken turn as well.
//!
//! The table spans what the rule generalises over: the modality the row answers with (TEXT,
//! SPEECH, both SPEECH and the text it transcribes, EMBEDDING, IMAGE), whether the row takes
//! a spoken turn as well, and whether it streams. Two of its rows are classes the published
//! catalog holds a row of, so the published rows are checked through the wire too, against
//! the independent projection in `crate::support`.
//!
//! The capability members are RECORDED behaviour: the on-disk shape declares the ten
//! members `FoundationModelSummary` carries and none of these
//! (`smithy_query(query="shape",shape="FoundationModelSummary")`), and so is the spoken
//! modality itself — `ModelModality` declares TEXT, IMAGE and EMBEDDING
//! (`smithy_query(query="shape",shape="ModelModality")`) — so the expectations here come
//! from the rule above rather than from the model.

use serde_json::Value;

use bedrock::state::{FoundationModel, FOUNDATION_MODELS};
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;

use crate::support::{
    emulator, expected_foundation_model_projection, label, Emulator, Reply, REGION,
};

/// The path `ListFoundationModels` is bound to, and the prefix `GetFoundationModel` reads
/// one model under (`smithy_query(query="operation",operation="ListFoundationModels")`).
const PATH: &str = "/foundation-models";

/// The modality a SPOKEN turn of a conversation is made of, as the catalog publishes it.
const SPEECH: &str = "SPEECH";

/// The modality a WRITTEN turn of a conversation is made of.
const TEXT: &str = "TEXT";

async fn list(emu: &Emulator) -> Reply {
    emu.call_in_region(REGION, "GET", PATH, None).await
}

async fn read(emu: &Emulator, model_id: &str) -> Reply {
    emu.call_in_region(REGION, "GET", &format!("{PATH}/{}", label(model_id)), None)
        .await
}

/// The one summary a listing reported for `model_id`.
fn summary_of(case: &str, reply: &Reply, model_id: &str) -> Value {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: a listing must answer 200, got {} {:?}",
        reply.status, reply.body
    );
    reply
        .get("modelSummaries")
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: modelSummaries must be a present array"))
        .iter()
        .find(|row| row.get("modelId").and_then(Value::as_str) == Some(model_id))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the listing must carry {model_id:?}"))
}

/// A catalog row for the one class a case puts it in.
const fn row(
    model_id: &'static str,
    input_modalities: &'static [&'static str],
    output_modalities: &'static [&'static str],
    response_streaming_supported: bool,
) -> FoundationModel {
    FoundationModel {
        model_id,
        model_name: "Table Row",
        provider_name: "Table AI",
        model_family: "Table",
        input_modalities,
        output_modalities,
        response_streaming_supported,
        customizations_supported: &[],
        inference_types_supported: &["ON_DEMAND"],
        features_supported: &["BATCH_INFERENCE", "LATENCY_OPTIMIZATION"],
        lifecycle_status: "ACTIVE",
        start_of_life_time: "2024-01-01T00:00:00Z",
        legacy_window: None,
    }
}

/// One row of the table: a model built to sit in ONE class of the rule, and what the rule
/// then decides about it.
struct Case {
    name: &'static str,
    model: FoundationModel,
    /// Whether a `converse` record and `guardrailsSupported` hold: the surfaces that act on a
    /// WRITTEN turn.
    written_turn: bool,
    /// Whether the members describing a conversation with the model or a job built over it
    /// are reported: `modelFamily`, `batchSupported`, `latencyOptimizationSupported`.
    answers_a_turn: bool,
    /// `inferenceAPIsSupported.converse.streaming`.
    converse_streaming: bool,
    /// `inferenceAPIsSupported.invokeModel.sync`.
    invoke_sync: bool,
    /// `inferenceAPIsSupported.invokeModel.bidirectionalStreaming`.
    bidirectional: bool,
}

#[tokio::test]
async fn the_modality_a_model_answers_with_decides_which_surfaces_reach_it() {
    let cases = [
        Case {
            name: "a streaming model whose turn is written",
            model: row("table.text-streaming", &[TEXT], &[TEXT], true),
            written_turn: true,
            answers_a_turn: true,
            converse_streaming: true,
            invoke_sync: true,
            bidirectional: false,
        },
        Case {
            name: "a model whose written turn arrives in one piece",
            model: row("table.text-unary", &[TEXT], &[TEXT], false),
            written_turn: true,
            answers_a_turn: true,
            converse_streaming: false,
            invoke_sync: true,
            bidirectional: false,
        },
        Case {
            name: "a speech-to-speech model that also answers the text it transcribes",
            model: row(
                "table.speech-and-text",
                &[SPEECH, TEXT],
                &[SPEECH, TEXT],
                true,
            ),
            // It SPEAKS, so no written-conversation surface reaches it, however much text
            // travels beside the speech.
            written_turn: false,
            answers_a_turn: true,
            converse_streaming: true,
            invoke_sync: false,
            bidirectional: true,
        },
        Case {
            name: "a model that takes and answers a spoken turn alone",
            model: row("table.speech-only", &[SPEECH], &[SPEECH], true),
            written_turn: false,
            answers_a_turn: true,
            converse_streaming: true,
            invoke_sync: false,
            bidirectional: true,
        },
        Case {
            name: "a model prompted in writing that answers by speaking",
            model: row("table.text-to-speech", &[TEXT], &[SPEECH], true),
            written_turn: false,
            answers_a_turn: true,
            converse_streaming: true,
            // Nothing is streamed TO the model, so the stream runs one way only.
            invoke_sync: false,
            bidirectional: false,
        },
        Case {
            name: "a model whose answer is a vector, which no turn can be built from",
            model: row("table.embedding", &[TEXT], &["EMBEDDING"], false),
            written_turn: false,
            answers_a_turn: false,
            converse_streaming: false,
            invoke_sync: true,
            bidirectional: false,
        },
        Case {
            name: "a model whose answer is an image",
            model: row("table.image", &[TEXT, "IMAGE"], &["IMAGE"], false),
            written_turn: false,
            answers_a_turn: false,
            converse_streaming: false,
            invoke_sync: true,
            bidirectional: false,
        },
    ];

    for case in &cases {
        let name = case.name;
        let model = &case.model;

        let converse = bedrock::state::model_converse_support(model);
        assert_eq!(
            converse.is_some(),
            case.written_turn,
            "case {name:?}: expected a converse record to be reported: {}, got {converse:?}",
            case.written_turn
        );
        let guardrails = bedrock::state::model_guardrails_supported(model);
        assert_eq!(
            guardrails, case.written_turn,
            "case {name:?}: expected guardrailsSupported {}, got {guardrails}",
            case.written_turn
        );
        // Prompt optimization is PUBLISHED per model rather than decided by the modality the
        // model answers with, so it is read off the row: these rows publish none, so none of
        // them reports it however written its turn is.
        let features = bedrock::state::model_features_supported(model);
        let publishes_optimization = model.features_supported.contains(&"PROMPT_OPTIMIZATION");
        assert_eq!(
            features.prompt_optimization,
            Some(publishes_optimization),
            "case {name:?}: expected featuresSupported.promptOptimization \
             {publishes_optimization:?}, got {features:?}"
        );

        let apis = bedrock::state::model_inference_apis_supported(model);
        let converse_api = apis
            .converse
            .clone()
            .unwrap_or_else(|| panic!("case {name:?}: the Converse API must answer, got {apis:?}"));
        assert_eq!(
            (converse_api.sync, converse_api.streaming),
            (Some(case.written_turn), Some(case.converse_streaming)),
            "case {name:?}: expected inferenceAPIsSupported.converse to answer sync {} and \
             streaming {}, got {converse_api:?}",
            case.written_turn,
            case.converse_streaming
        );
        let invoke_api = apis.invoke_model.clone().unwrap_or_else(|| {
            panic!("case {name:?}: the InvokeModel API must answer, got {apis:?}")
        });
        assert_eq!(
            (
                invoke_api.sync,
                invoke_api.response_streaming,
                invoke_api.bidirectional_streaming,
            ),
            (
                Some(case.invoke_sync),
                Some(model.response_streaming_supported),
                Some(case.bidirectional),
            ),
            "case {name:?}: expected inferenceAPIsSupported.invokeModel to answer sync {}, \
             responseStreaming {} and bidirectionalStreaming {}, got {invoke_api:?}",
            case.invoke_sync,
            model.response_streaming_supported,
            case.bidirectional
        );

        // The members describing a conversation with the model, or a job built over one,
        // hold of a model that answers a TURN of one, spoken or written.
        let answers_a_turn = bedrock::state::model_answers_a_turn(model);
        assert_eq!(
            answers_a_turn, case.answers_a_turn,
            "case {name:?}: expected the row to answer a turn of a conversation: {}, got \
             {answers_a_turn}",
            case.answers_a_turn
        );
    }
}

/// The ids of the published rows that answer a SPOKEN turn.
fn spoken_rows() -> Vec<&'static FoundationModel> {
    FOUNDATION_MODELS
        .iter()
        .filter(|model| model.output_modalities.contains(&SPEECH))
        .collect()
}

#[tokio::test]
async fn a_published_model_that_speaks_is_listed_and_read_as_the_rule_projects_it() {
    let spoken = spoken_rows();
    assert!(
        !spoken.is_empty(),
        "case \"the published catalog\": Amazon Bedrock publishes speech-to-speech models, so \
         at least one catalog row must answer a spoken turn, got none"
    );

    let emu = emulator();
    let listing = list(&emu).await;
    for model in spoken {
        let case = model.model_id;
        let row = summary_of(case, &listing, model.model_id);
        let expected = expected_foundation_model_projection(model, REGION);
        assert_eq!(
            row, expected,
            "case {case:?}: expected the projection {expected:?}, got {row:?}"
        );
        // The three members a written-conversation surface would carry, read off the wire
        // rather than off the projection, so the rule is pinned at the member a client reads.
        assert_eq!(
            row.get("converse"),
            None,
            "case {case:?}: a model that SPEAKS its turn is reached through a stream, so its \
             summary carries no converse record, got {row:?}"
        );
        assert_eq!(
            row.get("guardrailsSupported").and_then(Value::as_bool),
            Some(false),
            "case {case:?}: a guardrail decides about written text, so it does not apply to a \
             model that answers by speaking, got {row:?}"
        );
        assert_eq!(
            row.pointer("/inferenceAPIsSupported/invokeModel/bidirectionalStreaming")
                .and_then(Value::as_bool),
            Some(model.input_modalities.contains(&SPEECH)),
            "case {case:?}: a two-way stream carries a spoken turn each way, so it reaches a \
             model that takes one as well as answers one, got {row:?}"
        );
        // It still answers a TURN of a conversation, so the members describing one are there.
        assert_eq!(
            row.get("modelFamily").and_then(Value::as_str),
            Some(model.model_family),
            "case {case:?}: a model that answers a spoken turn belongs to a family line, got \
             {row:?}"
        );
        assert!(
            row.get("batchSupported").is_some(),
            "case {case:?}: a model that answers a turn carries the batch record, got {row:?}"
        );

        // And the read of that one model reports what the listing carries for it.
        let reply = read(&emu, model.model_id).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: a read must answer 200, got {} {:?}",
            reply.status, reply.body
        );
        let details = reply.get("modelDetails");
        assert_eq!(
            details, row,
            "case {case:?}: expected the read to report the listed projection {row:?}, got \
             {details:?}"
        );
    }
}
