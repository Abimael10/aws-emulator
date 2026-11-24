// SPDX-License-Identifier: CC0-1.0

//! `ListFoundationModels` end-to-end, over the same `MultiServiceRouter` the emulator
//! boots.
//!
//! The rules under test, each asserted over a SET of inputs rather than over one recorded
//! request:
//!
//! 1. **The listing IS the published catalog**: one `FoundationModelSummary` per row of
//!    the catalog the store is seeded with, in `modelId` order, every member reported out
//!    of that row — so the response is read off state rather than written per request.
//! 2. **Each `by*` member narrows the listing on one published attribute, and filters
//!    combine by conjunction**: the models a filtered listing reports are exactly the
//!    catalog rows whose own attribute satisfies every filter the request carried. The
//!    rows span each filterable member, every value of each of them, both spellings
//!    `byProvider` accepts, conjunctions of two and three filters, and filters that admit
//!    nothing.
//! 3. **A model's ARN is the `foundation-model` ARN for the region the request
//!    addresses**, with the EMPTY account field `FoundationModelArn` declares
//!    (`smithy_query(query="shape",shape="FoundationModelSummary")`), and the catalog
//!    itself does not vary by region.
//! 4. **A value no filter member admits is refused with 400 `ValidationException`** by the
//!    constraints the model declares for it, never listed around.
//! 5. **Listing is a pure read**: two identical listings answer identically, and a listing
//!    does not disturb the guardrail store beside it.
//! 6. **Every catalog row is modelled data**: each value is one the matching Smithy enum
//!    carries and each name satisfies its member's `@length`, and the catalog spans every
//!    value a caller can filter on, which is what makes rule 2's table meaningful.
//! 7. **A model a customization job accepts is published under a training-qualified id
//!    beside its on-demand one, and only while a customization of it is still offered**:
//!    a DEPRECATED model (`LEGACY`) takes no new customization job, so the catalog
//!    carries it under its base id alone — no row of it publishes a customization and no
//!    listing reports a training-qualified row for it — while the qualified id goes on
//!    naming the same published model and still resolves to that base row.
//!
//! Expectations come from the model
//! (`smithy_query(query="operation",operation="ListFoundationModels")` and
//! `query="shape"` on `FoundationModelSummary`, `ModelModality`, `ModelCustomization`,
//! `InferenceType`, `FoundationModelLifecycleStatus`, `BrandedName`, `Provider`), and the
//! requests go through `crate::support` — the ONE harness every suite in this crate drives
//! the emulator with.

use serde_json::{json, Value};

use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::ops::types::{
    FOUNDATION_MODEL_LIFECYCLE_STATUS_VALUES, INFERENCE_TYPE_VALUES, MODEL_CUSTOMIZATION_VALUES,
    MODEL_MODALITY_VALUES,
};
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::state::{
    FoundationModel, FOUNDATION_MODELS,
};

use crate::support::{
    emulator, error_code, expected_foundation_model_projection, label, Emulator, Reply, REGION,
};

/// The one path `ListFoundationModels` is bound to
/// (`smithy_query(query="operation",operation="ListFoundationModels")`: `GET
/// /foundation-models`).
const PATH: &str = "/foundation-models";

/// `BrandedName`'s declared length span, which `modelName` and `providerName` both target.
const BRANDED_NAME_LENGTH: std::ops::RangeInclusive<usize> = 1..=20;

/// `BedrockModelId`'s declared maximum length, which `modelId` targets.
const MODEL_ID_MAX_LENGTH: usize = 140;

async fn list(emu: &Emulator, query: &str) -> Reply {
    list_in_region(emu, REGION, query).await
}

async fn list_in_region(emu: &Emulator, region: &str, query: &str) -> Reply {
    let path = if query.is_empty() {
        PATH.to_string()
    } else {
        format!("{PATH}?{query}")
    };
    emu.call_in_region(region, "GET", &path, None).await
}

/// The summaries a listing reported, in the order it reported them.
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

