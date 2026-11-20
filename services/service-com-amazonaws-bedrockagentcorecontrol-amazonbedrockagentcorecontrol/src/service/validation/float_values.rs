// SPDX-License-Identifier: CC0-1.0

//! How a constraint clause of this service quotes back the value a FLOATING-POINT member
//! held, as rule 8 of [`super`] states it.
//!
//! [`super`] owns the sentence — which clauses a refusal states, the member each names, the
//! requirement each states and whether the value is echoed at all — and this module owns one
//! input to it: the TEXT a submitted floating-point value is echoed with.
//!
//! WHY THERE IS ANYTHING TO DO: the framework's validating walk renders every numeric leaf
//! with Rust's `to_string`, which writes a whole `double` as its integer digits (`-5`,
//! `20000000`), because a violation carries no record of the member's modelled type. A
//! service writes a floating-point value the way it formats one: always with a fractional
//! part or an exponent, and in scientific notation outside the plain-decimal band
//! (`-5.0`, `2.0E7`). The echoed value is the half of the clause a caller matches against
//! what it sent — and parses as a number to find the offending field in a large batch — so
//! it has to be the submitted value in the form the service prints it.
//!
//! WHICH MEMBERS: those the MODEL types as `float` or `double`, read off the generated
//! schemas (`service_common::validation_message::floating_point_member_paths`) rather than
//! named by hand, and keyed by the member path the validating walk reports a violation at.
//! A member schema carries none of its target's own members, so a NESTED shape is listed
//! under the path that binds it, with each collection step written [`ANY_POSITION`]: which
//! members are floating point is a property of the model, never of one request's element
//! count.
//!
//! THE BOUND IS NOT RE-RENDERED. A `@range` requirement states the bound as the model
//! declares it (`… greater than or equal to 0`, `… less than or equal to 10000000`), which
//! is what this service's recorded clauses show and what the framework already writes:
//! only the submitted VALUE is the service's own floating-point rendering.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use aws_smithy_schema::Schema;
use aws_smithy_schema_server::constraints::{self, Violation};

/// The path segment a collection step is written with here: the framework reports a list
/// element under its 1-based POSITION (`entries.1.member.requests.1.member.rate`), while
/// which members are floating point is a property of the model rather than of one request's
/// element count, so one entry stands for every position.
const ANY_POSITION: &str = "*";

/// The segment the framework's walk marks a step from a collection into its element with
/// (`constraints::path::element`), which a listed prefix spells for itself because the
/// position in front of it is [`ANY_POSITION`] rather than one request's index.
const COLLECTION_STEP: &str = "member";

/// `violation` with the value half quoted back the way this service writes a floating-point
/// value, and every other violation — and every member the model does not type as floating
/// point — returned unchanged.
///
/// The CLAUSE stays [`super`]'s: only the rendering of the submitted value moves, so the
/// member path, the requirement and the envelope keep coming from the one place that writes
/// them.
pub(super) fn restated(violation: &Violation) -> Violation {
    service_common::validation_message::in_float_value_rendering(violation, |path| {
        FLOATING_POINT_MEMBERS.contains(&at_any_position(path))
    })
}

/// `path` with every collection position replaced by [`ANY_POSITION`], which is how a
/// listed member path spells one.
fn at_any_position(path: &str) -> String {
    path.split('.')
        .map(|segment| {
            if segment.parse::<usize>().is_ok() {
                ANY_POSITION
            } else {
                segment
            }
        })
        .collect::<Vec<&str>>()
        .join(".")
}

/// The member paths of this service's floating-point members, as a clause names them, with
/// every collection position written [`ANY_POSITION`].
///
/// Read off the MODEL: the shared collector keeps exactly the members whose own compiled
/// schema types them as `float` or `double`, so a member retargeted upstream drops out of
/// the set and keeps the framework's own rendering, which is the fail-safe direction.
static FLOATING_POINT_MEMBERS: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
    let bound: Vec<(String, &'static Schema)> = floating_point_shapes();
    let shapes: Vec<(&str, &'static Schema)> = bound
        .iter()
        .map(|(prefix, schema)| (prefix.as_str(), *schema))
        .collect();
    service_common::validation_message::floating_point_member_paths(&shapes)
});

