// SPDX-License-Identifier: CC0-1.0

//! The tracker ↔ geofence collection association plane, end to end over the wire.
//!
//! Every call goes through the shared harness (`crate::harness`), so each one is a
//! separate HTTP request against one shared engine: a consumer this suite reads back
//! survived because the association was PERSISTED on the tracker, not because a
//! handler echoed its own input back.
//!
//! The tables span the classes the rules generalise over: how many collections one
//! tracker is associated with (none, one, several, the documented quota and one past
//! it), the SCOPE fields an equivalent `ConsumerArn` may be written with (the minted
//! ARN, another Region, an account-less ARN, the `type:id` resource form), the page
//! sizes a paginated read admits, and the trackers an association is NOT visible on.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_location_locationservice::state::MAX_CONSUMERS_PER_TRACKER;

// The one HTTP harness this crate's suites share, declared from `tests/all.rs`.
use crate::harness::{call, emulator, Answer, ACCOUNT, REGION};

/// The sentence a `ConsumerArn` that names no geofence collection of this service is
/// refused with, as `crate::state::not_a_geofence_collection_arn` words it.
const NOT_A_COLLECTION_ARN: &str =
    "'ConsumerArn' must be the ARN of an Amazon Location geofence collection.";

async fn create_tracker(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "POST",
        "/tracking/v0/trackers",
        Some(json!({ "TrackerName": name })),
    )
    .await
}

/// Create a geofence collection and answer the ARN the service minted for it — the
/// value an association is expected to report, taken from the create's own response
/// rather than written by hand here.
async fn create_collection(router: &MultiServiceRouter, name: &str) -> String {
    let answer = call(
        router,
        "POST",
        "/geofencing/v0/collections",
        Some(json!({ "CollectionName": name })),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "creating collection {name} must succeed, got {} {}",
        answer.status, answer.body
    );
    answer
        .json()
        .get("CollectionArn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("CollectionArn must be present in {}", answer.body))
        .to_string()
}

async fn associate(router: &MultiServiceRouter, tracker: &str, consumer_arn: &str) -> Answer {
    call(
        router,
        "POST",
        &format!("/tracking/v0/trackers/{tracker}/consumers"),
        Some(json!({ "ConsumerArn": consumer_arn })),
    )
    .await
}

async fn list_consumers(router: &MultiServiceRouter, tracker: &str, body: Value) -> Answer {
    call(
        router,
        "POST",
        &format!("/tracking/v0/trackers/{tracker}/list-consumers"),
        Some(body),
    )
    .await
}

/// `DisassociateTrackerConsumer` binds the ARN as a URI LABEL, which is ONE path
/// segment, so the ARN's `:` and `/` have to travel as `%3A` and `%2F` or the request
/// addresses a different route.
async fn disassociate(router: &MultiServiceRouter, tracker: &str, consumer_arn: &str) -> Answer {
    let label = consumer_arn.replace(':', "%3A").replace('/', "%2F");
    call(
        router,
        "DELETE",
        &format!("/tracking/v0/trackers/{tracker}/consumers/{label}"),
        None,
    )
    .await
}

/// The `ConsumerArns` a list answer reports.
fn reported_consumers(answer: &Answer) -> Vec<String> {
    answer
        .json()
        .get("ConsumerArns")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("ConsumerArns must be present in {}", answer.body))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("a consumer ARN must be a string, got {value}"))
                .to_string()
        })
        .collect()
}

