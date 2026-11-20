// SPDX-License-Identifier: CC0-1.0

//! The data plane's PARTIAL-DOCUMENT UPDATE: the `doc` object a caller sends instead of the
//! whole document, merged onto the document the index holds.
//!
//! Two requests ask for it and one rule answers both — `POST /{index}/_update/{id}`, served
//! here, and the `update` action of a bulk stream ([`super::bulk`]) — so a field advanced on
//! its own and one advanced inside a stream change the stored document the same way and are
//! refused by the same sentences.
//!
//! What the rule is: the members the partial names are written over the stored document and
//! every member it does not name stays, which is what lets a client advance one field
//! without re-sending the rest; the result is stored as the document's next revision through
//! the ONE data-plane write path ([`ingest::index_into`]), so an updated document is mapped
//! and numbered exactly as a written one. An update of an id the index does not hold is the
//! engine's missing-document refusal — or, for a request naming an index the cluster does not
//! hold at all, its missing-index one, since an update READS the document before writing it —
//! unless the request asks for the partial, or the `upsert` document beside it, to be indexed
//! instead. A merge that changes nothing writes nothing (`detect_noop`), and reports the
//! revision the document is still at.
//!
//! The IAM decision is the request's, taken once at this plane's front door against the
//! resource the path addresses; the cluster's own decision is taken here
//! ([`super::guarded`]) for the action the update asks for.

use std::time::SystemTime;

use aws_smithy_runtime_api::http::Response as RtResponse;
use serde_json::{json, Map, Value};

use crate::state::{fgac, ingest, Domain};

use super::{
    guarded, record_written, rendered, refused, requested_revision, unreadable_write,
    write_conflict, write_index, write_shards, written_document, wrong_method, Creates, Refusal,
    Request, Written,
};

/// The member a partial update carries the fields to merge in: "doc: the partial document
/// to merge into the stored one".
pub(super) const DOC: &str = "doc";

/// The member asking for the partial document to be INDEXED where the index holds no
/// document of that id: "doc_as_upsert: use the contents of doc as the upsert value".
pub(super) const DOC_AS_UPSERT: &str = "doc_as_upsert";

/// The member carrying the document to index INSTEAD in that case — the upsert document a
/// request sends beside its partial.
const UPSERT: &str = "upsert";

/// The member that switches off the comparison below: "detect_noop: specify whether to
/// perform the operation only if the document has changed", which is on unless a request
/// says otherwise.
const DETECT_NOOP: &str = "detect_noop";

/// The member a SCRIPTED update carries.
const SCRIPT: &str = "script";

/// What a merge that changed nothing reports as its result.
const NOOP: &str = "noop";

/// The most members one partial document may NAME, counting every level of it.
///
/// The emulator's OWN bound, beside [`MAX_PARTIAL_DEPTH`]: the merge applies each member the
/// partial names into the document it is merging onto, so without these two the work one
/// request causes would be sized by the shape the caller sent rather than by a constant. A
/// partial past either bound is refused at the member or the level that reaches it.
const MAX_PARTIAL_MEMBERS: usize = 10_000;

/// How deep one partial document may nest. See [`MAX_PARTIAL_MEMBERS`].
const MAX_PARTIAL_DEPTH: usize = 32;

/// What the partial update makes of the document the index holds.
pub(super) enum Updated {
    /// The source to store, which is what the write path is handed.
    Stored(Value),
    /// The merge left the document exactly as it was, so nothing is written at all and the
    /// revision the document is still at is what the answer reports.
    Unchanged,
}

/// What the partial update `body` makes of the document `held`, or why it is refused.
///
/// The ONE reading of a partial update, so the single-document route and a bulk `update`
/// item cannot merge, upsert or refuse differently. `index_name`/`id` name what the update
/// addresses, for the refusals that are about them, and `holds_index` is whether the cluster
/// holds that index at all — an update is a READ of the document followed by a write, so a
/// request that names no index to read from is refused as a missing index rather than as a
/// missing document, while one that asks for the partial to be indexed instead names the
/// index the write creates.
pub(super) fn updated(
    index_name: &str,
    id: &str,
    holds_index: bool,
    held: Option<&Value>,
    body: &Value,
) -> Result<Updated, Refusal> {
    // "script or doc is missing": this surface updates a document with the partial document
    // the request carries. A SCRIPTED update runs caller-supplied code, which this cluster
    // does not run, so such a request is refused rather than reported as applied.
    if body.get(SCRIPT).is_some() {
        return Err(Refusal::of_illegal_argument(
            "scripted updates are not supported by this cluster; send [doc] with the fields \
             to change",
        ));
    }
    let Some(doc) = body.get(DOC).filter(|doc| doc.is_object()) else {
        return Err(Refusal::of_illegal_argument(
            "Validation Failed: 1: script or doc is missing;",
        ));
    };
    // The partial is held to the bound ONE document is held to
    // ([`ingest::MAX_DOCUMENT_BYTES`]) before anything is done with it, so neither the merge
    // below nor the upsert materialises work for a partial larger than a document may be.
    within_bounds(doc)?;
    if let Some(source) = held {
        let mut merged = source.clone();
        merge(&mut merged, doc)?;
        if detects_noop(body) && merged == *source {
            return Ok(Updated::Unchanged);
        }
        return Ok(Updated::Stored(merged));
    }
    // The index holds no document of that id: what is indexed is the partial itself where
    // the request asks for that, the `upsert` document where it carries one, and otherwise
    // nothing at all — the engine's missing-document refusal, or its missing-index one for a
    // request that named no index to read the document from.
    if body.get(DOC_AS_UPSERT).and_then(Value::as_bool) == Some(true) {
        return Ok(Updated::Stored(doc.clone()));
    }
    if let Some(upsert) = body.get(UPSERT).filter(|upsert| upsert.is_object()) {
        return Ok(Updated::Stored(upsert.clone()));
    }
    if holds_index {
        return Err(Refusal::of_document_missing(id));
    }
    Err(Refusal::of_index_not_found(index_name))
}

