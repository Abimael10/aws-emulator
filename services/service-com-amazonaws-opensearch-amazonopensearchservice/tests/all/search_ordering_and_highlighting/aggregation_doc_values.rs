// SPDX-License-Identifier: CC0-1.0

//! What an aggregation may be taken ON: the PER-DOCUMENT VALUES of the field it names.
//!
//! > an aggregation reads a field's per-document values — the same values a `sort` key orders
//! > the hits by — so a field the index holds none for, an analyzed `text` field with
//! > `fielddata` left off, can bucket and measure nothing: an aggregation naming one is
//! > REFUSED exactly as a sort key naming one is, rather than answered with an empty
//! > `buckets` array, which says the selected documents hold no such value. The `keyword`
//! > sub-field a mapping adds to that same field DOES hold them — a multi-field reads the
//! > values of the field above it — and is served, and so is a `text` field whose own mapping
//! > switches `fielddata` on.
//!
//! Which matters because the usual mapping for "full-text search plus exact aggregation on one
//! field" is exactly a `text` field with a `keyword` sub-field: a client that aggregates the
//! analyzed field by mistake must be told so rather than shown zero everywhere.
//!
//! The harness, the index and the request helpers are [`super`]'s; this suite adds none of its
//! own.

use super::*;

/// Every aggregation this surface computes, as a request names it — the bucketing and each
/// single-value metric — because every one of them reads the same per-document values and is
/// refused on the same fields.
const AGGREGATIONS: [&str; 6] = ["terms", "value_count", "min", "max", "sum", "avg"];

/// The analyzed `text` fields the index maps whose mappings leave `fielddata` off, so the
/// index holds no per-document values for either of them.
const WITHOUT_DOC_VALUES: [&str; 2] = ["title", "body"];

/// The keys the buckets of `field` answer, in the order they are answered in.
fn bucket_keys(answer: &Answer, name: &str) -> Vec<String> {
    answer.body["aggregations"][name]["buckets"]
        .as_array()
        .map(|buckets| {
            buckets
                .iter()
                .map(|bucket| bucket["key"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// RULE: an aggregation naming a field the index holds no per-document values for is refused,
/// and the same aggregation over a field that does hold them — the keyword sub-field of that
/// very field among them — is served.
#[tokio::test]
async fn an_aggregation_on_a_field_with_no_doc_values_is_refused_rather_than_answered_empty() {
    let emulator = Emulator::new();
    given_indexed(&emulator).await;

    // The product of the two sets: every aggregation, over each analyzed field, because what
    // decides the refusal is the field rather than the aggregation asked of it.
    for field in WITHOUT_DOC_VALUES {
        for kind in AGGREGATIONS {
            let case = format!("{kind} over the analyzed field [{field}]");
            let answered = search(
                &emulator,
                &format!("/{INDEX}/_search"),
                Some(json!({ "size": 0, "aggs": { "asked": { kind: { "field": field } } } })),
            )
            .await;
            assert_eq!(
                (
                    answered.status,
                    answered.body["error"]["type"].as_str(),
                    answered.body["error"]["root_cause"][0]["type"].as_str(),
                ),
                (
                    400,
                    Some("search_phase_execution_exception"),
                    Some("illegal_argument_exception"),
                ),
                "case {case:?}: expected the engine's per-shard refusal, got {:?}",
                answered.body
            );
            let reason = answered.body["error"]["root_cause"][0]["reason"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                reason.contains(&format!("fielddata=true on [{field}]")),
                "case {case:?}: expected the reason to name the field and what switches its \
                 values on, got {reason:?}"
            );
            // Nothing of the refused aggregation is answered: no section carrying an empty
            // bucket list, which is what a client would read as "no such value".
            assert!(
                answered.body.get("aggregations").is_none(),
                "case {case:?}: expected no aggregations section on a refusal, got {:?}",
                answered.body
            );
        }
    }

    // (the case, the field, the keys its buckets must carry) — the rows span the two ways a
    // field the index DOES hold per-document values for can sit beside an analyzed one: as its
    // own `keyword` sub-field, and as a `text` field whose mapping switches `fielddata` on.
    let served: &[(&str, &str, [&str; 3])] = &[
        (
            "the keyword sub-field of the refused field",
            "title.raw",
            ["alpha release", "beta release", "gamma release"],
        ),
        (
            "the keyword sibling of the other refused field",
            "body.keyword",
            [
                "a vorpal blade went snicker snack",
                "nothing to see here",
                "the quick brown vorpal jumps over the lazy dog",
            ],
        ),
        (
            "a text field whose mapping switches fielddata on",
            "notes",
            ["alpha notes", "beta notes", "gamma notes"],
        ),
    ];
    for (case, field, keys) in served {
        let answered = search(
            &emulator,
            &format!("/{INDEX}/_search"),
            Some(json!({
                "size": 0,
                "aggs": {
                    "by_value": { "terms": { "field": field } },
                    "values": { "value_count": { "field": field } }
                }
            })),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected 200, got {} with {:?}",
            answered.status, answered.body
        );
        let answered_keys = bucket_keys(&answered, "by_value");
        assert_eq!(
            answered_keys, *keys,
            "case {case:?}: expected one bucket per distinct value, got {answered_keys:?}"
        );
        assert_eq!(
            answered.body["aggregations"]["values"]["value"].as_u64(),
            Some(3),
            "case {case:?}: expected the metric over the same values, got {:?}",
            answered.body["aggregations"]
        );
    }

    // The refusals changed nothing a later caller sees: the index still holds its documents,
    // and each still reads back through the document API as it was written.
    let after = search(&emulator, &format!("/{INDEX}/_search"), Some(json!({ "size": 0 }))).await;
    assert_eq!(
        after.body["hits"]["total"]["value"].as_u64(),
        Some(3),
        "case \"a search after the refused aggregations\": expected the documents still held, \
         got {:?}",
        after.body
    );
    let read = rest_prefixed(&emulator, DOMAIN, "GET", &format!("/{INDEX}/_doc/d2"), None).await;
    assert_eq!(
        read.body["_source"]["title"].as_str(),
        Some("alpha release"),
        "case \"reading a document behind a refused aggregation\": expected the source the \
         write carried, got {:?}",
        read.body
    );
}