/// The `NextToken` a list answer reports, if any.
fn reported_next_token(answer: &Answer) -> Option<String> {
    answer
        .json()
        .get("NextToken")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Every consumer of `tracker`, read page by page with `max_results` per page.
///
/// The pages are walked until the service reports no further token, so the union is
/// what the read answers rather than what one page happened to hold.
async fn all_consumers(
    router: &MultiServiceRouter,
    tracker: &str,
    max_results: Option<i64>,
) -> Vec<String> {
    let mut collected = Vec::new();
    let mut token: Option<String> = None;
    // One more iteration than the quota allows pages for: a token that never clears
    // is a defect, not a reason to spin.
    for _ in 0..=MAX_CONSUMERS_PER_TRACKER + 1 {
        let mut body = json!({});
        if let Some(limit) = max_results {
            body["MaxResults"] = json!(limit);
        }
        if let Some(cursor) = &token {
            body["NextToken"] = json!(cursor);
        }
        let answer = list_consumers(router, tracker, body).await;
        assert_eq!(
            answer.status, 200,
            "case max_results={max_results:?}: listing {tracker} must succeed, got {} {}",
            answer.status, answer.body
        );
        let page = reported_consumers(&answer);
        if let Some(limit) = max_results {
            assert!(
                page.len() as i64 <= limit,
                "case max_results={limit}: a page must hold at most {limit} consumers, got {}",
                page.len()
            );
        }
        collected.extend(page);
        token = reported_next_token(&answer);
        if token.is_none() {
            return collected;
        }
    }
    panic!("case max_results={max_results:?}: paging {tracker} never reported a last page");
}

// ---------------------------------------------------------------------------
// Associate → List round trip
// ---------------------------------------------------------------------------

/// One row of the ARN-scope table: an equivalent spelling of the SAME collection's
/// ARN, as a caller may address it.
struct ScopeCase {
    case: &'static str,
    /// The ARN the request carries, built from the collection's name.
    written: fn(&str) -> String,
}

/// The ARN-scope table.
///
/// Every row names the same geofence collection: the ARN fields that differ carry no
/// resource identity (an ARN's `region` and `account` fields may be empty, and the
/// resource field is written `type/id` or `type:id`), so each row must associate the
/// one collection the name resolves to.
fn scope_cases() -> Vec<ScopeCase> {
    vec![
        ScopeCase {
            case: "the ARN the create minted",
            written: |name| format!("arn:aws:geo:{REGION}:{ACCOUNT}:geofence-collection/{name}"),
        },
        ScopeCase {
            case: "another Region in the ARN's Region field",
            written: |name| format!("arn:aws:geo:eu-west-1:{ACCOUNT}:geofence-collection/{name}"),
        },
        ScopeCase {
            case: "an ARN with empty Region and account fields",
            written: |name| format!("arn:aws:geo:::geofence-collection/{name}"),
        },
        ScopeCase {
            case: "the type:id resource form",
            written: |name| format!("arn:aws:geo:{REGION}:{ACCOUNT}:geofence-collection:{name}"),
        },
        ScopeCase {
            case: "another partition",
            written: |name| {
                format!("arn:aws-us-gov:geo:{REGION}:{ACCOUNT}:geofence-collection/{name}")
            },
        },
    ]
}

#[tokio::test]
async fn an_association_is_reported_by_the_tracker_it_was_made_on() {
    let router = emulator();
    create_tracker(&router, "holder").await;
    create_tracker(&router, "bystander").await;

    for (index, case) in scope_cases().into_iter().enumerate() {
        let collection = format!("scope-{index}");
        let minted = create_collection(&router, &collection).await;
        let written = (case.written)(&collection);

        let answer = associate(&router, "holder", &written).await;
        assert_eq!(
            answer.status, 200,
            "case {}: associating {written} must succeed, got {} {}",
            case.case, answer.status, answer.body
        );

        let reported = all_consumers(&router, "holder", None).await;
        assert!(
            reported.contains(&minted),
            "case {}: the tracker must report {minted}, got {reported:?}",
            case.case
        );
        // The stored association is the COLLECTION's ARN, so an equivalent spelling
        // adds nothing: the tracker holds each consumer once.
        assert_eq!(
            reported.iter().filter(|arn| **arn == minted).count(),
            1,
            "case {}: {minted} must be reported once, got {reported:?}",
            case.case
        );

        // The association belongs to the tracker it was made on.
        let bystander = all_consumers(&router, "bystander", None).await;
        assert!(
            bystander.is_empty(),
            "case {}: a tracker with no association of its own must report none, got {bystander:?}",
            case.case
        );
    }

    // Re-associating a collection the tracker already holds answers like the first
    // call and leaves the set unchanged.
    let before = all_consumers(&router, "holder", None).await;
    let repeat = format!("arn:aws:geo:{REGION}:{ACCOUNT}:geofence-collection:scope-0");
    let answer = associate(&router, "holder", &repeat).await;
    assert_eq!(
        answer.status, 200,
        "case repeat: re-associating {repeat} must succeed, got {} {}",
        answer.status, answer.body
    );
    let after = all_consumers(&router, "holder", None).await;
    assert_eq!(
        after, before,
        "case repeat: the consumer set must be unchanged, got {after:?}"
    );
}

#[tokio::test]
async fn a_tracker_with_no_association_reports_an_empty_list() {
    let router = emulator();
    create_tracker(&router, "fresh").await;

    let answer = list_consumers(&router, "fresh", json!({})).await;
    assert_eq!(
        answer.status, 200,
        "case fresh: listing must succeed, got {} {}",
        answer.status, answer.body
    );
    let reported = reported_consumers(&answer);
    assert!(
        reported.is_empty(),
        "case fresh: a tracker with no consumer must report an empty list, got {reported:?}"
    );
    let token = reported_next_token(&answer);
    assert!(
        token.is_none(),
        "case fresh: a last page must carry no NextToken, got {token:?}"
    );
}

#[tokio::test]
async fn each_tracker_reports_only_its_own_consumers() {
    let router = emulator();
    create_tracker(&router, "alpha").await;
    create_tracker(&router, "beta").await;

    let mut alpha_expected = Vec::new();
    for index in 0..3 {
        let arn = create_collection(&router, &format!("alpha-{index}")).await;
        let answer = associate(&router, "alpha", &arn).await;
        assert_eq!(
            answer.status, 200,
            "case alpha-{index}: associating must succeed, got {} {}",
            answer.status, answer.body
        );
        alpha_expected.push(arn);
    }
    let beta_arn = create_collection(&router, "beta-only").await;
    let answer = associate(&router, "beta", &beta_arn).await;
    assert_eq!(
        answer.status, 200,
        "case beta-only: associating must succeed, got {} {}",
        answer.status, answer.body
    );

    alpha_expected.sort();
    let alpha_reported = all_consumers(&router, "alpha", None).await;
    assert_eq!(
        alpha_reported, alpha_expected,
        "case alpha: the tracker must report its own three consumers in ARN order, got {alpha_reported:?}"
    );
    let beta_reported = all_consumers(&router, "beta", None).await;
    assert_eq!(
        beta_reported,
        vec![beta_arn],
        "case beta: the tracker must report only its own consumer, got {beta_reported:?}"
    );
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_page_size_walks_the_whole_consumer_set() {
    let router = emulator();
    create_tracker(&router, "paged").await;

    let mut expected = Vec::new();
    for index in 0..MAX_CONSUMERS_PER_TRACKER {
        let arn = create_collection(&router, &format!("paged-{index}")).await;
        let answer = associate(&router, "paged", &arn).await;
        assert_eq!(
            answer.status, 200,
            "case paged-{index}: associating must succeed, got {} {}",
            answer.status, answer.body
        );
        expected.push(arn);
    }
    expected.sort();

    // Each page size from one per page up to one page for everything, plus the
    // model's own `MaxResults` ceiling (`range 1..100`) and no `MaxResults` at all.
    let mut sizes: Vec<Option<i64>> = (1..=MAX_CONSUMERS_PER_TRACKER as i64).map(Some).collect();
    sizes.push(Some(100));
    sizes.push(None);
    for size in sizes {
        let reported = all_consumers(&router, "paged", size).await;
        assert_eq!(
            reported, expected,
            "case max_results={size:?}: paging must report every consumer once, in ARN order, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_cursor_this_read_did_not_mint_is_refused() {
    let router = emulator();
    create_tracker(&router, "cursors").await;
    let arn = create_collection(&router, "cursors-only").await;
    associate(&router, "cursors", &arn).await;

    // A client-invented string, a cursor of another list operation, and one naming no
    // page at all.
    for token in ["not-a-cursor", "trackers:holder", "tracker-consumers:"] {
        let answer = list_consumers(&router, "cursors", json!({ "NextToken": token })).await;
        assert_eq!(
            answer.status, 400,
            "case {token:?}: a foreign cursor must be refused, got {} {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            "ValidationException",
            "case {token:?}: the refusal must be a ValidationException, got {}",
            answer.body
        );
        assert!(
            answer.message().contains("NextToken"),
            "case {token:?}: the refusal must name NextToken, got {:?}",
            answer.message()
        );
    }
}

// ---------------------------------------------------------------------------
// Disassociate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disassociating_removes_exactly_the_named_collection() {
    let router = emulator();
    create_tracker(&router, "unlink").await;

    let mut arns = Vec::new();
    for index in 0..3 {
        let arn = create_collection(&router, &format!("unlink-{index}")).await;
        associate(&router, "unlink", &arn).await;
        arns.push(arn);
    }

    // Addressed by an equivalent spelling of the collection's ARN: the association is
    // held against the collection, not against one request's text.
    let removed = "arn:aws:geo:::geofence-collection/unlink-1".to_string();
    let answer = disassociate(&router, "unlink", &removed).await;
    assert_eq!(
        answer.status, 200,
        "case unlink-1: disassociating must succeed, got {} {}",
        answer.status, answer.body
    );

    let mut expected = vec![arns[0].clone(), arns[2].clone()];
    expected.sort();
    let reported = all_consumers(&router, "unlink", None).await;
    assert_eq!(
        reported, expected,
        "case unlink-1: only the named collection must be removed, got {reported:?}"
    );

    // Unlinking what is already unlinked answers exactly like the first call.
    let answer = disassociate(&router, "unlink", &arns[1]).await;
    assert_eq!(
        answer.status, 200,
        "case unlink-1 repeated: disassociating again must succeed, got {} {}",
        answer.status, answer.body
    );
    let again = all_consumers(&router, "unlink", None).await;
    assert_eq!(
        again, expected,
        "case unlink-1 repeated: the consumer set must be unchanged, got {again:?}"
    );

    // And the association can be made again afterwards.
    let answer = associate(&router, "unlink", &arns[1]).await;
    assert_eq!(
        answer.status, 200,
        "case unlink-1 re-associated: associating must succeed, got {} {}",
        answer.status, answer.body
    );
    let mut restored = arns.clone();
    restored.sort();
    let reported = all_consumers(&router, "unlink", None).await;
    assert_eq!(
        reported, restored,
        "case unlink-1 re-associated: the collection must be reported again, got {reported:?}"
    );
}

// ---------------------------------------------------------------------------
// The documented per-tracker quota
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_collection_past_the_documented_quota_is_refused() {
    let router = emulator();
    create_tracker(&router, "quota").await;

    // Every collection up to and including the documented bound associates.
    let mut expected = Vec::new();
    for index in 0..MAX_CONSUMERS_PER_TRACKER {
        let arn = create_collection(&router, &format!("quota-{index}")).await;
        let answer = associate(&router, "quota", &arn).await;
        assert_eq!(
            answer.status, 200,
            "case {index}: associating within the quota must succeed, got {} {}",
            answer.status, answer.body
        );
        expected.push(arn);
    }
    expected.sort();

    // One past it is refused with the modelled quota error, and nothing is stored.
    let past = create_collection(&router, "quota-past").await;
    let answer = associate(&router, "quota", &past).await;
    assert_eq!(
        answer.status, 402,
        "case one past the quota: associating must be refused, got {} {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ServiceQuotaExceededException",
        "case one past the quota: the refusal must be a ServiceQuotaExceededException, got {}",
        answer.body
    );
    let reported = all_consumers(&router, "quota", None).await;
    assert_eq!(
        reported, expected,
        "case one past the quota: the refused association must not be stored, got {reported:?}"
    );
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// One row of the refusal table: a request that names something this service does not
/// hold, or a value that is no geofence collection ARN at all.
struct RefusalCase {
    case: &'static str,
    /// The tracker the request addresses.
    tracker: &'static str,
    /// The `ConsumerArn` the association request carries, if the row makes one.
    consumer_arn: Option<&'static str>,
    status: u16,
    error_code: &'static str,
    message: fn() -> String,
}

#[tokio::test]
async fn associating_refuses_what_this_service_does_not_hold() {
    let router = emulator();
    create_tracker(&router, "present").await;
    let present_collection = create_collection(&router, "present-collection").await;

    let cases = vec![
        RefusalCase {
            case: "a tracker this service holds none of",
            tracker: "absent",
            consumer_arn: None,
            status: 404,
            error_code: "ResourceNotFoundException",
            message: || "Tracker not found: absent.".to_string(),
        },
        RefusalCase {
            case: "a geofence collection this service holds none of",
            tracker: "present",
            consumer_arn: Some("arn:aws:geo:us-west-2:000000000000:geofence-collection/absent"),
            status: 404,
            error_code: "ResourceNotFoundException",
            message: || "Geofence collection not found: absent.".to_string(),
        },
        RefusalCase {
            case: "an ARN naming another resource type of this service",
            tracker: "present",
            consumer_arn: Some("arn:aws:geo:us-west-2:000000000000:tracker/present"),
            status: 400,
            error_code: "ValidationException",
            message: || NOT_A_COLLECTION_ARN.to_string(),
        },
        RefusalCase {
            case: "an ARN of another vendor",
            tracker: "present",
            consumer_arn: Some("arn:aws:sqs:us-west-2:000000000000:geofence-collection/present"),
            status: 400,
            error_code: "ValidationException",
            message: || NOT_A_COLLECTION_ARN.to_string(),
        },
        RefusalCase {
            case: "an ARN naming no resource id",
            tracker: "present",
            consumer_arn: Some("arn:aws:geo:us-west-2:000000000000:geofence-collection/"),
            status: 400,
            error_code: "ValidationException",
            message: || NOT_A_COLLECTION_ARN.to_string(),
        },
    ];

    for case in cases {
        let consumer_arn = case.consumer_arn.unwrap_or(&present_collection);
        let answer = associate(&router, case.tracker, consumer_arn).await;
        assert_eq!(
            answer.status, case.status,
            "case {}: expected HTTP {}, got {} {}",
            case.case, case.status, answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            case.error_code,
            "case {}: expected {}, got {}",
            case.case,
            case.error_code,
            answer.body
        );
        assert_eq!(
            answer.message(),
            (case.message)(),
            "case {}: expected the refusal to state {:?}, got {:?}",
            case.case,
            (case.message)(),
            answer.message()
        );
    }
}

#[tokio::test]
async fn reading_and_unlinking_refuse_a_tracker_this_service_does_not_hold() {
    let router = emulator();
    let collection = create_collection(&router, "orphan-collection").await;

    let answer = list_consumers(&router, "absent", json!({})).await;
    assert_eq!(
        answer.status, 404,
        "case list: an absent tracker must be refused, got {} {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ResourceNotFoundException",
        "case list: the refusal must be a ResourceNotFoundException, got {}",
        answer.body
    );
    assert_eq!(
        answer.message(),
        "Tracker not found: absent.",
        "case list: expected the refusal to name the tracker, got {:?}",
        answer.message()
    );

    let answer = disassociate(&router, "absent", &collection).await;
    assert_eq!(
        answer.status, 404,
        "case disassociate: an absent tracker must be refused, got {} {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ResourceNotFoundException",
        "case disassociate: the refusal must be a ResourceNotFoundException, got {}",
        answer.body
    );

    create_tracker(&router, "orphan").await;
    let answer = disassociate(
        &router,
        "orphan",
        "arn:aws:geo:us-west-2:000000000000:tracker/orphan",
    )
    .await;
    assert_eq!(
        answer.status, 400,
        "case disassociate a non-collection ARN: expected HTTP 400, got {} {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.message(),
        NOT_A_COLLECTION_ARN,
        "case disassociate a non-collection ARN: expected the refusal to state {NOT_A_COLLECTION_ARN:?}, got {:?}",
        answer.message()
    );
}
