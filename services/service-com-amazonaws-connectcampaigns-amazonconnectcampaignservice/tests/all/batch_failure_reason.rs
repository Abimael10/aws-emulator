// SPDX-License-Identifier: CC0-1.0

//! The reason a batch run-state read gives for each id it could not report a state for.
//!
//! The rule under test: a per-id failure reported inside a batch read's 200 body EXPLAINS
//! ITSELF — beside the modelled failure code, every `failedRequests` entry carries the
//! reason in words, and that reason is the same sentence this service answers when a
//! single-id run-state read is addressed to that same absent campaign, composed from the
//! id the request carried in that entry's own position. So a batch of several unknown ids
//! carries as many distinct reasons, no entry borrows another's text, and an entry carries
//! those members and nothing else.
//!
//! Each rule is asserted over a TABLE whose rows differ along what it generalises over:
//! the form of the id, how many unknown ids one call carries, whether one of them repeats,
//! and whether resolved ids are interleaved with them.

use serde_json::{json, Value};

use super::common::{call, router, Answer, Harness};

/// The region every case here signs for.
const REGION: &str = "us-west-2";

/// The Connect instance the campaigns of the interleaved rows are created for.
const INSTANCE: &str = "failure-reason-instance";

/// The failure code a requested id that resolves to no campaign is reported with — one of
/// the two values `GetCampaignStateBatchFailureCode` carries
/// (`smithy_query(query="shape", shape="GetCampaignStateBatchFailureCode")`:
/// `ResourceNotFound | UnknownError`), written out here so this suite pins the wire value
/// rather than reading it back out of the implementation.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFound";

/// The members a `failedRequests` entry carries, in the order a JSON object's keys sort:
/// the two the model declares (`campaignId`, `failureCode`, from
/// `smithy_query(query="shape", shape="FailedCampaignStateResponse")`) and the reason
/// beside them.
const FAILED_ENTRY_MEMBERS: [&str; 3] = ["campaignId", "failureCode", "failureReason"];

/// `CampaignId`'s modelled `@length` maximum (`smithy_query(query="shape",
/// shape="CampaignId")` gives len 0..256 with pattern `^[\S]*$`).
const CAMPAIGN_ID_MAX_LENGTH: usize = 256;

/// The reason a requested id that names no campaign is reported with: the fixed text
/// `"Campaign: "` and `" doesn't exist"` around the id the request carried, which is the
/// only variable part of it. Written out here rather than read from the crate's own
/// helper, so a drift in that helper cannot keep these cases green.
fn not_found_reason(id: &str) -> String {
    format!("Campaign: {id} doesn't exist")
}

/// A minimal `CreateCampaign` body: every member the model marks required, and nothing
/// else.
fn create_body(name: &str) -> Value {
    json!({
        "name": name,
        "connectInstanceId": INSTANCE,
        "dialerConfig": {"progressiveDialerConfig": {"bandwidthAllocation": 0.5}},
        "outboundCallConfig": {"connectContactFlowId": "flow-1"},
    })
}

/// Create one campaign and hand back the id the service minted for it.
async fn a_campaign(router: &Harness, name: &str) -> String {
    router.register_connect_instance(REGION, INSTANCE);
    let created = call(router, REGION, "PUT", "/campaigns", &create_body(name)).await;
    assert_eq!(
        created.status, 200,
        "case \"create campaign {name}\": expected HTTP 200, got {} {}",
        created.status, created.body
    );
    created.body["id"]
        .as_str()
        .unwrap_or_else(|| panic!("case \"create campaign {name}\": no id in {}", created.body))
        .to_string()
}

/// One batch read.
async fn batch(router: &Harness, ids: &[String]) -> Answer {
    call(
        router,
        REGION,
        "POST",
        "/campaigns-state",
        &json!({"campaignIds": ids}),
    )
    .await
}

/// The single-id run-state read, for the same id.
async fn single(router: &Harness, id: &str) -> Answer {
    call(
        router,
        REGION,
        "GET",
        &format!("/campaigns/{id}/state"),
        &Value::Null,
    )
    .await
}

