// SPDX-License-Identifier: CC0-1.0

//! The data plane's BULK API: `POST /_bulk` and `POST /{index}/_bulk`, "the bulk
//! operation lets you add, update, or delete many documents in a single request".
//!
//! The body is a NEWLINE-DELIMITED stream: an action line naming what to do and the index
//! and id to do it to, followed — for an action that carries one — by the document source
//! on the next line. The answer is HTTP 200 whatever the items did: `errors` says whether
//! ANY of them failed, and each item carries its own `status` and, where it failed, its own
//! `error`. A body this surface cannot read as that stream at all is the one failure the
//! whole request earns, since there is no item to report it against.
//!
//! Every item is applied through the ONE data-plane write path
//! ([`ingest::index_into`]) and is held to the same two conditions a single document write
//! is ([`super::write_conflict`]) — so a document loaded in bulk is mapped, numbered and
//! refused exactly as one written on its own, and a bulk load is not a second way into an
//! index with rules of its own. The IAM decision is the request's, taken once at this
//! plane's front door against the resource the bulk path addresses.
//!
//! Each item the stream APPLIES produces the records the domain's log publishing has of a
//! document write, exactly as the single-document routes do ([`super::write_records`]): a
//! bulk load is where a client's documents arrive, so a slow-log group that stayed empty
//! for it would stay empty for the load the index's threshold was set for. The duration one
//! record states is that ITEM's own write, since each item is a write of its own.

use std::time::{Instant, SystemTime};

use aws_smithy_runtime_api::http::Response as RtResponse;
use serde_json::{json, Map, Value};

use crate::state::{fgac, ingest, Domain};

use super::{
    apply_pipeline, document_update, error, guarded, ok, record_written, wrong_method,
    write_conflict, Creates, Refusal, Request, Written,
};

/// The actions a bulk stream may name, each the name of the member its metadata is carried
/// under: "index", "create", "update" and "delete".
const ACTIONS: [&str; 4] = [INDEX, CREATE, UPDATE, DELETE];

/// See [`ACTIONS`].
const INDEX: &str = "index";
/// See [`ACTIONS`].
const CREATE: &str = "create";
/// See [`ACTIONS`].
const UPDATE: &str = "update";
/// See [`ACTIONS`].
const DELETE: &str = "delete";

/// The metadata members an action names the index and the document with.
const META_INDEX: &str = "_index";

/// See [`META_INDEX`].
const META_ID: &str = "_id";

/// The most actions ONE bulk request may carry.
///
/// The emulator's OWN bound on the work one request does: each action is a write of its
/// own into this process's heap. The request body is bounded as every body of this surface
/// is ([`super::MAX_REQUEST_BODY_BYTES`]), so this bounds the ACTIONS a body within that
/// could name — a stream of minimal action lines — rather than the bytes again.
const MAX_BULK_ACTIONS: usize = 10_000;

/// `POST`/`PUT /_bulk`, and the index-scoped form whose path names the index an action
/// that names none lands in.
pub(super) fn bulk_request(
    domain: &mut Domain,
    request: &Request<'_>,
    default_index: Option<&str>,
    now: SystemTime,
) -> RtResponse {
    match request.method {
        "POST" | "PUT" => guarded(domain, request, fgac::ACTION_BULK, |domain| {
            bulk(domain, request, default_index, now)
        }),
        _ => wrong_method(request, "POST, PUT"),
    }
}

/// One action of a bulk stream, as the parse reads it.
struct Action {
    /// Which of [`ACTIONS`] it is.
    kind: &'static str,
    /// The index it addresses: its own `_index`, or the one the path named.
    index_name: String,
    /// The document id it addresses, empty where the action named none (which only an
    /// `index` or a `create` may do — the cluster mints one for it).
    id: String,
    /// The action's own metadata, which carries the conditional revision and the pipeline.
    meta: Value,
    /// The source line that followed it, for an action that carries one.
    source: Option<Value>,
}

