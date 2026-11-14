// SPDX-License-Identifier: CC0-1.0

//! How this service words a request refused because the `domainId` it named is not a
//! domain id at all.
//!
//! `DomainId` is `@length(min: 20, max: 25)` with `@pattern "^(hai-|dom-)[a-z0-9]+$"`
//! (`smithy_query(query="shape",shape="DomainId",model_path="services/service-com-amazonaws-connecthealth-connecthealth/smithy-model/com-amazonaws-connecthealth-connecthealth.json")`),
//! and every operation that addresses a domain binds a member of that shape. Those
//! constraints are checked at the REQUEST BOUNDARY, so a value outside one never reaches a
//! handler and the sentence refusing it is worded here instead — through
//! [`smithy_server_runtime::Service::override_refusal`], the one seam a service has for a
//! refusal the framework raised before any handler ran.
//!
//! RECORDED behaviour: ConnectHealth does not state that refusal as the framework's
//! generic member-constraint clause (`1 validation error detected: Value '…' at '…' failed
//! to satisfy constraint: …`), which names the member and the requirement it failed. The
//! frontend resolves the id by CALLING the component that owns a domain, and reports what
//! that call came back with: HTTP 400 `ValidationException` whose detail text is the value
//! the request supplied, the FIXED sentence [`NOT_A_VALID_DOMAIN_ID`], and then the
//! envelope the SDK layer states around a relayed call — the workspace's one rendering of
//! it ([`service_common::relayed_refusal`]), so the service, the status and the request id
//! a client parses back out read the same here as for every other relayed refusal in this
//! emulator. The sentence and the relayed status are the requirement; the request id and
//! the attempt count inside the envelope belong to that call and are per-request.
//!
//! Only a value violation of that ONE member is reworded: a refusal that also blames
//! another member, or that reports an absent member rather than a bad value, keeps the
//! framework's clause rather than being collapsed into a sentence about a domain id the
//! request may not even have carried. The code stays `ValidationException` — the shape this
//! model declares for invalid client input — so a client branching on the code sees no
//! change at all.

use aws_smithy_schema_server::{Violation, ViolationKind};
use smithy_server_runtime::{Refusal, Refused};

use super::error_detail::echoed;

/// The member a request names a domain by, spelled as the model spells it — which is also
/// how a violation path spells it, since a path names a member in lowerCamel
/// (`vendor/smithy-schema-server/src/constraints.rs`, `path::member`).
const DOMAIN_ID_MEMBER: &str = "domainId";

/// The FIXED text of the sentence, after the value the request supplied.
const NOT_A_VALID_DOMAIN_ID: &str = "is not a valid domain id.";

/// The error code this refusal carries: the shape this service's model declares for
/// invalid client input (`ValidationException`, `@error client`, `@httpError 400`), which
/// is the code the framework's own clause already carries — so rewording the sentence
/// leaves what a client branches on untouched.
const VALIDATION_EXCEPTION_CODE: &str = "ValidationException";

/// The status the relayed call answered, which is the status this refusal is rendered with:
/// the one `ValidationException`'s `@httpError` declares.
const VALIDATION_EXCEPTION_STATUS: u16 = 400;

/// The component the relayed call was made to: the owner of a domain, named as the resource
/// type the model's own `@arnReference` on `DomainId` names it (`AWS::HealthAgent::Domain`,
/// from `smithy_query(query="shape",shape="DomainId")`) — not a token copied out of one
/// recorded run, and not this service's `sdkId`, which is the FRONTEND a client addressed.
const DOMAIN_COMPONENT: &str = "HealthAgent";

/// The detail text this service answers for a `domainId` it cannot read as a domain id:
/// the value the request supplied, the fixed sentence, and the envelope naming the relayed
/// call that refused it.
///
/// The value is named because that is what tells one refusal from another, and it goes out
/// through the one bounded rendering every sentence of this service echoes a request value
/// through ([`echoed`]), so a caller-sized value cannot size the response answering it.
///
/// The envelope comes from [`service_common::relayed_refusal`], this workspace's ONE
/// rendering of it: a resolution refused on the caller's own input is not retried, so the
/// call it reports was made once.
pub fn invalid_domain_id_message(domain_id: &str) -> String {
    ::service_common::relayed_refusal::relayed_refusal_message(
        &format!("{} {NOT_A_VALID_DOMAIN_ID}", echoed(domain_id)),
        ::service_common::relayed_refusal::RelayedCall::once(
            DOMAIN_COMPONENT,
            VALIDATION_EXCEPTION_STATUS,
        ),
    )
}

