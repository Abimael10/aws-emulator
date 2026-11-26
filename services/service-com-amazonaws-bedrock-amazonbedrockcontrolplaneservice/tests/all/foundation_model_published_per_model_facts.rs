// SPDX-License-Identifier: CC0-1.0

//! What `ListFoundationModels` publishes PER MODEL, as against what it may compute from a
//! model's generic attributes.
//!
//! The rule under test, asserted over a SET of rows rather than over one recorded listing:
//! **a capability Amazon Bedrock publishes for a foundation model is a fact of that model's
//! own published data — its catalogue row and the published tables keyed by its `modelId` —
//! so two models that share their modalities, their streaming and their provider still
//! report different capabilities when the catalogue publishes different facts for them.**
//! Only what genuinely FOLLOWS from an attribute the row carries (whether an answer arrives
//! in one piece, whether a guardrail has written text to decide about) is computed.
//!
//! The members this suite covers, each of which was derived from a generic attribute before:
//!
//! 1. `inferenceAPIsSupported.openAiChatCompletions` and `.openAiResponses` — the two
//!    OpenAI-compatible endpoints are Amazon Bedrock surfaces in the SHAPE of another API,
//!    each published for the models it serves, so neither follows from the model's provider
//!    and one of them answering is no reason for the other to.
//! 2. `featuresSupported.promptOptimization` — a feature with a supported-models table of its
//!    own, so it is published per model rather than implied by a model being conversational.
//! 3. `featuresSupported.batchInference` and `batchSupported` — batch inference publishes one
//!    table per REGION SCOPE, so the scope the model is offered in and the cross-region scope
//!    are two published facts.
//! 4. `inferenceTypesSupported` — a model Amazon Bedrock offers only through a cross-region
//!    inference profile publishes that, and no reserved capacity is sold over it.
//! 5. `converse.userImageTypesSupported` — the format SET is what the Converse API accepts,
//!    while the ORDER is published with the provider's own integration, and a client that
//!    takes the first supported format as its default reads a different default from a
//!    different order.
//! 6. Catalogue MEMBERSHIP — the version-and-context-qualified id of a model is published
//!    beside its base id, carrying that model's own facts.
//!
//! Every one of these members is RECORDED behaviour: the on-disk model declares the ten
//! members `FoundationModelSummary` carries and none of them
//! (`smithy_query(query="shape",shape="FoundationModelSummary")` lists modelArn/modelId/
//! modelName/providerName/inputModalities/outputModalities/responseStreamingSupported/
//! customizationsSupported/inferenceTypesSupported/modelLifecycle), so the expectations here
//! are the rules above, stated per row and checked against the published data.

use serde_json::Value;

use bedrock::state::{FoundationModel, FOUNDATION_MODELS};
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;

use crate::support::{
    emulator, expected_foundation_model_projection, foundation_model_arn, label, Emulator, Reply,
    REGION,
};

/// The path `ListFoundationModels` is bound to, and the prefix `GetFoundationModel` reads
/// one model under (`smithy_query(query="operation",operation="ListFoundationModels")`).
const PATH: &str = "/foundation-models";

async fn list(emu: &Emulator, query: &str) -> Reply {
    let path = if query.is_empty() {
        PATH.to_string()
    } else {
        format!("{PATH}?{query}")
    };
    emu.call_in_region(REGION, "GET", &path, None).await
}

async fn read(emu: &Emulator, model_id: &str) -> Reply {
    emu.call_in_region(REGION, "GET", &format!("{PATH}/{}", label(model_id)), None)
        .await
}

/// Every summary a listing carries.
fn summaries(case: &str, reply: &Reply) -> Vec<Value> {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: a listing must answer 200, got {} {:?}",
        reply.status, reply.body
    );
    reply
        .get("modelSummaries")
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: modelSummaries must be a present array, got {:?}",
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

/// The string list `member` carries, or `None` when the member is absent.
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
// 1. The two OpenAI-compatible endpoints are published per model.
// ---------------------------------------------------------------------------------------

