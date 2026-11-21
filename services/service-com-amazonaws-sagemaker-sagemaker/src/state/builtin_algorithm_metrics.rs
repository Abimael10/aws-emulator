// SPDX-License-Identifier: CC0-1.0

//! The metric set a SageMaker BUILT-IN algorithm publishes, as the job that runs it is
//! reported with.
//!
//! # Why a built-in algorithm brings its own metric set
//!
//! `AlgorithmSpecification$MetricDefinitions` is "a list of metric definition objects. Each
//! object specifies the metric name and regular expressions used to parse algorithm logs.
//! SageMaker publishes each metric to Amazon CloudWatch", and each entry is a
//! `MetricDefinition` of a required `Name` and a required `Regex`
//! (`smithy_query(query="shape",shape="AlgorithmSpecification,MetricDefinition")`). The
//! definitions therefore belong to the ALGORITHM whose logs they parse, not to the request
//! that launches a job: a caller's own container publishes whatever metrics the caller
//! declares, while an algorithm AWS itself publishes already emits a fixed set of metrics and
//! already has the expressions that read them out of its logs. The same member's neighbour
//! says so in as many words — time-series metrics are generated without the caller asking
//! when "You use one of the SageMaker built-in algorithms"
//! (`AlgorithmSpecification$EnableSageMakerMetricsTimeSeries`).
//!
//! So a job launched from a built-in algorithm image is reported with that algorithm's own
//! metric set when its request states none, exactly as a job naming a REGISTERED algorithm is
//! reported with the set that algorithm's `TrainingSpecification` declares; a job that states
//! its own keeps exactly those, and a job running a container of the caller's own is reported
//! with none, because nothing here knows what such a container writes.
//!
//! # Which algorithm an image names, and what it publishes
//!
//! A built-in algorithm is published as an image whose repository is the algorithm's own name
//! ("Docker registry paths and example code"), so the algorithm is read off the repository the
//! image path names ([`super::training_algorithm`] does the same reading for the file its
//! container writes the model into). The entry below is the gradient-boosted tree algorithm —
//! the one built-in algorithm this service models the outputs of
//! ([`super::BOOSTED_TREES_REPOSITORY`], [`super::boosted_model`]) — with the metric names it
//! publishes and the shape of the expression that reads one out of its logs. An image naming
//! an algorithm no entry covers resolves to no metric set at all, rather than to another
//! algorithm's names: a metric name no container emits would be a name a client could never
//! collect a value for.

use super::training_algorithm::names_repository;
use super::BOOSTED_TREES_REPOSITORY;
use crate::ops::types::MetricDefinition;

/// The metric names SageMaker's built-in gradient-boosted tree algorithm publishes, and the
/// ORDER a read reports them in: recorded behaviour, which is where a built-in algorithm's
/// own set is stated at all — a `DescribeTrainingJob` of a job launched from that algorithm's
/// image carries exactly these entries. That algorithm's published metric table names the same
/// `<channel>:<metric>` metrics; neither the entries nor their order follow from the request,
/// so every job on that image is reported with this same set.
///
/// `MetricDefinitionList` is `@length 0..40`, so the whole set is reportable as it stands.
const BOOSTED_TREES_METRICS: &[&str] = &[
    "train:mae",
    "train:merror",
    "validation:mae",
    "validation:logloss",
    "validation:macro_precision",
    "train:mlogloss",
    "validation:binary_f_beta",
    "validation:recall",
    "validation:precision",
    "train:error",
    "train:ndcg",
    "validation:map",
    "validation:auc",
    "train:auc",
    "validation:error",
];

/// The expression that reads the metric `name` out of the gradient-boosted tree algorithm's
/// logs.
///
/// That algorithm writes one line per boosting round — the round in brackets, then each
/// metric as `<channel>-<metric>:<value>` behind a tab, which is the `#011` an Amazon
/// CloudWatch log line carries a tab as. So the expression anchors on the round, then on the
/// metric's own logged token, and captures the number behind it in the decimal or exponent
/// form a floating-point value is written in.
#[must_use]
fn boosted_trees_metric_regex(name: &str) -> String {
    // The logged token of a metric named `<channel>:<metric>`: the same pair, written the way
    // the container writes it into the line.
    let logged = name.replace(':', "-");
    format!(".*\\[[0-9]+\\].*#011{logged}:([-+]?[0-9]*\\.?[0-9]+(?:[eE][-+]?[0-9]+)?).*")
}

/// The metric set the built-in algorithm the image path `image` names publishes, and `None`
/// for a path naming no built-in algorithm this module covers — a caller's own container
/// included.
///
/// Read off the IMAGE, because that is what names a built-in algorithm: a reference to a
/// REGISTERED algorithm carries its own declared set
/// (`TrainingSpecification$MetricDefinitions`), which is resolved where that reference is.
#[must_use]
pub fn builtin_image_metric_definitions(image: &str) -> Option<Vec<MetricDefinition>> {
    if !names_repository(image, BOOSTED_TREES_REPOSITORY) {
        return None;
    }
    Some(
        BOOSTED_TREES_METRICS
            .iter()
            .map(|name| MetricDefinition {
                name: (*name).to_string(),
                regex: boosted_trees_metric_regex(name),
            })
            .collect(),
    )
}

