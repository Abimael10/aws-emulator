// SPDX-License-Identifier: CC0-1.0

//! The ALGORITHM an accepted training job runs, as far as its OUTPUTS are observable: the file
//! its container writes the trained model into, and the metric it emits per input channel.
//!
//! # Why this reading exists
//!
//! Two of the things `DescribeTrainingJob` reports about a finished job are the ALGORITHM's,
//! not SageMaker's:
//!
//! * the model artifact is whatever the container left in `/opt/ml/model` — "a SageMaker
//!   container stores your trained model artifacts in the /opt/ml/model directory. After
//!   training has completed, by default, these artifacts are uploaded to your Amazon S3 bucket
//!   as compressed files" (`ModelArtifacts$S3ModelArtifacts`) — so the NAME of the file inside
//!   `model.tar.gz` is the one that algorithm's container writes;
//! * `FinalMetricDataList` is "the name, value, and date and time of a metric that was emitted
//!   to Amazon CloudWatch" (`MetricData`), i.e. the metrics the training container really
//!   emitted, named per the algorithm's own metric definitions and per input channel.
//!
//! So a job that names a BUILT-IN algorithm is reported with that algorithm's own names, and
//! one that names a container of the caller's own is reported with the names of what this
//! emulator's default train instructions measured. This module states which of the two a job's
//! `AlgorithmSpecification` names and what that choice implies; the measuring itself is the
//! run's ([`super::training_run`]).
//!
//! # The built-in algorithm this reading knows
//!
//! SageMaker publishes each built-in algorithm as an image whose repository is
//! `sagemaker-<algorithm>` ("Docker registry paths for SageMaker built-in algorithms"), so the
//! algorithm a job runs is read off the repository the image path names rather than guessed
//! from anything else. The one built-in algorithm named here is the gradient-boosted tree
//! algorithm, which is also the one whose serialized model this emulator can SERVE
//! ([`super::boosted_model`]), so the pair stays consistent: it writes its model as
//! `xgboost-model` and emits `<channel>:<eval_metric>` per channel.
//!
//! Which metric that is comes from the learning task the job asked for: XGBoost's
//! `eval_metric` parameter is documented as defaulting "according to objective", and a request
//! may also name it outright. Both are hyperparameters of the job, so both are read from the
//! `HyperParameters` the request carried.

use super::*;

/// The directory a training container writes its model artifacts into
/// (`ModelArtifacts$S3ModelArtifacts`: "a SageMaker container stores your trained model
/// artifacts in the /opt/ml/model directory"), which is what the archive uploaded for the job
/// holds.
pub const MODEL_DIRECTORY: &str = "/opt/ml/model";

/// Where a job's container writes its checkpoints when `CheckpointConfig` names no local path:
/// "(Optional) The local directory where checkpoints are written. The default directory is
/// /opt/ml/checkpoints/" (`CheckpointConfig$LocalPath`).
pub const DEFAULT_CHECKPOINT_DIRECTORY: &str = "/opt/ml/checkpoints";

/// The repository segment SageMaker's built-in gradient-boosted tree algorithm is published
/// under ("Docker registry paths and example code" lists it as `sagemaker-xgboost`), which is
/// how a job's `TrainingImage` names that algorithm.
pub const BOOSTED_TREES_REPOSITORY: &str = "xgboost";

/// The file that algorithm's container writes the trained model into, inside
/// [`MODEL_DIRECTORY`].
pub const BOOSTED_TREES_MODEL_FILE: &str = "xgboost-model";

/// The hyperparameter naming the learning task the job asked for, which is what the default
/// evaluation metric follows ("`eval_metric` [default according to objective]").
pub const OBJECTIVE_HYPER_PARAMETER: &str = "objective";

/// The hyperparameter naming the evaluation metric outright, which takes precedence over the
/// default the objective implies.
pub const EVAL_METRIC_HYPER_PARAMETER: &str = "eval_metric";

