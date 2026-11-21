// SPDX-License-Identifier: CC0-1.0

//! The SERVER-SIDE RUN of an accepted hyperparameter tuning job: the training jobs it
//! LAUNCHES, the hyperparameter values each of them is given, the objective metric each one
//! is evaluated by, and which of them is the best.
//!
//! # What a tuning job DOES
//!
//! "Hyperparameter tuning uses the value of this metric to evaluate the training jobs it
//! launches, and returns the training job that results in either the highest or lowest value
//! for this metric" (`HyperParameterTuningJobObjective`), bounded by
//! `ResourceLimits$MaxNumberOfTrainingJobs` — "the maximum number of training jobs that a
//! hyperparameter tuning job can launch". So the job's own work is: launch training jobs over
//! the ranges `ParameterRanges` declares, evaluate each one's objective metric, and report the
//! best of them as `BestTrainingJob`.
//!
//! This module performs that work with the SAME training runs a caller's own
//! `CreateTrainingJob` gets ([`super::run_training_job`]): each child is a real training job
//! in this service's store — `DescribeTrainingJob` answers for it, it carries the
//! `TuningJobArn` of the tuning job that launched it, and its own run reads the definition's
//! channels out of Amazon S3, fits a model on them and writes the model artifact under the
//! definition's `OutputDataConfig`. So `BestTrainingJob` names a training job that exists and
//! an artifact that was written, which is what lets a client deploy it.
//!
//! # Which values each child is given
//!
//! The strategy "specifies how hyperparameter tuning chooses the combinations of
//! hyperparameter values to use for the training job it launches"
//! (`HyperParameterTuningJobConfig$Strategy`); it does not fix which point of a range any one
//! job takes. This module samples each declared range at EVENLY SPACED points across the jobs
//! it launches — a single job takes the middle of each range — so every child is a distinct
//! point of the declared search space and the values are the caller's own bounds rather than
//! anything invented. A categorical range is sampled by walking its own values. The static
//! hyperparameters the definition declares are given to every child, as
//! `StaticHyperParameters` documents ("the values of hyperparameters that do not change for
//! the tuning job").
//!
//! # How a child's objective metric is evaluated
//!
//! The objective names the metric the ALGORITHM emits for the run
//! (`HyperParameterTuningJobObjective$MetricName`), and what a run here emits is the error it
//! MEASURED over each channel it read, named for that channel ([`super::run_training_job`]).
//! So a child's objective value is that measurement, taken over the channel the objective
//! metric names when its name names one (`<channel>:<metric>`, the form SageMaker's built-in
//! algorithms emit) and otherwise over the channel the model was fitted on. A child whose run
//! measured nothing at all evaluated no objective metric, which is the `Failed`
//! `ObjectiveStatus` the model carries for exactly that case, and it is not a candidate for
//! the best job.
//!
//! `HyperParameterTuningJobObjective$Type` is "whether to minimize or maximize the objective
//! metric", so the best child is the one with the smallest value under `Minimize` and the
//! largest under `Maximize`.

use super::*;

use crate::ops::types::{
    AlgorithmSpecification, CreateTrainingJobInput, FinalHyperParameterTuningJobObjectiveMetric,
    HyperParameterTrainingJobDefinition, HyperParameterTrainingJobSummary,
    HyperParameterTuningJobObjective, MetricData, ParameterRanges,
};

/// The most training jobs ONE create launches, across every training job definition the
/// request carries, whatever `ResourceLimits$MaxNumberOfTrainingJobs` asks for.
///
/// An EMULATOR ceiling, not an AWS limit: each child is a real training run that reads its
/// channels and fits a model, and is a stored record besides, so the TOTAL work one create
/// performs is bounded here rather than left to the two factors a request chooses.
pub const MAX_LAUNCHED_TRAINING_JOBS: usize = 4;

/// `ObjectiveStatus`'s member for a child whose "final objective metric for the training job
/// was evaluated by the hyperparameter tuning job and used in the hyperparameter tuning
/// process".
///
/// Named for the tuning job's own enum: an AutoML candidate's objective status is a different
/// member of a different shape ([`super::auto_ml_run::OBJECTIVE_STATUS_SUCCEEDED`]), so the two
/// are not one constant.
pub const TUNING_OBJECTIVE_STATUS_SUCCEEDED: &str = "Succeeded";

