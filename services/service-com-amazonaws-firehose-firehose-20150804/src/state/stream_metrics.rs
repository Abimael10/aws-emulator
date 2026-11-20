// SPDX-License-Identifier: CC0-1.0

//! The `AWS/Firehose` series a delivery stream publishes about its own data plane: what its
//! producers put into it, and what it wrote to Amazon S3.
//!
//! # What the series are
//!
//! A Firehose stream is a metered pipeline, and the two ends of it are metered separately —
//! which is the whole point of the namespace: "Firehose accumulates and publishes a particular
//! metric for a customer account in one minute intervals" (`PutRecordBatch`). Every series lives
//! in [`NAMESPACE`] under the [`DELIVERY_STREAM_NAME_DIMENSION`] dimension naming the stream it
//! is about:
//!
//! * INGESTION, for what a put ACCEPTED: [`INCOMING_RECORDS`] and [`INCOMING_BYTES`]. A record
//!   the stream's buffer had no room for was not accepted, so it is not counted; the bytes are
//!   the record's own data, which is the measure every buffer budget in this crate charges
//!   against.
//! * DELIVERY, for what reached the destination: [`DELIVERY_TO_S3_RECORDS`],
//!   [`DELIVERY_TO_S3_BYTES`], [`DELIVERY_TO_S3_SUCCESS`] (one observation of 1 per delivery
//!   that was written, so a `Maximum` over a window is non-zero exactly when that window wrote
//!   one) and [`DELIVERY_TO_S3_DATA_FRESHNESS`], the age of the oldest record the delivery
//!   carried, in seconds.
//!
//! So the two ends are what tells a healthy stream from a broken one: a stream whose destination
//! role may not write the object accepted the records all the same, so its ingestion counters
//! move and its delivery counters do not.
//!
//! # When an observation happens
//!
//! Inside the call or the delivery that caused it, and only for what it actually did: a put
//! meters the records it stored, and a delivery meters the records it wrote. The measurements of
//! one of those are collected into a [`Measured`] and published with THIS service's store guard
//! RELEASED ([`Measured::publish`]), into the CloudWatch emulator's own store: the one
//! `ListMetrics`, `GetMetricStatistics` and the alarm evaluation read, so a metric has one home
//! and the publisher and its readers cannot drift. They never travel over `PutMetricData`, which
//! refuses a publish into a namespace the services own.
//!
//! Nothing here fails a request or a delivery: the datapoints are published off the answer, so a
//! CloudWatch store this engine does not hold, or one at capacity, drops the observation and the
//! call is answered exactly as it was.

use ::std::time::{SystemTime, UNIX_EPOCH};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;

/// The namespace Amazon Data Firehose publishes a stream's own metrics under.
pub const NAMESPACE: &str = "AWS/Firehose";

/// The dimension that names the delivery stream a series belongs to.
pub const DELIVERY_STREAM_NAME_DIMENSION: &str = "DeliveryStreamName";

/// The unit a tally of records or of deliveries is published in: the `StandardUnit` value for a
/// plain count.
const COUNT_UNIT: &str = "Count";

/// The unit a measurement of record data is published in.
const BYTES_UNIT: &str = "Bytes";

/// The unit an age is published in.
const SECONDS_UNIT: &str = "Seconds";

/// The storage resolution a standard-resolution metric is published at, in seconds — what
/// `PutMetricData` stores for a publisher that names no `StorageResolution`.
const STANDARD_STORAGE_RESOLUTION_SECONDS: i32 = 60;

/// One series of this namespace: the metric name it is published under and the unit it measures
/// in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Series {
    /// The metric name, as the CloudWatch metric reference spells it.
    pub metric_name: &'static str,
    /// The unit each of its datapoints carries.
    pub unit: &'static str,
}

impl Series {
    /// A series of this namespace.
    const fn new(metric_name: &'static str, unit: &'static str) -> Self {
        Self { metric_name, unit }
    }
}

/// The records a producer put into the stream and the stream accepted.
pub const INCOMING_RECORDS: Series = Series::new("IncomingRecords", COUNT_UNIT);

/// The record data a producer put into the stream and the stream accepted.
pub const INCOMING_BYTES: Series = Series::new("IncomingBytes", BYTES_UNIT);

/// The records the stream wrote to its Amazon S3 destination.
pub const DELIVERY_TO_S3_RECORDS: Series = Series::new("DeliveryToS3.Records", COUNT_UNIT);

/// The record data it wrote there.
pub const DELIVERY_TO_S3_BYTES: Series = Series::new("DeliveryToS3.Bytes", BYTES_UNIT);

