// SPDX-License-Identifier: CC0-1.0

//! How `CreateProvisionedModelThroughput` classifies a purchase that names NO commitment
//! term, driven end to end over the same `MultiServiceRouter` the emulator boots, so what is
//! asserted is what a client sees.
//!
//! # The rule
//!
//! A Provisioned Throughput is purchasable over every base model THIS SERVICE'S OWN
//! catalogue publishes `PROVISIONED` inference for, and what `commitmentDuration` decides is
//! only WHICH granted model-unit quota the purchase draws on. The no-commitment quota is
//! granted at zero, so a well-formed purchase that omits the term over such a model is
//! classified against the account's model units and refused 400
//! `ServiceQuotaExceededException`, whose sentence names the BARE model id the request
//! resolved to and the Region the request addressed and buys nothing; the not-supported 400
//! `ValidationException` stays reserved for a model offered under no Provisioned Throughput
//! at all — one the catalogue offers on demand only, and one this service recognises and
//! offers no longer.
//!
//! A malformed purchase stays a malformed purchase: `modelUnits` is `PositiveInteger`
//! (`@range(min: 1)`), so a count below that bound is still the constraint walk's own 400
//! `ValidationException`, which is how a client tells "fix the request" from "ask for model
//! units".
//!
//! # What the tables generalise over
//!
//! The candidates are READ OUT of the emulator's own
//! `ListFoundationModels(byInferenceType=PROVISIONED)` answer rather than pinned to one
//! model id, so the catalogue this service advertises and the purchases it admits cannot
//! disagree — and every lifecycle status that listing reports is walked. The rows then
//! differ along the `modelId` SPELLING (a bare model id and a `foundation-model` ARN), the
//! Region the request is signed for and the `modelUnits` count, and the complement table
//! walks the model classes no such purchase can be made over. The expectations come from the
//! operation's own model
//! (`smithy_query(query="operation",operation="CreateProvisionedModelThroughput,ListFoundationModels,ListProvisionedModelThroughputs")`)
//! and from the published catalogue this service serves.

use serde_json::{json, Value};
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;

use crate::support::{emulator, error_code, foundation_model_arn, label, Emulator, Reply, REGION};

/// The published catalogue this service serves through `ListFoundationModels`.
const CATALOG: &[bedrock::state::FoundationModel] = bedrock::state::FOUNDATION_MODELS;

/// `CommitmentDuration` — the terms a purchase may name
/// (`smithy_query(query="shape",shape="CommitmentDuration")`); omitting the member is the
/// no-commitment purchase this suite is about.
const COMMITMENT: &str = "OneMonth";

/// The Regions the rows sign for: the suite's own and two others, so the Region the refusal
/// states is the one the REQUEST addressed rather than a fixed one.
const REGIONS: &[&str] = &[REGION, "us-east-1", "eu-central-1"];

/// The model-unit counts the rows ask for, each inside `PositiveInteger`'s `@range(min: 1)`:
/// the bound itself, a small count and one far past any quota — every one of them more than
/// the zero no-commitment model units an account holds.
const UNIT_COUNTS: &[i32] = &[1, 2, 64, i32::MAX];

/// A count BELOW `PositiveInteger`'s published minimum, which is an input fault rather than
/// a purchase to classify.
const BELOW_MINIMUM_UNITS: i32 = 0;

/// The envelope and clause the constraint walk states for a `@range(min: 1)` violation of a
/// member carried in the request body, written out here so this suite pins the wording a
/// client reads.
const MODEL_UNITS_RANGE_REFUSAL: &str = "1 validation error detected: Value at 'modelUnits' \
     failed to satisfy constraint: Member must have value greater than or equal to 1";

/// One candidate: a model the emulator's own listing reports as purchasable with Provisioned
/// Throughput, as that listing spells it.
#[derive(Debug)]
struct Candidate {
    model_id: String,
    model_arn: String,
    lifecycle_status: String,
}

