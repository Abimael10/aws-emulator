// SPDX-License-Identifier: CC0-1.0

//! How a filter operation words its refusal of a well-formed filter ARN that names no
//! stored filter, driven end-to-end through `CreateFilter`, `UpdateFilter` and (as the way
//! a stored filter is removed) `DeleteFilter` over the real `MultiServiceRouter`.
//!
//! `DeleteFilter` is not among the operations that refuse: deleting an ARN the store does
//! not hold is the idempotent success `delete_filter_idempotence` pins, so the refusal this
//! suite is about belongs to the operations that must RESOLVE a filter to answer at all.
//!
//! The rule under test: a filter lookup that finds nothing is HTTP 404
//! `ResourceNotFoundException` whose message states the ARN FIRST, exactly as the request
//! sent it, then the bare sentence " does not exist" — no noun phrase before the ARN and no
//! closing period — then the envelope the relayed call to the findings backend is reported
//! inside: that backend's name, the status it answered with, that call's own freshly minted
//! request id and how many attempts it took.
//!
//! The rows span what the rule generalises over: WHICH operation looks the filter up, and
//! which part of a well-formed ARN is unfamiliar — including an ARN this emulator itself
//! minted and then deleted, so the refusal is about the resource's absence rather than the
//! value's shape.

use serde_json::{json, Value};

use crate::filter_arn_and_tags::{create_filter, unknown_filter_arns, ACCOUNT, DELETE, UPDATE};
use crate::filters_and_enablement::{post_in, router};

/// The envelope's trailing clause, with the attempt count stripped off — a refusal of the
/// caller's own input is not retried, so the count that follows is read and checked rather
/// than assumed.
const ATTEMPTS_OPENING: &str = ") (SDK Attempt Count: ";

/// Assert `message` is the refusal the rule prescribes for `arn`, and answer the request id
/// it states so a caller can compare two refusals' ids.
fn assert_not_found_wording(case: &str, arn: &str, message: &str) -> String {
    let head = format!(
        "{arn} does not exist (Service: MandoFindings, Status Code: 404, Request ID: "
    );
    let rest = message.strip_prefix(&head).unwrap_or_else(|| {
        panic!("case {case}: expected the message to start {head:?}, got {message:?}")
    });
    let (request_id, attempts) = rest.split_once(ATTEMPTS_OPENING).unwrap_or_else(|| {
        panic!(
            "case {case}: expected the message to carry {ATTEMPTS_OPENING:?} after the request \
             id, got {message:?}"
        )
    });
    assert!(
        ::service_common::query_error_envelope::is_request_id(request_id),
        "case {case}: expected a request id between the envelope's parentheses, got \
         {request_id:?}"
    );
    let attempts = attempts
        .strip_suffix(')')
        .unwrap_or_else(|| panic!("case {case}: expected the envelope to close, got {message:?}"));
    let counted: u32 = attempts.parse().unwrap_or_else(|error| {
        panic!("case {case}: expected an attempt count, got {attempts:?} ({error})")
    });
    assert!(
        counted >= 1,
        "case {case}: expected at least one attempt, got {counted}"
    );
    request_id.to_string()
}

