// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS a refusal of the input: the `message` member of the
//! `ValidationException` a request that violates a modelled constraint is answered with.
//!
//! The envelope and every clause in it are the framework's own
//! (`::aws_smithy_schema_server::constraints`), which is where the sentence a constraint
//! violation states is written and where a `@sensitive` member's value is withheld from
//! it. This module owns the ONE part of that sentence the framework cannot derive: how a
//! `@pattern` is REPORTED.
//!
//! RULE: a modelled `@pattern` is a FIND expression — the value satisfies it when the
//! expression matches anywhere in it — while the expression a refusal reports is the
//! WHOLE-VALUE form of that same constraint. So each outer anchor the modelled pattern
//! carries is dropped, and each one it lacks is replaced by [`ANY`]:
//!
//! * `^X$` is reported as `X`: under a whole-value match the anchors say nothing more.
//! * `X`, anchored at neither end, is reported as `.*X.*`: what the find expression
//!   admitted before and after the match is spelled out instead.
//! * a pattern anchored at one end only keeps that anchor's side and spells out the other.
//!
//! Applied at the two places a DataZone refusal reports a pattern, so the two cannot
//! drift: [`reworded`], which this service hands
//! [`smithy_server_runtime::Service::override_refusal`] for every violation the request
//! boundary finds, and [`pattern_violation_message`], for the one pattern the boundary
//! cannot check itself — `FormTypeName` needs look-around, which the boundary's regex
//! engine does not offer, so `CreateFormType` checks that member in its own handler and
//! reports it through here.

use ::aws_smithy_schema_server::constraints::{Violation, ViolationKind};
use ::aws_smithy_schema_server::envelope;
use ::smithy_server_runtime::{Refusal, Refused};

/// The error shape a refusal of the input names, as `x-amzn-errortype` carries it. It is
/// the code the framework already picks for this refusal, restated because a chosen
/// wording replaces the code and the message together.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// How "any run of characters" is spelled in a reported expression: what a FIND
/// expression admits on either side of its match, and a whole-value expression has to
/// state.
const ANY: &str = ".*";

/// The anchor a pattern carries at its start.
const ANCHOR_START: char = '^';

/// The anchor a pattern carries at its end.
const ANCHOR_END: char = '$';

/// `modelled` as a refusal reports it: the whole-value form of the modelled find
/// expression (see the module docs).
#[must_use]
pub fn reported_pattern(modelled: &str) -> String {
    let (start_anchored, after_start) = match modelled.strip_prefix(ANCHOR_START) {
        Some(rest) => (true, rest),
        None => (false, modelled),
    };
    let (end_anchored, core) = match strip_end_anchor(after_start) {
        Some(rest) => (true, rest),
        None => (false, after_start),
    };
    let mut reported = String::with_capacity(core.len() + 2 * ANY.len());
    if !start_anchored {
        reported.push_str(ANY);
    }
    reported.push_str(core);
    if !end_anchored {
        reported.push_str(ANY);
    }
    reported
}

/// `pattern` without the end ANCHOR, or `None` when it carries none.
///
/// A trailing `$` the expression ESCAPES is the dollar CHARACTER, not the anchor, and a
/// character class is what escapes it — so it is counted rather than assumed: the `$` is
/// escaped when an odd number of backslashes immediately precedes it.
fn strip_end_anchor(pattern: &str) -> Option<&str> {
    let head = pattern.strip_suffix(ANCHOR_END)?;
    let escapes = head.chars().rev().take_while(|ch| *ch == '\\').count();
    if escapes % 2 == 0 {
        Some(head)
    } else {
        None
    }
}

/// One `@pattern` violation as this service reports it: the member it names, the pattern
/// in the form the rule reports, and the value only when there is one to show.
///
/// `shown_value` is `None` for a `@sensitive` member, whose value a refusal withholds —
/// which is also what marks the violation sensitive, so the framework's clause names the
/// member alone rather than reporting the value as `null` (the way it reports a member
/// that was absent).
fn pattern_violation(
    member: &str,
    modelled_pattern: &str,
    shown_value: Option<&str>,
) -> Violation {
    Violation {
        path: member.to_string(),
        kind: ViolationKind::Pattern(reported_pattern(modelled_pattern)),
        value: shown_value.map(str::to_string),
        sensitive: shown_value.is_none(),
    }
}