/// The models `ListFoundationModels(byInferenceType=PROVISIONED)` reports, which is the set
/// this service advertises a Provisioned Throughput purchase over.
async fn provisioned_candidates(emu: &Emulator) -> Vec<Candidate> {
    let reply = emu
        .call("GET", "/foundation-models?byInferenceType=PROVISIONED", None)
        .await;
    assert_eq!(
        reply.status, 200,
        "case \"the PROVISIONED listing\": the catalogue must be served, got {} {:?}",
        reply.status, reply.body
    );
    let candidates: Vec<Candidate> = reply
        .get("modelSummaries")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|summary| Candidate {
            model_id: text(summary, "modelId"),
            model_arn: text(summary, "modelArn"),
            lifecycle_status: summary
                .get("modelLifecycle")
                .and_then(|lifecycle| lifecycle.get("status"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
        .collect();
    assert!(
        !candidates.is_empty(),
        "case \"the PROVISIONED listing\": the catalogue must advertise a model with \
         Provisioned Throughput, or this table asserts nothing, got {:?}",
        reply.body
    );
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.lifecycle_status == "ACTIVE"),
        "case \"the PROVISIONED listing\": at least one advertised model must be ACTIVE, got \
         {candidates:?}"
    );
    candidates
}

/// The string member `name` of a listed summary.
fn text(summary: &Value, name: &str) -> String {
    summary
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a model summary must carry {name}, got {summary:?}"))
        .to_string()
}

/// A `CreateProvisionedModelThroughput` body: `provisionedModelName`, `modelId` and
/// `modelUnits` are the `@required` members, and `commitmentDuration` is carried only when
/// the row names a term.
fn purchase(name: &str, model_id: &str, units: i32, commitment: Option<&str>) -> Value {
    let mut body = json!({
        "provisionedModelName": name,
        "modelId": model_id,
        "modelUnits": units,
    });
    if let Some(commitment) = commitment {
        body["commitmentDuration"] = json!(commitment);
    }
    body
}

/// `POST /provisioned-model-throughput`, signed for `region`.
async fn buy(emu: &Emulator, region: &str, body: Value) -> Reply {
    emu.call_in_region(region, "POST", "/provisioned-model-throughput", Some(body))
        .await
}

/// The wire error code a refusal carries.
fn code(reply: &Reply) -> &str {
    reply.error.as_deref().map(error_code).unwrap_or("none")
}

/// The `message` a refusal carries.
fn message(case: &str, reply: &Reply) -> String {
    reply
        .body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case}: a refusal must carry a message, got {:?}", reply.body)
        })
        .to_string()
}

/// A `provisionedModelName` inside `ProvisionedModelName`'s published pattern
/// (`^([0-9a-zA-Z][_-]?)+$`), unique per row so no row is answered out of another's record.
fn name(stem: &str, index: usize) -> String {
    format!("pt-{stem}-{index}")
}

