// SPDX-License-Identifier: CC0-1.0

//! The Amazon EBS volumes Compute Optimizer has analysed FOR AN ACCOUNT.
//!
//! Compute Optimizer "generates recommendations for Amazon EBS volumes that meet a specific
//! set of requirements" from the utilization it observes on a volume the account OWNS, so
//! the volumes it can report are the account's own inventory and nothing else: a volume no
//! EBS volume of the account corresponds to has no finding to report, and an account that
//! holds no volume at all reads the empty list. The inventory is Amazon EC2's own store —
//! the service that owns an EBS volume — and the utilization is the `AWS/EBS` series
//! published for that volume, exactly as the Amazon ECS analysis of this service reads the
//! ECS inventory and the `AWS/ECS` series ([`super::ecs_analysis`]) and the database analysis
//! reads the Amazon RDS inventory ([`super::rds_inventory`]).
//!
//! Everything a reader reports about a volume is computed here from those two inputs and from
//! the preferences in force for it: the configuration it runs today, the peaks observed on
//! it, the finding those peaks yield, the configuration the analysis would move it to and what
//! that would save ([`ObservedVolume`]). Nothing is written: this is the reporting side of an
//! analysis no request to this API creates.
//!
//! [`analysed_volumes`] is the ONE selection every EBS volume surface of this service reads —
//! the recommendation reader, the summary that counts it and the export of the same rows — so
//! no two of them can disagree about which volumes the account has an analysis for.

use super::*;

use ::service_com_amazonaws_ec2_amazonec2 as ec2;

use crate::ops::types::LOOK_BACK_PERIOD_PREFERENCE_VALUES;

/// The CloudWatch namespace Amazon EBS publishes a volume's utilization in, and the dimension
/// that identifies ONE volume's series in it.
///
/// Source: the Amazon EBS metrics reference ("Amazon CloudWatch metrics for Amazon EBS"),
/// which publishes the per-volume metrics below in the `AWS/EBS` namespace under the
/// `VolumeId` dimension.
pub const EBS_METRIC_NAMESPACE: &str = "AWS/EBS";
/// See [`EBS_METRIC_NAMESPACE`].
pub const EBS_VOLUME_DIMENSION: &str = "VolumeId";

/// How one published series fills a sample: the measurement of a [`VolumeSample`] a
/// datapoint's per-second rate is read into.
type Measurement = fn(&mut VolumeSample) -> &mut f64;

/// The `AWS/EBS` series this analysis reads, each paired with the measurement of a
/// [`VolumeSample`] it fills.
///
/// Left column: the metric name Amazon EBS publishes. Right column: the per-second rate of a
/// sample that measurement is. Amazon EBS publishes each of these as a COUNT over the
/// datapoint's period — "to calculate the average IOPS for a period, divide the `Sum` of
/// `VolumeReadOps`/`VolumeWriteOps` by the period in seconds", and likewise for the byte
/// counts — so a datapoint is read as its sum over the period it stands for
/// ([`cloudwatch::state::StoredDatum::aggregated_span`]), which is the per-second rate this
/// API's `VolumeRead/WriteOpsPerSecond` and `VolumeRead/WriteBytesPerSecond` report.
const ANALYSED_SERIES: [(&str, Measurement); 4] = [
    ("VolumeReadOps", |sample| &mut sample.read_ops_per_second),
    ("VolumeWriteOps", |sample| &mut sample.write_ops_per_second),
    ("VolumeReadBytes", |sample| &mut sample.read_bytes_per_second),
    ("VolumeWriteBytes", |sample| {
        &mut sample.write_bytes_per_second
    }),
];

