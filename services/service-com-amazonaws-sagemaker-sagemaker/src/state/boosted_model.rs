// SPDX-License-Identifier: CC0-1.0

//! The GRADIENT-BOOSTED model a deployed artifact holds, and the inference it computes.
//!
//! # Why this reading exists
//!
//! A model deployed behind an endpoint is a container image plus the artifact its
//! `ModelDataUrl` names, and "after you deploy a model into production using Amazon SageMaker
//! AI hosting services, your client applications use this API to get inferences from the model
//! hosted at the specified endpoint" (`InvokeEndpoint`). The artifact is where the model's
//! PARAMETERS are: SageMaker's own gradient-boosting algorithm, and every framework container
//! that wraps it, writes the booster's serialized model into the archive it uploads, in the
//! documented model-serialization format — a document carrying the learner's parameters, its
//! objective and the booster's trees.
//!
//! So an invocation of such a model is answered the way the container answers it: the
//! parameters are READ out of the artifact and the prediction is computed from them and from
//! the record the caller sent. Nothing is derived from the artifact's identity, its name or its
//! bytes' shape — a tree that is not in the document contributes nothing, and an artifact
//! holding no model this reading can take parameters out of is served by nothing at all.
//!
//! # The document
//!
//! The serialized model is ONE document in two interchangeable encodings — text JSON and
//! universal binary JSON — so both are decoded by a maintained parser into the same document
//! and read once here. What is read out of it:
//!
//! * `learner.learner_model_param` — the number of features the model was fitted on, the base
//!   score every prediction starts from, and how many output groups the model has;
//! * `learner.objective.name` — which decides the function applied to the margin the booster
//!   accumulates (see [`ObjectiveLink`]);
//! * `learner.gradient_booster.model.trees` — the boosted trees, each a set of parallel arrays
//!   over its nodes (the split feature and threshold, the two children, the leaf weights and
//!   which way a missing value goes), which is how a tree walk is performed;
//! * `learner.gradient_booster.model.weights` — the coefficients of a LINEAR booster, which
//!   has no trees.
//!
//! Every list the document carries is bounded on the way in ([`MAX_BOOSTED_TREES`],
//! [`MAX_TREE_NODES`]), and the bytes handed to the parsers are bounded by the caller
//! ([`super::MAX_INFLATED_ARCHIVE_BYTES`]): the artifact is a caller's own object.

use super::*;

/// The most trees one booster may carry for this reading to serve it. An EMULATOR ceiling, not
/// a limit of the format: the trees are walked per record of every invocation, so what one
/// invocation can ask for is bounded whatever the artifact declares.
///
/// It bounds the PRODUCT the work is, together with the two other ceilings around it: a walk
/// costs at most [`MAX_TREE_DEPTH`] steps per tree, a payload carries at most
/// [`super::MAX_PAYLOAD_RECORDS`] records, and an artifact declares at most this many trees —
/// so the steps one invocation can cost are bounded by their product rather than by any one of
/// them.
pub const MAX_BOOSTED_TREES: usize = 1_024;

/// The most nodes one tree may carry, bounded for the same reason as [`MAX_BOOSTED_TREES`].
pub const MAX_TREE_NODES: usize = 1 << 16;

/// The most steps one tree's walk may take, which is the depth a record can be routed through.
/// A decision tree deeper than this would need more nodes than any artifact this reading admits
/// (a tree of depth `d` holds `2^d` nodes), so the bound costs no model its predictions and
/// stops a document whose children lead in circles from costing a walk per node.
pub const MAX_TREE_DEPTH: usize = 64;

/// The node index a child position carries when the node is a LEAF: a tree's child arrays use
/// `-1` for "no child", which is where a walk stops.
const NO_CHILD: i64 = -1;