/// `ObjectiveStatus`'s member for a child whose final objective metric was NOT evaluated, and
/// so was not used in the tuning process.
pub const TUNING_OBJECTIVE_STATUS_FAILED: &str = "Failed";

/// `HyperParameterTuningJobObjectiveType`'s member asking for the SMALLEST value of the
/// objective metric.
pub const TUNING_OBJECTIVE_MINIMIZE: &str = "Minimize";

/// `HyperParameterTuningJobObjectiveType`'s member asking for the LARGEST value of the
/// objective metric.
pub const TUNING_OBJECTIVE_MAXIMIZE: &str = "Maximize";

/// What separates the channel a built-in algorithm's metric was measured over from the metric
/// itself, in the metric names such an algorithm emits (`validation:rmse`).
const METRIC_CHANNEL_SEPARATOR: char = ':';

/// What a tuning job's run produced: one summary per training job it launched, and which of
/// them the objective picked.
#[derive(Debug, Clone)]
pub struct TuningRunResult {
    /// One `HyperParameterTrainingJobSummary` per launched training job, in launch order.
    pub children: Vec<HyperParameterTrainingJobSummary>,
    /// Which of [`Self::children`] is `BestTrainingJob`, and `None` when no child evaluated
    /// the objective metric at all.
    pub best: Option<usize>,
    /// The instant the last child's work is over, which is what the tuning job's own terminal
    /// status cannot fall due before.
    pub children_end: SystemTime,
}

impl TuningRunResult {
    /// How many children reached each `TrainingJobStatusCounters` member.
    #[must_use]
    pub fn completed(&self) -> i32 {
        self.counted(TRAINING_STATUS_COMPLETED)
    }

    /// See [`Self::completed`].
    #[must_use]
    pub fn failed(&self) -> i32 {
        self.counted(TRAINING_STATUS_FAILED)
    }

    /// See [`Self::completed`].
    #[must_use]
    pub fn stopped(&self) -> i32 {
        self.counted(TRAINING_STATUS_STOPPED)
    }

    /// How many children evaluated the objective metric, which is
    /// `ObjectiveStatusCounters$Succeeded`.
    #[must_use]
    pub fn objective_succeeded(&self) -> i32 {
        i32::try_from(
            self.children
                .iter()
                .filter(|child| {
                    child.objective_status.as_deref() == Some(TUNING_OBJECTIVE_STATUS_SUCCEEDED)
                })
                .count(),
        )
        .unwrap_or(i32::MAX)
    }

    /// How many children did not, which is `ObjectiveStatusCounters$Failed`.
    #[must_use]
    pub fn objective_failed(&self) -> i32 {
        i32::try_from(self.children.len())
            .unwrap_or(i32::MAX)
            .saturating_sub(self.objective_succeeded())
    }

    /// `BestTrainingJob`: the child the objective picked, when one did.
    #[must_use]
    pub fn best_training_job(&self) -> Option<HyperParameterTrainingJobSummary> {
        self.children.get(self.best?).cloned()
    }

    /// Why the tuning job failed, when every child it launched failed: the reason the first of
    /// them reported, because the tuning job's work IS those training jobs.
    #[must_use]
    pub fn failure_reason(&self) -> Option<String> {
        if self.completed() != 0 {
            return None;
        }
        self.children
            .iter()
            .find_map(|child| child.failure_reason.clone())
    }

    fn counted(&self, status: &str) -> i32 {
        i32::try_from(
            self.children
                .iter()
                .filter(|child| child.training_job_status == status)
                .count(),
        )
        .unwrap_or(i32::MAX)
    }
}

/// One training job a tuning job is about to launch: the request it is created with and the
/// hyperparameter values the search picked for it.
#[derive(Debug, Clone)]
pub struct LaunchedTrainingJob {
    /// The `CreateTrainingJobInput` the child is stored and run from.
    pub request: CreateTrainingJobInput,
    /// `HyperParameterTrainingJobSummary$TrainingJobDefinitionName`, when the definition
    /// carried one.
    pub definition_name: Option<String>,
    /// "A list of the hyperparameters for which you specified ranges to search"
    /// (`TunedHyperParameters`): the values the search picked, without the static ones.
    pub tuned: HashMap<String, String>,
    /// The objective this child is evaluated by, which is the definition's own when it carries
    /// one and the tuning configuration's otherwise.
    pub objective: Option<HyperParameterTuningJobObjective>,
}

