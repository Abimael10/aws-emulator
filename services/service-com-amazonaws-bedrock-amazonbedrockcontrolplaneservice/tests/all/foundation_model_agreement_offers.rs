// SPDX-License-Identifier: CC0-1.0

//! `ListFoundationModelAgreementOffers` and `CreateFoundationModelAgreement` end to end over
//! HTTP, through the same `MultiServiceRouter` the emulator boots.
//!
//! # The rule
//!
//! An offer is an offer of the AGREEMENT a model is published under, so the `modelId` has
//! to name a model this account's published catalog offers under one: such a model answers
//! 200 with its own offers — an offer carrying the `offerToken` that feeds
//! `CreateFoundationModelAgreement` and the term structures the agreement is made on.
//!
//! Every other `modelId` names no model there are offers OF, and is refused rather than
//! answered with an empty list: a model AWS publishes itself is reached without accepting
//! another party's terms, so no agreement of it exists to be offered; a model the service
//! recognises and offers no longer is offered under nothing at all; and an id naming no
//! model of the catalog at all names nothing either — none of the three is a value this
//! surface can read as the identifier of an agreement-bearing model, which is the 400
//! `ValidationException` carrying this service's fixed invalid-identifier sentence.
//!
//! **Accepting an offer** is the write that answers it: the `offerToken` of an offer of a
//! model accepts THAT model's agreement for the account, which a later read of the model's
//! availability reports — once the account's use case for model access is on record, which
//! is the acknowledgement Amazon Bedrock takes before it acts on a request for access to a
//! model. A token that names no offer of the model, an id that names no agreement-bearing
//! model, and a model whose agreement the account already holds each accept nothing and
//! leave the account's standing as it was.
//!
//! The last two tables walk the reading of `modelId` both operations share over its WHOLE
//! class: every id this service recognises and offers no longer, every model AWS publishes
//! itself and ids naming no catalogued model, each in both forms `BedrockModelId` admits,
//! against both operations — and over what else is wrong with the request, which is what
//! shows the identifier is read before any of it.
//!
//! # What the tables below generalise over
//!
//! The rows walk the WHOLE published catalog rather than a chosen row of it, so every
//! provider in it is covered on both sides of the rule, and they differ along the
//! `offerType` the request names (absent, `ALL`, `PUBLIC` — the two values `OfferType`
//! declares) and along the form of the `modelId` (the bare id, and the qualified form
//! `BedrockModelId`'s pattern admits). The refusal rows differ along WHY an id names no
//! agreement-bearing model: a provider the catalog carries no model of, a model of a
//! provider it does carry, a deployment-suffixed id, a model past its end of life, and a
//! model AWS publishes itself. The acceptance rows add the axes that rule generalises over:
//! the model accepted (every agreement-bearing row of the catalog, in both spellings of its
//! id), and WHY a request accepts nothing (the model it names, the token it carries, the
//! acknowledgement the account has not submitted, and the agreement it already holds).

use serde_json::{json, Value};
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;

use crate::support::{emulator, error_code, label, Emulator, Reply};

/// The published catalog this service serves.
const CATALOG: &[bedrock::state::FoundationModel] = bedrock::state::FOUNDATION_MODELS;

/// Every `offerType` a request can carry: absent, and the two values `OfferType` declares
/// (`smithy_query(query="shape",shape="OfferType")`).
const OFFER_TYPES: [Option<&str>; 3] = [
    None,
    Some(bedrock::state::OFFER_TYPE_ALL),
    Some(bedrock::state::OFFER_TYPE_PUBLIC),
];

/// `GET /list-foundation-model-agreement-offers/{modelId}`, with the `offerType` the row
/// names.
async fn offers(emulator: &Emulator, model_id: &str, offer_type: Option<&str>) -> Reply {
    let path = match offer_type {
        Some(offer_type) => format!(
            "/list-foundation-model-agreement-offers/{}?offerType={offer_type}",
            label(model_id)
        ),
        None => format!(
            "/list-foundation-model-agreement-offers/{}",
            label(model_id)
        ),
    };
    emulator.call("GET", &path, None).await
}

/// The offers a reply reports.
fn reported(reply: &Reply) -> Vec<Value> {
    reply.get("offers").as_array().cloned().unwrap_or_default()
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
            panic!(
                "case {case}: a refusal must carry a message, got {:?}",
                reply.body
            )
        })
        .to_string()
}