/// The device names an instance BOOTS from, which is what makes a volume attached at one of
/// them the instance's root volume (`VolumeConfiguration$rootVolume`, "contains the image used
/// to boot the instance during launch").
///
/// The names are Amazon EC2's own ([`ec2::state::LINUX_ROOT_DEVICE_NAME`] and
/// [`ec2::state::WINDOWS_ROOT_DEVICE_NAME`], which is also the instance-store root device
/// name), read from that service rather than restated here, so a volume this analysis calls a
/// root volume is a volume attached where EC2 mounts a root device.
const ROOT_DEVICE_NAMES: [&str; 2] = [
    ec2::state::LINUX_ROOT_DEVICE_NAME,
    ec2::state::WINDOWS_ROOT_DEVICE_NAME,
];

/// The analysed Amazon EBS volumes of one account at the endpoint of `region`, by volume id —
/// the order Amazon EC2 holds them in, so a page boundary is stable across calls.
///
/// A volume becomes an analysed resource only once the analysis has something to compute from,
/// which is the "specific set of requirements" Compute Optimizer documents for a volume: the
/// account is enrolled and the analysis its enrollment started is due
/// ([`Enrollment::analysis_visible`], the gate every reader of this service applies), the
/// volume is one this endpoint's region holds and Amazon EC2 still holds it, and the account
/// has published utilization for it in the look-back window. A volume that fails any of those
/// is absent from the answer — the state of every volume of a freshly enrolled account, which
/// Compute Optimizer has accumulated no metric history for yet — rather than reported with an
/// invented finding.
///
/// Reads only: the Amazon EC2 inventory and the CloudWatch series are another service's state,
/// and this never writes to either, nor to this service's own store.
pub fn analysed_volumes<Eng: Send + Sync + 'static>(
    engine: &Eng,
    store: &ComAmazonawsComputeoptimizerComputeoptimizerserviceState,
    region: &str,
    account_id: &str,
    now: f64,
) -> Vec<ObservedVolume> {
    // The gate every recommendation reader applies, ahead of any inventory read: an account
    // that never opted in, one that opted out and one that has just opted in hold no analysis
    // at all, whatever their inventory holds.
    if !store.enrollment.analysis_visible() {
        return Vec::new();
    }
    // The inventory holds the resources of the ONE account this emulator presents
    // ([`engine::emulator_account_id`]), so an account the request named that is not it — a
    // member account of the organization this service records — owns no volume here and has
    // no inventory swept on its behalf.
    if account_id != ::engine::emulator_account_id() {
        return Vec::new();
    }
    // An emulator that does not hold the Amazon EC2 inventory, or the metric store the
    // analysis reads, has no volume to analyse — the same answer as an account that owns none
    // — rather than a finding computed from an input nothing provided.
    let (Some(inventory), Some(observations)) = (
        ec2::state::service_state(engine),
        cloudwatch::state::service_state(engine),
    ) else {
        return Vec::new();
    };
    let inventory = inventory.lock();
    let observations = observations.lock();
    // The longest window any look-back preference can ask for: the samples kept here are the
    // ones some preference could analyse, and the preference in force for a given read narrows
    // them further ([`ObservedVolume::analysed_samples`]).
    let oldest = now - longest_look_back_days() * SECONDS_PER_DAY;
    let live = ::std::time::Instant::now();
    inventory
        .volumes
        .values()
        // A volume Amazon EC2 has taken back is one the account no longer holds, so it is no
        // more analysed than a deleted one is.
        .filter(|record| !record.is_withdrawn_at(live))
        .filter(|record| volume_region(record) == Some(region))
        .filter_map(|record| observed_volume(record, &observations, account_id, live, oldest, now))
        .collect()
}

/// The Region one stored EBS volume is in: the Region of its Availability Zone
/// ([`ec2::state::region_of_zone_name`], Amazon EC2's own reading of which Region a zone
/// belongs to). `None` for a volume whose record names no zone, which no regional endpoint
/// holds.
fn volume_region(record: &ec2::state::VolumeRecord) -> Option<&str> {
    record
        .view
        .availability_zone
        .as_deref()
        .map(ec2::state::region_of_zone_name)
}

