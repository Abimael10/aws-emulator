// SPDX-License-Identifier: CC0-1.0

//! Where a measured label is turned into the modelled [`Label`] structure, and the rows of
//! the published general-label taxonomy that structure is filled from.
//!
//! `Label` models SIX members — `Name`, `Confidence`, `Instances`, `Parents`, `Aliases`,
//! `Categories` (`smithy_query(query="shape",shape="Label")`) — and four of them are LISTS.
//! A list member a response has nothing to report for is reported as the EMPTY LIST, never
//! omitted: a client that follows the model deserialises an absent list as null and throws
//! where it should loop zero times, so "nothing to report" and "not reported" are not the
//! same answer. [`reported_label`] is the one constructor every label this service reports
//! goes through, so no reporting path can leave one of them out.
//!
//! The taxonomy itself is published rather than measured: "Categories - The label categories
//! that the detected label belongs to" and "Aliases - Possible Aliases for the label" come
//! from the category and alias lists the Amazon Rekognition Developer Guide publishes for
//! the general-label model, linked from the filters' own documentation ("To see a list of
//! label categories, see …", `smithy_query(query="shape",shape="GeneralLabelsSettings")`).
//! This emulator holds a row for each label it can MEASURE — the category that label belongs
//! to and its ancestors — and holds no alias list at all, so `Aliases` reports the empty
//! list it has nothing to put in rather than a name nothing looked up.

use crate::ops::types::{Instance, Label, LabelCategory, Parent};

/// The category of the labels that describe a person and the parts of one: the row the
/// labels measured by this service's geometric face detector belong to.
pub const PERSON_DESCRIPTION_CATEGORY: &str = "Person Description";

/// The category of the labels that describe the colour and the visual composition of the
/// frame itself: the row the labels measured off the frame's own pixels belong to.
pub const COLORS_CATEGORY: &str = "Colors and Visual Composition";

/// One measured label, as the model shapes it: its name, the confidence measured for it, the
/// category the taxonomy puts it in, the ancestor labels above it and an `Instance` per
/// instance of it located in the frame.
///
/// Every list member is PRESENT: `instances` and `parents` carry the empty list where
/// nothing was located and where the label is a root of the taxonomy, and `Aliases` carries
/// the empty list because this emulator holds no published alias list to report from (see
/// this module's docs) — an empty list is what it has to report, and reporting it is what
/// the model asks for.
pub fn reported_label(
    name: &str,
    confidence: f32,
    category: &str,
    parents: &[&str],
    instances: Vec<Instance>,
) -> Label {
    Label {
        name: Some(name.to_string()),
        confidence: Some(confidence),
        instances: Some(instances),
        parents: Some(
            parents
                .iter()
                .map(|parent| Parent {
                    name: Some((*parent).to_string()),
                })
                .collect(),
        ),
        aliases: Some(Vec::new()),
        categories: Some(vec![LabelCategory {
            name: Some(category.to_string()),
        }]),
    }
}