/// Apply every action the stream carries, in the order it carries them.
fn bulk(
    domain: &mut Domain,
    request: &Request<'_>,
    default_index: Option<&str>,
    now: SystemTime,
) -> RtResponse {
    let started = Instant::now();
    let actions = match parse(request, default_index) {
        Ok(actions) => actions,
        Err(refusal) => return *refusal,
    };
    // The pipeline the whole request names, which an action's own `pipeline` overrides.
    let pipeline = request.parameter("pipeline");
    let mut items: Vec<Value> = Vec::new();
    let mut errors = false;
    for action in actions {
        let item = apply(domain, request, &action, pipeline.as_deref(), now);
        errors = errors || item.failed;
        items.push(item_map(action.kind, item.reported));
    }
    let took = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    ok(&json!({ "took": took, "errors": errors, "items": items }))
}

/// What one action answered: the item the response carries for it, and whether it failed —
/// which is what `errors` reports over the whole request.
struct Item {
    reported: Value,
    failed: bool,
}

/// Read the newline-delimited stream the body carries.
///
/// A body that is not that stream is the whole request's own failure: the engine cannot
/// tell which items a malformed line was meant to be, so there is nothing to report an
/// item-level error against.
fn parse(
    request: &Request<'_>,
    default_index: Option<&str>,
) -> Result<Vec<Action>, Box<RtResponse>> {
    let body = ::std::str::from_utf8(request.body).map_err(|failed| {
        Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("failed to parse the bulk request body: {failed}"),
        ))
    })?;
    // A blank line carries no action and no source, which is what the trailing newline the
    // stream is terminated by leaves.
    let lines: Vec<&str> = body
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            "Validation Failed: 1: no requests added;",
        )));
    }
    let mut actions: Vec<Action> = Vec::new();
    let mut at = 0_usize;
    while at < lines.len() {
        // The line NUMBER the engine's own refusals quote is the line in the stream as it
        // was sent, counted from 1.
        let numbered = at.saturating_add(1);
        if actions.len() >= MAX_BULK_ACTIONS {
            return Err(Box::new(error(
                400,
                ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                &format!("a bulk request may carry at most {MAX_BULK_ACTIONS} actions"),
            )));
        }
        // The line is read as JSON first, so what the refusal names as found is the token
        // the line itself carries rather than one form's.
        let parsed: Value = ::serde_json::from_str(lines[at]).map_err(|failed| {
            malformed(numbered, &format!("failed to parse: {failed}"))
        })?;
        let Some((name, meta)) = parsed.as_object().and_then(|members| members.iter().next()) else {
            return Err(malformed(
                numbered,
                &format!(
                    "expected START_OBJECT but found [{}]",
                    token_kind(&parsed)
                ),
            ));
        };
        let Some(kind) = ACTIONS.iter().find(|action| *action == name) else {
            return Err(malformed(
                numbered,
                &format!(
                    "expected field [index], [create], [delete] or [update] but found [{name}]"
                ),
            ));
        };
        let index_name = meta
            .get(META_INDEX)
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| default_index.map(str::to_string))
            .unwrap_or_default();
        if index_name.is_empty() {
            return Err(Box::new(error(
                400,
                ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                "Validation Failed: 1: index is missing;",
            )));
        }
        let id = meta
            .get(META_ID)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        at = at.saturating_add(1);
        // Every action but a delete is followed by the document it acts with. A stream
        // that ends where that document was expected is one the client did not finish
        // sending, which is what the engine says about it.
        let source = if *kind == DELETE {
            None
        } else {
            let line = lines.get(at).ok_or_else(|| {
                Box::new(error(
                    400,
                    ingest::ILLEGAL_ARGUMENT_EXCEPTION,
                    "The bulk request must be terminated by a newline [\\n]",
                ))
            })?;
            at = at.saturating_add(1);
            let source: Value = ::serde_json::from_str(line)
                .map_err(|failed| malformed(at, &format!("failed to parse: {failed}")))?;
            if !source.is_object() {
                return Err(malformed(
                    at,
                    &format!(
                        "expected START_OBJECT but found [{}]",
                        token_kind(&source)
                    ),
                ));
            }
            Some(source)
        };
        actions.push(Action {
            kind,
            index_name,
            id,
            meta: meta.clone(),
            source,
        });
    }
    Ok(actions)
}