/// Assert `reply` is the refusal this service answers an id that names no agreement-bearing
/// model with: 400 `ValidationException`, the error class in the `x-amzn-errortype` header,
/// carrying the fixed invalid-identifier sentence and no offers.
fn assert_invalid_identifier(case: &str, reply: &Reply) {
    assert_eq!(
        (reply.status, code(reply)),
        (400, "ValidationException"),
        "case {case}: must be refused 400 ValidationException, got {} {} {:?}",
        reply.status,
        code(reply),
        reply.body
    );
    assert_eq!(
        message(case, reply),
        bedrock::state::INVALID_MODEL_IDENTIFIER_MESSAGE,
        "case {case}: the refusal must be the service's fixed sentence, got {:?}",
        reply.body
    );
    assert!(
        reported(reply).is_empty(),
        "case {case}: a refusal reports no offers, got {:?}",
        reply.body
    );
}

/// Every model the account's own `ListFoundationModels` catalog reports.
async fn catalogued(emulator: &Emulator) -> Vec<Value> {
    let listed = emulator.call("GET", "/foundation-models", None).await;
    assert_eq!(
        listed.status, 200,
        "the catalog must be served, got {} {:?}",
        listed.status, listed.body
    );
    let summaries = listed
        .get("modelSummaries")
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !summaries.is_empty(),
        "the published catalog must report models, got {:?}",
        listed.body
    );
    summaries
}

/// RULE: a catalogued model is answered by whether it is offered under an agreement at all
/// — one offered under a provider's terms answers 200 with its own usable offer, and one AWS
/// publishes itself has no agreement to be offered and is refused as an identifier naming no
/// agreement-bearing model.
#[tokio::test]
async fn every_catalogued_model_is_answered_by_the_agreement_it_is_offered_under() {
    let emulator = emulator();
    let mut with_offers = 0usize;
    let mut refused = 0usize;
    for summary in catalogued(&emulator).await {
        let model_id = summary["modelId"].as_str().expect("a modelId").to_string();
        let provider = summary["providerName"]
            .as_str()
            .expect("a providerName")
            .to_string();
        let case = format!("{model_id} by {provider}");

        let reply = offers(&emulator, &model_id, None).await;

        // A model AWS publishes itself carries no agreement to accept; every other
        // provider's model is offered under that provider's own terms.
        if provider == bedrock::state::AWS_MODEL_PROVIDER {
            assert_invalid_identifier(&case, &reply);
            refused += 1;
            continue;
        }

        assert_eq!(
            reply.status, 200,
            "case {case}: a model offered under an agreement must be answered 200, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.text("modelId"),
            model_id,
            "case {case}: the response must name the model it is about, got {:?}",
            reply.get("modelId")
        );

        let listed = reported(&reply);
        assert!(
            !listed.is_empty(),
            "case {case}: a model offered under a provider's own terms must expose an offer, \
             got {:?}",
            reply.body
        );
        with_offers += 1;

        for offer in &listed {
            let token = offer["offerToken"].as_str().unwrap_or_else(|| {
                panic!("case {case}: an offer must carry an offerToken, got {offer:?}")
            });
            assert!(
                !token.is_empty(),
                "case {case}: an offerToken must be usable, got {token:?}"
            );
            let offer_id = offer["offerId"].as_str().unwrap_or_default();
            assert_eq!(
                offer_id,
                bedrock::state::model_offer_id(&model_id),
                "case {case}: the offerId must be the one this model's offer carries, got \
                 {offer_id:?}"
            );
            assert_eq!(
                token,
                bedrock::state::model_offer_token(&model_id, offer_id),
                "case {case}: the offerToken must be the token of this model's offer, got \
                 {token:?}"
            );
            // The terms an offer is made on: the three structures `TermDetails` requires
            // are reported, and the price list, license document and support policy they
            // would name are not invented here.
            for term in ["usageBasedPricingTerm", "legalTerm", "supportTerm"] {
                assert!(
                    offer["termDetails"].get(term).is_some(),
                    "case {case}: termDetails must carry {term}, got {:?}",
                    offer["termDetails"]
                );
            }
            assert_eq!(
                offer["termDetails"]["usageBasedPricingTerm"]
                    .get("rateCard")
                    .and_then(Value::as_array)
                    .map(Vec::len),
                Some(0),
                "case {case}: the usage-based pricing term must report its rate card, got {:?}",
                offer["termDetails"]
            );
        }

        // Reading twice changes nothing: this operation is @readonly, and an offer is
        // published data rather than a resource a read brings into being.
        let again = offers(&emulator, &model_id, None).await;
        assert_eq!(
            reported(&again),
            listed,
            "case {case}: a second listing must report the same offers, got {:?}",
            again.body
        );
    }
    assert!(
        with_offers > 0 && refused > 0,
        "the catalog must carry both a model offered under an agreement and one offered under \
         none, got {with_offers} and {refused}"
    );
}

