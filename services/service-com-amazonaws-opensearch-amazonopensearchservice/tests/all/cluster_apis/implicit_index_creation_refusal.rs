// SPDX-License-Identifier: CC0-1.0

//! WHAT a domain's REST data plane says about an index it will not create for a write.
//!
//! RULE: a document write addressed to an index the cluster does not hold, where
//! `action.auto_create_index` does not permit creating it, is refused as the MISSING INDEX
//! it is — HTTP 404 `index_not_found_exception` whose reason names that index and nothing
//! else (`no such index [<index>]`), the same sentence a read addressed to an index the
//! cluster does not hold is refused with. Which form of the setting would not create it —
//! the boolean off, a blocking pattern the name matches, or a list no pattern of which
//! matches — changes neither the status, the type nor the sentence; a setting that DOES
//! permit the creation mints the index the write names.
//!
//! WHETHER the write creates the index is [`super`]'s rule, over the same forms of the
//! setting; what is new here is the ANSWER a refused write gets, which a client reads to
//! tell a missing index from a malformed request — so the rows are driven through BOTH
//! addressings a client on this machine reaches a domain by: the prefixed path on the
//! emulator's own endpoint, and the HOST the control plane published in
//! `DomainStatus$Endpoint`, which is the address a client is configured with and has to be
//! served by the engine rather than answered by the front door for a host no service claims.
//!
//! The harness is this crate's own ([`crate::domain_lifecycle`],
//! [`crate::domain_rest_data_plane`]) and the domain comes from [`super`]'s own helper; this
//! suite adds no second copy of either.

use serde_json::json;

use crate::domain_lifecycle::{Answer, Emulator, DEFAULT_ACCESS_KEY_ID};
use crate::domain_rest_bulk_and_index_apis::bulk;
use crate::domain_rest_data_plane::{endpoint_of, error_type, rest, rest_prefixed};

/// The engine's error type for an index the cluster does not hold.
const INDEX_NOT_FOUND: &str = "index_not_found_exception";

/// One `action.auto_create_index` value to write a document under: the value the cluster
/// holds, the index a write then names, and whether the cluster creates it.
struct Row {
    case: &'static str,
    domain: &'static str,
    /// The value written to `action.auto_create_index`, in the flat spelling a setting's
    /// name has.
    setting: &'static str,
    index: &'static str,
    /// Whether the write is served — the index created — rather than refused.
    created: bool,
}

/// The table: every form of the setting that will not create an index — the boolean off, a
/// blocking pattern the name matches, and a list no pattern of which matches — over index
/// names of different shapes, plus one value that permits the creation as the control.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the setting switched off",
            domain: "implicit-off",
            setting: "false",
            index: "logs-2026",
            created: false,
        },
        Row {
            case: "the setting switched off, and a dotted index name",
            domain: "implicit-dotted",
            setting: "false",
            index: "app.events-2026",
            created: false,
        },
        Row {
            case: "a blocking pattern the name matches",
            domain: "implicit-blocked",
            setting: "-logs-*,+*",
            index: "logs-2026",
            created: false,
        },
        Row {
            case: "a pattern list no pattern of which matches the name",
            domain: "implicit-unmatched",
            setting: "logs-*",
            index: "metrics-2026",
            created: false,
        },
        Row {
            case: "a permitting pattern the name matches",
            domain: "implicit-allowed",
            setting: "+logs-*,-*",
            index: "logs-2026",
            created: true,
        },
    ]
}

/// Write the row's value to `action.auto_create_index` in the `persistent` scope, asserting
/// the cluster took it.
async fn given_setting(emulator: &Emulator, row: &Row) {
    let case = row.case;
    let applied = rest_prefixed(
        emulator,
        row.domain,
        "PUT",
        "/_cluster/settings",
        Some(json!({ "persistent": { "action.auto_create_index": row.setting } })),
    )
    .await;
    assert_eq!(
        applied.status, 200,
        "case {case:?}: expected 200 from the settings update, got {} with {:?}",
        applied.status, applied.body
    );
}

