// SPDX-License-Identifier: CC0-1.0

//! What a request's OUTCOME counts into the CloudWatch series DynamoDB publishes it under:
//! the items a read returned, and the writes a condition turned down.
//!
//! Beside the capacity a request consumes ([`crate::state::consumed_capacity_metrics`]) and the
//! time it took ([`crate::service`]'s latency family), Amazon DynamoDB publishes what each
//! request's outcome WAS as `AWS/DynamoDB` counting series, and each family carries its own
//! dimension set:
//!
//! * `ReturnedItemCount` — "the number of items returned by `Query`, `Scan`, or
//!   `ExecuteStatement` operations" — under the PAIR `{TableName, Operation}`, so a client
//!   reads how many items a table handed back per API it was read with, and the `SampleCount`
//!   of a period is how many such reads it served in it;
//! * `ConditionalCheckFailedRequests` — "the number of failed attempts to perform conditional
//!   writes" — under `{TableName}`, so the alarm a client sets on lost
//!   optimistic-concurrency races sees every write the table turned down, whichever surface
//!   made it.
//!
//! A family is published under its own dimension set and under NO other: a series "is uniquely
//! defined by a name, a namespace, and zero or more dimensions", so a read of
//! `ReturnedItemCount` under `{TableName}` alone, or of `ConditionalCheckFailedRequests` under
//! the pair, names a series DynamoDB does not have and finds no datapoints.
//!
//! The observations are recorded where the outcome is SETTLED — under this service's store
//! lock, in the read that counted its page or the refusal that was charged for it — and
//! published with that lock released, into the CloudWatch store, by the one seam every request
//! of this service passes through ([`publish_pending_request_outcomes`]). Nothing here fails a
//! request: a CloudWatch store this engine does not hold, or one at capacity, drops the
//! observation and the request is answered as it was.

use std::time::{SystemTime, UNIX_EPOCH};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;

use super::consumed_capacity_metrics::Series;

/// The namespace Amazon DynamoDB publishes a table's own metrics under.
const NAMESPACE: &str = "AWS/DynamoDB";

/// The metric that counts the items a read handed back.
const RETURNED_ITEM_COUNT: &str = "ReturnedItemCount";

/// The metric that counts the writes a condition turned down.
const CONDITIONAL_CHECK_FAILED_REQUESTS: &str = "ConditionalCheckFailedRequests";

/// The dimension that names the API the requests of a series were made with.
const OPERATION_DIMENSION: &str = "Operation";

/// The dimension that names the table a series belongs to, as every family of a table spells
/// it ([`Series::dimensions`]).
const TABLE_NAME_DIMENSION: &str = "TableName";

/// The unit a count is published in: the `StandardUnit` value for a plain tally.
const COUNT_UNIT: &str = "Count";

/// The storage resolution a standard-resolution metric is published at, in seconds — which is
/// what `PutMetricData` stores for a publisher that names no `StorageResolution`.
const STANDARD_STORAGE_RESOLUTION_SECONDS: i32 = 60;

/// The value one refused write contributes: it was one failed attempt.
const ONE_REQUEST: f64 = 1.0;

/// How many observations this store holds before the oldest is dropped.
///
/// The EMULATOR's own bound, so a store nobody publishes from cannot grow without end: the
/// observations are published by the service seam every request of this service passes through
/// ([`crate::service`]), so this only holds what one request recorded.
const MAX_PENDING_OBSERVATIONS: usize = 4_096;

/// The dimension set DynamoDB publishes a family dimensioned by the API that was served under:
/// the table AND the operation, which is the pair's whole identity.
///
/// The ONE spelling of that pair, shared by every family published under it — the latency of a
/// served request and the items a read returned — so the two cannot drift apart.
pub fn operation_dimensions(table: &str, operation: &str) -> cloudwatch::state::DimensionSet {
    [
        (TABLE_NAME_DIMENSION.to_string(), table.to_string()),
        (OPERATION_DIMENSION.to_string(), operation.to_string()),
    ]
    .into_iter()
    .collect()
}

/// Which counting family one observation belongs to, and the dimension set it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Counted {
    /// The items ONE read returned, booked under the operation it was read with.
    ReturnedItems {
        /// The API the read was made with, which is the `Operation` dimension value.
        operation: &'static str,
    },
    /// ONE write refused because the condition it stated was not met.
    ConditionalCheckFailedRequest,
}

