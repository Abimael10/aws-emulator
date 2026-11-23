// SPDX-License-Identifier: CC0-1.0

//! The DELETE BY QUERY API: `POST /{index}/_delete_by_query`, which "deletes documents that
//! match the specified query" — the one way a client empties part of an index without
//! knowing the ids it holds.
//!
//! The documents it deletes are the ones the QUERY selects, resolved through the data
//! plane's own search ([`super::selected_documents`]): the same query vocabulary, the same
//! index-pattern resolution and the same analysis chains, so a delete removes exactly the
//! documents a search with that query reports. The counts it answers are what it really did —
//! the documents the query matched, the ones removed, and the batches that took — and a
//! request naming `max_docs` has that many removed and no more.
//!
//! Two FORMS, and which one a request asked for is its own `wait_for_completion`: a request
//! that waits is answered with the finished report, and a request that sets the parameter
//! false is answered with a task HANDLE and nothing else, while the write runs in the
//! background and the client watches it through `_tasks/<handle>` ([`super::tasks`]). The
//! figures either form reports are read from the request's own parameters — the `scroll_size`
//! its scroll pages in and the `requests_per_second` it is throttled to — through the one
//! report both forms are rendered from ([`state::by_query_tasks`]).

use std::collections::VecDeque;

use aws_smithy_runtime_api::http::Response as RtResponse;
use serde_json::{json, Value};

use crate::state::{
    self, by_query_tasks::ByQueryReport, fgac, ingest, ByQueryTask, Domain,
    DEFAULT_SCROLL_SIZE, SubmittedByQuery, TASK_QUOTA, UNTHROTTLED_REQUESTS_PER_SECOND,
};

use super::{
    error, guarded, ok, wrong_method, Answering, CrossCluster, Request, MAX_RESULT_WINDOW,
};

/// How many batches one request may delete in.
///
/// The engine deletes in batches of its scroll window and keeps going until the query
/// selects nothing more; this is the emulator's own ceiling on how many of those batches ONE
/// request performs, so the work it does is bounded by a constant and not by the documents a
/// pattern resolves to. A request that reaches it reports the batches it really performed and
/// the documents it really deleted, which is what tells a client there is more to delete.
const MAX_BATCHES: usize = 16;

/// `POST /{index}/_delete_by_query`.
pub(super) fn delete_by_query_request(
    domain: &mut Domain,
    request: &Request<'_>,
    index_name: &str,
    files: &crate::state::analysis::PackageFiles,
) -> RtResponse {
    match request.method {
        "POST" => guarded(domain, request, fgac::ACTION_DELETE_BY_QUERY, |domain| {
            delete_by_query(domain, request, index_name, files)
        }),
        _ => wrong_method(request, "POST"),
    }
}

/// Delete every document of `index_name` the request's query selects, or — for a request that
/// does not wait for the write — hand back the handle of the run that will.
fn delete_by_query(
    domain: &mut Domain,
    request: &Request<'_>,
    index_name: &str,
    files: &crate::state::analysis::PackageFiles,
) -> RtResponse {
    // "The request body must contain a query" — a request carrying none selects nothing to
    // delete, and the engine refuses it rather than deleting the whole index.
    let body = match request.json() {
        Ok(body) => body.unwrap_or(Value::Null),
        Err(refusal) => return *refusal,
    };
    if !body.get(QUERY_MEMBER).is_some_and(Value::is_object) {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("[{QUERY_MEMBER}] required property is missing"),
        );
    }
    // "max_docs — the maximum number of documents to delete", which a client sends as a
    // safety limit on a destructive delete: the documents past it are left, and the answer
    // reports what was really deleted.
    let limit = match requested_limit(&body, request) {
        Ok(limit) => limit,
        Err(refusal) => return *refusal,
    };
    // How the scroll is paged and how fast the write may go, both read from the request
    // itself: they decide the batch count and the throttle every answer below reports, and
    // the pace a background run is removed at.
    let scroll_size = match requested_scroll_size(request) {
        Ok(scroll_size) => scroll_size,
        Err(refusal) => return *refusal,
    };
    let throttle = match requested_throttle(request) {
        Ok(throttle) => throttle,
        Err(refusal) => return *refusal,
    };
    let waits = match waits_for_completion(request) {
        Ok(waits) => waits,
        Err(refusal) => return *refusal,
    };
    if waits {
        return performed(
            domain,
            request,
            index_name,
            files,
            Asked {
                limit,
                scroll_size,
                throttle,
            },
        );
    }
    submitted(
        domain,
        request,
        index_name,
        files,
        Asked {
            limit,
            scroll_size,
            throttle,
        },
    )
}

/// What the request asked of the write, apart from the query that selects its documents.
#[derive(Debug, Clone, Copy)]
struct Asked {
    /// `max_docs`: the most documents the write may remove.
    limit: usize,
    /// `scroll_size`: the documents one batch of its scroll carries.
    scroll_size: usize,
    /// `requests_per_second`: the rate it may remove them at, or
    /// [`UNTHROTTLED_REQUESTS_PER_SECOND`] for a request that asked for no throttle.
    throttle: f64,
}

