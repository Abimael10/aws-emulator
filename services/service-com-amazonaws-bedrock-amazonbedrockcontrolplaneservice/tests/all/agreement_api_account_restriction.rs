// SPDX-License-Identifier: CC0-1.0

//! The ACCOUNT restriction on the foundation-model Agreement API, end to end over HTTP,
//! through the same `MultiServiceRouter` the emulator boots.
//!
//! # The rule
//!
//! The Agreement API is the MUTATING half of the foundation-model agreement surface:
//! `CreateFoundationModelAgreement` accepts the agreement a model is offered under and
//! `DeleteFoundationModelAgreement` withdraws that acceptance. It is a surface an account is
//! admitted to: an account Amazon Bedrock has not admitted is refused EVERY call of it with
//! 403 `AccessDeniedException` whose message names that surface — the Agreement APIs — so a
//! caller reads which surface is closed to it rather than a remediation for some other
//! refusal, and the refusal records nothing.
//!
//! The admission is account-level, so one condition governs both calls and every model:
//! Amazon Bedrock admits the account once it holds the account's use case for model access,
//! the one acknowledgement `PutUseCaseForModelAccess` submits, which names no model at all.
//!
//! The READ half of the same surface is not restricted — `ListFoundationModelAgreementOffers`
//! goes on reporting the offer of a model's agreement and `GetFoundationModelAvailability`
//! goes on reporting the model's standing — which is also what shows a refused mutating call
//! consumed no offer and changed no availability member.
//!
//! # What the tables below generalise over
//!
//! The rows span the surface the restriction covers rather than one call of it: both mutating
//! operations, every agreement-bearing model of the published catalog (so every provider that
//! publishes under its own terms), both spellings of the id `BedrockModelId` admits (the bare
//! id and the `:`-qualified form), and each row twice over, because a refusal records nothing
//! and so cannot turn the second attempt into a different answer. The admitted side drives
//! the very same calls for an account the service holds the acknowledgement of, which is what
//! shows the refusal is read off the account's standing rather than being a blanket one.

use serde_json::Value;
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice as bedrock;

use crate::foundation_model_agreement_offers::{
    accept, agreement_models, assert_standing, availability, code, message, offer_token_of,
    offers, reported, submit_use_case, withdraw, ENTITLED, OUTSTANDING,
};
use crate::support::{emulator, Emulator, Reply};

/// The two calls that MUTATE a model's agreement: the whole surface the account restriction
/// covers (`smithy_query(query="operation",operation="CreateFoundationModelAgreement,DeleteFoundationModelAgreement")`).
#[derive(Clone, Copy, Debug)]
enum Mutating {
    /// `CreateFoundationModelAgreement` — accepting the agreement of the model named.
    Accept,
    /// `DeleteFoundationModelAgreement` — withdrawing the acceptance the account holds.
    Withdraw,
}

impl Mutating {
    /// Every mutating call of the surface, so a row spans it rather than naming one of them.
    const ALL: [Self; 2] = [Self::Accept, Self::Withdraw];
}

/// Make the mutating call `call` for `model_id`, carrying `offer_token` where the call takes
/// one.
async fn mutate(
    emulator: &Emulator,
    call: Mutating,
    model_id: &str,
    offer_token: &str,
) -> Reply {
    match call {
        Mutating::Accept => accept(emulator, model_id, offer_token).await,
        Mutating::Withdraw => withdraw(emulator, model_id).await,
    }
}

/// The spellings of `model_id` this table drives: the bare id, and the `:`-qualified form
/// `BedrockModelId`'s pattern admits up to two qualifiers of — a variant of the same
/// published model, so the restriction covers it alike.
fn spellings(model_id: &str) -> Vec<String> {
    let mut rows = vec![model_id.to_string()];
    if model_id.matches(':').count() < 2 {
        rows.push(format!("{model_id}:24k"));
    }
    rows
}

/// The four availability members a refused mutating call must leave untouched, as
/// `GetFoundationModelAvailability` reports them
/// (`smithy_query(query="operation",operation="GetFoundationModelAvailability")`:
/// `agreementAvailability`, `authorizationStatus`, `entitlementAvailability`,
/// `regionAvailability`).
async fn standing_members(emulator: &Emulator, case: &str, model_id: &str) -> Vec<Value> {
    let reply = availability(emulator, model_id).await;
    assert_eq!(
        reply.status, 200,
        "case {case}: the standing of a catalogued model must be served, got {} {:?}",
        reply.status, reply.body
    );
    [
        "agreementAvailability",
        "authorizationStatus",
        "entitlementAvailability",
        "regionAvailability",
    ]
    .iter()
    .map(|member| reply.get(member))
    .collect()
}

/// Assert `reply` is the refusal the Agreement API answers an account that is not allowed to
/// call it: 403 `AccessDeniedException`, carrying the sentence that names THAT surface.
fn assert_not_allowed(case: &str, reply: &Reply) {
    assert_eq!(
        (reply.status, code(reply)),
        (403, "AccessDeniedException"),
        "case {case}: expected 403 AccessDeniedException, got {} {} {:?}",
        reply.status,
        code(reply),
        reply.body
    );
    let carried = message(case, reply);
    assert_eq!(
        carried,
        bedrock::state::AGREEMENT_API_NOT_ALLOWED_MESSAGE,
        "case {case}: the refusal must be this service's Agreement-API sentence, got \
         {carried:?}"
    );
    assert!(
        carried.to_lowercase().contains("agreement api"),
        "case {case}: the refusal must name the surface closed to the account, got {carried:?}"
    );
}

