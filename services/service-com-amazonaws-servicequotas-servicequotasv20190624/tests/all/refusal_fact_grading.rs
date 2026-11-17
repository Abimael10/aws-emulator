// SPDX-License-Identifier: CC0-1.0

//! The refusal facts Service Quotas can state, told APART from each other, over the wire.
//!
//! Every call here is a real `POST /` with `X-Amz-Target:
//! ServiceQuotasV20190624.<Op>` dispatched through the same `MultiServiceRouter` the
//! emulator serves with, over ONE shared engine. The harness (`emulator`, `client`,
//! `Client`) is `quota_workflow.rs`'s, the catalog tables are `no_such_quota_wording.rs`'s
//! (`catalog_services`, `unheld_pairs`) and `no_such_service_wording.rs`'s
//! (`unpublished_service_codes`), the adjustable-quota picker is
//! `refusal_message_contract.rs`'s and the refusal reader is `auto_management.rs`'s.
//!
//! The rule asserted, ACROSS the facts rather than within one of them — each fact's own
//! inputs are spanned by `no_such_service_wording.rs`, `no_such_quota_wording.rs`,
//! `refusal_wording.rs` and `refusal_message_contract.rs`:
//!
//! * a refusal states the service's own sentence for the FACT it reports, so every
//!   operation that can report a fact answers one string and no two facts share one — a
//!   handler can neither invent a sentence for a fact nor report one fact in another's
//!   words. The only sentence with a variable part is the refusal of a quota value below
//!   the quota's AWS default, and that part is the default of the quota the request named,
//!   read back here as a number;
//! * which fact is reported follows what the request named and which half of it was
//!   missing, not which operation was called — including on the one listing that is scoped
//!   to a `ServiceCode` AND names a `QuotaCode` under it, which states the service fact
//!   for a missing service and the failed pair lookup for a missing quota.

use serde_json::{json, Value};

use service_com_amazonaws_servicequotas_servicequotasv20190624::service::refusals::{
    below_default_value_message, NO_SUCH_QUOTA_REQUEST_MESSAGE, QUOTA_AND_SERVICE_ABSENT_MESSAGE,
    SERVICE_NOT_AVAILABLE_MESSAGE,
};

use crate::auto_management::refused as answer;
use crate::no_such_quota_wording::{catalog_services, unheld_pairs};
use crate::no_such_service_wording::unpublished_service_codes;
use crate::quota_workflow::{client, emulator, Client};
use crate::refusal_message_contract::quotas_with_adjustable;

/// Every modelled client fault is HTTP 400 on this awsJson service, whatever
/// `@httpError` its shape declares — see the `error_envelope` suite.
const CLIENT_FAULT: u16 = 400;

/// The modelled error a missing service, a missing quota and a missing quota increase
/// request are all reported as.
const MISSING_RESOURCE: &str = "NoSuchResourceException";

/// The modelled error a quota value the service will not accept is reported as.
const BAD_ARGUMENT: &str = "IllegalArgumentException";

/// The account-wide change history, whose name is too long to repeat in every row.
const HISTORY: &str = "ListRequestedServiceQuotaChangeHistory";

/// The change history of ONE quota: the listing that is scoped to a `ServiceCode` and
/// names a `QuotaCode` under it.
const HISTORY_BY_QUOTA: &str = "ListRequestedServiceQuotaChangeHistoryByQuota";

/// The sentence a call scoped to a service the endpoint serves no quotas for is refused
/// with, down to the trailing space the service sends it with. Spelled here so the suite
/// pins the WORDING as well as its single sourcing: comparing a refusal against the
/// crate's constant alone would stay green if that constant's own sentence drifted.
const SERVICE_WORDING: &str =
    "This service is not available in the current Region. Choose a different Region or a \
     different service. ";

/// The sentence a call that resolves ONE quota by its pair is refused with when the pair
/// resolves to no quota, spelled here for the same reason.
const PAIR_WORDING: &str =
    "The request failed because the specified quota and service do not exist.";

/// The sentence a call that resolves one quota increase request by its `RequestId` is
/// refused with when no request answers to it, spelled here for the same reason.
const REQUEST_WORDING: &str = "The request failed because the specified requestId to increase \
                               the service quota does not exist.";

/// The fixed half of the refusal of a quota value below the quota's AWS default: the
/// sentence ends in that default.
const BELOW_DEFAULT: &str = "You must provide a quota value greater than the default quota \
                             value of ";

/// The modelled error code and `Message` of the refusal `operation` answers `body` with,
/// after asserting this front end's client-fault status.
async fn refusal(caller: &Client, case: &str, operation: &str, body: &Value) -> (String, String) {
    let (status, code, message) = answer(caller, operation, body).await;
    assert_eq!(
        status, CLIENT_FAULT,
        "case {case}: expected HTTP {CLIENT_FAULT}, got {status} {code} with {message:?}"
    );
    (code, message)
}

