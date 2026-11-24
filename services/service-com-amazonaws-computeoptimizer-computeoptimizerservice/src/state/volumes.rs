// SPDX-License-Identifier: CC0-1.0

//! One Amazon EBS volume as Compute Optimizer observes it, and the recommendation every
//! reader of one derives from that observation.
//!
//! WHICH volumes the service observes is not here: a volume belongs to the account's Amazon
//! EC2 inventory and its utilization to the `AWS/EBS` series published for it, so the
//! observation of each one is read from those two stores at the moment of a read
//! ([`analysed_volumes`]). This file holds what is derived FROM one observation, so every
//! surface that reports a volume derives it the same way.
//!
//! Part of [`super`]'s one state module, in its own file so one concern lives in one place.

use super::*;

/// One Amazon EBS volume Compute Optimizer observes: its configuration, its tags, and
/// the metric samples a recommendation for it is computed from. This is the OBSERVATION;
/// everything a `VolumeRecommendation` reports is derived from it, so no two readers of
/// the same volume can disagree.
#[derive(Debug, Clone)]
pub struct ObservedVolume {
    /// The volume's EC2 volume id, which is its key in the inventory. The ARN a reader
    /// reports is built from it and the region the request was signed for
    /// ([`ObservedVolume::volume_arn`]), because Compute Optimizer is a REGIONAL service:
    /// the volumes it reports are the ones in the Region the caller addressed.
    pub volume_id: String,
    /// The account that owns the volume.
    pub account_id: String,
    /// The volume's configuration as EC2 reports it: type, size, and the baseline and
    /// burst IOPS and throughput that type and size deliver.
    pub configuration: VolumeConfiguration,
    /// The volume's tags, as key/value pairs, in the order EC2 reports them.
    pub tags: Vec<(String, String)>,
    /// The metric samples observed for the volume, each one interval of the `AWS/EBS` series
    /// published for it ([`analysed_volumes`]).
    pub samples: Vec<VolumeSample>,
}

impl ObservedVolume {
    /// This volume's ARN in `region`: `arn:aws:ec2:<region>:<account>:volume/<id>`.
    pub fn volume_arn(&self, region: &str) -> String {
        ::engine::emulator_arn("ec2", region, &format!("volume/{}", self.volume_id))
    }

    /// The samples the look-back period in force analyses: those taken within
    /// `look_back_days` of the last refresh.
    pub(super) fn analysed_samples(&self, look_back_days: f64) -> impl Iterator<Item = &VolumeSample> {
        self.samples
            .iter()
            .filter(move |sample| sample.days_ago <= look_back_days)
    }

    /// The peak of one sample measurement over the analysed period, or `0.0` when the
    /// period holds no sample.
    pub(super) fn peak(&self, look_back_days: f64, of: fn(&VolumeSample) -> f64) -> f64 {
        self.analysed_samples(look_back_days)
            .map(of)
            .fold(0.0_f64, f64::max)
    }

    /// Whether any analysed sample `exceeded` answers true for, as the `1`/`0` indicator the
    /// two "exceeded" metrics report: "a value of 1 means the provisioned IOPS were exceeded;
    /// a value of 0 means they were not" (`EBSUtilizationMetric$name`), reported under the
    /// `Maximum` statistic — so a period in which the volume's provisioned performance was
    /// exceeded at all reports 1.
    pub(super) fn exceeded_indicator(
        &self,
        look_back_days: f64,
        exceeded: impl Fn(&VolumeSample) -> bool,
    ) -> f64 {
        f64::from(u8::from(
            self.analysed_samples(look_back_days).any(exceeded),
        ))
    }

    /// "The number of days for which utilization metrics were analyzed for the volume"
    /// (`VolumeRecommendation$lookBackPeriodInDays`): the length of the look-back period in
    /// force ([`look_back_period_in_days`]), which is the window the samples analysed were
    /// taken in ([`Self::analysed_samples`]).
    pub fn analysed_days(&self, look_back_period: &str) -> f64 {
        look_back_period_in_days(look_back_period)
    }

