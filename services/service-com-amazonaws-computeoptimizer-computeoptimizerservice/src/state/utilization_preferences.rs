// SPDX-License-Identifier: CC0-1.0

//! How a utilization preference's threshold and headroom RESOLVE into the capacity a
//! rightsizing-recommendation option has to carry.
//!
//! A `UtilizationPreference` is "the preference to control the resource's CPU utilization
//! threshold, CPU utilization headroom, and memory utilization headroom": its `threshold` is
//! the percentile of the analysed utilization the sizing reads ("you can only specify the
//! threshold value for CPU utilization") and its `headroom` is "the headroom value in
//! percentage used for the specified metric parameter", i.e. the spare capacity an option has
//! to keep above that figure. Both are read from the value's own text rather than from a table
//! of the values the model happens to declare today ([`headroom_fraction`],
//! [`threshold_percentile`]), and a setting no stored preference states falls back to the
//! service's documented default for it ([`super::default_utilization_preferences`]) — the same
//! default a read of the preference reports, so the parameters a sizing decision applies are
//! the parameters `GetEffectiveRecommendationPreferences` says were used.
//!
//! The resolution is stated once, here, and reached through
//! [`EffectivePreferences::metric_headroom`] and
//! [`EffectivePreferences::metric_threshold_percentile`], so every reader that sizes a
//! candidate against an analysed workload applies the parameters in force rather than a
//! headroom of its own: the reader of a recommendation, its export and the projection of its
//! options' metrics cannot disagree about which candidates the workload fits on.

use super::*;

/// The `CustomizableMetricName` that customizes the `MetricName` the analysis observes as
/// `metric_name`, or `None` for a metric no utilization preference names.
///
/// `CustomizableMetricName` is `CpuUtilization | MemoryUtilization`, which are the two
/// resources an instance type sizes ([`metric_sized_by_instance_type`]) — "the preference to
/// control the resource's CPU utilization threshold, CPU utilization headroom, and memory
/// utilization headroom". Every other `MetricName` measures the workload's own traffic, which
/// no utilization preference customizes.
pub fn customizable_metric_name(metric_name: &str) -> Option<&'static str> {
    match metric_name {
        METRIC_CPU => Some(METRIC_CPU_UTILIZATION),
        METRIC_MEMORY => Some(METRIC_MEMORY_UTILIZATION),
        _ => None,
    }
}

/// The headroom fraction a `CustomizableMetricHeadroom` value names, or `None` for text that
/// is not one of its values.
///
/// The enum spells a percentage — `PERCENT_30 | PERCENT_20 | PERCENT_10 | PERCENT_0`
/// (`crate::ops::types::CUSTOMIZABLE_METRIC_HEADROOM_VALUES`) — and "the headroom value in
/// percentage used for the specified metric parameter" is that percentage, so the fraction is
/// read from the value itself and a value the model adds needs no row here.
pub fn headroom_fraction(value: &str) -> Option<f64> {
    let percent: f64 = value.strip_prefix("PERCENT_")?.parse().ok()?;
    // A headroom is spare capacity kept ON TOP of the utilization, so a figure below zero —
    // which no value of the enum spells — names no headroom.
    (percent.is_finite() && percent >= 0.0).then_some(percent / UTILIZATION_PERCENTAGE_CEILING)
}

/// The percentile a `CustomizableMetricThreshold` value names, or `None` for text that is not
/// one of its values.
///
/// The enum spells a percentile — `P90 | P95 | P99_5`
/// (`crate::ops::types::CUSTOMIZABLE_METRIC_THRESHOLD_VALUES`) — as `P` followed by the
/// percentile itself, whose fractional part the value writes with `_` because a value set
/// admits no `.`. Read from the value rather than tabulated, for the reason
/// [`headroom_fraction`] is. A percentile outside the 0..100 a percentile is measured on names
/// no point of a distribution, so it resolves to nothing.
pub fn threshold_percentile(value: &str) -> Option<f64> {
    let percentile: f64 = value.strip_prefix('P')?.replace('_', ".").parse().ok()?;
    (0.0..=UTILIZATION_PERCENTAGE_CEILING)
        .contains(&percentile)
        .then_some(percentile)
}

/// The relative slack a capacity comparison allows: the capacity a candidate has to provide is
/// a share of another capacity multiplied by a headroom, so a candidate sized EXACTLY at the
/// figure that arithmetic reached is an option whichever way its last bit fell, rather than one
/// under one representation of the same number and not under another.
pub const SIZING_TOLERANCE: f64 = 1e-9;

