// SPDX-License-Identifier: CC0-1.0

//! How `BatchGetTraces` ASSEMBLES the documents of one trace into the tree a
//! reader is answered with.
//!
//! The rule under test: **a trace is one TREE. A producer submits one document
//! per ENTITY, and an entity whose parent was still open when it completed
//! travels on its own, as a document whose `type` is `subsegment` naming the
//! entity it belongs inside; reading the trace puts it back there — inside that
//! entity's own document, however deep the entity sits and whichever order the
//! two arrived in. An entity whose parent the trace does not hold stays at the
//! top as it came, a re-submitted entity replaces the one it supersedes rather
//! than standing beside it, and a document that adopted nothing is answered with
//! the bytes it was submitted as.**
//!
//! The rows differ along what the rule generalises over: WHICH entity the
//! standalone document names (the segment itself, a subsegment carried inline in
//! it, another standalone subsegment document, and an entity of no document at
//! all), the ORDER the documents arrive in, and how many of them there are.

use std::collections::BTreeSet;

use serde_json::{json, Value};
// The bound assembly nests to is the service's own, so the row past it is the
// one the service states rather than one this suite picked.
use service_com_amazonaws_xray_awsxray::state::MAX_ASSEMBLED_DEPTH;

use crate::xray_wire::{array, root_document, trace_id, Xray};

/// The ids every row below uses: the caller's segment, a subsegment carried
/// inline inside it, and the two standalone documents.
const SEGMENT: &str = "1111111111111111";
const INLINE: &str = "2222222222222222";
const STANDALONE: &str = "3333333333333333";
const DEEPER: &str = "4444444444444444";

/// An id no document of these rows carries, so nothing of the trace holds the
/// entity it names.
const UNHELD: &str = "5555555555555555";

/// The times every document carries.
const STARTED: f64 = 1_000.0;
const ENDED: f64 = 1_021.0;

/// The caller's own segment, carrying the inline subsegment above.
fn segment(trace: &str) -> Value {
    let mut document = root_document(SEGMENT, trace, "frontend", STARTED, Some(ENDED));
    document["subsegments"] = json!([{
        "id": INLINE,
        "name": "in-process-work",
        "start_time": STARTED + 0.1,
        "end_time": ENDED - 0.1,
    }]);
    document
}

/// One subsegment submitted as its own document, naming `parent_id`.
fn standalone(trace: &str, id: &str, parent_id: &str) -> Value {
    json!({
        "trace_id": trace,
        "id": id,
        "name": format!("work-{id}"),
        "type": "subsegment",
        "parent_id": parent_id,
        "start_time": STARTED + 0.2,
        "end_time": ENDED - 0.2,
    })
}

/// Every top-level document of `trace`, as `BatchGetTraces` answers it.
async fn documents_of(xray: &Xray, trace: &str) -> Vec<Value> {
    let answered = xray.batch_get(&[trace.to_string()], None).await;
    assert_eq!(
        answered.status, 200,
        "BatchGetTraces must answer, got {answered:?}"
    );
    array(&answered.body, "Traces")
        .iter()
        .flat_map(|held| array(held, "Segments").to_vec())
        .filter_map(|segment| {
            segment
                .get("Document")
                .and_then(Value::as_str)
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
        })
        .collect()
}

/// The id of the document that holds the entity `id`, and the id of the entity
/// CARRYING it — `None` where the entity is a top-level document of the trace,
/// and no entry at all where the trace holds no such entity.
fn container_of(documents: &[Value], id: &str) -> Option<(String, Option<String>)> {
    for document in documents {
        let top = document.get("id").and_then(Value::as_str).unwrap_or_default();
        if top == id {
            return Some((top.to_string(), None));
        }
        if let Some(carrier) = carrier_of(document, id) {
            return Some((top.to_string(), Some(carrier)));
        }
    }
    None
}