    /// The metrics `VolumeRecommendation$utilizationMetrics` reports: every
    /// `EBSMetricName` the model declares, each the peak of the analysed samples — and,
    /// for the two "exceeded" metrics, whether any analysed sample's demand went past the
    /// capacity the volume's configuration delivers ([`Self::exceeded_indicator`]).
    pub fn utilization_metrics(&self, look_back_days: f64) -> Vec<EBSUtilizationMetric> {
        let iops_capacity = configured_iops_capacity(&self.configuration);
        let throughput_capacity = configured_throughput_capacity(&self.configuration);
        let metric = |name: &str, value: f64| EBSUtilizationMetric {
            name: Some(name.to_string()),
            statistic: Some(METRIC_STATISTIC_MAXIMUM.to_string()),
            value,
        };
        vec![
            metric(
                EBS_METRIC_READ_OPS_PER_SECOND,
                self.peak(look_back_days, |sample| sample.read_ops_per_second),
            ),
            metric(
                EBS_METRIC_WRITE_OPS_PER_SECOND,
                self.peak(look_back_days, |sample| sample.write_ops_per_second),
            ),
            metric(
                EBS_METRIC_READ_BYTES_PER_SECOND,
                self.peak(look_back_days, |sample| sample.read_bytes_per_second),
            ),
            metric(
                EBS_METRIC_WRITE_BYTES_PER_SECOND,
                self.peak(look_back_days, |sample| sample.write_bytes_per_second),
            ),
            metric(
                EBS_METRIC_IOPS_EXCEEDED,
                self.exceeded_indicator(look_back_days, |sample| sample.iops() > iops_capacity),
            ),
            metric(
                EBS_METRIC_THROUGHPUT_EXCEEDED,
                self.exceeded_indicator(look_back_days, |sample| {
                    sample.throughput_mib_per_second() > throughput_capacity
                }),
            ),
        ]
    }

    /// The configuration this service recommends for the volume: gp3 at the same size,
    /// with IOPS and throughput provisioned to carry the analysed peak plus
    /// [`RECOMMENDATION_HEADROOM`], inside gp3's documented limits. A volume whose peak
    /// already fits gp3's included performance gets gp3's included performance.
    pub fn recommended_configuration(&self, look_back_days: f64) -> VolumeConfiguration {
        let size = self.configuration.volume_size.max(0);
        let headroom = 1.0 + RECOMMENDATION_HEADROOM;

        let peak_iops = self.peak(look_back_days, VolumeSample::iops);
        let iops_ceiling = GP3_MAX_IOPS
            .min(size.saturating_mul(GP3_MAX_IOPS_PER_GIB))
            .max(GP3_INCLUDED_IOPS);
        let iops = ((peak_iops * headroom).ceil() as i32).clamp(GP3_INCLUDED_IOPS, iops_ceiling);

        let peak_throughput = self.peak(look_back_days, VolumeSample::throughput_mib_per_second);
        let throughput_ceiling = GP3_MAX_THROUGHPUT_MIB
            .min((f64::from(iops) * GP3_MAX_THROUGHPUT_MIB_PER_IOPS) as i32)
            .max(GP3_INCLUDED_THROUGHPUT_MIB);
        let throughput = ((peak_throughput * headroom).ceil() as i32)
            .clamp(GP3_INCLUDED_THROUGHPUT_MIB, throughput_ceiling);

        VolumeConfiguration {
            volume_type: Some(VOLUME_TYPE_GP3.to_string()),
            volume_size: size,
            volume_baseline_iops: iops,
            // gp3 performance is provisioned rather than bursted — a gp3 volume uses no
            // burst credits — so the option delivers its baseline and nothing above it.
            volume_burst_iops: 0,
            volume_baseline_throughput: throughput,
            volume_burst_throughput: 0,
            // A recommendation changes a volume's type and provisioned performance; the
            // volume stays attached where it was, so whether it is a root volume is a
            // property of the volume, not of the option.
            root_volume: self.configuration.root_volume,
        }
    }

