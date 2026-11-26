// SPDX-License-Identifier: CC0-1.0

//! What a `SYSTEM_DEFINED` cross-region inference profile reports ABOUT ITSELF — the name
//! and description it is published under, and the two instants of its record — read end to
//! end over the same `MultiServiceRouter` the emulator boots.
//!
//! # The rules under test
//!
//! 1. **A published profile names its base model the way its publisher publishes it**: its
//!    `inferenceProfileName` is the geography's label, then the provider's name for a model
//!    published by a third party (nothing for a model AWS publishes itself), then the
//!    model's own name — reported WHOLE, qualifier included, such as an instruction-tuned
//!    variant's `Instruct`. The catalogue's `modelName` is a `BrandedName` (@length 1..20)
//!    and so carries a long name shortened to fit, while `InferenceProfileName` is @length
//!    1..64: a profile therefore names its model at least as fully as the catalogue does,
//!    never more briefly, because the name is what a client looks a profile up and labels
//!    it by. Its `description` names the model by that same name.
//! 2. **A published profile reports its own two instants**: `createdAt` is the day THAT
//!    profile was published and `updatedAt` the instant its record last changed, so the two
//!    are independent members read off the profile's own published row rather than one value
//!    reported twice, and one constant is not reported for every profile of the catalogue.
//!    `updatedAt` is never earlier than `createdAt`.
//! 3. **A listing and a read agree**, and reading a profile moves neither instant: the
//!    catalogue is published data, so nothing a reader does changes what a later reader
//!    sees.
//!
//! # What the tables below generalise over
//!
//! The rows span the three geographies the catalogue publishes (of three different sizes)
//! and a region of none; models published by AWS itself and by two third parties; models
//! still offered and models offered no longer (whose profiles AWS goes on publishing);
//! profiles published with the capability, with their model, and later than both; and a
//! profile AWS has revised since publishing it beside ones it has not.

use std::collections::BTreeSet;

use serde_json::Value;
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;

use crate::support::{date_time_seconds, emulator, label, profile_model_id, Emulator, Reply, REGION};

/// `InferenceProfileType`'s value for a profile Amazon Bedrock defines
/// (`smithy_query(query="shape",shape="InferenceProfileType")`).
const SYSTEM_DEFINED: &str = "SYSTEM_DEFINED";

/// `InferenceProfileName` is `@length(min: 1, max: 64)`
/// (`smithy_query(query="shape",shape="InferenceProfileName")`), while the catalogue's
/// `modelName` is a `BrandedName` of at most twenty characters
/// (`smithy_query(query="shape",shape="BrandedName")`) — which is why a profile's name may
/// be longer than the catalogue's name for its model, and never shorter.
const MAX_PROFILE_NAME: usize = 64;

/// The provider AWS publishes its own models under — the brand a profile over one of them
/// carries is the model's name alone, because AWS does not brand its own models by its
/// provider name.
const AWS_PROVIDER: &str = "Amazon";

/// `(case, region)` rows: one region of each geography the catalogue publishes, a second
/// region of one of them, and a region belonging to none.
const REGIONS: &[(&str, &str)] = &[
    ("a region of the us geography", REGION),
    ("another region of the us geography", "us-east-1"),
    ("a region of the eu geography", "eu-central-1"),
    ("a region of the apac geography", "ap-northeast-1"),
    ("a region of no geography", "sa-east-1"),
];

/// The published name and description of one profile of the US geography, as the recorded
/// `ListInferenceProfiles` body carries them.
///
/// The rows are the Meta Llama profiles of that listing — the generation the recording
/// shows naming its model with the `Instruct` qualifier the model id carries, including the
/// two sizes and the generation the issue did not cite — beside a profile over a model AWS
/// publishes itself, whose brand carries no provider name at all.
struct PublishedName {
    model_id: &'static str,
    name: &'static str,
    description: &'static str,
}

