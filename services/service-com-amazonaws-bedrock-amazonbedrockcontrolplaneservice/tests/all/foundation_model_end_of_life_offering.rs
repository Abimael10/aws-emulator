// SPDX-License-Identifier: CC0-1.0

//! What this control plane reports for a base model that is PAST ITS END OF LIFE, read end
//! to end over the same `MultiServiceRouter` the emulator boots.
//!
//! # The rule
//!
//! A model past its end of life is offered by nobody: the published catalog carries no row
//! for it, so `ListFoundationModels` reports it under no `modelId` and `GetFoundationModel`
//! answers 404 with the end-of-life reason — `ListFoundationModels` reports "the foundation
//! models that you can use" and `FoundationModelLifecycle`'s `endOfLifeTime` is "Time when
//! the model is no longer available for use"
//! (`smithy_query(query="operation",operation="ListFoundationModels")` and
//! `query="shape",shape="FoundationModelLifecycle"`), so a listing reports the models the
//! service offers NOW and an identifier naming one it offers no longer names no offered
//! resource.
//!
//! The routing resources Amazon Bedrock publishes OVER a model are published resources in
//! their own right, and they outlive the offering of the model they route to: the
//! `SYSTEM_DEFINED` cross-region inference profile published over a base model goes on being
//! listed, and the `default` prompt router that routes to that profile goes on reporting it
//! as a target, after the model itself stops being offered. Nothing creates either of them
//! (`InferenceProfileType`'s `SYSTEM_DEFINED` is "defined by Amazon Bedrock";
//! `PromptRouterType` is `[custom, default]`), so neither follows the offered catalog's
//! membership.
//!
//! # What the tables below generalise over
//!
//! The rows differ along what the rule spans: which model is past its end of life (every id
//! this service recognises as retired, whether or not a published row is kept for it — the
//! whole set, not the ids the issue cited), the spelling the identifier is submitted in (the
//! bare `modelId` and the model's `foundation-model` ARN), the region the request addresses,
//! the SIZE of the region set a published profile routes to (two regions and three), and
//! whether the model behind a published profile is still offered — the two classes a profile
//! can be published over.

use serde_json::Value;
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;

use crate::support::{emulator, foundation_model_arn, label, Emulator, Reply, REGION};

/// `InferenceProfileType` / `InferenceProfileStatus` for a published profile.
const SYSTEM_DEFINED: &str = "SYSTEM_DEFINED";
const PROFILE_ACTIVE: &str = "ACTIVE";

/// Every base model id this service recognises and offers no longer: the ids no published
/// resource names, and the ids a published row is kept for because a routing resource does.
fn ids_past_end_of_life() -> Vec<&'static str> {
    bedrock::state::RETIRED_FOUNDATION_MODELS
        .iter()
        .copied()
        .chain(
            bedrock::state::PAST_END_OF_LIFE_MODELS
                .iter()
                .map(|model| model.model_id),
        )
        .collect()
}

async fn list_foundation_models(emu: &Emulator, region: &str) -> Reply {
    emu.call_in_region(region, "GET", "/foundation-models", None)
        .await
}

async fn get_foundation_model(emu: &Emulator, region: &str, identifier: &str) -> Reply {
    emu.call_in_region(
        region,
        "GET",
        &format!("/foundation-models/{}", label(identifier)),
        None,
    )
    .await
}

async fn list_profiles(emu: &Emulator, region: &str) -> Reply {
    emu.call_in_region(
        region,
        "GET",
        &format!("/inference-profiles?type={SYSTEM_DEFINED}"),
        None,
    )
    .await
}

async fn get_profile(emu: &Emulator, region: &str, identifier: &str) -> Reply {
    emu.call_in_region(
        region,
        "GET",
        &format!("/inference-profiles/{}", label(identifier)),
        None,
    )
    .await
}

async fn list_routers(emu: &Emulator, region: &str) -> Reply {
    emu.call_in_region(region, "GET", "/prompt-routers?type=default", None)
        .await
}

/// The summaries a listing reported.
fn rows(reply: &Reply, member: &str) -> Vec<Value> {
    reply
        .get(member)
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("{member} must be a present array, got {:?}", reply.body))
}

/// The string member `name` of one summary row.
fn text(row: &Value, name: &str) -> String {
    row.get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `modelId`s `ListFoundationModels` reports in `region`.
async fn offered_model_ids(emu: &Emulator, region: &str) -> Vec<String> {
    let listed = list_foundation_models(emu, region).await;
    assert_eq!(
        listed.status, 200,
        "case {region:?}: ListFoundationModels must answer 200, got {} {:?}",
        listed.status, listed.body
    );
    rows(&listed, "modelSummaries")
        .iter()
        .map(|row| text(row, "modelId"))
        .collect()
}

// ---------------------------------------------------------------------------------------
// 1. A model past its end of life is offered by nobody.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn no_listing_offers_a_model_past_its_end_of_life() {
    let emu = emulator();
    // The rows are regions a request may address: the published catalog is the same for
    // every one of them, so none of them offers a retired model.
    for region in [REGION, "us-east-1", "eu-west-1"] {
        let offered = offered_model_ids(&emu, region).await;
        assert!(
            !offered.is_empty(),
            "case {region:?}: the published catalog must offer at least one model, got {offered:?}"
        );
        for model_id in ids_past_end_of_life() {
            assert!(
                !offered.contains(&model_id.to_string()),
                "case {model_id:?} in {region:?}: a model past its end of life is offered by \
                 nobody, so no modelSummaries entry may carry it, got {offered:?}"
            );
        }
        // The same listing goes on offering every row the catalog does carry, so the rule
        // narrows the catalog to what is offered rather than emptying it.
        for model in bedrock::state::FOUNDATION_MODELS {
            assert!(
                offered.contains(&model.model_id.to_string()),
                "case {:?} in {region:?}: an offered model must be listed, got {offered:?}",
                model.model_id
            );
        }
    }
}

