// SPDX-License-Identifier: CC0-1.0

//! WHICH of this plane's operations is serving the request, for the one refusal rule that is a
//! fact about the OPERATION rather than about the constraint: the error SHAPE a boundary
//! refusal is named under.
//!
//! The framework refuses a request that breaks a constraint the model declares BEFORE any
//! handler runs, and asks the service for the words through `Service::override_refusal`. That
//! seam is handed the violations ALONE — no request and no operation — so the shape question
//! ([`super::error_type_locator`]: which client-input error does the operation being served
//! DECLARE?) cannot be answered there.
//!
//! None of the plumbing that closes that gap is this crate's: `service_common::restated_refusal`
//! is the fleet's one home for it, and [`SERVED_OPERATION_LAYER`] is its wrapper, handed back
//! from `Service::override_operation`. It serves the modelled operation the generated routing
//! table names — so the framework keeps the whole deserialize → validate → handle → serialize
//! walk and every status, code and word it answers with — with the served operation in scope
//! for the run, and then hands what it rendered to this plane's own restatement.
//!
//! This file states only the fact that is THIS plane's: which operations declare
//! [`INVALID_REQUEST_EXCEPTION`] for a refused request ([`INVALID_REQUEST_OPERATIONS`]), read
//! off the model.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use service_common::restated_refusal::{served_operation, RestatedRefusalOperation};

use super::error_type_locator::{answered, BOUNDARY_SHAPE, INVALID_REQUEST_EXCEPTION};

/// The operations that declare [`INVALID_REQUEST_EXCEPTION`] as their client-input error.
///
/// The model is what says so: `DescribeJobExecution`, `GetPendingJobExecutions`,
/// `StartNextPendingJobExecution` and `UpdateJobExecution` each declare
/// `InvalidRequestException` (`@httpError 400`, "The contents of the request were invalid.")
/// and do not declare `ValidationException`, while `StartCommandExecution` declares
/// `ValidationException` and not `InvalidRequestException`
/// (`smithy_query(query="digest")`). An operation absent from this list therefore keeps the
/// shape the framework's boundary renderer named, which is the one `StartCommandExecution`
/// models.
///
/// Sorted, which [`modelled_refusal_shape`] relies on for `binary_search` and
/// `the_operations_are_sorted` pins.
const INVALID_REQUEST_OPERATIONS: &[&str] = &[
    "DescribeJobExecution",
    "GetPendingJobExecutions",
    "StartNextPendingJobExecution",
    "UpdateJobExecution",
];

/// The client-input error shape `operation` declares: [`INVALID_REQUEST_EXCEPTION`] for the
/// operations that declare it, and otherwise [`BOUNDARY_SHAPE`] — which restates nothing, so an
/// operation this list does not name keeps exactly what the framework rendered.
fn modelled_refusal_shape(operation: &str) -> &'static str {
    if INVALID_REQUEST_OPERATIONS.binary_search(&operation).is_ok() {
        INVALID_REQUEST_EXCEPTION
    } else {
        BOUNDARY_SHAPE
    }
}

/// `resp` as this plane answers it, for a request the wrapper below served: a boundary refusal
/// names the client-input error shape the served operation declares
/// ([`super::error_type_locator::answered`]).
///
/// Outside a served operation there is no operation's shape to name, so the response is
/// answered exactly as it was rendered. Nothing is noted by the refusal seam: the words it
/// chose are already in the body it rendered, and only the shape NAME is restated here.
fn with_the_modelled_refusal_shape(
    _req: &RtRequest,
    resp: RtResponse,
    _noted: Option<String>,
) -> RtResponse {
    match served_operation() {
        Some(served) => answered(resp, modelled_refusal_shape(&served)),
        None => resp,
    }
}

/// The layer every modelled operation of this plane is served through: the fleet's wrapper,
/// which puts the served operation in scope for the whole run (read back by
/// [`with_the_modelled_refusal_shape`]) and answers what the run rendered through it.
///
/// A `static` so the reference `Service::override_operation` hands back outlives the call, and
/// one value for the whole process because the wrapper holds nothing per service but that one
/// function.
pub(crate) static SERVED_OPERATION_LAYER: RestatedRefusalOperation =
    RestatedRefusalOperation::new(with_the_modelled_refusal_shape);

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the shape a boundary refusal is named under is the client-input error the SERVED
    /// operation declares.
    ///
    /// The rows span every operation this model binds — the four that declare
    /// `InvalidRequestException` and the one that declares the boundary shape itself — plus a
    /// name the model does not bind, which must restate nothing.
    #[test]
    fn each_operation_names_the_shape_its_model_declares() {
        for (operation, expected) in [
            ("DescribeJobExecution", INVALID_REQUEST_EXCEPTION),
            ("GetPendingJobExecutions", INVALID_REQUEST_EXCEPTION),
            ("StartNextPendingJobExecution", INVALID_REQUEST_EXCEPTION),
            ("UpdateJobExecution", INVALID_REQUEST_EXCEPTION),
            ("StartCommandExecution", BOUNDARY_SHAPE),
            ("NoSuchOperation", BOUNDARY_SHAPE),
        ] {
            let actual = modelled_refusal_shape(operation);
            assert_eq!(
                actual, expected,
                "case {operation:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// [`INVALID_REQUEST_OPERATIONS`] is searched by `binary_search`, which a list out of order
    /// answers wrongly for.
    #[test]
    fn the_operations_are_sorted() {
        let mut sorted = INVALID_REQUEST_OPERATIONS.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            INVALID_REQUEST_OPERATIONS,
            &sorted[..],
            "expected the operations sorted, got {INVALID_REQUEST_OPERATIONS:?}"
        );
    }

    /// Outside a served operation there is no operation's shape to name, so the rendered
    /// response is answered as it was.
    #[tokio::test]
    async fn no_served_operation_restates_nothing() {
        let req = RtRequest::new(::aws_smithy_types::body::SdkBody::empty());
        let rendered = RtResponse::new(
            ::aws_smithy_runtime_api::http::StatusCode::try_from(400u16).expect("400"),
            ::aws_smithy_types::body::SdkBody::from(
                r#"{"__type":"ValidationException","message":"1 validation error detected: …"}"#,
            ),
        );
        let answer = with_the_modelled_refusal_shape(&req, rendered, None);
        let body: ::serde_json::Value =
            ::serde_json::from_slice(answer.body().bytes().expect("an in-memory body"))
                .expect("the error body is JSON");
        assert_eq!(
            body["__type"].as_str(),
            Some(BOUNDARY_SHAPE),
            "expected the rendered shape kept, got {body}"
        );
    }
}