const PUBLISHED_US_NAMES: &[PublishedName] = &[
    PublishedName {
        model_id: "amazon.nova-lite-v1:0",
        name: "US Nova Lite",
        description: "Routes requests to Nova Lite in us-east-1, us-east-2 and us-west-2.",
    },
    PublishedName {
        model_id: "meta.llama3-1-8b-instruct-v1:0",
        name: "US Meta Llama 3.1 8B Instruct",
        description: "Routes requests to Meta Llama 3.1 8B Instruct in us-east-1, us-east-2 \
                      and us-west-2.",
    },
    PublishedName {
        model_id: "meta.llama3-1-70b-instruct-v1:0",
        name: "US Meta Llama 3.1 70B Instruct",
        description: "Routes requests to Meta Llama 3.1 70B Instruct in us-east-1, us-east-2 \
                      and us-west-2.",
    },
    PublishedName {
        model_id: "meta.llama3-3-70b-instruct-v1:0",
        name: "US Meta Llama 3.3 70B Instruct",
        description: "Routes requests to Meta Llama 3.3 70B Instruct in us-east-1, us-east-2 \
                      and us-west-2.",
    },
    PublishedName {
        model_id: "meta.llama3-2-90b-instruct-v1:0",
        name: "US Meta Llama 3.2 90B Instruct",
        description: "Routes requests to Meta Llama 3.2 90B Instruct in us-east-1 and \
                      us-west-2.",
    },
];

/// The two instants one profile of the US geography was published and last revised at, as
/// the recorded `ListInferenceProfiles` body carries them: `(modelId, createdAt, whether
/// AWS has revised the profile since publishing it)`.
///
/// The rows span the three classes of publication day the catalogue carries — a profile
/// published the day cross-region profiles began being published, one published with its
/// own model, and one published later than either — and both the revised and the
/// never-revised case.
struct PublishedInstants {
    model_id: &'static str,
    created_at: &'static str,
    revised: bool,
}

const PUBLISHED_US_INSTANTS: &[PublishedInstants] = &[
    PublishedInstants {
        model_id: "anthropic.claude-3-haiku-20240307-v1:0",
        created_at: "2024-08-26T00:00:00Z",
        revised: false,
    },
    PublishedInstants {
        model_id: "amazon.nova-pro-v1:0",
        created_at: "2024-12-03T00:00:00Z",
        revised: false,
    },
    PublishedInstants {
        model_id: "meta.llama3-1-8b-instruct-v1:0",
        created_at: "2024-11-01T00:00:00Z",
        revised: true,
    },
];

async fn list_published(emu: &Emulator, region: &str) -> Reply {
    // `typeEquals` is bound to the `type` query parameter
    // (`smithy_query(query="operation",operation="ListInferenceProfiles")`).
    emu.call_in_region(
        region,
        "GET",
        &format!("/inference-profiles?type={SYSTEM_DEFINED}"),
        None,
    )
    .await
}

async fn get_published(emu: &Emulator, region: &str, identifier: &str) -> Reply {
    emu.call_in_region(
        region,
        "GET",
        &format!("/inference-profiles/{}", label(identifier)),
        None,
    )
    .await
}

async fn get_model(emu: &Emulator, region: &str, model_id: &str) -> Reply {
    emu.call_in_region(
        region,
        "GET",
        &format!("/foundation-models/{}", label(model_id)),
        None,
    )
    .await
}

fn summaries(case: &str, listing: &Reply) -> Vec<Value> {
    assert_eq!(
        listing.status, 200,
        "case {case:?}: ListInferenceProfiles must answer 200, got {} {:?}",
        listing.status, listing.body
    );
    listing
        .get("inferenceProfileSummaries")
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: inferenceProfileSummaries must be a present array, got {:?}",
                listing.body
            )
        })
}

