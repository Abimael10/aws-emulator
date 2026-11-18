// SPDX-License-Identifier: CC0-1.0

//! The `AWS/Kinesis` series a stream publishes about its own data plane: what its producers
//! wrote, what its consumers read, and how far behind a consumer is.
//!
//! # What the series are
//!
//! Amazon Kinesis Data Streams publishes a stream's traffic as CloudWatch metrics in the
//! [`NAMESPACE`] namespace, and a metric is published under two different identities
//! (<https://docs.aws.amazon.com/streams/latest/dev/monitoring-with-cloudwatch.html>):
//!
//! * STREAM level, dimensioned by [`STREAM_NAME_DIMENSION`] alone, for every stream and with
//!   no opt-in: a stream that serves a write or a read publishes it, which is what a client's
//!   throughput dashboard plots and what a scaling policy on `IncomingRecords` is evaluated
//!   against;
//! * SHARD level, dimensioned by [`STREAM_NAME_DIMENSION`] AND [`SHARD_ID_DIMENSION`], only
//!   for the metrics the stream's own enhanced monitoring names
//!   (`EnableEnhancedMonitoring`/`DisableEnhancedMonitoring`, whose `ShardLevelMetrics` is
//!   that set) — so a stream nothing enhanced has no shard-dimensioned series at all, and one
//!   whose set names a metric publishes the SAME metric name per shard beside the stream-level
//!   one. Which metrics have such a form is the modelled `MetricsName` catalog
//!   ([`super::shard_level_metric_catalog`]), which is why a series of this module names its
//!   shard-level form by that catalog's spelling.
//!
//! A record RETRIEVED from a shard is counted the same way whichever reader asked for it — the
//! pull read of `GetRecords` and the push delivery of `SubscribeToShard` both retrieve records
//! from the shard — while each reader's own per-API family counts its calls separately.
//!
//! Each observation is one call's own contribution, so a `Sum` over a period is the traffic
//! that period carried and a `SampleCount` is how many calls contributed to it: N writes of
//! equal-sized records come to `IncomingRecords` N and `IncomingBytes` N times the size of one
//! record, on the stream-level series and, where the stream's set names them, on the
//! shard-level one. The counters measured in bytes count the RECORD's size — "when the data
//! blob (the payload before base64-encoding) is added to the partition key size, the total
//! size must not exceed the maximum record size" (`PutRecordInput$Data`) — which is the one
//! measure of a record this service uses ([`super::DataRecord::stored_bytes`]) and the figure
//! the shard's own throughput quota charges, so a dashboard reading these bytes against the
//! shard's published limit is reading the same measure.
//!
//! # When an observation happens
//!
//! Inside the data-plane call that caused it, and only for what that call actually did: a
//! write that was stored, a read that was served, a record a shard's quota turned down. The
//! measurements of one call are collected into a [`Measured`] while this service's store guard
//! is held — the stream's name and its enhanced-monitoring set are read there — and published
//! with that guard RELEASED ([`Measured::publish`]), into the CloudWatch emulator's own store:
//! the one `ListMetrics`, `GetMetricStatistics` and the alarm evaluation read, so a metric has
//! one home and the publisher and its readers cannot drift. They never travel over
//! `PutMetricData`, which refuses a publish into a namespace the services own.
//!
//! Nothing here fails a request: the datapoints are published off the answer, so a CloudWatch
//! store this engine does not hold, or one at capacity, drops the observation and the call is
//! answered exactly as it was.

use ::std::time::{SystemTime, UNIX_EPOCH};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;

use super::Stream;

/// The namespace Amazon Kinesis Data Streams publishes a stream's own metrics under.
pub const NAMESPACE: &str = "AWS/Kinesis";

/// The dimension that names the stream a series belongs to.
pub const STREAM_NAME_DIMENSION: &str = "StreamName";

/// The dimension a shard-level series carries beside the stream's name.
pub const SHARD_ID_DIMENSION: &str = "ShardId";

/// The unit a tally of records or calls is published in: the `StandardUnit` value for a plain
/// count.
const COUNT_UNIT: &str = "Count";