/// Whether one delivery to Amazon S3 was written: one observation of 1 per delivery that was.
pub const DELIVERY_TO_S3_SUCCESS: Series = Series::new("DeliveryToS3.Success", COUNT_UNIT);

/// The age of the oldest record one delivery to Amazon S3 carried, at the instant it was written.
pub const DELIVERY_TO_S3_DATA_FRESHNESS: Series =
    Series::new("DeliveryToS3.DataFreshness", SECONDS_UNIT);

/// One observation: of which series, about which stream, how much, and when.
#[derive(Debug, Clone)]
struct Observation {
    /// The instant it was taken, which is the instant its datapoint carries.
    at: SystemTime,
    /// The stream it is about, which is the dimension value.
    stream_name: String,
    /// The series it is an observation of.
    series: Series,
    /// The figure observed.
    value: f64,
}

impl Observation {
    /// The identity of the series this observation goes into: the namespace, the metric name and
    /// the dimension set, which is what a reader selects a series by.
    fn key(&self) -> cloudwatch::state::MetricKey {
        let mut dimensions = cloudwatch::state::DimensionSet::new();
        dimensions.insert(
            DELIVERY_STREAM_NAME_DIMENSION.to_string(),
            self.stream_name.clone(),
        );
        cloudwatch::state::MetricKey {
            namespace: NAMESPACE.to_string(),
            metric_name: self.series.metric_name.to_string(),
            dimensions,
        }
    }
}

/// What ONE put or ONE delivery measured, collected while it is performed and published once it
/// is settled.
#[derive(Debug, Default)]
pub struct Measured(Vec<Observation>);

impl Measured {
    /// A call that has measured nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold one observation of `series` about `stream_name`.
    fn push(&mut self, stream_name: &str, series: Series, value: f64, at: SystemTime) {
        if !value.is_finite() {
            return;
        }
        self.0.push(Observation {
            at,
            stream_name: stream_name.to_string(),
            series,
            value,
        });
    }

    /// Record that `stream_name` ACCEPTED `records` records totalling `bytes` bytes of data.
    ///
    /// A put that stored nothing — every record of it refused by the buffer budget — measures
    /// nothing: there is no ingestion to count.
    pub fn record_ingested(
        &mut self,
        stream_name: &str,
        records: usize,
        bytes: usize,
        at: SystemTime,
    ) {
        if records == 0 {
            return;
        }
        self.push(stream_name, INCOMING_RECORDS, counted(records), at);
        self.push(stream_name, INCOMING_BYTES, counted(bytes), at);
    }

    /// Record that one delivery of `stream_name` WROTE `records` records totalling `bytes` bytes
    /// of data to Amazon S3, the oldest of them `oldest_age` old when it was written.
    pub fn record_delivered_to_s3(
        &mut self,
        stream_name: &str,
        records: usize,
        bytes: usize,
        oldest_age: ::std::time::Duration,
        at: SystemTime,
    ) {
        self.push(stream_name, DELIVERY_TO_S3_RECORDS, counted(records), at);
        self.push(stream_name, DELIVERY_TO_S3_BYTES, counted(bytes), at);
        // One delivery was written, which is what this series observes: a `Maximum` over a
        // window is non-zero exactly when that window wrote one.
        self.push(stream_name, DELIVERY_TO_S3_SUCCESS, 1.0, at);
        self.push(
            stream_name,
            DELIVERY_TO_S3_DATA_FRESHNESS,
            oldest_age.as_secs_f64(),
            at,
        );
    }