/// Apply ONE action, answering the item it earns.
fn apply(
    domain: &mut Domain,
    request: &Request<'_>,
    action: &Action,
    pipeline: Option<&str>,
    now: SystemTime,
) -> Item {
    // A write addressed to an ALIAS lands in the index that alias is the write index of,
    // which is what makes a write alias the address a producer keeps while the index
    // behind it is rotated.
    let index_name = match super::write_index(domain, &action.index_name) {
        Ok(index_name) => index_name,
        Err(refusal) => return failure(action, &action.index_name, &refusal),
    };
    match action.kind {
        DELETE => delete(domain, action, &index_name),
        UPDATE => update(domain, request, action, &index_name, pipeline, now),
        kind => index(
            domain,
            request,
            action,
            &index_name,
            if kind == CREATE {
                Creates::Only
            } else {
                Creates::Upsert
            },
            action.source.clone(),
            pipeline,
            now,
        ),
    }
}

/// An `index` or a `create` item: the source the action carried, stored through the one
/// write path.
#[allow(clippy::too_many_arguments)]
fn index(
    domain: &mut Domain,
    request: &Request<'_>,
    action: &Action,
    index_name: &str,
    creates: Creates,
    source: Option<Value>,
    pipeline: Option<&str>,
    now: SystemTime,
) -> Item {
    if let Some(refusal) = write_conflict(
        domain,
        index_name,
        Some(&action.id),
        creates,
        revision(&action.meta),
    ) {
        return failure(action, index_name, &refusal);
    }
    let Some(mut source) = source else {
        return failure(
            action,
            index_name,
            &Refusal::of_illegal_argument("Validation Failed: 1: source is missing;"),
        );
    };
    // The pipeline the action names, or the one the request named for every action.
    let named = action
        .meta
        .get("pipeline")
        .and_then(Value::as_str)
        .or(pipeline);
    if let Some(named) = named {
        if let Err(refusal) = apply_pipeline(domain, named, &mut source) {
            return failure(action, index_name, &refusal);
        }
    }
    let document = match ::serde_json::to_vec(&source) {
        Ok(document) => document,
        Err(failed) => {
            return failure(
                action,
                index_name,
                &Refusal::of_mapper_parsing(&format!("failed to render the document: {failed}")),
            )
        }
    };
    let address = domain.arn.clone();
    let write = ingest::DocumentWrite {
        domain: &address,
        index_name,
        document_id: Some(action.id.as_str()).filter(|id| !id.is_empty()),
        document: &document,
        principal_arn: request.principal,
        now,
    };
    // Whether this item will CREATE the index it names, read before it is applied, for the
    // reason every write route reads it ([`super::write_records`]).
    let auto_created = !domain.indexes.contains_key(index_name);
    // How long this ITEM's write takes is what the index's own indexing slow-log threshold
    // is compared against: each item is a write of its own, so each is timed on its own.
    let started = Instant::now();
    match ingest::index_into(domain, &write) {
        Ok(indexed) => {
            let took = started.elapsed();
            let Some(written) = super::written_document(domain, &indexed) else {
                return failure(
                    action,
                    index_name,
                    &super::unreadable_write(&indexed),
                );
            };
            record_written(
                domain,
                request,
                Written {
                    index_name: &indexed.index_name,
                    id: &indexed.document_id,
                    took,
                    source: ::std::str::from_utf8(&document).unwrap_or_default(),
                    auto_created,
                },
            );
            let created = written.version == 1;
            Item {
                reported: json!({
                    "_index": indexed.index_name,
                    "_id": indexed.document_id,
                    "_version": written.version,
                    "result": if created { "created" } else { "updated" },
                    "_shards": super::write_shards(domain, &indexed.index_name),
                    "_seq_no": written.seq_no,
                    "_primary_term": written.primary_term,
                    "status": if created { 201 } else { 200 },
                }),
                failed: false,
            }
        }
        Err(refusal) => failure(action, index_name, &Refusal::of_write(&refusal)),
    }
}

