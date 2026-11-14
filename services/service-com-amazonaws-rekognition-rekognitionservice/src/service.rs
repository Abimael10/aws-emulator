// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-rekognition-rekognitionservice` service: how it claims requests and how it words the
//! failures the framework hands back.
//!
//! EDIT THIS FILE. It is emitted once with working defaults and is never
//! regenerated, so a choice made here survives adding another service or
//! re-running the generator.
//!
//! `EmulatedService` is what the router mounts (`EmulatedService::new(engine)`).
//! It wraps the generated [`ServiceDefinition`], which carries the model-derived half
//! of this service — its identity, its wire protocol, and its operation routing
//! table — built from the Smithy model at compile time. You do not edit the
//! entry; you edit the behavioral seams below.
//!
//! Five opt-in seams are yours to change; every one defaults to the framework's
//! own behaviour, so an unedited service still routes and answers correctly:
//!
//! - `can_handle` — which requests this service claims. The default matches the
//!   request's resolved route key against this service's signing name. Edit it
//!   for anything the model does not express: a signing name shared by several
//!   services, or routing that keys on the access-key id.
//! - `override_operation` — serve a request the model's routing key cannot name
//!   correctly. The default returns `None`, so every request resolves through the
//!   generated routing table. Return `Some(op)` to route one yourself.
//! - `override_refusal` — how this service words a request the framework refused
//!   before any handler ran. This one is edited: it states the refusal in AWS's own
//!   envelope through the shared statement of it, with a FLOATING-POINT member's
//!   submitted value quoted back the way the service writes one (`101.0`, not `101`).
//!   See the inline note on that method.
//! - `override_routing_error` — how this service words a request that reached it
//!   but named no modeled operation. The default returns `None`, so the entry
//!   renders the protocol-correct default (query -> 400 InvalidAction, awsJson ->
//!   404, …). Return `Some(resp)` to answer in your own envelope if desired.
//! - `override_framework_error` — how this service words a transport-level
//!   failure the orchestrator detects up front (e.g. a malformed `aws-chunked`
//!   body). The default returns `None`, keeping the entry's protocol-correct
//!   rendering. Return `Some(resp)` to reword it if desired.
//!
//! The model-derived half of this service — its identity, wire protocol, routing
//! table, and the default error renderers — lives in the generated `ServiceDefinition`
//! and is reached, not edited, through the one `definition` method below. You
//! do not write it; editing the entry would let this file drift from the model.

use aws_smithy_schema::Schema;
use smithy_server_runtime::{
    http, ErasedOperation, Refusal, Refused, Service,
};
use std::collections::BTreeSet;
use std::sync::{Arc, LazyLock};