/// The metric set an `AlgorithmSpecification` is reported with when it states none of its own
/// and names no registered algorithm: the set of the built-in algorithm its `TrainingImage`
/// names ([`builtin_image_metric_definitions`]), and `None` for a specification that names no
/// image or names one of the caller's own containers.
#[must_use]
pub fn builtin_specification_metric_definitions(
    specification: &crate::ops::types::AlgorithmSpecification,
) -> Option<Vec<MetricDefinition>> {
    specification
        .training_image
        .as_deref()
        .and_then(builtin_image_metric_definitions)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the metric set follows the ALGORITHM the image names, so every entry is one of
    /// that algorithm's published metrics with the expression that reads it out of its logs,
    /// and an image naming no built-in algorithm resolves to no set.
    #[test]
    fn the_metric_set_follows_the_built_in_algorithm_the_image_names() {
        // Rows differ along what the rule generalises over: the registry the image is pulled
        // from, whether the path carries a tag, a digest or neither, and whether the
        // repository names a built-in algorithm at all.
        let rows: [(&str, &str, bool); 7] = [
            (
                "the published algorithm image of one Region",
                "433757028032.dkr.ecr.us-west-2.amazonaws.com/xgboost:1",
                true,
            ),
            (
                "the published framework image of another Region",
                "683313688378.dkr.ecr.us-east-1.amazonaws.com/sagemaker-xgboost:1.7-1",
                true,
            ),
            (
                "the same repository with no tag",
                "433757028032.dkr.ecr.us-west-2.amazonaws.com/xgboost",
                true,
            ),
            (
                "the same repository pinned by digest",
                "433757028032.dkr.ecr.us-west-2.amazonaws.com/xgboost@sha256:abc",
                true,
            ),
            (
                "another built-in algorithm this catalogue does not cover",
                "433757028032.dkr.ecr.us-west-2.amazonaws.com/linear-learner:1",
                false,
            ),
            (
                "a container of the caller's own",
                "000000000000.dkr.ecr.us-east-1.amazonaws.com/trainer:1",
                false,
            ),
            (
                "a repository whose name merely starts the same",
                "000000000000.dkr.ecr.us-east-1.amazonaws.com/xgboostish:1",
                false,
            ),
        ];
        for (case, image, built_in) in rows {
            let resolved = builtin_image_metric_definitions(image);
            assert_eq!(
                resolved.is_some(),
                built_in,
                "case {case:?}: expected a built-in metric set {built_in}, got {resolved:?}"
            );
            let Some(resolved) = resolved else {
                continue;
            };
            assert_eq!(
                resolved.len(),
                BOOSTED_TREES_METRICS.len(),
                "case {case:?}: one entry per published metric, got {resolved:?}"
            );
            assert!(
                resolved.len() <= 40,
                "case {case:?}: MetricDefinitionList is @length 0..40, got {}",
                resolved.len()
            );
            for (entry, name) in resolved.iter().zip(BOOSTED_TREES_METRICS) {
                assert_eq!(
                    entry.name, *name,
                    "case {case:?}: expected the metric {name:?}, got {:?}",
                    entry.name
                );
                let logged = name.replace(':', "-");
                assert!(
                    entry.regex.contains(&format!("#011{logged}:")),
                    "case {case:?}: {name:?}'s expression must read the logged token \
                     {logged:?}, got {:?}",
                    entry.regex
                );
                assert!(
                    !entry.regex.is_empty() && entry.regex.len() <= 500,
                    "case {case:?}: MetricRegex is @length 1..500, got {:?}",
                    entry.regex
                );
            }
        }
    }

    /// RULE: the expression captures the value the container logs, in every floating-point
    /// form it writes one in.
    #[test]
    fn the_expression_reads_the_value_the_container_logs() {
        let definitions = builtin_image_metric_definitions(
            "433757028032.dkr.ecr.us-west-2.amazonaws.com/xgboost:1",
        )
        .expect("the built-in algorithm's metric set");
        let entry = definitions
            .iter()
            .find(|entry| entry.name == "validation:auc")
            .expect("a metric of the published set");
        let expression = ::regex::Regex::new(&entry.regex).expect("a usable expression");
        // Rows differ along the form the logged value takes: a plain decimal, an integer, a
        // signed value and an exponent form.
        let rows: [(&str, &str, &str); 4] = [
            ("a decimal", "[0]#011validation-auc:0.8312", "0.8312"),
            ("an integer", "[12]#011validation-auc:1", "1"),
            ("a signed value", "[3]#011validation-auc:-0.5", "-0.5"),
            ("an exponent", "[7]#011validation-auc:1.5e-3", "1.5e-3"),
        ];
        for (case, line, expected) in rows {
            let captured = expression
                .captures(line)
                .and_then(|captures| captures.get(1))
                .map(|value| value.as_str().to_string());
            assert_eq!(
                captured.as_deref(),
                Some(expected),
                "case {case:?}: expected {expected:?} out of {line:?}, got {captured:?}"
            );
        }
        // A line of another metric is not read as this one's.
        assert!(
            expression.captures("[0]#011validation-logloss:0.2").is_none(),
            "another metric's line must not match validation:auc's expression"
        );
    }
}