/// Whether `violation` reports a VALUE supplied for the domain-id member that is outside
/// what [`DOMAIN_ID_MEMBER`]'s shape declares — either end of its `@length` or its
/// `@pattern` — and that the member is one whose value may be named at all.
///
/// An absent member is not one of these: there is no value to read as a domain id, so that
/// refusal keeps the framework's own clause. Neither is a `@sensitive` member: its value is
/// withheld from a response by the walk that found it, and this sentence may not be the one
/// place it reappears.
fn names_an_unreadable_domain_id(violation: &Violation) -> bool {
    violation.path == DOMAIN_ID_MEMBER
        && !violation.sensitive
        && matches!(
            violation.kind,
            ViolationKind::Pattern(_) | ViolationKind::MinLength(_) | ViolationKind::MaxLength(_)
        )
}

/// The sentence this service states for `violations`, or `None` to keep the framework's own
/// clause.
///
/// One sentence is about ONE value, so every violation has to name the domain-id member: a
/// short value of a foreign prefix fails both the `@length` and the `@pattern` of the same
/// member and so arrives as two violations
/// (`vendor/smithy-schema-server/src/constraints.rs` checks both), while a refusal that
/// also blames another member keeps the clause that names them all.
///
/// Public so the one operation whose members the GENERATED request walk does not read —
/// `StartMedicalScribeListeningSession`, whose `@httpHeader`-bound settings are read by its
/// own event-stream path — states the same sentence for the same value rather than a second
/// wording of it.
pub fn sentence_for(violations: &[Violation]) -> Option<String> {
    if violations.is_empty() || !violations.iter().all(names_an_unreadable_domain_id) {
        return None;
    }
    // Every violation of one member carries the same value.
    let value = violations.iter().find_map(|v| v.value.as_deref())?;
    Some(invalid_domain_id_message(value))
}

