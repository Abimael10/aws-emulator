// SPDX-License-Identifier: CC0-1.0

//! The WIRE SHAPE of the capability members a catalogue listing reports, over a table of
//! catalogue rows: what each member's TYPE is, which members a row carries at all, and the
//! order a format list is reported in.
//!
//! The rules under test, each asserted over a SET of rows rather than over one recorded
//! request:
//!
//! 1. **A capability whose answer is more than one bit is a RECORD, never a bare boolean.**
//!    `featuresSupported.batchInference` answers per REGION SCOPE (`crossRegion`,
//!    `inRegion`), because a job that may run a model where it is offered is not thereby a
//!    job in another region that may; `converse.reasoningSupported` answers whether
//!    reasoning may be asked for AND where it arrives (`isSupported`, `embedded`). A client
//!    reads either by member name, so a boolean in its place is a type a generated client
//!    cannot read at all.
//! 2. **A feature that follows from what the model is offered under is computed from the
//!    row**: `featuresSupported.provisionedThroughput` holds of a model offered with
//!    Provisioned Throughput — the same published fact that decides whether a purchase over
//!    the model is accepted — so a client that reads it and then buys one is not refused.
//! 3. **A member's PRESENCE follows from the question it answers.** A member that answers
//!    what a turn may CARRY to the model, or what a request may ASK of it, is reported for
//!    every model the Converse API reaches, with the EMPTY answer of its own type where the
//!    model's line publishes nothing: the document formats are published per provider line
//!    (`DocumentFormat` of the Amazon Bedrock Runtime model names the vocabulary), so a
//!    model no document block reaches reports an empty `converse.userDocumentTypesSupported`
//!    rather than the whole vocabulary or no member at all, and a line that accepts no
//!    provider-specific request field reports `converse.additionalRequestFieldsSchema` as
//!    the empty JSON object. A member that answers a question only SOME models raise is
//!    reported for those models alone: `converse.reasoningSupported`, which names where a
//!    model's reasoning arrives, is there for a model whose provider publishes extended
//!    reasoning and absent for the rest.
//! 4. **A format list keeps the order it is published in**: `converse.userImageTypesSupported`
//!    reports the `ImageFormat` values in the published order, because a client that takes
//!    the FIRST supported format as its default reads a different default from a different
//!    order.
//! 5. **The invocation APIs are a RECORD of per-API calling modes**, reported for EVERY
//!    offered model: `inferenceAPIsSupported` names each API Amazon Bedrock serves a base
//!    model on and, for an API whose answer has parts, the modes it offers for that model
//!    (`converse.sync`/`.streaming`,
//!    `invokeModel.sync`/`.responseStreaming`/`.bidirectionalStreaming`). A client reads the
//!    mode it is about to use by name, which a list of API names cannot be asked; and every
//!    offered model can be invoked, so the record answers for a model that takes part in no
//!    conversation too.
//!
//! The capability members are RECORDED behaviour: the on-disk model declares none of them
//! (`smithy_query(query="shape", shape="com.amazonaws.bedrock#FoundationModelSummary")`
//! lists modelArn/modelId/modelName/providerName/inputModalities/outputModalities/
//! responseStreamingSupported/customizationsSupported/inferenceTypesSupported/
//! modelLifecycle), so the expectations here are the rules above, stated per row.

use serde_json::Value;

use bedrock::state::{FoundationModel, FOUNDATION_MODELS};
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;

use crate::support::{
    emulator, expected_max_output_tokens, expected_reasoning_supported,
    expected_user_document_formats, expected_user_image_formats, label, Emulator, Reply, REGION,
};

/// The path `ListFoundationModels` is bound to, and the prefix `GetFoundationModel` reads
/// one model under (`smithy_query(query="operation",operation="ListFoundationModels")`).
const PATH: &str = "/foundation-models";

/// The image formats a turn may carry to a model that takes IMAGE, in the order the
/// catalogue publishes them: the `ImageFormat` values of the Amazon Bedrock Runtime model
/// (`smithy_query(query="shape",shape="ImageFormat",
/// model_path="services/service-com-amazonaws-bedrockruntime-amazonbedrockfrontendservice/\
/// smithy-model/com-amazonaws-bedrockruntime-amazonbedrockfrontendservice.json")`), with
/// the format a client that takes the first supported one as its default would pick first.
const PUBLISHED_IMAGE_FORMATS: &[&str] = &["jpeg", "png", "gif", "webp"];