#[tokio::test]
async fn a_read_of_a_model_past_its_end_of_life_reports_it_missing() {
    let emu = emulator();
    let expected = bedrock::state::FOUNDATION_MODEL_END_OF_LIFE_MESSAGE;
    for model_id in ids_past_end_of_life() {
        // Both spellings `GetFoundationModelIdentifier` admits name the same model, so both
        // are refused for the same reason.
        for identifier in [
            model_id.to_string(),
            foundation_model_arn(REGION, model_id),
        ] {
            let read = get_foundation_model(&emu, REGION, &identifier).await;
            assert_eq!(
                read.status, 404,
                "case {identifier:?}: a model past its end of life is no resource to read, so \
                 the read must answer 404, got {} {:?}",
                read.status, read.body
            );
            assert_eq!(
                read.get("message").as_str().unwrap_or_default(),
                expected,
                "case {identifier:?}: expected the end-of-life reason {expected:?}, got {:?}",
                read.body
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// 2. A published cross-region profile outlives the offering of its model.
// ---------------------------------------------------------------------------------------

/// One published `SYSTEM_DEFINED` profile of the US geography, as AWS publishes it: the
/// base model it routes to, the name it carries, the regions it fans a request out to and
/// whether the model behind it is still offered.
///
/// The rows are the profiles the recorded US listing carries for the two classes a profile
/// is published over — a model still offered (Amazon Nova Pro, whose profile routes to every
/// region of the geography) and a model offered no longer (Anthropic Claude 3 Haiku and
/// Claude 3.5 Sonnet v1, whose profiles route to two and three regions, and the Meta Llama
/// 3.2 vision models) — so the table spans both classes and both region-set sizes.
struct PublishedProfile {
    model_id: &'static str,
    name: &'static str,
    /// The sentence the profile describes itself with, written out rather than rebuilt here:
    /// the brand its name carries and the regions it routes to, as the published description
    /// reads them.
    description: &'static str,
    routed_regions: &'static [&'static str],
    offered: bool,
}

const PUBLISHED_US_PROFILES: &[PublishedProfile] = &[
    PublishedProfile {
        model_id: "amazon.nova-pro-v1:0",
        name: "US Nova Pro",
        description: "Routes requests to Nova Pro in us-east-1, us-east-2 and us-west-2.",
        routed_regions: &["us-east-1", "us-east-2", "us-west-2"],
        offered: true,
    },
    PublishedProfile {
        model_id: "anthropic.claude-3-haiku-20240307-v1:0",
        name: "US Anthropic Claude 3 Haiku",
        description: "Routes requests to Anthropic Claude 3 Haiku in us-east-1 and us-west-2.",
        routed_regions: &["us-east-1", "us-west-2"],
        offered: false,
    },
    PublishedProfile {
        model_id: "anthropic.claude-3-5-sonnet-20240620-v1:0",
        name: "US Anthropic Claude 3.5 Sonnet",
        description: "Routes requests to Anthropic Claude 3.5 Sonnet in us-east-1, us-east-2 \
                      and us-west-2.",
        routed_regions: &["us-east-1", "us-east-2", "us-west-2"],
        offered: false,
    },
    PublishedProfile {
        model_id: "meta.llama3-2-11b-instruct-v1:0",
        name: "US Meta Llama 3.2 11B Instruct",
        description: "Routes requests to Meta Llama 3.2 11B Instruct in us-east-1 and \
                      us-west-2.",
        routed_regions: &["us-east-1", "us-west-2"],
        offered: false,
    },
    PublishedProfile {
        model_id: "meta.llama3-2-90b-instruct-v1:0",
        name: "US Meta Llama 3.2 90B Instruct",
        description: "Routes requests to Meta Llama 3.2 90B Instruct in us-east-1 and \
                      us-west-2.",
        routed_regions: &["us-east-1", "us-west-2"],
        offered: false,
    },
];

#[tokio::test]
async fn a_published_profile_outlives_the_offering_of_its_model() {
    let emu = emulator();
    let listed = list_profiles(&emu, REGION).await;
    assert_eq!(
        listed.status, 200,
        "ListInferenceProfiles must answer 200, got {} {:?}",
        listed.status, listed.body
    );
    let summaries = rows(&listed, "inferenceProfileSummaries");
    let offered = offered_model_ids(&emu, REGION).await;

    for profile in PUBLISHED_US_PROFILES {
        let profile_id = format!("us.{}", profile.model_id);
        let row = summaries
            .iter()
            .find(|row| text(row, "inferenceProfileId") == profile_id)
            .unwrap_or_else(|| {
                panic!(
                    "case {profile_id:?}: the published US listing must carry the profile \
                     Amazon Bedrock publishes over this model, got {:?}",
                    summaries
                        .iter()
                        .map(|row| text(row, "inferenceProfileId"))
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(
            text(row, "inferenceProfileName"),
            profile.name,
            "case {profile_id:?}: expected the published name {:?}, got {row:?}",
            profile.name
        );
        assert_eq!(
            text(row, "description"),
            profile.description,
            "case {profile_id:?}: expected the published description {:?}, got {row:?}",
            profile.description
        );
        assert_eq!(
            text(row, "status"),
            PROFILE_ACTIVE,
            "case {profile_id:?}: expected status {PROFILE_ACTIVE:?}, got {row:?}"
        );
        assert_eq!(
            text(row, "type"),
            SYSTEM_DEFINED,
            "case {profile_id:?}: expected type {SYSTEM_DEFINED:?}, got {row:?}"
        );
        let expected_models: Vec<String> = profile
            .routed_regions
            .iter()
            .map(|region| foundation_model_arn(region, profile.model_id))
            .collect();
        let reported_models: Vec<String> = row
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|model| text(model, "modelArn"))
            .collect();
        assert_eq!(
            reported_models, expected_models,
            "case {profile_id:?}: a profile reports one model per region it routes to, so \
             expected {expected_models:?}, got {reported_models:?}"
        );
        // The profile is reported whether or not the model behind it is still offered, and
        // the row says which class this one is.
        assert_eq!(
            offered.contains(&profile.model_id.to_string()),
            profile.offered,
            "case {profile_id:?}: expected the model behind the profile to be offered \
             {:?}, got the opposite from ListFoundationModels",
            profile.offered
        );
        // The same profile resolves through a read of it, so a listing and a read agree.
        let read = get_profile(&emu, REGION, &profile_id).await;
        assert_eq!(
            read.status, 200,
            "case {profile_id:?}: GetInferenceProfile must resolve a published profile, got \
             {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.get("inferenceProfileName").as_str().unwrap_or_default(),
            profile.name,
            "case {profile_id:?}: a read must report what the listing reports, got {:?}",
            read.body
        );
    }
}

#[tokio::test]
async fn a_published_router_keeps_routing_to_a_profile_over_a_model_offered_no_longer() {
    let emu = emulator();
    let listed = list_routers(&emu, REGION).await;
    assert_eq!(
        listed.status, 200,
        "ListPromptRouters must answer 200, got {} {:?}",
        listed.status, listed.body
    );
    let summaries = rows(&listed, "promptRouterSummaries");
    assert_eq!(
        summaries.len(),
        bedrock::state::DEFAULT_PROMPT_ROUTERS.len(),
        "every published router must be reported, whether or not a model it routes to is \
         offered, got {:?}",
        summaries
            .iter()
            .map(|row| text(row, "promptRouterName"))
            .collect::<Vec<_>>()
    );
    let offered = offered_model_ids(&emu, REGION).await;
    let profile_ids: Vec<String> = rows(&list_profiles(&emu, REGION).await, "inferenceProfileSummaries")
        .iter()
        .map(|row| text(row, "inferenceProfileId"))
        .collect();

    // The rows are the published routers: each one's targets are published profiles, and a
    // target whose own model is offered no longer is reported like any other.
    let mut routers_over_a_retired_model = 0;
    for router in &summaries {
        let fallback = router
            .get("fallbackModel")
            .map(|model| text(model, "modelArn"))
            .unwrap_or_default();
        let targets: Vec<String> = router
            .get("models")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|model| text(model, "modelArn"))
            .collect();
        let name = text(router, "promptRouterName");
        assert!(
            targets.contains(&fallback),
            "case {name:?}: the fallback model must be one of the routed models, got \
             {fallback:?} and {targets:?}"
        );
        for target in &targets {
            let profile_id = target
                .rsplit_once("inference-profile/")
                .map(|(_, id)| id.to_string())
                .unwrap_or_default();
            assert!(
                profile_ids.contains(&profile_id),
                "case {name:?}: a router's target must be a published profile, got \
                 {target:?} against {profile_ids:?}"
            );
            let model_id = profile_id
                .split_once('.')
                .map(|(_, model_id)| model_id.to_string())
                .unwrap_or_default();
            if !offered.contains(&model_id) {
                routers_over_a_retired_model += 1;
            }
        }
    }
    assert!(
        routers_over_a_retired_model > 0,
        "the published routers must still report a target whose own model is offered no \
         longer: that is what makes a published router independent of the offered catalog, \
         got none among {:?}",
        summaries
            .iter()
            .map(|row| text(row, "promptRouterName"))
            .collect::<Vec<_>>()
    );
}