/// RULE: `offerType` narrows the listing to the offers of that type. Every offer this
/// emulator publishes is public, so `ALL` and `PUBLIC` admit the same offers and a request
/// naming neither gets all of them.
#[tokio::test]
async fn the_offer_type_narrows_the_listing_to_offers_of_that_type() {
    let emulator = emulator();
    // A third-party model: the one that has an offer to narrow at all. Its id is a
    // published row of this service's own catalog.
    const MODEL_ID: &str = "anthropic.claude-sonnet-4-20250514-v1:0";
    let all = reported(&offers(&emulator, MODEL_ID, None).await);
    assert!(
        !all.is_empty(),
        "the fixture model must expose an offer, got {all:?}"
    );
    for offer_type in [
        bedrock::state::OFFER_TYPE_ALL,
        bedrock::state::OFFER_TYPE_PUBLIC,
    ] {
        let reply = offers(&emulator, MODEL_ID, Some(offer_type)).await;
        assert_eq!(
            reply.status, 200,
            "case {offer_type}: the listing must answer 200, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reported(&reply),
            all,
            "case {offer_type}: every offer here is public, so this type admits them all, got \
             {:?}",
            reply.body
        );
    }

    // A value `OfferType` does not declare is refused by the member's own constraint,
    // ahead of the handler.
    let unknown = offers(&emulator, MODEL_ID, Some("PRIVATE")).await;
    assert_eq!(
        unknown.status, 400,
        "case an undeclared offerType: must be refused 400, got {} {:?}",
        unknown.status, unknown.body
    );
    assert_eq!(
        unknown.error.as_deref(),
        Some("ValidationException"),
        "case an undeclared offerType: a constraint violation is a ValidationException, got {:?}",
        unknown.error
    );
}

/// RULE: a `modelId` that names no model offered under an agreement is refused 400
/// `ValidationException` with the fixed invalid-identifier sentence — never an empty success
/// — whichever `offerType` the request carries, and the qualified form of an
/// agreement-bearing id names that same published model.
#[tokio::test]
async fn an_id_that_names_no_agreement_bearing_model_is_refused_as_invalid() {
    let emulator = emulator();

    // The qualified form `BedrockModelId`'s pattern admits — the published id with the
    // `:`-separated qualifiers AWS appends to it — names the same model, so it answers the
    // offers of that model.
    let qualified = offers(&emulator, "anthropic.claude-sonnet-4-20250514-v1:0:200k", None).await;
    assert_eq!(
        qualified.status, 200,
        "case a qualified model id: must name the same published model, got {} {:?}",
        qualified.status, qualified.body
    );
    assert!(
        !reported(&qualified).is_empty(),
        "case a qualified model id: must report that model's offers, got {:?}",
        qualified.body
    );

    // One axis per row: a provider the catalog has no model of, a model id of a provider it
    // does carry, a well-formed id whose model was never published, and — the other half of
    // the rule — models AWS publishes itself, which the catalog carries and offers under no
    // agreement. The AWS-published rows are read off the catalog rather than written out, so
    // they are not one chosen id.
    let mut rows: Vec<(String, String)> = vec![
        (
            "a provider the catalog carries no model of".to_string(),
            "acme.model-v1".to_string(),
        ),
        (
            "a model the catalog does not carry from a provider it does".to_string(),
            "anthropic.claude-9-opus-20990101-v9:0".to_string(),
        ),
        (
            "a model id with a deployment suffix".to_string(),
            "meta.llama3-1-8b-instruct-v1:0/abcdef123456".to_string(),
        ),
    ];
    let aws_published: Vec<&bedrock::state::FoundationModel> = CATALOG
        .iter()
        .filter(|model| !bedrock::state::model_offered_under_agreement(model))
        .collect();
    assert!(
        aws_published.len() > 1,
        "the catalog must carry models AWS publishes itself, got {}",
        aws_published.len()
    );
    for model in &aws_published {
        rows.push((
            format!("the AWS-published model {}", model.model_id),
            model.model_id.to_string(),
        ));
    }

    for (case, model_id) in rows {
        for offer_type in OFFER_TYPES {
            let case = format!("{case} with offerType {offer_type:?}");
            let reply = offers(&emulator, &model_id, offer_type).await;
            assert_invalid_identifier(&case, &reply);
        }
    }
}