/// The same four formats in the order the provider lines that publish PNG first do — the
/// ORDER is published with the provider's own integration, so it is read per line.
const PUBLISHED_IMAGE_FORMATS_PNG_FIRST: &[&str] = &["png", "jpeg", "gif", "webp"];

/// One row of the catalogue and what the rules above say its capability members are.
struct Case {
    /// The `modelId` the listing carries the summary under.
    model_id: &'static str,
    /// What this row contributes to the table: the attribute combination it stands for.
    note: &'static str,
    /// Whether a model invocation job submitted where the model is offered may run it.
    batch_inference: bool,
    /// Whether a job submitted in ANOTHER region may run it: the other region scope, which
    /// batch inference publishes in its own right.
    cross_region_batch_inference: bool,
    /// Whether reserved capacity may be bought for the model.
    provisioned_throughput: bool,
    /// Whether extended reasoning may be asked of the model.
    reasoning: bool,
    /// The document formats a turn may carry; empty for a model no document reaches.
    documents: &'static [&'static str],
    /// The image formats a turn may carry; empty for a model that takes no image.
    images: &'static [&'static str],
}

/// The table: the rows differ along every attribute the rules generalise over — which
/// features the row publishes (none, batch in one region scope, batch in both, batch and
/// routing), whether it is offered
/// with Provisioned Throughput, whether its provider publishes reasoning for it, which
/// modalities it takes, and who publishes it (a provider line published with document input,
/// one that publishes its image formats in another order, and three that are neither).
const CASES: &[Case] = &[
    Case {
        model_id: "openai.gpt-6-astra",
        note: "no feature's table lists it, offered through an inference profile alone, takes \
               text and images",
        batch_inference: false,
        cross_region_batch_inference: false,
        provisioned_throughput: false,
        reasoning: false,
        documents: &[],
        images: PUBLISHED_IMAGE_FORMATS,
    },
    Case {
        model_id: "nvidia.nemotron-nano-12b-v2",
        note: "batch inference published for the region it is offered in alone, and a provider \
               line that publishes its image formats PNG first",
        batch_inference: true,
        cross_region_batch_inference: false,
        provisioned_throughput: false,
        reasoning: false,
        documents: &[],
        images: PUBLISHED_IMAGE_FORMATS_PNG_FIRST,
    },
    Case {
        model_id: "anthropic.claude-opus-4-6-v1",
        note: "a reasoning model of the provider line published with document input",
        batch_inference: false,
        cross_region_batch_inference: false,
        provisioned_throughput: false,
        reasoning: true,
        documents: &["pdf"],
        images: PUBLISHED_IMAGE_FORMATS,
    },
    Case {
        model_id: "anthropic.claude-haiku-4-5-20251001-v1:0",
        note: "every feature's table lists it, on demand only, document input published for \
               its provider line",
        batch_inference: true,
        cross_region_batch_inference: true,
        provisioned_throughput: false,
        reasoning: true,
        documents: &["pdf"],
        images: PUBLISHED_IMAGE_FORMATS,
    },
    Case {
        model_id: "meta.llama3-1-70b-instruct-v1:0",
        note: "batch inference published in both scopes and reserved capacity sold, text only",
        batch_inference: true,
        cross_region_batch_inference: true,
        provisioned_throughput: true,
        reasoning: false,
        documents: &[],
        images: &[],
    },
    Case {
        model_id: "meta.llama3-3-70b-instruct-v1:0",
        note: "batch inference published, on demand only, text only",
        batch_inference: true,
        cross_region_batch_inference: true,
        provisioned_throughput: false,
        reasoning: false,
        documents: &[],
        images: &[],
    },
    Case {
        model_id: "qwen.qwen3-32b-v1:0",
        note: "a reasoning model of a line published with no document input",
        batch_inference: false,
        cross_region_batch_inference: false,
        provisioned_throughput: false,
        reasoning: true,
        documents: &[],
        images: &[],
    },
];

async fn list(emu: &Emulator) -> Reply {
    emu.call_in_region(REGION, "GET", PATH, None).await
}

async fn read(emu: &Emulator, model_id: &str) -> Reply {
    emu.call_in_region(REGION, "GET", &format!("{PATH}/{}", label(model_id)), None)
        .await
}