/// Each fact this service refuses with is stated in its OWN sentence: one sentence per
/// fact, whichever operation reported it and whatever the request carried, and no two
/// facts share one.
#[tokio::test]
async fn each_refusal_fact_has_one_sentence_of_its_own() {
    let engine = emulator();
    let caller = client(&engine, "us-east-1");

    // A pair the catalog really publishes, read over the wire: a row that carries a
    // QuotaCode must be refused for the half under test and not for a code that exists
    // nowhere.
    let (_, published_quota) = catalog_services(&caller, 1)
        .await
        .into_iter()
        .next()
        .expect("case setup: the catalog publishes at least one quota");
    let unpublished = unpublished_service_codes(&caller).await;

    // FACT: the call is scoped to a service this endpoint serves no quotas for — reported
    // by the listing keyed by a service, by the account-wide history keyed by one, and by
    // the listing that names a quota under it.
    let mut service_fact: Vec<String> = Vec::new();
    for (why, service_code) in &unpublished {
        for (operation, body) in [
            ("ListServiceQuotas", json!({"ServiceCode": service_code})),
            (HISTORY, json!({"ServiceCode": service_code})),
            (
                HISTORY_BY_QUOTA,
                json!({"ServiceCode": service_code, "QuotaCode": published_quota}),
            ),
        ] {
            let case = format!("{why}, scoped to the service through {operation}");
            let (code, message) = refusal(&caller, &case, operation, &body).await;
            assert_eq!(
                code, MISSING_RESOURCE,
                "case {case}: expected {MISSING_RESOURCE}, got {code} with {message:?}"
            );
            assert_eq!(
                message, SERVICE_WORDING,
                "case {case}: expected the sentence {SERVICE_WORDING:?}, got {message:?}"
            );
            assert_eq!(
                message, SERVICE_NOT_AVAILABLE_MESSAGE,
                "case {case}: expected the crate's one sentence for the fact, got {message:?}"
            );
            // The trailing space is part of the service's own text.
            assert!(
                message.ends_with("different service. "),
                "case {case}: expected the sentence to keep its trailing space, got {message:?}"
            );
            assert!(
                !message.contains(service_code.as_str())
                    && !message.contains(published_quota.as_str()),
                "case {case}: expected a refusal naming neither the ServiceCode nor the \
                 QuotaCode of the request, got {message:?}"
            );
            service_fact.push(message);
        }
    }

    // FACT: the call resolves ONE quota by its pair and the pair resolves to nothing. The
    // rows span both halves: the shared table of pairs a published service does not hold,
    // and the unpublished service codes named with a quota code the catalog does publish.
    let mut pairs = unheld_pairs(&caller).await;
    pairs.extend(unpublished.iter().map(|(why, service_code)| {
        (
            format!("{why}, with a QuotaCode the catalog publishes"),
            service_code.clone(),
            published_quota.clone(),
        )
    }));

    let mut pair_fact: Vec<String> = Vec::new();
    for (why, service_code, quota_code) in &pairs {
        for (operation, body) in [
            (
                "GetServiceQuota",
                json!({"ServiceCode": service_code, "QuotaCode": quota_code}),
            ),
            (
                "RequestServiceQuotaIncrease",
                json!({
                    "ServiceCode": service_code,
                    "QuotaCode": quota_code,
                    "DesiredValue": 10.0,
                }),
            ),
        ] {
            let case = format!("{why}, resolving one quota through {operation}");
            let (code, message) = refusal(&caller, &case, operation, &body).await;
            assert_eq!(
                code, MISSING_RESOURCE,
                "case {case}: expected {MISSING_RESOURCE}, got {code} with {message:?}"
            );
            assert_eq!(
                message, PAIR_WORDING,
                "case {case}: expected the sentence {PAIR_WORDING:?}, got {message:?}"
            );
            assert_eq!(
                message, QUOTA_AND_SERVICE_ABSENT_MESSAGE,
                "case {case}: expected the crate's one sentence for the fact, got {message:?}"
            );
            assert!(
                !message.contains(service_code.as_str()) && !message.contains(quota_code.as_str()),
                "case {case}: expected a refusal naming neither half of the pair, got {message:?}"
            );
            pair_fact.push(message);
        }
    }

    // FACT: no quota increase request answers to the `RequestId` the call named — reported
    // by both operations that resolve one. The ids differ in form while satisfying the
    // member's own `@pattern ^[0-9a-zA-Z][a-zA-Z0-9-]{1,128}$` (`refusal_wording.rs` spans
    // this member's bounds; what is needed here is the fact, from both its reporters).
    let mut request_fact: Vec<String> = Vec::new();
    for (why, request_id) in [
        ("a word nothing was minted for", "norequestansweringtothisid"),
        ("an id that starts with a digit", "4-0-4"),
    ] {
        for operation in ["GetRequestedServiceQuotaChange", "CreateSupportCase"] {
            let case = format!("{why} ({request_id}), through {operation}");
            let (code, message) =
                refusal(&caller, &case, operation, &json!({"RequestId": request_id})).await;
            assert_eq!(
                code, MISSING_RESOURCE,
                "case {case}: expected {MISSING_RESOURCE}, got {code} with {message:?}"
            );
            assert_eq!(
                message, REQUEST_WORDING,
                "case {case}: expected the sentence {REQUEST_WORDING:?}, got {message:?}"
            );
            assert_eq!(
                message, NO_SUCH_QUOTA_REQUEST_MESSAGE,
                "case {case}: expected the crate's one sentence for the fact, got {message:?}"
            );
            assert!(
                !message.contains(request_id),
                "case {case}: expected the refusal not to echo the RequestId, got {message:?}"
            );
            request_fact.push(message);
        }
    }

    // FACT: the value asked for is below the quota's AWS default. This is the one sentence
    // with a variable part, and that part is the DEFAULT of the quota the request named —
    // so the tail of the sentence is read back as a number and compared with that default,
    // rather than re-rendered here.
    let quotas: Vec<(String, String, f64)> = quotas_with_adjustable(&caller, true, 2)
        .await
        .into_iter()
        // A quota whose AWS default is 0 has no value below it, so it cannot carry a row
        // for this fact at all.
        .filter(|(_, _, default_value)| *default_value > 0.0)
        .collect();
    assert!(
        !quotas.is_empty(),
        "case setup: expected an adjustable quota with a default above zero"
    );
    let mut below_default: Vec<String> = Vec::new();
    for (service_code, quota_code, default_value) in &quotas {
        // The values differ in where they sit below the default: its bottom bound and half
        // of it.
        for (why, desired) in [
            ("the bottom of the member's range", 0.0),
            ("half the default", default_value / 2.0),
        ] {
            let case = format!("{service_code}/{quota_code}, {why} ({desired})");
            let (code, message) = refusal(
                &caller,
                &case,
                "RequestServiceQuotaIncrease",
                &json!({
                    "ServiceCode": service_code,
                    "QuotaCode": quota_code,
                    "DesiredValue": desired,
                }),
            )
            .await;
            assert_eq!(
                code, BAD_ARGUMENT,
                "case {case}: expected {BAD_ARGUMENT}, got {code} with {message:?}"
            );
            let tail = message.strip_prefix(BELOW_DEFAULT).unwrap_or_else(|| {
                panic!("case {case}: expected a sentence opening {BELOW_DEFAULT:?}, got {message:?}")
            });
            // The variable part is the named quota's own default, rendered as a decimal.
            assert_eq!(
                tail.parse::<f64>().ok(),
                Some(*default_value),
                "case {case}: expected the sentence to end in the quota's default value \
                 {default_value}, got {tail:?}"
            );
            assert!(
                tail.contains('.'),
                "case {case}: expected the default rendered as a decimal, got {tail:?}"
            );
            assert_eq!(
                message,
                below_default_value_message(*default_value),
                "case {case}: expected the crate's one sentence for the fact, got {message:?}"
            );
            assert!(
                !message.contains(service_code.as_str()) && !message.contains(quota_code.as_str()),
                "case {case}: expected a refusal naming neither the service nor the quota, got \
                 {message:?}"
            );
            below_default.push(message);
        }
    }

    // One sentence per fixed fact, however many rows and operations reported it.
    for (fact, stated) in [
        (
            "a service this endpoint serves no quotas for",
            &service_fact,
        ),
        ("a pair resolving to no quota", &pair_fact),
        ("an unresolvable RequestId", &request_fact),
    ] {
        let mut distinct = stated.clone();
        distinct.sort();
        distinct.dedup();
        assert!(
            !stated.is_empty() && distinct.len() == 1,
            "case the table for {fact}: expected one sentence over {} calls, got {distinct:?}",
            stated.len()
        );
    }
    // And the quota-value fact: exactly one sentence per DISTINCT default among the quotas
    // driven, so the variable part follows the quota and not the value asked for.
    let mut distinct_defaults: Vec<String> = quotas
        .iter()
        .map(|(_, _, default_value)| format!("{default_value}"))
        .collect();
    distinct_defaults.sort();
    distinct_defaults.dedup();
    let mut distinct_sentences = below_default.clone();
    distinct_sentences.sort();
    distinct_sentences.dedup();
    assert_eq!(
        distinct_sentences.len(),
        distinct_defaults.len(),
        "case the table for a value below the default: expected one sentence per distinct \
         default {distinct_defaults:?}, got {distinct_sentences:?}"
    );

    // The facts are told apart: no two of them share a sentence.
    let facts = [
        (
            "a service this endpoint serves no quotas for",
            &service_fact[0],
        ),
        ("a pair resolving to no quota", &pair_fact[0]),
        ("an unresolvable RequestId", &request_fact[0]),
        ("a value below the quota's default", &below_default[0]),
    ];
    for (index, (fact, sentence)) in facts.iter().enumerate() {
        assert!(
            !sentence.is_empty(),
            "case {fact}: expected a non-empty sentence, got {sentence:?}"
        );
        for (other_fact, other) in facts.iter().skip(index + 1) {
            assert_ne!(
                sentence, other,
                "case {fact} against {other_fact}: expected two facts to be stated in two \
                 sentences, got {sentence:?} for both"
            );
        }
    }
}

