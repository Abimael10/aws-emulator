// SPDX-License-Identifier: CC0-1.0

//! How this service STATES the refusal of a value outside a modelled constraint: which
//! member path the clause names, and how the rejected value is quoted back.
//!
//! # The rule
//!
//! A refused request is answered with the standard constraint report — `<N> validation
//! error(s) detected: ` over one clause per violated member, each
//! `Value '<value>' at '<member path>' failed to satisfy constraint: <requirement>`. The
//! envelope, the requirement and the bound are the model's, and the framework's own walk
//! builds them; two parts of a clause are not derivable from the model:
//!
//! 1. the member path. The walk names the member as the model spells it, in lowerCamel.
//!    For most members that is what the frontend reports, but not for every one: a
//!    frontend member whose public name differs from the internal one it validates is
//!    reported under the name the service validates, so a client parsing the clause to
//!    learn which member to correct is pointed at that one. That rename is the one thing
//!    this module owns ([`REPORTED_MEMBER_PATHS`]).
//! 2. the rendering of the rejected value, which the framework writes with Rust's
//!    decimal expansion — dropping a float's fractional part when it is zero (`150` for a
//!    submitted `150.0`). WHICH members are floating point is read off this service's own
//!    compiled schemas and HOW such a value is quoted back is the one shared rendering,
//!    both through `service_common::validation_message` ([`stated`]): neither is restated
//!    here.
//!
//! Both are properties of a MEMBER, so they are applied by every refusal of every
//! operation through [`report`], rather than in the handler of whichever operation a
//! divergence was first seen on: the same member is validated on `CreateEndpointGroup`
//! and on `UpdateEndpointGroup`, and both legs must state the same clause.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use smithy_server_runtime::Violation;

/// Every input member of this service the frontend reports under a path other than the
/// one the model-driven walk builds for it, as `(the walk's path, the reported path)`.
///
/// Recorded wire behaviour: a `TrafficDialPercentage` past its maximum is refused with a
/// clause naming `trafficRetentionPercentage`, the member this service validates the
/// percentage as. Every other member is absent from this table deliberately — the walk's
/// own path is what the frontend reports for it.
const REPORTED_MEMBER_PATHS: [(&str, &str); 1] =
    [("trafficDialPercentage", "trafficRetentionPercentage")];

/// The shapes a violation of this service can name a floating-point member of: the two
/// operation inputs the model types such a member on.
///
/// `TrafficDialPercentage` (`smithy_query(query="shape",
/// shape="TrafficDialPercentage")`: type `float`, `@range` 0..100) is the only
/// floating-point member in this service's whole model — the generated types carry no
/// other `f32`/`f64` — and `CreateEndpointGroup` and `UpdateEndpointGroup` are the two
/// operations whose input carries it. The paths themselves are never named here: they are
/// read off these schemas, so a member the model types as floating point is covered by
/// having been modelled ([`floating_point_member_paths`], pinned against the schemas by
/// this module's own test).
const VALIDATED_SHAPES: &[(&str, &::aws_smithy_schema::Schema)] = &[
    (
        "",
        crate::ops::types::CREATE_ENDPOINT_GROUP_INPUT_SCHEMA,
    ),
    (
        "",
        crate::ops::types::UPDATE_ENDPOINT_GROUP_INPUT_SCHEMA,
    ),
];

/// The validation paths of the members this service's MODEL types as floating point.
///
/// Built once off the schemas themselves, through the one shared reader of that set, so
/// nothing here names a member: a path is in this set exactly when the model says that
/// member is floating point.
fn floating_point_member_paths() -> &'static BTreeSet<String> {
    static PATHS: LazyLock<BTreeSet<String>> =
        LazyLock::new(|| service_common::validation_message::floating_point_member_paths(
            VALIDATED_SHAPES,
        ));
    &PATHS
}

/// The member of the request a clause is about: the head of the dotted path the walk
/// reports, since a clause about something nested inside a member (`tags.1.member.key`)
/// is still a clause about that member.
fn member_of(path: &str) -> &str {
    path.split('.').next().unwrap_or_default()
}

/// The path this service reports `path` under, which differs from the model's own
/// spelling only for a member of [`REPORTED_MEMBER_PATHS`]. A clause about something
/// nested inside such a member keeps the rest of its trail.
fn reported_path(path: &str) -> Option<String> {
    let member = member_of(path);
    let (_, reported) = REPORTED_MEMBER_PATHS
        .iter()
        .find(|(modelled, _)| *modelled == member)?;
    Some(match path.split_once('.') {
        Some((_, rest)) => format!("{reported}.{rest}"),
        None => (*reported).to_string(),
    })
}

/// `violation` as this service states it: the same requirement and bound, over the member
/// path this service reports and the value rendering it writes a floating-point member
/// with.
///
/// The rendering is the shared one in `service_common`, applied to the members the model
/// types as floating point, so this service cannot drift from the others; the path rename
/// is applied AFTER it, because the set it is keyed on is the model's own spelling of the
/// member.
fn stated(violation: &Violation) -> Violation {
    let rendered = service_common::validation_message::in_float_value_rendering(
        violation,
        |path| floating_point_member_paths().contains(path),
    );
    match reported_path(&rendered.path) {
        Some(path) => Violation { path, ..rendered },
        None => rendered,
    }
}