/// What a booster's accumulated margin is turned into, which is what the learner's objective
/// decides.
///
/// The three readings cover every objective the gradient-boosting algorithm documents: the
/// logistic ones answer a probability, the Poisson, gamma, tweedie and accelerated-failure-time
/// ones answer a positive quantity, and every other — the squared-error, absolute-error,
/// pseudo-Huber, raw-logit, hinge and ranking objectives — answers the margin itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectiveLink {
    /// The margin as it stands, which is what a regression or a ranking objective answers.
    Identity,
    /// The logistic function of the margin: a probability in the closed unit interval, which is
    /// what a logistic objective answers.
    Logistic,
    /// The exponential of the margin: the positive quantity a count, gamma, tweedie or survival
    /// objective answers.
    Exponential,
}

impl ObjectiveLink {
    /// The link the objective `name` answers through.
    ///
    /// The prefixes are the objective families the algorithm's own documentation lists
    /// (`binary:logistic` and `reg:logistic` answer "probability", the count, gamma, tweedie and
    /// survival families answer a positive quantity through a log link, and the raw-logit,
    /// hinge, squared-error, absolute-error, pseudo-Huber, quantile and ranking families answer
    /// the margin). An objective this reading does not know answers the margin, which is the
    /// booster's own output before any link.
    #[must_use]
    pub fn of_objective(name: &str) -> Self {
        // `binary:logitraw` answers the margin, so it is matched BEFORE the logistic family.
        if name == "binary:logitraw" || name == "binary:hinge" {
            return Self::Identity;
        }
        if name == "binary:logistic" || name == "reg:logistic" {
            return Self::Logistic;
        }
        if name.starts_with("count:")
            || name.starts_with("survival:")
            || name == "reg:gamma"
            || name == "reg:tweedie"
        {
            return Self::Exponential;
        }
        Self::Identity
    }

    /// The name this link is RECORDED under in a model document this emulator's own training
    /// writes ([`super::model_document`]), so an artifact replays the function its model's
    /// predictions are answered through rather than having it decided again on the way out.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Logistic => "logistic",
            Self::Exponential => "exponential",
        }
    }

    /// The link `label` names ([`Self::label`]), and `None` for a name none of them carries —
    /// which is what a document stating no link at all carries, and answers the margin.
    #[must_use]
    pub fn named(label: &str) -> Option<Self> {
        [Self::Identity, Self::Logistic, Self::Exponential]
            .into_iter()
            .find(|link| link.label() == label)
    }

    /// `margin` through this link.
    #[must_use]
    pub fn apply(self, margin: f64) -> f64 {
        match self {
            Self::Identity => margin,
            Self::Logistic => logistic(margin),
            Self::Exponential => margin.exp(),
        }
    }

    /// The MARGIN a base score of `score` stands for: the base score is stated in the space the
    /// objective answers in, so it is taken back through the link before the trees are added to
    /// it.
    #[must_use]
    pub fn margin_of(self, score: f64) -> f64 {
        match self {
            Self::Identity => score,
            // The logit, which is the inverse of the logistic function; a base score at either
            // end of the interval has no finite logit, so it contributes none.
            Self::Logistic => {
                let ratio = score / (1.0 - score);
                if ratio.is_finite() && ratio > 0.0 {
                    ratio.ln()
                } else {
                    0.0
                }
            }
            Self::Exponential => {
                if score > 0.0 {
                    score.ln()
                } else {
                    0.0
                }
            }
        }
    }
}

/// The logistic function of `value`: `1 / (1 + e^-value)`, in the closed unit interval.
///
/// Saturating rather than overflowing: a magnitude whose exponential is infinite reads as the
/// end of the interval it is heading for, which is the limit it is approaching.
fn logistic(value: f64) -> f64 {
    let read = 1.0 / (1.0 + (-value).exp());
    if read.is_nan() {
        return read;
    }
    read.clamp(0.0, 1.0)
}

/// One boosted tree, as the serialized model carries it: parallel arrays over the tree's nodes.
#[derive(Debug, Clone)]
pub struct BoostedTree {
    /// Which feature each node splits on.
    split_indices: Vec<usize>,
    /// The threshold each node splits at: a record goes LEFT where its value for the node's
    /// feature is below the threshold.
    split_conditions: Vec<f64>,
    /// Each node's left child, or [`NO_CHILD`] for a leaf.
    left_children: Vec<i64>,
    /// Each node's right child, or [`NO_CHILD`] for a leaf.
    right_children: Vec<i64>,
    /// Each node's weight, which is the value a LEAF contributes to the margin.
    base_weights: Vec<f64>,
    /// Which way a record with no value for the node's feature goes.
    default_left: Vec<bool>,
}