    /// How close the analysed peak came to what `configuration` sustains, as a
    /// `PerformanceRisk` on the model's 0..4 scale: 0 while the peak stays under a
    /// quarter of the sustained capacity, 4 once it reaches 125% of it. "The higher the
    /// risk, the more likely the current EBS volume doesn't have sufficient capacity"
    /// (`VolumeRecommendation$currentPerformanceRisk`), so the score rises with that
    /// ratio and is measured against the BASELINE: burst capacity is not sustained.
    pub fn performance_risk(
        &self,
        configuration: &VolumeConfiguration,
        look_back_days: f64,
    ) -> f64 {
        let baseline_iops = f64::from(configuration.volume_baseline_iops.max(0));
        let baseline_throughput = f64::from(configuration.volume_baseline_throughput.max(0));
        let iops_ratio = demand_ratio(self.peak(look_back_days, VolumeSample::iops), baseline_iops);
        let throughput_ratio = demand_ratio(
            self.peak(look_back_days, VolumeSample::throughput_mib_per_second),
            baseline_throughput,
        );
        performance_risk_score_of_ratio(iops_ratio.max(throughput_ratio))
    }

    /// The volume's tags as the modelled `Tags` list, or `None` when it carries none.
    pub fn tag_list(&self) -> Option<Vec<Tag>> {
        modelled_tags(&self.tags)
    }

    /// The volume recommendation this observation yields in `region` under
    /// `look_back_period`, as generated at `last_refresh_timestamp`.
    ///
    /// Every figure is computed here from the stored configuration and samples: the
    /// metrics are the analysed peaks, the option is the configuration those peaks ask
    /// for, each cost comes from [`volume_monthly_cost`], and the finding is
    /// `NotOptimized` exactly when the option would either carry a peak the current
    /// configuration does not sustain or cost less than it — "a volume is considered not
    /// optimized when Compute Optimizer identifies a recommendation that can provide
    /// better performance for your workload", and `Optimized` otherwise, where "Compute
    /// Optimizer might [still] recommend a new generation volume type"
    /// (`VolumeRecommendation$finding`).
    /// `after_discounts` is whether the account activated the savings-estimation-mode
    /// preference for the volume ([`EffectivePreferences::after_discounts`]), which is what
    /// decides whether the savings member net of discounts is reported at all.
    pub fn to_recommendation(
        &self,
        region: &str,
        look_back_period: &str,
        after_discounts: bool,
        last_refresh_timestamp: Option<DateTime>,
    ) -> VolumeRecommendation {
        let look_back_days = self.analysed_days(look_back_period);
        let current = self.configuration.clone();
        let recommended = self.recommended_configuration(look_back_days);
        let current_risk = self.performance_risk(&current, look_back_days);
        let option_risk = self.performance_risk(&recommended, look_back_days);
        let savings = self.savings(look_back_days);

        VolumeRecommendation {
            volume_arn: Some(self.volume_arn(region)),
            account_id: Some(self.account_id.clone()),
            current_configuration: Some(current),
            finding: Some(self.finding(look_back_days).to_string()),
            utilization_metrics: Some(self.utilization_metrics(look_back_days)),
            look_back_period_in_days: look_back_days,
            volume_recommendation_options: Some(vec![VolumeRecommendationOption {
                configuration: Some(recommended),
                performance_risk: option_risk,
                // One option, so it is the first-ranked one. "Rank 1 is considered the
                // best" (`VolumeRecommendationOption$rank`).
                rank: 1,
                savings_opportunity: savings.map(|(percentage, value)| SavingsOpportunity {
                    savings_opportunity_percentage: percentage,
                    estimated_monthly_savings: Some(EstimatedMonthlySavings {
                        currency: Some(CURRENCY_USD.to_string()),
                        value,
                    }),
                }),
                // The savings net of Savings Plans and Reserved Instances discounts are
                // reported only once the account activates the preference that "activates"
                // them, the one gate every recommendation surface of this service applies.
                // This emulator prices a volume from the public price table in
                // [`volume_monthly_cost`] and records no negotiated discount for storage, so
                // where the member is reported its figures are the before-discount ones —
                // which is what the reported [`SAVINGS_ESTIMATION_SOURCE`] says.
                savings_opportunity_after_discounts: savings
                    .filter(|_| after_discounts)
                    .map(|(percentage, value)| EBSSavingsOpportunityAfterDiscounts {
                        savings_opportunity_percentage: percentage,
                        estimated_monthly_savings: Some(EBSEstimatedMonthlySavings {
                            currency: Some(CURRENCY_USD.to_string()),
                            value,
                        }),
                    }),
            }]),
            last_refresh_timestamp,
            current_performance_risk: Some(performance_risk_band(current_risk).to_string()),
            effective_recommendation_preferences: Some(EBSEffectiveRecommendationPreferences {
                savings_estimation_mode: Some(EBSSavingsEstimationMode {
                    source: Some(SAVINGS_ESTIMATION_SOURCE.to_string()),
                }),
                look_back_period: Some(look_back_period.to_string()),
            }),
            tags: self.tag_list(),
        }
    }