/// Each shape a constraint-violation path of this service can name a FLOATING-POINT member
/// of, with the path prefix the validating walk reports that shape's members under (the
/// empty string for an operation input's own members).
///
/// This is the LIST of shapes to look in, not a list of members: which of their members are
/// floating point comes from the model
/// (`service_common::validation_message::floating_point_member_paths`). The shapes listed
/// are the ones reachable from an operation's INPUT whose float members declare a `@range`
/// (`smithy_query(query="shape", shape="Percent,SegmentConfidence,MaxPixelThreshold,\
/// MinCoveragePercentage")`: `Percent` 0-100 on the confidence and threshold members,
/// `SegmentConfidence` 50-100 on the segment filters, `MaxPixelThreshold` 0-1 and
/// `MinCoveragePercentage` 0-100 on the black-frame filter, `BoundingBoxHeight` /
/// `BoundingBoxWidth` 0-1 on the word filter), since a float member under no bound can
/// never be the subject of a clause. Codegen emits a member schema WITHOUT its target's own
/// members (`Schema::new_member` takes a shape type and a name), so a NESTED shape is named
/// here under the member path that binds it — the same path for two operations that bind it
/// alike. An operation this model grows, or a float member put under a `@range`, therefore
/// needs a line here; `every_bounded_float_member_is_covered` pins the set against the
/// model.
const VALIDATED_SHAPES: &[(&str, &Schema)] = &[
    ("", crate::ops::types::ASSOCIATE_FACES_INPUT_SCHEMA),
    ("", crate::ops::types::COMPARE_FACES_INPUT_SCHEMA),
    ("", crate::ops::types::DETECT_CUSTOM_LABELS_INPUT_SCHEMA),
    ("", crate::ops::types::DETECT_LABELS_INPUT_SCHEMA),
    ("", crate::ops::types::DETECT_MODERATION_LABELS_INPUT_SCHEMA),
    ("", crate::ops::types::SEARCH_FACES_BY_IMAGE_INPUT_SCHEMA),
    ("", crate::ops::types::SEARCH_FACES_INPUT_SCHEMA),
    ("", crate::ops::types::SEARCH_USERS_BY_IMAGE_INPUT_SCHEMA),
    ("", crate::ops::types::SEARCH_USERS_INPUT_SCHEMA),
    ("", crate::ops::types::START_CONTENT_MODERATION_INPUT_SCHEMA),
    ("", crate::ops::types::START_FACE_SEARCH_INPUT_SCHEMA),
    ("", crate::ops::types::START_LABEL_DETECTION_INPUT_SCHEMA),
    (
        "summarizationAttributes",
        crate::ops::types::PROTECTIVE_EQUIPMENT_SUMMARIZATION_ATTRIBUTES_SCHEMA,
    ),
    // The word filter of both text operations, which bind it under the same member.
    ("filters.wordFilter", crate::ops::types::DETECTION_FILTER_SCHEMA),
    (
        "filters.shotFilter",
        crate::ops::types::START_SHOT_DETECTION_FILTER_SCHEMA,
    ),
    (
        "filters.technicalCueFilter",
        crate::ops::types::START_TECHNICAL_CUE_DETECTION_FILTER_SCHEMA,
    ),
    (
        "filters.technicalCueFilter.blackFrame",
        crate::ops::types::BLACK_FRAME_SCHEMA,
    ),
    (
        "settings.faceSearch",
        crate::ops::types::FACE_SEARCH_SETTINGS_SCHEMA,
    ),
    (
        "settings.connectedHome",
        crate::ops::types::CONNECTED_HOME_SETTINGS_SCHEMA,
    ),
    (
        "settingsForUpdate.connectedHomeForUpdate",
        crate::ops::types::CONNECTED_HOME_SETTINGS_FOR_UPDATE_SCHEMA,
    ),
    (
        "operationsConfig.detectModerationLabels",
        crate::ops::types::MEDIA_ANALYSIS_DETECT_MODERATION_LABELS_CONFIG_SCHEMA,
    ),
    (
        "featureConfig.contentModeration",
        crate::ops::types::CUSTOMIZATION_FEATURE_CONTENT_MODERATION_CONFIG_SCHEMA,
    ),
];

/// The validation paths of the members this service's MODEL types as `float`.
///
/// Built once off the schemas themselves, through the one shared reader of that set
/// (`service_common::validation_message::floating_point_member_paths`), so nothing here
/// names a member: a path is in this set exactly when the model says that member is
/// floating point.
fn floating_point_member_paths() -> &'static BTreeSet<String> {
    static PATHS: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
        service_common::validation_message::floating_point_member_paths(VALIDATED_SHAPES)
    });
    &PATHS
}

/// `refusal` with every clause about a FLOATING-POINT member quoting the submitted value
/// the way this service writes one, and everything else untouched.
///
/// A clause states the value it was handed, and the framework renders every numeric leaf
/// with Rust's decimal expansion, which has no way to know the member it is writing is a
/// floating-point one: a whole `101` submitted for a float member reads `101`, where the
/// service writes `101.0`. The rendering is the one shared implementation in
/// `service_common`, so this service cannot drift from the others; which members it applies
/// to comes from the model ([`floating_point_member_paths`]).
fn in_float_value_rendering(refusal: &Refusal) -> Refusal {
    let Refusal::Invalid(violations) = refusal else {
        return refusal.clone();
    };
    Refusal::Invalid(
        violations
            .iter()
            .map(|violation| {
                service_common::validation_message::in_float_value_rendering(violation, |path| {
                    floating_point_member_paths().contains(path)
                })
            })
            .collect(),
    )
}