/// One row of the endpoint table: a published model, which of the two endpoints Amazon
/// Bedrock publishes it on, and what the row stands for.
struct EndpointCase {
    model_id: &'static str,
    note: &'static str,
    chat_completions: bool,
    responses: bool,
}

/// The table spans what the rule generalises over: both endpoints, one of them, neither, for
/// a model of the provider whose own API they are named after and for models of four other
/// providers — including ids the reports name and ids they do not.
const ENDPOINT_CASES: &[EndpointCase] = &[
    EndpointCase {
        model_id: "openai.gpt-6-astra",
        note: "a model of the provider the endpoints are named after, published on both",
        chat_completions: true,
        responses: true,
    },
    EndpointCase {
        model_id: "openai.gpt-oss-120b-1:0",
        note: "a model of that same provider published on Chat Completions alone",
        chat_completions: true,
        responses: false,
    },
    EndpointCase {
        model_id: "nvidia.nemotron-nano-12b-v2",
        note: "a model of another provider published on both",
        chat_completions: true,
        responses: true,
    },
    EndpointCase {
        model_id: "qwen.qwen3-coder-480b-a35b-v1:0",
        note: "a model published on Chat Completions alone",
        chat_completions: true,
        responses: false,
    },
    EndpointCase {
        model_id: "zai.glm-5.3",
        note: "an id neither report cites as an example, published on both",
        chat_completions: true,
        responses: true,
    },
    EndpointCase {
        model_id: "amazon.nova-pro-v1:0",
        note: "a conversational model neither endpoint is published for",
        chat_completions: false,
        responses: false,
    },
    EndpointCase {
        model_id: "writer.palmyra-x4-v1:0",
        note: "another conversational model neither endpoint is published for",
        chat_completions: false,
        responses: false,
    },
];

#[tokio::test]
async fn an_openai_compatible_endpoint_answers_for_the_models_it_is_published_for() {
    let emu = emulator();
    let listed = summaries("the whole catalogue", &list(&emu, "").await);

    for case in ENDPOINT_CASES {
        let name = case.model_id;
        let note = case.note;
        let model = row(name, name);
        let summary = summary_of(name, &listed, name);
        let apis = summary.get("inferenceAPIsSupported").unwrap_or(&Value::Null);
        for (member, published) in [
            ("openAiChatCompletions", case.chat_completions),
            ("openAiResponses", case.responses),
        ] {
            assert_eq!(
                apis.get(member).and_then(Value::as_bool),
                Some(published),
                "case {name:?} ({note}): expected inferenceAPIsSupported.{member} \
                 {published:?}, got {apis:?}"
            );
        }
        // The provider decides nothing here: these rows include a model of the provider the
        // endpoints are named after that only one of them is published for, and models of
        // other providers that both are.
        assert_eq!(
            (case.chat_completions, case.responses),
            (
                bedrock::state::model_open_ai_chat_completions_supported(model),
                bedrock::state::model_open_ai_responses_supported(model),
            ),
            "case {name:?} ({note}): the published tables must carry the same answer the \
             summary reports, got the row {model:?}"
        );

        // A read of that one model answers what the listing carries for it.
        let reply = read(&emu, name).await;
        assert_eq!(
            reply.status, 200,
            "case {name:?} ({note}): a read of a published model must answer 200, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.get("modelDetails"),
            summary,
            "case {name:?} ({note}): expected the read to report the listed projection"
        );
    }
}