/// Every summary the listing `reply` carries, by `modelId`.
fn summaries(reply: &Reply) -> Vec<Value> {
    assert_eq!(
        reply.status, 200,
        "case \"the listing\": a listing must answer 200, got {} {:?}",
        reply.status, reply.body
    );
    reply
        .get("modelSummaries")
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case \"the listing\": modelSummaries must be a present array, got {:?}",
                reply.body
            )
        })
        .clone()
}

/// The summary `model_id` is reported under.
fn summary_of(case: &str, listed: &[Value], model_id: &str) -> Value {
    listed
        .iter()
        .find(|row| row.get("modelId").and_then(Value::as_str) == Some(model_id))
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the listing must carry {model_id:?}"))
}

/// The catalogue row `model_id` names.
fn row(case: &str, model_id: &str) -> &'static FoundationModel {
    FOUNDATION_MODELS
        .iter()
        .find(|model| model.model_id == model_id)
        .unwrap_or_else(|| panic!("case {case:?}: the catalogue must publish {model_id:?}"))
}

/// The string list `member` carries, or `None` when the summary omits it.
fn string_list(value: &Value, member: &str) -> Option<Vec<String>> {
    value.get(member).map(|list| {
        list.as_array()
            .unwrap_or_else(|| panic!("member {member} must be a list, got {list:?}"))
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .unwrap_or_else(|| panic!("member {member} must carry strings, got {entry:?}"))
                    .to_string()
            })
            .collect()
    })
}

// ---------------------------------------------------------------------------------------
// 5. The invocation APIs are a RECORD of per-API calling modes, reported for every offered
// model, and the Converse record states one answer per attachment kind.
// ---------------------------------------------------------------------------------------

/// One catalogue row and what the invocation-API rule says its record answers.
struct ApiCase {
    /// The `modelId` the listing carries the summary under.
    model_id: &'static str,
    /// What this row contributes: the attribute combination it stands for.
    note: &'static str,
    /// Whether the model takes part in a conversation, which is what Converse carries.
    converse: bool,
    /// Whether the model streams what it answers.
    streaming: bool,
    /// Whether Amazon Bedrock publishes the model on its OpenAI-compatible Chat Completions
    /// endpoint.
    openai_chat_completions: bool,
    /// Whether it publishes the model on its OpenAI-compatible Responses endpoint: a surface
    /// of its own, published for its own models.
    openai_responses: bool,
}

/// The table: the rows differ along everything the rule reads off a row — what the model
/// answers with (text, an embedding vector, a relevance score, an image, a clip of
/// frames), whether it streams that answer, who publishes it, and which of the two
/// OpenAI-compatible endpoints Amazon Bedrock publishes it on (both, Chat Completions alone,
/// neither). None of these is an id the issue this rule was written for cites as an example
/// of the defect.
const API_CASES: &[ApiCase] = &[
    ApiCase {
        model_id: "cohere.embed-v4:0",
        note: "an embedding model: a vector is no turn of a conversation",
        converse: false,
        streaming: false,
        openai_chat_completions: false,
        openai_responses: false,
    },
    ApiCase {
        model_id: "cohere.rerank-v3-5:0",
        note: "a reranking model: a relevance score is no turn either",
        converse: false,
        streaming: false,
        openai_chat_completions: false,
        openai_responses: false,
    },
    ApiCase {
        model_id: "stability.stable-image-ultra-v1:1",
        note: "an image model, answered in one piece",
        converse: false,
        streaming: false,
        openai_chat_completions: false,
        openai_responses: false,
    },
    ApiCase {
        model_id: "luma.ray-v2:0",
        note: "a model that generates a clip of frames",
        converse: false,
        streaming: false,
        openai_chat_completions: false,
        openai_responses: false,
    },
    ApiCase {
        model_id: "writer.palmyra-x5-v1:0",
        note: "a streaming text model neither endpoint is published for",
        converse: true,
        streaming: true,
        openai_chat_completions: false,
        openai_responses: false,
    },
    ApiCase {
        model_id: "google.gemma-3-27b-it",
        note: "a streaming text model published on both endpoints, by a provider whose own API \
               neither is named after",
        converse: true,
        streaming: true,
        openai_chat_completions: true,
        openai_responses: true,
    },
    ApiCase {
        model_id: "openai.gpt-oss-120b-1:0",
        note: "a text model published on Chat Completions and on no Responses endpoint",
        converse: true,
        streaming: true,
        openai_chat_completions: true,
        openai_responses: false,
    },
    ApiCase {
        model_id: "openai.gpt-oss-safeguard-20b",
        note: "a text model published on both endpoints",
        converse: true,
        streaming: true,
        openai_chat_completions: true,
        openai_responses: true,
    },
];

