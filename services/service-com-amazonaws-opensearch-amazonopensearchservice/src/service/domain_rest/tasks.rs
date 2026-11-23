// SPDX-License-Identifier: CC0-1.0

//! The TASK API: `GET /_tasks/{task_id}`, the route a client watches work it offloaded to
//! the cluster through.
//!
//! A by-query write a client asked for with `wait_for_completion=false` is answered with a
//! HANDLE and nothing else ([`super::delete_by_query`]), and this is where that handle leads:
//! the answer says whether the run has finished (`completed`), carries the TASK itself — the
//! node and id the handle is composed of, the engine action it is a run of, and the counts it
//! has reached so far — and, once the run has finished, the `response` it came to, which is
//! the same bulk-by-query report a client that waited for the write would have been answered
//! with ([`crate::state::by_query_tasks`]).
//!
//! Nothing here writes: the documents a run removes are removed by the clock that paces it
//! ([`crate::state::Domain::settle_by_query_tasks`]), which every read of the domain
//! materialises before a route is reached — so polling a handle neither starts nor advances
//! the work, and a client that never polls sees the same documents gone at the same instant.

use aws_smithy_runtime_api::http::Response as RtResponse;

use crate::state::{fgac, ingest, Domain};

use super::{error, guarded, ok, wrong_method, Request, RESOURCE_NOT_FOUND_EXCEPTION};

/// The separator a task handle composes its two halves with: `<node>:<id>`.
const HANDLE_SEPARATOR: char = ':';

/// `GET /_tasks/{task_id}`.
pub(super) fn task_request(
    domain: &mut Domain,
    request: &Request<'_>,
    task_id: &str,
) -> RtResponse {
    match request.method {
        "GET" => guarded(domain, request, fgac::ACTION_GET_TASK, |domain| {
            polled(domain, request, task_id)
        }),
        _ => wrong_method(request, "GET"),
    }
}

/// What the run `task_id` names has come to.
///
/// A value that is not a handle at all names no run and is refused as the argument it is; a
/// handle this cluster holds no run for is the missing resource the engine reports, which is
/// also what a run whose record the cluster no longer keeps answers.
fn polled(domain: &Domain, request: &Request<'_>, task_id: &str) -> RtResponse {
    if !task_id.contains(HANDLE_SEPARATOR) {
        return error(
            400,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            &format!("malformed task id {task_id}"),
        );
    }
    let Some(task) = domain.by_query_tasks.get(task_id) else {
        return error(
            404,
            RESOURCE_NOT_FOUND_EXCEPTION,
            &format!("task [{task_id}] isn't running and hasn't stored its results"),
        );
    };
    ok(&task.polled(request.at))
}
