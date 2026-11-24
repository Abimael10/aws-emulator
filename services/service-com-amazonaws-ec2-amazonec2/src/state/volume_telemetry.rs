// SPDX-License-Identifier: CC0-1.0

//! The `AWS/EBS` series one VOLUME publishes about the I/O performed on it: the metric families,
//! what one I/O operation is, and where the datapoints are stored.
//!
//! # What the series are
//!
//! Amazon EBS publishes each measurement of one volume as its own `AWS/EBS` metric, dimensioned on
//! exactly that volume ([`VOLUME_ID_DIMENSION`]) and measured in that metric's own unit
//! ([`VOLUME_METRICS`] is the catalog, with the unit of each entry). Those series are what a
//! per-volume dashboard plots and what every volume alarm is evaluated against, and they are
//! dimensioned on the VOLUME and not on the instance — which is what makes them follow the volume
//! when it is detached from one instance and attached to another.
//!
//! Only the measurements of real I/O are published here. `VolumeReadOps`/`VolumeWriteOps` are "the
//! total number of read/write operations in a specified period of time" and
//! `VolumeReadBytes`/`VolumeWriteBytes` "the total number of bytes transferred", so each of them is
//! a measurement of operations something performed ([`super::guest_disk_io`], the one place a guest
//! performs any) — in a period in which nothing read or wrote the volume there is nothing of it to
//! measure, which is why no datapoint is published for one. The queue, idle-time and burst-balance
//! metrics measure quantities nothing here observes, and so are claimed by nothing.
//!
//! `VolumeConsumedReadWriteOps` is published for a volume of a PROVISIONED IOPS type only — "this
//! metric is only available for Provisioned IOPS SSD volumes"
//! ([`super::volumes::PROVISIONED_IOPS_VOLUME_TYPES`] is the one reading of which types those are)
//! — so a volume of any other type carries that series not at all, however much I/O it took.
//!
//! # When a datapoint happens
//!
//! An I/O operation is a thing that ALREADY HAPPENED: the guest performed it, through the device
//! the volume is attached as, at the instant it performed it. So the datapoint carries that instant
//! ([`VolumeIoRecord::at_secs`]) and is published once — the record is taken as it is published
//! ([`due_volume_samples`]), the way the agent's collections are
//! ([`super::guest_cloudwatch_agent::due_samples`]) — rather than being recomputed per period by a
//! sampler. A reader taking a statistic of a period therefore sees exactly the operations performed
//! in it: the `Sum` of `VolumeWriteBytes` over a period is the bytes written in it.
//!
//! Nothing is published for a volume nothing read or wrote, and a volume that is not attached is
//! not a device of any guest, so no I/O can happen on it at all — which is what makes the series
//! stop when the volume is detached.

use ::std::collections::BTreeMap;
use ::std::time::SystemTime;

use super::guest_cloudwatch_agent::AgentSample;
use super::guest_disk_io::VolumeIo;
use super::volumes::PROVISIONED_IOPS_VOLUME_TYPES;
use super::{ComAmazonawsEc2Amazonec2State, VolumeRecord};

/// The namespace Amazon EBS publishes a volume's metrics under.
pub const NAMESPACE: &str = "AWS/EBS";

/// The dimension a volume's series is dimensioned on: "`VolumeId`: This dimension filters the data
/// you request for the specified volume" (Amazon EBS User Guide, "Amazon EBS metrics").
pub const VOLUME_ID_DIMENSION: &str = "VolumeId";

/// How many bytes ONE I/O operation moves at most.
///
/// Amazon EBS counts the operations a volume performed by this size: "I/O operations that are
/// smaller than 256 KiB each count as 1 consumed IOPS" and one larger "that is 1,024 KiB … consumes
/// 4 IOPS" (`VolumeConsumedReadWriteOps`, Amazon EBS User Guide) — so a transfer of `n` bytes is
/// the operations it takes to move them at this size each.
pub const IO_OPERATION_BYTES: i64 = 256 * 1024;

/// How many I/O OPERATIONS a transfer of `bytes` is, counted as Amazon EBS counts them
/// ([`IO_OPERATION_BYTES`]): a transfer of nothing is no operation, and anything up to one
/// operation's size is one.
#[must_use]
pub fn operations_for(bytes: i64) -> i64 {
    if bytes <= 0 {
        return 0;
    }
    bytes.saturating_add(IO_OPERATION_BYTES - 1) / IO_OPERATION_BYTES
}