/// Perform the write NOW and answer the report it came to — the form a request that waits for
/// the write asked for.
fn performed(
    domain: &mut Domain,
    request: &Request<'_>,
    index_name: &str,
    files: &crate::state::analysis::PackageFiles,
    asked: Asked,
) -> RtResponse {
    let sorting = super::search::Sorting::default();
    let highlighting = super::search::Highlighting::default();
    let selecting = Answering {
        from: 0,
        // One batch is the engine's own result window, which is also the most documents one
        // index of this emulator holds.
        size: MAX_RESULT_WINDOW,
        sorting: &sorting,
        highlighting: &highlighting,
    };
    let mut matched = 0usize;
    let mut deleted = 0usize;
    let mut batches = 0usize;
    // The documents still selected when the loop stopped, which is what says whether the
    // delete finished.
    let mut remaining = 0usize;
    while batches < MAX_BATCHES {
        let selected = match super::selected_documents(
            domain,
            request,
            index_name,
            &CrossCluster::default(),
            files,
            &selecting,
        ) {
            Ok(selected) => selected,
            Err(refusal) => return *refusal,
        };
        // The total is read from EVERY batch, since each one is a fresh search over what the
        // indexes hold now: the figure reported is the largest the delete ever saw selected.
        matched = matched.max(selected.0);
        remaining = selected.0;
        if selected.1.is_empty() {
            break;
        }
        batches += 1;
        for selection in selected.1 {
            if deleted >= asked.limit {
                break;
            }
            let (index_of_hit, id, _source) = selection.hit;
            let Some(index) = domain.indexes.get_mut(&index_of_hit) else {
                continue;
            };
            if index.documents.remove(&id).is_some() {
                // A delete is a write, so it takes a sequence number of its own: a client
                // reading the index afterwards can tell the revision it is at.
                let _seq_no = index.take_seq_no();
                deleted += 1;
                remaining = remaining.saturating_sub(1);
            }
        }
        if deleted >= asked.limit {
            break;
        }
    }
    // The documents were selected by a QUERY over the indexes the path names, so each of
    // them was READ: the domain's search slow log has a record of that read where the
    // index's own threshold makes it slow, exactly as it has one for a `_search` over the
    // same documents ([`super::record_searched`]).
    super::record_searched(domain, request, index_name);
    ok(&ByQueryReport {
        took: request.took(),
        // A delete that stopped with documents it had not reached is one that did not
        // finish, which the engine signals rather than reporting an unqualified success. A
        // delete that stopped because the request's own `max_docs` was reached DID finish
        // what it was asked for.
        timed_out: remaining > 0 && deleted < asked.limit,
        // "The number of documents that were successfully processed", which a request capping
        // the write with `max_docs` stops at that cap — the same figure the offloaded form
        // reports for the subset its scroll took, so one request cannot report two totals
        // depending on which form it asked for.
        total: matched.min(asked.limit),
        deleted,
        // The documents the scroll paged through, which is what its batch count is of.
        processed: deleted,
        scroll_size: asked.scroll_size,
        requests_per_second: asked.throttle,
        // A request that WAITS is answered as soon as the write is done: nothing padded it,
        // because this plane never holds a caller's connection open for a span.
        throttled: ::std::time::Duration::ZERO,
    }
    .rendered())
}

/// Register the write as a background run and answer its HANDLE — the form a request that
/// does not wait for the write asked for.
///
/// The subset is taken HERE, from one selection over the indexes the path names: a by-query
/// write scrolls the documents it will remove at the start, so the run removes exactly the
/// documents the query selected when it was submitted and a document written afterwards is
/// not one of them.
fn submitted(
    domain: &mut Domain,
    request: &Request<'_>,
    index_name: &str,
    files: &crate::state::analysis::PackageFiles,
    asked: Asked,
) -> RtResponse {
    if domain.by_query_tasks_running() >= TASK_QUOTA {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "cluster [{}] is already running the maximum of {TASK_QUOTA} by-query \
                 tasks, so the delete could not be started",
                super::cluster_name(domain)
            ),
        );
    }
    let sorting = super::search::Sorting::default();
    let highlighting = super::search::Highlighting::default();
    let selecting = Answering {
        from: 0,
        size: MAX_RESULT_WINDOW,
        sorting: &sorting,
        highlighting: &highlighting,
    };
    let selected = match super::selected_documents(
        domain,
        request,
        index_name,
        &CrossCluster::default(),
        files,
        &selecting,
    ) {
        Ok(selected) => selected,
        Err(refusal) => return *refusal,
    };
    // The scroll named the documents it named; `max_docs` is what the client asked to have
    // removed of them, so the run is over that many and no more.
    let documents: VecDeque<(String, String)> = selected
        .1
        .into_iter()
        .take(asked.limit)
        .map(|selection| (selection.hit.0, selection.hit.1))
        .collect();
    // The subset was selected by a QUERY, so the same read a waiting request records is
    // recorded for this one.
    super::record_searched(domain, request, index_name);
    let node = state::logging_node_id(domain);
    let number = domain.take_by_query_task_number();
    let task = ByQueryTask::submitted(
        SubmittedByQuery {
            node,
            number,
            action: fgac::ACTION_DELETE_BY_QUERY,
            description: format!("delete-by-query [{index_name}]"),
            scroll_size: asked.scroll_size,
            requests_per_second: asked.throttle,
        },
        documents,
        request.at,
    );
    let handle = domain.register_by_query_task(task);
    // ONLY the handle: a client that offloaded the write reads it to learn what to poll, and
    // the counts belong to the run rather than to this answer.
    ok(&json!({ TASK_MEMBER: handle }))
}

