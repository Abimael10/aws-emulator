// SPDX-License-Identifier: CC0-1.0

//! The RESULT WINDOW a search's page is bounded by, driven over the real wire path: a signed
//! request to the endpoint a domain publishes, through the multi-service router, into the
//! mounted service.
//!
//! The rule, over the set of pages one search may ask for:
//!
//! > `from + size` is bounded by the index's own `index.max_result_window`, a page exactly at
//! > it is served, and a page past it is refused the way a SHARD's failure is reported — HTTP
//! > 400 `search_phase_execution_exception` carrying the illegal argument as its root cause
//! > and no `hits` section at all — because the engine applies that bound while a shard
//! > executes the query.
//!
//! What a client does with the difference is why it matters: a deep-paging guard that reported
//! itself as a plain argument error would be indistinguishable from a malformed request, so
//! pagination code that should switch to `search_after` never does.
//!
//! The harness is [`super::domain_lifecycle`]'s and the data-plane request helper is
//! [`super::domain_rest_data_plane`]'s; this suite adds no second copy of either.

use serde_json::{json, Value};

use super::domain_lifecycle::{Answer, Emulator};
use super::domain_rest_data_plane::{error_type, rest_prefixed};

/// The domain whose published endpoint every request below is addressed to.
const DOMAIN: &str = "paging-domain";

/// The index that states no window of its own, so the setting's own default bounds it, and the
/// one that states a smaller window — the two the table is read over.
const DEFAULT_WINDOW_INDEX: &str = "wide";
const NARROW_INDEX: &str = "narrow";

/// The window the index above states, and the one `index.max_result_window` defaults to.
///
/// The default is the setting's own documented value; the narrow one is what the other index
/// holds the same setting at, which is what makes the bound the INDEX's rather than a constant
/// of this surface.
const NARROW_WINDOW: usize = 50;
const DEFAULT_WINDOW: usize = 10_000;

/// `CreateDomain`, then the two indexes and one document in each.
async fn given_indexed(emulator: &Emulator) {
    let created = emulator
        .create(json!({
            "DomainName": DOMAIN,
            "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
            "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
        }))
        .await;
    assert_eq!(
        created.status, 200,
        "case CreateDomain({DOMAIN:?}): expected 200, got {} with {:?}",
        created.status, created.body
    );
    for (index, settings) in [
        (
            DEFAULT_WINDOW_INDEX,
            json!({ "number_of_shards": 1, "number_of_replicas": 0 }),
        ),
        (
            NARROW_INDEX,
            json!({
                "number_of_shards": 1,
                "number_of_replicas": 0,
                "max_result_window": NARROW_WINDOW,
            }),
        ),
    ] {
        let created = rest_prefixed(
            emulator,
            DOMAIN,
            "PUT",
            &format!("/{index}"),
            Some(json!({
                "settings": settings,
                "mappings": { "properties": { "rank": { "type": "integer" } } },
            })),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case \"PUT /{index}\": expected 200, got {} with {:?}",
            created.status, created.body
        );
        let written = rest_prefixed(
            emulator,
            DOMAIN,
            "PUT",
            &format!("/{index}/_doc/one"),
            Some(json!({ "rank": 1 })),
        )
        .await;
        assert!(
            written.status == 200 || written.status == 201,
            "case \"write {index}/one\": expected 200 or 201, got {} with {:?}",
            written.status,
            written.body
        );
    }
}

/// One `POST /{index}/_search`.
async fn search(emulator: &Emulator, index: &str, body: Value) -> Answer {
    rest_prefixed(
        emulator,
        DOMAIN,
        "POST",
        &format!("/{index}/_search"),
        Some(body),
    )
    .await
}

/// RULE: a page exactly at the index's own window is served, and one past it is refused as a
/// shard failure.
#[tokio::test]
async fn a_page_past_the_indexs_own_result_window_is_refused_as_a_shard_failure() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    // (case, the index, from, size, the window the index holds, whether it is served) — rows
    // spanning both sides of each index's own bound, each bound reached by `from` and by
    // `size` alike, and the page one past it.
    let rows: [(&str, &str, usize, usize, usize, bool); 8] = [
        (
            "the last page the default window admits",
            DEFAULT_WINDOW_INDEX,
            DEFAULT_WINDOW - 10,
            10,
            DEFAULT_WINDOW,
            true,
        ),
        (
            "one hit past the default window",
            DEFAULT_WINDOW_INDEX,
            DEFAULT_WINDOW - 9,
            10,
            DEFAULT_WINDOW,
            false,
        ),
        (
            "a size alone at the default window",
            DEFAULT_WINDOW_INDEX,
            0,
            DEFAULT_WINDOW,
            DEFAULT_WINDOW,
            true,
        ),
        (
            "a size alone one past it",
            DEFAULT_WINDOW_INDEX,
            0,
            DEFAULT_WINDOW + 1,
            DEFAULT_WINDOW,
            false,
        ),
        (
            "the last page the narrower window admits",
            NARROW_INDEX,
            NARROW_WINDOW - 10,
            10,
            NARROW_WINDOW,
            true,
        ),
        (
            "one hit past the narrower window",
            NARROW_INDEX,
            NARROW_WINDOW - 9,
            10,
            NARROW_WINDOW,
            false,
        ),
        (
            "a page the default window admits but the narrower one does not",
            NARROW_INDEX,
            NARROW_WINDOW,
            10,
            NARROW_WINDOW,
            false,
        ),
        (
            "that same page against the index holding the default window",
            DEFAULT_WINDOW_INDEX,
            NARROW_WINDOW,
            10,
            DEFAULT_WINDOW,
            true,
        ),
    ];
    for (case, index, from, size, window, served) in rows {
        let answered = search(
            &emulator,
            index,
            json!({ "from": from, "size": size, "sort": [{ "rank": "asc" }] }),
        )
        .await;
        if served {
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected the page to be served, got {} with {:?}",
                answered.status, answered.body
            );
            assert!(
                answered.body["hits"]["total"]["value"].as_u64() == Some(1),
                "case {case:?}: expected the total the index holds, got {:?}",
                answered.body["hits"]
            );
            continue;
        }
        assert_eq!(
            (answered.status, error_type(&answered)),
            (400, "search_phase_execution_exception".to_string()),
            "case {case:?}: expected the search-phase refusal, got {} with {:?}",
            answered.status,
            answered.body
        );
        assert_eq!(
            answered.body["error"]["root_cause"][0]["type"].as_str(),
            Some("illegal_argument_exception"),
            "case {case:?}: expected the argument failure as the root cause, got {:?}",
            answered.body["error"]
        );
        let reason = answered.body["error"]["root_cause"][0]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            reason.starts_with(&format!(
                "Result window is too large, from + size must be less than or equal to: \
                 [{window}] but was [{}].",
                from + size
            )),
            "case {case:?}: expected the reason to state the window and the page, got {reason:?}"
        );
        assert!(
            reason.contains("[index.max_result_window]"),
            "case {case:?}: expected the reason to name the setting that carries the window, \
             got {reason:?}"
        );
        // A refused search answers no hits at all: a client must not read a page out of a
        // refusal.
        assert!(
            answered.body.get("hits").is_none(),
            "case {case:?}: expected no hits section on a refusal, got {:?}",
            answered.body
        );
    }
}