/// The longest look-back window a preference can ask for, in days: the longest of the
/// `LookBackPeriodPreference` values the model declares
/// ([`LOOK_BACK_PERIOD_PREFERENCE_VALUES`]), so the samples this analysis keeps cover every
/// period a preference could put in force and no shorter range is hard-coded beside them.
///
/// The crate's ONE reading of it, shared by every inventory analysis of this service, so two
/// of them cannot keep different histories.
pub fn longest_look_back_days() -> f64 {
    LOOK_BACK_PERIOD_PREFERENCE_VALUES
        .iter()
        .map(|preference| look_back_period_in_days(preference))
        .fold(0.0_f64, f64::max)
}

/// One volume of the account's Amazon EC2 inventory as an observation this service can
/// analyse, or `None` for one the analysis has nothing to compute a finding from (see
/// [`analysed_volumes`]).
fn observed_volume(
    record: &ec2::state::VolumeRecord,
    observations: &cloudwatch::state::ComAmazonawsCloudwatchGraniteserviceversion20100801State,
    account_id: &str,
    live: ::std::time::Instant,
    oldest: f64,
    now: f64,
) -> Option<ObservedVolume> {
    let volume_id = record.view.volume_id.clone()?;
    // "Compute Optimizer generates recommendations for ... volumes that meet a specific set of
    // requirements": the utilization observed on the volume over the look-back period is the
    // analysis's input — Amazon EBS publishes it while the volume is attached to a running
    // instance — so a volume the account published none for has no finding at all.
    let samples = observed_samples(observations, &volume_id, oldest, now);
    if samples.is_empty() {
        return None;
    }
    Some(ObservedVolume {
        volume_id,
        account_id: account_id.to_string(),
        configuration: volume_configuration(record, live),
        tags: record
            .view
            .tags
            .iter()
            .flatten()
            .map(|tag| {
                (
                    tag.key.clone().unwrap_or_default(),
                    tag.value.clone().unwrap_or_default(),
                )
            })
            .collect(),
        samples,
    })
}

/// The utilization the account published for one volume over the window `oldest..now`, as the
/// per-second samples a recommendation is computed from: one sample per interval CloudWatch
/// aggregates the series in ([`cloudwatch::state::StoredDatum::aggregated_instant`]), most
/// recent last.
///
/// One sample holds every measurement published for that interval, so the read and write rates
/// a sample carries are the rates observed AT THE SAME TIME: that is what lets the analysis
/// add them into the IOPS and the throughput the volume's capacity had to carry in that
/// interval. Two datapoints of one series in one interval are folded to the higher rate, since
/// what the analysis reports is the peak observed.
fn observed_samples(
    observations: &cloudwatch::state::ComAmazonawsCloudwatchGraniteserviceversion20100801State,
    volume_id: &str,
    oldest: f64,
    now: f64,
) -> Vec<VolumeSample> {
    let mut intervals: BTreeMap<i64, VolumeSample> = BTreeMap::new();
    for (metric_name, measurement) in ANALYSED_SERIES {
        let key = cloudwatch::state::MetricKey {
            namespace: EBS_METRIC_NAMESPACE.to_string(),
            metric_name: metric_name.to_string(),
            dimensions: [(EBS_VOLUME_DIMENSION.to_string(), volume_id.to_string())]
                .into_iter()
                .collect(),
        };
        let Some(data) = observations.metrics.get(&key) else {
            continue;
        };
        for datum in data.iter() {
            // A datapoint whose aggregates are not finite describes no utilization, so it is
            // left out of the fold exactly as one outside the window is — the same rule the
            // other analyses of this service fold a series by.
            if !datum.timestamp_secs.is_finite() || !datum.sum.is_finite() {
                continue;
            }
            let at = datum.aggregated_instant();
            if at < oldest || at > now {
                continue;
            }
            let rate = datum.sum / datum.aggregated_span();
            let sample = intervals.entry(at as i64).or_insert_with(|| VolumeSample {
                days_ago: ((now - at) / SECONDS_PER_DAY).max(0.0),
                ..VolumeSample::default()
            });
            let observed = measurement(sample);
            *observed = observed.max(rate);
        }
    }
    intervals.into_values().collect()
}