/// Every training job the tuning job `tuning_name` launches, in launch order.
///
/// The definitions are the request's: `TrainingJobDefinition` when it carries one, and every
/// entry of `TrainingJobDefinitions` otherwise — the two ways one request declares what its
/// children run. How many children each definition asks for is
/// `ResourceLimits$MaxNumberOfTrainingJobs`, and never fewer than one: a tuning job that
/// launched nothing would have nothing to pick a best job from.
///
/// [`MAX_LAUNCHED_TRAINING_JOBS`] bounds the TOTAL this one create performs — across every
/// definition the request carries, not per definition — because each child is a real training
/// run and a stored record: the request chooses neither factor of that product.
#[must_use]
pub fn launched_training_jobs(
    tuning_name: &str,
    config: &crate::ops::types::HyperParameterTuningJobConfig,
    definitions: &[&HyperParameterTrainingJobDefinition],
) -> Vec<LaunchedTrainingJob> {
    let jobs = config
        .resource_limits
        .max_number_of_training_jobs
        .and_then(|asked| usize::try_from(asked).ok())
        .unwrap_or(1)
        .clamp(1, MAX_LAUNCHED_TRAINING_JOBS);
    let mut launched: Vec<LaunchedTrainingJob> = Vec::new();
    for definition in definitions {
        for index in 0..jobs {
            // The total, not the per-definition count: once this create has laid out as many
            // children as it may perform, no definition adds another.
            if launched.len() >= MAX_LAUNCHED_TRAINING_JOBS {
                return launched;
            }
            let name = child_training_job_name(tuning_name, launched.len());
            let ranges = definition
                .hyper_parameter_ranges
                .as_ref()
                .or(config.parameter_ranges.as_ref());
            let tuned = sampled_hyper_parameters(ranges, index, jobs);
            let mut hyper_parameters: HashMap<String, String> =
                definition.static_hyper_parameters.clone().unwrap_or_default();
            hyper_parameters.extend(tuned.iter().map(|(name, value)| (name.clone(), value.clone())));
            launched.push(LaunchedTrainingJob {
                request: child_request(&name, definition, hyper_parameters),
                definition_name: definition.definition_name.clone(),
                tuned,
                objective: definition
                    .tuning_objective
                    .clone()
                    .or_else(|| config.hyper_parameter_tuning_job_objective.clone()),
            });
        }
    }
    launched
}

/// The name one launched training job is created under: the tuning job's own name, the child's
/// position in the launch order and an opaque suffix, which is the form SageMaker names the
/// training jobs a tuning job launches in.
///
/// `HyperParameterTuningJobName` is modelled `len 1..32` and `TrainingJobName` `len 1..63`, so
/// the composed name is inside the name a training job may carry.
#[must_use]
pub fn child_training_job_name(tuning_name: &str, index: usize) -> String {
    let suffix = mint_opaque_resource_id();
    let suffix = suffix.get(..8).unwrap_or(&suffix);
    format!("{tuning_name}-{:03}-{suffix}", index + 1)
}

/// The `CreateTrainingJobInput` one child is created and run from: the definition's own
/// members under the names `CreateTrainingJob` models them with, plus the hyperparameters the
/// search gave this child.
///
/// Every member of a `HyperParameterTrainingJobDefinition` is a `CreateTrainingJob` member of
/// the same name, so the child is the definition rather than a reconstruction of it. The
/// definition's `HyperParameterTuningResourceConfig` — the instance-flexible spelling of the
/// same compute — is read as the `ResourceConfig` the child runs on when the definition
/// declares no `ResourceConfig` of its own.
fn child_request(
    name: &str,
    definition: &HyperParameterTrainingJobDefinition,
    hyper_parameters: HashMap<String, String>,
) -> CreateTrainingJobInput {
    let specification = &definition.algorithm_specification;
    CreateTrainingJobInput {
        training_job_name: name.to_string(),
        hyper_parameters: Some(hyper_parameters),
        algorithm_specification: Some(AlgorithmSpecification {
            training_image: specification.training_image.clone(),
            algorithm_name: specification.algorithm_name.clone(),
            training_input_mode: specification.training_input_mode.clone(),
            // The definition's own metric set when it declares one, and otherwise the set the
            // built-in algorithm its image names publishes — the same resolution a caller's
            // own `CreateTrainingJob` gets, because the child IS a training job of that
            // algorithm (`crate::state::builtin_algorithm_metrics`).
            metric_definitions: specification.metric_definitions.clone().or_else(|| {
                specification
                    .training_image
                    .as_deref()
                    .and_then(super::builtin_image_metric_definitions)
            }),
            ..AlgorithmSpecification::default()
        }),
        role_arn: definition.role_arn.clone(),
        input_data_config: definition.input_data_config.clone(),
        output_data_config: definition.output_data_config.clone(),
        resource_config: definition
            .resource_config
            .clone()
            .or_else(|| tuning_resource_config(definition)),
        vpc_config: definition.vpc_config.clone(),
        stopping_condition: Some(definition.stopping_condition.clone()),
        enable_network_isolation: definition.enable_network_isolation,
        enable_inter_container_traffic_encryption: definition
            .enable_inter_container_traffic_encryption,
        enable_managed_spot_training: definition.enable_managed_spot_training,
        checkpoint_config: definition.checkpoint_config.clone(),
        environment: definition.environment.clone(),
        retry_strategy: definition.retry_strategy.clone(),
        ..CreateTrainingJobInput::default()
    }
}