    /// Publish everything this call or delivery measured into the CloudWatch metric store, and
    /// make the alarms watching those series due for evaluation.
    ///
    /// Call it with NO store guard of this service held: it takes CloudWatch's own.
    pub fn publish<Eng: Send + Sync + 'static>(self, engine: &Eng) {
        if self.0.is_empty() {
            return;
        }
        let Some(state) = cloudwatch::state::service_state(engine) else {
            return;
        };
        let mut store = state.lock();
        let mut written: Vec<cloudwatch::state::WrittenSeries> = Vec::new();
        let mut latest_secs = 0.0_f64;
        for observation in &self.0 {
            let key = observation.key();
            let timestamp_secs = epoch_secs(observation.at);
            latest_secs = latest_secs.max(timestamp_secs);
            let datum = cloudwatch::state::StoredDatum {
                // CloudWatch holds a datapoint's instant as epoch SECONDS, which is the unit
                // its own readers window by.
                timestamp_secs,
                // One observation: this call's own contribution.
                sample_count: 1.0,
                sum: observation.value,
                minimum: observation.value,
                maximum: observation.value,
                unit: Some(observation.series.unit.to_string()),
                storage_resolution: STANDARD_STORAGE_RESOLUTION_SECONDS,
            };
            if store.append_datapoints(key.clone(), vec![datum]).is_err() {
                // The metric store holds no more series. The call itself was already served,
                // so nothing is failed for it; the dropped observation is reported where a
                // developer can see it.
                ::tracing::warn!(
                    stream = %observation.stream_name,
                    metric = %observation.series.metric_name,
                    "the CloudWatch metric store is at capacity, so what this call measured \
                     was not published"
                );
                continue;
            }
            written.push(cloudwatch::state::WrittenSeries {
                key,
                units: vec![observation.series.unit.to_string()],
            });
        }
        // An alarm on one of these series grades the datapoints that arrive in it, so storing
        // them is what makes its next evaluation due — exactly as a published datapoint does.
        store.make_alarms_due(&written, latest_secs);
    }
}

/// `at` as epoch seconds, the unit CloudWatch holds a datapoint's instant in.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// A count or a byte total as a datapoint carries it.
///
/// Every figure this service measures — the records one put accepted, the data bytes one buffer
/// holds — is bounded by the published per-call limits and the buffer budgets, so it converts
/// exactly; a figure past what that conversion holds is reported at the ceiling rather than
/// wrapping.
fn counted(count: usize) -> f64 {
    u32::try_from(count).map_or(f64::from(u32::MAX), f64::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The series one `Measured` holds, as `(metric name, unit, value)`.
    fn held(measured: &Measured) -> Vec<(&'static str, &'static str, f64)> {
        measured
            .0
            .iter()
            .map(|observation| {
                (
                    observation.series.metric_name,
                    observation.series.unit,
                    observation.value,
                )
            })
            .collect()
    }

    /// RULE: a put meters the records it ACCEPTED and their bytes, and nothing else; a put that
    /// accepted nothing meters nothing.
    #[test]
    fn ingestion_is_metered_as_the_records_and_bytes_a_put_accepted() {
        // The rows differ along what the rule generalises over: how many records a put accepted
        // and how much data they carried, including the empty put.
        let rows: [(&str, usize, usize); 4] = [
            ("nothing accepted", 0, 0),
            ("one record", 1, 45),
            ("seven records", 7, 315),
            ("a record carrying no data", 1, 0),
        ];
        for (case, records, bytes) in rows {
            let mut measured = Measured::new();
            measured.record_ingested("stream", records, bytes, SystemTime::UNIX_EPOCH);
            let expected: Vec<(&str, &str, f64)> = if records == 0 {
                Vec::new()
            } else {
                vec![
                    ("IncomingRecords", COUNT_UNIT, records as f64),
                    ("IncomingBytes", BYTES_UNIT, bytes as f64),
                ]
            };
            let got = held(&measured);
            assert_eq!(
                got, expected,
                "case {case:?}: expected {expected:?}, got {got:?}"
            );
        }
    }

    /// RULE: a delivery that was written meters its records, its bytes, one success and the age
    /// of the oldest record it carried, each in its documented unit.
    #[test]
    fn a_written_delivery_is_metered_with_its_records_bytes_success_and_freshness() {
        // The rows differ along what the rule generalises over: the records one delivery wrote,
        // their bytes, and how old the oldest of them was.
        let rows: [(&str, usize, usize, u64); 3] = [
            ("one record, just arrived", 1, 45, 0),
            ("seven records, a minute old", 7, 315, 60),
            ("five hundred records, an hour old", 500, 1_000_000, 3_600),
        ];
        for (case, records, bytes, age_secs) in rows {
            let mut measured = Measured::new();
            measured.record_delivered_to_s3(
                "stream",
                records,
                bytes,
                ::std::time::Duration::from_secs(age_secs),
                SystemTime::UNIX_EPOCH,
            );
            let expected = vec![
                ("DeliveryToS3.Records", COUNT_UNIT, records as f64),
                ("DeliveryToS3.Bytes", BYTES_UNIT, bytes as f64),
                ("DeliveryToS3.Success", COUNT_UNIT, 1.0),
                (
                    "DeliveryToS3.DataFreshness",
                    SECONDS_UNIT,
                    age_secs as f64,
                ),
            ];
            let got = held(&measured);
            assert_eq!(
                got, expected,
                "case {case:?}: expected {expected:?}, got {got:?}"
            );
        }
    }
}