#[tokio::test]
async fn the_invocation_apis_answer_per_api_and_per_calling_mode() {
    let emu = emulator();
    let listed = summaries(&list(&emu).await);

    for case in API_CASES {
        let name = case.model_id;
        let note = case.note;
        let model = row(name, name);
        assert_eq!(
            model.output_modalities.contains(&"TEXT"),
            case.converse,
            "case {name:?} ({note}): the table says this row {} a conversation, got the output \
             modalities {:?}",
            if case.converse { "takes part in" } else { "answers no turn of" },
            model.output_modalities
        );
        assert_eq!(
            model.response_streaming_supported, case.streaming,
            "case {name:?} ({note}): the table says this row streams {:?}, got {:?}",
            case.streaming, model.response_streaming_supported
        );

        let summary = summary_of(name, &listed, name);
        // Every offered model can be invoked, so the record is reported for every row.
        let apis = summary.get("inferenceAPIsSupported").unwrap_or(&Value::Null);
        assert!(
            apis.is_object(),
            "case {name:?} ({note}): expected inferenceAPIsSupported to be the record naming \
             each invocation API, got {apis:?}"
        );
        // InvokeModel reaches every offered model, whatever it answers with, and streams
        // its answer exactly where the row streams; no modality this catalogue declares is
        // carried by a stream running in both directions.
        assert_eq!(
            (
                apis["invokeModel"]["sync"].as_bool(),
                apis["invokeModel"]["responseStreaming"].as_bool(),
                apis["invokeModel"]["bidirectionalStreaming"].as_bool(),
            ),
            (Some(true), Some(case.streaming), Some(false)),
            "case {name:?} ({note}): expected invokeModel to answer in one piece, to stream \
             {:?} and to carry no two-way stream, got {apis:?}",
            case.streaming
        );
        // Converse carries a CONVERSATION, so it answers for a model that takes part in one
        // and streams under the same condition.
        assert_eq!(
            (
                apis["converse"]["sync"].as_bool(),
                apis["converse"]["streaming"].as_bool(),
            ),
            (Some(case.converse), Some(case.converse && case.streaming)),
            "case {name:?} ({note}): expected converse to answer {:?} and to stream {:?}, got \
             {apis:?}",
            case.converse,
            case.converse && case.streaming
        );
        for (member, published) in [
            ("openAiChatCompletions", case.openai_chat_completions),
            ("openAiResponses", case.openai_responses),
        ] {
            assert_eq!(
                apis.get(member).and_then(Value::as_bool),
                Some(published),
                "case {name:?} ({note}): an OpenAI-compatible endpoint serves the models Amazon \
                 Bedrock publishes it for ({published:?}), got {apis:?}"
            );
        }
        // An asynchronous invocation answers with an artifact instead of a response body,
        // which Amazon Bedrock serves for a generation that takes minutes; no modality
        // `ModelModality` declares is one, so no row answers yes.
        assert_eq!(
            apis["asyncInvoke"].as_bool(),
            Some(false),
            "case {name:?} ({note}): expected asyncInvoke false, got {apis:?}"
        );

        // The Converse record states one answer per attachment kind, and the schema of the
        // provider-specific fields the model accepts — the empty object where it accepts
        // none. A model that answers no turn of a conversation carries no record at all.
        match summary.get("converse") {
            Some(converse) => {
                assert!(
                    case.converse,
                    "case {name:?} ({note}): this row answers no turn of a conversation, so no \
                     Converse record is reported for it, got {converse:?}"
                );
                for kind in ["userImageTypesSupported", "userVideoTypesSupported"] {
                    assert!(
                        string_list(converse, kind).is_some(),
                        "case {name:?} ({note}): {kind} answers for a modality the model takes \
                         and is empty for one it does not, got {converse:?}"
                    );
                }
            }
            None => assert!(
                !case.converse,
                "case {name:?} ({note}): a conversational model carries a Converse record, got \
                 {summary:?}"
            ),
        }

        // And a read of that one model answers what the listing carries for it.
        let reply = read(&emu, name).await;
        assert_eq!(
            reply.status, 200,
            "case {name:?} ({note}): a read of a published model must answer 200, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.get("modelDetails"),
            summary,
            "case {name:?} ({note}): expected the read to report the listed projection \
             {summary:?}, got {:?}",
            reply.get("modelDetails")
        );
    }
}

