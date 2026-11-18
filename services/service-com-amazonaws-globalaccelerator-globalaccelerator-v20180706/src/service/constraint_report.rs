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
//!
//! # The requirements stated in the service's own sentence
//!
//! A few members are not reported through that clause at all: this service validates them
//! in its own frontend and states the requirement in ONE sentence of its own. Which members
//! those are, and which sentence each takes, is recorded wire behaviour
//! ([`STATED_ENUM_SENTENCES`], [`STATED_INTERVAL_MEMBER`]); what goes INTO a sentence is the
//! member as the API spells it, the value set the MODEL declares for it, and the value the
//! REQUEST carried — never anything else about the request. The interval sentence is not
//! written here at all: it is taken from the one place the handlers state it
//! ([`crate::state::unsupported_health_check_interval_message`]), so the seam and the
//! handlers cannot word the same class differently. [`message`] is the one entry point that
//! asks for such a sentence before falling back to the clause envelope, so every operation
//! carrying such a member states the same words.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use smithy_server_runtime::{Violation, ViolationKind};

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

/// How this service states the refusal of one value outside an enumerated member's value
/// set: the violation the walk reported, and the values the model admits, in — the sentence
/// out.
type StatedEnumSentence = fn(&Violation, &[String]) -> Option<String>;

/// Every input member of this service whose refusal of a value outside its value set is
/// stated in a sentence of this service's own rather than through a constraint clause, with
/// the sentence it states.
///
/// Recorded wire behaviour, one entry per member a recording shows: the listener protocol
/// ([`protocol_sentence`]) and the accelerator IP address type
/// ([`ip_address_type_sentence`]). Keyed on the MEMBER, so every operation whose input
/// carries it states the same sentence — `Protocol` on `CreateListener` and
/// `UpdateListener`, `IpAddressType` on the two accelerator kinds' creates and updates
/// (`smithy_query(query="operation", operation="CreateListener,UpdateListener,
/// CreateAccelerator,UpdateAccelerator,CreateCustomRoutingAccelerator")`). Every other
/// enumerated member of this service is absent from this table deliberately: nothing
/// records a sentence of its own for one, so it keeps the clause.
const STATED_ENUM_SENTENCES: [(&str, StatedEnumSentence); 2] = [
    ("protocol", protocol_sentence),
    ("ipAddressType", ip_address_type_sentence),
];