/// RULE: a model this service recognises and offers no longer is offered under nothing at
/// all, so there is no agreement of it for an offer to be an offer of: its id is refused as
/// one naming no agreement-bearing model, exactly as every other such id is.
#[tokio::test]
async fn a_model_past_its_end_of_life_has_no_offers_to_report() {
    let emulator = emulator();
    // Three providers, read off the ids this service publishes as past their end of life
    // (`crate::state::RETIRED_FOUNDATION_MODELS`), none of them the id the recorded step
    // named.
    for model_id in [
        "anthropic.claude-v2",
        "ai21.j2-ultra-v1",
        "meta.llama2-70b-chat-v1",
    ] {
        assert!(
            bedrock::state::RETIRED_FOUNDATION_MODELS.contains(&model_id),
            "case {model_id}: this row must name a model the service recognises as retired"
        );
        let reply = offers(&emulator, model_id, None).await;
        assert_invalid_identifier(model_id, &reply);
    }
}

// =======================================================================================
// `CreateFoundationModelAgreement`: accepting the agreement an offer is an offer OF
// =======================================================================================

/// The URI `CreateFoundationModelAgreement` is bound to
/// (`smithy_query(query="operation",operation="CreateFoundationModelAgreement")`: POST
/// `/create-foundation-model-agreement`, success code 202).
const ACCEPT_URI: &str = "/create-foundation-model-agreement";

/// The URI `PutUseCaseForModelAccess` is bound to (`query="operation"`: POST
/// `/use-case-for-model-access`, success code 201).
const USE_CASE_URI: &str = "/use-case-for-model-access";

/// A form inside `AcknowledgementFormDataBody`'s `@length(10..16384)`, as the account's
/// acknowledged use case for model access.
const USE_CASE_FORM: &[u8] = br#"{"companyName":"Acme","useCase":"document summarisation"}"#;

/// The standing a model whose provider's terms this account has NOT accepted is reported
/// in: no agreement, no entitlement, no authorization.
const OUTSTANDING: (&str, &str, &str) = (
    bedrock::state::AGREEMENT_STATUS_NOT_AVAILABLE,
    bedrock::state::AVAILABILITY_NOT_AVAILABLE,
    bedrock::state::AUTHORIZATION_STATUS_NOT_AUTHORIZED,
);

/// The standing a model whose agreement this account holds is reported in.
const ENTITLED: (&str, &str, &str) = (
    bedrock::state::AGREEMENT_STATUS_AVAILABLE,
    bedrock::state::AVAILABILITY_AVAILABLE,
    bedrock::state::AUTHORIZATION_STATUS_AUTHORIZED,
);

/// Every catalogued model offered under a provider's own agreement: the rows that have an
/// agreement to accept at all.
fn agreement_models() -> Vec<&'static bedrock::state::FoundationModel> {
    CATALOG
        .iter()
        .filter(|model| bedrock::state::model_offered_under_agreement(model))
        .collect()
}

/// `POST /use-case-for-model-access`: the one account-level acknowledgement
/// `PutUseCaseForModelAccess` submits. A blob member travels as base64 on restJson1.
async fn submit_use_case(emulator: &Emulator) -> Reply {
    let reply = emulator
        .call(
            "POST",
            USE_CASE_URI,
            Some(json!({ "formData": ::aws_smithy_types::base64::encode(USE_CASE_FORM) })),
        )
        .await;
    assert_eq!(
        reply.status, 201,
        "case the account's use case for model access: the put must answer 201, got {} {:?}",
        reply.status, reply.body
    );
    reply
}

/// `POST /create-foundation-model-agreement` with the `modelId` and `offerToken` a row
/// names.
async fn accept(emulator: &Emulator, model_id: &str, offer_token: &str) -> Reply {
    emulator
        .call(
            "POST",
            ACCEPT_URI,
            Some(json!({ "modelId": model_id, "offerToken": offer_token })),
        )
        .await
}

/// `GET /foundation-model-availability/{modelId}`: the read an acceptance shows up in.
async fn availability(emulator: &Emulator, model_id: &str) -> Reply {
    emulator
        .call(
            "GET",
            &format!("/foundation-model-availability/{}", label(model_id)),
            None,
        )
        .await
}

/// The `offerToken` the listing of `model_id`'s offers hands out — the token that accepts
/// that model's agreement.
async fn offer_token_of(emulator: &Emulator, model_id: &str) -> String {
    let listed = reported(&offers(emulator, model_id, None).await);
    let offer = listed
        .first()
        .unwrap_or_else(|| panic!("case {model_id}: the model must expose an offer to accept"));
    offer["offerToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {model_id}: an offer must carry an offerToken, got {offer:?}"))
        .to_string()
}