/// The unit a measurement of record data is published in.
const BYTES_UNIT: &str = "Bytes";

/// The unit a lag is published in.
const MILLISECONDS_UNIT: &str = "Milliseconds";

/// The storage resolution a standard-resolution metric is published at, in seconds — what
/// `PutMetricData` stores for a publisher that names no `StorageResolution`.
const STANDARD_STORAGE_RESOLUTION_SECONDS: i32 = 60;

/// One series of this namespace: the metric name it is published under, the unit it measures
/// in, and under which of the two identities it exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Series {
    /// The metric name, as the CloudWatch metric reference spells it.
    pub metric_name: &'static str,
    /// The unit each of its datapoints carries.
    pub unit: &'static str,
    /// Whether the stream-level series (the stream's name alone) exists for this metric.
    stream_level: bool,
    /// Whether a shard-dimensioned form of this metric exists — i.e. whether the name is one
    /// of the modelled `MetricsName` catalog enhanced monitoring can enable
    /// ([`super::shard_level_metric_catalog`]).
    shard_level: bool,
}

impl Series {
    /// A series published under BOTH identities: at stream level for every stream, and per
    /// shard for a stream whose enhanced monitoring names it.
    const fn stream_and_shard(metric_name: &'static str, unit: &'static str) -> Self {
        Self {
            metric_name,
            unit,
            stream_level: true,
            shard_level: true,
        }
    }

    /// A series published at STREAM level only: the per-API families, whose names carry the
    /// operation they are about and which the shard-level catalog does not list.
    const fn stream_only(metric_name: &'static str, unit: &'static str) -> Self {
        Self {
            metric_name,
            unit,
            stream_level: true,
            shard_level: false,
        }
    }

    /// A series published per SHARD only: the lag, which the catalog spells
    /// `IteratorAgeMilliseconds` while the stream-level reading of the same lag is its own
    /// per-API series ([`GET_RECORDS_ITERATOR_AGE`]).
    const fn shard_only(metric_name: &'static str, unit: &'static str) -> Self {
        Self {
            metric_name,
            unit,
            stream_level: false,
            shard_level: true,
        }
    }
}

/// The records a producer put into the stream.
pub const INCOMING_RECORDS: Series = Series::stream_and_shard("IncomingRecords", COUNT_UNIT);

/// The record data a producer put into the stream.
pub const INCOMING_BYTES: Series = Series::stream_and_shard("IncomingBytes", BYTES_UNIT);

/// The records a consumer read from the stream.
pub const OUTGOING_RECORDS: Series = Series::stream_and_shard("OutgoingRecords", COUNT_UNIT);

/// The record data a consumer read from the stream.
pub const OUTGOING_BYTES: Series = Series::stream_and_shard("OutgoingBytes", BYTES_UNIT);

/// The records a shard's write quota turned down: "Each shard can support writes up to 1,000
/// records per second, up to a maximum data write total of 1 MB per second" (`PutRecord`), and
/// a record past that is refused rather than stored.
pub const WRITE_THROUGHPUT_EXCEEDED: Series =
    Series::stream_and_shard("WriteProvisionedThroughputExceeded", COUNT_UNIT);

/// How far behind the tip of the shard the read left its consumer, per shard.
pub const ITERATOR_AGE: Series = Series::shard_only("IteratorAgeMilliseconds", MILLISECONDS_UNIT);

/// Whether one `PutRecord` call succeeded: one observation of 1 for a stored record and of 0
/// for a refused one, so an `Average` over a period is that period's success rate.
pub const PUT_RECORD_SUCCESS: Series = Series::stream_only("PutRecord.Success", COUNT_UNIT);

/// The record data one `PutRecord` call stored.
pub const PUT_RECORD_BYTES: Series = Series::stream_only("PutRecord.Bytes", BYTES_UNIT);

/// Whether one `PutRecords` call was served at all.
pub const PUT_RECORDS_SUCCESS: Series = Series::stream_only("PutRecords.Success", COUNT_UNIT);

/// The record data one `PutRecords` call stored.
pub const PUT_RECORDS_BYTES: Series = Series::stream_only("PutRecords.Bytes", BYTES_UNIT);

