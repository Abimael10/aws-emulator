// SPDX-License-Identifier: CC0-1.0

//! WHICH of this service's operations is serving the request, for the refusal wording that
//! is a fact about the OPERATION rather than about the constraint alone.
//!
//! The framework refuses a request that breaks a rule the model declares BEFORE any handler
//! runs, and asks the service for the words through `Service::override_refusal`. That seam is
//! handed the violations ALONE: a member path, a constraint class and the value that sat
//! there, with no request and no operation. For most of this service's refusal wording that
//! is enough, because those rules are properties of the constraint. One is not: an input
//! list of resource identifiers left empty is refused with a sentence naming the KIND of
//! identifier the list holds, and the member path alone does not say which kind — `names`
//! carries a list of project names on one operation and of fleet names on another
//! (`BatchGetProjectsInput$names` targets `ProjectNames`, `BatchGetFleetsInput$names`
//! targets `FleetNames`), and a member schema carries its target's KIND but not its target's
//! name, so nothing of the model distinguishes them without knowing which operation was
//! called.
//!
//! None of the plumbing that closes that gap is this service's: `service_common::restated_refusal`
//! is the workspace's one home for it, and [`SERVED_OPERATION_LAYER`] is its wrapper, handed
//! back from `Service::override_operation`. It serves the modelled operation the generated
//! routing table names — so the framework keeps the whole deserialize → validate → handle →
//! serialize walk and every status, code and word it answers with — with the served operation
//! in scope for the run, which [`served_input`] reads back.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::Schema;
use service_common::restated_refusal::{served_operation, RestatedRefusalOperation};

/// The input schema of the operation being served.
///
/// `None` outside a served request — the refusal the router raises before any operation is
/// reached — where no operation's wording applies and the shared envelope stands. The schema
/// rather than the name, because what a sentence is keyed to is a MEMBER of that input
/// (`super::refusal`), which the schema is what names.
pub(super) fn served_input() -> Option<&'static Schema> {
    super::input_schema(&served_operation()?)
}

/// `resp` as this service answers it: exactly as the framework rendered it.
///
/// Nothing about the response is this service's to restate — the code, the status and the
/// words are the ones `Service::override_refusal` and every operation's own error channel
/// already chose. This layer exists for the OTHER direction the wrapper carries: the served
/// operation in scope while the walk runs ([`served_input`]).
fn as_rendered(_req: &RtRequest, resp: RtResponse, _noted: Option<String>) -> RtResponse {
    resp
}

/// The layer every modelled operation of this service is served through: the workspace's
/// wrapper, which puts the served operation in scope for the whole run and answers what that
/// run rendered.
///
/// A `static` so the reference `Service::override_operation` hands back outlives the call, and
/// one value for the whole process because the wrapper holds nothing per service but that one
/// function.
pub(super) static SERVED_OPERATION_LAYER: RestatedRefusalOperation =
    RestatedRefusalOperation::new(as_rendered);