/// Whether the request leaves the no-op comparison in force, which it does unless it names
/// `detect_noop` false.
fn detects_noop(body: &Value) -> bool {
    body.get(DETECT_NOOP).and_then(Value::as_bool) != Some(false)
}

/// What an update that changed nothing answers: the revision the document is still at, and
/// a `_shards` summary counting NO copy — no shard took a write, so none is reported as
/// having taken one.
pub(super) fn noop_answer(
    index_name: &str,
    id: &str,
    held: &ingest::IndexedDocument,
) -> Value {
    json!({
        "_index": index_name,
        "_id": id,
        "_version": held.version,
        "result": NOOP,
        "_shards": { "total": 0, "successful": 0, "failed": 0 },
        "_seq_no": held.seq_no,
        "_primary_term": held.primary_term,
    })
}

/// `POST /{index}/_update/{id}` — the partial-document update of one document.
///
/// `POST` is the one method the route takes, as the engine registers it; every other is the
/// engine's own wrong-method refusal naming this route's list.
pub(super) fn update_request(
    domain: &mut Domain,
    request: &Request<'_>,
    index_name: &str,
    id: &str,
    now: SystemTime,
) -> RtResponse {
    match request.method {
        "POST" => guarded(domain, request, fgac::ACTION_UPDATE_DOCUMENT, |domain| {
            update(domain, request, index_name, id, now)
        }),
        _ => wrong_method(request, "POST"),
    }
}

/// Merge the request's partial onto the document the index holds and store what it comes
/// to, answering the revision the write left it at.
fn update(
    domain: &mut Domain,
    request: &Request<'_>,
    named: &str,
    id: &str,
    now: SystemTime,
) -> RtResponse {
    // An update addressed to an ALIAS acts on the index that alias is the write index of,
    // the same resolution every write of this surface goes through.
    let index_name = match write_index(domain, named) {
        Ok(index_name) => index_name,
        Err(refusal) => return refusal.response(),
    };
    let index_name = index_name.as_str();
    let body = match request.json() {
        Ok(body) => body.unwrap_or(Value::Null),
        Err(refusal) => return *refusal,
    };
    // The compare-and-set a conditional update performs, held to exactly as a document
    // write is: the update proceeds only while the document is still at the revision the
    // request names.
    if let Some(refusal) = write_conflict(
        domain,
        index_name,
        Some(id),
        Creates::Upsert,
        requested_revision(request),
    ) {
        return refusal.response();
    }
    let holds_index = domain.indexes.contains_key(index_name);
    let held = domain
        .indexes
        .get(index_name)
        .and_then(|index| index.documents.get(id))
        .cloned();
    let source = match updated(
        index_name,
        id,
        holds_index,
        held.as_ref().map(|document| &document.source),
        &body,
    ) {
        Ok(Updated::Stored(source)) => source,
        Ok(Updated::Unchanged) => {
            return match &held {
                Some(document) => rendered(200, &noop_answer(index_name, id, document)),
                // Nothing is unchanged where the index holds no document to compare with:
                // the merge reports that case as the missing document it is.
                None => Refusal::of_document_missing(id).response(),
            }
        }
        Err(refusal) => return refusal.response(),
    };
    let created = held.is_none();
    let document = match ::serde_json::to_vec(&source) {
        Ok(document) => document,
        Err(failed) => {
            return Refusal::of_mapper_parsing(&format!(
                "failed to render the updated document: {failed}"
            ))
            .response()
        }
    };
    let address = domain.arn.clone();
    let write = ingest::DocumentWrite {
        domain: &address,
        index_name,
        document_id: Some(id).filter(|id| !id.is_empty()),
        document: &document,
        principal_arn: request.principal,
        now,
    };
    // Whether the merged document will CREATE the index it names, read before it is
    // applied, for the reason every write route reads it ([`super::write_records`]).
    let auto_created = !domain.indexes.contains_key(index_name);
    // The merged document goes through the ONE data-plane write path, which owns the
    // mapping rules and the revision numbering, so an updated document is judged by the
    // same cluster rules a written one is.
    match ingest::index_into(domain, &write) {
        Ok(indexed) => {
            let Some(written) = written_document(domain, &indexed) else {
                return unreadable_write(&indexed).response();
            };
            record_written(
                domain,
                request,
                Written {
                    index_name: &indexed.index_name,
                    id: &indexed.document_id,
                    took: request.took(),
                    source: ::std::str::from_utf8(&document).unwrap_or_default(),
                    auto_created,
                },
            );
            rendered(
                if created { 201 } else { 200 },
                &json!({
                    "_index": indexed.index_name,
                    "_id": indexed.document_id,
                    "_version": written.version,
                    "result": if created { "created" } else { "updated" },
                    "_shards": write_shards(domain, &indexed.index_name),
                    "_seq_no": written.seq_no,
                    "_primary_term": written.primary_term,
                }),
            )
        }
        // What the write path refuses is the CLUSTER's own (a principal fine-grained access
        // control binds to no role, or a document the index's mappings do not accept),
        // answered in the engine's envelope as a document write's refusal is.
        Err(refusal) => refused(&refusal),
    }
}