impl BoostedTree {
    /// What this tree contributes to a record's margin: the weight of the leaf the record's own
    /// values walk down to.
    ///
    /// A record that carries no value for a node's feature — a shorter record, or a value that
    /// is not a number — takes that node's default direction, which is how a missing value is
    /// documented to be routed. The walk is bounded by [`MAX_TREE_DEPTH`] steps and by the
    /// tree's own node count, whichever is smaller, so a document whose children lead in
    /// circles cannot spin here.
    #[must_use]
    pub fn leaf_weight(&self, features: &[f64]) -> f64 {
        let mut node = 0_usize;
        for _ in 0..self.left_children.len().min(MAX_TREE_DEPTH) {
            let left = self.left_children.get(node).copied().unwrap_or(NO_CHILD);
            let right = self.right_children.get(node).copied().unwrap_or(NO_CHILD);
            if left == NO_CHILD || right == NO_CHILD {
                break;
            }
            let value = self
                .split_indices
                .get(node)
                .and_then(|index| features.get(*index))
                .copied()
                .filter(|value| !value.is_nan());
            let threshold = self.split_conditions.get(node).copied().unwrap_or(0.0);
            let goes_left = match value {
                Some(value) => value < threshold,
                None => self.default_left.get(node).copied().unwrap_or(true),
            };
            let next = if goes_left { left } else { right };
            match usize::try_from(next) {
                Ok(next) if next < self.base_weights.len() => node = next,
                // A child outside the tree's own nodes is no node at all, so the walk stops
                // where it is rather than reading past the arrays.
                _ => break,
            }
        }
        self.base_weights.get(node).copied().unwrap_or(0.0)
    }
}

/// The model a serialized booster holds: what it was fitted on, what it answers, and the
/// parameters an inference is computed from.
#[derive(Debug, Clone)]
pub struct BoostedModel {
    /// `learner_model_param.num_feature`: how many features the model was fitted on.
    pub features: usize,
    /// The margin every prediction starts from, taken from `learner_model_param.base_score`
    /// through the objective's link.
    pub base_margin: f64,
    /// The objective's own name, as the document states it.
    pub objective: String,
    /// What the accumulated margin is answered through.
    pub link: ObjectiveLink,
    /// The booster's trees, in the order the document carries them. Empty for a linear booster.
    pub trees: Vec<BoostedTree>,
    /// A LINEAR booster's coefficients, one per feature and then the bias. Empty for a tree
    /// booster.
    pub weights: Vec<f64>,
}

impl BoostedModel {
    /// What this model answers for one record: the margin its booster accumulates for the
    /// record, through the objective's link.
    #[must_use]
    pub fn predict(&self, features: &[f64]) -> f64 {
        let margin = if self.trees.is_empty() {
            self.linear_margin(features)
        } else {
            self.base_margin
                + self
                    .trees
                    .iter()
                    .map(|tree| tree.leaf_weight(features))
                    .sum::<f64>()
        };
        self.link.apply(margin)
    }

    /// The margin a LINEAR booster accumulates: its coefficients against the record, plus its
    /// bias, which is the last weight the document carries.
    fn linear_margin(&self, features: &[f64]) -> f64 {
        let (bias, coefficients) = match self.weights.split_last() {
            Some((bias, coefficients)) => (*bias, coefficients),
            None => return self.base_margin,
        };
        self.base_margin
            + bias
            + coefficients
                .iter()
                .zip(features)
                .map(|(weight, value)| weight * value)
                .sum::<f64>()
    }
}

/// The model the serialized booster `bytes` hold, in either of the document's two encodings —
/// text JSON and universal binary JSON — or `None` for bytes that carry no such document.
///
/// Both encodings are decoded by a maintained parser; what this adds is the one reading of the
/// document's own members (see the module docs).
#[must_use]
pub fn boosted_model_from_bytes(bytes: &[u8]) -> Option<BoostedModel> {
    let document = boosted_document(bytes)?;
    boosted_model_from_document(&document)
}