/// The `modelId`s a listing reported, in the order it reported them.
fn listed_ids(case: &str, reply: &Reply) -> Vec<String> {
    summaries(case, reply)
        .iter()
        .map(|row| {
            row.get("modelId")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: a summary must carry modelId: {row:?}"))
                .to_string()
        })
        .collect()
}

/// The catalog rows a predicate admits, as their `modelId`s in catalog order.
fn expected_ids(admits: impl Fn(&FoundationModel) -> bool) -> Vec<String> {
    let mut ids: Vec<String> = FOUNDATION_MODELS
        .iter()
        .filter(|model| admits(model))
        .map(|model| model.model_id.to_string())
        .collect();
    ids.sort();
    ids
}

/// Whether `values` carries `wanted`.
fn carries(values: &[&str], wanted: &str) -> bool {
    values.contains(&wanted)
}

// ---------------------------------------------------------------------------------------
// 1 and 3. The listing is the catalog, with every member read off its row.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn an_unfiltered_listing_reports_every_catalog_row_in_model_id_order() {
    let emu = emulator();
    let reply = list(&emu, "").await;
    let rows = summaries("unfiltered", &reply);

    assert_eq!(
        rows.len(),
        FOUNDATION_MODELS.len(),
        "case \"unfiltered\": expected one summary per catalog row ({}), got {} ({:?})",
        FOUNDATION_MODELS.len(),
        rows.len(),
        listed_ids("unfiltered", &reply)
    );

    let actual = listed_ids("unfiltered", &reply);
    let mut sorted = actual.clone();
    sorted.sort();
    assert_eq!(
        actual, sorted,
        "case \"unfiltered\": the listing must be in modelId order, got {actual:?}"
    );
    assert_eq!(
        actual,
        expected_ids(|_| true),
        "case \"unfiltered\": the listed ids must be the catalog's"
    );

    for model in FOUNDATION_MODELS {
        let case = model.model_id;
        let row = rows
            .iter()
            .find(|row| row.get("modelId").and_then(Value::as_str) == Some(model.model_id))
            .unwrap_or_else(|| panic!("case {case:?}: the listing must carry this catalog row"));

        let mut lifecycle = json!({
            "status": model.lifecycle_status,
            // Every offered model has launched, so a summary reports its launch instant.
            "startOfLifeTime": model.start_of_life_time,
        });
        // Both ends of the legacy window AWS publishes, for a deprecated model and for no
        // other; `publicExtendedAccessTime` is published for none.
        if let Some(window) = model.legacy_window {
            lifecycle["legacyTime"] = json!(window.entered_at);
            lifecycle["endOfLifeTime"] = json!(window.end_of_life_at);
        }
        // The projection a read of this one model reports too
        // (`crate::support::expected_foundation_model_projection`): a listing and a read
        // cannot disagree about a model.
        let expected = expected_foundation_model_projection(model, REGION);
        assert_eq!(
            row.get("modelLifecycle"),
            Some(&lifecycle),
            "case {case:?}: expected the lifecycle {lifecycle:?}, got {row:?}"
        );
        assert_eq!(
            row, &expected,
            "case {case:?}: expected the summary {expected:?}, got {row:?}"
        );
    }
}

