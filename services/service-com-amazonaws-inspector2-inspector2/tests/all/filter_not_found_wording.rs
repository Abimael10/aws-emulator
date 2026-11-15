// SPDX-License-Identifier: CC0-1.0

//! How a filter operation words its refusal of a well-formed filter ARN that names no
//! stored filter, driven end-to-end through `CreateFilter`, `DeleteFilter` and
//! `UpdateFilter` over the real `MultiServiceRouter`.
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

/// Every filter operation that looks a filter up refuses a well-formed ARN naming no stored
/// filter with the ARN-first sentence and the relayed call's envelope.
///
/// The rows are the shared table of well-formed ARNs this emulator never minted, which
/// differ along which part of the ARN is unfamiliar — the partition, the region, the
/// account, the owner (an account and an organization) and the id — none of which the shape
/// check may refuse, and each of which must still be echoed verbatim in the refusal.
#[tokio::test]
async fn an_unknown_filter_arn_is_refused_arn_first_with_the_relayed_envelope() {
    for (row, candidate) in unknown_filter_arns() {
        for (uri, member) in [UPDATE, DELETE] {
            let case = format!("{row} / {uri}");
            let router = router();
            let mut body = json!({ member: candidate });
            if uri == UPDATE.0 {
                body["action"] = Value::String("SUPPRESS".to_string());
            }
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
}

/// A filter this emulator minted and then deleted is refused by exactly the same wording,
/// quoting the ARN the create handed out: the refusal reports the resource's absence, not
/// anything about the value's shape.
///
/// The rows differ along how many filters the store held before the delete, so the ARN under
/// test is a minted one whichever position it occupied.
#[tokio::test]
async fn a_deleted_filter_arn_is_refused_with_the_same_wording() {
    for neighbours in [0usize, 1, 3] {
        for (uri, member) in [UPDATE, DELETE] {
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

            let mut body = json!({ member: arn.clone() });
            if uri == UPDATE.0 {
                body["action"] = Value::String("SUPPRESS".to_string());
            }
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
}

/// The request id in the envelope belongs to the RESPONSE: two refusals of the same ARN,
/// through the same operation, never report one id — which is what makes the id usable for
/// correlating a single failed call.
#[tokio::test]
async fn each_refusal_reports_its_own_request_id() {
    let router = router();
    let arn = format!("arn:aws:inspector2:us-east-1:{ACCOUNT}:owner/{ACCOUNT}/filter/0123456789ab");

    let mut ids = Vec::new();
    for (uri, member) in [UPDATE, DELETE, UPDATE] {
        let mut body = json!({ member: arn.clone() });
        if uri == UPDATE.0 {
            body["description"] = Value::String("tuned".to_string());
        }
        let (status, _, response) = post_in(&router, "us-east-1", uri, &body).await;
        assert_eq!(
            status, 404,
            "case a repeated refusal / {uri}: expected 404, got {status} / {response}"
        );
        let message = response["message"]
            .as_str()
            .unwrap_or_else(|| panic!("case a repeated refusal / {uri}: got {response}"));
        ids.push(assert_not_found_wording(
            &format!("a repeated refusal / {uri}"),
            &arn,
            message,
        ));
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