/// The `com-amazonaws-rekognition-rekognitionservice` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(engine),
        }
    }
}

impl<Eng: Send + Sync + 'static> Service for EmulatedService<Eng> {
    fn definition(&self) -> &dyn smithy_server_runtime::ErasedServiceDefinition {
        // The model-derived half of this service — identity, protocol, routing
        // table, and the default error renderers — lives in the generated entry.
        // The router reads it through this one accessor. Do not edit the entry.
        &self.entry
    }

    fn can_handle(&self, parts: &http::request::Parts) -> bool {
        // Claim a request whose resolved route key is this service's signing
        // name. Edit this for anything the model does not express — a signing
        // name shared by several services, or access-key-id-based routing.
        match smithy_server_runtime::resolve_route_key(parts) {
            Some(key) => key == self.entry.identity().signing_name(),
            None => false,
        }
    }

    fn override_operation(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<&dyn ErasedOperation> {
        // Return `Some(op)` to serve a request the model's routing key cannot name
        // correctly; `None` (the default) lets the generated routing table resolve
        // it. `req` is the whole request, so a decision that depends on the body —
        // the `Action` form field on a query protocol, say — is reachable here.
        let _ = req;
        None
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) before any handler
        // runs. This service is `awsJson1_1`, where AWS answers such a refusal as
        // `400 ValidationException` with the `N validation error(s) detected: …`
        // envelope the framework already renders — which is why this delegates to the
        // shared statement of that envelope rather than wording anything itself.
        //
        // ONE rendering inside it is this service's: a FLOATING-POINT member's
        // submitted value is quoted back the way the service writes one — with its
        // fractional part, so a whole `101` for a float member reads `101.0`
        // (`in_float_value_rendering`). Everything else is the shared statement's,
        // including how a `@pattern` requirement is quoted: AWS publishes the
        // expression the model then wraps in `^`/`$` to turn a find into a whole-value
        // match, so the quoted expression drops that pair — the double-wrapped patterns
        // of this model (`^(^arn:…$)$`) keep their inner anchors, which is the form a
        // recorded refusal of such a member quotes. The CHECK uses the model's pattern
        // as written either way.
        service_common::validation_message::aws_validation_refusal(&in_float_value_rendering(
            refusal,
        ))
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no
        // modeled operation — neither `override_operation` nor the generated
        // routing table resolved it. Returning `None` renders the protocol-correct
        // default for this service's envelope (query -> 400 InvalidAction text/xml,
        // awsJson -> 404, rpcv2Cbor -> 404, rest -> 501). Return `Some(resp)` to
        // word that routing failure in your own envelope instead.
        let _ = req;
        None
    }

    fn override_framework_error(
        &self,
        error: &aws_smithy_schema_server::ProtocolError,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this on a transport-level failure it detects up front —
        // before (or instead of) naming an operation — such as a malformed
        // `aws-chunked` body. Returning `None` keeps the entry's protocol-correct
        // rendering. Return `Some(resp)` to reword that failure in your own
        // envelope. The error is borrowed, so the default renderer can still
        // consume it on `None`.
        let _ = error;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::floating_point_member_paths;

    /// The model this crate was generated from, which ships beside it.
    const MODEL_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-rekognition-rekognitionservice.json"
    );

    /// RULE applied to the whole surface: every FLOATING-POINT member a constraint clause
    /// can be stated about — one reachable from an operation's input and declaring a
    /// `@range` — is covered by [`floating_point_member_paths`] AT THE PATH the validating
    /// walk reports it under, so its clause quotes the submitted value in the member's
    /// modelled type.
    ///
    /// The paths come from the MODEL rather than from a second hand-written list: a float
    /// member added to an input shape, a `@range` put on one that had none, or a nested
    /// shape bound under a member [`VALIDATED_SHAPES`] does not name, fails here instead of
    /// quietly having its clause state an integer. The comparison is on the WHOLE path, not
    /// on the member's own name, because this model names many distinct members
    /// `MinConfidence`: one covered `…minConfidence` must not stand for another's.
    #[test]
    fn every_bounded_float_member_is_covered() {
        let model = std::fs::read_to_string(MODEL_PATH)
            .expect("the model that generated this crate ships beside it");
        let model: serde_json::Value =
            serde_json::from_str(&model).expect("the model that generated this crate is JSON");
        let shapes = model["shapes"]
            .as_object()
            .expect("a Smithy model declares shapes");

        // Walk out from every operation's INPUT, since a clause names a member of a
        // request, carrying the path the walk reports that shape's members under. A
        // member's own traits or its target shape's may carry the `@range`.
        let mut pending: Vec<(&str, String)> = shapes
            .iter()
            .filter(|(_, shape)| shape["type"] == "operation")
            .filter_map(|(_, shape)| shape["input"]["target"].as_str())
            .map(|target| (target, String::new()))
            .collect();
        let mut visited: std::collections::BTreeSet<(&str, String)> =
            std::collections::BTreeSet::new();
        let mut bounded_floats: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        while let Some((id, prefix)) = pending.pop() {
            if !visited.insert((id, prefix.clone())) {
                continue;
            }
            let Some(shape) = shapes.get(id) else {
                continue;
            };
            // One step deeper, under `segment`.
            let under = |segment: &str| {
                if prefix.is_empty() {
                    segment.to_string()
                } else {
                    format!("{prefix}.{segment}")
                }
            };
            match shape["type"].as_str() {
                Some("structure" | "union") => {
                    let Some(members) = shape["members"].as_object() else {
                        continue;
                    };
                    for (name, member) in members {
                        let Some(target) = member["target"].as_str() else {
                            continue;
                        };
                        // The casing of a path segment is the framework's; a member names
                        // itself in lowerCamel.
                        let mut segment = name.chars();
                        let Some(first) = segment.next() else {
                            continue;
                        };
                        let path = under(&first.to_lowercase().chain(segment).collect::<String>());
                        pending.push((target, path.clone()));
                        let Some(targeted) = shapes.get(target) else {
                            continue;
                        };
                        let is_float =
                            matches!(targeted["type"].as_str(), Some("float" | "double"));
                        let bounded = !member["traits"]["smithy.api#range"].is_null()
                            || !targeted["traits"]["smithy.api#range"].is_null();
                        if is_float && bounded {
                            bounded_floats.insert(path);
                        }
                    }
                }
                // A collection step names the element's 1-based POSITION, which belongs to
                // one request rather than to the model, so it is carried as `*`: no bounded
                // float sits under a collection in this model, and one added there arrives
                // here as a path no catalog entry can match, which is the right failure.
                Some("list" | "set") => {
                    if let Some(target) = shape["member"]["target"].as_str() {
                        pending.push((target, under("*.member")));
                    }
                }
                Some("map") => {
                    for slot in ["key", "value"] {
                        if let Some(target) = shape[slot]["target"].as_str() {
                            pending.push((target, under("*.member")));
                        }
                    }
                }
                _ => {}
            }
        }

        assert!(
            !bounded_floats.is_empty(),
            "case the model's surface: this service's inputs declare float members under a \
             @range, so finding none means this walk reads the model wrongly"
        );
        let covered = floating_point_member_paths();
        for path in &bounded_floats {
            assert!(
                covered.contains(path),
                "case {path:?}: the model declares this float member under a @range at this \
                 path, so a clause about it must quote its value as a float; no path in \
                 {covered:?} is it"
            );
        }
    }
}