// ---------------------------------------------------------------------------------------
// 1 and 2. A capability whose answer is more than one bit is a record; a feature that
// follows from what the row is offered under is computed from the row.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_feature_record_answers_per_region_scope_and_per_inference_type() {
    let emu = emulator();
    let listed = summaries(&list(&emu).await);

    for case in CASES {
        let name = case.model_id;
        let note = case.note;
        let model = row(name, name);
        let summary = summary_of(name, &listed, name);
        let features = summary
            .get("featuresSupported")
            .unwrap_or_else(|| panic!("case {name:?} ({note}): a conversational model carries a feature record, got {summary:?}"));

        // Batch inference answers per region scope, so it is a record and never one bit.
        let batch = features.get("batchInference").unwrap_or(&Value::Null);
        assert!(
            batch.is_object(),
            "case {name:?} ({note}): expected featuresSupported.batchInference to be the record \
             naming each region scope, got {batch:?}"
        );
        for (scope, published) in [
            ("crossRegion", case.cross_region_batch_inference),
            ("inRegion", case.batch_inference),
        ] {
            assert_eq!(
                batch.get(scope).and_then(Value::as_bool),
                Some(published),
                "case {name:?} ({note}): expected featuresSupported.batchInference.{scope} \
                 {published:?}, got {batch:?}"
            );
        }

        // Reserved capacity is offered for exactly the models the catalogue publishes it
        // for, which is what `inferenceTypesSupported` reports.
        assert_eq!(
            features.get("provisionedThroughput").and_then(Value::as_bool),
            Some(case.provisioned_throughput),
            "case {name:?} ({note}): the row is offered under {:?}, so expected \
             featuresSupported.provisionedThroughput {:?}, got {features:?}",
            model.inference_types_supported,
            case.provisioned_throughput
        );

        // And a read of that one model answers what the listing carries for it.
        let reply = read(&emu, name).await;
        assert_eq!(
            reply.status, 200,
            "case {name:?} ({note}): a read of a published model must answer 200, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.get("modelDetails"),
            summary,
            "case {name:?} ({note}): expected the read to report the listed projection \
             {summary:?}, got {:?}",
            reply.get("modelDetails")
        );
    }
}

