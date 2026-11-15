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
//! 3. **A member the catalogue publishes nothing for is OMITTED**, never defaulted to a
//!    fixed list: the document formats a turn may carry are published per provider line
//!    (`DocumentFormat` of the Amazon Bedrock Runtime model names the vocabulary), and a
//!    model no document block reaches carries no `converse.userDocumentTypesSupported` at
//!    all rather than the whole vocabulary.
//! 4. **A format list keeps the order it is published in**: `converse.userImageTypesSupported`
//!    reports the `ImageFormat` values in the published order, because a client that takes
//!    the FIRST supported format as its default reads a different default from a different
//!    order.
//!
//! The capability members are RECORDED behaviour: the on-disk model declares none of them
//! (`smithy_query(query="shape", shape="com.amazonaws.bedrock#FoundationModelSummary")`
//! lists modelArn/modelId/modelName/providerName/inputModalities/outputModalities/
//! responseStreamingSupported/customizationsSupported/inferenceTypesSupported/
//! modelLifecycle), so the expectations here are the rules above, stated per row.

use serde_json::Value;

use bedrock::state::{FoundationModel, FOUNDATION_MODELS};
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;

use crate::support::{emulator, expected_user_document_formats, label, Emulator, Reply, REGION};

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

/// One row of the catalogue and what the rules above say its capability members are.
struct Case {
    /// The `modelId` the listing carries the summary under.
    model_id: &'static str,
    /// What this row contributes to the table: the attribute combination it stands for.
    note: &'static str,
    /// Whether a model invocation job may run the model, in either region scope.
    batch_inference: bool,
    /// Whether reserved capacity may be bought for the model.
    provisioned_throughput: bool,
    /// Whether extended reasoning may be asked of the model.
    reasoning: bool,
    /// The document formats a turn may carry, or `None` for a model no document reaches.
    documents: Option<&'static [&'static str]>,
    /// The image formats a turn may carry; empty for a model that takes no image.
    images: &'static [&'static str],
}

/// The table: the rows differ along every attribute the rules generalise over — which
/// features the row publishes (none, batch only, batch and routing), whether it is offered
/// with Provisioned Throughput, whether its provider publishes reasoning for it, which
/// modalities it takes, and who publishes it (a provider line published with document input
/// and three that are not).
const CASES: &[Case] = &[
    Case {
        model_id: "openai.gpt-6-astra",
        note: "no feature's table lists it, on demand only, takes text and images",
        batch_inference: false,
        provisioned_throughput: false,
        reasoning: false,
        documents: None,
        images: PUBLISHED_IMAGE_FORMATS,
    },
    Case {
        model_id: "anthropic.claude-opus-4-6-v1",
        note: "a reasoning model of the provider line published with document input",
        batch_inference: false,
        provisioned_throughput: false,
        reasoning: true,
        documents: Some(&["pdf"]),
        images: PUBLISHED_IMAGE_FORMATS,
    },
    Case {
        model_id: "anthropic.claude-haiku-4-5-20251001-v1:0",
        note: "every feature's table lists it, on demand only, document input published for \
               its provider line",
        batch_inference: true,
        provisioned_throughput: false,
        reasoning: true,
        documents: Some(&["pdf"]),
        images: PUBLISHED_IMAGE_FORMATS,
    },
    Case {
        model_id: "meta.llama3-1-70b-instruct-v1:0",
        note: "batch inference published and reserved capacity sold, text only",
        batch_inference: true,
        provisioned_throughput: true,
        reasoning: false,
        documents: None,
        images: &[],
    },
    Case {
        model_id: "meta.llama3-3-70b-instruct-v1:0",
        note: "batch inference published, on demand only, text only",
        batch_inference: true,
        provisioned_throughput: false,
        reasoning: false,
        documents: None,
        images: &[],
    },
    Case {
        model_id: "qwen.qwen3-32b-v1:0",
        note: "a reasoning model of a line published with no document input",
        batch_inference: false,
        provisioned_throughput: false,
        reasoning: true,
        documents: None,
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
        for scope in ["crossRegion", "inRegion"] {
            assert_eq!(
                batch.get(scope).and_then(Value::as_bool),
                Some(case.batch_inference),
                "case {name:?} ({note}): expected featuresSupported.batchInference.{scope} {:?}, \
                 got {batch:?}",
                case.batch_inference
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
        let reasoning = summary
            .get("converse")
            .and_then(|converse| converse.get("reasoningSupported"))
            .unwrap_or_else(|| {
                panic!(
                    "case {name:?} ({note}): a conversational model carries a Converse record \
                     with a reasoning record, got {summary:?}"
                )
            });
        assert!(
            reasoning.is_object(),
            "case {name:?} ({note}): expected converse.reasoningSupported to be a record, got \
             {reasoning:?}"
        );
        assert_eq!(
            reasoning.get("isSupported").and_then(Value::as_bool),
            Some(case.reasoning),
            "case {name:?} ({note}): expected converse.reasoningSupported.isSupported {:?}, got \
             {reasoning:?}",
            case.reasoning
        );
        // Reasoning arrives in a block of its own for every model this catalogue publishes,
        // so a client never has to parse it out of the answer text.
        assert_eq!(
            reasoning.get("embedded").and_then(Value::as_bool),
            Some(false),
            "case {name:?} ({note}): expected converse.reasoningSupported.embedded false, got \
             {reasoning:?}"
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
        // block reaches carries NO member rather than the whole vocabulary.
        let documents = string_list(converse, "userDocumentTypesSupported");
        let expected: Option<Vec<String>> = case
            .documents
            .map(|formats| formats.iter().map(|format| (*format).to_string()).collect());
        assert_eq!(
            documents, expected,
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
        let Some(features) = summary.get("featuresSupported") else {
            // A model that answers no turn of a conversation carries no feature record at
            // all; the suite that owns that rule asserts it.
            continue;
        };
        let batch = features.get("batchInference").unwrap_or(&Value::Null);
        let runs_batch = model.features_supported.contains(&"BATCH_INFERENCE");
        for scope in ["crossRegion", "inRegion"] {
            assert_eq!(
                batch.get(scope).and_then(Value::as_bool),
                Some(runs_batch),
                "case {name:?}: featuresSupported.batchInference answers per region scope, so \
                 expected {scope} {runs_batch:?}, got {batch:?}"
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

        let converse = summary
            .get("converse")
            .unwrap_or_else(|| panic!("case {name:?}: a conversational model carries a Converse record, got {summary:?}"));
        assert_eq!(
            converse
                .get("reasoningSupported")
                .and_then(|reasoning| reasoning.get("embedded"))
                .and_then(Value::as_bool),
            Some(false),
            "case {name:?}: converse.reasoningSupported names where the reasoning arrives, and \
             every model of this catalogue answers in a block of its own, got {converse:?}"
        );
        // The member is there exactly while the row's provider line publishes document
        // input, and carries the formats that line publishes.
        let documents = string_list(converse, "userDocumentTypesSupported");
        let expected: Option<Vec<String>> = expected_user_document_formats(model)
            .map(|formats| formats.iter().map(|format| (*format).to_string()).collect());
        assert_eq!(
            documents, expected,
            "case {name:?}: expected converse.userDocumentTypesSupported {expected:?}, got \
             {documents:?}"
        );
        // And an image format list, where the model takes images, is the published order.
        if model.input_modalities.contains(&"IMAGE") {
            let images = string_list(converse, "userImageTypesSupported");
            let expected: Vec<String> = PUBLISHED_IMAGE_FORMATS
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
