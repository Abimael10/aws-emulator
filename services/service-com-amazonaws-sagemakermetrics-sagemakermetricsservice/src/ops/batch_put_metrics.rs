// SPDX-License-Identifier: CC0-1.0

//! @op BatchPutMetrics
//! Ingests raw training metrics for one trial component.
//!
//! Each `MetricData` entry is stored on its own: the batch is PARTIALLY successful, so a
//! point this emulator cannot store is reported as a `BatchPutMetricsError` carrying its
//! `MetricIndex` while every other point in the same call is persisted. `Errors` is on
//! EVERY answer of this operation, as the empty list when every point was ingested, so a
//! client reads one member whatever happened.
//!
//! What happens before any of that is IAM's decision on the CALLER: a principal whose
//! identity-based policies do not allow `sagemaker:BatchPutMetrics` on the trial component
//! this batch names is refused `AccessDeniedException` and nothing is ingested
//! ([`crate::service::authorization`]).
//!
//! The modelled constraints of a batch (`MetricName` 1..255 matching `^.+$`, `Step` >= 0,
//! `TrialComponentName` matching `^[a-z0-9](-*[a-z0-9]){0,119}$`, 1..10 data points) are
//! NOT refused in front of this handler (`skip_validation` in `super::types`): a
//! questionable point is this operation's own per-index verdict, not a refusal of the
//! whole batch, so a point outside one of them is stored as sent. What refuses a call
//! before this handler runs is the envelope alone — a batch that names no trial component
//! or carries no data at all ([`crate::service::envelope`]).
//!
//! The per-index codes this handler reports:
//!
//! * `INTERNAL_ERROR` — the point never became a measurement: a `Value` that is NOT A
//!   NUMBER (`NaN`, which the codec reads from a quoted value, so a request can carry
//!   one), which has no position in any ordering and so can be neither stored at a
//!   coordinate nor aggregated, or a `MetricName` with no non-blank character, which names
//!   no series to store it in. The shared store being unreachable is the same code,
//!   reported for every point of the batch.
//! * `CONFLICT_ERROR` — a different `Value` at a `(MetricName, Timestamp, Step)`
//!   coordinate the series already holds. Writing the SAME value there again is accepted
//!   and stores nothing new, so a retried batch is idempotent.
//! * `METRIC_LIMIT_EXCEEDED` — the point would take the store past
//!   [`crate::state::MAX_TRIAL_COMPONENTS`], the trial component past
//!   [`crate::state::MAX_METRIC_NAMES_PER_TRIAL_COMPONENT`] or the series past
//!   [`crate::state::MAX_DATA_POINTS_PER_SERIES`].
//!
//! `VALIDATION_ERROR`, the fourth modelled code, is reported for no point: this service
//! validates nothing about a point it can store. An INFINITE `Value` is a number and is
//! stored as one; a `MetricName` is otherwise an opaque series key, so a point is stored
//! under the name as sent — however long, whatever it holds — and read back under that same
//! name; a `Step` is stored as sent; and every instant the modelled `Timestamp` can hold is
//! stored at the instant it names.
//!
//! `VALIDATION_ERROR`, the fourth modelled code, is reported for no point: this service
//! validates nothing about a point it can store. A `MetricName` is an opaque series key,
//! so a point is stored under the name as sent — blank, long or absent — and read back
//! under that same name; and every instant the modelled `Timestamp` can hold is stored at
//! the instant it names.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. Reach the store through
//! [`crate::state::service_state`] and read and write it through the guard it hands back.
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions`.
//! The engine is NOT reached through `ctx`.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `BatchPutMetrics`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-sagemakermetrics-sagemakermetricsservice/smithy-model/com-amazonaws-sagemakermetrics-sagemakermetricsservice.json")
//! ```
//!
//! Used to ingest training metrics into SageMaker. These metrics can be visualized in
//! SageMaker Studio.
//!
//! ## Input
//!
//! * `MetricData` (RawMetricDataList, len 1..10) **required** — A list of raw metric values
//!   to put.
//! * `TrialComponentName` (ExperimentEntityName, len 1..120, pattern
//!   ^[a-z0-9](-*[a-z0-9]){0,119}$) **required** — The name of the Trial Component to
//!   associate with the metrics. The Trial Component name must be entirely lowercase.
//!
//! ## Output
//!
//! * `Errors` (BatchPutMetricsErrorList, len 1..10) — Lists any errors that occur when
//!   inserting metric data.

#[allow(unused_imports)]
use super::types::*;

