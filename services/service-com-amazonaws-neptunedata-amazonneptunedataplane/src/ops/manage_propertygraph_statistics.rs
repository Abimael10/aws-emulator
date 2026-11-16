// SPDX-License-Identifier: CC0-1.0

//! @op ManagePropertygraphStatistics
//!
//! Manages the generation and use of property graph statistics: this is the one operation
//! that WRITES the statistics state `GetPropertygraphStatistics` reports and
//! `DeletePropertygraphStatistics` removes.
//!
//! Each operation is a struct that owns the shared engine and implements
//! [`smithy_server_runtime::Operation`]. The framework constructs one per service via
//! `ManagePropertygraphStatistics::new(engine)` and drives it through the centralized request walk:
//! deserialize → validate → `handle` → serialize.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. The store is reached through
//! [`crate::state::service_state`], and the statistics state itself is
//! [`crate::state::StatisticsRecord`], which this handler applies a mode through.
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions`.
//! The engine is NOT reached through `ctx`.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `ManagePropertygraphStatistics`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-neptunedata-amazonneptunedataplane/smithy-model/com-amazonaws-neptunedata-amazonneptunedataplane.json")
//! ```
//!
//! Manages the generation and use of property graph statistics. When invoking this
//! operation in a Neptune cluster that has IAM authentication enabled, the IAM user or role
//! making the request must have a policy attached that allows the
//! neptune-db:ManageStatistics IAM action in that cluster.
//!
//! ## Input
//!
//! * `mode` (StatisticsAutoGenerationMode, one of: disableAutoCompute | enableAutoCompute |
//!   refresh) — The statistics generation mode. One of: DISABLE_AUTOCOMPUTE,
//!   ENABLE_AUTOCOMPUTE, or REFRESH, the last of which manually triggers DFE statistics
//!   generation.
//!
//! ## Output
//!
//! * `payload` (RefreshStatisticsIdMap) — This is only returned for refresh mode.
//! * `status` (String) **required** — The HTTP return code of the request. If the request
//!   succeeded, the code is 200.
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
// `ManagePropertygraphStatisticsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-neptunedata-amazonneptunedataplane.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ManagePropertygraphStatisticsError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.neptunedata#ManagePropertygraphStatistics")]
#[derive(Debug)]
pub enum ManagePropertygraphStatisticsErrorModeled {
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
/// (`crate::ops::manage_propertygraph_statistics::ManagePropertygraphStatisticsError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type ManagePropertygraphStatisticsError = crate::scaffolded::Scaffolded<ManagePropertygraphStatisticsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ManagePropertygraphStatisticsErrorModeled> for ManagePropertygraphStatisticsError {
    fn from(modeled: ManagePropertygraphStatisticsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ManagePropertygraphStatistics` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ManagePropertygraphStatistics<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ManagePropertygraphStatistics<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

/// The input member a management call has to carry, by its wire name. The model leaves it
/// optional — it is the only thing a management call says, so a call naming no mode asks
/// for nothing and is refused for the missing parameter rather than served a guess.
const MODE_MEMBER: &str = "mode";

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ManagePropertygraphStatistics<Eng>
{
    type Input = ManagePropertygraphStatisticsInput;
    type Output = ManagePropertygraphStatisticsOutput;
    type Error = ManagePropertygraphStatisticsError;

    const NAME: &'static str = "ManagePropertygraphStatistics";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::MANAGE_PROPERTYGRAPH_STATISTICS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::MANAGE_PROPERTYGRAPH_STATISTICS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            record_access(ctx, input.mode.as_deref(), "store-unavailable");
            return Err(ManagePropertygraphStatisticsErrorModeled::BadRequestException(
                crate::state::store_unavailable_bad_request(),
            )
            .into());
        };

        // A member that arrived blank names no mode either.
        let supplied = input.mode.as_deref().unwrap_or_default();
        if supplied.trim().is_empty() {
            record_access(ctx, input.mode.as_deref(), "missing-mode");
            return Err(
                ManagePropertygraphStatisticsErrorModeled::MissingParameterException(
                    crate::state::missing_parameter(MODE_MEMBER),
                )
                .into(),
            );
        }
        // The boundary validator refuses a value outside the modelled
        // `StatisticsAutoGenerationMode` enum before this handler runs, so this refusal is
        // reachable only if that set ever widens; it names the modes the member accepts
        // rather than guessing which one was meant.
        let Some(mode) = crate::state::StatisticsMode::from_wire(supplied) else {
            record_access(ctx, input.mode.as_deref(), "unsupported-mode");
            return Err(
                ManagePropertygraphStatisticsErrorModeled::IllegalArgumentException(
                    crate::state::unsupported_statistics_mode(supplied),
                )
                .into(),
            );
        };

        let now = ::std::time::SystemTime::now();
        // A `refresh` counts the whole graph with the store's lock held, so the write runs
        // on the blocking pool rather than on the worker serving this request: the `.await`
        // below is a real yield point, so health checks, request timeouts and a graceful
        // shutdown go on working while a run counts, and a second request does not park a
        // second worker waiting for the lock — which a concurrent openCypher query can hold
        // for the whole of its own run. The autocompute modes take the same path, so there
        // is one place the statistics state is written.
        let store = state.clone();
        let generated = ::tokio::task::spawn_blocking(move || {
            store.lock().manage_propertygraph_statistics(mode, now)
        })
        .await;
        let statistics_id = match generated {
            Ok(statistics_id) => statistics_id,
            // The run did not finish, so there is no statistics state to report: the call is
            // refused rather than answered with a run nothing completed.
            Err(joined) => {
                record_access(ctx, Some(supplied), "generation-interrupted");
                return Err(
                    ManagePropertygraphStatisticsErrorModeled::BadRequestException(
                        crate::state::statistics_generation_interrupted(&joined.to_string()),
                    )
                    .into(),
                );
            }
        };
        record_access(
            ctx,
            Some(supplied),
            if statistics_id.is_some() {
                "generation-recorded"
            } else {
                "mode-applied"
            },
        );

        Ok(ManagePropertygraphStatisticsOutput {
            status: crate::state::STATISTICS_REQUEST_SERVED.to_string(),
            // "This is only returned for refresh mode" — the id names the generation run
            // that mode performed, and the autocompute modes perform none.
            payload: statistics_id.map(|statistics_id| RefreshStatisticsIdMap {
                statistics_id: Some(statistics_id),
            }),
        })
    }
}

/// Record one management call: what was asked (the method, the URI and the mode), who asked
/// (the access key the request presented) and how it ended — including every refusal, so a
/// refused attempt to change the statistics state leaves a record too.
///
/// The mode is rendered with `Debug`, which escapes control characters, so a value carrying
/// CRLF cannot forge a second line, and no secret is recorded (the access key id is an
/// identifier, never the signing key).
fn record_access(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    mode: Option<&str>,
    outcome: &'static str,
) {
    let parts = ctx.parts();
    ::tracing::info!(
        operation = "ManagePropertygraphStatistics",
        method = %parts.method,
        uri = %parts.uri,
        mode = ?mode,
        akid = ?::emulator_server::extract_akid(&parts.headers),
        outcome,
        "neptunedata manage propertygraph statistics"
    );
}
