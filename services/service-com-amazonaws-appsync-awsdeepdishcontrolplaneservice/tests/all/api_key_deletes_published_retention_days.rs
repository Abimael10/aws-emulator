// SPDX-License-Identifier: CC0-1.0

//! The retention `ListApiKeys` reports for a `da2` API key, read over the WHOLE validity
//! window the service publishes — including past its ends, where nothing is minted at
//! all.
//!
//! The arithmetic itself, its constants and their sources are
//! [`crate::api_key_deletion_window`]'s, whose `assert_retention_span` this suite calls
//! rather than restating: a reported key's `deletes` is its `expires` plus the service's
//! post-expiry retention, floored to the hour, which for the `da2` keys this service
//! mints is "Expired API keys are kept for 60 days after the expiration time"
//! (`smithy_query(query="doc", shape="ApiKey")`), restated by `ListApiKeys` as "API keys
//! are deleted automatically 60 days after they expire".
//!
//! What this suite adds is the EDGE of the input class and what happens past it: one API
//! holds a key from each row of a validity table that reaches both published bounds, a
//! validity outside each bound is refused and asserted to leave the inventory exactly as
//! it was, and the inventory the rows built is then read back in ONE page and asserted
//! key by key. A derivation reading anything but the key's own stored expiration, or a
//! refusal that half-wrote a key, would answer a listing that disagrees with the keys its
//! creates had handed back.
//!
//! The rows differ along the key's validity, the one input the derivation reads, taken
//! across the published window — "The API key expiration must be set to a value between
//! 1 and 365 days from creation (for CreateApiKey) or from update (for UpdateApiKey)"
//! (`ApiKeyValidityOutOfBoundsException`): each bound of that range, day counts between
//! them that no recorded case names, a request that names no expiration at all and so
//! takes `CreateApiKey`'s documented default validity (asserted as such), and a validity
//! just past each bound, which mints no key, so the inventory the window is read off
//! never holds one derived from it.
//!
//! The harness, the clock read and the hour flooring are
//! [`crate::api_key_deletion_window`]'s; every request goes through the router the
//! production emulator mounts.

use serde_json::{json, Value};

use crate::api_key_deletion_window::{
    assert_retention_span, create_key, floor_to_hour, listed_key, unix_now, BOUND_SLACK, DAY,
    DEFAULT_VALIDITY, HOUR, MAX_VALIDITY, MIN_VALIDITY,
};
use crate::error_contract::{call, create_api, router};

/// One row of the validity table: its label, the validity it submits (seconds past the
/// clock the row itself reads, or `None` for a request that names no expiration), and
/// whether the published window accepts that validity.
struct Row {
    case: &'static str,
    validity: Option<i64>,
    accepted: bool,
}

/// The validities the window is asserted over: both published bounds, day counts inside
/// the window, the documented default, and one validity just outside each bound.
const ROWS: &[Row] = &[
    Row {
        case: "the minimum validity, one day",
        validity: Some(MIN_VALIDITY + BOUND_SLACK),
        accepted: true,
    },
    Row {
        case: "thirteen days, off an hour boundary",
        validity: Some(13 * DAY + 5 * HOUR + 37),
        accepted: true,
    },
    Row {
        case: "ninety-one days",
        validity: Some(91 * DAY),
        accepted: true,
    },
    Row {
        case: "two hundred and forty-seven days",
        validity: Some(247 * DAY + 1),
        accepted: true,
    },
    Row {
        case: "the maximum validity, three hundred and sixty-five days",
        validity: Some(MAX_VALIDITY - BOUND_SLACK),
        accepted: true,
    },
    Row {
        case: "no expiration at all",
        validity: None,
        accepted: true,
    },
    Row {
        case: "an hour short of the minimum validity",
        validity: Some(MIN_VALIDITY - HOUR),
        accepted: false,
    },
    Row {
        case: "a day past the maximum validity",
        validity: Some(MAX_VALIDITY + DAY),
        accepted: false,
    },
];

/// Rule: every key `ListApiKeys` reports carries the retention its own expiration
/// derives, whatever validity inside the published window minted it — and a validity
/// outside that window mints no key at all.
#[tokio::test]
async fn every_validity_of_the_published_window_is_listed_with_its_own_retention() {
    let router = router();
    let api_id = create_api(&router, "Api_key_deletes_published_window").await;

    // Every key the accepted rows have minted so far, in creation order: what the
    // listing must hold, and what a refused create must leave untouched.
    let mut inventory: Vec<Value> = Vec::new();

    for row in ROWS {
        let case = row.case;
        let before = unix_now();
        let mut body = json!({ "description": case });
        if let Some(validity) = row.validity {
            body["expires"] = json!(before + validity);
        }
        let answer = create_key(&router, &api_id, body).await;

        if !row.accepted {
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected CreateApiKey 400, got {answer:?}"
            );
            assert_eq!(
                answer.error_type, "ApiKeyValidityOutOfBoundsException",
                "case {case:?}: expected error type ApiKeyValidityOutOfBoundsException, \
                 got {answer:?}"
            );
            let listing = call(&router, "GET", &format!("/v1/apis/{api_id}/apikeys"), None).await;
            assert_eq!(
                listing.status, 200,
                "case {case:?}: expected ListApiKeys 200, got {listing:?}"
            );
            assert_eq!(
                listing.body["apiKeys"],
                json!(inventory),
                "case {case:?}: expected a refused create to leave the inventory as it \
                 was, got {listing:?}"
            );
            continue;
        }

        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected CreateApiKey 200, got {answer:?}"
        );
        let created = answer.body["apiKey"].clone();
        let key_id = created["id"].as_str().unwrap_or_default().to_string();
        let expires = assert_retention_span(case, "CreateApiKey", &created);
        match row.validity {
            Some(validity) => assert_eq!(
                expires,
                floor_to_hour(before + validity),
                "case {case:?}: expected the submitted expiration floored to the hour, \
                 got {created:?}"
            ),
            // The handler reads its own clock, which sits at or after this row's, so the
            // documented default lands in the span between the two reads.
            None => assert!(
                (floor_to_hour(before + DEFAULT_VALIDITY)
                    ..=floor_to_hour(unix_now() + DEFAULT_VALIDITY))
                    .contains(&expires),
                "case {case:?}: expected the documented default validity, got {created:?}"
            ),
        }

        let listed = listed_key(&router, &api_id, &key_id).await;
        assert_retention_span(case, "ListApiKeys", &listed);
        assert_eq!(
            listed, created,
            "case {case:?}: expected ListApiKeys to project the key CreateApiKey stored, \
             got {listed:?}"
        );
        inventory.push(created);
    }

    // The whole listing, read in one page, carries the published window for every key it
    // holds: the rows above are read back together, not only one at a time.
    let listing = call(&router, "GET", &format!("/v1/apis/{api_id}/apikeys"), None).await;
    assert_eq!(
        listing.status, 200,
        "case the whole listing: expected ListApiKeys 200, got {listing:?}"
    );
    assert_eq!(
        listing.body["apiKeys"],
        json!(inventory),
        "case the whole listing: expected the keys the accepted rows stored, in creation \
         order, got {listing:?}"
    );
    for key in listing.body["apiKeys"].as_array().unwrap_or(&Vec::new()) {
        assert_retention_span("the whole listing", "ListApiKeys", key);
    }
}