/// The percentile the AVERAGE of a period stands at: the middle of the distribution the
/// analysis folded its observations into.
///
/// Compute Optimizer publishes an analysed metric as its peak and its average over the
/// look-back period and no distribution beside them, so the figure at a percentile between the
/// two is this emulator's own estimate ([`utilization_at_percentile`]), anchored at the only
/// two points the analysis holds.
pub const AVERAGE_PERCENTILE: f64 = 50.0;

/// The analysed utilization at `percentile` of a distribution whose `average` and whose
/// `peak` the analysis observed.
///
/// A `threshold` names the percentile of the metric's distribution a sizing decision reads, so
/// a lower threshold reads a lower figure and sizes an option smaller. The analysis holds that
/// distribution as its average over the look-back period and the peak it reached, so a
/// percentile between them is interpolated: the average stands at [`AVERAGE_PERCENTILE`] and
/// the peak at the ceiling of the scale ([`UTILIZATION_PERCENTAGE_CEILING`]), which makes the
/// estimate monotonic in the threshold and equal to the peak at the top of the scale. A
/// percentile at or below the average's reads the average, and an observation whose average
/// exceeds its peak describes no spread, so it reads the average too.
pub fn utilization_at_percentile(average: f64, peak: f64, percentile: f64) -> f64 {
    let span = UTILIZATION_PERCENTAGE_CEILING - AVERAGE_PERCENTILE;
    let above_average = ((percentile - AVERAGE_PERCENTILE) / span).clamp(0.0, 1.0);
    average + (peak - average).max(0.0) * above_average
}

impl EffectivePreferences {
    /// The headroom a sizing decision keeps above the analysed utilization of the resource the
    /// `MetricName` `metric_name` measures, as a fraction of that utilization.
    ///
    /// The `headroom` of the utilization preference in force for the metric — "the headroom
    /// value in percentage used for the specified metric parameter" — falling back to the
    /// service's documented default for it, and to the headroom this service sizes a resource
    /// type that carries no such preference with ([`RECOMMENDATION_HEADROOM`]) for a metric no
    /// preference names at all.
    pub fn metric_headroom(&self, metric_name: &str) -> f64 {
        self.metric_setting(metric_name, |parameters| parameters.headroom.clone())
            .as_deref()
            .and_then(headroom_fraction)
            .unwrap_or(RECOMMENDATION_HEADROOM)
    }

    /// The percentile of the analysed utilization a sizing decision reads for the resource the
    /// `MetricName` `metric_name` measures.
    ///
    /// The `threshold` of the utilization preference in force for the metric — "the threshold
    /// value used for the specified metric parameter" — falling back to the service's
    /// documented default for it. A metric no threshold is in force for is sized on the PEAK
    /// the analysis observed, the top of the scale: "the threshold setting isn't available for
    /// memory utilization", so memory utilization is sized on the whole of what it reached.
    pub fn metric_threshold_percentile(&self, metric_name: &str) -> f64 {
        self.metric_setting(metric_name, |parameters| parameters.threshold.clone())
            .as_deref()
            .and_then(threshold_percentile)
            .unwrap_or(UTILIZATION_PERCENTAGE_CEILING)
    }