/// An `update` item: the partial document the action carried MERGED onto the one the index
/// holds, which is then stored through the same write path.
///
/// What the merge COMES TO is the one rule the update API states
/// ([`document_update::updated`]), so an item of a stream and a request of its own change
/// the stored document the same way.
fn update(
    domain: &mut Domain,
    request: &Request<'_>,
    action: &Action,
    index_name: &str,
    pipeline: Option<&str>,
    now: SystemTime,
) -> Item {
    let body = action.source.clone().unwrap_or(Value::Null);
    let holds_index = domain.indexes.contains_key(index_name);
    let held = domain
        .indexes
        .get(index_name)
        .and_then(|index| index.documents.get(&action.id))
        .cloned();
    let source = match document_update::updated(
        index_name,
        &action.id,
        holds_index,
        held.as_ref().map(|document| &document.source),
        &body,
    ) {
        Ok(document_update::Updated::Stored(source)) => source,
        // The merge changed nothing, so nothing is written and the item reports the
        // revision the document is still at.
        Ok(document_update::Updated::Unchanged) => {
            return match &held {
                Some(document) => Item {
                    reported: with_status(
                        document_update::noop_answer(index_name, &action.id, document),
                        200,
                    ),
                    failed: false,
                },
                None => failure(
                    action,
                    index_name,
                    &Refusal::of_document_missing(&action.id),
                ),
            }
        }
        Err(refusal) => return failure(action, index_name, &refusal),
    };
    index(
        domain,
        request,
        action,
        index_name,
        Creates::Upsert,
        Some(source),
        pipeline,
        now,
    )
}

/// One item's answer with the STATUS that item reports beside it, which is what a bulk item
/// carries and the single-request answer does not.
fn with_status(mut reported: Value, status: u16) -> Value {
    if let Some(members) = reported.as_object_mut() {
        members.insert("status".to_string(), json!(status));
    }
    reported
}

/// A `delete` item: the document the action addresses removed from the index, which takes
/// a sequence number of its own as every write does.
fn delete(domain: &mut Domain, action: &Action, index_name: &str) -> Item {
    if let Some(refusal) = write_conflict(
        domain,
        index_name,
        Some(&action.id),
        Creates::Upsert,
        revision(&action.meta),
    ) {
        return failure(action, index_name, &refusal);
    }
    let shards = super::write_shards(domain, index_name);
    let Some(index) = domain.indexes.get_mut(index_name) else {
        return failure(
            action,
            index_name,
            &Refusal::of_index_not_found(index_name),
        );
    };
    let removed = index.documents.remove(&action.id);
    let version = removed
        .as_ref()
        .map_or(1, |document| document.version.saturating_add(1));
    let seq_no = index.take_seq_no();
    Item {
        reported: json!({
            "_index": index_name,
            "_id": action.id,
            "_version": version,
            "result": if removed.is_some() { "deleted" } else { "not_found" },
            "_shards": shards,
            "_seq_no": seq_no,
            "_primary_term": ingest::PRIMARY_TERM,
            "status": if removed.is_some() { 200 } else { 404 },
        }),
        failed: false,
    }
}

/// One failed item: its own status and the engine's error, which is what makes a bulk
/// answer 200 with a failure inside it rather than a refusal of the whole request.
fn failure(action: &Action, index_name: &str, refusal: &Refusal) -> Item {
    Item {
        reported: json!({
            "_index": index_name,
            "_id": action.id,
            "status": refusal.status(),
            "error": refusal.item_error(),
        }),
        failed: true,
    }
}

/// The conditional revision an action's metadata names, read as the engine reads it: both
/// members together or neither.
fn revision(meta: &Value) -> Option<(u64, u64)> {
    super::conditional_revision(
        meta.get("if_seq_no").and_then(Value::as_u64),
        meta.get("if_primary_term").and_then(Value::as_u64),
    )
}

/// The refusal a line the stream's grammar does not admit earns, worded as the engine
/// words it: the line it was reading, and what it expected there.
fn malformed(line: usize, expected: &str) -> Box<RtResponse> {
    Box::new(error(
        400,
        ingest::ILLEGAL_ARGUMENT_EXCEPTION,
        &format!("Malformed action/metadata line [{line}], {expected}"),
    ))
}

/// The token a value IS, as the engine's own parser names the token it found where it
/// expected the start of an object.
fn token_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "VALUE_NULL",
        Value::Bool(_) => "VALUE_BOOLEAN",
        Value::Number(_) => "VALUE_NUMBER",
        Value::String(_) => "VALUE_STRING",
        Value::Array(_) => "START_ARRAY",
        // An object naming no member at all: the parser reached its end where a member
        // was expected.
        Value::Object(_) => "END_OBJECT",
    }
}

/// One item of the answer: a map whose single member is the action's own name, which is
/// the shape a bulk item has.
fn item_map(kind: &str, reported: Value) -> Value {
    let mut item = Map::new();
    item.insert(kind.to_string(), reported);
    Value::Object(item)
}