/// The compute a definition's `HyperParameterTuningResourceConfig` asks for, as the
/// `ResourceConfig` a training job runs on: "the configuration for the hyperparameter tuning
/// resources, including the compute instances and storage volumes, used for training jobs
/// launched by the tuning job", whose instance type and count are the same members.
fn tuning_resource_config(
    definition: &HyperParameterTrainingJobDefinition,
) -> Option<crate::ops::types::ResourceConfig> {
    let asked = definition.hyper_parameter_tuning_resource_config.as_ref()?;
    Some(crate::ops::types::ResourceConfig {
        instance_type: asked.instance_type.clone(),
        instance_count: asked.instance_count,
        volume_size_in_gb: asked.volume_size_in_gb,
        volume_kms_key_id: asked.volume_kms_key_id.clone(),
        ..crate::ops::types::ResourceConfig::default()
    })
}

/// The hyperparameter values the search gives child `index` of `jobs`: each declared range
/// sampled at its own evenly spaced point, as the module docs state.
///
/// Every value is computed with saturating arithmetic and clamped into the range it came from,
/// and a range whose bounds are not a searchable interval — unparseable, not finite, or a
/// minimum past its maximum — contributes nothing: the bounds are a caller's own text, so no
/// magnitude a request can state reaches an unchecked operation here.
#[must_use]
pub fn sampled_hyper_parameters(
    ranges: Option<&ParameterRanges>,
    index: usize,
    jobs: usize,
) -> HashMap<String, String> {
    let mut sampled: HashMap<String, String> = HashMap::new();
    let Some(ranges) = ranges else {
        return sampled;
    };
    // Where in the range this child sits: evenly spaced across the jobs the tuning job runs,
    // and the middle of the range for a tuning job that runs a single one.
    let fraction = if jobs <= 1 {
        0.5
    } else {
        index as f64 / (jobs - 1) as f64
    };
    for range in ranges.integer_parameter_ranges.iter().flatten() {
        let (Ok(low), Ok(high)) = (
            range.min_value.trim().parse::<i64>(),
            range.max_value.trim().parse::<i64>(),
        ) else {
            continue;
        };
        // A range whose minimum exceeds its maximum searches nothing, so nothing is sampled
        // from it; the arithmetic below is saturating besides, so no bound a caller may write
        // can carry the sampled point outside the range it names.
        if low > high {
            continue;
        }
        let span = high.saturating_sub(low) as f64;
        let offset = (span * fraction).round();
        let value = low
            .saturating_add(if offset.is_finite() { offset as i64 } else { 0 })
            .clamp(low, high);
        sampled.insert(range.name.clone(), value.to_string());
    }
    for range in ranges.continuous_parameter_ranges.iter().flatten() {
        let (Ok(low), Ok(high)) = (
            range.min_value.trim().parse::<f64>(),
            range.max_value.trim().parse::<f64>(),
        ) else {
            continue;
        };
        if !low.is_finite() || !high.is_finite() || low > high {
            continue;
        }
        let value = (low + (high - low) * fraction).clamp(low, high);
        sampled.insert(range.name.clone(), value.to_string());
    }
    for range in ranges.categorical_parameter_ranges.iter().flatten() {
        if range.values.is_empty() {
            continue;
        }
        let value = &range.values[index % range.values.len()];
        sampled.insert(range.name.clone(), value.clone());
    }
    sampled
}