/// What `ListProvisionedModelThroughputs` reports in `region`.
async fn listed(emu: &Emulator, region: &str, case: &str) -> Vec<Value> {
    let reply = emu
        .call_in_region(region, "GET", "/provisioned-model-throughputs", None)
        .await;
    assert_eq!(
        reply.status, 200,
        "case {case}: the listing must be served, got {} {:?}",
        reply.status, reply.body
    );
    reply
        .get("provisionedModelSummaries")
        .as_array()
        .cloned()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------
// 1. Every advertised model reaches the granted-model-unit classification.
// ---------------------------------------------------------------------------------------

/// RULE: a no-commitment purchase over a model the service's own catalogue advertises with
/// `PROVISIONED` inference is classified against the account's granted no-commitment model
/// units — zero — and so answered 400 `ServiceQuotaExceededException` naming that model's
/// BARE id and the Region the request addressed, whichever spelling the request used,
/// whichever Region it was signed for and however many units it asked for.
#[tokio::test]
async fn a_no_commitment_purchase_of_an_advertised_model_is_classified_against_the_quota() {
    let emu = emulator();
    let candidates = provisioned_candidates(&emu).await;

    for (index, candidate) in candidates.iter().enumerate() {
        // Both spellings `ModelIdentifier` admits, each with its own Region and unit count,
        // so no row shares a variable with its neighbour.
        let spellings = [
            ("its bare model id", candidate.model_id.clone()),
            ("its foundation-model ARN", candidate.model_arn.clone()),
        ];
        for (spelling_index, (spelling, identifier)) in spellings.iter().enumerate() {
            let region = REGIONS[(index + spelling_index) % REGIONS.len()];
            let units = UNIT_COUNTS[(index + spelling_index) % UNIT_COUNTS.len()];
            let case = format!(
                "{} ({}) by {spelling}, {units} model unit(s) in {region}",
                candidate.model_id, candidate.lifecycle_status
            );
            let reply = buy(
                &emu,
                region,
                purchase(
                    &name("nc", index * spellings.len() + spelling_index),
                    identifier,
                    units,
                    None,
                ),
            )
            .await;
            assert_eq!(
                (reply.status, code(&reply)),
                (400, "ServiceQuotaExceededException"),
                "case {case}: an advertised model must reach the granted-model-unit refusal, \
                 got {} {} {:?}",
                reply.status,
                code(&reply),
                reply.body
            );
            // The sentence is built from the request: the model's BARE id — never the ARN
            // the request may have spelled — and the Region the request addressed.
            let expected =
                bedrock::state::no_commitment_model_units_message(&candidate.model_id, region);
            let stated = message(&case, &reply);
            assert_eq!(
                stated, expected,
                "case {case}: the refusal must name the model units the account lacks, got \
                 {stated:?}"
            );
        }
    }

    // A refused purchase bought nothing, in any Region it was attempted in.
    for region in REGIONS {
        let summaries = listed(&emu, region, "after the refused purchases").await;
        assert!(
            summaries.is_empty(),
            "case \"after the refused purchases\" in {region}: a refused purchase must leave \
             the listing empty, got {summaries:?}"
        );
    }
    for index in 0..candidates.len() * 2 {
        let read = emu
            .call(
                "GET",
                &format!("/provisioned-model-throughput/{}", label(&name("nc", index))),
                None,
            )
            .await;
        assert_eq!(
            (read.status, code(&read)),
            (404, "ResourceNotFoundException"),
            "case {:?}: a refused purchase must store nothing, got {} {} {:?}",
            name("nc", index),
            read.status,
            code(&read),
            read.body
        );
    }
}

// ---------------------------------------------------------------------------------------
// 2. The two refusals stay distinguishable.
// ---------------------------------------------------------------------------------------

/// RULE: the not-supported `ValidationException` is about the MODEL, so it stays reserved
/// for a model offered under no Provisioned Throughput at all — one the catalogue offers on
/// demand only, and one this service recognises and offers no longer — while the model-unit
/// `ServiceQuotaExceededException` is about the ACCOUNT. The same model under a commitment
/// term draws on the commitment quota instead and is bought, which is what makes the term a
/// choice of quota rather than a property of the model.
#[tokio::test]
async fn the_not_supported_refusal_is_reserved_for_models_offered_no_provisioned_throughput() {
    let emu = emulator();

    // A model the catalogue carries and offers ON DEMAND ONLY, and a model the service
    // recognises and offers no longer: two classes, taken from the published data rather
    // than written out, in both spellings `ModelIdentifier` admits.
    let on_demand_only: Vec<&str> = CATALOG
        .iter()
        .filter(|model| !bedrock::state::supports_provisioned_throughput(model))
        .map(|model| model.model_id)
        .collect();
    assert!(
        !on_demand_only.is_empty(),
        "the catalogue must carry a model offered on demand only, or this table asserts \
         nothing"
    );
    let retired: Vec<&str> = bedrock::state::RETIRED_FOUNDATION_MODELS.to_vec();
    assert!(
        !retired.is_empty(),
        "the service must recognise a retired model, or this table asserts nothing"
    );

    let mut index = 0;
    for (class, model_ids) in [
        ("offered on demand only", on_demand_only),
        ("offered no longer", retired),
    ] {
        for model_id in model_ids {
            for (spelling, identifier) in [
                ("its bare model id", model_id.to_string()),
                (
                    "its foundation-model ARN",
                    foundation_model_arn(REGION, model_id),
                ),
            ] {
                // With a term and without one alike: the model backs no purchase either way.
                for commitment in [None, Some(COMMITMENT)] {
                    let case = format!(
                        "{model_id} ({class}) by {spelling}, commitment {commitment:?}"
                    );
                    let reply = buy(
                        &emu,
                        REGION,
                        purchase(&name("ns", index), &identifier, 1, commitment),
                    )
                    .await;
                    index += 1;
                    assert_eq!(
                        (reply.status, code(&reply)),
                        (400, "ValidationException"),
                        "case {case}: a model no Provisioned Throughput is offered over must \
                         be refused as not supported, got {} {} {:?}",
                        reply.status,
                        code(&reply),
                        reply.body
                    );
                    assert_eq!(
                        message(&case, &reply),
                        bedrock::state::MODEL_NOT_SUPPORTED_FOR_PROVISIONED_THROUGHPUT,
                        "case {case}: the refusal must be the not-supported sentence, got {:?}",
                        reply.body
                    );
                }
            }
        }
    }

    // The contrast the rule rests on: an advertised model under a TERM is bought, and the
    // same model with no term is answered about the account's model units.
    let candidates = provisioned_candidates(&emu).await;
    for (candidate_index, candidate) in candidates.iter().enumerate() {
        let case = format!("{} under a term", candidate.model_id);
        let bought = buy(
            &emu,
            REGION,
            purchase(
                &name("term", candidate_index),
                &candidate.model_id,
                1,
                Some(COMMITMENT),
            ),
        )
        .await;
        assert_eq!(
            bought.status, 201,
            "case {case}: an advertised model under a term must be bought, got {} {:?}",
            bought.status, bought.body
        );
        // Round trip: the purchase the create stored reads back under the ARN it minted.
        let arn = bought.text("provisionedModelArn");
        let read = emu
            .call(
                "GET",
                &format!("/provisioned-model-throughput/{}", label(&arn)),
                None,
            )
            .await;
        assert_eq!(
            read.status, 200,
            "case {case}: the bought Provisioned Throughput must read back, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.get("modelArn"),
            json!(foundation_model_arn(REGION, &candidate.model_id)),
            "case {case}: the read must report the base model the purchase named, got {:?}",
            read.body
        );
    }
}

/// RULE (the other side of the classification): `modelUnits` below `PositiveInteger`'s
/// published minimum is an input fault, refused by the constraint walk with its own
/// `ValidationException` — so a malformed purchase and a quota-gated one never collapse into
/// one answer.
#[tokio::test]
async fn a_model_unit_count_below_the_published_minimum_stays_an_input_fault() {
    let emu = emulator();
    let candidates = provisioned_candidates(&emu).await;

    for (index, candidate) in candidates.iter().enumerate() {
        let case = format!("{} with {BELOW_MINIMUM_UNITS} model units", candidate.model_id);
        let reply = buy(
            &emu,
            REGION,
            purchase(
                &name("below", index),
                &candidate.model_id,
                BELOW_MINIMUM_UNITS,
                None,
            ),
        )
        .await;
        assert_eq!(
            (reply.status, code(&reply)),
            (400, "ValidationException"),
            "case {case}: a count below the published minimum must be an input fault, got {} \
             {} {:?}",
            reply.status,
            code(&reply),
            reply.body
        );
        let stated = message(&case, &reply);
        assert_eq!(
            stated, MODEL_UNITS_RANGE_REFUSAL,
            "case {case}: the refusal must state the range the member publishes, got {stated:?}"
        );
        // And the SAME request with a count the member admits is answered about the account's
        // model units instead: the two refusals are told apart by the request.
        let quota_gated = buy(
            &emu,
            REGION,
            purchase(&name("admitted", index), &candidate.model_id, 1, None),
        )
        .await;
        assert_eq!(
            (quota_gated.status, code(&quota_gated)),
            (400, "ServiceQuotaExceededException"),
            "case {case}: a well-formed count must reach the model-unit refusal, got {} {} {:?}",
            quota_gated.status,
            code(&quota_gated),
            quota_gated.body
        );
    }
}