/// Assert the standing `GET /foundation-model-availability/{model_id}` reports is
/// `expected` — the one read that answers whether this account may use the model.
async fn assert_standing(
    emulator: &Emulator,
    case: &str,
    model_id: &str,
    expected: (&str, &str, &str),
) {
    let reply = availability(emulator, model_id).await;
    assert_eq!(
        reply.status, 200,
        "case {case}: a catalogued model's availability must be served, got {} {:?}",
        reply.status, reply.body
    );
    let actual = (
        reply
            .get("agreementAvailability")
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_string(),
        reply
            .get("entitlementAvailability")
            .as_str()
            .unwrap_or("none")
            .to_string(),
        reply
            .get("authorizationStatus")
            .as_str()
            .unwrap_or("none")
            .to_string(),
    );
    assert_eq!(
        (actual.0.as_str(), actual.1.as_str(), actual.2.as_str()),
        expected,
        "case {case}: expected the standing {expected:?}, got {:?}",
        reply.body
    );
}

/// RULE: the `offerToken` of an offer of a model accepts THAT model's agreement for the
/// account, and the acceptance is state: the read of the model's availability reports the
/// account entitled to it from then on, whichever spelling of the id the acceptance named.
/// The terms of one model are accepted once, so a repeat of an accepted request is a
/// conflict rather than a second acceptance.
#[tokio::test]
async fn accepting_an_offer_of_a_model_entitles_the_account_to_that_model() {
    let emulator = emulator();
    submit_use_case(&emulator).await;

    let models = agreement_models();
    assert!(
        models.len() > 1,
        "the catalog must carry models offered under an agreement, got {}",
        models.len()
    );
    for (index, model) in models.iter().enumerate() {
        // The rows alternate between the two spellings `BedrockModelId` admits: the bare
        // published id, and the `:`-qualified form, which names a variant of the same
        // published model — so the agreement either spelling accepts is the model's.
        let qualified = index % 2 == 1 && model.model_id.matches(':').count() < 2;
        let requested = if qualified {
            format!("{}:24k", model.model_id)
        } else {
            model.model_id.to_string()
        };
        let case = format!("{} accepted as {requested}", model.model_id);

        // Before: the model's terms are outstanding, so the account is entitled to nothing.
        assert_standing(&emulator, &case, model.model_id, OUTSTANDING).await;

        let token = offer_token_of(&emulator, model.model_id).await;
        let reply = accept(&emulator, &requested, &token).await;
        assert_eq!(
            reply.status, 202,
            "case {case}: an accepted agreement answers 202, got {} {:?}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.text("modelId"),
            requested,
            "case {case}: the response must name the model the agreement is for, got {:?}",
            reply.body
        );

        // After: the acceptance is state the availability read reports, under the bare
        // published id whichever spelling accepted it.
        assert_standing(&emulator, &case, model.model_id, ENTITLED).await;

        // The agreement of one model is accepted once.
        let again = accept(&emulator, &requested, &token).await;
        assert_eq!(
            (again.status, code(&again)),
            (400, "ConflictException"),
            "case {case}: a second acceptance must be refused as a conflict, got {} {} {:?}",
            again.status,
            code(&again),
            again.body
        );
        assert_eq!(
            message(&case, &again),
            bedrock::state::foundation_model_agreement_conflict_message(&requested),
            "case {case}: the conflict must state the agreement the account holds, got {:?}",
            again.body
        );
        assert_standing(&emulator, &case, model.model_id, ENTITLED).await;
    }
}

/// One request that accepts nothing: the model and token it carries, and the refusal it must
/// answer.
struct Refused {
    case: String,
    model_id: String,
    offer_token: String,
    status: u16,
    code: &'static str,
    message: String,
}