/// The configuration one stored EBS volume runs today, as `VolumeConfiguration` describes it:
/// the type and size Amazon EC2 holds for it, the baseline and burst performance that type and
/// size deliver ([`volume_performance`]), and whether an instance boots from it.
fn volume_configuration(
    record: &ec2::state::VolumeRecord,
    live: ::std::time::Instant,
) -> VolumeConfiguration {
    let view = &record.view;
    let volume_type = view
        .volume_type
        .clone()
        .unwrap_or_else(|| ec2::state::DEFAULT_VOLUME_TYPE.to_string());
    let size = view.size.unwrap_or_default().max(0);
    let performance = volume_performance(&volume_type, size, view.iops, view.throughput);
    VolumeConfiguration {
        volume_type: Some(volume_type),
        volume_size: size,
        volume_baseline_iops: performance.baseline_iops,
        volume_burst_iops: performance.burst_iops,
        volume_baseline_throughput: performance.baseline_throughput,
        volume_burst_throughput: performance.burst_throughput,
        // The attachments the volume holds NOW, as Amazon EC2 reports them — one whose attach
        // the clock has finished counts, one whose detach it has finished does not.
        root_volume: Some(record.attachments_at(live).iter().any(|attachment| {
            attachment
                .device
                .as_deref()
                .is_some_and(|device| ROOT_DEVICE_NAMES.contains(&device))
        })),
    }
}

/// What a volume of `volume_type` at `size` GiB, provisioned with `iops` and `throughput`,
/// delivers: the baseline it sustains and the burst it can reach above it.
#[derive(Debug, Clone, Copy, Default)]
struct VolumePerformance {
    /// `volumeBaselineIOPS` — the IOPS the volume sustains.
    baseline_iops: i32,
    /// `volumeBurstIOPS` — the IOPS it can reach above that baseline, `0` for a type that
    /// bursts none.
    burst_iops: i32,
    /// `volumeBaselineThroughput` — the throughput in MiB/s the volume sustains.
    baseline_throughput: i32,
    /// `volumeBurstThroughput` — the throughput in MiB/s the volume can reach: `0` for a type
    /// that bursts none, and the baseline figure itself where the type reaches no more than
    /// what it sustains at that size (a small `gp2` volume).
    burst_throughput: i32,
}

/// The IOPS a `gp2` volume sustains: "gp2 volumes deliver a consistent baseline of 3 IOPS per
/// GiB", with a floor of 100 IOPS and a ceiling of 16,000 (the Amazon EBS volume types
/// reference, "General Purpose SSD volumes").
const GP2_IOPS_PER_GIB: i32 = 3;
/// See [`GP2_IOPS_PER_GIB`].
const GP2_MIN_IOPS: i32 = 100;
/// See [`GP2_IOPS_PER_GIB`].
const GP2_MAX_IOPS: i32 = 16_000;
/// The IOPS a `gp2` volume whose baseline is below it can BURST to: "volumes with a baseline
/// performance below 3,000 IOPS can burst to 3,000 IOPS" (same reference).
const GP2_BURST_IOPS: i32 = 3_000;
/// The throughput a `gp2` volume delivers, in MiB/s: 128 MiB/s for a small volume, and 250
/// MiB/s from [`GP2_BURST_THROUGHPUT_GIB`] on when burst credits are available, sustained from
/// [`GP2_FULL_THROUGHPUT_GIB`] on (same reference, "gp2 volume throughput").
const GP2_SMALL_THROUGHPUT_MIB: i32 = 128;
/// See [`GP2_SMALL_THROUGHPUT_MIB`].
const GP2_MAX_THROUGHPUT_MIB: i32 = 250;
/// See [`GP2_SMALL_THROUGHPUT_MIB`].
const GP2_BURST_THROUGHPUT_GIB: i32 = 170;
/// See [`GP2_SMALL_THROUGHPUT_MIB`].
const GP2_FULL_THROUGHPUT_GIB: i32 = 334;