#[tokio::test]
async fn reasoning_names_whether_it_is_asked_for_and_where_it_arrives() {
    let emu = emulator();
    let listed = summaries(&list(&emu).await);

    for case in CASES {
        let name = case.model_id;
        let note = case.note;
        let summary = summary_of(name, &listed, name);
        let converse = summary.get("converse").unwrap_or_else(|| {
            panic!(
                "case {name:?} ({note}): a conversational model carries a Converse record, got \
                 {summary:?}"
            )
        });
        // The record answers where a model's reasoning arrives, so it is reported for a
        // model whose provider publishes reasoning and for no other.
        let reasoning = converse.get("reasoningSupported");
        assert_eq!(
            reasoning.is_some(),
            case.reasoning,
            "case {name:?} ({note}): expected converse.reasoningSupported to be reported {:?}, \
             got {converse:?}",
            case.reasoning
        );
        let Some(reasoning) = reasoning else {
            continue;
        };
        assert!(
            reasoning.is_object(),
            "case {name:?} ({note}): expected converse.reasoningSupported to be a record, got \
             {reasoning:?}"
        );
        assert_eq!(
            (
                reasoning.get("isSupported").and_then(Value::as_bool),
                // Reasoning arrives in a block of its own for every model this catalogue
                // publishes, so a client never has to parse it out of the answer text.
                reasoning.get("embedded").and_then(Value::as_bool),
            ),
            (Some(true), Some(false)),
            "case {name:?} ({note}): expected converse.reasoningSupported to report a \
             supported, non-embedded reasoning, got {reasoning:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3 and 4. A member nothing is published for is omitted, and a format list keeps its order.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_format_list_is_published_per_model_and_keeps_its_order() {
    let emu = emulator();
    let listed = summaries(&list(&emu).await);

    for case in CASES {
        let name = case.model_id;
        let note = case.note;
        let summary = summary_of(name, &listed, name);
        let converse = summary.get("converse").unwrap_or_else(|| {
            panic!("case {name:?} ({note}): a conversational model carries a Converse record, got {summary:?}")
        });

        // The document formats are published per provider line, so a model no document
        // block reaches reports an EMPTY list rather than the whole vocabulary: what a turn
        // may attach has the empty answer for it.
        let documents = string_list(converse, "userDocumentTypesSupported");
        let expected: Vec<String> = case
            .documents
            .iter()
            .map(|format| (*format).to_string())
            .collect();
        assert_eq!(
            documents,
            Some(expected.clone()),
            "case {name:?} ({note}): expected converse.userDocumentTypesSupported \
             {expected:?}, got {documents:?}"
        );

        // The image formats follow the modality the model takes, in the published order.
        let images = string_list(converse, "userImageTypesSupported");
        let expected: Vec<String> = case
            .images
            .iter()
            .map(|format| (*format).to_string())
            .collect();
        assert_eq!(
            images,
            Some(expected.clone()),
            "case {name:?} ({note}): expected converse.userImageTypesSupported {expected:?}, got \
             {images:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// The same rules over the WHOLE catalogue, so no row answers a capability in another shape.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn every_summary_answers_every_capability_in_the_published_shape() {
    let emu = emulator();
    let listed = summaries(&list(&emu).await);
    assert_eq!(
        listed.len(),
        FOUNDATION_MODELS.len(),
        "case \"the whole catalogue\": expected one summary per offered model ({}), got {}",
        FOUNDATION_MODELS.len(),
        listed.len()
    );

    for model in FOUNDATION_MODELS {
        let name = model.model_id;
        let summary = summary_of(name, &listed, name);
        // The feature record answers for every offered model, because every offered model
        // can be invoked.
        let features = summary.get("featuresSupported").unwrap_or_else(|| {
            panic!("case {name:?}: every offered model carries a feature record, got {summary:?}")
        });
        let batch = features.get("batchInference").unwrap_or(&Value::Null);
        for (scope, published) in [
            (
                "crossRegion",
                model
                    .features_supported
                    .contains(&"CROSS_REGION_BATCH_INFERENCE"),
            ),
            (
                "inRegion",
                model.features_supported.contains(&"BATCH_INFERENCE"),
            ),
        ] {
            assert_eq!(
                batch.get(scope).and_then(Value::as_bool),
                Some(published),
                "case {name:?}: featuresSupported.batchInference answers per region scope out of \
                 that scope's own table, so expected {scope} {published:?}, got {batch:?}"
            );
        }
        let provisioned = model.inference_types_supported.contains(&"PROVISIONED");
        assert_eq!(
            features
                .get("provisionedThroughput")
                .and_then(Value::as_bool),
            Some(provisioned),
            "case {name:?}: the row is offered under {:?}, so expected \
             featuresSupported.provisionedThroughput {provisioned:?}, got {features:?}",
            model.inference_types_supported
        );

        let Some(converse) = summary.get("converse") else {
            // A model that answers no turn of a conversation carries no Converse record at
            // all; the suite that owns that rule asserts it.
            continue;
        };
        // The reasoning record names where that reasoning arrives: in a block of its own
        // for every model this catalogue publishes it for, and it is reported for those
        // models alone.
        let reasoning = converse.get("reasoningSupported");
        let publishes_reasoning = expected_reasoning_supported(model);
        assert_eq!(
            reasoning.is_some(),
            publishes_reasoning,
            "case {name:?}: converse.reasoningSupported is reported for a model whose provider \
             publishes extended reasoning ({publishes_reasoning:?}) and for no other, got \
             {converse:?}"
        );
        if let Some(reasoning) = reasoning {
            assert_eq!(
                reasoning.get("embedded").and_then(Value::as_bool),
                Some(false),
                "case {name:?}: converse.reasoningSupported names where the reasoning arrives, \
                 and every model of this catalogue answers in a block of its own, got \
                 {converse:?}"
            );
        }
        // The member carries the formats the row's provider line publishes, and the empty
        // list for a line no document block reaches.
        let documents = string_list(converse, "userDocumentTypesSupported");
        let expected: Vec<String> = expected_user_document_formats(model)
            .iter()
            .map(|format| (*format).to_string())
            .collect();
        assert_eq!(
            documents,
            Some(expected.clone()),
            "case {name:?}: expected converse.userDocumentTypesSupported {expected:?}, got \
             {documents:?}"
        );
        // And an image format list, where the model takes images, is the order its provider
        // line publishes.
        if model.input_modalities.contains(&"IMAGE") {
            let images = string_list(converse, "userImageTypesSupported");
            let expected: Vec<String> = expected_user_image_formats(model)
                .iter()
                .map(|format| (*format).to_string())
                .collect();
            assert_eq!(
                images,
                Some(expected.clone()),
                "case {name:?}: expected converse.userImageTypesSupported {expected:?}, got \
                 {images:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// 3. What a Converse request may CARRY and ASK is answered for every model the API reaches.
// ---------------------------------------------------------------------------------------

/// One row of the table below: a published model, and what the rule says the two members a
/// request is BUILT from answer for it.
struct RequestMetadataCase {
    /// The `modelId` the listing carries the summary under.
    model_id: &'static str,
    /// What this row contributes: the combination of published facts it stands for.
    note: &'static str,
    /// The schema of the provider-specific fields a request may carry, as the JSON text the
    /// member reports; `"{}"` for a line that accepts none.
    schema: &'static str,
    /// The ceiling a request may ask for, or `None` for a model whose line publishes none.
    max_tokens_maximum: Option<i64>,
    /// The document formats a turn may attach; empty for a model no document reaches.
    documents: &'static [&'static str],
}

/// The schema the one line this catalogue publishes a provider-specific request field for
/// accepts: a reasoning-effort choice, with the effort the model spends when a request
/// names none as its default.
const REASONING_EFFORT_SCHEMA: &str = concat!(
    r#"{"reasoning_config":{"type":"enum","default":"medium","#,
    r#""enum":["low","medium","high","xhigh","max"]}}"#
);

/// A line that accepts no provider-specific request field accepts the empty JSON object.
const NO_REQUEST_FIELDS_SCHEMA: &str = "{}";

/// The table: the rows differ along what the catalogue publishes for the model's line — a
/// line with a provider-specific request field and one without, a model with a ceiling of
/// its own and one that reads its line's, a provider that publishes neither, and a line
/// published with document input.
const REQUEST_METADATA_CASES: &[RequestMetadataCase] = &[
    RequestMetadataCase {
        model_id: "openai.gpt-6-astra",
        note: "a line that publishes a provider-specific request field, and a line ceiling",
        schema: REASONING_EFFORT_SCHEMA,
        max_tokens_maximum: Some(131_072),
        documents: &[],
    },
    RequestMetadataCase {
        model_id: "nvidia.nemotron-nano-12b-v2",
        note: "a line that publishes no extra request field, and a line ceiling",
        schema: NO_REQUEST_FIELDS_SCHEMA,
        max_tokens_maximum: Some(131_072),
        documents: &[],
    },
    RequestMetadataCase {
        model_id: "amazon.nova-pro-v1:0",
        note: "a model with a ceiling published for it rather than for its line",
        schema: NO_REQUEST_FIELDS_SCHEMA,
        max_tokens_maximum: Some(5120),
        documents: &[],
    },
    RequestMetadataCase {
        model_id: "anthropic.claude-opus-4-6-v1",
        note: "a model of the line published with document input, whose ceiling is published \
               nowhere",
        schema: NO_REQUEST_FIELDS_SCHEMA,
        max_tokens_maximum: None,
        documents: &["pdf"],
    },
    RequestMetadataCase {
        model_id: "zai.glm-4.7",
        note: "a provider that publishes no ceiling, per model or per line",
        schema: NO_REQUEST_FIELDS_SCHEMA,
        max_tokens_maximum: None,
        documents: &[],
    },
];

#[tokio::test]
async fn what_a_request_may_carry_and_ask_is_answered_for_every_model_converse_reaches() {
    let emu = emulator();
    let listed = summaries(&list(&emu).await);

    for case in REQUEST_METADATA_CASES {
        let name = case.model_id;
        let note = case.note;
        let summary = summary_of(name, &listed, name);
        let converse = summary.get("converse").unwrap_or_else(|| {
            panic!(
                "case {name:?} ({note}): a conversational model carries a Converse record, got \
                 {summary:?}"
            )
        });

        // Which extra fields a request may carry is answered for every model the API
        // reaches, as a JSON OBJECT a client can read — the empty object where the line
        // accepts none, never an absent member.
        let schema = converse
            .get("additionalRequestFieldsSchema")
            .and_then(Value::as_str);
        assert_eq!(
            schema,
            Some(case.schema),
            "case {name:?} ({note}): expected converse.additionalRequestFieldsSchema {:?}, got \
             {schema:?}",
            case.schema
        );
        let parsed: Value = serde_json::from_str(schema.unwrap_or_default()).unwrap_or_else(|error| {
            panic!(
                "case {name:?} ({note}): converse.additionalRequestFieldsSchema carries a JSON \
                 schema, and {schema:?} does not parse: {error}"
            )
        });
        assert!(
            parsed.is_object(),
            "case {name:?} ({note}): expected converse.additionalRequestFieldsSchema to be a \
             JSON object, got {parsed:?}"
        );

        // How large an answer a request may ask for is the ceiling published for the
        // model's line, which a model's own figure overrides.
        let ceiling = converse.get("maxTokensMaximum").and_then(Value::as_i64);
        assert_eq!(
            ceiling, case.max_tokens_maximum,
            "case {name:?} ({note}): expected converse.maxTokensMaximum {:?}, got {ceiling:?}",
            case.max_tokens_maximum
        );

        // And what a turn may attach is a list, empty where nothing of that kind reaches
        // the model.
        let documents = string_list(converse, "userDocumentTypesSupported");
        let expected: Vec<String> = case
            .documents
            .iter()
            .map(|format| (*format).to_string())
            .collect();
        assert_eq!(
            documents,
            Some(expected.clone()),
            "case {name:?} ({note}): expected converse.userDocumentTypesSupported {expected:?}, \
             got {documents:?}"
        );

        // A read of the model reports the same record, so a client that discovers a model
        // and then reads it builds the same request either way.
        let reply = read(&emu, name).await;
        assert_eq!(
            reply.status, 200,
            "case {name:?} ({note}): a read of a published model must answer 200, got {} {:?}",
            reply.status, reply.body
        );
        let details = reply.get("modelDetails");
        let read_converse = details.get("converse");
        assert_eq!(
            read_converse,
            Some(converse),
            "case {name:?} ({note}): expected the read to report the listed Converse record \
             {converse:?}, got {read_converse:?}"
        );
    }
}

#[tokio::test]
async fn every_model_converse_reaches_answers_both_request_members_and_no_other_does() {
    let emu = emulator();
    let listed = summaries(&list(&emu).await);

    for model in FOUNDATION_MODELS {
        let name = model.model_id;
        let summary = summary_of(name, &listed, name);
        let converse = summary.get("converse");
        // Converse carries a WRITTEN conversation, so the record — and with it every
        // member a request to that API is built from — is reported for a model that answers
        // a written turn (it produces TEXT and does not answer by speaking, since a spoken
        // turn is carried by the stream the model is driven through) and for no other.
        let written_turn = model.output_modalities.contains(&"TEXT")
            && !model.output_modalities.contains(&"SPEECH");
        assert_eq!(
            converse.is_some(),
            written_turn,
            "case {name:?}: the Converse record is reported for a model that answers a written \
             turn ({written_turn:?}) and for no other, got {summary:?}"
        );
        let Some(converse) = converse else {
            continue;
        };
        let schema = converse
            .get("additionalRequestFieldsSchema")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "case {name:?}: every model the Converse API reaches answers which extra \
                     fields a request may carry, got {converse:?}"
                )
            });
        let parsed: Value = serde_json::from_str(schema).unwrap_or_else(|error| {
            panic!(
                "case {name:?}: converse.additionalRequestFieldsSchema carries a JSON schema, \
                 and {schema:?} does not parse: {error}"
            )
        });
        assert!(
            parsed.is_object(),
            "case {name:?}: expected converse.additionalRequestFieldsSchema to be a JSON object, \
             got {parsed:?}"
        );
        assert!(
            string_list(converse, "userDocumentTypesSupported").is_some(),
            "case {name:?}: every model the Converse API reaches answers what a turn may \
             attach, with the empty list where no document reaches it, got {converse:?}"
        );
        // A ceiling is reported exactly where the catalogue publishes one for the model or
        // for its line, and it is the figure that is published — stated by the suites' own
        // rendering of the published figures rather than read back off the service.
        let ceiling = converse.get("maxTokensMaximum").and_then(Value::as_i64);
        let published = expected_max_output_tokens(model).map(i64::from);
        assert_eq!(
            ceiling, published,
            "case {name:?}: expected converse.maxTokensMaximum {published:?}, got {ceiling:?}"
        );
    }
}