/// RULE: a request that names no agreement-bearing model, or no offer of the model it names,
/// accepts nothing — it is refused in the class that identifier's own reading decides, and
/// leaves every model's standing and every offer exactly as they were.
#[tokio::test]
async fn a_request_naming_no_offer_of_an_agreement_bearing_model_accepts_nothing() {
    let emulator = emulator();
    submit_use_case(&emulator).await;

    let models = agreement_models();
    // The model the token rows are read against, and a model of ANOTHER provider whose token
    // is one of those rows — both read off the catalog rather than written out.
    let model = models[0];
    let other = models
        .iter()
        .find(|row| row.provider_name != model.provider_name)
        .expect("the catalog must carry agreement-bearing models of two providers");
    let token = offer_token_of(&emulator, model.model_id).await;
    let other_token = offer_token_of(&emulator, other.model_id).await;

    let mut rows: Vec<Refused> = Vec::new();

    // The model id names no model this operation can read as an agreement-bearing one: a
    // provider the catalog carries no model of, a model the catalog does not carry from a
    // provider it does, and a deployment-suffixed id.
    for unreadable in [
        "acme.model-v1",
        "anthropic.claude-9-opus-20990101-v9:0",
        "meta.llama3-1-8b-instruct-v1:0/abcdef123456",
    ] {
        rows.push(Refused {
            case: format!("the id {unreadable}, which names no catalogued model"),
            model_id: unreadable.to_string(),
            offer_token: token.clone(),
            status: 400,
            code: "ValidationException",
            message: bedrock::state::INVALID_MODEL_IDENTIFIER_MESSAGE.to_string(),
        });
    }
    // A model AWS publishes itself is reached under no provider's terms, so there is no
    // agreement of it to accept. Read off the catalog, so these are not one chosen id.
    let aws_published: Vec<&bedrock::state::FoundationModel> = CATALOG
        .iter()
        .filter(|row| !bedrock::state::model_offered_under_agreement(row))
        .collect();
    assert!(
        aws_published.len() > 1,
        "the catalog must carry models AWS publishes itself, got {}",
        aws_published.len()
    );
    for published in &aws_published {
        rows.push(Refused {
            case: format!("the AWS-published model {}", published.model_id),
            model_id: published.model_id.to_string(),
            offer_token: token.clone(),
            status: 400,
            code: "ValidationException",
            message: bedrock::state::INVALID_MODEL_IDENTIFIER_MESSAGE.to_string(),
        });
    }
    // A model this service recognises and offers no longer is offered under nothing at all,
    // from three different providers (`crate::state::RETIRED_FOUNDATION_MODELS`).
    for retired in [
        "anthropic.claude-v2",
        "ai21.j2-ultra-v1",
        "meta.llama2-70b-chat-v1",
    ] {
        assert!(
            bedrock::state::RETIRED_FOUNDATION_MODELS.contains(&retired),
            "case {retired}: this row must name a model the service recognises as retired"
        );
        rows.push(Refused {
            case: format!("the retired model {retired}"),
            model_id: retired.to_string(),
            offer_token: token.clone(),
            status: 400,
            code: "ValidationException",
            message: bedrock::state::INVALID_MODEL_IDENTIFIER_MESSAGE.to_string(),
        });
    }
    // The token names no offer of the model the request names: a value that is no token at
    // all, the offer's own `offerId`, the token of another model's offer, and the model's own
    // token mangled at either end.
    for (axis, carried) in [
        ("no offer token at all", "not-an-offer-token".to_string()),
        (
            "the offerId rather than the offerToken",
            bedrock::state::model_offer_id(model.model_id),
        ),
        ("the token of another model's offer", other_token.clone()),
        (
            "the token with its last character dropped",
            token[..token.len() - 1].to_string(),
        ),
        (
            "the token in upper case",
            token.to_ascii_uppercase(),
        ),
    ] {
        rows.push(Refused {
            case: format!("{} offered {axis}", model.model_id),
            model_id: model.model_id.to_string(),
            offer_token: carried,
            status: 400,
            code: "ValidationException",
            message: bedrock::state::INVALID_OFFER_TOKEN_MESSAGE.to_string(),
        });
    }

    for row in &rows {
        let reply = accept(&emulator, &row.model_id, &row.offer_token).await;
        assert_eq!(
            (reply.status, code(&reply)),
            (row.status, row.code),
            "case {}: expected {} {}, got {} {} {:?}",
            row.case,
            row.status,
            row.code,
            reply.status,
            code(&reply),
            reply.body
        );
        assert_eq!(
            message(&row.case, &reply),
            row.message,
            "case {}: the refusal must state the reason this request accepts nothing, got {:?}",
            row.case,
            reply.body
        );
    }

    // Nothing any of those requests named was accepted: every agreement-bearing model's
    // terms are still outstanding, and the offer of each is still listed.
    for row in &models {
        let case = format!("{} after every refused acceptance", row.model_id);
        assert_standing(&emulator, &case, row.model_id, OUTSTANDING).await;
        assert!(
            !reported(&offers(&emulator, row.model_id, None).await).is_empty(),
            "case {case}: the offer of a model whose terms are unaccepted is still listed"
        );
    }
}