/// The throughput a Provisioned IOPS SSD volume reaches at most, in MiB/s, and the provisioned
/// IOPS it takes to reach it: "io1 and io2 volumes can achieve a maximum throughput of 1,000
/// MiB/s at 16,000 IOPS or more" (the Amazon EBS volume types reference, "Provisioned IOPS SSD
/// volumes"), which is 64 KiB per I/O up to that ceiling.
const PROVISIONED_IOPS_MAX_THROUGHPUT_MIB: i32 = 1_000;
/// See [`PROVISIONED_IOPS_MAX_THROUGHPUT_MIB`].
const PROVISIONED_IOPS_FOR_MAX_THROUGHPUT: i32 = 16_000;

/// Bytes a TiB holds in GiB — the unit the HDD throughput figures below are stated per.
const GIB_PER_TIB: f64 = 1_024.0;

/// What one HDD volume type delivers, as `(type, baseline MiB/s per TiB, burst MiB/s per TiB,
/// max MiB/s, max IOPS)`.
///
/// Source: the Amazon EBS volume types reference — `st1` "throughput credits accumulate at 40
/// MiB/s per TiB" and bursts to "250 MiB/s per TiB", up to 500 MiB/s and 500 IOPS per volume;
/// `sc1` accumulates at "12 MiB/s per TiB" and bursts to "80 MiB/s per TiB", up to 250 MiB/s
/// and 250 IOPS per volume. An HDD volume is driven in 1 MiB I/O operations, so its IOPS and
/// its throughput in MiB/s are the same figure up to the per-volume IOPS ceiling.
const HDD_PERFORMANCE: [(&str, f64, f64, i32, i32); 2] = [
    ("st1", 40.0, 250.0, 500, 500),
    ("sc1", 12.0, 80.0, 250, 250),
];

/// What a `standard` (magnetic) volume delivers: "magnetic volumes deliver approximately 100
/// IOPS on average" and 40 to 90 MiB/s of throughput (the Amazon EBS volume types reference,
/// "Previous generation volumes").
const MAGNETIC_IOPS: i32 = 100;
/// See [`MAGNETIC_IOPS`].
const MAGNETIC_BASELINE_THROUGHPUT_MIB: i32 = 40;
/// See [`MAGNETIC_IOPS`].
const MAGNETIC_BURST_THROUGHPUT_MIB: i32 = 90;

