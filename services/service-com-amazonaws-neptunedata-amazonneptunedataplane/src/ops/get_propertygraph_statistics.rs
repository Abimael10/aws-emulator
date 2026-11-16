// SPDX-License-Identifier: CC0-1.0

//! @op GetPropertygraphStatistics
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
//! [`crate::state::service_state`], and what this operation reports is the statistics
//! record [`crate::state::StatisticsRecord`] the engine holds for property-graph data:
//! `ManagePropertygraphStatistics` writes it and `DeletePropertygraphStatistics` removes
//! it. A read of statistics the engine does not hold is refused with the modelled
//! `StatisticsNotAvailableException` rather than answered — there is nothing to report,
//! and a report of statistics nothing computed would be a fabrication.
//!
//! This is a `@readonly` operation: it reports the stored record and writes nothing, so
//! two reads in a row answer the same statistics.
//!
//! ---
//!
//! # Modeled contract for `GetPropertygraphStatistics`
//!
//! Gets property graph statistics (Gremlin and openCypher). When invoking this operation
//! in a Neptune cluster that has IAM authentication enabled, the IAM user or role making
//! the request must have a policy attached that allows the neptune-db:GetStatistics IAM
//! action in that cluster. For the full documentation of any one member:
//!
//! ```text
//! smithy_query(query="doc",shape="Statistics",member="<Member>",model_path="services/service-com-amazonaws-neptunedata-amazonneptunedataplane/smithy-model/com-amazonaws-neptunedata-amazonneptunedataplane.json")
//! ```
//!
//! * **Route:** `GET /propertygraph/statistics -> 200`, `@readonly`
//! * **Input:** none (`smithy.api#Unit`)
//!
//! ## Output
//!
//! * `status` (String) **required** — The HTTP return code of the request. If the request
//!   succeeded, the code is 200.
//! * `payload` (Statistics) **required** — Statistics for property-graph data:
//!   * `autoCompute` (Boolean) — Indicates whether or not automatic statistics generation
//!     is enabled.
//!   * `active` (Boolean) — Indicates whether or not DFE statistics generation is enabled
//!     at all.
//!   * `statisticsId` (String) — Reports the ID of the current statistics generation run.
//!     A value of -1 indicates that no statistics have been generated.
//!   * `date` (Timestamp, date-time) — The UTC time at which DFE statistics have most
//!     recently been generated.
//!   * `note` (String) — A note about problems in the case where statistics are invalid.
//!   * `signatureInfo` (StatisticsSummary) — `signatureCount` (the total number of
//!     signatures across all characteristic sets), `instanceCount` (the total number of
//!     characteristic-set instances) and `predicateCount` (the total number of unique
//!     predicates).
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
// `GetPropertygraphStatisticsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-neptunedata-amazonneptunedataplane.json");

/// This operation's input: the model gives it `smithy.api#Unit`, so there is nothing to
/// read off the request.
///
/// Declared here with the empty-shape form of the `#[smithy]` macro, which emits the
/// `Schema`, `GET_PROPERTYGRAPH_STATISTICS_INPUT_SCHEMA` and the serde impls the
/// centralized request walk needs. The `input` key is what names the shape without looking
/// it up in the service model: `smithy.api#Unit` is a Smithy prelude shape, so the model
/// JSON does not carry it. No member is HTTP-bound, so the schema needs no URI template.
#[smithy_macros::smithy(input = "smithy.api#Unit")]
#[derive(Debug, Clone, Default)]
pub struct GetPropertygraphStatisticsInput {}

/// The characteristic sets a statistics generation run counted.
#[smithy_macros::smithy(shape = "com.amazonaws.neptunedata#StatisticsSummary")]
#[derive(Debug, Clone, Default)]
pub struct StatisticsSummary {
    pub signature_count: Option<i32>,
    pub instance_count: Option<i32>,
    pub predicate_count: Option<i32>,
}

/// The statistics themselves, as the model declares them.
#[smithy_macros::smithy(shape = "com.amazonaws.neptunedata#Statistics")]
#[derive(Debug, Clone, Default)]
pub struct Statistics {
    pub auto_compute: Option<bool>,
    pub active: Option<bool>,
    pub statistics_id: Option<String>,
    pub date: Option<::aws_smithy_types::DateTime>,
    pub note: Option<String>,
    pub signature_info: Option<StatisticsSummary>,
}

/// This operation's output, as the model declares it.
#[smithy_macros::smithy(shape = "com.amazonaws.neptunedata#GetPropertygraphStatisticsOutput")]
#[derive(Debug, Clone, Default)]
pub struct GetPropertygraphStatisticsOutput {
    pub status: String,
    pub payload: Statistics,
}

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `GetPropertygraphStatisticsError` alias below.
#[smithy_macros::operation_error("com.amazonaws.neptunedata#GetPropertygraphStatistics")]
#[derive(Debug)]
pub enum GetPropertygraphStatisticsErrorModeled {
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
/// (`crate::ops::get_propertygraph_statistics::GetPropertygraphStatisticsError`).
pub type GetPropertygraphStatisticsError =
    crate::scaffolded::Scaffolded<GetPropertygraphStatisticsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so the handler
/// writes `Err(SomeModeledError.into())` rather than naming the wrapper variant.
impl From<GetPropertygraphStatisticsErrorModeled> for GetPropertygraphStatisticsError {
    fn from(modeled: GetPropertygraphStatisticsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `GetPropertygraphStatistics` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type.
pub struct GetPropertygraphStatistics<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> GetPropertygraphStatistics<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for GetPropertygraphStatistics<Eng>
{
    type Input = GetPropertygraphStatisticsInput;
    type Output = GetPropertygraphStatisticsOutput;
    type Error = GetPropertygraphStatisticsError;

    const NAME: &'static str = "GetPropertygraphStatistics";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema =
        GET_PROPERTYGRAPH_STATISTICS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema =
        GET_PROPERTYGRAPH_STATISTICS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let _ = (&input, ctx);
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return Err(GetPropertygraphStatisticsErrorModeled::BadRequestException(
                crate::state::store_unavailable_bad_request(),
            )
            .into());
        };

        // A @readonly operation: the record is read, never written. Cloning it under the
        // lock keeps the rendering below off the critical section.
        let record = state.lock().propertygraph_statistics.clone();
        let Some(record) = record else {
            // The engine holds no statistics for this data — none has been generated, or a
            // delete removed them — so there are none to report.
            return Err(
                GetPropertygraphStatisticsErrorModeled::StatisticsNotAvailableException(
                    crate::state::statistics_not_available(),
                )
                .into(),
            );
        };

        // `date` and `signatureInfo` report on the latest generation run, so a record that
        // no run has produced statistics for carries neither: there is no instant at which
        // statistics were generated and no characteristic set was counted.
        let latest = record.latest.as_ref();

        Ok(GetPropertygraphStatisticsOutput {
            status: crate::state::STATISTICS_REQUEST_SERVED.to_string(),
            payload: Statistics {
                auto_compute: Some(record.auto_compute),
                active: Some(record.is_active()),
                statistics_id: Some(record.statistics_id()),
                date: latest.map(|run| ::aws_smithy_types::DateTime::from(run.generated_at)),
                // "A note about problems in the case where statistics are invalid" — the
                // statistics this engine reports are counted from its own graph, so there
                // is no problem to note.
                note: None,
                signature_info: latest.map(|run| StatisticsSummary {
                    signature_count: Some(run.signatures.signature_count),
                    instance_count: Some(run.signatures.instance_count),
                    predicate_count: Some(run.signatures.predicate_count),
                }),
            },
        })
    }
}