/// RULE: Amazon Bedrock acts on a request for access to a model only once the account's use
/// case for model access is on record — the one account-level acknowledgement
/// `PutUseCaseForModelAccess` submits — so until then an acceptance is refused 403
/// `AccessDeniedException` and accepts nothing, and the very same request is served once the
/// form has been submitted.
#[tokio::test]
async fn an_acceptance_waits_on_the_accounts_use_case_for_model_access() {
    let emulator = emulator();

    // Two agreement-bearing models of DIFFERENT providers, read off the catalog: the
    // acknowledgement is account-level, so it gates them alike.
    let models = agreement_models();
    let first = models[0];
    let second = models
        .iter()
        .find(|row| row.provider_name != first.provider_name)
        .expect("the catalog must carry agreement-bearing models of two providers");
    let gated = [first, *second];

    let mut tokens: Vec<(&bedrock::state::FoundationModel, String)> = Vec::new();
    for model in gated {
        tokens.push((model, offer_token_of(&emulator, model.model_id).await));
    }

    for (model, token) in &tokens {
        // Twice, because a refusal records nothing: the second request is refused for the
        // same missing acknowledgement rather than as a conflict with an agreement the first
        // one would have made.
        for attempt in 1..=2 {
            let case = format!(
                "{} without the account's use case, attempt {attempt}",
                model.model_id
            );
            let reply = accept(&emulator, model.model_id, token).await;
            assert_eq!(
                (reply.status, code(&reply)),
                (403, "AccessDeniedException"),
                "case {case}: expected 403 AccessDeniedException, got {} {} {:?}",
                reply.status,
                code(&reply),
                reply.body
            );
            assert_eq!(
                message(&case, &reply),
                bedrock::state::MODEL_ACCESS_USE_CASE_REQUIRED_MESSAGE,
                "case {case}: the refusal must state the acknowledgement it waits on, got {:?}",
                reply.body
            );
            assert_standing(&emulator, &case, model.model_id, OUTSTANDING).await;
            assert!(
                !reported(&offers(&emulator, model.model_id, None).await).is_empty(),
                "case {case}: the offer of the model is still outstanding to be accepted"
            );
        }
    }

    // With the acknowledgement on record, the very same requests are served.
    submit_use_case(&emulator).await;
    for (model, token) in &tokens {
        let case = format!("{} with the account's use case on record", model.model_id);
        let reply = accept(&emulator, model.model_id, token).await;
        assert_eq!(
            reply.status, 202,
            "case {case}: the same request must now be accepted, got {} {:?}",
            reply.status, reply.body
        );
        assert_standing(&emulator, &case, model.model_id, ENTITLED).await;
    }
}

// =======================================================================================
// The ONE reading of `modelId` both agreement operations share
// =======================================================================================

/// Every base model id this service recognises and offers NO LONGER: the ids no published
/// resource names (`crate::state::RETIRED_FOUNDATION_MODELS`), and the ids a published row is
/// kept for because a routing resource still names them
/// (`crate::state::PAST_END_OF_LIFE_MODELS`) — the whole set this service knows, rather than
/// the ids any one row of it was recorded under.
fn ids_past_end_of_life() -> Vec<&'static str> {
    let ids: Vec<&'static str> = bedrock::state::RETIRED_FOUNDATION_MODELS
        .iter()
        .copied()
        .chain(
            bedrock::state::PAST_END_OF_LIFE_MODELS
                .iter()
                .map(|model| model.model_id),
        )
        .collect();
    assert!(
        ids.len() > 1,
        "this service must recognise models it offers no longer, or the table asserts nothing"
    );
    ids
}

/// The `modelId`s this surface can read as the identifier of NO agreement-bearing model, one
/// group of rows per reason an id names none — and each id in both forms `BedrockModelId`
/// admits, the bare id and the `:`-qualified form its pattern allows up to two qualifiers of
/// (`smithy_query(query="operation",operation="CreateFoundationModelAgreement")`).
fn ids_naming_no_agreement_bearing_model() -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut push = |reason: &str, model_id: &str| {
        rows.push((format!("{reason} {model_id}"), model_id.to_string()));
        // One more qualifier than the id already carries, while the pattern still admits it:
        // a variant of the same model, so it names no agreement-bearing model either.
        if model_id.matches(':').count() < 2 {
            rows.push((
                format!("{reason} {model_id} with a version qualifier"),
                format!("{model_id}:24k"),
            ));
        }
    };
    // Offered no longer: the model is offered under nothing at all, so no agreement of it
    // exists to be offered or accepted.
    for retired in ids_past_end_of_life() {
        push("the model offered no longer", retired);
    }
    // Published by AWS itself: reached under no provider's terms, so there is no agreement of
    // it either. Read off the catalog rather than written out.
    let aws_published: Vec<&'static str> = CATALOG
        .iter()
        .filter(|model| !bedrock::state::model_offered_under_agreement(model))
        .map(|model| model.model_id)
        .collect();
    assert!(
        aws_published.len() > 1,
        "the catalog must carry models AWS publishes itself, got {}",
        aws_published.len()
    );
    for published in aws_published {
        push("the AWS-published model", published);
    }
    // Naming no model of this catalog at all: a provider it carries no model of, a model of a
    // provider it does carry, and a deployment-suffixed id.
    for unknown in [
        "acme.model-v1",
        "anthropic.claude-9-opus-20990101-v9:0",
        "meta.llama3-1-8b-instruct-v1:0/abcdef123456",
    ] {
        rows.push((
            format!("the id naming no catalogued model {unknown}"),
            unknown.to_string(),
        ));
    }
    rows
}