#[tokio::test]
async fn every_summary_reports_the_endpoints_published_for_its_own_model_id() {
    let emu = emulator();
    let listed = summaries("the whole catalogue", &list(&emu, "").await);

    for model in FOUNDATION_MODELS {
        let name = model.model_id;
        let summary = summary_of(name, &listed, name);
        let apis = summary.get("inferenceAPIsSupported").unwrap_or(&Value::Null);
        for (member, published) in [
            (
                "openAiChatCompletions",
                bedrock::state::OPENAI_CHAT_COMPLETIONS_MODELS.contains(&name),
            ),
            (
                "openAiResponses",
                bedrock::state::OPENAI_RESPONSES_MODELS.contains(&name),
            ),
        ] {
            assert_eq!(
                apis.get(member).and_then(Value::as_bool),
                Some(published),
                "case {name:?}: inferenceAPIsSupported.{member} answers out of the published \
                 table of the endpoint, so expected {published:?}, got {apis:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// 2. Prompt optimization is published per model.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn prompt_optimization_answers_what_the_catalogue_publishes_for_the_model() {
    let emu = emulator();
    let listed = summaries("the whole catalogue", &list(&emu, "").await);

    // The rows span what the member was derived from before: a streaming conversational
    // model, one that answers in one piece, a vision model, a model that answers by
    // SPEAKING, an embedding model and an image model.
    let spanned = [
        ("amazon.nova-pro-v1:0", "a streaming conversational model"),
        ("mistral.ministral-3-3b-instruct", "another written-turn model"),
        ("openai.gpt-6-astra", "a model that takes text and images"),
        ("amazon.nova-2-sonic-v1:0", "a model that answers by speaking"),
        ("amazon.titan-embed-text-v2:0", "an embedding model"),
        ("stability.stable-image-ultra-v1:1", "an image model"),
    ];

    for (model_id, note) in spanned {
        let model = row(model_id, model_id);
        let published = model.features_supported.contains(&"PROMPT_OPTIMIZATION");
        let summary = summary_of(model_id, &listed, model_id);
        let features = summary.get("featuresSupported").unwrap_or(&Value::Null);
        assert_eq!(
            features.get("promptOptimization").and_then(Value::as_bool),
            Some(published),
            "case {model_id:?} ({note}): featuresSupported.promptOptimization answers what the \
             row publishes ({published:?}) rather than what the model's turn is, got {features:?}"
        );
    }

    // And over the whole catalogue, so a conversational row cannot report a feature its
    // published data does not carry.
    for model in FOUNDATION_MODELS {
        let name = model.model_id;
        let published = model.features_supported.contains(&"PROMPT_OPTIMIZATION");
        let summary = summary_of(name, &listed, name);
        let features = summary.get("featuresSupported").unwrap_or(&Value::Null);
        assert_eq!(
            features.get("promptOptimization").and_then(Value::as_bool),
            Some(published),
            "case {name:?}: expected featuresSupported.promptOptimization {published:?}, got \
             {features:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3. Batch inference is published per region scope.
// ---------------------------------------------------------------------------------------

/// One row of the batch table: a published model and the two scopes the catalogue publishes
/// batch inference for it in.
struct BatchCase {
    model_id: &'static str,
    note: &'static str,
    in_region: bool,
    cross_region: bool,
}

/// The table spans the three combinations the published tables produce — both scopes, the
/// region the model is offered in alone, and neither — over a conversational model, a vision
/// model and two embedding models, since `batchSupported` is reported for a model that
/// answers a turn and `featuresSupported.batchInference` for every offered model.
const BATCH_CASES: &[BatchCase] = &[
    BatchCase {
        model_id: "meta.llama3-3-70b-instruct-v1:0",
        note: "a conversational model both scopes' tables list",
        in_region: true,
        cross_region: true,
    },
    BatchCase {
        model_id: "nvidia.nemotron-nano-12b-v2",
        note: "a model only the in-region table lists",
        in_region: true,
        cross_region: false,
    },
    BatchCase {
        model_id: "amazon.titan-embed-text-v2:0",
        note: "an embedding model only the in-region table lists",
        in_region: true,
        cross_region: false,
    },
    BatchCase {
        model_id: "amazon.titan-embed-text-v1",
        note: "an embedding model neither table lists",
        in_region: false,
        cross_region: false,
    },
    BatchCase {
        model_id: "openai.gpt-6-astra",
        note: "a conversational model neither table lists",
        in_region: false,
        cross_region: false,
    },
];

#[tokio::test]
async fn batch_inference_answers_per_region_scope_out_of_each_scopes_own_table() {
    let emu = emulator();
    let listed = summaries("the whole catalogue", &list(&emu, "").await);

    for case in BATCH_CASES {
        let name = case.model_id;
        let note = case.note;
        let model = row(name, name);
        let summary = summary_of(name, &listed, name);
        let features = summary.get("featuresSupported").unwrap_or(&Value::Null);
        let batch_inference = features.get("batchInference").unwrap_or(&Value::Null);
        for (scope, published) in [
            ("inRegion", case.in_region),
            ("crossRegion", case.cross_region),
        ] {
            assert_eq!(
                batch_inference.get(scope).and_then(Value::as_bool),
                Some(published),
                "case {name:?} ({note}): expected featuresSupported.batchInference.{scope} \
                 {published:?}, got {batch_inference:?}"
            );
        }

        // `batchSupported` reads the same published facts, so the two records cannot
        // disagree about the model; it is reported for a model that answers a turn of a
        // conversation, which the suite owning that rule asserts.
        if let Some(batch) = summary.get("batchSupported") {
            assert_eq!(
                (
                    batch.get("baseModelSupported").and_then(Value::as_bool),
                    batch.get("crossRegionSupported").and_then(Value::as_bool),
                ),
                (Some(case.in_region), Some(case.cross_region)),
                "case {name:?} ({note}): batchSupported reads the same published scopes as the \
                 feature record, so expected {:?} and {:?}, got {batch:?}",
                case.in_region,
                case.cross_region
            );
            let customizable =
                case.in_region && !model.customizations_supported.is_empty();
            assert_eq!(
                batch.get("customModelSupported").and_then(Value::as_bool),
                Some(customizable),
                "case {name:?} ({note}): a customization of the model is batch-run only where \
                 the model accepts one, so expected {customizable:?}, got {batch:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// 4. The inference types a model is offered under are published for it.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_model_offered_through_an_inference_profile_alone_publishes_that() {
    let emu = emulator();
    let listed = summaries("the whole catalogue", &list(&emu, "").await);

    // The rows span the three offerings the catalogue publishes: a model reached through a
    // cross-region inference profile alone, one offered on demand, and one offered on demand
    // with reserved capacity beside it.
    let spanned = [
        (
            "openai.gpt-6-astra",
            "offered through an inference profile alone",
            vec!["INFERENCE_PROFILE"],
            false,
        ),
        (
            "openai.gpt-6-luna",
            "a sibling of the same line, offered on demand",
            vec!["ON_DEMAND"],
            false,
        ),
        (
            "meta.llama3-1-70b-instruct-v1:0",
            "offered on demand and with reserved capacity",
            vec!["ON_DEMAND", "PROVISIONED"],
            true,
        ),
    ];

    for (model_id, note, types, provisioned) in spanned {
        let summary = summary_of(model_id, &listed, model_id);
        let reported = string_list(&summary, "inferenceTypesSupported");
        let expected: Vec<String> = types.iter().map(|value| (*value).to_string()).collect();
        assert_eq!(
            reported,
            Some(expected.clone()),
            "case {model_id:?} ({note}): expected inferenceTypesSupported {expected:?}, got \
             {reported:?}"
        );
        // Reserved capacity is sold over exactly the models the catalogue offers it under,
        // so the feature follows the published list rather than the other way round.
        let features = summary.get("featuresSupported").unwrap_or(&Value::Null);
        assert_eq!(
            features.get("provisionedThroughput").and_then(Value::as_bool),
            Some(provisioned),
            "case {model_id:?} ({note}): expected featuresSupported.provisionedThroughput \
             {provisioned:?}, got {features:?}"
        );
    }

    // A filter narrows the listing to the rows whose published list carries the value, so a
    // model offered through a profile alone is in neither on-demand nor provisioned listing.
    for (query, note) in [
        ("byInferenceType=ON_DEMAND", "the on-demand listing"),
        ("byInferenceType=PROVISIONED", "the provisioned listing"),
    ] {
        let filtered = summaries(note, &list(&emu, query).await);
        let ids: Vec<&str> = filtered
            .iter()
            .filter_map(|row| row.get("modelId").and_then(Value::as_str))
            .collect();
        assert!(
            !ids.contains(&"openai.gpt-6-astra"),
            "case {note:?}: a model offered through an inference profile alone is offered under \
             neither filtered inference type, got {} rows carrying it",
            ids.len()
        );
        assert!(
            !ids.is_empty(),
            "case {note:?}: the catalogue must offer models under this inference type for the \
             rule to bite"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 5. A format list keeps the order its provider line publishes.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn the_image_formats_keep_the_order_the_provider_line_publishes_them_in() {
    let emu = emulator();
    let listed = summaries("the whole catalogue", &list(&emu, "").await);

    // The rows span the orders the catalogue publishes and the model that takes no image at
    // all, over three provider lines.
    let spanned = [
        (
            "nvidia.nemotron-nano-12b-v2",
            "a line published PNG first",
            vec!["png", "jpeg", "gif", "webp"],
        ),
        (
            "openai.gpt-6-astra",
            "a line published JPEG first",
            vec!["jpeg", "png", "gif", "webp"],
        ),
        (
            "anthropic.claude-opus-4-6-v1",
            "another line published JPEG first",
            vec!["jpeg", "png", "gif", "webp"],
        ),
        (
            "mistral.ministral-3-3b-instruct",
            "a model that takes no image, so nothing of that kind reaches it",
            Vec::new(),
        ),
    ];

    for (model_id, note, formats) in spanned {
        let summary = summary_of(model_id, &listed, model_id);
        let converse = summary.get("converse").unwrap_or_else(|| {
            panic!("case {model_id:?} ({note}): a conversational model carries a Converse record")
        });
        let reported = string_list(converse, "userImageTypesSupported");
        let expected: Vec<String> = formats.iter().map(|value| (*value).to_string()).collect();
        assert_eq!(
            reported,
            Some(expected.clone()),
            "case {model_id:?} ({note}): expected converse.userImageTypesSupported {expected:?}, \
             got {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 6. A qualified id is published beside the base id of the same model.
// ---------------------------------------------------------------------------------------

/// The version-and-context-qualified ids this rule covers, with the base id of the model
/// each one is another id of: two deprecated Cohere Command models and two Cohere Embed
/// models, published with one qualifier and with two.
const QUALIFIED_IDS: &[(&str, &str)] = &[
    ("cohere.command-text-v14:7:4k", "cohere.command-text-v14"),
    (
        "cohere.command-light-text-v14:7:4k",
        "cohere.command-light-text-v14",
    ),
    ("cohere.embed-english-v3:0:512", "cohere.embed-english-v3:0"),
    (
        "cohere.embed-multilingual-v3:0:512",
        "cohere.embed-multilingual-v3:0",
    ),
];

#[tokio::test]
async fn a_qualified_id_is_listed_beside_the_base_id_of_the_same_model() {
    let emu = emulator();
    let listed = summaries("the whole catalogue", &list(&emu, "").await);

    for (qualified, base) in QUALIFIED_IDS {
        let case = *qualified;
        let qualified_summary = summary_of(case, &listed, qualified);
        let base_summary = summary_of(case, &listed, base);

        // The qualified row is the projection of its own catalogue row, computed by the
        // suites' own rendering of the published data.
        let model = row(case, qualified);
        assert_eq!(
            qualified_summary,
            expected_foundation_model_projection(model, REGION),
            "case {case:?}: expected the listing to report the published row's projection"
        );
        // It carries its OWN id and ARN, and every other published fact of the model is the
        // one its base id reports: one published model, two ids a client may address it by.
        assert_eq!(
            qualified_summary.get("modelArn").and_then(Value::as_str),
            Some(foundation_model_arn(REGION, qualified).as_str()),
            "case {case:?}: expected the summary's ARN to carry the id it is listed under, got \
             {qualified_summary:?}"
        );
        for member in [
            "modelName",
            "providerName",
            "inputModalities",
            "outputModalities",
            "responseStreamingSupported",
            "modelLifecycle",
        ] {
            assert_eq!(
                qualified_summary.get(member),
                base_summary.get(member),
                "case {case:?}: {member} is a fact of the model, so the qualified id must \
                 report what {base:?} reports"
            );
        }

        // And a read of the qualified id answers the row the listing carries for it.
        let reply = read(&emu, qualified).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: a read of a published id must answer 200, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.get("modelDetails"),
            qualified_summary,
            "case {case:?}: expected the read to report the listed projection"
        );
    }
}