#[tokio::test]
async fn a_model_arn_carries_the_region_the_request_addresses_and_no_account() {
    // The rows differ along the region the request signs for: the suite's own, another
    // partition-mate, and one in a third geography.
    let regions = ["us-west-2", "eu-central-1", "ap-southeast-2"];

    let emu = emulator();
    for region in regions {
        let reply = list_in_region(&emu, region, "").await;
        let rows = summaries(region, &reply);
        assert_eq!(
            listed_ids(region, &reply),
            expected_ids(|_| true),
            "case {region:?}: the catalog must not vary by region"
        );
        for row in &rows {
            let arn = row
                .get("modelArn")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {region:?}: a summary must carry modelArn: {row:?}"));
            let model_id = row.get("modelId").and_then(Value::as_str).unwrap_or_default();
            let expected = format!("arn:aws:bedrock:{region}::foundation-model/{model_id}");
            assert_eq!(
                arn, expected,
                "case {region:?}: expected the ARN {expected:?}, got {arn:?}"
            );
            // The account field of a `foundation-model` ARN is empty: the model is
            // published by AWS rather than owned by the account reading it.
            let account = arn.split(':').nth(4).unwrap_or("missing");
            assert_eq!(
                account, "",
                "case {region:?}: a foundation-model ARN carries no account, got {account:?} in \
                 {arn:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// 2. Each filter narrows on one attribute, and filters combine by conjunction.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn every_filter_reports_exactly_the_catalog_rows_it_admits() {
    /// One filtered listing: the query it sends, and the catalog rows it must report.
    struct Row {
        case: &'static str,
        query: &'static str,
        admits: Box<dyn Fn(&FoundationModel) -> bool>,
    }

    fn row(
        case: &'static str,
        query: &'static str,
        admits: impl Fn(&FoundationModel) -> bool + 'static,
    ) -> Row {
        Row {
            case,
            query,
            admits: Box::new(admits),
        }
    }

    // The rows span each filterable member, every value of each of them, both spellings
    // `byProvider` takes (the branded name and the lowercase provider segment of the
    // model id), a provider whose branded name carries a space, conjunctions of two and
    // three filters, and filters that admit nothing.
    let rows: Vec<Row> = vec![
        row("no filter at all", "", |_| true),
        row("byProvider as the branded name", "byProvider=Amazon", |m| {
            m.provider_name == "Amazon"
        }),
        row(
            "byProvider lowercased, as the CLI's own examples pass it",
            "byProvider=anthropic",
            |m| m.provider_name == "Anthropic",
        ),
        row(
            "byProvider as the model id's provider segment",
            "byProvider=mistral",
            |m| m.provider_name == "Mistral AI",
        ),
        row(
            "byProvider as a branded name carrying a space",
            "byProvider=Mistral%20AI",
            |m| m.provider_name == "Mistral AI",
        ),
        row("byProvider naming a provider of one model", "byProvider=stability", |m| {
            m.provider_name == "Stability AI"
        }),
        row("byProvider naming nobody", "byProvider=Nonesuch", |_| false),
        row("byOutputModality=TEXT", "byOutputModality=TEXT", |m| {
            carries(m.output_modalities, "TEXT")
        }),
        row("byOutputModality=IMAGE", "byOutputModality=IMAGE", |m| {
            carries(m.output_modalities, "IMAGE")
        }),
        row("byOutputModality=EMBEDDING", "byOutputModality=EMBEDDING", |m| {
            carries(m.output_modalities, "EMBEDDING")
        }),
        row(
            "byCustomizationType=FINE_TUNING",
            "byCustomizationType=FINE_TUNING",
            |m| carries(m.customizations_supported, "FINE_TUNING"),
        ),
        row(
            "byCustomizationType=CONTINUED_PRE_TRAINING",
            "byCustomizationType=CONTINUED_PRE_TRAINING",
            |m| carries(m.customizations_supported, "CONTINUED_PRE_TRAINING"),
        ),
        row(
            "byCustomizationType=DISTILLATION",
            "byCustomizationType=DISTILLATION",
            |m| carries(m.customizations_supported, "DISTILLATION"),
        ),
        row("byInferenceType=ON_DEMAND", "byInferenceType=ON_DEMAND", |m| {
            carries(m.inference_types_supported, "ON_DEMAND")
        }),
        row("byInferenceType=PROVISIONED", "byInferenceType=PROVISIONED", |m| {
            carries(m.inference_types_supported, "PROVISIONED")
        }),
        row(
            "two filters conjoined",
            "byProvider=Amazon&byOutputModality=IMAGE",
            |m| m.provider_name == "Amazon" && carries(m.output_modalities, "IMAGE"),
        ),
        row(
            "three filters conjoined",
            "byOutputModality=TEXT&byCustomizationType=FINE_TUNING&byInferenceType=PROVISIONED",
            |m| {
                carries(m.output_modalities, "TEXT")
                    && carries(m.customizations_supported, "FINE_TUNING")
                    && carries(m.inference_types_supported, "PROVISIONED")
            },
        ),
        row(
            "a conjunction no model satisfies",
            "byProvider=Cohere&byOutputModality=IMAGE",
            |m| m.provider_name == "Cohere" && carries(m.output_modalities, "IMAGE"),
        ),
    ];

    let emu = emulator();
    for Row {
        case,
        query,
        admits,
    } in rows
    {
        let reply = list(&emu, query).await;
        let actual = listed_ids(case, &reply);
        let expected = expected_ids(&admits);
        assert_eq!(
            actual, expected,
            "case {case:?} ({query:?}): expected the models {expected:?}, got {actual:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 4. A value no filter member admits is refused, never listed around.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_filter_value_the_model_does_not_admit_is_refused() {
    // (case, query) — a value outside each enum's value set, the right value in the wrong
    // case, and a `byProvider` outside `Provider`'s `^[A-Za-z0-9- ]{1,63}$`.
    let rows: &[(&str, &str)] = &[
        ("an output modality outside ModelModality", "byOutputModality=AUDIO"),
        (
            "an output modality in the wrong case",
            "byOutputModality=text",
        ),
        (
            "a customization outside ModelCustomization",
            "byCustomizationType=PROMPT_TUNING",
        ),
        (
            "a customization in the wrong case",
            "byCustomizationType=fine_tuning",
        ),
        (
            "an inference type outside InferenceType",
            "byInferenceType=ONDEMAND",
        ),
        (
            "a provider carrying a character the pattern excludes",
            "byProvider=Anthropic.",
        ),
        (
            "a provider longer than the pattern admits",
            "byProvider=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ),
    ];

    let emu = emulator();
    for (case, query) in rows {
        let reply = list(&emu, query).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?} ({query:?}): expected 400, got {} {:?}",
            reply.status, reply.body
        );
        let code = reply.error.as_deref().map(error_code).unwrap_or("none");
        assert_eq!(
            code, "ValidationException",
            "case {case:?} ({query:?}): expected ValidationException, got {code:?} {:?}",
            reply.body
        );
    }
}

// ---------------------------------------------------------------------------------------
// 5. Listing is a pure read.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn listing_changes_nothing_a_later_caller_sees() {
    let emu = emulator();
    // A resource of this service beside the catalog, so a listing that wrote anything
    // would be visible in more than the listing itself.
    let guardrail_id = emu.create_named("catalog-neighbour").await;

    for query in ["", "byProvider=Amazon", "byOutputModality=TEXT"] {
        let first = list(&emu, query).await;
        let second = list(&emu, query).await;
        assert_eq!(
            first.body, second.body,
            "case {query:?}: two identical listings must answer identically"
        );
    }

    let guardrail = emu.get(&guardrail_id, None).await;
    assert_eq!(
        guardrail.status, 200,
        "case \"neighbour\": the guardrail beside the catalog must still read back, got {} {:?}",
        guardrail.status, guardrail.body
    );
}

// ---------------------------------------------------------------------------------------
// 6. Every catalog row is modelled data, and the catalog spans every filterable value.
// ---------------------------------------------------------------------------------------

/// Whether `value` is a modality value a catalog row may publish: one `ModelModality`
/// declares (`MODEL_MODALITY_VALUES`), or the SPOKEN turn a speech-to-speech model is
/// published with.
///
/// The spoken modality is RECORDED behaviour, not modelled: this model copy declares TEXT,
/// IMAGE and EMBEDDING and no value for a spoken turn
/// (`smithy_query(query="shape",shape="ModelModality")`), while the catalog publishes one
/// for a model whose conversation is carried by a stream in both directions — the same split
/// as the capability members a summary reports, which the shape declares none of.
fn carries_modality(value: &str) -> bool {
    carries(MODEL_MODALITY_VALUES, value) || value == "SPEECH"
}

#[test]
fn every_catalog_row_carries_only_values_the_model_declares() {
    let mut seen_ids: Vec<&str> = Vec::new();
    for model in FOUNDATION_MODELS {
        let case = model.model_id;
        assert!(
            !seen_ids.contains(&model.model_id),
            "case {case:?}: a modelId appears twice in the catalog"
        );
        seen_ids.push(model.model_id);

        assert!(
            (1..=MODEL_ID_MAX_LENGTH).contains(&model.model_id.len())
                && model.model_id.contains('.')
                && model
                    .model_id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-.:".contains(&b)),
            "case {case:?}: a modelId must satisfy BedrockModelId, got {:?}",
            model.model_id
        );
        for (member, name) in [
            ("modelName", model.model_name),
            ("providerName", model.provider_name),
        ] {
            assert!(
                BRANDED_NAME_LENGTH.contains(&name.len()),
                "case {case:?}: {member} must satisfy BrandedName's length {BRANDED_NAME_LENGTH:?}, \
                 got {name:?} ({} characters)",
                name.len()
            );
        }
        // `byProvider` must be able to NAME the provider, so the branded name stays inside
        // `Provider`'s own pattern.
        assert!(
            model
                .provider_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b' '),
            "case {case:?}: providerName must satisfy Provider's pattern, got {:?}",
            model.provider_name
        );

        assert!(
            !model.input_modalities.is_empty()
                && model
                    .input_modalities
                    .iter()
                    .all(|value| carries_modality(value)),
            "case {case:?}: inputModalities must be non-empty published modality values, got {:?}",
            model.input_modalities
        );
        assert!(
            !model.output_modalities.is_empty()
                && model
                    .output_modalities
                    .iter()
                    .all(|value| carries_modality(value)),
            "case {case:?}: outputModalities must be non-empty published modality values, got \
             {:?}",
            model.output_modalities
        );
        assert!(
            model
                .customizations_supported
                .iter()
                .all(|value| carries(MODEL_CUSTOMIZATION_VALUES, value)),
            "case {case:?}: customizationsSupported must be ModelCustomization values, got {:?}",
            model.customizations_supported
        );
        assert!(
            !model.inference_types_supported.is_empty()
                && model
                    .inference_types_supported
                    .iter()
                    .all(|value| carries(INFERENCE_TYPE_VALUES, value)),
            "case {case:?}: inferenceTypesSupported must be non-empty InferenceType values, got \
             {:?}",
            model.inference_types_supported
        );
        assert!(
            carries(FOUNDATION_MODEL_LIFECYCLE_STATUS_VALUES, model.lifecycle_status),
            "case {case:?}: modelLifecycle.status must be a FoundationModelLifecycleStatus value, \
             got {:?}",
            model.lifecycle_status
        );
    }
}

#[test]
fn the_catalog_spans_every_value_a_caller_can_filter_on() {
    /// Whether a catalog row carries one value of the member under test.
    type Carried = Box<dyn Fn(&FoundationModel, &str) -> bool>;
    /// One member a caller can filter on: its name, the values the model declares for it,
    /// and how a row carries one.
    type Check = (&'static str, &'static [&'static str], Carried);

    let checks: Vec<Check> = vec![
        (
            "byOutputModality",
            MODEL_MODALITY_VALUES,
            Box::new(|model: &FoundationModel, value: &str| carries(model.output_modalities, value)),
        ),
        (
            "byCustomizationType",
            MODEL_CUSTOMIZATION_VALUES,
            Box::new(|model: &FoundationModel, value: &str| {
                carries(model.customizations_supported, value)
            }),
        ),
        (
            "byInferenceType",
            INFERENCE_TYPE_VALUES,
            Box::new(|model: &FoundationModel, value: &str| {
                carries(model.inference_types_supported, value)
            }),
        ),
        (
            "modelLifecycle.status",
            FOUNDATION_MODEL_LIFECYCLE_STATUS_VALUES,
            Box::new(|model: &FoundationModel, value: &str| model.lifecycle_status == value),
        ),
    ];

    for (member, values, carried) in checks {
        for value in values {
            let count = FOUNDATION_MODELS
                .iter()
                .filter(|model| carried(model, value))
                .count();
            // A value AWS publishes for no model it still offers is carried by no row: the
            // models continued pre-training was offered for are past their end of life, so
            // the catalog offers none, and a filter on it narrows the listing to no model
            // rather than to a row the catalog invents.
            let offered_for_no_model = (member, *value) == ("byCustomizationType", CONTINUED_PRE_TRAINING);
            assert_eq!(
                count == 0,
                offered_for_no_model,
                "case {member:?}={value:?}: expected the rows carrying this value to be \
                 {}, got {count}",
                if offered_for_no_model { "none" } else { "some" }
            );
        }
    }
}

/// `ModelCustomization` — the customization AWS offered for the Amazon Titan text models,
/// which are past their end of life, so the catalog offers no model for it.
const CONTINUED_PRE_TRAINING: &str = "CONTINUED_PRE_TRAINING";

// ---------------------------------------------------------------------------------------
// 7. The training-qualified id of a model, and the deprecated model that has none.
// ---------------------------------------------------------------------------------------

/// `FoundationModelLifecycleStatus` — the status of a deprecated model.
const LEGACY: &str = "LEGACY";

/// The member of a summary this section reads a row's lifecycle out of.
fn listed_status(case: &str, summary: &Value) -> String {
    summary
        .get("modelLifecycle")
        .and_then(|lifecycle| lifecycle.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a summary must carry modelLifecycle.status, got {summary:?}")
        })
        .to_string()
}

/// The `customizationsSupported` a summary reports.
fn listed_customizations(case: &str, summary: &Value) -> Vec<String> {
    summary
        .get("customizationsSupported")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?}: a summary must carry customizationsSupported, got {summary:?}")
        })
        .iter()
        .map(|value| value.as_str().unwrap_or_default().to_string())
        .collect()
}

