// SPDX-License-Identifier: CC0-1.0

//! How this service words a request the framework refused BEFORE any handler ran — the
//! `override_refusal` seam of [`crate::service::EmulatedService`].
//!
//! The framework's own wording for a value outside a declared bound is a constraint report:
//! it quotes the value the request sent, the member path it sat at, and the model's own
//! `@length`/`@pattern` text. Route 53 ARC's cluster API does not state its refusals that
//! way. A request whose ARN-bearing member carries something that is not an ARN of the
//! shape this model declares is refused with ONE fixed sentence
//! ([`INVALID_RESOURCE_ARN_MESSAGE`]) that echoes nothing of the request: not the value, not
//! the member name, and not the pattern. So the refusal is reworded here, where the service
//! owns the words, rather than left to the generic reporter — and the code stays the
//! `ValidationException` (HTTP 400) the model declares for a validation error on the
//! request.
//!
//! Only the ARN members are reworded. Every other member this model constrains — a
//! `NextToken`'s pattern, a `MaxResults` range, a `RoutingControlState` outside the enum —
//! keeps the framework's wording, because nothing observed says this API states those any
//! differently.

use ::aws_smithy_schema_server::constraints::{Violation, ViolationKind};
use ::smithy_server_runtime::{Refusal, Refused};

/// The modelled error a validation failure on the request is reported as: "There was a
/// validation error on the request", a client error at HTTP 400
/// (`smithy_query(query="operation", operation="GetRoutingControlState", model_path="services/service-com-amazonaws-route53recoverycluster-togglecustomerapi/smithy-model/com-amazonaws-route53recoverycluster-togglecustomerapi.json")`).
/// It is the code the framework would answer anyway; it is named with the message because a
/// client branches on the code, so the two are chosen together.
pub const VALIDATION_EXCEPTION: &str = "ValidationException";

/// How this API words a request whose ARN-bearing member is absent or is not an ARN of the
/// modelled shape.
///
/// The WHOLE string is fixed text — it states that the resource ARN was missing or invalid
/// and nothing else, so it carries no value of the request and is the same sentence for
/// every such request.
pub const INVALID_RESOURCE_ARN_MESSAGE: &str = "Missing or invalid resource arn";

/// The input members of this model that carry an ARN, named as a violation path spells them
/// (lowerCamel).
///
/// Source: the members targeting the `Arn` string shape and the `Arns` list of it
/// (`smithy_query(query="operation", operation="GetRoutingControlState,ListRoutingControls,UpdateRoutingControlState,UpdateRoutingControlStates", model_path="services/service-com-amazonaws-route53recoverycluster-togglecustomerapi/smithy-model/com-amazonaws-route53recoverycluster-togglecustomerapi.json")`
/// and `smithy_query(query="shape", shape="Arns,UpdateRoutingControlStateEntry", …)`):
/// `RoutingControlArn` (the get and both updates, the latter inside a batch entry),
/// `ControlPanelArn` (the listing's filter) and `SafetyRulesToOverride` (a list whose
/// elements are ARNs).
const ARN_MEMBERS: &[&str] = &[
    "routingControlArn",
    "controlPanelArn",
    "safetyRulesToOverride",
];

/// The constraints an ARN member can fail, and the absence of a required one — the three
/// facts this API answers with the one sentence above.
fn about_an_arn(violation: &Violation) -> bool {
    let arn_constraint = matches!(
        violation.kind,
        ViolationKind::MissingRequiredMember
            | ViolationKind::MinLength(_)
            | ViolationKind::MaxLength(_)
            | ViolationKind::Pattern(_)
    );
    arn_constraint && ARN_MEMBERS.contains(&addressed_member(&violation.path))
}

/// The member a violation path addresses: its last segment that names a MEMBER.
///
/// A path into a collection ends in the element's position and the collection step (an ARN
/// at `safetyRulesToOverride.1.member`, a batch entry's at
/// `updateRoutingControlStateEntries.1.member.routingControlArn`), so those segments are
/// stepped over to reach the member that carries the value.
fn addressed_member(path: &str) -> &str {
    path.rsplit('.')
        .find(|segment| {
            *segment != "member" && !segment.chars().all(|c| c.is_ascii_digit()) && !segment.is_empty()
        })
        .unwrap_or(path)
}