/// The `(status, error type, reason)` an answer carries, as a client reads them off the
/// engine's error envelope.
fn refusal(answer: &Answer) -> (u16, String, String) {
    (
        answer.status,
        error_type(answer),
        answer.body["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// The first item a bulk answer carries, as `(status, error type, reason)` — an item that
/// landed carrying neither type nor reason.
fn bulk_item(answer: &Answer) -> (u16, String, String) {
    let item = &answer.body["items"][0]["index"];
    (
        u16::try_from(item["status"].as_i64().unwrap_or_default()).unwrap_or_default(),
        item["error"]["type"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        item["error"]["reason"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// RULE: where `action.auto_create_index` will not create the index a write names, the write
/// is refused as the missing index it is — 404 `index_not_found_exception`, reason `no such
/// index [<index>]` — exactly as a read of an index the cluster does not hold is, at both
/// addressings of the domain and whether the request wrote one document or a stream of them;
/// where the setting permits the creation, the write mints the index.
#[tokio::test]
async fn a_write_the_setting_will_not_create_an_index_for_is_refused_as_a_missing_index() {
    let emulator = Emulator::new();

    for row in rows() {
        let case = row.case;
        super::given_domain(&emulator, row.domain, None).await;
        given_setting(&emulator, &row).await;

        // What the cluster has to say about an index it does not hold, as a READ of that
        // index is told it: the condition the write below meets is the same one, so this is
        // what it has to be told too.
        let read =
            rest_prefixed(&emulator, row.domain, "GET", &format!("/{}", row.index), None).await;
        let missing = (
            404,
            INDEX_NOT_FOUND.to_string(),
            format!("no such index [{}]", row.index),
        );
        assert_eq!(
            refusal(&read),
            missing,
            "case {case:?}: expected a read of an index the cluster does not hold to be \
             refused as a missing index, got {:?}",
            read.body
        );

        // The HOST the control plane published is the address a client is configured with,
        // so the writes below go there: an unrouted host is answered by the front door
        // rather than by the engine, which is neither this status nor this envelope.
        let host = endpoint_of(&emulator, row.domain);
        let written = rest(
            &emulator,
            DEFAULT_ACCESS_KEY_ID,
            &host,
            "PUT",
            &format!("/{}/_doc/1", row.index),
            Some(json!({ "message": "hello" })),
        )
        .await;
        let batched = bulk(
            &emulator,
            &host,
            "/_bulk",
            &[
                json!({ "index": { "_index": row.index, "_id": "2" } }),
                json!({ "message": "hello" }),
            ],
        )
        .await;
        assert_eq!(
            batched.status, 200,
            "case {case:?}: the bulk API answers 200 whatever its items did, got {} with {:?}",
            batched.status, batched.body
        );

        if row.created {
            assert_eq!(
                (written.status, written.body["result"].as_str()),
                (201, Some("created")),
                "case {case:?}: expected the write to create the index, got {} with {:?}",
                written.status,
                written.body
            );
            assert_eq!(
                bulk_item(&batched),
                (201, String::new(), String::new()),
                "case {case:?}: expected the bulk item to be created, got {:?}",
                batched.body
            );
            let held =
                rest_prefixed(&emulator, row.domain, "GET", &format!("/{}", row.index), None).await;
            assert_eq!(
                held.status, 200,
                "case {case:?}: expected the minted index to read back, got {} with {:?}",
                held.status, held.body
            );
            continue;
        }

        assert_eq!(
            refusal(&written),
            missing,
            "case {case:?}: expected the write to be refused as a missing index, got {} with \
             {:?}",
            written.status,
            written.body
        );
        assert_eq!(
            bulk_item(&batched),
            missing,
            "case {case:?}: expected the bulk item to be refused as a missing index, got {:?}",
            batched.body
        );

        // And nothing was minted: a refused write leaves the cluster holding no such index,
        // whichever addressing a client reads it through.
        let through_host = rest(
            &emulator,
            DEFAULT_ACCESS_KEY_ID,
            &host,
            "GET",
            &format!("/{}", row.index),
            None,
        )
        .await;
        assert_eq!(
            refusal(&through_host),
            missing,
            "case {case:?}: the refused write must mint no index, got {} with {:?}",
            through_host.status,
            through_host.body
        );
    }
}

/// RULE: the setting governs CREATION only — an index the cluster already holds takes the
/// write whatever the setting says, and the document reads back.
#[tokio::test]
async fn an_index_the_cluster_holds_takes_the_write_whatever_the_setting_says() {
    let emulator = Emulator::new();

    for row in rows().into_iter().filter(|row| !row.created) {
        let case = row.case;
        super::given_domain(&emulator, row.domain, None).await;
        given_setting(&emulator, &row).await;

        let made = rest_prefixed(
            &emulator,
            row.domain,
            "PUT",
            &format!("/{}", row.index),
            Some(json!({})),
        )
        .await;
        assert_eq!(
            (made.status, made.body["acknowledged"].as_bool()),
            (200, Some(true)),
            "case {case:?}: an explicit create is served whatever the setting says, got {} \
             with {:?}",
            made.status,
            made.body
        );

        let written = rest_prefixed(
            &emulator,
            row.domain,
            "PUT",
            &format!("/{}/_doc/1", row.index),
            Some(json!({ "message": "hello" })),
        )
        .await;
        assert_eq!(
            (written.status, written.body["result"].as_str()),
            (201, Some("created")),
            "case {case:?}: expected the write into the held index to be served, got {} with \
             {:?}",
            written.status,
            written.body
        );

        let read = rest_prefixed(
            &emulator,
            row.domain,
            "GET",
            &format!("/{}/_doc/1", row.index),
            None,
        )
        .await;
        assert_eq!(
            read.body["_source"]["message"].as_str(),
            Some("hello"),
            "case {case:?}: expected the document to read back, got {:?}",
            read.body
        );
    }
}