/// The constraint report this service answers `violations` with: the framework's own
/// envelope over the clauses, each stated the way this service states it ([`stated`]).
///
/// `None` when there is nothing to state, which is what the envelope answers for an empty
/// set of violations.
pub fn report(violations: &[Violation]) -> Option<String> {
    let stated: Vec<Violation> = violations.iter().map(stated).collect();
    ::aws_smithy_schema_server::envelope(&stated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithy_server_runtime::ViolationKind;

    /// One violation at `path` holding `value`, of the shape the walk reports for a
    /// `@range` maximum.
    fn past_the_maximum(path: &str, value: &str, max: &str) -> Violation {
        Violation {
            path: path.to_owned(),
            kind: ViolationKind::MaxValue(max.to_owned()),
            value: Some(value.to_owned()),
            sensitive: false,
        }
    }

    /// The floating-point members are the model's: the set read off this service's
    /// schemas is exactly the one member the model types as a float, so nothing in this
    /// module decides which members those are.
    #[test]
    fn the_floating_point_members_come_from_the_model() {
        let paths: Vec<&str> = floating_point_member_paths()
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(
            paths,
            vec!["trafficDialPercentage"],
            "expected the model's own floating-point members, got {paths:?}"
        );
    }

    /// RULE: the clause names the member path this service reports and quotes a
    /// floating-point member's rejected value the way this service writes one; every
    /// other member keeps the path and the rendering the model-driven walk built.
    #[test]
    fn a_clause_states_the_member_path_and_value_rendering_this_service_reports() {
        // (case, the walk's path, the walk's value, the clause this service states)
        let rows: [(&str, &str, &str, &str); 7] = [
            (
                "the float member, a whole rejected value",
                "trafficDialPercentage",
                "150",
                "Value '150.0' at 'trafficRetentionPercentage'",
            ),
            (
                "the float member, a fractional rejected value",
                "trafficDialPercentage",
                "100.5",
                "Value '100.5' at 'trafficRetentionPercentage'",
            ),
            (
                "the float member, a negative rejected value",
                "trafficDialPercentage",
                "-1",
                "Value '-1.0' at 'trafficRetentionPercentage'",
            ),
            (
                // Outside the band a value is quoted as a plain decimal in, the shared
                // rendering writes the scientific form the service writes.
                "the float member, a magnitude past the plain-decimal band",
                "trafficDialPercentage",
                "12345678",
                "Value '1.2345678E7' at 'trafficRetentionPercentage'",
            ),
            (
                "an integer member",
                "healthCheckIntervalSeconds",
                "31",
                "Value '31' at 'healthCheckIntervalSeconds'",
            ),
            (
                "another integer member",
                "maxResults",
                "101",
                "Value '101' at 'maxResults'",
            ),
            (
                "a member nested inside a list",
                "portOverrides.1.member.listenerPort",
                "70000",
                "Value '70000' at 'portOverrides.1.member.listenerPort'",
            ),
        ];

        for (case, path, value, expected) in rows {
            let stated = report(&[past_the_maximum(path, value, "100")])
                .unwrap_or_else(|| panic!("case {case:?}: expected a report"));
            assert!(
                stated.starts_with(&format!("1 validation error detected: {expected} ")),
                "case {case:?}: expected the clause to open {expected:?}, got {stated:?}"
            );
            assert!(
                stated.ends_with(
                    "failed to satisfy constraint: Member must have value less than or \
                     equal to 100"
                ),
                "case {case:?}: expected the model's own requirement, got {stated:?}"
            );
        }
    }

    /// A clause about something nested inside a reported member keeps the rest of its
    /// trail, so the position inside the member survives the rename.
    #[test]
    fn a_nested_clause_keeps_its_trail_under_the_reported_member() {
        let stated = report(&[past_the_maximum(
            "trafficDialPercentage.1.member",
            "150",
            "100",
        )])
        .expect("a report");
        assert!(
            stated.contains("at 'trafficRetentionPercentage.1.member'"),
            "expected the trail to survive the rename, got {stated:?}"
        );
    }

    /// Nothing to state: no violation is no report, and a report over several violations
    /// is the framework's own multi-clause envelope with each clause stated this way.
    #[test]
    fn the_envelope_counts_the_clauses_it_states() {
        assert!(
            report(&[]).is_none(),
            "expected no report for no violation"
        );
        let stated = report(&[
            past_the_maximum("trafficDialPercentage", "150", "100"),
            past_the_maximum("thresholdCount", "11", "10"),
        ])
        .expect("a report");
        assert!(
            stated.starts_with("2 validation errors detected: "),
            "expected the two-clause envelope, got {stated:?}"
        );
        assert!(
            stated.contains("Value '150.0' at 'trafficRetentionPercentage'")
                && stated.contains("Value '11' at 'thresholdCount'"),
            "expected both clauses, got {stated:?}"
        );
    }
}