/// Which fact is reported follows WHICH HALF of the request was missing, not which
/// operation was called: ONE listing — the change history of one quota, which is scoped to
/// a service AND names a quota under it — reports the service fact when the service is the
/// missing half and the failed pair lookup when the quota is, while the read that resolves
/// only a pair reports the pair fact for the very same request.
#[tokio::test]
async fn which_fact_is_reported_follows_the_missing_half_of_the_request() {
    let engine = emulator();
    let caller = client(&engine, "eu-west-1");

    let (published_service, published_quota) = catalog_services(&caller, 1)
        .await
        .into_iter()
        .next()
        .expect("case setup: the catalog publishes at least one quota");
    let (why, unpublished_service) = unpublished_service_codes(&caller)
        .await
        .into_iter()
        .next()
        .expect("case setup: a code the catalog does not publish");

    // The missing half is the SERVICE. The listing keyed by it reports the service fact —
    // before it considers the quota code it was given, which is one the catalog publishes
    // under that other service — while the read that resolves only the pair reports the
    // failed pair lookup for the same request.
    let case = format!("{why}, through the listing of one quota's history");
    let (_, scoped) = refusal(
        &caller,
        &case,
        HISTORY_BY_QUOTA,
        &json!({"ServiceCode": &unpublished_service, "QuotaCode": &published_quota}),
    )
    .await;
    assert_eq!(
        scoped, SERVICE_NOT_AVAILABLE_MESSAGE,
        "case {case}: expected the service fact for a missing service, got {scoped:?}"
    );

    let single_read = format!("{why}, through the single read of one quota");
    let (_, resolved) = refusal(
        &caller,
        &single_read,
        "GetServiceQuota",
        &json!({"ServiceCode": &unpublished_service, "QuotaCode": &published_quota}),
    )
    .await;
    assert_eq!(
        resolved, QUOTA_AND_SERVICE_ABSENT_MESSAGE,
        "case {single_read}: expected the pair fact for a call that resolves one quota, got \
         {resolved:?}"
    );
    assert_ne!(
        scoped, resolved,
        "case {why}: expected the two classes of call to report two different facts, got \
         {scoped:?} for both"
    );

    // The missing half is the QUOTA, under a service this endpoint does serve: the SAME
    // listing now reports the failed pair lookup, so the fact is the request's and not the
    // operation's. The pair comes from the shared table of pairs the catalog does not hold.
    let (unheld_why, unheld_service, unheld_quota) = unheld_pairs(&caller)
        .await
        .into_iter()
        .next()
        .expect("case setup: a pair the catalog does not hold");
    let case = format!("{unheld_why}, through the listing of one quota's history");
    let (code, message) = refusal(
        &caller,
        &case,
        HISTORY_BY_QUOTA,
        &json!({"ServiceCode": &unheld_service, "QuotaCode": &unheld_quota}),
    )
    .await;
    assert_eq!(
        (code.as_str(), message.as_str()),
        (MISSING_RESOURCE, QUOTA_AND_SERVICE_ABSENT_MESSAGE),
        "case {case}: expected {MISSING_RESOURCE} stating the pair fact, got {code} with \
         {message:?}"
    );

    // And neither half is missing: the same listing is served, so the refusals above report
    // what the request named rather than a lookup that fails for everything.
    let (status, parsed) = caller
        .call(
            HISTORY_BY_QUOTA,
            &json!({"ServiceCode": &published_service, "QuotaCode": &published_quota}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {published_service}/{published_quota}, through the listing of one quota's \
         history: expected HTTP 200, got {status} with {parsed}"
    );
}