/// How this service states `refusal`, or `None` to keep the framework's own wording.
pub fn refused(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    Some(Refused {
        code: VALIDATION_EXCEPTION_CODE.to_string(),
        message: sentence_for(violations)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pattern `DomainId` declares, as the model writes it — what a boundary violation
    /// of that member reports.
    const DOMAIN_ID_PATTERN: &str = "^(hai-|dom-)[a-z0-9]+$";

    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: value.map(str::to_string),
            sensitive: false,
        }
    }

    /// RULE: a value supplied for the domain-id member that is outside what its shape
    /// declares is refused with this service's own sentence, whichever of that shape's
    /// constraints it failed and however many of them at once.
    #[test]
    fn a_value_outside_the_domain_id_shape_takes_this_services_sentence() {
        let rows: Vec<(&str, Vec<Violation>, &str)> = vec![
            (
                "a foreign prefix inside the modelled length",
                vec![violation(
                    DOMAIN_ID_MEMBER,
                    ViolationKind::Pattern(DOMAIN_ID_PATTERN.to_string()),
                    Some("xxx-00000000000000000"),
                )],
                "xxx-00000000000000000",
            ),
            (
                "shorter than the modelled length, and of a foreign prefix",
                vec![
                    violation(
                        DOMAIN_ID_MEMBER,
                        ViolationKind::MinLength(20),
                        Some("nope"),
                    ),
                    violation(
                        DOMAIN_ID_MEMBER,
                        ViolationKind::Pattern(DOMAIN_ID_PATTERN.to_string()),
                        Some("nope"),
                    ),
                ],
                "nope",
            ),
            (
                "longer than the modelled length",
                vec![violation(
                    DOMAIN_ID_MEMBER,
                    ViolationKind::MaxLength(25),
                    Some("dom-00000000000000000000000"),
                )],
                "dom-00000000000000000000000",
            ),
        ];
        for (case, violations, value) in &rows {
            let chosen = refused(&Refusal::Invalid(violations.clone()));
            let expected = Refused {
                code: VALIDATION_EXCEPTION_CODE.to_string(),
                message: invalid_domain_id_message(value),
            };
            let stated = chosen
                .clone()
                .unwrap_or_else(|| panic!("case {case:?}: expected a stated refusal"));
            assert_eq!(
                stated.code, expected.code,
                "case {case:?}: expected code {:?}, got {:?}",
                expected.code, stated.code
            );
            let opening = format!("{value} {NOT_A_VALID_DOMAIN_ID} (Service: ");
            assert!(
                stated.message.starts_with(&opening),
                "case {case:?}: expected a message starting {opening:?}, got {:?}",
                stated.message
            );
        }
    }

    /// RULE: only a bad VALUE of that one member is reworded; everything else keeps the
    /// framework's own clause.
    #[test]
    fn every_other_refusal_keeps_the_frameworks_wording() {
        let rows: Vec<(&str, Refusal)> = vec![
            (
                "another member's value",
                Refusal::Invalid(vec![violation(
                    "subscriptionId",
                    ViolationKind::Pattern("^sub-[a-zA-Z0-9]{21}$".to_string()),
                    Some("sub-x"),
                )]),
            ),
            (
                "the domain id and another member at once",
                Refusal::Invalid(vec![
                    violation(
                        DOMAIN_ID_MEMBER,
                        ViolationKind::Pattern(DOMAIN_ID_PATTERN.to_string()),
                        Some("xxx-00000000000000000"),
                    ),
                    violation("maxResults", ViolationKind::MaxValue("100".to_string()), Some("101")),
                ]),
            ),
            (
                "an absent domain id",
                Refusal::Invalid(vec![violation(
                    DOMAIN_ID_MEMBER,
                    ViolationKind::MissingRequiredMember,
                    None,
                )]),
            ),
            (
                "a domain id whose value was withheld",
                Refusal::Invalid(vec![Violation {
                    path: DOMAIN_ID_MEMBER.to_string(),
                    kind: ViolationKind::Pattern(DOMAIN_ID_PATTERN.to_string()),
                    value: None,
                    sensitive: true,
                }]),
            ),
            (
                "a domain id marked sensitive that still carries its value",
                Refusal::Invalid(vec![Violation {
                    path: DOMAIN_ID_MEMBER.to_string(),
                    kind: ViolationKind::Pattern(DOMAIN_ID_PATTERN.to_string()),
                    value: Some("xxx-00000000000000000".to_string()),
                    sensitive: true,
                }]),
            ),
            ("no violation at all", Refusal::Invalid(Vec::new())),
            ("an unauthenticated caller", Refusal::Unauthenticated),
        ];
        for (case, refusal) in &rows {
            let chosen = refused(refusal);
            assert!(
                chosen.is_none(),
                "case {case:?}: expected the framework's own wording, got {chosen:?}"
            );
        }
    }

    /// RULE: the sentence names the value the request supplied, but its size is fixed by
    /// this service — a caller-sized value is elided exactly as every other sentence of
    /// this service elides one.
    #[test]
    fn the_sentence_is_not_sized_by_the_request() {
        use crate::service::error_detail::MAX_ECHOED_VALUE_LEN;

        let huge = "z".repeat(16 * MAX_ECHOED_VALUE_LEN);
        let stated = invalid_domain_id_message(&huge);
        assert!(
            !stated.contains(huge.as_str()),
            "case \"a caller-sized domain id\": the whole value must not be echoed, got {} bytes",
            stated.len()
        );
        assert!(
            stated.contains(&format!("... [{} bytes]", huge.len())),
            "case \"a caller-sized domain id\": the value must be named elided, got {stated:?}"
        );
        assert!(
            stated.len() <= MAX_ECHOED_VALUE_LEN + 256,
            "case \"a caller-sized domain id\": the sentence must not be sized by the \
             request, got {} bytes",
            stated.len()
        );
    }

    /// RULE: the request id the tail names is minted per call, so two refusals of the same
    /// value are distinguishable while the sentence in front of the tail is identical.
    #[test]
    fn the_tail_names_a_per_call_request_id() {
        let value = "xxx-00000000000000000";
        let first = invalid_domain_id_message(value);
        let second = invalid_domain_id_message(value);
        let opening = format!("{value} {NOT_A_VALID_DOMAIN_ID} (Service: ");
        for (case, message) in [("first", &first), ("second", &second)] {
            assert!(
                message.starts_with(&opening),
                "case {case:?}: expected a message starting {opening:?}, got {message:?}"
            );
        }
        assert_ne!(
            first, second,
            "case \"two refusals of one value\": expected distinct request ids, got {first:?}"
        );
    }
}