/// One metric of the `AWS/EBS` families: its name, the unit it is measured in, and what it measures
/// of one transfer.
#[derive(Debug, Clone, Copy)]
pub struct VolumeMetric {
    /// The `MetricName` the series is published under.
    pub name: &'static str,
    /// The `StandardUnit` its datapoints carry.
    pub unit: &'static str,
    /// Whether it is published for a volume of a provisioned-IOPS type ALONE.
    pub provisioned_iops_only: bool,
    /// What it MEASURES of one transfer, read off the transfer itself — so the catalog and the
    /// measurement cannot drift apart.
    pub measures: fn(&VolumeIo) -> i64,
}

/// The metrics a volume publishes about the I/O performed on it, with the unit of each and what
/// each one measures.
///
/// The catalog and every unit in it come from the metrics Amazon EBS documents for one VOLUME
/// (Amazon EBS User Guide, "Amazon EBS metrics"); they are documented metrics rather than Smithy
/// shapes, so there is no model member to read them from.
pub const VOLUME_METRICS: &[VolumeMetric] = &[
    VolumeMetric {
        name: "VolumeReadOps",
        unit: super::instance_telemetry::COUNT_UNIT,
        provisioned_iops_only: false,
        measures: |performed| performed.read_ops,
    },
    VolumeMetric {
        name: "VolumeWriteOps",
        unit: super::instance_telemetry::COUNT_UNIT,
        provisioned_iops_only: false,
        measures: |performed| performed.write_ops,
    },
    VolumeMetric {
        name: "VolumeReadBytes",
        unit: super::instance_telemetry::BYTES_UNIT,
        provisioned_iops_only: false,
        measures: |performed| performed.read_bytes,
    },
    VolumeMetric {
        name: "VolumeWriteBytes",
        unit: super::instance_telemetry::BYTES_UNIT,
        provisioned_iops_only: false,
        measures: |performed| performed.write_bytes,
    },
    VolumeMetric {
        name: "VolumeConsumedReadWriteOps",
        unit: super::instance_telemetry::COUNT_UNIT,
        provisioned_iops_only: true,
        // "the total amount of read and write operations" the volume consumed.
        measures: |performed| performed.read_ops.saturating_add(performed.write_ops),
    },
];

/// How many I/O records ONE volume keeps waiting to be published.
///
/// The records are published off the next request this service serves and off the metric clock, so
/// one waits for the length of a request at most; this bounds what a guest performing I/O without
/// anything reading the account can make this process hold, by dropping the OLDEST, the way a
/// fixed-size buffer does.
pub const MAX_PENDING_IO_RECORDS: usize = 256;

/// One transfer the guest performed on a volume, as the volume holds it until it is published: what
/// was moved, and WHEN.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeIoRecord {
    /// The operations and the bytes they moved ([`VolumeIo`]).
    pub performed: VolumeIo,
    /// The instant they were performed at, as epoch seconds — the time stamp the datapoints carry.
    pub at_secs: i64,
}

impl VolumeRecord {
    /// Record that the guest performed `performed` on this volume at `at`.
    ///
    /// The one writer of a volume's pending I/O, so every path a guest can perform I/O by leaves
    /// one kind of record behind.
    pub fn record_guest_io(&mut self, performed: VolumeIo, at: SystemTime) {
        if !performed.happened() {
            return;
        }
        self.guest_io.push(VolumeIoRecord {
            performed,
            at_secs: epoch_secs(at),
        });
        while self.guest_io.len() > MAX_PENDING_IO_RECORDS {
            self.guest_io.remove(0);
        }
    }
}

/// The datapoints the account's volumes owe, taking the I/O records they carry as published.
///
/// Each record is one transfer that happened, so it becomes one datapoint per metric it is a
/// measurement of — and nothing at all for the measurements it is not: a transfer that only read
/// publishes no write series, and a volume of a type Amazon EBS publishes no consumed-IOPS series
/// for carries none. No instant is passed in, because each datapoint carries the instant its own
/// I/O was performed at ([`VolumeIoRecord::at_secs`]).
pub fn due_volume_samples(store: &mut ComAmazonawsEc2Amazonec2State) -> Vec<AgentSample> {
    let mut samples: Vec<AgentSample> = Vec::new();
    for (volume_id, record) in &mut store.volumes {
        if record.guest_io.is_empty() {
            continue;
        }
        let provisioned_iops = record
            .view
            .volume_type
            .as_deref()
            .is_some_and(|volume_type| PROVISIONED_IOPS_VOLUME_TYPES.contains(&volume_type));
        for performed in ::std::mem::take(&mut record.guest_io) {
            samples.extend(samples_of(volume_id, &performed, provisioned_iops));
        }
    }
    samples
}