/// The objective metric value a child's run evaluated, and `None` when its run measured
/// nothing the objective could be read from.
///
/// The reading the module docs state: the measurement named exactly as the objective names it
/// when the run emitted one, then the measurement over the CHANNEL the objective's name names,
/// and otherwise the measurement over the channel the model was fitted on — which is the first
/// one the run reports.
#[must_use]
pub fn objective_metric_value(metrics: &[MetricData], metric_name: &str) -> Option<f32> {
    let named = |wanted: &str| {
        metrics
            .iter()
            .find(|metric| metric.metric_name.as_deref() == Some(wanted))
            .and_then(|metric| metric.value)
    };
    if let Some(value) = named(metric_name) {
        return Some(value);
    }
    if let Some((channel, _)) = metric_name.split_once(METRIC_CHANNEL_SEPARATOR) {
        let over_channel = metrics
            .iter()
            .find(|metric| {
                metric.metric_name.as_deref().is_some_and(|name| {
                    name.split_once(METRIC_CHANNEL_SEPARATOR)
                        .is_some_and(|(measured, _)| measured == channel)
                })
            })
            .and_then(|metric| metric.value);
        if let Some(value) = over_channel {
            return Some(value);
        }
    }
    metrics.first().and_then(|metric| metric.value)
}

/// The summary of one launched child, as `BestTrainingJob` and the tuning job's counters read
/// it: what the child's own run produced, which is also what `DescribeTrainingJob` reports for
/// it once the clock has settled that job.
#[must_use]
pub fn child_summary(
    launched: &LaunchedTrainingJob,
    tuning_name: &str,
    arn: &str,
    created: SystemTime,
    ends_at: SystemTime,
    outcome: &TrainingRunOutcome,
) -> HyperParameterTrainingJobSummary {
    let objective = launched.objective.as_ref();
    let (status, failure_reason, measured) = match outcome {
        TrainingRunOutcome::Completed { final_metrics, .. } => (
            TRAINING_STATUS_COMPLETED,
            None,
            objective.and_then(|objective| {
                objective_metric_value(final_metrics, &objective.metric_name)
            }),
        ),
        TrainingRunOutcome::Failed { reason, .. } => {
            (TRAINING_STATUS_FAILED, Some(reason.clone()), None)
        }
    };
    HyperParameterTrainingJobSummary {
        training_job_definition_name: launched.definition_name.clone(),
        training_job_name: launched.request.training_job_name.clone(),
        training_job_arn: arn.to_string(),
        tuning_job_name: Some(tuning_name.to_string()),
        creation_time: timestamp(created),
        training_start_time: Some(timestamp(created)),
        training_end_time: Some(timestamp(ends_at)),
        training_job_status: status.to_string(),
        tuned_hyper_parameters: launched.tuned.clone(),
        failure_reason,
        final_hyper_parameter_tuning_job_objective_metric: measured.and_then(|value| {
            objective.map(|objective| FinalHyperParameterTuningJobObjectiveMetric {
                r#type: Some(objective.r#type.clone()),
                metric_name: objective.metric_name.clone(),
                value,
            })
        }),
        objective_status: Some(if measured.is_some() {
            TUNING_OBJECTIVE_STATUS_SUCCEEDED.to_string()
        } else {
            TUNING_OBJECTIVE_STATUS_FAILED.to_string()
        }),
    }
}