/// A filter operation that looks a filter up refuses a well-formed ARN naming no stored
/// filter with the ARN-first sentence and the relayed call's envelope.
///
/// The rows are the shared table of well-formed ARNs this emulator never minted, which
/// differ along which part of the ARN is unfamiliar — the partition, the region, the
/// account, the owner (an account and an organization) and the id — none of which the shape
/// check may refuse, and each of which must still be echoed verbatim in the refusal.
#[tokio::test]
async fn an_unknown_filter_arn_is_refused_arn_first_with_the_relayed_envelope() {
    let (uri, member) = UPDATE;
    for (row, candidate) in unknown_filter_arns() {
        let case = format!("{row} / {uri}");
        let router = router();
        let body = json!({ member: candidate, "action": "SUPPRESS" });
        let (status, error_type, response) = post_in(&router, "us-east-1", uri, &body).await;
        assert_eq!(
            status, 404,
            "case {case}: expected 404, got {status} / {response}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: expected ResourceNotFoundException, got {error_type:?} / {response}"
        );
        let message = response["message"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a message, got {response}"));
        assert_not_found_wording(&case, &candidate, message);
    }
}

/// A filter this emulator minted and then deleted is refused by exactly the same wording,
/// quoting the ARN the create handed out: the refusal reports the resource's absence, not
/// anything about the value's shape.
///
/// The rows differ along how many filters the store held before the delete, so the ARN under
/// test is a minted one whichever position it occupied.
#[tokio::test]
async fn a_deleted_filter_arn_is_refused_with_the_same_wording() {
    let (uri, member) = UPDATE;
    for neighbours in [0usize, 1, 3] {
        let case = format!("{neighbours} other filters / {uri}");
        let router = router();
        for index in 0..neighbours {
            create_filter(&router, &format!("neighbour-{index}")).await;
        }
        let arn = create_filter(&router, "doomed").await;

        let (status, _, deleted) =
            post_in(&router, "us-east-1", DELETE.0, &json!({ "arn": arn })).await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteFilter: expected 200, got {status} / {deleted}"
        );

        let body = json!({ member: arn.clone(), "action": "SUPPRESS" });
        let (status, error_type, response) = post_in(&router, "us-east-1", uri, &body).await;
        assert_eq!(
            status, 404,
            "case {case}: expected 404, got {status} / {response}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: expected ResourceNotFoundException, got {error_type:?} / {response}"
        );
        let message = response["message"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a message, got {response}"));
        assert_not_found_wording(&case, &arn, message);
    }
}

/// A `filterCriteria` selecting one severity.
/// Source: `smithy_query(query="shape", shape="FilterCriteria")` — `severity`, a list of
/// `StringFilter` members `comparison` and `value`.
fn severity_criteria(severity: &str) -> Value {
    json!({ "severity": [{ "comparison": "EQUALS", "value": severity }] })
}

/// The ARNs a listing reports, in the order it reports them.
async fn listed_filters(
    router: &smithy_server_runtime::MultiServiceRouter,
    region: &str,
    case: &str,
) -> Vec<Value> {
    let (status, _, listed) = post_in(router, region, "/filters/list", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case}: ListFilters: expected 200, got {status} / {listed}"
    );
    listed["filters"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: expected a list of filters, got {listed}"))
}

/// The refusal is worded the same way after a filter has LIVED a whole lifecycle — created,
/// listed, in some rows successfully updated (whose new action and criteria do persist),
/// deleted and gone from the listing: the 404 states the minted ARN first, verbatim, with no
/// noun phrase before it and no closing period, then the relayed call's envelope.
///
/// The rows differ along the region the filter was minted in — so the ARN echoed back differs
/// in a field the refusal must not normalise — whether a successful update preceded the
/// delete, and how many other filters the store still holds when the refusal is answered.
#[tokio::test]
async fn the_wording_is_the_same_after_a_filters_whole_lifecycle() {
    // (the region the lifecycle runs in, the severity a pre-delete update selects — none for
    // a row that deletes the filter as created, the filters left behind)
    let rows: [(&str, Option<&str>, usize); 4] = [
        ("us-east-1", None, 0),
        ("us-west-2", Some("HIGH"), 0),
        ("eu-central-1", Some("CRITICAL"), 2),
        ("ap-southeast-2", None, 1),
    ];

    let (uri, member) = UPDATE;
    for (region, updated, neighbours) in rows {
        let case = format!("{region} / update before delete {updated:?} / {neighbours} left");
        let router = router();
        for index in 0..neighbours {
            create_filter(&router, &format!("neighbour-{index}")).await;
        }

        let (status, _, created) = post_in(
            &router,
            region,
            "/filters/create",
            &json!({
                "action": "NONE",
                "name": "lifecycle",
                "filterCriteria": severity_criteria("MEDIUM"),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateFilter: expected 200, got {status} / {created}"
        );
        let arn = created["arn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a minted ARN, got {created}"))
            .to_string();
        let listed = listed_filters(&router, region, &case).await;
        assert!(
            listed.iter().any(|filter| filter["arn"] == arn.as_str()),
            "case {case}: expected the created filter listed, got {listed:?}"
        );

        if let Some(severity) = updated {
            let criteria = severity_criteria(severity);
            let (status, _, response) = post_in(
                &router,
                region,
                uri,
                &json!({ member: arn.clone(), "action": "SUPPRESS", "filterCriteria": criteria }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: UpdateFilter: expected 200, got {status} / {response}"
            );
            let listed = listed_filters(&router, region, &case).await;
            let stored = listed
                .iter()
                .find(|filter| filter["arn"] == arn.as_str())
                .unwrap_or_else(|| panic!("case {case}: expected the filter listed, got {listed:?}"));
            assert_eq!(
                stored["action"], "SUPPRESS",
                "case {case}: expected the updated action stored, got {stored}"
            );
            assert_eq!(
                stored["criteria"], criteria,
                "case {case}: expected the updated criteria stored, got {stored}"
            );
        }

        let (status, _, deleted) =
            post_in(&router, region, DELETE.0, &json!({ "arn": arn.clone() })).await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteFilter: expected 200, got {status} / {deleted}"
        );
        let listed = listed_filters(&router, region, &case).await;
        assert!(
            !listed.iter().any(|filter| filter["arn"] == arn.as_str()),
            "case {case}: expected the deleted filter gone from the listing, got {listed:?}"
        );
        assert_eq!(
            listed.len(),
            neighbours,
            "case {case}: expected the other {neighbours} filter(s) left, got {listed:?}"
        );

        let (status, error_type, response) = post_in(
            &router,
            region,
            uri,
            &json!({ member: arn.clone(), "action": "SUPPRESS" }),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case}: expected 404, got {status} / {response}"
        );
        assert_eq!(
            error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: expected ResourceNotFoundException, got {error_type:?} / {response}"
        );
        let message = response["message"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: expected a message, got {response}"));
        assert_not_found_wording(&case, &arn, message);
        // The ARN leads the message and the envelope closes it: no noun phrase is prepended
        // and the sentence carries no closing period.
        assert!(
            message.starts_with(&arn) && message.ends_with(')'),
            "case {case}: expected the ARN to lead and the envelope to close the message, got \
             {message:?}"
        );
    }
}

/// The request id in the envelope belongs to the RESPONSE: two refusals of the same ARN,
/// through the same operation, never report one id — which is what makes the id usable for
/// correlating a single failed call.
#[tokio::test]
async fn each_refusal_reports_its_own_request_id() {
    let router = router();
    let arn = format!("arn:aws:inspector2:us-east-1:{ACCOUNT}:owner/{ACCOUNT}/filter/0123456789ab");

    let (uri, member) = UPDATE;
    let mut ids = Vec::new();
    for refusal in 0..3 {
        let case = format!("refusal {refusal} / {uri}");
        let body = json!({ member: arn.clone(), "description": "tuned" });
        let (status, _, response) = post_in(&router, "us-east-1", uri, &body).await;
        assert_eq!(
            status, 404,
            "case {case}: expected 404, got {status} / {response}"
        );
        let message = response["message"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: got {response}"));
        ids.push(assert_not_found_wording(&case, &arn, message));
    }

    for (first, second) in [(0usize, 1usize), (0, 2), (1, 2)] {
        assert_ne!(
            ids[first], ids[second],
            "case \"refusals {first} and {second}\": expected each response its own request id, \
             got {:?} twice",
            ids[first]
        );
    }
}
