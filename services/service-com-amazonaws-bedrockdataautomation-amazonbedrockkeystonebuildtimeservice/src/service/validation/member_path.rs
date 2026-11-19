// SPDX-License-Identifier: CC0-1.0

//! The member path a boundary refusal of this service NAMES: the model's own traversal
//! from the operation's input to the offending member.
//!
//! THE RULE: the path a refusal states is that traversal spelled out — one segment per
//! structure AND per union member name, and a collection step for each element the
//! traversal passed through — so every segment of it is a member the request's shape
//! really has, and a client can map the path back onto its own payload.
//!
//! WHY THIS MODULE EXISTS: the two layers that build a path do not build it alike. The
//! validating walk descends into a value it already read and names every member it
//! passes, union members included. An ABSENT required member, though, is found while the
//! request is being READ, and the path is assembled as that failure travels back out, each
//! enclosing member prepending itself — and a union prepends nothing, because the read of
//! a union's one variant is not entered through a member of its own
//! (`vendor/smithy-macros`, read-only from here). A required member absent under a union
//! variant is therefore reported at a path that descends from a union straight into what
//! the variant targets: a path the request's shape does not have.
//!
//! What this module does about it is READ the reported path back against the model: every
//! segment of it is matched against the shape graph from an operation's input inwards, and
//! a union crossed without a segment of its own contributes the variant that makes the
//! REST of the path resolve. A path that already resolves is returned unchanged, and so is
//! one that the model reads more than one way — the model is what decides, never the
//! request.
//!
//! Nothing here is keyed to an operation, a member or a submitted value: the rule is stated
//! against this service's whole model, so it holds for every refusal of every operation of
//! it, at whatever depth and through whatever shapes the traversal ran.

use std::collections::BTreeSet;

use aws_smithy_schema_server::constraints::path;
use serde_json::{Map, Value};

/// How many union variants one reading may cross without a segment of its own. A path
/// crosses one per union between the input and the offending member; the budget only stops
/// a self-referential union from being descended forever.
const MAX_VARIANTS_CROSSED: usize = 8;

/// How many readings of one path are collected before it is left alone. A path the model
/// reads two ways is already ambiguous, so nothing past the second decides anything.
const MAX_READINGS: usize = 4;

/// One Smithy model's shape graph, as the paths of its operations' inputs are read against
/// it.
///
/// Built once per service from the model text (a compile-time constant, so the answer
/// cannot change while the process runs) and asked per refusal.
#[derive(Debug, Clone, Default)]
pub struct ModelledPaths {
    /// Every shape of the model, by shape id.
    shapes: Map<String, Value>,
    /// The shapes this model's operations read a request INTO — where every member path of
    /// a refusal starts. Which operation was called is not known at the refusal seam, so a
    /// path is read against all of them and only a reading they AGREE on is used.
    inputs: Vec<String>,
}

impl ModelledPaths {
    /// The shape graph `model_json` declares.
    ///
    /// A model that cannot be read leaves every path alone, which keeps the framework's own
    /// spelling — the fail-safe direction, since that spelling is what a refusal states
    /// without this seam at all.
    #[must_use]
    pub fn from_model(model_json: &str) -> Self {
        let model: Value = serde_json::from_str(model_json).unwrap_or(Value::Null);
        let Some(shapes) = model.get("shapes").and_then(Value::as_object) else {
            return Self::default();
        };
        let mut inputs: Vec<String> = shapes
            .values()
            .filter(|shape| shape.get("type").and_then(Value::as_str) == Some("operation"))
            .filter_map(|operation| target_of(operation.get("input")))
            .map(str::to_string)
            .collect();
        inputs.sort();
        inputs.dedup();
        Self {
            shapes: shapes.clone(),
            inputs,
        }
    }

    /// `reported` as the model traverses it: the one reading of it the shape graph admits,
    /// or `reported` itself when the graph admits none or several.
    #[must_use]
    pub fn modelled(&self, reported: &str) -> String {
        let segments: Vec<&str> = reported.split('.').collect();
        let mut readings: BTreeSet<String> = BTreeSet::new();
        for input in &self.inputs {
            self.read(
                input,
                &segments,
                &mut Vec::new(),
                MAX_VARIANTS_CROSSED,
                &mut readings,
            );
        }
        let mut readings = readings.into_iter();
        match (readings.next(), readings.next()) {
            (Some(only), None) => only,
            _ => reported.to_string(),
        }
    }