fn text(row: &Value, member: &str) -> String {
    row.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Whether the catalogue still OFFERS `model_id`, as against publishing a profile over it
/// once it is offered no longer.
fn is_offered(model_id: &str) -> bool {
    bedrock::state::FOUNDATION_MODELS
        .iter()
        .any(|model| model.model_id == model_id)
}

/// The profile of `model_id` in the listing, or a panic naming what the listing did carry.
fn profile_of<'rows>(case: &str, rows: &'rows [Value], model_id: &str) -> &'rows Value {
    let profile_id = format!("us.{model_id}");
    rows.iter()
        .find(|row| text(row, "inferenceProfileId") == profile_id)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the published listing must carry {profile_id:?}, got {:?}",
                rows.iter()
                    .map(|row| text(row, "inferenceProfileId"))
                    .collect::<Vec<_>>()
            )
        })
}

// ---------------------------------------------------------------------------------------
// 1. A published profile names its model the way its publisher publishes it.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_published_profile_names_its_model_the_way_its_publisher_publishes_it() {
    let emu = emulator();

    for (case, region) in REGIONS {
        let listing = list_published(&emu, region).await;
        for summary in summaries(case, &listing) {
            let profile_id = text(&summary, "inferenceProfileId");
            let case = format!("{case} / {profile_id}");
            let model_id = profile_model_id(&text(&summary, "inferenceProfileArn"));
            if !is_offered(&model_id) {
                // A profile outlives the offering of its model, and the catalogue reports
                // nothing about a model it offers no longer, so there is no published name
                // to compare this row against.
                continue;
            }
            let read = get_model(&emu, region, &model_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: GetFoundationModel must report the model behind the profile, \
                 got {} {:?}",
                read.status, read.body
            );
            let details = read.get("modelDetails");
            let model_name = text(&details, "modelName");
            let provider_name = text(&details, "providerName");
            assert!(
                !model_name.is_empty() && !provider_name.is_empty(),
                "case {case:?}: the catalogue must publish a name and a provider for this \
                 model, got {details:?}"
            );
            let brand = if provider_name == AWS_PROVIDER {
                model_name.clone()
            } else {
                format!("{provider_name} {model_name}")
            };
            // The geography's label, as the published id spells it ahead of the model id.
            let geography = profile_id
                .split_once('.')
                .map(|(prefix, _)| prefix.to_uppercase())
                .unwrap_or_default();
            let name = text(&summary, "inferenceProfileName");
            // The profile names the model by the whole of its published name, of which the
            // catalogue's `BrandedName` is the leading part when the two differ.
            assert!(
                name.starts_with(&format!("{geography} {brand}")),
                "case {case:?}: the published name is the geography's label and the name \
                 {model_id:?} is published under — at least as fully as the catalogue's \
                 {brand:?} — got {name:?}"
            );
            assert!(
                !name.is_empty() && name.len() <= MAX_PROFILE_NAME,
                "case {case:?}: a published name is inside InferenceProfileName's length \
                 1..{MAX_PROFILE_NAME}, got {name:?}"
            );
            let label_in_name = name
                .strip_prefix(&format!("{geography} "))
                .unwrap_or_default()
                .to_string();
            let description = text(&summary, "description");
            assert!(
                description.starts_with(&format!("Routes requests to {label_in_name} in ")),
                "case {case:?}: the published description names the model by the same name \
                 the profile is named after ({label_in_name:?}), got {description:?}"
            );
        }
    }

    // And the rows the recording pins, spelled out: the name and description of each are
    // what the recorded US listing carries for that profile.
    let listing = list_published(&emu, REGION).await;
    let rows = summaries("the us geography", &listing);
    for published in PUBLISHED_US_NAMES {
        let case = format!("the us geography / {}", published.model_id);
        let summary = profile_of(&case, &rows, published.model_id);
        assert_eq!(
            text(summary, "inferenceProfileName"),
            published.name,
            "case {case:?}: expected the published name {:?}, got {summary:?}",
            published.name
        );
        assert_eq!(
            text(summary, "description"),
            published.description,
            "case {case:?}: expected the published description {:?}, got {summary:?}",
            published.description
        );
    }
}