/// The serialized model `bytes` carry as ONE document, whichever of the two encodings it is in.
///
/// The text encoding is parsed by the maintained JSON parser and the binary one by this
/// service's bounded reading of that encoding ([`super::binary_json`]), so both answer the same
/// document and the members below are read once.
fn boosted_document(bytes: &[u8]) -> Option<::serde_json::Value> {
    if let Ok(document) = ::serde_json::from_slice::<::serde_json::Value>(bytes) {
        return Some(document);
    }
    binary_json_document(bytes)
}

/// The model the document `document` describes, or `None` when it is not a serialized booster
/// this reading can take parameters out of.
///
/// `None` rather than a partial model: a document missing the learner, carrying more output
/// groups than one prediction per record can report, or declaring more trees or nodes than this
/// reading serves, holds no model to compute an inference from.
#[must_use]
pub fn boosted_model_from_document(document: &::serde_json::Value) -> Option<BoostedModel> {
    let learner = document.get("learner")?;
    let parameters = learner.get("learner_model_param");
    let objective = learner
        .get("objective")
        .and_then(|objective| objective.get("name"))
        .and_then(::serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let link = ObjectiveLink::of_objective(&objective);
    // The learner's parameters are stated as TEXT in this document, numbers included, so each
    // is read through the one reading below.
    let features = parameters
        .and_then(|parameters| number_member(parameters, "num_feature"))
        .unwrap_or_default()
        .max(0.0) as usize;
    let groups = parameters
        .and_then(|parameters| number_member(parameters, "num_class"))
        .unwrap_or_default();
    // A model with several output groups answers a vector per record rather than one value, so
    // it is not one this reading serves.
    if groups > 1.0 {
        return None;
    }
    let base_score = parameters
        .and_then(|parameters| number_member(parameters, "base_score"))
        .unwrap_or_default();
    let model = learner
        .get("gradient_booster")
        .and_then(|booster| booster.get("model"))?;
    let trees = boosted_trees(model)?;
    let weights = number_list(model.get("weights")).unwrap_or_default();
    // A booster with neither trees nor coefficients holds no parameters at all.
    if trees.is_empty() && weights.is_empty() {
        return None;
    }
    Some(BoostedModel {
        features,
        base_margin: link.margin_of(base_score),
        objective,
        link,
        trees,
        weights,
    })
}

/// The trees `model` carries, and `None` when it declares more trees or nodes than this reading
/// serves.
fn boosted_trees(model: &::serde_json::Value) -> Option<Vec<BoostedTree>> {
    let Some(trees) = model.get("trees").and_then(::serde_json::Value::as_array) else {
        return Some(Vec::new());
    };
    if trees.len() > MAX_BOOSTED_TREES {
        return None;
    }
    let mut read = Vec::with_capacity(trees.len());
    for tree in trees {
        let base_weights = number_list(tree.get("base_weights"))?;
        if base_weights.len() > MAX_TREE_NODES {
            return None;
        }
        read.push(BoostedTree {
            split_indices: number_list(tree.get("split_indices"))?
                .into_iter()
                .map(|index| index.max(0.0) as usize)
                .collect(),
            split_conditions: number_list(tree.get("split_conditions"))?,
            left_children: integer_list(tree.get("left_children"))?,
            right_children: integer_list(tree.get("right_children"))?,
            default_left: integer_list(tree.get("default_left"))?
                .into_iter()
                .map(|value| value != 0)
                .collect(),
            base_weights,
        });
    }
    Some(read)
}

/// One member of the document as a number, whether the document states it as a number or as the
/// text of one: the learner's own parameters are carried as text in this format, and the
/// booster's arrays as numbers.
fn number_member(document: &::serde_json::Value, member: &str) -> Option<f64> {
    number_value(document.get(member)?)
}

/// One value of the document as a number, read the same way whichever form it takes.
fn number_value(value: &::serde_json::Value) -> Option<f64> {
    match value {
        ::serde_json::Value::Number(number) => number.as_f64(),
        ::serde_json::Value::String(text) => text.trim().parse::<f64>().ok(),
        ::serde_json::Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
        _ => None,
    }
}

/// One member of the document as a list of numbers, and `None` when it is a list this reading
/// cannot take numbers out of. An absent member is the empty list.
fn number_list(value: Option<&::serde_json::Value>) -> Option<Vec<f64>> {
    let Some(value) = value else {
        return Some(Vec::new());
    };
    if value.is_null() {
        return Some(Vec::new());
    }
    let items = value.as_array()?;
    if items.len() > MAX_TREE_NODES {
        return None;
    }
    items.iter().map(number_value).collect()
}

/// [`number_list`] as whole numbers, which is what a child index and a direction flag are.
fn integer_list(value: Option<&::serde_json::Value>) -> Option<Vec<i64>> {
    Some(
        number_list(value)?
            .into_iter()
            .map(|value| value as i64)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::{boosted_model_from_bytes, ObjectiveLink};

    /// A serialized booster of one tree splitting feature 0 at `threshold`, with the leaves
    /// `low` and `high` and the objective `objective` — the document this reading takes its
    /// parameters out of, in its text encoding.
    fn booster(objective: &str, base_score: &str, threshold: f64, low: f64, high: f64) -> Vec<u8> {
        ::serde_json::json!({
            "learner": {
                "learner_model_param": {
                    "base_score": base_score,
                    "num_feature": "2",
                    "num_class": "0",
                },
                "objective": { "name": objective },
                "gradient_booster": {
                    "name": "gbtree",
                    "model": {
                        "trees": [{
                            "split_indices": [0, 0, 0],
                            "split_conditions": [threshold, low, high],
                            "left_children": [1, -1, -1],
                            "right_children": [2, -1, -1],
                            "base_weights": [0.0, low, high],
                            "default_left": [1, 0, 0],
                        }],
                    },
                },
            },
        })
        .to_string()
        .into_bytes()
    }

    /// RULE: the prediction is computed from the parameters the artifact's own document carries
    /// — the tree a record walks down and the objective the margin is answered through.
    ///
    /// The rows span what the rule generalises over: the objective family (a logistic one, a
    /// regression one, a count one — the last of which the issue did not cite), the side of the
    /// split a record falls on, and a record shorter than the model's features, which takes the
    /// node's default direction.
    #[test]
    fn a_prediction_is_computed_from_the_documents_own_parameters() {
        // (case, objective, base score, the record, the prediction)
        let rows: [(&str, &str, &str, &[f64], f64); 6] = [
            (
                "a logistic objective, below the split",
                "binary:logistic",
                "5E-1",
                &[0.0, 0.0],
                1.0 / (1.0 + (2.0_f64).exp()),
            ),
            (
                "a logistic objective, above the split",
                "binary:logistic",
                "5E-1",
                &[9.0, 0.0],
                1.0 / (1.0 + (-3.0_f64).exp()),
            ),
            (
                "a squared-error objective answers the margin",
                "reg:squarederror",
                "0",
                &[9.0, 0.0],
                3.0,
            ),
            (
                "a base score the margin starts from",
                "reg:squarederror",
                "1.5",
                &[9.0, 0.0],
                4.5,
            ),
            (
                "a count objective answers a positive quantity",
                "count:poisson",
                "1",
                &[9.0, 0.0],
                (3.0_f64).exp(),
            ),
            (
                "a record shorter than the model's features takes the default direction",
                "reg:squarederror",
                "0",
                &[],
                -2.0,
            ),
        ];
        for (case, objective, base_score, record, expected) in rows {
            let model = boosted_model_from_bytes(&booster(objective, base_score, 5.0, -2.0, 3.0))
                .unwrap_or_else(|| panic!("case {case:?}: the document holds a model"));
            let served = model.predict(record);
            assert!(
                (served - expected).abs() < 1e-9,
                "case {case:?}: expected {expected}, got {served}"
            );
        }
    }

    /// RULE: bytes carrying no serialized booster hold no model, so nothing is served from
    /// them.
    ///
    /// The rows differ along how the bytes fall short: not a document at all, a document of
    /// another kind, a learner with no booster, a booster with neither trees nor coefficients,
    /// and a model with more output groups than one prediction per record can report.
    #[test]
    fn bytes_carrying_no_booster_hold_no_model() {
        let rows: [(&str, Vec<u8>); 5] = [
            ("not a document at all", b"\x00\x01\x02not a model".to_vec()),
            (
                "a document of another kind",
                ::serde_json::json!({ "model": { "weights": [1.0] } })
                    .to_string()
                    .into_bytes(),
            ),
            (
                "a learner with no booster",
                ::serde_json::json!({ "learner": { "objective": { "name": "binary:logistic" } } })
                    .to_string()
                    .into_bytes(),
            ),
            (
                "a booster with no parameters",
                ::serde_json::json!({
                    "learner": {
                        "objective": { "name": "binary:logistic" },
                        "gradient_booster": { "model": { "trees": [] } },
                    },
                })
                .to_string()
                .into_bytes(),
            ),
            (
                "several output groups",
                ::serde_json::json!({
                    "learner": {
                        "learner_model_param": { "num_class": "3", "num_feature": "2" },
                        "objective": { "name": "multi:softprob" },
                        "gradient_booster": {
                            "model": {
                                "trees": [{
                                    "split_indices": [0],
                                    "split_conditions": [0.0],
                                    "left_children": [-1],
                                    "right_children": [-1],
                                    "base_weights": [1.0],
                                    "default_left": [1],
                                }],
                            },
                        },
                    },
                })
                .to_string()
                .into_bytes(),
            ),
        ];
        for (case, bytes) in rows {
            assert!(
                boosted_model_from_bytes(&bytes).is_none(),
                "case {case:?}: bytes carrying no booster must hold no model"
            );
        }
    }

    /// RULE: the objective decides what the accumulated margin is answered through.
    ///
    /// The rows name one objective of each family the algorithm documents, including the
    /// raw-logit one, whose family's other members answer a probability but which answers the
    /// margin itself.
    #[test]
    fn the_objective_decides_the_link_the_margin_is_answered_through() {
        for (case, objective, link) in [
            ("binary classification", "binary:logistic", ObjectiveLink::Logistic),
            ("logistic regression", "reg:logistic", ObjectiveLink::Logistic),
            ("the raw logit", "binary:logitraw", ObjectiveLink::Identity),
            ("the hinge loss", "binary:hinge", ObjectiveLink::Identity),
            ("squared error", "reg:squarederror", ObjectiveLink::Identity),
            ("absolute error", "reg:absoluteerror", ObjectiveLink::Identity),
            ("ranking", "rank:pairwise", ObjectiveLink::Identity),
            ("counts", "count:poisson", ObjectiveLink::Exponential),
            ("gamma", "reg:gamma", ObjectiveLink::Exponential),
            ("tweedie", "reg:tweedie", ObjectiveLink::Exponential),
            ("survival times", "survival:aft", ObjectiveLink::Exponential),
            ("an objective of no family", "", ObjectiveLink::Identity),
        ] {
            let read = ObjectiveLink::of_objective(objective);
            assert_eq!(
                read, link,
                "case {case:?}: expected {link:?} for {objective:?}, got {read:?}"
            );
        }
    }

    /// RULE: a logistic objective answers a PROBABILITY — a value in the closed unit interval —
    /// whatever margin the booster accumulates.
    ///
    /// The rows span the margin's own range: either saturating end, both signs and zero.
    #[test]
    fn a_logistic_objective_answers_a_value_in_the_unit_interval() {
        for (case, margin, expected) in [
            ("a large positive margin", 1.0e9, 1.0),
            ("a large negative margin", -1.0e9, 0.0),
            ("no margin at all", 0.0, 0.5),
            ("the positive infinity", f64::INFINITY, 1.0),
            ("the negative infinity", f64::NEG_INFINITY, 0.0),
        ] {
            let read = ObjectiveLink::Logistic.apply(margin);
            assert!(
                (0.0..=1.0).contains(&read) && (read - expected).abs() < 1e-9,
                "case {case:?}: expected {expected}, got {read}"
            );
        }
    }
}