/// A metric a training run can MEASURE over a channel's records, and the name the algorithm
/// emits it under.
///
/// Only metrics this run can really measure from the model it fitted and the records it read
/// are here: a name whose value nothing measured would be a number with no run behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelMetric {
    /// Root mean square error — XGBoost's `rmse`, the default for the regression objectives.
    RootMeanSquareError,
    /// Binary classification error rate — XGBoost's `error`, "#(wrong cases)/#(all cases)".
    ClassificationError,
    /// Multiclass classification error rate — XGBoost's `merror`, the same ratio over the
    /// predicted class.
    MultiClassError,
    /// Mean absolute error — XGBoost's `mae`.
    MeanAbsoluteError,
}

impl ChannelMetric {
    /// The metric's own name, which the emitted metric is `<channel>:<name>`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::RootMeanSquareError => "rmse",
            Self::ClassificationError => "error",
            Self::MultiClassError => "merror",
            Self::MeanAbsoluteError => "mae",
        }
    }

    /// The metric NAMED `label`, and `None` for a name this run can measure nothing for.
    #[must_use]
    pub fn named(label: &str) -> Option<Self> {
        [
            Self::RootMeanSquareError,
            Self::ClassificationError,
            Self::MultiClassError,
            Self::MeanAbsoluteError,
        ]
        .into_iter()
        .find(|metric| metric.label() == label)
    }

    /// This metric MEASURED over `records` with `model`'s own predictions.
    ///
    /// Every value is a measurement over real predictions, so a caller that gates on it reads a
    /// number the run produced from the data the request named. An empty channel measures zero
    /// error, because there is no case the model got wrong.
    #[must_use]
    pub fn measure(self, model: &LinearModel, records: &[Record]) -> f64 {
        if records.is_empty() {
            return 0.0;
        }
        let count = records.len() as f64;
        match self {
            Self::RootMeanSquareError => model.rmse(records),
            // "#(wrong cases)/#(all cases)": a margin at or above the midpoint predicts the
            // positive class, which is the threshold XGBoost's own `error` metric applies.
            Self::ClassificationError => {
                let wrong = records
                    .iter()
                    .filter(|record| {
                        (model.predict(&record.features) >= 0.5) != (record.target >= 0.5)
                    })
                    .count();
                wrong as f64 / count
            }
            // The same ratio over the CLASS a prediction names, which for a numeric target is
            // the nearest class label.
            Self::MultiClassError => {
                let wrong = records
                    .iter()
                    .filter(|record| {
                        model.predict(&record.features).round() != record.target.round()
                    })
                    .count();
                wrong as f64 / count
            }
            Self::MeanAbsoluteError => {
                records
                    .iter()
                    .map(|record| (model.predict(&record.features) - record.target).abs())
                    .sum::<f64>()
                    / count
            }
        }
    }
}

/// The default evaluation metric the learning task `objective` implies, as XGBoost documents
/// it ("`eval_metric` [default according to objective]"): the squared error for the regression
/// and count tasks, the classification error rate for the binary ones, and the multiclass
/// error rate for the multiclass ones.
///
/// An objective this catalog does not name — and a request that names none — falls to the
/// squared error, which is the measurement the default train instructions make.
#[must_use]
pub fn default_metric_for_objective(objective: Option<&str>) -> ChannelMetric {
    let Some(objective) = objective else {
        return ChannelMetric::RootMeanSquareError;
    };
    let (family, _) = objective.split_once(':').unwrap_or((objective, ""));
    match (family, objective) {
        ("binary", _) => ChannelMetric::ClassificationError,
        ("multi", _) => ChannelMetric::MultiClassError,
        _ => ChannelMetric::RootMeanSquareError,
    }
}

/// What the algorithm a job names decides about the job's observable outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrainingAlgorithm {
    /// Whether the job names SageMaker's built-in gradient-boosted tree algorithm.
    pub boosted_trees: bool,
    /// The metric its container emits per input channel, and `None` for a job whose requested
    /// metric this run can measure nothing for — such a job reports no final metric at all,
    /// because a name with no measurement behind it is not a metric the run emitted.
    pub metric: Option<ChannelMetric>,
}

