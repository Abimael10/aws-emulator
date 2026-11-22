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
//! # The built-in algorithms this reading knows
//!
//! SageMaker publishes each built-in algorithm as an image whose repository is
//! `sagemaker-<algorithm>` ("Docker registry paths for SageMaker built-in algorithms"), so the
//! algorithm a job runs is read off the repository the image path names rather than guessed
//! from anything else ([`BuiltInAlgorithm`]). One is the gradient-boosted tree algorithm, which
//! is also the one whose serialized model this emulator can SERVE ([`super::boosted_model`]),
//! so the pair stays consistent: it writes its model as `xgboost-model` and emits
//! `<channel>:<eval_metric>` per channel. The other is the text algorithm, whose supervised mode
//! is a classification: it writes its model as `model.bin` and is evaluated by the share of the
//! labels it predicts right, so a job of that mode emits `<channel>:accuracy`.
//!
//! Which metric a job emits therefore comes from the learning task its own hyperparameters
//! declare: XGBoost's `eval_metric` parameter is documented as defaulting "according to
//! objective", the text algorithm's `mode` states whether its run is a supervised
//! classification, and a request may name the metric outright. All are hyperparameters of the
//! job, so all are read from the `HyperParameters` the request carried.

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

/// The repository segment SageMaker's built-in text algorithm is published under ("Docker
/// registry paths and example code" lists it as `sagemaker-blazingtext`), which is how a job's
/// `TrainingImage` names that algorithm.
pub const TEXT_CLASSIFICATION_REPOSITORY: &str = "blazingtext";

/// The file that algorithm's container writes the trained model into, inside
/// [`MODEL_DIRECTORY`]: the model archive of a text job holds `model.bin`.
pub const TEXT_CLASSIFICATION_MODEL_FILE: &str = "model.bin";

/// The hyperparameter naming the TRAINING MODE of the text algorithm, which is what decides
/// whether its run is a supervised classification or a word-vector one.
pub const MODE_HYPER_PARAMETER: &str = "mode";

/// The [`MODE_HYPER_PARAMETER`] value whose run is a supervised text classification — the mode
/// whose container is evaluated by how many of the labels it predicts are right.
pub const SUPERVISED_MODE: &str = "supervised";

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
    /// The share of the records whose class the model predicts right — the metric a supervised
    /// text classification run is evaluated by.
    Accuracy,
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
            Self::Accuracy => "accuracy",
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
            Self::Accuracy,
        ]
        .into_iter()
        .find(|metric| metric.label() == label)
    }

    /// This metric MEASURED over `records` with `model`'s own predictions.
    ///
    /// Every value is a measurement over real predictions, so a caller that gates on it reads a
    /// number the run produced from the data the request named. A channel the run read no record
    /// from measures zero, and no metric is emitted for it at all
    /// ([`super::training_run::run_training_job`]).
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
            // The share of the records the model got RIGHT, over the class a prediction names —
            // the nearest class label, the same reading the multiclass error rate takes of a
            // numeric target.
            Self::Accuracy => {
                let right = records
                    .iter()
                    .filter(|record| {
                        model.predict(&record.features).round() == record.target.round()
                    })
                    .count();
                right as f64 / count
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

/// A SageMaker BUILT-IN algorithm, as far as a job's observable outputs go: which file its
/// container writes the trained model into, and which metric it emits per input channel.
///
/// Every entry is an algorithm whose outputs this emulator can really produce: the run measures
/// that algorithm's own metric over the records it read, so an algorithm no entry names is
/// reported with what the default train instructions measured rather than with a name this run
/// has no measurement for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltInAlgorithm {
    /// The gradient-boosted tree algorithm ([`BOOSTED_TREES_REPOSITORY`]), whose evaluation
    /// metric follows the learning task its hyperparameters declare.
    BoostedTrees,
    /// The text algorithm ([`TEXT_CLASSIFICATION_REPOSITORY`]), whose supervised mode is a text
    /// classification evaluated by the share of the labels it predicts right.
    TextClassification,
}

impl BuiltInAlgorithm {
    /// The built-in algorithm the image path (or registered algorithm reference) `value` names,
    /// and `None` for one naming a container of the caller's own.
    #[must_use]
    pub fn named_by(value: &str) -> Option<Self> {
        if names_repository(value, BOOSTED_TREES_REPOSITORY) {
            Some(Self::BoostedTrees)
        } else if names_repository(value, TEXT_CLASSIFICATION_REPOSITORY) {
            Some(Self::TextClassification)
        } else {
            None
        }
    }

