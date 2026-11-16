// SPDX-License-Identifier: CC0-1.0

//! @op DeletePropertygraphStatistics
//!
//! HAND-AUTHORED: the scaffold op-stub generator declined this operation because its
//! input is `smithy.api#Unit` (see `SKIPPED_STUBS.md`), so the input type, its schema
//! and the output shapes are declared here rather than under `types/`. Everything else
//! follows the generated siblings exactly: the `#[operation_error]` enum, the
//! `Scaffolded` error channel, the engine-owning struct and the one `impl Operation`.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected). The store is reached through
//! [`crate::state::service_state`], and what this operation removes is the statistics
//! record [`crate::state::StatisticsRecord`] the engine holds for property-graph data, so
//! `GetPropertygraphStatistics` has nothing left to report afterwards.
//!
//! Which HTTP status the delete answers with follows from what it found:
//! `statusCode` is "200 if the delete was successful, or 204 if there were no statistics
//! to delete", and the member binds the response status (`@httpResponseCode`), so the
//! answer's status is the one the member carries.
//!
//! ---
//!
//! # Modeled contract for `DeletePropertygraphStatistics`
//!
//! Deletes statistics for Gremlin and openCypher (property graph) data. When invoking this
//! operation in a Neptune cluster that has IAM authentication enabled, the IAM user or role
//! making the request must have a policy attached that allows the
//! neptune-db:DeleteStatistics IAM action in that cluster. For the full documentation of
//! any one member:
//!
//! ```text
//! smithy_query(query="doc",shape="DeletePropertygraphStatisticsOutput",member="<Member>",model_path="services/service-com-amazonaws-neptunedata-amazonneptunedataplane/smithy-model/com-amazonaws-neptunedata-amazonneptunedataplane.json")
//! ```
//!
//! * **Route:** `DELETE /propertygraph/statistics`, `@idempotent`
//! * **Input:** none (`smithy.api#Unit`)
//!
//! ## Output
//!
//! * `statusCode` (Integer, `@httpResponseCode`) — The HTTP response code: 200 if the
//!   delete was successful, or 204 if there were no statistics to delete.
//! * `status` (String) — The cancel status.
//! * `payload` (DeleteStatisticsValueMap) — The deletion payload: `active` (the current
//!   status of the statistics) and `statisticsId` (the ID of the statistics generation run
//!   that is currently occurring).
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 403) — Raised in case of an authentication or
//!   authorization failure.
//! * `BadRequestException` (HTTP 400) — Raised when a request is submitted that cannot be
//!   processed.
//! * `ClientTimeoutException` (HTTP 408) — Raised when a request timed out in the client.
//! * `ConstraintViolationException` (HTTP 400) — Raised when a value in a request field did
//!   not satisfy required constraints.
//! * `IllegalArgumentException` (HTTP 400) — Raised when an argument in a request is not
//!   supported.
//! * `InvalidArgumentException` (HTTP 400) — Raised when an argument in a request has an
//!   invalid value.
//! * `InvalidParameterException` (HTTP 400) — Raised when a parameter value is not valid.
//! * `MissingParameterException` (HTTP 400) — Raised when a required parameter is missing.
//! * `PreconditionsFailedException` (HTTP 400) — Raised when a precondition for processing a
//!   request is not satisfied.
//! * `ReadOnlyViolationException` (HTTP 400) — Raised when a request attempts to write to a
//!   read-only resource.
//! * `StatisticsNotAvailableException` (HTTP 400) — Raised when statistics needed to satisfy
//!   a request are not available.
//! * `TooManyRequestsException` (HTTP 429) — Raised when the number of requests being
//!   processed exceeds the limit.
//! * `UnsupportedOperationException` (HTTP 400) — Raised when a request attempts to initiate
//!   an operation that is not supported.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `DeletePropertygraphStatisticsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-neptunedata-amazonneptunedataplane.json");

/// This operation's input: the model gives it `smithy.api#Unit`, so there is nothing to
/// read off the request.
///
/// Declared here with the empty-shape form of the `#[smithy]` macro, which emits the
/// `Schema`, `DELETE_PROPERTYGRAPH_STATISTICS_INPUT_SCHEMA` and the serde impls the
/// centralized request walk needs. The `input` key is what names the shape without looking
/// it up in the service model: `smithy.api#Unit` is a Smithy prelude shape, so the model
/// JSON does not carry it. No member is HTTP-bound, so the schema needs no URI template.
#[smithy_macros::smithy(input = "smithy.api#Unit")]
#[derive(Debug, Clone, Default)]
pub struct DeletePropertygraphStatisticsInput {}