/// The elements of one response list, in order, or none when the answer carries no such
/// list at all.
fn rows(answer: &Answer, list: &str) -> Vec<Value> {
    answer
        .body
        .get(list)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// One string member of a response row, or a marker naming it absent — so an assertion
/// that fails reports WHICH member the entry is missing rather than panicking.
fn member(row: &Value, name: &str) -> String {
    row.get(name)
        .and_then(Value::as_str)
        .unwrap_or("<missing>")
        .to_string()
}

/// The `(campaignId, failureCode, failureReason)` triples a `failedRequests` list carries,
/// in the order it carries them.
fn failed(answer: &Answer) -> Vec<(String, String, String)> {
    rows(answer, "failedRequests")
        .iter()
        .map(|row| {
            (
                member(row, "campaignId"),
                member(row, "failureCode"),
                member(row, "failureReason"),
            )
        })
        .collect()
}

/// The `campaignId`s a `successfulRequests` list carries, in the order it carries them.
fn served(answer: &Answer) -> Vec<String> {
    rows(answer, "successfulRequests")
        .iter()
        .map(|row| member(row, "campaignId"))
        .collect()
}

/// Ids no campaign is held under, spanning `CampaignId` (len 0..256, pattern `^[\S]*$`):
/// digits, the uuid form, one character, the punctuation the pattern admits, mixed case,
/// and the longest id the bound allows.
fn unknown_id_forms() -> Vec<(&'static str, String)> {
    vec![
        ("digits", "000000000001".to_string()),
        (
            "the uuid form",
            "1b9c3f52-0ad1-4d7e-8c41-5e2f6a7b8c90".to_string(),
        ),
        ("one character", "q".to_string()),
        ("punctuation the pattern admits", "a:b-D9.e_f".to_string()),
        ("mixed case", "MixedCase9".to_string()),
        (
            "256 characters (the length maximum)",
            "z".repeat(CAMPAIGN_ID_MAX_LENGTH),
        ),
    ]
}

#[tokio::test]
async fn every_failed_entry_explains_itself_with_the_id_it_was_asked_about() {
    let router = router();
    let known = a_campaign(&router, "known").await;
    let at_max_length = "z".repeat(CAMPAIGN_ID_MAX_LENGTH);

    // Rows differ along the FORM of the unknown ids, HOW MANY of them one call carries,
    // whether one repeats, and whether a resolved id sits between them.
    let mut cases: Vec<(String, Vec<String>)> = unknown_id_forms()
        .into_iter()
        .map(|(case, id)| (format!("one unknown id: {case}"), vec![id]))
        .collect();
    cases.push((
        "several unknown ids of different forms, so no two reasons can read alike".to_string(),
        vec![
            "AAA-111".to_string(),
            "b/c:d".to_string(),
            "MixedCase9".to_string(),
            at_max_length,
        ],
    ));
    cases.push((
        "one unknown id repeated beside another".to_string(),
        vec![
            "222222222222".to_string(),
            "222222222222".to_string(),
            "333333333333".to_string(),
        ],
    ));
    cases.push((
        "unknown ids with the stored campaign between them".to_string(),
        vec![
            "before-the-known-one".to_string(),
            known.clone(),
            "after-the-known-one".to_string(),
        ],
    ));

    for (case, sent) in cases {
        let answer = batch(&router, &sent).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: an unresolved id is a per-id failure, not a refusal of the \
             batch, got {} {}",
            answer.status, answer.body
        );

        // Every id the store holds no campaign under, in the order it was asked for, with
        // the not-found code and the reason composed from that entry's OWN id.
        let expected: Vec<(String, String, String)> = sent
            .iter()
            .filter(|id| **id != known)
            .map(|id| {
                (
                    id.clone(),
                    RESOURCE_NOT_FOUND.to_string(),
                    not_found_reason(id),
                )
            })
            .collect();
        assert_eq!(
            failed(&answer),
            expected,
            "case {case:?}: expected each entry to carry its own id, the not-found code \
             and the reason about that id, got {:?}",
            failed(&answer)
        );

        // A resolved id is still served beside the failures, and carries no reason of its
        // own: the reason belongs to a failure.
        let expected_served: Vec<String> = sent.iter().filter(|id| **id == known).cloned().collect();
        assert_eq!(
            served(&answer),
            expected_served,
            "case {case:?}: expected the resolved ids in request order, got {}",
            answer.body
        );
        for (index, row) in rows(&answer, "successfulRequests").iter().enumerate() {
            assert!(
                row.get("failureReason").is_none(),
                "case {case:?}: successful entry {index} must carry no failureReason, got \
                 {row}"
            );
        }

        // A failed entry carries those three members and nothing else: the reason is a
        // member of the entry, not a second list beside it.
        for (index, row) in rows(&answer, "failedRequests").iter().enumerate() {
            let mut carried: Vec<String> = row
                .as_object()
                .map(|object| object.keys().cloned().collect())
                .unwrap_or_default();
            carried.sort();
            assert_eq!(
                carried, FAILED_ENTRY_MEMBERS,
                "case {case:?}: entry {index} carried {carried:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_batch_reason_is_the_sentence_the_single_id_read_answers_with() {
    let router = router();

    // The rule is the SERVICE's wording for an absent campaign, not this operation's: the
    // reason a batch entry carries and the message the single-id run-state read refuses
    // with are one sentence. Only ids that are a single path segment are asked both ways,
    // since the single-id read carries the id in its URI.
    let ids: Vec<(&str, String)> = unknown_id_forms()
        .into_iter()
        .filter(|(_, id)| !id.contains('/'))
        .collect();

    for (case, id) in &ids {
        let answer = batch(&router, std::slice::from_ref(id)).await;
        assert_eq!(
            failed(&answer),
            vec![(
                id.clone(),
                RESOURCE_NOT_FOUND.to_string(),
                not_found_reason(id)
            )],
            "case {case:?}: expected the batch to explain the absent id, got {}",
            answer.body
        );

        let refused = single(&router, id).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected the single-id read to refuse an absent campaign, got \
             {} {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.message(),
            not_found_reason(id),
            "case {case:?}: expected the single-id read to answer the same sentence, got {}",
            refused.body
        );
    }

    // And the batch read wrote nothing while explaining the misses: the same batch asked
    // again answers the same reasons.
    let all: Vec<String> = ids.iter().map(|(_, id)| id.clone()).collect();
    let first = batch(&router, &all).await;
    let again = batch(&router, &all).await;
    assert_eq!(
        failed(&again),
        failed(&first),
        "case \"the same batch asked twice\": a read must not change what a later caller \
         sees, got {} then {}",
        first.body,
        again.body
    );
}