/// The whole `message` of the refusal of ONE member that violates `modelled_pattern`: the
/// framework's validation envelope over the one clause [`pattern_violation`] describes.
///
/// For the pattern the request boundary cannot check itself; everything the boundary DOES
/// check is reworded by [`reworded`] instead, from the violations the boundary found.
#[must_use]
pub fn pattern_violation_message(
    member: &str,
    modelled_pattern: &str,
    shown_value: Option<&str>,
) -> String {
    // One violation always renders, so the envelope is never absent here.
    envelope(&[pattern_violation(member, modelled_pattern, shown_value)]).unwrap_or_default()
}

/// How this service states a refusal the framework made before any handler ran, or `None`
/// to keep the framework's own wording.
///
/// Only the reported `@pattern` is this service's to state, so a refusal that reports no
/// pattern is left entirely alone; one that reports any is restated clause for clause
/// through the framework's own envelope, with every pattern in whole-value form.
#[must_use]
pub fn reworded(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    if !violations
        .iter()
        .any(|violation| matches!(violation.kind, ViolationKind::Pattern(_)))
    {
        return None;
    }
    let reported: Vec<Violation> = violations
        .iter()
        .map(|violation| match &violation.kind {
            ViolationKind::Pattern(modelled) => Violation {
                kind: ViolationKind::Pattern(reported_pattern(modelled)),
                ..violation.clone()
            },
            _ => violation.clone(),
        })
        .collect();
    Some(Refused {
        code: VALIDATION_EXCEPTION.to_string(),
        message: envelope(&reported)?,
    })
}

#[cfg(test)]
mod tests {
    use super::{pattern_violation_message, reported_pattern, reworded};
    use ::aws_smithy_schema_server::constraints::{Violation, ViolationKind};
    use ::smithy_server_runtime::Refusal;