/// The entries one `PutRecords` call carried, stored or not.
pub const PUT_RECORDS_TOTAL_RECORDS: Series =
    Series::stream_only("PutRecords.TotalRecords", COUNT_UNIT);

/// The entries of one `PutRecords` call that were stored.
pub const PUT_RECORDS_SUCCESSFUL_RECORDS: Series =
    Series::stream_only("PutRecords.SuccessfulRecords", COUNT_UNIT);

/// The entries of one `PutRecords` call that were not.
pub const PUT_RECORDS_FAILED_RECORDS: Series =
    Series::stream_only("PutRecords.FailedRecords", COUNT_UNIT);

/// The entries of one `PutRecords` call a shard's write quota turned down.
pub const PUT_RECORDS_THROTTLED_RECORDS: Series =
    Series::stream_only("PutRecords.ThrottledRecords", COUNT_UNIT);

/// Whether one `GetRecords` call was served.
pub const GET_RECORDS_SUCCESS: Series = Series::stream_only("GetRecords.Success", COUNT_UNIT);

/// The records one `GetRecords` call returned.
pub const GET_RECORDS_RECORDS: Series = Series::stream_only("GetRecords.Records", COUNT_UNIT);

/// The record data one `GetRecords` call returned.
pub const GET_RECORDS_BYTES: Series = Series::stream_only("GetRecords.Bytes", BYTES_UNIT);

/// How far behind the tip of the stream the read left its consumer: the stream-level reading
/// of the lag whose per-shard form is [`ITERATOR_AGE`].
pub const GET_RECORDS_ITERATOR_AGE: Series =
    Series::stream_only("GetRecords.IteratorAgeMilliseconds", MILLISECONDS_UNIT);

/// Whether one `SubscribeToShard` subscription was established.
pub const SUBSCRIBE_TO_SHARD_SUCCESS: Series =
    Series::stream_only("SubscribeToShard.Success", COUNT_UNIT);

/// The records one event of such a subscription delivered.
pub const SUBSCRIBE_TO_SHARD_EVENT_RECORDS: Series =
    Series::stream_only("SubscribeToShardEvent.Records", COUNT_UNIT);

/// The record data one event of such a subscription delivered.
pub const SUBSCRIBE_TO_SHARD_EVENT_BYTES: Series =
    Series::stream_only("SubscribeToShardEvent.Bytes", BYTES_UNIT);

/// How far behind the tip of the stream one such event left its consumer.
pub const SUBSCRIBE_TO_SHARD_EVENT_LAG: Series =
    Series::stream_only("SubscribeToShardEvent.MillisBehindLatest", MILLISECONDS_UNIT);

/// Which stream a call's measurements belong to, and which of its metrics are published per
/// shard as well: the stream's name and its own enhanced-monitoring set, read under this
/// service's store guard so a measurement is booked against the setting in force when the
/// call was served.
#[derive(Debug, Clone)]
pub struct StreamSeries {
    /// The stream's name, which is the `StreamName` dimension value.
    stream_name: String,
    /// The shard-level metrics the stream's enhanced monitoring names, as
    /// `EnableEnhancedMonitoring` left them.
    shard_level_metrics: Vec<String>,
}

impl StreamSeries {
    /// The series `stream` publishes.
    pub fn of(stream: &Stream) -> Self {
        Self {
            stream_name: stream.name.clone(),
            shard_level_metrics: stream.shard_level_metrics.clone(),
        }
    }

    /// Whether `series` is additionally published for one shard of this stream: it has a
    /// shard-dimensioned form AND the stream's enhanced monitoring names it.
    fn publishes_per_shard(&self, series: Series) -> bool {
        series.shard_level
            && self
                .shard_level_metrics
                .iter()
                .any(|metric| metric == series.metric_name)
    }
}