/// Whether the request WAITS for the write: `wait_for_completion` "waits for the matching
/// tasks to complete" and defaults to true, so the asynchronous form is the one that turns it
/// off.
///
/// The engine reads a boolean parameter as exactly one of two words and refuses every other
/// value, which is what keeps a client that spelled it some other way from being served the
/// form it did not ask for.
fn waits_for_completion(request: &Request<'_>) -> Result<bool, Box<RtResponse>> {
    let Some(named) = request.parameter(WAIT_FOR_COMPLETION_MEMBER) else {
        return Ok(true);
    };
    match named.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("Failed to parse value [{named}] as only [true] or [false] are allowed."),
        ))),
    }
}

/// The most documents this request asked to delete: its own `max_docs`, in the body or in
/// the query string, and everything the query selects for a request naming none.
///
/// A value that is not a count at all is refused rather than read as "every document": a
/// client that sent one asked for a LIMIT, and ignoring it would delete more than it asked
/// for.
fn requested_limit(body: &Value, request: &Request<'_>) -> Result<usize, Box<RtResponse>> {
    let named = body
        .get(MAX_DOCS_MEMBER)
        .cloned()
        .or_else(|| request.parameter(MAX_DOCS_MEMBER).map(Value::String));
    let Some(named) = named else {
        return Ok(usize::MAX);
    };
    let counted = match &named {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.parse::<u64>().ok(),
        _ => None,
    };
    match counted {
        Some(counted) => Ok(usize::try_from(counted).unwrap_or(usize::MAX)),
        None => Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("[{MAX_DOCS_MEMBER}] must be a non-negative number, and this request \
                      names {named}"),
        ))),
    }
}

/// The documents one batch of this request's scroll carries: its own `scroll_size`, and the
/// engine's default batch size for a request naming none.
///
/// Refused rather than defaulted where the value is not a batch size at all, for the reason
/// [`requested_limit`] refuses one: the parameter decides how the write is paged, so reading
/// a value the client did not mean would report a paging it did not ask for. A value that is
/// not a count is the engine's own failure to read a numeric parameter; a batch of NO
/// documents is the engine's refusal of a scroll that could never page.
fn requested_scroll_size(request: &Request<'_>) -> Result<usize, Box<RtResponse>> {
    let Some(named) = request.parameter(SCROLL_SIZE_MEMBER) else {
        return Ok(DEFAULT_SCROLL_SIZE);
    };
    let Ok(size) = named.parse::<usize>() else {
        return Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "Failed to parse int parameter [{SCROLL_SIZE_MEMBER}] with value [{named}]"
            ),
        )));
    };
    if size == 0 {
        return Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            "[size] cannot be [0] in a scroll context",
        )));
    }
    Ok(size)
}

/// The rate this request asked the write to be throttled to: its own `requests_per_second`,
/// and [`UNTHROTTLED_REQUESTS_PER_SECOND`] for a request naming none.
///
/// "Set `requests_per_second` to `-1` to disable throttling", so that value is the one
/// spelling of "no throttle" and every other non-positive value names no rate at all.
fn requested_throttle(request: &Request<'_>) -> Result<f64, Box<RtResponse>> {
    let Some(named) = request.parameter(REQUESTS_PER_SECOND_MEMBER) else {
        return Ok(UNTHROTTLED_REQUESTS_PER_SECOND);
    };
    let rate = named.parse::<f64>().ok().filter(|rate| rate.is_finite());
    match rate {
        Some(rate) if rate > 0.0 => Ok(rate),
        Some(rate) if rate == UNTHROTTLED_REQUESTS_PER_SECOND => {
            Ok(UNTHROTTLED_REQUESTS_PER_SECOND)
        }
        _ => Err(Box::new(error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!(
                "[{REQUESTS_PER_SECOND_MEMBER}] must be a float greater than 0. Use -1 to \
                 disable throttling."
            ),
        ))),
    }
}

/// The member a request names its deletion limit in.
const MAX_DOCS_MEMBER: &str = "max_docs";

/// The member the request carries the query to delete by in.
const QUERY_MEMBER: &str = "query";

/// The parameter a request says whether it waits for the write in.
const WAIT_FOR_COMPLETION_MEMBER: &str = "wait_for_completion";

/// The parameter it names the size of one scroll batch in.
const SCROLL_SIZE_MEMBER: &str = "scroll_size";

/// The parameter it names the throttle in.
const REQUESTS_PER_SECOND_MEMBER: &str = "requests_per_second";

/// The ONE member the answer to a request that does not wait for the write carries: the
/// handle of the run it started.
const TASK_MEMBER: &str = "task";