/// Every id that is a proper `:`-separated PREFIX of `model_id`, longest first: the ids a
/// training-qualified id is stripped back to, at a `:` boundary only, the way a base model
/// id embedded in a resource is resolved (`CustomModelArn` admits
/// `([:][a-z0-9-]{1,63}){0,2}` after the model id,
/// `smithy_query(query="operation",operation="CreateCustomModelDeployment")`).
fn qualifier_prefixes(model_id: &str) -> Vec<&str> {
    let mut prefixes = Vec::new();
    let mut candidate = model_id;
    while let Some((shorter, _)) = candidate.rsplit_once(':') {
        prefixes.push(shorter);
        candidate = shorter;
    }
    prefixes
}

#[tokio::test]
async fn a_deprecated_model_is_published_under_its_base_id_alone() {
    let emu = emulator();
    let case = "the whole catalog";
    let listed = summaries(case, &list(&emu, "").await);
    let rows: Vec<(String, String, Vec<String>)> = listed
        .iter()
        .map(|summary| {
            let id = summary
                .get("modelId")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: a summary must carry modelId"))
                .to_string();
            (
                id.clone(),
                listed_status(&id, summary),
                listed_customizations(&id, summary),
            )
        })
        .collect();
    assert!(
        rows.iter().any(|(_, status, _)| status == LEGACY),
        "setup: the catalog must publish a deprecated model for this rule to bite, got {} \
         rows",
        rows.len()
    );

    // A deprecated model takes no new customization job, so no row of one publishes a
    // customization …
    for (id, status, customizations) in &rows {
        if status != LEGACY {
            continue;
        }
        assert!(
            customizations.is_empty(),
            "case {id:?}: a deprecated model takes no new customization job, so it publishes \
             no customization, got {customizations:?}"
        );
    }
    // … and no row is the training-qualified variant of one.
    for (id, _, _) in &rows {
        for prefix in qualifier_prefixes(id) {
            let base = rows
                .iter()
                .find(|(listed_id, _, _)| listed_id == prefix)
                .map(|(_, status, _)| status.as_str());
            assert_ne!(
                base,
                Some(LEGACY),
                "case {id:?}: {prefix:?} is deprecated, so no training-qualified row is \
                 published for it, yet {id:?} is listed beside it"
            );
        }
    }
}