impl TrainingAlgorithm {
    /// The algorithm the `AlgorithmSpecification` of `request` names, read from the image path
    /// (or the registered algorithm's name) and from the hyperparameters that decide the
    /// metric.
    #[must_use]
    pub fn of(request: &crate::ops::types::CreateTrainingJobInput) -> Self {
        let specification = request.algorithm_specification.as_ref();
        let named: Vec<&str> = specification
            .into_iter()
            .flat_map(|spec| {
                spec.training_image
                    .as_deref()
                    .into_iter()
                    .chain(spec.algorithm_name.as_deref())
            })
            .collect();
        let boosted_trees = named
            .iter()
            .any(|value| names_repository(value, BOOSTED_TREES_REPOSITORY));
        let hyper_parameters = request.hyper_parameters.as_ref();
        let read = |name: &str| {
            hyper_parameters
                .and_then(|parameters| parameters.get(name))
                .map(String::as_str)
        };
        // A request that names the metric outright is reported with THAT metric, and otherwise
        // with the one its learning task implies. A name nothing here can measure a value for
        // leaves the job reporting NO metric: reporting another metric's name instead would be
        // a plausible wrong answer, and reporting that name with a number nothing measured
        // would be a value with no run behind it.
        let metric = match read(EVAL_METRIC_HYPER_PARAMETER) {
            Some(named) => ChannelMetric::named(named),
            None => Some(default_metric_for_objective(read(OBJECTIVE_HYPER_PARAMETER))),
        };
        Self {
            boosted_trees,
            metric,
        }
    }

    /// The file this algorithm's container writes the trained model into, inside
    /// [`MODEL_DIRECTORY`] and so inside the archive uploaded for the job.
    ///
    /// A job running a container of the caller's own is reported with the file this emulator's
    /// default train instructions write ([`super::MODEL_ARCHIVE_ENTRY`]), which carries the
    /// parameters the fit measured.
    #[must_use]
    pub fn model_file(self) -> &'static str {
        if self.boosted_trees {
            BOOSTED_TREES_MODEL_FILE
        } else {
            super::MODEL_ARCHIVE_ENTRY
        }
    }

    /// The metric the run emits for the channel `channel`: the channel's own name, then the
    /// metric's, which is the form SageMaker's built-in algorithms emit a per-channel metric
    /// under — and `None` for a job whose requested metric nothing here measures.
    #[must_use]
    pub fn channel_metric_name(self, channel: &str) -> Option<String> {
        self.metric
            .map(|metric| format!("{channel}:{}", metric.label()))
    }
}