/// The id of the entity inside `host` that carries the entity `id`.
fn carrier_of(host: &Value, id: &str) -> Option<String> {
    for nested in host
        .get("subsegments")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if nested.get("id").and_then(Value::as_str) == Some(id) {
            return host
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| Some(String::new()));
        }
        if let Some(found) = carrier_of(nested, id) {
            return Some(found);
        }
    }
    None
}

/// One row: the documents submitted, in order, and where the standalone
/// document's entity must end up.
struct Row {
    case: &'static str,
    /// The documents, in submission order, built against the row's trace.
    documents: fn(&str) -> Vec<Value>,
    /// The top-level document ids the assembled trace must hold.
    top_level: &'static [&'static str],
    /// The entity [`STANDALONE`] must be carried by, `None` for one that stays a
    /// top-level document of its own.
    carried_by: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "the subsegment document names the segment, and arrives after it",
        documents: |trace| vec![segment(trace), standalone(trace, STANDALONE, SEGMENT)],
        top_level: &[SEGMENT],
        carried_by: Some(SEGMENT),
    },
    Row {
        case: "the subsegment document names the segment, and arrives before it",
        documents: |trace| vec![standalone(trace, STANDALONE, SEGMENT), segment(trace)],
        top_level: &[SEGMENT],
        carried_by: Some(SEGMENT),
    },
    Row {
        case: "the subsegment document names a subsegment carried inline",
        documents: |trace| vec![segment(trace), standalone(trace, STANDALONE, INLINE)],
        top_level: &[SEGMENT],
        carried_by: Some(INLINE),
    },
    Row {
        case: "the subsegment document names another standalone document, which arrives last",
        documents: |trace| {
            vec![
                standalone(trace, STANDALONE, DEEPER),
                standalone(trace, DEEPER, SEGMENT),
                segment(trace),
            ]
        },
        top_level: &[SEGMENT],
        carried_by: Some(DEEPER),
    },
    Row {
        case: "the subsegment document names an entity no document of this trace holds",
        documents: |trace| vec![segment(trace), standalone(trace, STANDALONE, UNHELD)],
        top_level: &[SEGMENT, STANDALONE],
        carried_by: None,
    },
    Row {
        case: "the same subsegment document is submitted twice",
        documents: |trace| {
            vec![
                segment(trace),
                standalone(trace, STANDALONE, SEGMENT),
                standalone(trace, STANDALONE, SEGMENT),
            ]
        },
        top_level: &[SEGMENT],
        carried_by: Some(SEGMENT),
    },
];

#[tokio::test]
async fn a_submitted_subsegment_is_answered_for_inside_the_entity_it_named() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let xray = Xray::new();
        let nth = u32::try_from(index).expect("the row count fits a u32") + 1;
        let trace = trace_id(nth);
        for document in (row.documents)(&trace) {
            let put = xray.put_segments(&[document.to_string()]).await;
            assert_eq!(
                put.status, 200,
                "case {case:?}: seeding must succeed, got {put:?}"
            );
            assert!(
                array(&put.body, "UnprocessedTraceSegments").is_empty(),
                "case {case:?}: every document must be accepted, got {put:?}"
            );
        }

        let documents = documents_of(&xray, &trace).await;
        let held: BTreeSet<String> = documents
            .iter()
            .filter_map(|document| document.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        let expected: BTreeSet<String> =
            row.top_level.iter().map(|id| (*id).to_string()).collect();
        assert_eq!(
            held, expected,
            "case {case:?}: expected the top-level documents {expected:?}, got {documents:?}"
        );

        let found = container_of(&documents, STANDALONE);
        let carried = found.as_ref().and_then(|(_, carrier)| carrier.clone());
        assert_eq!(
            carried.as_deref(),
            row.carried_by,
            "case {case:?}: expected the submitted entity to be carried by {:?}, got {found:?} \
             of {documents:?}",
            row.carried_by
        );
        // Carried once, not twice: a trace is a tree, so one entity has one
        // place in it.
        let occurrences = documents
            .iter()
            .flat_map(ids_of)
            .filter(|id| id == STANDALONE)
            .count();
        assert_eq!(
            occurrences, 1,
            "case {case:?}: expected the entity once in the tree, got {documents:?}"
        );
    }
}

