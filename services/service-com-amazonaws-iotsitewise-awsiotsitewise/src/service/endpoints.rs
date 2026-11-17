// SPDX-License-Identifier: CC0-1.0

//! WHICH of this service's three endpoints an operation is hosted on, read off the
//! model: the partition its two refusal DIALECTS follow.
//!
//! # The rule
//!
//! Every operation this model binds carries an `@endpoint` host prefix, and the
//! model uses three of them: `api.`, the control plane that holds the asset models,
//! the assets and the rest of the configuration, `monitor.`, the IoT SiteWise
//! Monitor plane that holds the portals, projects, dashboards and access policies,
//! and [`DATA_ENDPOINT_PREFIX`], the data plane that reads and writes the time
//! series those assets publish. They are FRONTENDS of one service, and the data
//! plane does not word a refusal the way the other two do: its request-validation
//! report quotes the value it rejected and publishes a member's expression without
//! the anchors that wrap it, where theirs states neither
//! ([`super::validation`]).
//!
//! So the classification is what decides the dialect a refusal is stated in. It
//! is read from the shipped Smithy model rather than listed here, so it cannot
//! drift from the model and covers every operation the model binds — not only the
//! ones a recording happens to cover.
//!
//! # How a request is classified
//!
//! By the INPUT SHAPE being read, which is what the boundary has in hand when the
//! framework asks this service to word a refusal
//! (`super::input_shape::reading_input`): the census maps each operation's input
//! shape to that operation's host prefix, and the request-time question is one
//! lookup of the shape's own name.

use std::collections::HashMap;
use std::sync::LazyLock;

use ::serde_json::Value;

/// This service's Smithy model: the one source for which endpoint hosts an
/// operation.
const MODEL_JSON: &str =
    include_str!("../../smithy-model/com-amazonaws-iotsitewise-awsiotsitewise.json");

/// The Smithy trait that carries an operation's endpoint host prefix.
const ENDPOINT_TRAIT: &str = "smithy.api#endpoint";

/// The host prefix of the DATA endpoint — the time-series plane.
///
/// The two other prefixes the model uses (the module docs name them) are not
/// constants here: nothing is decided by the ABSENCE of this one, and a fourth
/// prefix a model refresh added would be classified with them rather than
/// silently read as this one.
pub const DATA_ENDPOINT_PREFIX: &str = "data.";

/// Each operation's host prefix, keyed by the short name of the shape its INPUT
/// targets (`GetAssetPropertyAggregatesRequest`), which is the name the schema a
/// request is read against carries.
///
/// Built once: the model is a compile-time constant, so the answer cannot change
/// while the process runs. A document that does not parse is a broken build
/// rather than a request-time failure, so it is not swallowed into an empty map
/// that would quietly state every refusal in the control plane's dialect.
static HOST_PREFIX_BY_INPUT_SHAPE: LazyLock<HashMap<String, String>> =
    LazyLock::new(host_prefixes_by_input_shape);

/// The host prefix the model binds the operation reading `input_shape_name` to,
/// or `None` for a shape no operation of this model takes as its input.
#[must_use]
pub fn host_prefix_of_input(input_shape_name: &str) -> Option<&'static str> {
    HOST_PREFIX_BY_INPUT_SHAPE
        .get(input_shape_name)
        .map(String::as_str)
}

/// Whether the operation reading `input_shape_name` is hosted on the DATA
/// endpoint.
#[must_use]
pub fn serves_the_data_endpoint(input_shape_name: &str) -> bool {
    host_prefix_of_input(input_shape_name) == Some(DATA_ENDPOINT_PREFIX)
}

/// The census: every operation's `@endpoint` host prefix against the short name
/// of the shape its input targets.
fn host_prefixes_by_input_shape() -> HashMap<String, String> {
    let model: Value = ::serde_json::from_str(MODEL_JSON)
        .expect("MODEL_JSON is this crate's own Smithy model and parses");
    let Some(shapes) = model.get("shapes").and_then(Value::as_object) else {
        return HashMap::new();
    };
    shapes
        .values()
        .filter(|shape| shape.get("type").and_then(Value::as_str) == Some("operation"))
        .filter_map(|operation| {
            let input = operation
                .pointer("/input/target")
                .and_then(Value::as_str)
                .map(short_name)?;
            let prefix = operation
                .pointer(&format!("/traits/{ENDPOINT_TRAIT}/hostPrefix"))
                .and_then(Value::as_str)?;
            Some((input.to_string(), prefix.to_string()))
        })
        .collect()
}

/// The short name of a shape id: what follows the namespace.
fn short_name(shape_id: &str) -> &str {
    shape_id.rsplit('#').next().unwrap_or(shape_id)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// RULE: the model hosts every operation it binds on one of the three
    /// endpoints this service has, so no operation is classified by the absence of
    /// a class the crate does not know about.
    #[test]
    fn the_model_binds_every_operation_to_one_of_its_endpoints() {
        let model: Value = ::serde_json::from_str(MODEL_JSON).expect("the shipped model parses");
        let shapes = model
            .get("shapes")
            .and_then(Value::as_object)
            .expect("the model holds shapes");
        let operations: Vec<&Value> = shapes
            .values()
            .filter(|shape| shape.get("type").and_then(Value::as_str) == Some("operation"))
            .collect();
        assert!(
            !operations.is_empty(),
            "the model must bind operations, got none"
        );
        let classes: BTreeSet<&str> = operations
            .iter()
            .filter_map(|operation| {
                operation
                    .pointer(&format!("/traits/{ENDPOINT_TRAIT}/hostPrefix"))
                    .and_then(Value::as_str)
            })
            .collect();
        assert_eq!(
            classes,
            ["api.", DATA_ENDPOINT_PREFIX, "monitor."]
                .into_iter()
                .collect(),
            "expected the control plane, the data plane and the Monitor plane, got {classes:?}"
        );
        let unclassified = operations.len() - HOST_PREFIX_BY_INPUT_SHAPE.len();
        assert_eq!(
            unclassified, 0,
            "every operation must carry a host prefix and an input shape, {unclassified} do not"
        );
    }

    /// RULE: a request is classified by the input shape being read. The rows are
    /// this model's own operations, one per endpoint class, and one shape no
    /// operation takes as input.
    #[test]
    fn an_input_shape_is_classified_by_the_endpoint_hosting_its_operation() {
        // (case, the input shape's short name, the prefix the model declares)
        let rows: [(&str, &str, Option<&str>); 7] = [
            (
                "the aggregate read",
                "GetAssetPropertyAggregatesRequest",
                Some(DATA_ENDPOINT_PREFIX),
            ),
            (
                "the bulk import create",
                "CreateBulkImportJobRequest",
                Some(DATA_ENDPOINT_PREFIX),
            ),
            (
                "the batch write",
                "BatchPutAssetPropertyValueRequest",
                Some(DATA_ENDPOINT_PREFIX),
            ),
            ("the asset-model read", "DescribeAssetModelRequest", Some("api.")),
            ("the time-series read", "DescribeTimeSeriesRequest", Some("api.")),
            ("a Monitor portal read", "DescribePortalRequest", Some("monitor.")),
            ("a response shape", "DescribeAssetModelResponse", None),
        ];
        for (case, shape, expected) in rows {
            let answered = host_prefix_of_input(shape);
            assert_eq!(
                answered, expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
            assert_eq!(
                serves_the_data_endpoint(shape),
                expected == Some(DATA_ENDPOINT_PREFIX),
                "case {case:?}: the data-endpoint question must follow the prefix, got {answered:?}"
            );
        }
    }
}