/// Whether the container image path (or algorithm reference) `value` names the repository
/// `repository`.
///
/// An image path is `<registry>/<repository>[:<tag>]`, so the repository is the last path
/// segment with its tag and digest cut off; a built-in algorithm's repository is
/// `sagemaker-<algorithm>`, so the segment is matched on the algorithm's own name inside it
/// rather than on the whole segment.
///
/// The ONE reading of which algorithm an image path names, so the metric set that algorithm
/// publishes ([`super::builtin_algorithm_metrics`]) is resolved off the same reading as the
/// file its container writes the model into.
pub(super) fn names_repository(value: &str, repository: &str) -> bool {
    let segment = value.rsplit('/').next().unwrap_or(value);
    let segment = segment.split_once('@').map_or(segment, |(name, _)| name);
    let segment = segment.split_once(':').map_or(segment, |(name, _)| name);
    segment.split(['-', '_']).any(|part| part == repository)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A model that predicts its single feature, which every measurement below is taken over.
    fn identity_model() -> LinearModel {
        LinearModel {
            intercept: 0.0,
            weights: vec![1.0],
            epochs: 1,
        }
    }

    /// Records whose target is `target` and whose single feature is `feature`.
    fn records(rows: &[(f64, f64)]) -> Vec<Record> {
        rows.iter()
            .map(|(target, feature)| Record {
                target: *target,
                features: vec![*feature],
            })
            .collect()
    }

    /// RULE: the metric a job's final metrics are named by follows the ALGORITHM and the
    /// learning task the request asked for, never a fixed pair of names.
    ///
    /// The rows differ along how the algorithm is named (a built-in image, a registered
    /// algorithm, a caller's own image, none at all), along the objective (regression, binary,
    /// multiclass, one no catalog entry names, none) and along whether the request names the
    /// metric outright. `multi:softprob` is an entry no report cited.
    #[test]
    fn the_metric_name_follows_the_algorithm_and_the_objective() {
        /// One row: the case, the image, the algorithm name, the hyperparameters, and the
        /// metric name the `train` channel is reported under together with the model file.
        struct Row {
            case: &'static str,
            image: Option<&'static str>,
            algorithm: Option<&'static str>,
            parameters: &'static [(&'static str, &'static str)],
            metric: Option<&'static str>,
            model_file: &'static str,
        }
        let rows = [
            Row {
                case: "the built-in boosted trees image with a binary objective",
                image: Some("683313688378.dkr.ecr.us-west-2.amazonaws.com/sagemaker-xgboost:1.7-1"),
                algorithm: None,
                parameters: &[("objective", "binary:logistic"), ("num_round", "10")],
                metric: Some("train:error"),
                model_file: BOOSTED_TREES_MODEL_FILE,
            },
            Row {
                case: "the same image with a regression objective",
                image: Some("683313688378.dkr.ecr.us-west-2.amazonaws.com/sagemaker-xgboost:1.7-1"),
                algorithm: None,
                parameters: &[("objective", "reg:squarederror")],
                metric: Some("train:rmse"),
                model_file: BOOSTED_TREES_MODEL_FILE,
            },
            Row {
                case: "a multiclass objective",
                image: Some("000000000000.dkr.ecr.us-east-1.amazonaws.com/sagemaker-xgboost:1.5-1"),
                algorithm: None,
                parameters: &[("objective", "multi:softprob"), ("num_class", "3")],
                metric: Some("train:merror"),
                model_file: BOOSTED_TREES_MODEL_FILE,
            },
            Row {
                case: "an objective outside the catalog",
                image: Some("000000000000.dkr.ecr.us-east-1.amazonaws.com/sagemaker-xgboost:1.5-1"),
                algorithm: None,
                parameters: &[("objective", "survival:cox")],
                metric: Some("train:rmse"),
                model_file: BOOSTED_TREES_MODEL_FILE,
            },
            Row {
                case: "the metric named outright, against the objective's default",
                image: Some("000000000000.dkr.ecr.us-east-1.amazonaws.com/sagemaker-xgboost:1.5-1"),
                algorithm: None,
                parameters: &[("objective", "binary:logistic"), ("eval_metric", "mae")],
                metric: Some("train:mae"),
                model_file: BOOSTED_TREES_MODEL_FILE,
            },
            Row {
                case: "a metric this run can measure nothing for",
                image: Some("000000000000.dkr.ecr.us-east-1.amazonaws.com/sagemaker-xgboost:1.5-1"),
                algorithm: None,
                parameters: &[("objective", "binary:logistic"), ("eval_metric", "auc")],
                metric: None,
                model_file: BOOSTED_TREES_MODEL_FILE,
            },
            Row {
                case: "a registered algorithm naming the same algorithm",
                image: None,
                algorithm: Some("arn:aws:sagemaker:us-east-1:000000000000:algorithm/xgboost"),
                parameters: &[("objective", "binary:hinge")],
                metric: Some("train:error"),
                model_file: BOOSTED_TREES_MODEL_FILE,
            },
            Row {
                case: "a container of the caller's own",
                image: Some("000000000000.dkr.ecr.us-east-1.amazonaws.com/my-trainer:1"),
                algorithm: None,
                parameters: &[],
                metric: Some("train:rmse"),
                model_file: super::super::MODEL_ARCHIVE_ENTRY,
            },
        ];
        for row in rows {
            let request = crate::ops::types::CreateTrainingJobInput {
                algorithm_specification: Some(crate::ops::types::AlgorithmSpecification {
                    training_image: row.image.map(str::to_string),
                    algorithm_name: row.algorithm.map(str::to_string),
                    ..Default::default()
                }),
                hyper_parameters: Some(
                    row.parameters
                        .iter()
                        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                        .collect(),
                ),
                ..Default::default()
            };
            let algorithm = TrainingAlgorithm::of(&request);
            let name = algorithm.channel_metric_name("train");
            assert_eq!(
                name.as_deref(),
                row.metric,
                "case {:?}: expected the metric {:?}, got {name:?}",
                row.case,
                row.metric
            );
            assert_eq!(
                algorithm.model_file(),
                row.model_file,
                "case {:?}: expected the model file {:?}, got {:?}",
                row.case,
                row.model_file,
                algorithm.model_file()
            );
        }
    }

    /// RULE: every metric VALUE is measured from the model's own predictions over the
    /// channel's records, so a metric name carries the number its own definition computes.
    ///
    /// The rows differ along the metric and along how well the model fits the records.
    #[test]
    fn each_metric_is_measured_from_the_predictions() {
        let model = identity_model();
        // (case, metric, records, the value its definition gives)
        let rows: [(&str, ChannelMetric, Vec<Record>, f64); 6] = [
            (
                "no error at all",
                ChannelMetric::RootMeanSquareError,
                records(&[(1.0, 1.0), (2.0, 2.0)]),
                0.0,
            ),
            (
                "a unit error on every record",
                ChannelMetric::RootMeanSquareError,
                records(&[(2.0, 1.0), (3.0, 2.0)]),
                1.0,
            ),
            (
                "half the cases on the wrong side of the threshold",
                ChannelMetric::ClassificationError,
                records(&[(1.0, 1.0), (1.0, 0.0), (0.0, 0.0), (0.0, 1.0)]),
                0.5,
            ),
            (
                "every class predicted right",
                ChannelMetric::MultiClassError,
                records(&[(0.0, 0.1), (1.0, 1.1), (2.0, 1.9)]),
                0.0,
            ),
            (
                "one class out of three predicted wrong",
                ChannelMetric::MultiClassError,
                records(&[(0.0, 0.1), (1.0, 1.1), (2.0, 0.0)]),
                1.0 / 3.0,
            ),
            (
                "the mean of the absolute errors",
                ChannelMetric::MeanAbsoluteError,
                records(&[(2.0, 1.0), (2.0, 4.0)]),
                1.5,
            ),
        ];
        for (case, metric, rows, expected) in rows {
            let measured = metric.measure(&model, &rows);
            assert!(
                (measured - expected).abs() < 1e-9,
                "case {case:?}: expected {expected}, got {measured}"
            );
        }
        for metric in [
            ChannelMetric::RootMeanSquareError,
            ChannelMetric::ClassificationError,
            ChannelMetric::MultiClassError,
            ChannelMetric::MeanAbsoluteError,
        ] {
            let measured = metric.measure(&model, &[]);
            assert!(
                measured.abs() < f64::EPSILON,
                "case {:?} over no record: expected 0, got {measured}",
                metric.label()
            );
        }
    }

    /// RULE: the algorithm is read off the REPOSITORY an image path names, so a tag, a digest,
    /// a registry and a repository that merely contains the name are each read for what they
    /// are.
    #[test]
    fn the_repository_decides_the_built_in_algorithm() {
        // (case, the value, whether it names the boosted-trees repository)
        let rows: [(&str, &str, bool); 7] = [
            ("the published repository", "1.dkr.ecr.us-east-1.amazonaws.com/sagemaker-xgboost:1.7-1", true),
            ("no tag", "1.dkr.ecr.us-east-1.amazonaws.com/sagemaker-xgboost", true),
            ("a digest", "1.dkr.ecr.us-east-1.amazonaws.com/sagemaker-xgboost@sha256:abc", true),
            ("the bare algorithm name", "xgboost", true),
            ("another built-in algorithm", "1.dkr.ecr.us-east-1.amazonaws.com/linear-learner:1", false),
            ("a caller's own image", "1.dkr.ecr.us-east-1.amazonaws.com/trainer:1", false),
            ("a repository whose name merely starts the same", "1.dkr.ecr.us-east-1.amazonaws.com/xgboostish:1", false),
        ];
        for (case, value, expected) in rows {
            let read = names_repository(value, BOOSTED_TREES_REPOSITORY);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected}, got {read}"
            );
        }
    }
}