    /// The file this algorithm's container writes the trained model into, inside
    /// [`MODEL_DIRECTORY`].
    #[must_use]
    pub fn model_file(self) -> &'static str {
        match self {
            Self::BoostedTrees => BOOSTED_TREES_MODEL_FILE,
            Self::TextClassification => TEXT_CLASSIFICATION_MODEL_FILE,
        }
    }

    /// The metric this algorithm's container emits per channel when the request names none
    /// outright: the one the LEARNING TASK its own hyperparameters declare is evaluated by.
    ///
    /// `None` is an algorithm whose task this run measures nothing for — the word-vector modes
    /// of the text algorithm, which are evaluated over a vocabulary rather than over the
    /// records — so such a job reports no final metric rather than a number nothing measured.
    #[must_use]
    pub fn default_metric(
        self,
        mode: Option<&str>,
        objective: Option<&str>,
    ) -> Option<ChannelMetric> {
        match self {
            Self::BoostedTrees => Some(default_metric_for_objective(objective)),
            Self::TextClassification => {
                (mode == Some(SUPERVISED_MODE)).then_some(ChannelMetric::Accuracy)
            }
        }
    }
}

/// What the algorithm a job names decides about the job's observable outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrainingAlgorithm {
    /// The built-in algorithm the job names, and `None` for a job running a container of the
    /// caller's own.
    pub built_in: Option<BuiltInAlgorithm>,
    /// The metric its container emits per input channel, and `None` for a job whose requested
    /// metric this run can measure nothing for — such a job reports no final metric at all,
    /// because a name with no measurement behind it is not a metric the run emitted.
    pub metric: Option<ChannelMetric>,
    /// The function the learning task the job asked for answers its score through, which is
    /// the space the model it trains is fitted and served in ([`super::ObjectiveLink`]): a
    /// probability task answers the logistic of the score, a count task a positive quantity,
    /// and every other — including a job that asked for no task at all — the score itself.
    pub link: ObjectiveLink,
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
        let built_in = named
            .iter()
            .find_map(|value| BuiltInAlgorithm::named_by(value));
        let hyper_parameters = request.hyper_parameters.as_ref();
        let read = |name: &str| {
            hyper_parameters
                .and_then(|parameters| parameters.get(name))
                .map(String::as_str)
        };
        // A request that names the metric outright is reported with THAT metric, and otherwise
        // with the one the ALGORITHM it names emits for the learning task its hyperparameters
        // declare. A name nothing here can measure a value for leaves the job reporting NO
        // metric: reporting another metric's name instead would be a plausible wrong answer, and
        // reporting that name with a number nothing measured would be a value with no run behind
        // it.
        let metric = match read(EVAL_METRIC_HYPER_PARAMETER) {
            Some(named) => ChannelMetric::named(named),
            None => match built_in {
                Some(algorithm) => algorithm.default_metric(
                    read(MODE_HYPER_PARAMETER),
                    read(OBJECTIVE_HYPER_PARAMETER),
                ),
                // A container of the caller's own is reported with what this emulator's own
                // default train instructions measured over its data.
                None => Some(default_metric_for_objective(read(OBJECTIVE_HYPER_PARAMETER))),
            },
        };
        // The learning task itself decides what the trained model ANSWERS, which is a
        // different question from how it is evaluated: the task is read off the same
        // hyperparameter and through the one reading of an objective this crate has
        // ([`super::ObjectiveLink::of_objective`]), so the model an invocation is served from
        // answers in the space its task is defined on.
        let link = read(OBJECTIVE_HYPER_PARAMETER)
            .map_or(ObjectiveLink::Identity, ObjectiveLink::of_objective);
        Self {
            built_in,
            metric,
            link,
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
        match self.built_in {
            Some(algorithm) => algorithm.model_file(),
            None => super::MODEL_ARCHIVE_ENTRY,
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
            link: ObjectiveLink::Identity,
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
    /// multiclass, one no catalog entry names, none), along the text algorithm's training mode
    /// (supervised, a word-vector one) and along whether the request names the metric outright.
    /// `multi:softprob` is an entry no report cited.
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
                case: "the built-in text image in its supervised mode",
                image: Some(
                    "811284229777.dkr.ecr.us-east-1.amazonaws.com/blazingtext:1",
                ),
                algorithm: None,
                parameters: &[("mode", "supervised"), ("epochs", "5")],
                metric: Some("train:accuracy"),
                model_file: TEXT_CLASSIFICATION_MODEL_FILE,
            },
            Row {
                case: "the same image in a word-vector mode, which this run measures nothing for",
                image: Some(
                    "811284229777.dkr.ecr.us-east-1.amazonaws.com/sagemaker-blazingtext:latest",
                ),
                algorithm: None,
                parameters: &[("mode", "skipgram")],
                metric: None,
                model_file: TEXT_CLASSIFICATION_MODEL_FILE,
            },
            Row {
                case: "the text image with the metric named outright",
                image: Some(
                    "811284229777.dkr.ecr.us-east-1.amazonaws.com/blazingtext:1",
                ),
                algorithm: None,
                parameters: &[("mode", "supervised"), ("eval_metric", "mae")],
                metric: Some("train:mae"),
                model_file: TEXT_CLASSIFICATION_MODEL_FILE,
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
        let rows: [(&str, ChannelMetric, Vec<Record>, f64); 7] = [
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
            (
                "three classes out of four predicted right",
                ChannelMetric::Accuracy,
                records(&[(0.0, 0.1), (1.0, 1.1), (2.0, 1.9), (2.0, 0.0)]),
                0.75,
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
            ChannelMetric::Accuracy,
        ] {
            let measured = metric.measure(&model, &[]);
            assert!(
                measured.abs() < f64::EPSILON,
                "case {:?} over no record: expected 0, got {measured}",
                metric.label()
            );
        }
    }

    /// RULE: the function a trained model answers its score through follows the LEARNING TASK
    /// the job's hyperparameters declare, so a task whose output is a probability serves a
    /// value in the unit interval and a task whose output is a count a positive quantity.
    ///
    /// The rows differ along the objective family (probability, count, survival, regression,
    /// the raw-margin members of the probability family, one no catalog entry names) and along
    /// whether the request declares a task at all. `reg:logistic` and `count:poisson` are
    /// entries no report cited.
    #[test]
    fn the_answer_space_follows_the_learning_task() {
        // (case, the `objective` hyperparameter, the link the task answers through)
        let rows: [(&str, Option<&str>, ObjectiveLink); 8] = [
            (
                "a binary probability task",
                Some("binary:logistic"),
                ObjectiveLink::Logistic,
            ),
            (
                "a regression task on a probability target",
                Some("reg:logistic"),
                ObjectiveLink::Logistic,
            ),
            (
                "the raw-margin member of the same family",
                Some("binary:logitraw"),
                ObjectiveLink::Identity,
            ),
            (
                "a binary task answering a class rather than a probability",
                Some("binary:hinge"),
                ObjectiveLink::Identity,
            ),
            (
                "a count task",
                Some("count:poisson"),
                ObjectiveLink::Exponential,
            ),
            (
                "a survival task",
                Some("survival:cox"),
                ObjectiveLink::Exponential,
            ),
            (
                "a squared-error regression task",
                Some("reg:squarederror"),
                ObjectiveLink::Identity,
            ),
            ("no task declared at all", None, ObjectiveLink::Identity),
        ];
        for (case, objective, expected) in rows {
            let request = crate::ops::types::CreateTrainingJobInput {
                hyper_parameters: Some(
                    objective
                        .map(|named| {
                            (
                                OBJECTIVE_HYPER_PARAMETER.to_string(),
                                named.to_string(),
                            )
                        })
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            };
            let link = TrainingAlgorithm::of(&request).link;
            assert_eq!(
                link, expected,
                "case {case:?}: expected the link {expected:?}, got {link:?}"
            );
            // The link is what the model ANSWERS through, so a probability task's model
            // cannot answer outside the unit interval however large its score.
            if expected == ObjectiveLink::Logistic {
                let model = LinearModel {
                    intercept: 0.0,
                    weights: vec![1.0],
                    epochs: 1,
                    link,
                };
                for feature in [-1e6, -3.0, 0.0, 2.5, 1e6] {
                    let served = model.predict(&[feature]);
                    assert!(
                        (0.0..=1.0).contains(&served),
                        "case {case:?}: a probability task's model must answer in [0, 1] \
                         for the feature {feature}, got {served}"
                    );
                }
            }
        }
    }

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

        // And the same reading decides WHICH built-in algorithm a value names: each
        // repository its own, and nothing for a container of the caller's own.
        let named: [(&str, &str, Option<BuiltInAlgorithm>); 4] = [
            (
                "the boosted-trees repository",
                "1.dkr.ecr.us-east-1.amazonaws.com/sagemaker-xgboost:1.7-1",
                Some(BuiltInAlgorithm::BoostedTrees),
            ),
            (
                "the text repository",
                "811284229777.dkr.ecr.us-east-1.amazonaws.com/blazingtext:1",
                Some(BuiltInAlgorithm::TextClassification),
            ),
            (
                "the same text repository as the framework image path",
                "811284229777.dkr.ecr.us-east-1.amazonaws.com/sagemaker-blazingtext@sha256:abc",
                Some(BuiltInAlgorithm::TextClassification),
            ),
            (
                "a container of the caller's own",
                "1.dkr.ecr.us-east-1.amazonaws.com/trainer:1",
                None,
            ),
        ];
        for (case, value, expected) in named {
            let read = BuiltInAlgorithm::named_by(value);
            assert_eq!(
                read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }
}