use crate::state::{MetricDatum, RecordRejection};

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `BatchPutMetricsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-sagemakermetrics-sagemakermetricsservice.json");

/// `PutMetricsErrorCode` members, as `types::PUT_METRICS_ERROR_CODE_VALUES` lists them.
/// `VALIDATION_ERROR` is the one member no point of this service draws (see the module
/// docs), so it is not named here.
const CODE_METRIC_LIMIT_EXCEEDED: &str = "METRIC_LIMIT_EXCEEDED";
/// See [`CODE_METRIC_LIMIT_EXCEEDED`].
const CODE_INTERNAL_ERROR: &str = "INTERNAL_ERROR";
/// See [`CODE_METRIC_LIMIT_EXCEEDED`].
const CODE_CONFLICT_ERROR: &str = "CONFLICT_ERROR";

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `BatchPutMetricsError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.sagemakermetrics#BatchPutMetrics")]
#[derive(Debug)]
pub enum BatchPutMetricsErrorModeled {
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::batch_put_metrics::BatchPutMetricsError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type BatchPutMetricsError = crate::scaffolded::Scaffolded<BatchPutMetricsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<BatchPutMetricsErrorModeled> for BatchPutMetricsError {
    fn from(modeled: BatchPutMetricsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `BatchPutMetrics` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct BatchPutMetrics<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> BatchPutMetrics<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

/// One `BatchPutMetricsError` entry: the code, and the index of the data point it is about.
fn failure(code: &str, index: usize) -> BatchPutMetricsError2 {
    BatchPutMetricsError2 {
        code: Some(code.to_string()),
        // `MetricData` holds at most 10 entries, so the index always fits; saturate
        // rather than panic if a future model widens the list.
        metric_index: Some(i32::try_from(index).unwrap_or(i32::MAX)),
    }
}

/// The instant `timestamp` names, as the epoch second it falls in and the millisecond
/// within that second.
///
/// `aws_smithy_types::DateTime` owns both parts (`secs`, `subsec_nanos`), so every instant
/// it can hold is storable and no point is ever refused for the instant it names. The
/// nanosecond remainder is quantised DOWN to the millisecond, which is the resolution this
/// store keeps ([`crate::state::MetricDatum`]).
fn instant(timestamp: &::aws_smithy_types::DateTime) -> (i64, u32) {
    (timestamp.secs(), timestamp.subsec_nanos() / 1_000_000)
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for BatchPutMetrics<Eng>
{
    type Input = BatchPutMetricsInput;
    type Output = BatchPutMetricsOutput;
    type Error = BatchPutMetricsError;

    const NAME: &'static str = "BatchPutMetrics";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::BATCH_PUT_METRICS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::BATCH_PUT_METRICS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // IAM decides first, as AWS decides it in front of the service: the action is this
        // operation's own name under the `sagemaker` namespace, and the resource is the trial
        // component the request names. A refused call ingests nothing, so a later read of the
        // point it carried reports an empty series.
        let resources = [crate::service::authorization::trial_component_addressed(
            ctx,
            &input.trial_component_name,
        )];
        if let Some(denial) =
            crate::service::authorization::refusal(&*self.engine, ctx, Self::NAME, &resources)
        {
            return Err(crate::scaffolded::Scaffolded::access_denied(denial));
        }

        let Some(state) = crate::state::service_state(&*self.engine) else {
            // No shared store, so nothing could be ingested. The model has no
            // operation-level error for that, but it has a per-datum one.
            let errors = (0..input.metric_data.len())
                .map(|index| failure(CODE_INTERNAL_ERROR, index))
                .collect();
            return Ok(BatchPutMetricsOutput {
                errors: Some(errors),
            });
        };

        let mut errors: Vec<BatchPutMetricsError2> = Vec::new();
        let mut state = state.lock();
        for (index, raw) in input.metric_data.iter().enumerate() {
            // A point that never became a measurement is reported as this service's
            // INTERNAL error for that index rather than as a validation fault: the
            // ingesting side is what failed on it, and a client retries that code where it
            // drops a validation one. Two values are such a point: a `Value` that is not a
            // number at all (`NaN` has no position in any ordering, so it can be neither
            // stored at a coordinate nor aggregated) and a `MetricName` with nothing in it
            // (it names no series to store the point in — `crate::service::member_text` is
            // this service's one reading of whether a member's text states anything). Nothing else about a point is
            // judged — an infinite value is a number, a name is otherwise an opaque series
            // key stored as sent, and every instant the modelled timestamp can hold is
            // stored.
            if raw.value.is_nan()
                || crate::service::member_text::states_no_value(&raw.metric_name)
            {
                errors.push(failure(CODE_INTERNAL_ERROR, index));
                continue;
            }
            let (epoch_second, subsec_millis) = instant(&raw.timestamp);

            let datum = MetricDatum {
                epoch_second,
                subsec_millis,
                step: raw.step,
                value: raw.value,
            };
            // Stored under the name as written: trimming would silently rename a
            // caller's metric. The trial component is created on first write rather than
            // required to exist: this service binds no operation that can create one
            // (SageMaker's own API owns `CreateTrialComponent`), so demanding existence
            // here would make ingestion unreachable in this emulator.
            match state.record(&input.trial_component_name, &raw.metric_name, datum) {
                Ok(()) => {}
                Err(RecordRejection::MetricLimitExceeded) => {
                    errors.push(failure(CODE_METRIC_LIMIT_EXCEEDED, index));
                }
                Err(RecordRejection::Conflict) => {
                    errors.push(failure(CODE_CONFLICT_ERROR, index));
                }
            }
        }
        drop(state);

        // An audit record for a state-changing call: what was called, for which trial
        // component, and how many points were stored versus refused. No metric value and
        // nothing header-derived is logged.
        tracing::info!(
            operation = Self::NAME,
            method = %ctx.parts().method,
            trial_component = %input.trial_component_name,
            accepted = input.metric_data.len() - errors.len(),
            rejected = errors.len(),
            "ingested training metrics",
        );

        Ok(BatchPutMetricsOutput {
            // `Errors` is reported on every answer, as the empty list for a call in which
            // nothing failed: the member's presence is not conditional on a failure, so a
            // client that reads it unconditionally — the natural reading of a
            // partial-success batch API — finds it on the success path too.
            errors: Some(errors),
        })
    }
}