/// Every shape of this model that declares a floating-point member, under each path that
/// binds it in an operation's input.
///
/// The two families this model constrains a floating-point member in
/// (`smithy_query(query="shape", shape="RateConfig,LimitEntry,BatchPutLimitEntry,\
/// CreateGatewayRateLimitRequest,BatchPutGatewayRateLimitsRequest,SamplingConfig,Rule")`):
///
/// * `RateConfig.rate`, a `double` under `@range(0, 10000000)`, reached from
///   `CreateGatewayRateLimit` and `UpdateGatewayRateLimit` through
///   `entries -> LimitEntry -> {requests, tokens, connections}`, and from
///   `BatchPutGatewayRateLimits` one collection deeper, through
///   `rateLimits -> BatchPutLimitEntry -> entries`;
/// * `SamplingConfig.samplingPercentage`, a `double` under `@range(0.01, 100.0)`, reached
///   from the online-evaluation configuration requests through `rule -> Rule`.
fn floating_point_shapes() -> Vec<(String, &'static Schema)> {
    let entries = element_of(RATE_LIMIT_ENTRIES);
    let batched = format!("{}.{entries}", element_of(BATCH_PUT_RATE_LIMITS));
    let mut shapes: Vec<(String, &'static Schema)> = Vec::new();
    for entry in [entries.clone(), batched] {
        for metric in RATE_METRIC_MEMBERS {
            shapes.push((
                format!("{entry}.{}", element_of(metric)),
                crate::ops::types::RATE_CONFIG_SCHEMA,
            ));
        }
    }
    shapes.push((
        format!(
            "{}.{}",
            constraints::path::member(EVALUATION_RULE),
            constraints::path::member(SAMPLING_CONFIG)
        ),
        crate::ops::types::SAMPLING_CONFIG_SCHEMA,
    ));
    shapes
}

/// The path an ELEMENT of the list member `member` is reported under, at any position.
fn element_of(member: &str) -> String {
    format!(
        "{}.{ANY_POSITION}.{COLLECTION_STEP}",
        constraints::path::member(member)
    )
}

/// The member that carries one rate limit's rule entries, on the single-limit requests and
/// on a batch's own entry alike, and the member that carries the whole batch.
const RATE_LIMIT_ENTRIES: &str = "entries";
const BATCH_PUT_RATE_LIMITS: &str = "rateLimits";

/// The three rate metrics one rule entry configures, each a list of `RateConfig`.
const RATE_METRIC_MEMBERS: [&str; 3] = ["requests", "tokens", "connections"];

/// The member carrying an online-evaluation configuration's rule, and that rule's sampling
/// configuration.
const EVALUATION_RULE: &str = "rule";
const SAMPLING_CONFIG: &str = "samplingConfig";

#[cfg(test)]
mod tests {
    //! What the set has to be true of, read off the model rather than off a request. The
    //! end-to-end half — which operation echoes which text — is
    //! `tests/all/validation_double_echo.rs`.

    use super::*;
    use aws_smithy_schema::ShapeType;
    use aws_smithy_schema_server::constraints::ViolationKind;

    /// Every member a listed shape types as floating point is covered by the set, at the
    /// path each binding reports it under: the rule is about the member's modelled TYPE, so
    /// a shape listed here may not contribute some of its float members and none of the
    /// others.
    #[test]
    fn every_floating_point_member_of_a_listed_shape_is_covered() {
        for (prefix, shape) in floating_point_shapes() {
            let floating: Vec<String> = shape
                .members()
                .iter()
                .filter(|member| {
                    matches!(member.shape_type(), ShapeType::Float | ShapeType::Double)
                })
                .filter_map(|member| member.member_name())
                .map(constraints::path::member)
                .collect();
            assert!(
                !floating.is_empty(),
                "case {prefix:?}: a listed shape declares at least one floating-point \
                 member, and this one declares none"
            );
            for member in floating {
                let path = format!("{prefix}.{member}");
                assert!(
                    FLOATING_POINT_MEMBERS.contains(&path),
                    "case {path:?}: every floating-point member of a listed shape is in the \
                     set, and this one is missing from {FLOATING_POINT_MEMBERS:?}"
                );
            }
        }
    }

    /// The set is keyed at ANY position, so the same member of the second element of a list
    /// is restated exactly as the first element's is.
    #[test]
    fn a_floating_point_value_is_restated_at_every_element_position() {
        // (case, the path the walk reports, the submitted text, what the clause quotes)
        let rows: [(&str, &str, &str, &str); 4] = [
            (
                "a whole value at the first element",
                "entries.1.member.requests.1.member.rate",
                "-5",
                "-5.0",
            ),
            (
                "the same member two elements along",
                "entries.2.member.tokens.3.member.rate",
                "-5",
                "-5.0",
            ),
            (
                "a magnitude above the plain-decimal band",
                "rateLimits.1.member.entries.1.member.connections.1.member.rate",
                "20000000",
                "2.0E7",
            ),
            (
                "a member no collection step leads to",
                "rule.samplingConfig.samplingPercentage",
                "0",
                "0.0",
            ),
        ];
        for (case, path, submitted, expected) in rows {
            let violation = Violation {
                path: path.to_string(),
                kind: ViolationKind::MinValue("0".to_string()),
                value: Some(submitted.to_string()),
                sensitive: false,
            };
            let restated = restated(&violation);
            assert_eq!(
                restated.value.as_deref(),
                Some(expected),
                "case {case:?}: expected the clause to quote {expected:?}, got {:?}",
                restated.value
            );
        }
    }

    /// A member the model does not type as floating point keeps the framework's own
    /// rendering, so an integral member's clause is untouched by this rule.
    #[test]
    fn an_integral_member_keeps_the_frameworks_rendering() {
        let violation = Violation {
            path: "eventExpiryDuration".to_string(),
            kind: ViolationKind::MaxValue("365".to_string()),
            value: Some("400".to_string()),
            sensitive: false,
        };
        let restated = restated(&violation);
        assert_eq!(
            restated.value.as_deref(),
            Some("400"),
            "case \"an integral member\": expected the submitted digits to stand, got {:?}",
            restated.value
        );
    }
}