/// This service's wording for `refusal`, or `None` to keep the framework's.
#[must_use]
pub fn reworded(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    // One sentence for the whole request: it names no member, so several failing ARN members
    // are the same refusal as one.
    violations.iter().any(about_an_arn).then(|| Refused {
        code: VALIDATION_EXCEPTION.to_string(),
        message: INVALID_RESOURCE_ARN_MESSAGE.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pattern violation at `path`, as the constraint walk reports one.
    fn pattern_violation(path: &str) -> Violation {
        Violation {
            path: path.to_string(),
            kind: ViolationKind::Pattern("^[A-Za-z0-9:.\\/_-]*$".to_string()),
            value: Some("not an arn!".to_string()),
            sensitive: false,
        }
    }

    /// RULE: the member a violation is about is the last segment of its path that names a
    /// member, whether the value sat directly on the input, inside a list, or inside a
    /// structure inside a list.
    #[test]
    fn the_addressed_member_steps_over_collection_segments() {
        // (case, the path, the member it addresses)
        let rows = [
            ("a member of the input", "routingControlArn", "routingControlArn"),
            (
                "an element of a list of ARNs",
                "safetyRulesToOverride.1.member",
                "safetyRulesToOverride",
            ),
            (
                "a member of a structure inside a list",
                "updateRoutingControlStateEntries.3.member.routingControlArn",
                "routingControlArn",
            ),
            ("a member that carries no ARN", "nextToken", "nextToken"),
        ];
        for (case, path, expected) in rows {
            let actual = addressed_member(path);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the member {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: only a refusal about an ARN member is reworded, and it is reworded to the one
    /// fixed sentence; every other refused member keeps the framework's wording.
    #[test]
    fn only_an_arn_member_is_reworded() {
        // (case, the violations, whether this service rewords them)
        let rows: Vec<(&str, Vec<Violation>, bool)> = vec![
            (
                "a malformed routing control ARN",
                vec![pattern_violation("routingControlArn")],
                true,
            ),
            (
                "a malformed control panel filter",
                vec![pattern_violation("controlPanelArn")],
                true,
            ),
            (
                "a malformed ARN in an overridden-rules list",
                vec![pattern_violation("safetyRulesToOverride.2.member")],
                true,
            ),
            (
                "a malformed ARN in a batch entry",
                vec![pattern_violation(
                    "updateRoutingControlStateEntries.1.member.routingControlArn",
                )],
                true,
            ),
            (
                "an absent required ARN",
                vec![Violation {
                    path: "routingControlArn".to_string(),
                    kind: ViolationKind::MissingRequiredMember,
                    value: None,
                    sensitive: false,
                }],
                true,
            ),
            (
                "an ARN longer than the modelled bound",
                vec![Violation {
                    path: "routingControlArn".to_string(),
                    kind: ViolationKind::MaxLength(255),
                    value: Some("a".repeat(256)),
                    sensitive: false,
                }],
                true,
            ),
            (
                "a page token outside its pattern",
                vec![pattern_violation("nextToken")],
                false,
            ),
            (
                "a page size outside its range",
                vec![Violation {
                    path: "maxResults".to_string(),
                    kind: ViolationKind::MinValue("1".to_string()),
                    value: Some("0".to_string()),
                    sensitive: false,
                }],
                false,
            ),
            (
                "a state outside the enum",
                vec![Violation {
                    path: "updateRoutingControlStateEntries.1.member.routingControlState"
                        .to_string(),
                    kind: ViolationKind::EnumValue(vec!["Off".to_string(), "On".to_string()]),
                    value: Some("ON".to_string()),
                    sensitive: false,
                }],
                false,
            ),
        ];
        for (case, violations, expected) in rows {
            let chosen = reworded(&Refusal::Invalid(violations));
            match (expected, &chosen) {
                (true, Some(refused)) => {
                    assert_eq!(
                        (refused.code.as_str(), refused.message.as_str()),
                        (VALIDATION_EXCEPTION, INVALID_RESOURCE_ARN_MESSAGE),
                        "case {case:?}: expected this API's ARN refusal, got {refused:?}"
                    );
                }
                (false, None) => {}
                _ => panic!(
                    "case {case:?}: expected reworded={expected}, got {chosen:?}"
                ),
            }
        }
        assert!(
            reworded(&Refusal::Unauthenticated).is_none(),
            "case \"an unauthenticated request\": expected the framework's wording, got a \
             rewording"
        );
    }
}