/// The datapoints ONE transfer on the volume `volume_id` is a measurement of.
fn samples_of(
    volume_id: &str,
    record: &VolumeIoRecord,
    provisioned_iops: bool,
) -> Vec<AgentSample> {
    let performed = record.performed;
    let dimensions = BTreeMap::from([(VOLUME_ID_DIMENSION.to_string(), volume_id.to_string())]);
    VOLUME_METRICS
        .iter()
        .filter(|metric| provisioned_iops || !metric.provisioned_iops_only)
        .filter_map(|metric| {
            let measured = (metric.measures)(&performed);
            // A measurement of nothing is not a measurement: a period in which nothing wrote the
            // volume carries no write datapoint, which is what tells it from one that wrote zero
            // bytes.
            (measured > 0).then(|| {
                #[allow(clippy::cast_precision_loss)]
                let value = measured as f64;
                AgentSample {
                    namespace: NAMESPACE.to_string(),
                    metric_name: metric.name.to_string(),
                    dimensions: dimensions.clone(),
                    value,
                    unit: Some(metric.unit.to_string()),
                    at_secs: record.at_secs,
                }
            })
        })
        .collect()
}

/// `at` as the epoch seconds a datapoint's time stamp carries.
fn epoch_secs(at: SystemTime) -> i64 {
    i64::try_from(
        at.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a transfer of `n` bytes is the operations it takes to move them at the documented
    /// operation size each — over each published bound and one past it.
    #[test]
    fn a_transfer_is_counted_in_operations_of_the_documented_size() {
        // (case, bytes, operations)
        let rows = [
            ("nothing at all", 0_i64, 0_i64),
            ("one byte", 1, 1),
            ("exactly one operation", IO_OPERATION_BYTES, 1),
            ("one byte past one operation", IO_OPERATION_BYTES + 1, 2),
            ("the documented four", 1_024 * 1_024, 4),
            ("a negative figure", -1, 0),
        ];
        for (case, bytes, operations) in rows {
            let counted = operations_for(bytes);
            assert_eq!(
                counted, operations,
                "case {case:?}: expected {operations} operations, got {counted}"
            );
        }
    }

    /// RULE: a transfer publishes exactly the series it is a measurement of, under the volume's own
    /// dimension — and the consumed-IOPS series only for a provisioned-IOPS volume type.
    #[test]
    fn a_transfer_publishes_the_series_it_measures() {
        let at_secs = 1_700_000_000;
        // (case, the transfer, whether the volume provisions IOPS, the series expected)
        let rows: [(&str, VolumeIo, bool, &[&str]); 4] = [
            (
                "a write on a gp3 volume",
                VolumeIo {
                    write_ops: 4,
                    write_bytes: 1_048_576,
                    ..VolumeIo::default()
                },
                false,
                &["VolumeWriteOps", "VolumeWriteBytes"],
            ),
            (
                "the same write on an io2 volume",
                VolumeIo {
                    write_ops: 4,
                    write_bytes: 1_048_576,
                    ..VolumeIo::default()
                },
                true,
                &[
                    "VolumeWriteOps",
                    "VolumeWriteBytes",
                    "VolumeConsumedReadWriteOps",
                ],
            ),
            (
                "a read alone",
                VolumeIo {
                    read_ops: 2,
                    read_bytes: 512_000,
                    ..VolumeIo::default()
                },
                false,
                &["VolumeReadOps", "VolumeReadBytes"],
            ),
            (
                "a read and a write together",
                VolumeIo {
                    read_ops: 1,
                    read_bytes: 1_024,
                    write_ops: 1,
                    write_bytes: 1_024,
                },
                false,
                &[
                    "VolumeReadOps",
                    "VolumeWriteOps",
                    "VolumeReadBytes",
                    "VolumeWriteBytes",
                ],
            ),
        ];
        for (case, performed, provisioned_iops, expected) in rows {
            let record = VolumeIoRecord {
                performed,
                at_secs,
            };
            let samples = samples_of("vol-0123456789abcdef0", &record, provisioned_iops);
            let mut published: Vec<&str> = samples
                .iter()
                .map(|sample| sample.metric_name.as_str())
                .collect();
            published.sort_unstable();
            let mut wanted: Vec<&str> = expected.to_vec();
            wanted.sort_unstable();
            assert_eq!(
                published, wanted,
                "case {case:?}: expected exactly those series, got {samples:?}"
            );
            for sample in &samples {
                assert_eq!(
                    sample.namespace, NAMESPACE,
                    "case {case:?}: expected the volume namespace, got {sample:?}"
                );
                assert_eq!(
                    sample.dimensions.get(VOLUME_ID_DIMENSION).map(String::as_str),
                    Some("vol-0123456789abcdef0"),
                    "case {case:?}: expected the volume's own dimension, got {sample:?}"
                );
                assert_eq!(
                    sample.at_secs, at_secs,
                    "case {case:?}: expected the instant the I/O happened, got {sample:?}"
                );
                assert!(
                    sample.value > 0.0,
                    "case {case:?}: expected a measurement of what happened, got {sample:?}"
                );
            }
        }
    }
}