/// Which child the objective picks as `BestTrainingJob`: the smallest objective value under
/// `Minimize` and the largest under `Maximize`, over the children that evaluated one at all.
#[must_use]
pub fn best_child(children: &[HyperParameterTrainingJobSummary]) -> Option<usize> {
    let ranked: Vec<(usize, f32, bool)> = children
        .iter()
        .enumerate()
        .filter_map(|(index, child)| {
            let metric = child
                .final_hyper_parameter_tuning_job_objective_metric
                .as_ref()?;
            let minimize = metric.r#type.as_deref() != Some(TUNING_OBJECTIVE_MAXIMIZE);
            Some((index, metric.value, minimize))
        })
        .collect();
    let best = ranked.iter().copied().reduce(|best, candidate| {
        let better = if candidate.2 {
            candidate.1 < best.1
        } else {
            candidate.1 > best.1
        };
        if better {
            candidate
        } else {
            best
        }
    })?;
    Some(best.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A metric the run measured over `channel`.
    fn measured(channel: &str, value: f32) -> MetricData {
        MetricData {
            metric_name: Some(format!("{channel}:rmse")),
            value: Some(value),
            timestamp: None,
        }
    }

    /// RULE: a child's objective value is the measurement the objective names — by its exact
    /// name, then over the channel its name names, and otherwise the measurement over the
    /// channel the model was fitted on.
    ///
    /// The rows differ along the FORM of the objective metric name against the measurements a
    /// run reported.
    #[test]
    fn the_objective_reads_the_measurement_it_names() {
        let metrics = vec![measured("train", 2.0), measured("validation", 1.0)];
        // (case, objective metric name, expected value)
        let rows: [(&str, &str, Option<f32>); 4] = [
            ("the exact name the run emitted", "validation:rmse", Some(1.0)),
            ("another metric of the same channel", "validation:mse", Some(1.0)),
            ("the fitted channel's own metric", "train:rmse", Some(2.0)),
            ("a name naming no channel at all", "objective_loss", Some(2.0)),
        ];
        for (case, metric_name, expected) in rows {
            let value = objective_metric_value(&metrics, metric_name);
            assert_eq!(
                value, expected,
                "case {case:?}: expected {expected:?}, got {value:?}"
            );
        }
        assert_eq!(
            objective_metric_value(&[], "validation:rmse"),
            None,
            "a run that measured nothing evaluates no objective metric"
        );
    }

    /// RULE: the search samples each declared range at evenly spaced points across the jobs
    /// the tuning job launches, and a single job takes the middle of each range.
    #[test]
    fn the_search_samples_the_declared_ranges() {
        let ranges = ParameterRanges {
            integer_parameter_ranges: Some(vec![crate::ops::types::IntegerParameterRange {
                name: "max_depth".to_string(),
                min_value: "2".to_string(),
                max_value: "10".to_string(),
                scaling_type: None,
            }]),
            continuous_parameter_ranges: Some(vec![crate::ops::types::ContinuousParameterRange {
                name: "eta".to_string(),
                min_value: "0".to_string(),
                max_value: "1".to_string(),
                scaling_type: None,
            }]),
            categorical_parameter_ranges: Some(vec![
                crate::ops::types::CategoricalParameterRange {
                    name: "objective".to_string(),
                    values: vec!["reg:squarederror".to_string(), "reg:logistic".to_string()],
                },
            ]),
            auto_parameters: None,
        };

        // (case, index, jobs, expected integer, expected continuous, expected categorical)
        let rows: [(&str, usize, usize, &str, f64, &str); 4] = [
            ("one job takes the middle", 0, 1, "6", 0.5, "reg:squarederror"),
            ("the first of three", 0, 3, "2", 0.0, "reg:squarederror"),
            ("the middle of three", 1, 3, "6", 0.5, "reg:logistic"),
            ("the last of three", 2, 3, "10", 1.0, "reg:squarederror"),
        ];
        for (case, index, jobs, integer, continuous, categorical) in rows {
            let sampled = sampled_hyper_parameters(Some(&ranges), index, jobs);
            assert_eq!(
                sampled.get("max_depth").map(String::as_str),
                Some(integer),
                "case {case:?}: expected max_depth {integer}, got {sampled:?}"
            );
            let eta: f64 = sampled
                .get("eta")
                .and_then(|value| value.parse().ok())
                .unwrap_or(f64::NAN);
            assert!(
                (eta - continuous).abs() < 1e-9,
                "case {case:?}: expected eta {continuous}, got {sampled:?}"
            );
            assert_eq!(
                sampled.get("objective").map(String::as_str),
                Some(categorical),
                "case {case:?}: expected objective {categorical}, got {sampled:?}"
            );
        }
        assert!(
            sampled_hyper_parameters(None, 0, 1).is_empty(),
            "a job searching no range is given no tuned hyperparameter"
        );
    }

    /// RULE: a range whose bounds are not a searchable interval contributes no value, whatever
    /// magnitudes it states — so no request-stated bound reaches an unchecked operation.
    ///
    /// The rows span the forms a caller may write: the extremes of the integer domain, an
    /// inverted range at those extremes, bounds that are not numbers, and a non-finite
    /// continuous bound.
    #[test]
    fn a_range_that_is_no_interval_is_sampled_at_nothing() {
        // (case, minimum, maximum, whether the range is searched at all)
        let rows: [(&str, &str, &str, bool); 6] = [
            ("the whole integer domain", "-9223372036854775808", "9223372036854775807", true),
            ("inverted at the domain's extremes", "9223372036854775807", "-9223372036854775808", false),
            ("inverted by one", "10", "9", false),
            ("a single point", "7", "7", true),
            ("bounds that are not integers", "low", "high", false),
            ("a bound past the integer domain", "0", "99999999999999999999", false),
        ];
        for (case, min_value, max_value, searched) in rows {
            let ranges = ParameterRanges {
                integer_parameter_ranges: Some(vec![crate::ops::types::IntegerParameterRange {
                    name: "depth".to_string(),
                    min_value: min_value.to_string(),
                    max_value: max_value.to_string(),
                    scaling_type: None,
                }]),
                ..ParameterRanges::default()
            };
            for (index, jobs) in [(0, 1), (0, 3), (2, 3)] {
                let sampled = sampled_hyper_parameters(Some(&ranges), index, jobs);
                let value = sampled.get("depth").cloned();
                assert_eq!(
                    value.is_some(),
                    searched,
                    "case {case:?} (child {index} of {jobs}): expected the range to be \
                     {}, got {sampled:?}",
                    if searched { "searched" } else { "skipped" }
                );
                if let (Some(value), Ok(low), Ok(high)) = (
                    value,
                    min_value.parse::<i64>(),
                    max_value.parse::<i64>(),
                ) {
                    let picked: i64 = value.parse().expect("an integer hyperparameter");
                    assert!(
                        (low..=high).contains(&picked),
                        "case {case:?}: the sampled point stays inside the range, got {picked}"
                    );
                }
            }
        }

        let continuous = ParameterRanges {
            continuous_parameter_ranges: Some(vec![crate::ops::types::ContinuousParameterRange {
                name: "eta".to_string(),
                min_value: "NaN".to_string(),
                max_value: "1".to_string(),
                scaling_type: None,
            }]),
            ..ParameterRanges::default()
        };
        assert!(
            sampled_hyper_parameters(Some(&continuous), 0, 2).is_empty(),
            "a continuous range whose bound is not finite is searched at nothing"
        );
    }

    /// RULE: the best child is the one with the smallest objective value under `Minimize` and
    /// the largest under `Maximize`, and a set of children that evaluated none has no best.
    #[test]
    fn the_objective_direction_picks_the_best_child() {
        let summary = |name: &str, value: Option<f32>, direction: &str| {
            HyperParameterTrainingJobSummary {
                training_job_definition_name: None,
                training_job_name: name.to_string(),
                training_job_arn: format!("arn:aws:sagemaker:us-east-1:000000000000:training-job/{name}"),
                tuning_job_name: Some("tune".to_string()),
                creation_time: timestamp(SystemTime::UNIX_EPOCH),
                training_start_time: None,
                training_end_time: None,
                training_job_status: TRAINING_STATUS_COMPLETED.to_string(),
                tuned_hyper_parameters: HashMap::new(),
                failure_reason: None,
                final_hyper_parameter_tuning_job_objective_metric: value.map(|value| {
                    FinalHyperParameterTuningJobObjectiveMetric {
                        r#type: Some(direction.to_string()),
                        metric_name: "validation:rmse".to_string(),
                        value,
                    }
                }),
                objective_status: None,
            }
        };

        for (case, direction, expected) in [
            ("minimize picks the smallest", TUNING_OBJECTIVE_MINIMIZE, Some(1)),
            ("maximize picks the largest", TUNING_OBJECTIVE_MAXIMIZE, Some(2)),
        ] {
            let children = vec![
                summary("a", Some(2.0), direction),
                summary("b", Some(0.5), direction),
                summary("c", Some(3.0), direction),
            ];
            assert_eq!(
                best_child(&children),
                expected,
                "case {case:?}: got {:?}",
                best_child(&children)
            );
        }
        let none = vec![summary("a", None, TUNING_OBJECTIVE_MINIMIZE)];
        assert_eq!(
            best_child(&none),
            None,
            "children that evaluated no objective metric have no best job"
        );
    }
}
