// SPDX-License-Identifier: CC0-1.0

//! WHICH of this service's operations is serving the request, for the one refusal fact that is
//! about the OPERATION rather than about the value refused: the error SHAPE a not-found refusal
//! of the TAGGING surface is named under ([`super::error_type_locator`]).
//!
//! RULE: the operations addressed by a RESOURCE ARN — `TagResource`, `UntagResource` and
//! `ListTagsForResource`, the three the model binds to `/tags/{resourceArn}` — are fronted by a
//! stack of their own, whose refusals carry its own generic error identity. Every operation
//! addressed by a resource ID — `GetApplication`, `GetJobRun`, `GetSession`, … — keeps the
//! modelled shape its own handler raised.
//!
//! Neither seam that words a refusal can decide that: `Service::override_refusal` is handed the
//! violations ALONE — no request and no operation — and a handler cannot reach the rendering of
//! its own error. So the shape is restated on the way OUT of the operation, where the operation
//! that answered is known.
//!
//! None of the plumbing is this crate's either: `service_common::restated_refusal` is the
//! fleet's one home for it, and [`SERVED_OPERATION_LAYER`] is its wrapper, handed back from
//! `Service::override_operation`. It serves the modelled operation the generated routing table
//! names — so the framework keeps the whole deserialize -> validate -> handle -> serialize walk
//! and every status, code and word it answers with — with the served operation in scope for the
//! run, and then hands what it rendered to this service's own restatement.
//!
//! This file states only the fact that is THIS service's: which operations that front door
//! answers for ([`TAGGING_OPERATIONS`]), read off the model.

use aws_smithy_runtime_api::http::{Request as WireRequest, Response as WireResponse};
use service_common::restated_refusal::{served_operation, RestatedRefusalOperation};

use super::error_type_locator::answered;

/// The operations the tagging front door answers for: the ones the model binds to
/// `/tags/{resourceArn}`, which are exactly the ones addressed by a resource ARN rather than by
/// a resource id (`smithy_query(query="digest")`).
///
/// Sorted, which [`fronts_the_tagging_surface`] relies on for `binary_search` and
/// `the_operations_are_sorted` pins.
const TAGGING_OPERATIONS: &[&str] = &["ListTagsForResource", "TagResource", "UntagResource"];

/// Whether `operation` is one that front door answers for.
fn fronts_the_tagging_surface(operation: &str) -> bool {
    TAGGING_OPERATIONS.binary_search(&operation).is_ok()
}

/// `resp` as this service answers it, for a request the wrapper below served: a not-found
/// refusal of the tagging surface names the front door's own shape
/// ([`super::error_type_locator::answered`]).
///
/// Outside a served operation there is no operation's identity to name, and an operation that
/// front door does not cover keeps what its own serializer wrote, so both are answered exactly
/// as they were rendered. Nothing is noted by the refusal seam: the words it chose are already
/// in the body it rendered, and only the shape NAME is restated here.
fn with_the_front_door_identity(
    _req: &WireRequest,
    resp: WireResponse,
    _noted: Option<String>,
) -> WireResponse {
    match served_operation() {
        Some(served) if fronts_the_tagging_surface(&served) => answered(resp),
        _ => resp,
    }
}

/// The layer every modelled operation of this service is served through: the fleet's wrapper,
/// which puts the served operation in scope for the whole run (read back by
/// [`with_the_front_door_identity`]) and answers what the run rendered through it.
///
/// A `static` so the reference `Service::override_operation` hands back outlives the call, and
/// one value for the whole process because the wrapper holds nothing per service but that one
/// function.
pub(crate) static SERVED_OPERATION_LAYER: RestatedRefusalOperation =
    RestatedRefusalOperation::new(with_the_front_door_identity);

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the operations the tagging front door covers are the ones addressed by a resource
    /// ARN, and only those.
    ///
    /// The rows span every operation of this model that takes a `resourceArn`, operations
    /// addressed by a resource id (an application's, a job run's and a session's alike), and a
    /// name the model does not bind at all.
    #[test]
    fn only_the_operations_addressed_by_a_resource_arn_are_fronted() {
        for (operation, expected) in [
            ("ListTagsForResource", true),
            ("TagResource", true),
            ("UntagResource", true),
            ("GetApplication", false),
            ("DeleteApplication", false),
            ("StartJobRun", false),
            ("GetJobRun", false),
            ("CancelJobRun", false),
            ("GetSession", false),
            ("NoSuchOperation", false),
        ] {
            let actual = fronts_the_tagging_surface(operation);
            assert_eq!(
                actual, expected,
                "case {operation:?}: expected {expected}, got {actual}"
            );
        }
    }

    /// [`TAGGING_OPERATIONS`] is searched by `binary_search`, which a list out of order answers
    /// wrongly for.
    #[test]
    fn the_operations_are_sorted() {
        let mut sorted = TAGGING_OPERATIONS.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            TAGGING_OPERATIONS,
            &sorted[..],
            "expected the operations sorted, got {TAGGING_OPERATIONS:?}"
        );
    }

    /// Outside a served operation there is no operation's identity to name, so the rendered
    /// response is answered as it was.
    #[tokio::test]
    async fn no_served_operation_restates_nothing() {
        let req = WireRequest::new(aws_smithy_types::body::SdkBody::empty());
        let rendered = WireResponse::new(
            aws_smithy_runtime_api::http::StatusCode::try_from(404u16).expect("404"),
            aws_smithy_types::body::SdkBody::from(
                r#"{"__type":"ResourceNotFoundException","message":"Resource was not found"}"#,
            ),
        );
        let answer = with_the_front_door_identity(&req, rendered, None);
        let document: serde_json::Value =
            serde_json::from_slice(answer.body().bytes().expect("an in-memory body"))
                .expect("the error body is JSON");
        assert_eq!(
            document["__type"].as_str(),
            Some(super::super::error_type_locator::RESOURCE_NOT_FOUND),
            "expected the rendered shape kept, got {document}"
        );
    }
}