    /// Read `segments` against the shape `shape_id` names, collecting into `readings` the
    /// full path of every traversal that consumes all of them. `walked` is the traversal so
    /// far, and `variants` the budget of [`MAX_VARIANTS_CROSSED`].
    fn read(
        &self,
        shape_id: &str,
        segments: &[&str],
        walked: &mut Vec<String>,
        variants: usize,
        readings: &mut BTreeSet<String>,
    ) {
        if readings.len() >= MAX_READINGS {
            return;
        }
        let Some(shape) = self.shapes.get(shape_id) else {
            return;
        };
        // Every segment placed: this traversal is one reading of the reported path. A path
        // ends at the offending member itself, whatever kind of shape that member targets.
        if segments.is_empty() {
            readings.insert(path::join(walked));
            return;
        }
        match shape.get("type").and_then(Value::as_str) {
            Some(kind @ ("structure" | "union")) => {
                let Some(members) = shape.get("members").and_then(Value::as_object) else {
                    return;
                };
                for (name, member) in members {
                    let Some(target) = target_of(Some(member)) else {
                        continue;
                    };
                    let named = path::member(name);
                    // The member the next segment names.
                    if named == segments[0] {
                        walked.push(named.clone());
                        self.read(target, &segments[1..], walked, variants, readings);
                        walked.pop();
                    }
                    // A union crossed with no segment of its own: this variant is the
                    // segment the read dropped, so it is placed without consuming one.
                    if kind == "union" && variants > 0 {
                        walked.push(named);
                        self.read(target, segments, walked, variants - 1, readings);
                        walked.pop();
                    }
                }
            }
            // A collection step: the element's 1-based position, or the entry's key,
            // followed by the step into the collection — spelled by the framework rather
            // than restated here, so the two cannot drift.
            Some("list" | "set") => {
                let Some(position) = segments.first().and_then(|first| first.parse::<usize>().ok())
                else {
                    return;
                };
                self.read_collection(
                    shape.get("member"),
                    &path::element(position),
                    segments,
                    walked,
                    variants,
                    readings,
                );
            }
            Some("map") => {
                let Some(first) = segments.first() else {
                    return;
                };
                self.read_collection(
                    shape.get("value"),
                    &path::entry(first),
                    segments,
                    walked,
                    variants,
                    readings,
                );
            }
            // A leaf with segments still to place names nothing: this traversal is no
            // reading of the reported path.
            _ => {}
        }
    }

    /// Read `segments` on from a collection whose step into its element or value is `step`,
    /// a two-segment spelling the framework owns.
    fn read_collection(
        &self,
        element: Option<&Value>,
        step: &str,
        segments: &[&str],
        walked: &mut Vec<String>,
        variants: usize,
        readings: &mut BTreeSet<String>,
    ) {
        let (Some(target), [first, second, rest @ ..]) = (target_of(element), segments) else {
            return;
        };
        if step != path::join(&[(*first).to_string(), (*second).to_string()]) {
            return;
        }
        walked.push((*first).to_string());
        walked.push((*second).to_string());
        self.read(target, rest, walked, variants, readings);
        walked.pop();
        walked.pop();
    }
}