    /// One metric parameter of the utilization preference in force for the `MetricName`
    /// `metric_name`: what the account stored for that metric, or the service's documented
    /// default where it stored no value for it.
    fn metric_setting(
        &self,
        metric_name: &str,
        read: fn(&CustomizableMetricParameters) -> Option<String>,
    ) -> Option<String> {
        let customizable = customizable_metric_name(metric_name)?;
        let stated = |preferences: &[UtilizationPreference]| {
            preferences
                .iter()
                .filter(|preference| preference.metric_name.as_deref() == Some(customizable))
                .find_map(|preference| preference.metric_parameters.as_ref().and_then(read))
        };
        stated(&self.utilization_preferences)
            .or_else(|| stated(&default_utilization_preferences()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::{
        CUSTOMIZABLE_METRIC_HEADROOM_VALUES, CUSTOMIZABLE_METRIC_THRESHOLD_VALUES,
    };

    /// The preferences in force for a resource whose account stored `stored`.
    fn preferences(stored: Vec<UtilizationPreference>) -> EffectivePreferences {
        EffectivePreferences {
            enhanced_infrastructure_metrics: DEFAULT_ENHANCED_INFRASTRUCTURE_METRICS.to_string(),
            inferred_workload_types: INFERRED_WORKLOAD_TYPES_ACTIVE.to_string(),
            external_metrics_preference: None,
            look_back_period: DEFAULT_LOOK_BACK_PERIOD.to_string(),
            utilization_preferences: stored,
            preferred_resources: default_preferred_resources(),
            savings_estimation_mode: None,
        }
    }

    /// One stored utilization preference entry.
    fn entry(
        metric_name: &str,
        threshold: Option<&str>,
        headroom: Option<&str>,
    ) -> UtilizationPreference {
        UtilizationPreference {
            metric_name: Some(metric_name.to_string()),
            metric_parameters: Some(CustomizableMetricParameters {
                threshold: threshold.map(str::to_string),
                headroom: headroom.map(str::to_string),
            }),
        }
    }

    /// EVERY `CustomizableMetricHeadroom` the model declares resolves to the percentage its
    /// own value names, and text outside that form resolves to nothing.
    #[test]
    fn every_modelled_headroom_resolves_to_the_percentage_it_names() {
        for value in CUSTOMIZABLE_METRIC_HEADROOM_VALUES {
            let resolved = headroom_fraction(value);
            let expected = value
                .strip_prefix("PERCENT_")
                .and_then(|percent| percent.parse::<f64>().ok())
                .map(|percent| percent / 100.0);
            assert_eq!(
                resolved, expected,
                "case {value:?}: expected {expected:?}, got {resolved:?}"
            );
        }
        for value in ["", "PERCENT_", "PERCENT_X", "20", "P20", "PERCENT_20 ", "PERCENT_-10"] {
            assert_eq!(
                headroom_fraction(value),
                None,
                "case {value:?}: expected no headroom, got {:?}",
                headroom_fraction(value)
            );
        }
    }

    /// EVERY `CustomizableMetricThreshold` the model declares resolves to the percentile its
    /// own value names — the fractional part written with `_` — and text outside that form,
    /// or a percentile off the scale, resolves to nothing.
    #[test]
    fn every_modelled_threshold_resolves_to_the_percentile_it_names() {
        // The model's own value set (`CUSTOMIZABLE_METRIC_THRESHOLD_VALUES`: P90 | P95 |
        // P99_5), each with the percentile it spells.
        let expected: &[(&str, f64)] = &[("P90", 90.0), ("P95", 95.0), ("P99_5", 99.5)];
        for value in CUSTOMIZABLE_METRIC_THRESHOLD_VALUES {
            let resolved = threshold_percentile(value);
            let wanted = expected
                .iter()
                .find(|(named, _)| named == value)
                .map(|(_, percentile)| *percentile);
            assert_eq!(
                resolved, wanted,
                "case {value:?}: expected {wanted:?}, got {resolved:?}"
            );
        }
        for value in ["", "P", "90", "PERCENT_90", "P101", "P-5"] {
            assert_eq!(
                threshold_percentile(value),
                None,
                "case {value:?}: expected no percentile, got {:?}",
                threshold_percentile(value)
            );
        }
    }

    /// With nothing stored, the parameters in force are the ones the service documents as its
    /// defaults: `P99_5` for the CPU threshold, `PERCENT_20` for both headrooms, and the peak
    /// for memory, whose threshold is not settable.
    #[test]
    fn the_documented_defaults_are_in_force_when_nothing_is_stored() {
        let preferences = preferences(Vec::new());
        // (case, metric, expected headroom, expected percentile)
        let cases: [(&str, &str, f64, f64); 2] = [
            ("cpu", METRIC_CPU, 0.20, 99.5),
            ("memory", METRIC_MEMORY, 0.20, UTILIZATION_PERCENTAGE_CEILING),
        ];
        for (case, metric, headroom, percentile) in cases {
            let in_force = preferences.metric_headroom(metric);
            assert_eq!(
                in_force, headroom,
                "case {case:?}: expected a headroom of {headroom}, got {in_force}"
            );
            let read = preferences.metric_threshold_percentile(metric);
            assert_eq!(
                read, percentile,
                "case {case:?}: expected percentile {percentile}, got {read}"
            );
        }
    }

    /// The parameters a metric's sizing is decided by: the headroom as a fraction, and the
    /// percentile of the analysed utilization the sizing reads.
    type Parameters = (f64, f64);

    /// One row of the table below: the case, the entries the account stored, the parameters in
    /// force for CPU utilization and the ones in force for memory utilization.
    type StoredRow<'a> = (&'a str, Vec<UtilizationPreference>, Parameters, Parameters);

    /// A stored entry's parameters are the ones in force for ITS metric, and leave the other
    /// metric on the service's default — including an entry that states one parameter and not
    /// the other.
    #[test]
    fn a_stored_entry_sets_the_parameters_of_its_own_metric() {
        let cases: [StoredRow; 4] = [
            (
                "a cpu headroom only",
                vec![entry(METRIC_CPU_UTILIZATION, None, Some("PERCENT_0"))],
                (0.0, 99.5),
                (0.20, UTILIZATION_PERCENTAGE_CEILING),
            ),
            (
                "a cpu threshold only",
                vec![entry(METRIC_CPU_UTILIZATION, Some("P90"), None)],
                (0.20, 90.0),
                (0.20, UTILIZATION_PERCENTAGE_CEILING),
            ),
            (
                "a memory headroom only",
                vec![entry(METRIC_MEMORY_UTILIZATION, None, Some("PERCENT_30"))],
                (0.20, 99.5),
                (0.30, UTILIZATION_PERCENTAGE_CEILING),
            ),
            (
                "both metrics",
                vec![
                    entry(METRIC_CPU_UTILIZATION, Some("P95"), Some("PERCENT_30")),
                    entry(METRIC_MEMORY_UTILIZATION, None, Some("PERCENT_10")),
                ],
                (0.30, 95.0),
                (0.10, UTILIZATION_PERCENTAGE_CEILING),
            ),
        ];
        for (case, stored, cpu, memory) in cases {
            let preferences = preferences(stored);
            for (metric, (headroom, percentile)) in [(METRIC_CPU, cpu), (METRIC_MEMORY, memory)] {
                let in_force = preferences.metric_headroom(metric);
                assert_eq!(
                    in_force, headroom,
                    "case {case:?}: expected a {metric} headroom of {headroom}, got {in_force}"
                );
                let read = preferences.metric_threshold_percentile(metric);
                assert_eq!(
                    read, percentile,
                    "case {case:?}: expected {metric} percentile {percentile}, got {read}"
                );
            }
        }
    }

    /// A metric no utilization preference names — one measuring the workload's own traffic
    /// rather than a resource the instance type sizes — is sized on the peak with this
    /// service's own headroom.
    #[test]
    fn a_metric_no_preference_names_keeps_the_services_own_sizing() {
        let preferences = preferences(vec![entry(
            METRIC_CPU_UTILIZATION,
            Some("P90"),
            Some("PERCENT_0"),
        )]);
        let metric = "NETWORK_IN_BYTES_PER_SECOND";
        assert_eq!(
            preferences.metric_headroom(metric),
            RECOMMENDATION_HEADROOM,
            "expected the service's own headroom for {metric}, got {}",
            preferences.metric_headroom(metric)
        );
        assert_eq!(
            preferences.metric_threshold_percentile(metric),
            UTILIZATION_PERCENTAGE_CEILING,
            "expected {metric} to be read at its peak, got {}",
            preferences.metric_threshold_percentile(metric)
        );
    }

    /// The estimate at a percentile rises with the percentile, reads the average at and below
    /// the average's own percentile, and reads the peak at the top of the scale.
    #[test]
    fn the_estimate_rises_from_the_average_to_the_peak() {
        // (case, average, peak)
        let observations: [(&str, f64, f64); 4] = [
            ("a spread observation", 20.0, 60.0),
            ("a flat observation", 40.0, 40.0),
            ("an average above its peak", 70.0, 50.0),
            ("an idle observation", 0.0, 0.0),
        ];
        for (case, average, peak) in observations {
            assert_eq!(
                utilization_at_percentile(average, peak, UTILIZATION_PERCENTAGE_CEILING),
                peak.max(average),
                "case {case:?}: expected the peak at the top of the scale, got {}",
                utilization_at_percentile(average, peak, UTILIZATION_PERCENTAGE_CEILING)
            );
            assert_eq!(
                utilization_at_percentile(average, peak, AVERAGE_PERCENTILE),
                average,
                "case {case:?}: expected the average at its own percentile, got {}",
                utilization_at_percentile(average, peak, AVERAGE_PERCENTILE)
            );
            let mut previous = f64::NEG_INFINITY;
            for percentile in [0.0, 25.0, AVERAGE_PERCENTILE, 90.0, 95.0, 99.5, 100.0] {
                let estimate = utilization_at_percentile(average, peak, percentile);
                assert!(
                    estimate >= previous,
                    "case {case:?}: expected percentile {percentile} to read at least \
                     {previous}, got {estimate}"
                );
                assert!(
                    estimate >= average.min(peak) && estimate <= average.max(peak),
                    "case {case:?}: expected percentile {percentile} between {average} and \
                     {peak}, got {estimate}"
                );
                previous = estimate;
            }
        }
    }
}