// ---------------------------------------------------------------------------------------
// 2. A published profile reports its own publication and revision instants.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_published_profile_reports_its_own_two_instants() {
    let emu = emulator();

    for (case, region) in REGIONS {
        let listing = list_published(&emu, region).await;
        let rows = summaries(case, &listing);
        let mut publications: BTreeSet<String> = BTreeSet::new();
        let mut revised = 0_usize;
        for summary in &rows {
            let profile_id = text(summary, "inferenceProfileId");
            let case = format!("{case} / {profile_id}");
            let created = text(summary, "createdAt");
            let updated = text(summary, "updatedAt");
            // Both are modelled `@timestampFormat("date-time")`, so each is an RFC 3339
            // string on the wire and the instants it denotes are what may be compared.
            let created_at = date_time_seconds(&case, &created);
            let updated_at = date_time_seconds(&case, &updated);
            assert!(
                updated_at >= created_at,
                "case {case:?}: a profile's record cannot have changed before the profile \
                 was published, got created {created:?} and updated {updated:?}"
            );
            publications.insert(created);
            if updated_at > created_at {
                revised += 1;
            }
        }
        if rows.len() < 2 {
            // A region of no geography publishes nothing, and nothing is claimed about one
            // profile's instants relative to another's.
            continue;
        }
        assert!(
            publications.len() > 1,
            "case {case:?}: each profile carries the day IT was published, so one instant is \
             not reported for every profile of the region, got {publications:?}"
        );
        assert!(
            revised > 0,
            "case {case:?}: a profile AWS has revised since publishing it reports a later \
             updatedAt than its createdAt, got {} profile(s) reporting one instant twice",
            rows.len()
        );
    }

    // And the rows the recording pins, spelled out.
    let listing = list_published(&emu, REGION).await;
    let rows = summaries("the us geography", &listing);
    for published in PUBLISHED_US_INSTANTS {
        let case = format!("the us geography / {}", published.model_id);
        let summary = profile_of(&case, &rows, published.model_id);
        let created = text(summary, "createdAt");
        let updated = text(summary, "updatedAt");
        assert_eq!(
            date_time_seconds(&case, &created),
            date_time_seconds(&case, published.created_at),
            "case {case:?}: expected the publication instant {:?}, got {created:?}",
            published.created_at
        );
        let later = date_time_seconds(&case, &updated) > date_time_seconds(&case, &created);
        assert_eq!(
            later, published.revised,
            "case {case:?}: a revised profile reports a later updatedAt and one AWS has not \
             revised reports its publication instant, got created {created:?} and updated \
             {updated:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3. A listing and a read agree, and reading changes neither instant.
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_read_reports_the_same_metadata_as_the_listing_and_changes_none_of_it() {
    let emu = emulator();
    let members = [
        "inferenceProfileName",
        "description",
        "createdAt",
        "updatedAt",
    ];

    for (case, region) in REGIONS {
        let listing = list_published(&emu, region).await;
        for summary in summaries(case, &listing) {
            let profile_id = text(&summary, "inferenceProfileId");
            let case = format!("{case} / {profile_id}");
            for identifier in [profile_id.clone(), text(&summary, "inferenceProfileArn")] {
                let read = get_published(&emu, region, &identifier).await;
                assert_eq!(
                    read.status, 200,
                    "case {case:?}: GetInferenceProfile must report the published profile \
                     addressed as {identifier:?}, got {} {:?}",
                    read.status, read.body
                );
                for member in members {
                    assert_eq!(
                        read.get(member),
                        summary.get(member).cloned().unwrap_or(Value::Null),
                        "case {case:?} / {member}: the read and the listing must report the \
                         same value, got {:?} and {:?}",
                        read.get(member),
                        summary.get(member)
                    );
                }
            }
        }

        // Published data: the second listing reports what the first did, member for member,
        // so neither the reads above nor the listing itself moved anything.
        let again = list_published(&emu, region).await;
        assert_eq!(
            summaries(case, &again),
            summaries(case, &listing),
            "case {case:?}: a read must not change what a later reader sees"
        );
    }
}