    /// RULE: a modelled pattern is reported in whole-value form — each outer anchor it
    /// carries dropped, each one it lacks spelled out as `.*`.
    ///
    /// The rows are the modelled patterns of this service's own members, differing along
    /// what the rule generalises over: anchored at both ends, anchored at neither, and
    /// (the rule's remaining case, which no member of this model declares) anchored at one
    /// end only. SOURCE for every modelled pattern below:
    /// `smithy_query(query="shape", shape="AccountPoolName,DomainId,AwsRegion,FormTypeName,GroupIdentifier")`.
    #[test]
    fn a_modelled_pattern_is_reported_as_its_whole_value_form() {
        let rows: &[(&str, &str, &str)] = &[
            ("AccountPoolName", r"^[\w -]+$", r"[\w -]+"),
            (
                "DomainId",
                r"^dzd[-_][a-zA-Z0-9_-]{1,36}$",
                r"dzd[-_][a-zA-Z0-9_-]{1,36}",
            ),
            (
                "AwsRegion",
                r"^[a-z]{2}-[a-z]{4,10}-\d$",
                r"[a-z]{2}-[a-z]{4,10}-\d",
            ),
            // Alternatives anchored one by one: only the OUTERMOST anchors are the
            // expression's own, so the `$` and `^` inside the alternation stay.
            (
                "FormTypeName",
                r"^(amazon.datazone.)?(?![0-9_])\w+$|^_\w*[a-zA-Z0-9]\w*$",
                r"(amazon.datazone.)?(?![0-9_])\w+$|^_\w*[a-zA-Z0-9]\w*",
            ),
            // Anchored at neither end, so both sides are spelled out.
            (
                "GroupIdentifier",
                r"(^([0-9a-f]{10}-|)[A-Fa-f0-9]{8}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{12}$|[\p{L}\p{M}\p{S}\p{N}\p{P}\t\n\r  ]+)",
                r".*(^([0-9a-f]{10}-|)[A-Fa-f0-9]{8}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{4}-[A-Fa-f0-9]{12}$|[\p{L}\p{M}\p{S}\p{N}\p{P}\t\n\r  ]+).*",
            ),
            // The rule's remaining cases, which this model declares for no member: one
            // outer anchor only, and a trailing dollar CHARACTER, which is escaped and so
            // is not an anchor at all.
            ("anchored at the start only", r"^[0-9]+", r"[0-9]+.*"),
            ("anchored at the end only", r"[0-9]+$", r".*[0-9]+"),
            ("a trailing escaped dollar", r"^[0-9]+\$", r"[0-9]+\$.*"),
        ];

        for (case, modelled, expected) in rows {
            let actual = reported_pattern(modelled);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the reported pattern reaches the message, and whether the offending value is
    /// shown is decided by whether there is one to show — a `@sensitive` member's value is
    /// withheld, and the clause then names the member alone.
    #[test]
    fn a_pattern_refusal_names_the_member_and_shows_only_a_value_it_may_show() {
        for (case, member, modelled, shown, expected) in [
            (
                "a sensitive member",
                "name",
                r"^[\w -]+$",
                None,
                r"1 validation error detected: Value at 'name' failed to satisfy constraint: Member must satisfy regular expression pattern: [\w -]+",
            ),
            (
                "a member whose value is shown",
                "domainIdentifier",
                r"^dzd[-_][a-zA-Z0-9_-]{1,36}$",
                Some("dzd_bad.id"),
                r"1 validation error detected: Value 'dzd_bad.id' at 'domainIdentifier' failed to satisfy constraint: Member must satisfy regular expression pattern: dzd[-_][a-zA-Z0-9_-]{1,36}",
            ),
            (
                "an empty value, which is still a value",
                "groupIdentifier",
                r"(a|b)",
                Some(""),
                r"1 validation error detected: Value '' at 'groupIdentifier' failed to satisfy constraint: Member must satisfy regular expression pattern: .*(a|b).*",
            ),
        ] {
            let actual = pattern_violation_message(member, modelled, shown);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: only a refusal that REPORTS a pattern is this service's to word — every other
    /// refusal keeps the framework's own wording, which is what `None` asks for.
    #[test]
    fn a_refusal_reporting_no_pattern_keeps_the_frameworks_wording() {
        for (case, refusal) in [
            (
                "an absent required member",
                Refusal::Invalid(vec![Violation {
                    path: "name".to_string(),
                    kind: ViolationKind::MissingRequiredMember,
                    value: None,
                    sensitive: false,
                }]),
            ),
            (
                "a length bound",
                Refusal::Invalid(vec![Violation {
                    path: "requestReason".to_string(),
                    kind: ViolationKind::MaxLength(4096),
                    value: None,
                    sensitive: true,
                }]),
            ),
            ("an unauthenticated caller", Refusal::Unauthenticated),
        ] {
            let chosen = reworded(&refusal);
            assert!(
                chosen.is_none(),
                "case {case:?}: expected the framework's wording, got {chosen:?}"
            );
        }
    }

    /// RULE: a refusal carrying SEVERAL violations keeps every clause and its order, with
    /// each reported pattern in whole-value form — the envelope counts what it states.
    #[test]
    fn every_clause_of_a_multi_violation_refusal_is_restated() {
        let refusal = Refusal::Invalid(vec![
            Violation {
                path: "name".to_string(),
                kind: ViolationKind::Pattern(r"^[\w -]+$".to_string()),
                value: None,
                sensitive: true,
            },
            Violation {
                path: "description".to_string(),
                kind: ViolationKind::MaxLength(2048),
                value: None,
                sensitive: true,
            },
        ]);
        let chosen = reworded(&refusal).expect("a refusal reporting a pattern is reworded");
        let expected = "2 validation errors detected: Value at 'name' failed to satisfy \
                        constraint: Member must satisfy regular expression pattern: [\\w -]+; \
                        Value at 'description' failed to satisfy constraint: Member must have \
                        length less than or equal to 2048";
        assert_eq!(
            chosen.message, expected,
            "expected {expected:?}, got {:?}",
            chosen.message
        );
        assert_eq!(
            chosen.code, "ValidationException",
            "a chosen wording replaces the code too, got {:?}",
            chosen.code
        );
    }
}