/// One observation: of which series, under which identity, how much, and when.
#[derive(Debug, Clone)]
struct Observation {
    /// The instant the call took it, which is the instant its datapoint carries.
    at: SystemTime,
    /// The stream whose series it belongs to.
    stream_name: String,
    /// The shard it is dimensioned by, or `None` for the stream-level series.
    shard_id: Option<String>,
    /// The series it is an observation of.
    series: Series,
    /// The figure observed.
    value: f64,
}

impl Observation {
    /// The identity of the series this observation goes into: the namespace, the metric name
    /// and the dimension set, which is what a reader selects a series by.
    fn key(&self) -> cloudwatch::state::MetricKey {
        let mut dimensions = cloudwatch::state::DimensionSet::new();
        dimensions.insert(
            STREAM_NAME_DIMENSION.to_string(),
            self.stream_name.clone(),
        );
        if let Some(shard_id) = &self.shard_id {
            dimensions.insert(SHARD_ID_DIMENSION.to_string(), shard_id.clone());
        }
        cloudwatch::state::MetricKey {
            namespace: NAMESPACE.to_string(),
            metric_name: self.series.metric_name.to_string(),
            dimensions,
        }
    }
}

/// What ONE data-plane call measured, collected while it is served and published once its
/// answer is settled.
#[derive(Debug, Default)]
pub struct Measured(Vec<Observation>);

impl Measured {
    /// A call that has measured nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `value` of `series` for the call, taken on `shard_id` of `stream`.
    ///
    /// The stream-level series gets the observation whenever the metric has that form, and the
    /// shard-dimensioned one whenever the stream's enhanced monitoring names the metric — so
    /// the two are the same figure under two identities, and a stream nothing enhanced gains
    /// no shard-dimensioned series.
    pub fn record(
        &mut self,
        stream: &StreamSeries,
        series: Series,
        shard_id: &str,
        value: f64,
        at: SystemTime,
    ) {
        if series.stream_level {
            self.push(stream, series, None, value, at);
        }
        if stream.publishes_per_shard(series) {
            self.push(stream, series, Some(shard_id), value, at);
        }
    }

    /// Record `value` of a series that is about the CALL rather than about one shard of the
    /// stream — the per-API families, which carry no shard dimension.
    pub fn record_for_the_call(
        &mut self,
        stream: &StreamSeries,
        series: Series,
        value: f64,
        at: SystemTime,
    ) {
        self.push(stream, series, None, value, at);
    }

    /// Hold one observation of `series` under one identity: the stream alone (`None`) or the
    /// stream and one of its shards.
    fn push(
        &mut self,
        stream: &StreamSeries,
        series: Series,
        shard_id: Option<&str>,
        value: f64,
        at: SystemTime,
    ) {
        if !value.is_finite() {
            return;
        }
        self.0.push(Observation {
            at,
            stream_name: stream.stream_name.clone(),
            shard_id: shard_id.map(str::to_string),
            series,
            value,
        });
    }

    /// Record that `records` records totalling `bytes` bytes of data were stored in `shard_id`.
    pub fn record_incoming(
        &mut self,
        stream: &StreamSeries,
        shard_id: &str,
        records: f64,
        bytes: f64,
        at: SystemTime,
    ) {
        self.record(stream, INCOMING_RECORDS, shard_id, records, at);
        self.record(stream, INCOMING_BYTES, shard_id, bytes, at);
    }

    /// Record that `records` records totalling `bytes` bytes of data were read from `shard_id`.
    pub fn record_outgoing(
        &mut self,
        stream: &StreamSeries,
        shard_id: &str,
        records: f64,
        bytes: f64,
        at: SystemTime,
    ) {
        self.record(stream, OUTGOING_RECORDS, shard_id, records, at);
        self.record(stream, OUTGOING_BYTES, shard_id, bytes, at);
    }

    /// Publish everything this call measured into the CloudWatch metric store, and make the
    /// alarms watching those series due for evaluation.
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
/// Every figure this service measures — the records of one call, the data bytes one page or
/// one batch carries — is bounded by the published per-call limits, so it converts exactly; a
/// figure past what that conversion holds is reported at the ceiling rather than wrapping.
pub fn counted(count: usize) -> f64 {
    u32::try_from(count).map_or(f64::from(u32::MAX), f64::from)
}