/// The performance a volume of `volume_type` at `size` GiB delivers, given the `iops` and
/// `throughput` Amazon EC2 holds provisioned for it.
///
/// `VolumeConfiguration` states a volume's baseline and burst IOPS and throughput, which
/// Amazon EC2 does not store: a volume stores only what was PROVISIONED on it (the `Iops` and
/// `Throughput` a volume of a provisioning type carries), and what a type and size DELIVER is a
/// fact of the volume type, published in the Amazon EBS volume types reference. Each arm below
/// cites the figure it applies. A type the reference publishes no figure for delivers what was
/// provisioned on it and nothing more, rather than an invented capacity.
fn volume_performance(
    volume_type: &str,
    size: i32,
    iops: Option<i32>,
    throughput: Option<i32>,
) -> VolumePerformance {
    let provisioned_iops = iops.unwrap_or_default().max(0);
    let provisioned_throughput = throughput.unwrap_or_default().max(0);
    // An HDD volume's performance scales with its size, so its row is read first.
    if let Some((_, baseline_per_tib, burst_per_tib, max_throughput, max_iops)) =
        hdd_performance(volume_type)
    {
        let tib = f64::from(size) / GIB_PER_TIB;
        let baseline_throughput = ((baseline_per_tib * tib).round() as i32).clamp(0, max_throughput);
        let burst_throughput = ((burst_per_tib * tib).round() as i32).clamp(0, max_throughput);
        return VolumePerformance {
            baseline_iops: baseline_throughput.min(max_iops),
            burst_iops: burst_throughput.min(max_iops),
            baseline_throughput,
            burst_throughput,
        };
    }
    match volume_type {
        // gp3 performance is PROVISIONED independently of size and does not burst: a gp3
        // volume delivers exactly what was provisioned on it, which for a volume created
        // without either parameter is gp3's included performance (Amazon EC2's own
        // `DEFAULT_GP3_IOPS` / `DEFAULT_GP3_THROUGHPUT`).
        VOLUME_TYPE_GP3 => VolumePerformance {
            baseline_iops: if provisioned_iops > 0 {
                provisioned_iops
            } else {
                ec2::state::DEFAULT_GP3_IOPS
            },
            burst_iops: 0,
            baseline_throughput: if provisioned_throughput > 0 {
                provisioned_throughput
            } else {
                ec2::state::DEFAULT_GP3_THROUGHPUT
            },
            burst_throughput: 0,
        },
        "gp2" => {
            let baseline_iops =
                (GP2_IOPS_PER_GIB.saturating_mul(size)).clamp(GP2_MIN_IOPS, GP2_MAX_IOPS);
            VolumePerformance {
                baseline_iops,
                burst_iops: if baseline_iops < GP2_BURST_IOPS {
                    GP2_BURST_IOPS
                } else {
                    0
                },
                baseline_throughput: if size >= GP2_FULL_THROUGHPUT_GIB {
                    GP2_MAX_THROUGHPUT_MIB
                } else {
                    GP2_SMALL_THROUGHPUT_MIB
                },
                burst_throughput: if size >= GP2_BURST_THROUGHPUT_GIB {
                    GP2_MAX_THROUGHPUT_MIB
                } else {
                    GP2_SMALL_THROUGHPUT_MIB
                },
            }
        }
        "io1" | "io2" => VolumePerformance {
            baseline_iops: provisioned_iops,
            burst_iops: 0,
            baseline_throughput: provisioned_iops_throughput(provisioned_iops),
            burst_throughput: 0,
        },
        "standard" => VolumePerformance {
            baseline_iops: MAGNETIC_IOPS,
            burst_iops: 0,
            baseline_throughput: MAGNETIC_BASELINE_THROUGHPUT_MIB,
            burst_throughput: MAGNETIC_BURST_THROUGHPUT_MIB,
        },
        _ => VolumePerformance {
            baseline_iops: provisioned_iops,
            burst_iops: 0,
            baseline_throughput: provisioned_throughput,
            burst_throughput: 0,
        },
    }
}

/// The row [`HDD_PERFORMANCE`] holds for `volume_type`, or `None` for a type that is not an
/// HDD type.
fn hdd_performance(volume_type: &str) -> Option<(&'static str, f64, f64, i32, i32)> {
    HDD_PERFORMANCE
        .iter()
        .find(|(name, _, _, _, _)| *name == volume_type)
        .copied()
}

/// The throughput in MiB/s a Provisioned IOPS SSD volume with `provisioned_iops` delivers, per
/// [`PROVISIONED_IOPS_MAX_THROUGHPUT_MIB`]: its share of the ceiling, and the ceiling itself
/// from [`PROVISIONED_IOPS_FOR_MAX_THROUGHPUT`] on.
fn provisioned_iops_throughput(provisioned_iops: i32) -> i32 {
    let per_iops = f64::from(PROVISIONED_IOPS_MAX_THROUGHPUT_MIB)
        / f64::from(PROVISIONED_IOPS_FOR_MAX_THROUGHPUT);
    ((f64::from(provisioned_iops) * per_iops).round() as i32)
        .clamp(0, PROVISIONED_IOPS_MAX_THROUGHPUT_MIB)
}