/// One `:`-qualified identifier a client may address a published model by: the catalog row
/// it names, and whether the catalog publishes a row of its OWN under it — which the
/// model's lifecycle decides.
struct Variant {
    identifier: &'static str,
    base: &'static str,
    own_row: bool,
}

/// The identifiers rule 7 splits: the training-qualified ids of the catalog's deprecated
/// models, published under no row of their own, beside the qualified ids of models that are
/// still available, which ARE rows of their own — one published with a single qualifier, one
/// with two, and from another provider.
const VARIANTS: &[Variant] = &[
    Variant {
        identifier: "cohere.command-text-v14:7:4k",
        base: "cohere.command-text-v14",
        own_row: false,
    },
    Variant {
        identifier: "cohere.command-light-text-v14:7:4k",
        base: "cohere.command-light-text-v14",
        own_row: false,
    },
    Variant {
        identifier: "amazon.titan-embed-image-v1:0",
        base: "amazon.titan-embed-image-v1:0",
        own_row: true,
    },
    Variant {
        identifier: "amazon.titan-embed-text-v1:2:8k",
        base: "amazon.titan-embed-text-v1:2:8k",
        own_row: true,
    },
];

#[tokio::test]
async fn a_training_qualified_identifier_reads_the_row_it_names() {
    let emu = emulator();
    let listed = listed_ids("the whole catalog", &list(&emu, "").await);
    for row in VARIANTS {
        let case = row.identifier;
        let carried = listed.iter().any(|id| id == case);
        assert_eq!(
            carried, row.own_row,
            "case {case:?}: the catalog publishes a row of its own under this id: {}, got \
             {carried}",
            row.own_row
        );
        assert!(
            listed.iter().any(|id| id == row.base),
            "case {case:?}: the model's base id {:?} must be listed, got {} rows without it",
            row.base,
            listed.len()
        );

        let reply = emu
            .call("GET", &format!("{PATH}/{}", label(case)), None)
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: a qualified id names a published model, so a read of it must \
             answer 200, got {} {:?}",
            reply.status, reply.body
        );
        let published = FOUNDATION_MODELS
            .iter()
            .find(|model| model.model_id == row.base)
            .unwrap_or_else(|| panic!("case {case:?}: the catalog must carry {:?}", row.base));
        assert_eq!(
            reply.get("modelDetails"),
            expected_foundation_model_projection(published, REGION),
            "case {case:?}: the read must report the row the id resolves to, {:?}",
            row.base
        );
    }
}

#[tokio::test]
async fn a_customization_filter_reports_no_deprecated_model() {
    let emu = emulator();
    for customization in MODEL_CUSTOMIZATION_VALUES {
        let case = format!("byCustomizationType={customization}");
        for summary in summaries(&case, &list(&emu, &case).await) {
            let id = summary
                .get("modelId")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?}: a summary must carry modelId"))
                .to_string();
            assert!(
                listed_customizations(&case, &summary)
                    .iter()
                    .any(|value| value == customization),
                "case {case:?}: {id:?} is listed, so it must publish {customization:?}"
            );
            assert_ne!(
                listed_status(&case, &summary),
                LEGACY,
                "case {case:?}: {id:?} is deprecated, so no customization is offered for it"
            );
        }
    }

    // The one customization the catalog publishes variant rows for must still narrow the
    // listing to something, or the rule above would hold vacuously.
    let case = "byCustomizationType=FINE_TUNING";
    assert!(
        !summaries(case, &list(&emu, case).await).is_empty(),
        "case {case:?}: the catalog must publish a model offered for FINE_TUNING"
    );
}