/// Whether the partial `doc` is within the bytes ONE document of this cluster may run to,
/// which is what every write is held to ([`ingest::MAX_DOCUMENT_BYTES`]).
///
/// Checked HERE rather than left to the write path, because the merge runs first: a partial
/// past it is refused before a member of it is applied or a merged document is built. A
/// partial this emulator cannot render at all is past every bound there is.
fn within_bounds(doc: &Value) -> Result<(), Refusal> {
    let bytes = ::serde_json::to_vec(doc).map_or(usize::MAX, |rendered| rendered.len());
    if bytes > ingest::MAX_DOCUMENT_BYTES {
        return Err(Refusal::of_illegal_argument(&format!(
            "the partial document is larger than the {} bytes one document may carry",
            ingest::MAX_DOCUMENT_BYTES
        )));
    }
    Ok(())
}

/// Write the members of the partial document `doc` over `source`, descending into an
/// object a member of both carries — which is how a partial update changes one field of a
/// nested object without dropping the fields beside it.
///
/// The descent writes each member STRAIGHT into the document it is merging onto, so no path
/// and no copy of one is ever held: what the merge allocates is the values it stores, which
/// the partial's own bound ([`within_bounds`]) already caps. It descends only while the
/// partial is within [`MAX_PARTIAL_DEPTH`] — checked before the descent, so the recursion is
/// bounded by that constant rather than by the body — and applies at most
/// [`MAX_PARTIAL_MEMBERS`] members.
fn merge(source: &mut Value, doc: &Value) -> Result<(), Refusal> {
    let mut applied = 0_usize;
    merge_into(source, doc, 0, &mut applied)
}

/// Apply the members `doc` names onto `target`, at nesting `depth`, counting what has been
/// applied so far in `applied`.
///
/// An object naming NO member is written only where the target carries no member there: it
/// names nothing to change, so merging it over an object the document already holds would
/// drop what stands there. A member standing at a value that is not an object becomes one,
/// as the engine's own merge does, so the members under it can be written.
fn merge_into(
    target: &mut Value,
    doc: &Value,
    depth: usize,
    applied: &mut usize,
) -> Result<(), Refusal> {
    let Some(members) = doc.as_object().filter(|members| !members.is_empty()) else {
        return Ok(());
    };
    if depth >= MAX_PARTIAL_DEPTH {
        return Err(too_deep());
    }
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let Some(into) = target.as_object_mut() else {
        return Ok(());
    };
    for (member, child) in members {
        *applied = applied.saturating_add(1);
        if *applied > MAX_PARTIAL_MEMBERS {
            return Err(too_many_members());
        }
        match child {
            // An object with members is the way to the members under it.
            Value::Object(under) if !under.is_empty() => {
                let standing = into
                    .entry(member.clone())
                    .or_insert_with(|| Value::Object(Map::new()));
                merge_into(standing, child, depth.saturating_add(1), applied)?;
            }
            // An object naming no member: owed to the document only where it carries none.
            Value::Object(_) => {
                into.entry(member.clone())
                    .or_insert_with(|| Value::Object(Map::new()));
            }
            value => {
                into.insert(member.clone(), value.clone());
            }
        }
    }
    Ok(())
}

/// The refusal a partial nesting past [`MAX_PARTIAL_DEPTH`] earns.
fn too_deep() -> Refusal {
    Refusal::of_illegal_argument(&format!(
        "the partial document nests deeper than the {MAX_PARTIAL_DEPTH} levels one update may \
         carry"
    ))
}

/// The refusal a partial naming more members than [`MAX_PARTIAL_MEMBERS`] earns.
fn too_many_members() -> Refusal {
    Refusal::of_illegal_argument(&format!(
        "the partial document names more than the {MAX_PARTIAL_MEMBERS} members one update may \
         carry"
    ))
}