impl Counted {
    /// The metric name this family is published as.
    fn metric_name(&self) -> &'static str {
        match self {
            Self::ReturnedItems { .. } => RETURNED_ITEM_COUNT,
            Self::ConditionalCheckFailedRequest => CONDITIONAL_CHECK_FAILED_REQUESTS,
        }
    }

    /// The dimension set `table`'s series of this family is published under.
    fn dimensions(&self, table: &str) -> cloudwatch::state::DimensionSet {
        match self {
            Self::ReturnedItems { operation } => operation_dimensions(table, operation),
            // The table's own dimension set, exactly as every table-wide family spells it.
            Self::ConditionalCheckFailedRequest => Series::Table.dimensions(table),
        }
    }
}

/// One observation of one request's outcome: of which family, on which table, how much, and
/// when.
#[derive(Debug, Clone)]
pub struct Observation {
    /// The instant the request settled it at, which is the instant the datapoint carries.
    pub at: SystemTime,
    /// The table whose series it belongs to.
    pub table: String,
    /// The family it is counted into.
    pub counted: Counted,
    /// The figure counted: the items a read returned, or the one request a refusal was.
    pub value: f64,
}

/// What the requests of this store have counted and this service has not published yet.
///
/// The outcome is settled under this store's lock and the series it belongs to lives in
/// CloudWatch's store, which cannot be written under it — so the observations wait here in
/// between, exactly as a charged capacity does
/// ([`crate::state::consumed_capacity_metrics::Pending`]).
#[derive(Debug, Default)]
pub struct Pending(Vec<Observation>);

impl Pending {
    /// Record that `operation` returned `items` items of `table`.
    ///
    /// A read that matched nothing is an observation too — it was a read, and it returned no
    /// items — so the family's sample count is how many such reads the table served.
    pub fn record_returned_items(
        &mut self,
        table: &str,
        operation: &'static str,
        items: i32,
        at: SystemTime,
    ) {
        self.record(
            table,
            Counted::ReturnedItems { operation },
            f64::from(items.max(0)),
            at,
        );
    }

    /// Record that one write of `table` was refused because its condition was not met.
    pub fn record_conditional_check_failed(&mut self, table: &str, at: SystemTime) {
        self.record(
            table,
            Counted::ConditionalCheckFailedRequest,
            ONE_REQUEST,
            at,
        );
    }

    /// Record one observation.
    ///
    /// Past [`MAX_PENDING_OBSERVATIONS`] the oldest goes, which at worst loses an observation
    /// nothing has published in that long.
    fn record(&mut self, table: &str, counted: Counted, value: f64, at: SystemTime) {
        if !value.is_finite() {
            return;
        }
        self.0.push(Observation {
            at,
            table: table.to_string(),
            counted,
            value,
        });
        let overflow = self.0.len().saturating_sub(MAX_PENDING_OBSERVATIONS);
        if overflow > 0 {
            self.0.drain(..overflow);
        }
    }

    /// Take everything recorded so far, leaving nothing behind: publishing is at-most-once.
    fn take(&mut self) -> Vec<Observation> {
        ::std::mem::take(&mut self.0)
    }
}

/// Publish what the requests of this store have counted since the last publish, into the
/// CloudWatch series each family is reported under.
///
/// Call it with NO store lock held: this takes this service's lock to drain the observations
/// and CloudWatch's to append them, one after the other. It is called from the one seam every
/// request of this service passes through, so what a request's outcome was is published as
/// part of serving it.
pub fn publish_pending_request_outcomes<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = crate::state::service_state(engine) else {
        return;
    };
    let counted = {
        let mut store = state.lock();
        store.pending_outcomes.take()
    };
    if counted.is_empty() {
        return;
    }
    let Some(metrics) = cloudwatch::state::service_state(engine) else {
        return;
    };
    let mut store = metrics.lock();
    for observation in counted {
        let key = cloudwatch::state::MetricKey {
            namespace: NAMESPACE.to_string(),
            metric_name: observation.counted.metric_name().to_string(),
            dimensions: observation.counted.dimensions(&observation.table),
        };
        let datum = cloudwatch::state::StoredDatum {
            // CloudWatch holds a datapoint's instant as epoch SECONDS, which is the unit its
            // own readers window by.
            timestamp_secs: observation
                .at
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
            // One observation: this request, and what it counted.
            sample_count: 1.0,
            sum: observation.value,
            minimum: observation.value,
            maximum: observation.value,
            unit: Some(COUNT_UNIT.to_string()),
            storage_resolution: STANDARD_STORAGE_RESOLUTION_SECONDS,
        };
        if store.append_datapoints(key, vec![datum]).is_err() {
            // The metric store holds no more series. The request itself was already served, so
            // nothing is failed for it; the dropped observation is reported where a developer
            // can see it.
            ::tracing::warn!(
                table = %observation.table,
                metric = %observation.counted.metric_name(),
                "the CloudWatch metric store is at capacity, so what this request counted was \
                 not published"
            );
        }
    }
}