    /// The volume's `EBSFinding` over the analysed period: `NotOptimized` exactly when the
    /// recommended configuration would either carry a peak the current one does not
    /// sustain or cost less than it, `Optimized` otherwise. `EBSFinding`'s two values are
    /// [`FINDING_NOT_OPTIMIZED`] and [`FINDING_OPTIMIZED`].
    pub fn finding(&self, look_back_days: f64) -> &'static str {
        let recommended = self.recommended_configuration(look_back_days);
        let safer = self.performance_risk(&recommended, look_back_days)
            < self.performance_risk(&self.configuration, look_back_days);
        let cheaper = match (
            volume_monthly_cost(&self.configuration),
            volume_monthly_cost(&recommended),
        ) {
            (Some(current), Some(option)) => option < current,
            _ => false,
        };
        if safer || cheaper {
            FINDING_NOT_OPTIMIZED
        } else {
            FINDING_OPTIMIZED
        }
    }

    /// What adopting the recommended configuration saves per month and that saving as a
    /// percentage of what the volume costs today, or `None` for a volume type this
    /// emulator holds no price for ([`volume_monthly_cost`]) — whose recommendation then
    /// carries no savings figure rather than an invented one.
    pub fn savings(&self, look_back_days: f64) -> Option<(f64, f64)> {
        let current = volume_monthly_cost(&self.configuration)?;
        let option = volume_monthly_cost(&self.recommended_configuration(look_back_days))?;
        let saved = round_hundredths((current - option).max(0.0));
        Some((percentage_of(saved, current), saved))
    }

    /// This volume as a row of the findings catalog, i.e. as `GetRecommendationSummaries`
    /// aggregates it, under the look-back period in force.
    ///
    /// The projection is what keeps one volume's analysis in ONE place: the summary reader
    /// and `GetEBSVolumeRecommendations` both answer from this observation, so they cannot
    /// disagree about a volume's finding, its performance risk or what adopting its
    /// recommendation saves. A volume recommendation reports no finding reason codes
    /// (`VolumeRecommendation` declares no such member) and infers no workload type, so
    /// the row carries neither.
    pub fn to_analysed_resource(&self, look_back_period: &str) -> AnalysedResource {
        let look_back_days = self.analysed_days(look_back_period);
        let current_monthly_cost = volume_monthly_cost(&self.configuration).unwrap_or_default();
        let recommended_monthly_cost =
            volume_monthly_cost(&self.recommended_configuration(look_back_days))
                .unwrap_or(current_monthly_cost);
        AnalysedResource {
            account_id: self.account_id.clone(),
            resource_type: SOURCE_TYPE_EBS_VOLUME.to_string(),
            // The catalog records a resource's REGION-FREE identity
            // ([`AnalysedResource::arn_in`]), which for a volume is its EC2 volume id.
            arn_service: "ec2".to_string(),
            arn_resource: format!("volume/{}", self.volume_id),
            finding: self.finding(look_back_days).to_string(),
            finding_reason_codes: Vec::new(),
            current_performance_risk: performance_risk_band(
                self.performance_risk(&self.configuration, look_back_days),
            )
            .to_string(),
            current_monthly_cost,
            recommended_monthly_cost,
            inferred_workload_types: Vec::new(),
            // A volume is not a database, so there is no database configuration to record.
            rds_database: None,
        }
    }
}