#[tokio::test]
async fn a_document_that_adopted_nothing_is_answered_with_the_bytes_it_carried() {
    let xray = Xray::new();
    let trace = trace_id(64);
    // One document adopts the standalone subsegment and the other adopts
    // nothing, so one read shows both halves of the rule.
    let other = root_document("6666666666666666", &trace, "sibling", STARTED, Some(ENDED));
    let submitted = [
        segment(&trace).to_string(),
        other.to_string(),
        standalone(&trace, STANDALONE, SEGMENT).to_string(),
    ];
    for document in &submitted {
        let put = xray.put_segments(std::slice::from_ref(document)).await;
        assert_eq!(put.status, 200, "seeding must succeed, got {put:?}");
    }

    let answered = xray.batch_get(std::slice::from_ref(&trace), None).await;
    let texts: Vec<String> = array(&answered.body, "Traces")
        .iter()
        .flat_map(|held| array(held, "Segments").to_vec())
        .filter_map(|segment| {
            segment
                .get("Document")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert!(
        texts.contains(&other.to_string()),
        "case \"a document that adopted nothing\": expected the submitted bytes back, got \
         {texts:?}"
    );
    let adopting: Vec<&String> = texts
        .iter()
        .filter(|text| text.contains(STANDALONE))
        .collect();
    assert_eq!(
        adopting.len(),
        1,
        "case \"a document that adopted the entity\": expected one document carrying it, got \
         {texts:?}"
    );
    assert_ne!(
        adopting[0],
        &segment(&trace).to_string(),
        "case \"a document that adopted the entity\": expected the adopted entity inside it, \
         got {:?}",
        adopting[0]
    );
}

#[tokio::test]
async fn a_chain_of_documents_nests_to_the_bound_and_no_entity_is_lost_past_it() {
    // The nesting assembly builds is bounded, because every reader of the answer
    // walks the tree it returns. A chain one document longer than the bound is
    // still answered for whole: what does not nest stays at the top.
    let xray = Xray::new();
    let trace = trace_id(96);
    let chained = MAX_ASSEMBLED_DEPTH + 1;
    let id_at = |level: usize| format!("{:016x}", 0xaa00_0000_0000_0000_u64 + level as u64);
    let mut submitted = vec![segment(&trace).to_string()];
    for level in 0..chained {
        let parent = if level == 0 {
            SEGMENT.to_string()
        } else {
            id_at(level - 1)
        };
        submitted.push(standalone(&trace, &id_at(level), &parent).to_string());
    }
    for document in &submitted {
        let put = xray.put_segments(std::slice::from_ref(document)).await;
        assert_eq!(put.status, 200, "seeding must succeed, got {put:?}");
    }

    let documents = documents_of(&xray, &trace).await;
    // Every entity is somewhere in the answer, exactly once.
    let held: Vec<String> = documents.iter().flat_map(ids_of).collect();
    for level in 0..chained {
        let id = id_at(level);
        assert_eq!(
            held.iter().filter(|found| **found == id).count(),
            1,
            "case \"a chain of {chained} documents\": expected the entity {id:?} once in the \
             answer, got {held:?}"
        );
    }
    // And the one past the bound is the one the top of the trace holds.
    let top: Vec<String> = documents
        .iter()
        .filter_map(|document| document.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    assert_eq!(
        top,
        vec![SEGMENT.to_string(), id_at(chained - 1)],
        "case \"a chain of {chained} documents\": expected the entity past the bound at the top \
         of the trace, got {documents:?}"
    );
}

/// Every entity id one document carries, its own first.
fn ids_of(document: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(id) = document.get("id").and_then(Value::as_str) {
        ids.push(id.to_string());
    }
    for nested in document
        .get("subsegments")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        ids.extend(ids_of(nested));
    }
    ids
}