/// `member` as the API spells it in a sentence about it: the walk reports a member in
/// lowerCamel, while the service names it the way the model declares it — upperCamel for
/// every member of this service.
fn api_member(member: &str) -> String {
    let mut characters = member.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

/// How this service states a listener protocol outside the value set: the member as the API
/// spells it, over the alternatives the MODEL admits for it, and nothing of the request —
/// recorded wire behaviour on `CreateListener`, where a value of another protocol is refused
/// with this sentence rather than with a clause.
fn protocol_sentence(violation: &Violation, admitted: &[String]) -> Option<String> {
    Some(format!(
        "{} must be either {}",
        api_member(&violation.path),
        admitted.join(" or ")
    ))
}

/// The value the request carried, as the walk echoed it (already elided to the walk's own
/// echo limit), or `None` when the refusal WITHHOLDS it: a member the model marks
/// `@sensitive`, or one that carried no value at all.
///
/// A sentence whose text NAMES that value cannot be stated without it, so such a refusal
/// keeps the clause — which states the member, the requirement and no value — rather than
/// being worded with a hole in it.
fn echoed_value(violation: &Violation) -> Option<&str> {
    if violation.sensitive {
        return None;
    }
    violation.value.as_deref()
}

/// How this service states an IP address type outside the value set: the value the REQUEST
/// carried, named in the service's own spelling of the member — recorded wire behaviour on
/// `CreateAccelerator`. The set the model admits is not part of this sentence, so it is not
/// stated.
fn ip_address_type_sentence(violation: &Violation, _admitted: &[String]) -> Option<String> {
    let submitted = echoed_value(violation)?;
    Some(format!("IP Address type {submitted} is invalid"))
}

/// The sentence this service states `violation` with, when it is a value outside the value
/// set of a member of [`STATED_ENUM_SENTENCES`].
///
/// `None` for anything else — another member, another constraint class, a position nested
/// inside such a member, or a refusal whose value the sentence needs and the walk withheld
/// ([`echoed_value`]) — so only what is recorded is stated this way.
fn enum_value_sentence(violation: &Violation) -> Option<String> {
    let ViolationKind::EnumValue(admitted) = &violation.kind else {
        return None;
    };
    let (_, sentence) = STATED_ENUM_SENTENCES
        .iter()
        .find(|(member, _)| *member == violation.path)?;
    sentence(violation, admitted)
}

/// The member whose SUPPORTED VALUES this service states a sentence of its own for, where
/// the modelled `@range` is the enclosing interval rather than the set of values the
/// service accepts.
///
/// `HealthCheckIntervalSeconds` is "The time—10 seconds or 30 seconds—between each health
/// check for an endpoint" (`smithy_query(query="doc",
/// shape="HealthCheckIntervalSeconds")`), while its `@range` is 10..30 — so a value inside
/// the interval but outside the set reaches a handler, and one outside the interval is
/// refused here. Both are the same class to the service, which states it with the one
/// sentence [`crate::state::unsupported_health_check_interval_message`], so the two paths
/// cannot answer the same request class differently.
const STATED_INTERVAL_MEMBER: &str = "healthCheckIntervalSeconds";

/// The sentence this service states `violation` with, when it is a value outside the set of
/// intervals the member of [`STATED_INTERVAL_MEMBER`] accepts: the one sentence the handlers
/// state that class with, over the value the request carried as the walk echoed it.
///
/// `None` for anything else — another member, another constraint class (an absent member is
/// not an unsupported interval), a position nested inside that member, or a refusal whose
/// value the walk withheld ([`echoed_value`]), since this sentence names it.
fn interval_sentence(violation: &Violation) -> Option<String> {
    if violation.path != STATED_INTERVAL_MEMBER {
        return None;
    }
    match violation.kind {
        ViolationKind::MinValue(_) | ViolationKind::MaxValue(_) => Some(
            crate::state::unsupported_health_check_interval_message(echoed_value(violation)?),
        ),
        _ => None,
    }
}

/// The message this service answers `violations` with: its own sentence about a member it
/// states one for, and otherwise the constraint report ([`report`]).
///
/// The sentence is stated only for a refusal that is about that member ALONE: it names no
/// other member, so answering it for a request that also broke a bound elsewhere would drop
/// what the report says about the other member.
pub fn message(violations: &[Violation]) -> Option<String> {
    if let [only] = violations {
        if let Some(sentence) = enum_value_sentence(only).or_else(|| interval_sentence(only)) {
            return Some(sentence);
        }
    }
    report(violations)
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

    /// One violation at `path`, of the shape the walk reports for a value outside an
    /// enumerated type's value set.
    fn outside_the_value_set(path: &str, value: &str, admitted: &[&str]) -> Violation {
        Violation {
            path: path.to_owned(),
            kind: ViolationKind::EnumValue(admitted.iter().map(|v| (*v).to_string()).collect()),
            value: Some(value.to_owned()),
            sensitive: false,
        }
    }

    /// RULE: a value outside the value set of a member this service states its own
    /// sentence for is answered with that sentence, in the form recorded for that member —
    /// over the alternatives the model admits, or over the value the request carried;
    /// every other enumerated member keeps the constraint clause.
    #[test]
    fn a_stated_member_answers_the_services_own_sentence() {
        // (case, the walk's path, the submitted value, the admitted set, the message)
        let rows: [(&str, &str, &str, &[&str], &str); 10] = [
            (
                "the stated member, a value of another protocol",
                "protocol",
                "SCTP",
                &["TCP", "UDP"],
                "Protocol must be either TCP or UDP",
            ),
            (
                "the stated member, a value differing in case",
                "protocol",
                "tcp",
                &["TCP", "UDP"],
                "Protocol must be either TCP or UDP",
            ),
            (
                "the stated member, an empty value",
                "protocol",
                "",
                &["TCP", "UDP"],
                "Protocol must be either TCP or UDP",
            ),
            (
                // The other stated member takes the other recorded form: the value the
                // request carried, and no set.
                "the member stated over its value, a type of neither family",
                "ipAddressType",
                "IPV5",
                &["IPV4", "DUAL_STACK"],
                "IP Address type IPV5 is invalid",
            ),
            (
                "the member stated over its value, a value differing in case",
                "ipAddressType",
                "ipv4",
                &["IPV4", "DUAL_STACK"],
                "IP Address type ipv4 is invalid",
            ),
            (
                "the member stated over its value, an empty value",
                "ipAddressType",
                "",
                &["IPV4", "DUAL_STACK"],
                "IP Address type  is invalid",
            ),
            (
                "the member stated over its value, a value that drops the separator",
                "ipAddressType",
                "DUALSTACK",
                &["IPV4", "DUAL_STACK"],
                "IP Address type DUALSTACK is invalid",
            ),
            (
                // Another enumerated member of this service: nothing records a sentence
                // of its own for it, so it keeps the clause.
                "another enumerated member",
                "healthCheckProtocol",
                "SMTP",
                &["TCP", "HTTP", "HTTPS"],
                "1 validation error detected: Value 'SMTP' at 'healthCheckProtocol' failed to \
                 satisfy constraint: Member must satisfy enum value set: [TCP, HTTP, HTTPS]",
            ),
            (
                "a third enumerated member",
                "clientAffinity",
                "SESSION",
                &["NONE", "SOURCE_IP"],
                "1 validation error detected: Value 'SESSION' at 'clientAffinity' failed to \
                 satisfy constraint: Member must satisfy enum value set: [NONE, SOURCE_IP]",
            ),
            (
                // A position NESTED inside the stated member is not that member's own
                // value, so it is reported as a clause.
                "a position nested inside the stated member",
                "protocol.1.member",
                "SCTP",
                &["TCP", "UDP"],
                "1 validation error detected: Value 'SCTP' at 'protocol.1.member' failed to \
                 satisfy constraint: Member must satisfy enum value set: [TCP, UDP]",
            ),
        ];

        for (case, path, value, admitted, expected) in rows {
            let answered = message(&[outside_the_value_set(path, value, admitted)])
                .unwrap_or_else(|| panic!("case {case:?}: expected a message"));
            assert_eq!(
                answered, expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }

    /// The sentence names no other member, so a refusal that also broke a bound elsewhere
    /// is answered with the report naming every clause rather than with the sentence; and
    /// a refusal of the stated member for another constraint class keeps its clause too.
    #[test]
    fn the_sentence_is_stated_only_for_that_member_alone() {
        let both = message(&[
            outside_the_value_set("protocol", "SCTP", &["TCP", "UDP"]),
            past_the_maximum("thresholdCount", "11", "10"),
        ])
        .expect("a message");
        assert!(
            both.starts_with("2 validation errors detected: ")
                && both.contains("at 'protocol'")
                && both.contains("at 'thresholdCount'"),
            "expected the report over both clauses, got {both:?}"
        );

        let absent = message(&[Violation {
            path: "protocol".to_owned(),
            kind: ViolationKind::MissingRequiredMember,
            value: None,
            sensitive: false,
        }])
        .expect("a message");
        assert_eq!(
            absent,
            "1 validation error detected: Value null at 'protocol' failed to satisfy \
             constraint: Member must not be null",
            "expected the clause for an absent member, got {absent:?}"
        );
    }

    /// A sentence that NAMES the rejected value cannot be stated for a refusal that
    /// withholds it, so such a refusal keeps the clause — which states the member and the
    /// requirement without the value. The sentence that names only the model's own
    /// alternatives is unaffected.
    #[test]
    fn a_refusal_whose_value_is_withheld_keeps_the_clause() {
        // (case, the violation, the message)
        let rows: [(&str, Violation, &str); 3] = [
            (
                "the member stated over its value, withheld",
                Violation {
                    sensitive: true,
                    value: None,
                    ..outside_the_value_set("ipAddressType", "IPV5", &["IPV4", "DUAL_STACK"])
                },
                "1 validation error detected: Value at 'ipAddressType' failed to satisfy \
                 constraint: Member must satisfy enum value set: [IPV4, DUAL_STACK]",
            ),
            (
                "the interval member, withheld",
                Violation {
                    sensitive: true,
                    value: None,
                    ..past_the_maximum("healthCheckIntervalSeconds", "31", "30")
                },
                "1 validation error detected: Value at 'healthCheckIntervalSeconds' failed to \
                 satisfy constraint: Member must have value less than or equal to 30",
            ),
            (
                // This one names no value at all, so withholding one takes nothing from it.
                "the member stated over the model's alternatives, withheld",
                Violation {
                    sensitive: true,
                    value: None,
                    ..outside_the_value_set("protocol", "SCTP", &["TCP", "UDP"])
                },
                "Protocol must be either TCP or UDP",
            ),
        ];

        for (case, violation, expected) in rows {
            let answered =
                message(&[violation]).unwrap_or_else(|| panic!("case {case:?}: expected a message"));
            assert_eq!(
                answered, expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }
}