/// RULE: the foundation-model agreement surface answers about the TERMS a model is offered
/// under, so a `modelId` that names no model this service offers under a provider's own terms
/// — offered no longer, published by AWS itself, or naming no catalogued model at all — is a
/// value it cannot read as the identifier of an agreement-bearing model: 400
/// `ValidationException` with the fixed invalid-identifier sentence, from BOTH operations
/// that take the member, and never a report that a resource is missing.
#[tokio::test]
async fn neither_agreement_operation_reads_an_id_that_names_no_agreement_bearing_model() {
    let emulator = emulator();
    // The two conditions that could otherwise decide the answer are satisfied first: the
    // account's acknowledgement is on record, and the token carried is a real offer's.
    submit_use_case(&emulator).await;
    let offered = agreement_models()[0];
    let token = offer_token_of(&emulator, offered.model_id).await;

    for (case, model_id) in ids_naming_no_agreement_bearing_model() {
        // Listing the offers of it, with every `offerType` a request can carry.
        for offer_type in OFFER_TYPES {
            let case = format!("{case}: offers with offerType {offer_type:?}");
            let reply = offers(&emulator, &model_id, offer_type).await;
            assert_invalid_identifier(&case, &reply);
        }
        // And accepting an agreement of it.
        let case = format!("{case}: accepting an agreement");
        let reply = accept(&emulator, &model_id, &token).await;
        assert_invalid_identifier(&case, &reply);
    }

    // The same surface goes on serving the model that IS offered under a provider's terms, so
    // the rule refuses the ids it cannot read rather than the member itself.
    let listed = offers(&emulator, offered.model_id, None).await;
    assert_eq!(
        listed.status, 200,
        "case {}: a model offered under an agreement must still be served, got {} {:?}",
        offered.model_id, listed.status, listed.body
    );
    let accepted = accept(&emulator, offered.model_id, &token).await;
    assert_eq!(
        accepted.status, 202,
        "case {}: the agreement of an offered model must still be accepted, got {} {:?}",
        offered.model_id, accepted.status, accepted.body
    );
}

/// RULE: the identifier is read BEFORE the conditions that decide whether the acceptance is
/// made, so an id this surface cannot read as naming an agreement-bearing model is answered
/// as that input fault whatever else the request or the account would be refused for — and
/// such a request accepts nothing.
#[tokio::test]
async fn an_id_naming_no_agreement_bearing_model_is_refused_before_anything_else() {
    let emulator = emulator();
    // NOTHING is on record: the account has submitted no use case for model access, which is
    // what an acceptance naming an offered model would be refused for.
    let offered = agreement_models()[0];
    let token = offer_token_of(&emulator, offered.model_id).await;
    let gate = accept(&emulator, offered.model_id, &token).await;
    assert_eq!(
        (gate.status, code(&gate)),
        (403, "AccessDeniedException"),
        "case {}: without the account's use case an offered model is refused 403, or this \
         table proves no ordering, got {} {} {:?}",
        offered.model_id,
        gate.status,
        code(&gate),
        gate.body
    );

    // The rows differ along what ELSE is wrong with the request: the token names an offer of
    // no model, or it names an offer of a model that is not the one named.
    for (case, model_id) in ids_naming_no_agreement_bearing_model() {
        for (axis, carried) in [
            ("a token that names no offer", "not-an-offer-token"),
            ("the token of an offered model's offer", token.as_str()),
        ] {
            let case = format!("{case} with {axis}");
            let reply = accept(&emulator, &model_id, carried).await;
            assert_invalid_identifier(&case, &reply);
        }
    }

    // None of those requests accepted anything: the offered model's terms are still
    // outstanding, and its offer is still there to be accepted.
    assert_standing(
        &emulator,
        "after every refused acceptance",
        offered.model_id,
        OUTSTANDING,
    )
    .await;
    assert!(
        !reported(&offers(&emulator, offered.model_id, None).await).is_empty(),
        "case {}: the offer of a model whose terms are unaccepted is still listed",
        offered.model_id
    );
}