/// RULE: every call of the MUTATING Agreement API — accepting a model's agreement and
/// withdrawing it — made by an account that is not allowed to call that API is refused 403
/// `AccessDeniedException` naming that surface, for every agreement-bearing model and
/// whichever spelling of its id the request names, and the refusal records nothing.
#[tokio::test]
async fn every_mutating_agreement_call_is_refused_for_an_account_not_allowed_to_call_the_api() {
    let emulator = emulator();
    // NOTHING admits the account: no use case for model access is on record, so the service
    // holds no acknowledgement that would open the Agreement API to it.
    let models = agreement_models();
    assert!(
        models.len() > 1,
        "the catalog must carry models offered under an agreement, got {}",
        models.len()
    );

    for model in &models {
        // The token of a REAL offer of the model, so nothing about the request itself could
        // be what the call is refused for.
        let token = offer_token_of(&emulator, model.model_id).await;
        for spelling in spellings(model.model_id) {
            for call in Mutating::ALL {
                // Twice over: a refusal records nothing, so the second attempt is answered
                // exactly as the first rather than as a conflict or a missing agreement.
                for attempt in 1..=2 {
                    let case =
                        format!("{call:?} {spelling} while unadmitted, attempt {attempt}");
                    let reply = mutate(&emulator, call, &spelling, &token).await;
                    assert_not_allowed(&case, &reply);
                }
            }
        }
    }

    // None of those calls changed anything: every model's terms are still outstanding, and
    // the offer of each is still there to be accepted.
    for model in &models {
        let case = format!("{} after every refused mutating call", model.model_id);
        assert_standing(&emulator, &case, model.model_id, OUTSTANDING).await;
        assert!(
            !reported(&offers(&emulator, model.model_id, None).await).is_empty(),
            "case {case}: the offer of a model whose terms are unaccepted is still listed"
        );
    }
}

/// RULE: the restriction is read off the ACCOUNT's standing, not off the operation, so an
/// account Amazon Bedrock has admitted to the Agreement API is served the very same mutating
/// calls: the acceptance entitles it to the model and the withdrawal returns the model's terms
/// to outstanding.
#[tokio::test]
async fn an_account_admitted_to_the_agreement_api_is_served_the_same_mutating_calls() {
    let emulator = emulator();
    let models = agreement_models();
    // Two agreement-bearing models of DIFFERENT providers, read off the catalog: the
    // admission is account-level, so it opens the surface for both alike.
    let first = models[0];
    let second = models
        .iter()
        .find(|row| row.provider_name != first.provider_name)
        .expect("the catalog must carry agreement-bearing models of two providers");

    // Refused first, so each row witnesses the SAME request on both sides of the admission.
    for model in [first, *second] {
        let token = offer_token_of(&emulator, model.model_id).await;
        let case = format!("{} before the account is admitted", model.model_id);
        assert_not_allowed(&case, &accept(&emulator, model.model_id, &token).await);
    }

    submit_use_case(&emulator).await;

    for model in [first, *second] {
        let token = offer_token_of(&emulator, model.model_id).await;
        for spelling in spellings(model.model_id) {
            let case = format!("{spelling} once the account is admitted");

            let accepted = accept(&emulator, &spelling, &token).await;
            assert_eq!(
                accepted.status, 202,
                "case {case}: the acceptance must be served, got {} {:?}",
                accepted.status, accepted.body
            );
            assert_standing(&emulator, &case, model.model_id, ENTITLED).await;

            let withdrawn = withdraw(&emulator, &spelling).await;
            assert_eq!(
                withdrawn.status, 202,
                "case {case}: the withdrawal must be served, got {} {:?}",
                withdrawn.status, withdrawn.body
            );
            assert_standing(&emulator, &case, model.model_id, OUTSTANDING).await;
        }
    }
}

/// RULE: only the MUTATING half of the agreement surface is restricted, so an account that
/// may not call it still reads the surface: the offer of a model's agreement is reported
/// before and after a refused call, with the same `offerToken`, and every availability member
/// the model's standing is read by is reported unchanged.
#[tokio::test]
async fn the_reads_of_the_agreement_surface_are_served_while_the_mutating_calls_are_refused() {
    let emulator = emulator();

    for model in agreement_models() {
        let case = model.model_id.to_string();

        // The read side before the refused calls: the offer of the model's agreement, and
        // every member its standing is reported by.
        let listed = offers(&emulator, model.model_id, None).await;
        assert_eq!(
            listed.status, 200,
            "case {case}: the offers of an agreement-bearing model must be served, got {} {:?}",
            listed.status, listed.body
        );
        let before_offers = reported(&listed);
        assert!(
            !before_offers.is_empty(),
            "case {case}: an agreement-bearing model must report an outstanding offer, got {:?}",
            listed.body
        );
        let before_standing = standing_members(&emulator, &case, model.model_id).await;

        // Both refused mutating calls, each carrying the token the listing just handed out.
        let token = offer_token_of(&emulator, model.model_id).await;
        for call in Mutating::ALL {
            let refused = format!("{call:?} {case} while unadmitted");
            assert_not_allowed(
                &refused,
                &mutate(&emulator, call, model.model_id, &token).await,
            );
        }

        // The read side after them: the same offer, the same token, the same standing. The
        // refusal consumed no offer and moved no availability member.
        let after_offers = reported(&offers(&emulator, model.model_id, None).await);
        assert_eq!(
            after_offers, before_offers,
            "case {case}: the offers reported after a refused call must be the ones reported \
             before it, got {after_offers:?}"
        );
        let after_standing = standing_members(&emulator, &case, model.model_id).await;
        assert_eq!(
            after_standing, before_standing,
            "case {case}: every availability member must be unchanged by a refused call, got \
             {after_standing:?}"
        );
    }
}