/// The shape id a member or an operation's input node targets.
fn target_of(node: Option<&Value>) -> Option<&str> {
    node?.get("target").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This service's own model: the graph the paths below are read against.
    fn modelled_paths() -> ModelledPaths {
        ModelledPaths::from_model(super::super::MODEL_JSON)
    }

    /// RULE: a path that crossed a union is read back as the model traverses it — the
    /// variant's own member name among the segments — while a path that already names every
    /// member of its traversal is left exactly as it is.
    ///
    /// Sources: `smithy_query(query="closure",
    /// operation="InvokeDataAutomationLibraryIngestionJob")` for the union chain
    /// (`InputConfiguration.inlinePayload` -> `InlinePayload` = union{`upsertEntitiesInfo`
    /// -> list of `UpsertEntityInfo` = union{`vocabulary` -> `VocabularyEntityInfo`},
    /// `deleteEntitiesInfo` -> `DeleteEntitiesInfo`}), and
    /// `smithy_query(query="operation", operation="CreateDataAutomationLibrary")` for the
    /// paths that cross none.
    #[test]
    fn a_path_is_read_as_the_model_traverses_it() {
        let paths = modelled_paths();
        // (case, the path the read reports, the path the model traverses)
        let rows: [(&str, &str, &str); 10] = [
            (
                "a member under a union, a list and a second union",
                "inputConfiguration.inlinePayload.1.member.phrases",
                "inputConfiguration.inlinePayload.upsertEntitiesInfo.1.member.vocabulary.phrases",
            ),
            (
                "another element of the same list",
                "inputConfiguration.inlinePayload.7.member.language",
                "inputConfiguration.inlinePayload.upsertEntitiesInfo.7.member.vocabulary.language",
            ),
            (
                "a member of a list nested under the variant",
                "inputConfiguration.inlinePayload.2.member.phrases.3.member.text",
                "inputConfiguration.inlinePayload.upsertEntitiesInfo.2.member.vocabulary.phrases.\
                 3.member.text",
            ),
            (
                "the other variant of the same union, which targets a structure",
                "inputConfiguration.inlinePayload.entityIds",
                "inputConfiguration.inlinePayload.deleteEntitiesInfo.entityIds",
            ),
            (
                "a path that already names its variants is unchanged",
                "inputConfiguration.inlinePayload.upsertEntitiesInfo.1.member.vocabulary.phrases",
                "inputConfiguration.inlinePayload.upsertEntitiesInfo.1.member.vocabulary.phrases",
            ),
            (
                "a path crossing no union: a nested structure member",
                "outputConfiguration.s3Uri",
                "outputConfiguration.s3Uri",
            ),
            (
                "a path crossing no union: two levels of structure",
                "notificationConfiguration.eventBridgeConfiguration.eventBridgeEnabled",
                "notificationConfiguration.eventBridgeConfiguration.eventBridgeEnabled",
            ),
            (
                "a path crossing no union: a member of a list element",
                "tags.2.member.value",
                "tags.2.member.value",
            ),
            ("a top-level member", "libraryName", "libraryName"),
            (
                "a member no shape of this model declares",
                "somethingElse.notAMember",
                "somethingElse.notAMember",
            ),
        ];
        for (case, reported, expected) in rows {
            let actual = paths.modelled(reported);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the model decides, so a path it cannot read, or reads more than one way, is
    /// left exactly as reported rather than guessed at.
    #[test]
    fn a_path_the_model_cannot_place_is_left_alone() {
        // A union whose two variants both admit the same continuation: no reading of
        // `label` is the model's, so neither is chosen.
        const AMBIGUOUS: &str = r#"{
            "smithy": "2.0",
            "shapes": {
                "example#Put": {
                    "type": "operation",
                    "input": { "target": "example#PutRequest" }
                },
                "example#PutRequest": {
                    "type": "structure",
                    "members": { "body": { "target": "example#Body" } }
                },
                "example#Body": {
                    "type": "union",
                    "members": {
                        "first": { "target": "example#First" },
                        "second": { "target": "example#Second" }
                    }
                },
                "example#First": {
                    "type": "structure",
                    "members": { "label": { "target": "smithy.api#String" } }
                },
                "example#Second": {
                    "type": "structure",
                    "members": { "label": { "target": "smithy.api#String" } }
                }
            }
        }"#;
        let paths = ModelledPaths::from_model(AMBIGUOUS);
        let actual = paths.modelled("body.label");
        assert_eq!(
            actual, "body.label",
            "case \"two variants admit the same member\": expected the reported path, got \
             {actual:?}"
        );
        let placed = paths.modelled("body.first.label");
        assert_eq!(
            placed, "body.first.label",
            "case \"the variant already named\": expected the reported path, got {placed:?}"
        );
    }

    /// RULE: a model that cannot be read leaves every path alone, so the framework's own
    /// spelling stands rather than one mangled by accident.
    #[test]
    fn an_unreadable_model_leaves_every_path_alone() {
        for (case, model) in [
            ("not JSON at all", "{"),
            ("JSON carrying no shapes", r#"{"smithy":"2.0"}"#),
            ("an empty document", ""),
        ] {
            let paths = ModelledPaths::from_model(model);
            let actual = paths.modelled("inputConfiguration.inlinePayload.1.member.phrases");
            assert_eq!(
                actual, "inputConfiguration.inlinePayload.1.member.phrases",
                "case {case:?}: expected the reported path, got {actual:?}"
            );
        }
    }
}