/// What a delete reports about the statistics it left behind.
#[smithy_macros::smithy(shape = "com.amazonaws.neptunedata#DeleteStatisticsValueMap")]
#[derive(Debug, Clone, Default)]
pub struct DeleteStatisticsValueMap {
    pub active: Option<bool>,
    pub statistics_id: Option<String>,
}

/// This operation's output, as the model declares it.
#[smithy_macros::smithy(shape = "com.amazonaws.neptunedata#DeletePropertygraphStatisticsOutput")]
#[derive(Debug, Clone, Default)]
pub struct DeletePropertygraphStatisticsOutput {
    pub status_code: Option<i32>,
    pub status: Option<String>,
    pub payload: Option<DeleteStatisticsValueMap>,
}

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `DeletePropertygraphStatisticsError` alias below.
#[smithy_macros::operation_error("com.amazonaws.neptunedata#DeletePropertygraphStatistics")]
#[derive(Debug)]
pub enum DeletePropertygraphStatisticsErrorModeled {
    AccessDeniedException(AccessDeniedException),
    BadRequestException(BadRequestException),
    ClientTimeoutException(ClientTimeoutException),
    ConstraintViolationException(ConstraintViolationException),
    IllegalArgumentException(IllegalArgumentException),
    InvalidArgumentException(InvalidArgumentException),
    InvalidParameterException(InvalidParameterException),
    MissingParameterException(MissingParameterException),
    PreconditionsFailedException(PreconditionsFailedException),
    ReadOnlyViolationException(ReadOnlyViolationException),
    StatisticsNotAvailableException(StatisticsNotAvailableException),
    TooManyRequestsException(TooManyRequestsException),
    UnsupportedOperationException(UnsupportedOperationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::delete_propertygraph_statistics::DeletePropertygraphStatisticsError`).
pub type DeletePropertygraphStatisticsError =
    crate::scaffolded::Scaffolded<DeletePropertygraphStatisticsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so the handler
/// writes `Err(SomeModeledError.into())` rather than naming the wrapper variant.
impl From<DeletePropertygraphStatisticsErrorModeled> for DeletePropertygraphStatisticsError {
    fn from(modeled: DeletePropertygraphStatisticsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `DeletePropertygraphStatistics` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type.
pub struct DeletePropertygraphStatistics<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> DeletePropertygraphStatistics<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

/// The `statusCode` of a delete that removed statistics: "200 if the delete was
/// successful".
const DELETED: i32 = 200;

/// The `statusCode` of a delete that found nothing to remove: "204 if there were no
/// statistics to delete". A 204 answer carries no content, so the members that describe
/// what was deleted are absent from it.
const NOTHING_TO_DELETE: i32 = 204;

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for DeletePropertygraphStatistics<Eng>
{
    type Input = DeletePropertygraphStatisticsInput;
    type Output = DeletePropertygraphStatisticsOutput;
    type Error = DeletePropertygraphStatisticsError;

    const NAME: &'static str = "DeletePropertygraphStatistics";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema =
        DELETE_PROPERTYGRAPH_STATISTICS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema =
        DELETE_PROPERTYGRAPH_STATISTICS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let _ = &input;
        let Some(state) = crate::state::service_state(&*self.engine) else {
            record_access(ctx, "store-unavailable");
            return Err(
                DeletePropertygraphStatisticsErrorModeled::BadRequestException(
                    crate::state::store_unavailable_bad_request(),
                )
                .into(),
            );
        };

        if !state.lock().delete_propertygraph_statistics() {
            record_access(ctx, "nothing-to-delete");
            return Ok(DeletePropertygraphStatisticsOutput {
                status_code: Some(NOTHING_TO_DELETE),
                status: None,
                payload: None,
            });
        }
        record_access(ctx, "statistics-deleted");

        Ok(DeletePropertygraphStatisticsOutput {
            status_code: Some(DELETED),
            status: Some(crate::state::STATISTICS_REQUEST_SERVED.to_string()),
            payload: Some(DeleteStatisticsValueMap {
                // What the delete left behind: no statistics are in use, and no generation
                // run is occurring, which the statistics id states as -1.
                active: Some(false),
                statistics_id: Some(crate::state::NO_STATISTICS_ID.to_string()),
            }),
        })
    }
}

/// Record one delete: what was asked (the method and the URI), who asked (the access key
/// the request presented) and how it ended — including the call that found nothing to
/// delete, so every attempt to remove the statistics leaves a record.
///
/// No secret is recorded: the access key id is an identifier, never the signing key.
fn record_access(ctx: &smithy_server_runtime::OperationContext<'_>, outcome: &'static str) {
    let parts = ctx.parts();
    ::tracing::info!(
        operation = "DeletePropertygraphStatistics",
        method = %parts.method,
        uri = %parts.uri,
        akid = ?::emulator_server::extract_akid(&parts.headers),
        outcome,
        "neptunedata delete propertygraph statistics"
    );
}
