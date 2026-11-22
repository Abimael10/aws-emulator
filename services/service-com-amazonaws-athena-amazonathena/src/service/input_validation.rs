// SPDX-License-Identifier: CC0-1.0

//! How this service answers a request the framework refused for breaking a MODELLED input
//! constraint — a `@required` member that was absent, or a value outside a declared
//! `@length`, `@range` or `@pattern`.
//!
//! # The rule
//!
//! Such a request is wrong in exactly the way this service's own
//! `InvalidRequestException` states: "something is wrong with the input to the request. For
//! example, a required parameter may be missing or out of range"
//! (`smithy_query(query="shape",shape="InvalidRequestException",model_path="services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json")`),
//! and that shape is bound to EVERY operation of this service (`smithy_query(query="digest")`
//! reports it among the service's `common_errors`), so there is no operation whose input
//! refusal is stated in another shape. It is therefore answered the way every other refusal
//! this service states is: HTTP 400, `__type` `InvalidRequestException`, the refusal CLASS as
//! a code under BOTH names the wire gives it ([`crate::scaffolded::AthenaRefusal`]), and the
//! sentence under the `Message` member the model declares. The class is the one a refusal
//! whose cause is the request itself carries, [`crate::state::ERROR_CODE_INVALID_INPUT`].
//!
//! The SENTENCE is AWS's own validation wording — `N validation error(s) detected: ` over one
//! clause per broken constraint, each naming the member and the requirement — which
//! `service_common::validation_message` renders from the violations the framework hands over,
//! so neither the envelope nor any requirement is spelled here. Three renderings are this
//! service's own, and they are all this file chooses:
//!
//! * a clause states NO value: the member and the requirement it broke, never the bytes the
//!   request carried;
//! * except for a member that was not there at all, which is stated as `Value null at
//!   '<member>'` — there is a value to state for an absent member, and it is the absence;
//! * the value set of an enumerated type is listed in the order the front door's own hash
//!   table over those values is walked
//!   (`service_common::validation_message::in_hashed_enum_order`) rather than in the order the
//!   model declares them: the values are held keyed on their own text, so the set is
//!   enumerated out of that table. For `EncryptionOption`, declared `SSE_S3`, `SSE_KMS`,
//!   `CSE_KMS` (`smithy_query(query="shape",shape="EncryptionOption")`), that is `CSE_KMS`,
//!   `SSE_KMS`, `SSE_S3`.
//!
//! One kind of broken bound is NOT stated in that envelope at all: a bound the service also
//! states as a RULE of its own is reported as that rule, in the service's own sentence for it
//! ([`own_sentence`]), which is the one place that choice is made — a clause stated that way is
//! not also stated as a numeric requirement in the envelope. A client is told what it must do
//! differently, and it is told the same thing whether the framework measured the value against
//! the model's bound or a handler measured it against the rule.
//!
//! And one bound is stated in that envelope against a DIFFERENT number from the one the
//! framework measured: where the service's own bound on a member is not the modelled one, the
//! clause names the service's ([`at_requested_capacity_bound`]), because a bound a client is
//! told to satisfy is the one the service applies.
//!
//! # Why two seams
//!
//! The framework refuses the request before any handler runs, and asks this service for the
//! words through `Service::override_refusal`. That seam chooses the error CODE and the
//! sentence and nothing else: the body it is rendered into is the framework's own synthetic
//! shape, whose single member is spelled `message`, so a body built there carries neither the
//! `Message` member this service's error shapes declare nor any code member. The renderer
//! that writes it (`vendor/smithy-server-runtime/src/pre_dispatch.rs`, one envelope for every
//! service) is agent-immutable.
//!
//! So the words are chosen at that seam ([`stated_refusal`]) and the BODY is completed at the
//! one seam that sees a whole rendered response — the operation wrapper
//! `Service::override_operation` hands back ([`REFUSAL_BODY`]), which serves the operation the
//! routing table names and so leaves the whole deserialize → validate → handle → serialize
//! walk, every status and every code to the framework. Neither the plumbing nor the renaming
//! is this service's own: they are `service_common::restated_refusal` and
//! `service_common::error_detail_member`.
//!
//! Only a body the FRAMEWORK rendered is completed. An error a handler returned is serialized
//! from [`crate::scaffolded::ATHENA_REFUSAL_SCHEMA`], so it already carries its `Message` and
//! both code members and is answered byte for byte as that renderer wrote it — as is a
//! success, a refusal of another class (the metastore's `MetadataException`, the IAM layer's
//! `AccessDeniedException`) and the scaffold's own 501. Both halves of the completion are
//! `service_common::error_detail_member`'s: the member a sentence travels in, and a member
//! only the service knows the value of.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema_server::constraints::{Violation, ViolationKind};
use service_common::restated_refusal::RestatedRefusalOperation;
use smithy_server_runtime::{Refusal, Refused};

use crate::scaffolded::INVALID_REQUEST_EXCEPTION;
use crate::state::ERROR_CODE_INVALID_INPUT;

/// The body member the FRAMEWORK's own refusal renderer writes a sentence in: the member of
/// the synthetic shape it builds a pre-handler refusal from.
const RENDERED_MESSAGE_MEMBER: &str = "message";

/// The body member this service's error shapes declare their sentence in — `Message` on
/// `InvalidRequestException`, as on every error shape of this model, with no `@jsonName`, so
/// an awsJson1_1 body carries it under that Smithy name.
const MODELLED_MESSAGE_MEMBER: &str = "Message";

/// The member the model declares the refusal class under.
const ATHENA_ERROR_CODE_MEMBER: &str = "AthenaErrorCode";

/// The second name the wire gives that same class, which a client also reads it under (see
/// [`crate::scaffolded::AthenaRefusal`]).
const ERROR_CODE_MEMBER: &str = "ErrorCode";

/// The member every operation that requests provisioned capacity declares the amount under,
/// as a violation's path spells it: the lowerCamel of `TargetDpus`
/// (`aws_smithy_schema_server::constraints::path::member`), which both
/// `CreateCapacityReservation` and `UpdateCapacityReservation` target at `TargetDpusInteger`
/// and its `@range`.
const TARGET_DPUS_MEMBER: &str = "targetDpus";

/// The wrapper this service hands back from `Service::override_operation`: it serves the
/// operation the routing table names and then completes what that walk rendered
/// ([`with_refusal_body`]).
///
/// A `static` so the reference that seam returns outlives the call, and one value for the
/// whole process because the wrapper holds nothing per service but that one function.
pub static REFUSAL_BODY: RestatedRefusalOperation = RestatedRefusalOperation::new(stated_body);

/// How this service states a refusal the framework raised before any handler ran — the whole
/// `Service::override_refusal` body.
///
/// A constraint refusal is this service's `InvalidRequestException`, worded as AWS words a
/// validation failure, with the renderings the module header states — except where the
/// service states the broken rule in a sentence of its OWN ([`own_sentence`]), which is then
/// the sentence, since that is the rule the request broke. Anything else —
/// a caller request authentication refused — keeps the framework's own code and wording, so
/// a 403 is not restated as a bad request.
#[must_use]
pub fn stated_refusal(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        // Not a refusal about the request's VALUES: an unauthenticated caller keeps the
        // framework's own 403 code and wording.
        return None;
    };
    // Only the ORDER of an enumerated type's value set and the BOUND of a clause the service
    // states its own bound for move, and both move before the envelope is rendered; every
    // clause, its member path and its requirement stay the framework's. A clause this service
    // states in a sentence of its OWN is not stated here at all ([`own_sentence`]).
    let stated: Vec<Violation> = violations
        .iter()
        .map(service_common::validation_message::in_hashed_enum_order)
        .filter(|violation| own_sentence(violation).is_none())
        .map(at_requested_capacity_bound)
        .collect();
    if stated.is_empty() {
        // Nothing is left for the envelope because every clause the framework wrote is one
        // this service states itself, so that statement is the answer: it is the rule the
        // request broke. A refusal carrying no clause at all states nothing here and keeps the
        // framework's own code and wording.
        return violations
            .iter()
            .find_map(own_sentence)
            .map(|message| Refused {
                code: INVALID_REQUEST_EXCEPTION.to_string(),
                message,
            });
    }
    let stated = service_common::validation_message::aws_validation_refusal_withholding_violations(
        &Refusal::Invalid(stated),
        withholds_value,
    )?;
    Some(Refused {
        // The code travels with the message, as it must: a client branches on the code.
        code: INVALID_REQUEST_EXCEPTION.to_string(),
        message: stated.message,
    })
}

/// The sentence this service states `violation` in ITSELF, where it has one, and `None`
/// where the refusal is stated in AWS's validation wording.
///
/// The ONE place that choice is made: a clause this answers for is not stated in the envelope
/// either, because a rule the service words itself is not also a numeric requirement the
/// framework words.
///
/// A modelled bound the service also states as a rule of its own is reported as that rule:
/// the amount of provisioned capacity a reservation asks for is allocated in blocks, and the
/// `@range` minimum on `TargetDpusInteger` — the only bound that shape declares — is the size
/// of one block, so an amount of capacity that is not a whole number of blocks is refused with
/// the service's own sentence for an amount it cannot allocate
/// ([`crate::state::capacity_dpus_not_in_increments`]) rather than with a clause about a
/// numeric bound. That is also what keeps the refusal of such an amount one sentence whether
/// the framework measured it against the model's bound or the handler against the rule.
///
/// An amount that states no capacity at all broke a different rule — the bound a request's
/// amount has to be positive ([`crate::state::capacity_dpus_below_minimum`]) — and that one IS
/// a numeric bound, so it keeps the validation wording, with the service's own number for it
/// ([`at_requested_capacity_bound`]).
fn own_sentence(violation: &Violation) -> Option<String> {
    if !measures_requested_capacity(violation) {
        return None;
    }
    let amount = submitted_capacity(violation)?;
    (!crate::state::capacity_dpus_below_minimum(amount))
        .then(crate::state::capacity_dpus_not_in_increments)
}

/// Whether `violation` is the clause the framework raises for the amount of requested
/// capacity being below the `@range` minimum of `TargetDpusInteger`.
///
/// The WHOLE member path, not its last segment: these rules are about the one top-level member
/// both operations that request capacity declare, so a nested member that happened to share
/// its name would not be measured against a bound that is not its own.
fn measures_requested_capacity(violation: &Violation) -> bool {
    violation.path == TARGET_DPUS_MEMBER && matches!(violation.kind, ViolationKind::MinValue(_))
}

/// The amount of capacity the refused request stated, read back off the clause the framework
/// wrote — the value half it echoes — and `None` when that half holds no whole number this
/// service's capacity rules are stated over.
///
/// A clause whose amount cannot be read is left to the envelope, where it is stated as the
/// bound the service applies: the fail-safe direction, since the framework writes that half for
/// every `@range` violation and an unreadable one says nothing about which rule was broken.
fn submitted_capacity(violation: &Violation) -> Option<i32> {
    violation.value.as_deref()?.parse().ok()
}

/// `violation` with the bound a clause about an amount of requested capacity states: the
/// smallest amount a request may state ([`crate::state::MIN_REQUESTED_CAPACITY_DPUS`]) rather
/// than the `@range` minimum the framework measured the value against, which is one whole
/// block and the bound a GENERATED client checks. Every other violation is returned as the
/// framework wrote it.
fn at_requested_capacity_bound(violation: Violation) -> Violation {
    if !measures_requested_capacity(&violation) {
        return violation;
    }
    Violation {
        kind: ViolationKind::MinValue(crate::state::MIN_REQUESTED_CAPACITY_DPUS.to_string()),
        ..violation
    }
}

/// Whether the clause for `violation` states no value at all.
///
/// Every clause but one: a member that was ABSENT is stated as `Value null at '<member>'`,
/// because the absence IS what the clause reports, while a member whose value broke a bound
/// or a pattern is named with the requirement alone and never with the bytes the request
/// carried.
fn withholds_value(violation: &Violation) -> bool {
    !matches!(violation.kind, ViolationKind::MissingRequiredMember)
}

/// The [`service_common::restated_refusal::Restatement`] [`REFUSAL_BODY`] answers through.
///
/// The request and the words noted at the refusal seam are not read: the sentence is already
/// in the body the framework rendered from the code and message [`stated_refusal`] chose, and
/// which body is completed is decided by the error SHAPE it names rather than by the operation
/// that was addressed, since every operation of this service states an input refusal in that
/// one shape.
fn stated_body(_req: &RtRequest, resp: RtResponse, _noted: Option<String>) -> RtResponse {
    with_refusal_body(resp)
}

/// `resp` as this service answers it: a refusal body carrying its sentence in the `Message`
/// member this model declares, and the refusal class under both names the wire gives it.
///
/// Everything else stays with the renderer that owns it — the status, the `__type`, every
/// other member and the whole success path — and a body this rule does not own is returned
/// byte for byte.
#[must_use]
pub fn with_refusal_body(resp: RtResponse) -> RtResponse {
    let reported = service_common::error_detail_member::with_renamed_detail_member(
        resp,
        &[INVALID_REQUEST_EXCEPTION],
        RENDERED_MESSAGE_MEMBER,
        MODELLED_MESSAGE_MEMBER,
    );
    with_refusal_class(reported)
}

/// `resp` with the refusal class written under both of its names, where the body is one this
/// rule owns and carries neither — through the one response-level rewrite `service_common`
/// states for every service whose wire carries a member the layer that rendered the body had
/// no value for. It is per MEMBER and non-destructive, so a body a handler serialized keeps
/// the class that handler stated.
fn with_refusal_class(resp: RtResponse) -> RtResponse {
    service_common::error_detail_member::with_error_body_members(
        resp,
        &[INVALID_REQUEST_EXCEPTION],
        &[
            (ATHENA_ERROR_CODE_MEMBER, ERROR_CODE_INVALID_INPUT),
            (ERROR_CODE_MEMBER, ERROR_CODE_INVALID_INPUT),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_types::body::SdkBody;
    use serde_json::Value;

    /// A violation of `kind` at `path`, holding `value`.
    fn violation(path: &str, kind: ViolationKind, value: Option<&str>) -> Violation {
        Violation {
            path: path.to_string(),
            kind,
            value: value.map(str::to_string),
            sensitive: false,
        }
    }

    /// RULE: one row per constraint class the model declares on this service's inputs — a
    /// member absent, a bound on a value, a bound on a collection's length, a bound on a
    /// string's length and a pattern — so the two renderings are a property of the class and
    /// not of one recorded request.
    #[test]
    fn every_constraint_class_is_stated_in_awss_validation_wording() {
        // (case, the violation, the clause it is stated with)
        let rows: Vec<(&str, Violation, &str)> = vec![
            (
                "a member that was not there at all",
                violation("databaseName", ViolationKind::MissingRequiredMember, None),
                "Value null at 'databaseName' failed to satisfy constraint: \
                 Member must not be null",
            ),
            (
                "a value above its range",
                violation(
                    "maxResults",
                    ViolationKind::MaxValue("50".to_string()),
                    Some("51"),
                ),
                "Value at 'maxResults' failed to satisfy constraint: \
                 Member must have value less than or equal to 50",
            ),
            (
                "a value below its range",
                violation(
                    "maxResults",
                    ViolationKind::MinValue("1".to_string()),
                    Some("0"),
                ),
                "Value at 'maxResults' failed to satisfy constraint: \
                 Member must have value greater than or equal to 1",
            ),
            (
                "a collection longer than its length",
                violation(
                    "queryExecutionIds",
                    ViolationKind::MaxLength(50),
                    Some("51 items"),
                ),
                "Value at 'queryExecutionIds' failed to satisfy constraint: \
                 Member must have length less than or equal to 50",
            ),
            (
                "a string longer than its length",
                violation("statementName", ViolationKind::MaxLength(256), Some("x")),
                "Value at 'statementName' failed to satisfy constraint: \
                 Member must have length less than or equal to 256",
            ),
            (
                "a string outside its pattern, whose anchors the clause drops",
                violation(
                    "statementName",
                    ViolationKind::Pattern("^[a-zA-Z_][a-zA-Z0-9_@:]{1,256}$".to_string()),
                    Some("1bad-name"),
                ),
                "Value at 'statementName' failed to satisfy constraint: \
                 Member must satisfy regular expression pattern: [a-zA-Z_][a-zA-Z0-9_@:]{1,256}",
            ),
            (
                "a value outside an enumerated type, whose set the clause lists out of the \
                 front door's own table",
                violation(
                    "resultConfiguration.encryptionConfiguration.encryptionOption",
                    // As the model declares them
                    // (`smithy_query(query="shape",shape="EncryptionOption")`).
                    ViolationKind::EnumValue(vec![
                        "SSE_S3".to_string(),
                        "SSE_KMS".to_string(),
                        "CSE_KMS".to_string(),
                    ]),
                    Some("SSE_S4"),
                ),
                "Value at 'resultConfiguration.encryptionConfiguration.encryptionOption' \
                 failed to satisfy constraint: \
                 Member must satisfy enum value set: [CSE_KMS, SSE_KMS, SSE_S3]",
            ),
            (
                "a value outside an enumerated type whose table order is its declared one \
                 (`smithy_query(query=\"shape\",shape=\"WorkGroupState\")`)",
                violation(
                    "state",
                    ViolationKind::EnumValue(vec![
                        "ENABLED".to_string(),
                        "DISABLED".to_string(),
                    ]),
                    Some("SUSPENDED"),
                ),
                "Value at 'state' failed to satisfy constraint: \
                 Member must satisfy enum value set: [ENABLED, DISABLED]",
            ),
        ];

        for (case, broken, clause) in rows {
            let stated = stated_refusal(&Refusal::Invalid(vec![broken]))
                .unwrap_or_else(|| panic!("case {case:?}: expected the refusal to be stated"));
            assert_eq!(
                stated.code, INVALID_REQUEST_EXCEPTION,
                "case {case:?}: expected this service's own refusal shape"
            );
            assert_eq!(
                stated.message,
                format!("1 validation error detected: {clause}"),
                "case {case:?}: expected AWS's validation wording, got {:?}",
                stated.message
            );
        }
    }

    /// RULE: the envelope counts the clauses, over as many constraints as one request broke.
    #[test]
    fn a_request_breaking_two_constraints_states_two_clauses() {
        let stated = stated_refusal(&Refusal::Invalid(vec![
            violation(
                "maxResults",
                ViolationKind::MaxValue("50".to_string()),
                Some("51"),
            ),
            violation("expression", ViolationKind::MaxLength(256), Some("x")),
        ]))
        .expect("expected the refusal to be stated");
        assert!(
            stated.message.starts_with("2 validation errors detected: "),
            "expected the envelope to count both clauses, got {:?}",
            stated.message
        );
        for member in ["'maxResults'", "'expression'"] {
            assert!(
                stated.message.contains(member),
                "expected {member} among the clauses, got {:?}",
                stated.message
            );
        }
    }

    /// RULE: a refusal that is not about the request's VALUES keeps the framework's own code
    /// and wording — an unauthenticated caller is answered 403, not as a bad request — and so
    /// does one that names no broken constraint at all.
    #[test]
    fn a_refusal_that_is_not_a_constraint_refusal_is_not_restated() {
        assert_eq!(
            stated_refusal(&Refusal::Unauthenticated),
            None,
            "expected the framework's own wording for a refusal of another class"
        );
        assert_eq!(
            stated_refusal(&Refusal::Invalid(Vec::new())),
            None,
            "expected the framework's own wording for a refusal naming no clause"
        );
    }

    /// RULE: a bound the service states as a rule of its own is reported as that rule, in the
    /// service's sentence for it, wherever the request broke that bound alone — and an amount
    /// that states no capacity at all broke the service's own POSITIVE bound instead, which
    /// keeps the validation wording and names that bound.
    #[test]
    fn a_bound_the_service_states_itself_is_reported_as_that_rule() {
        // (case, the amount the request carried, the sentence it is refused with) —
        // `TargetDpusInteger` declares one bound, the minimum of its `@range`, so every row
        // breaks that one and the service tells the two rules apart from the amount itself.
        let below_the_bound = format!(
            "1 validation error detected: Value at '{TARGET_DPUS_MEMBER}' failed to satisfy \
             constraint: Member must have value greater than or equal to {}",
            crate::state::MIN_REQUESTED_CAPACITY_DPUS
        );
        let inside_a_block = crate::state::capacity_dpus_not_in_increments();
        let rows: Vec<(&str, &str, &str)> = vec![
            ("an amount inside the first block", "1", &inside_a_block),
            (
                "another amount inside the first block",
                "3",
                &inside_a_block,
            ),
            ("no capacity at all", "0", &below_the_bound),
            ("a negative amount", "-4", &below_the_bound),
            ("a far negative amount", "-2147483648", &below_the_bound),
        ];
        for (case, amount, expected) in rows {
            let stated = stated_refusal(&Refusal::Invalid(vec![violation(
                "targetDpus",
                ViolationKind::MinValue("4".to_string()),
                Some(amount),
            )]))
            .unwrap_or_else(|| panic!("case {case:?}: expected the refusal to be stated"));
            assert_eq!(
                stated.code, INVALID_REQUEST_EXCEPTION,
                "case {case:?}: expected this service's own refusal shape"
            );
            assert_eq!(
                stated.message, expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                stated.message
            );
        }

        // A request that broke the service's own bound AND another constraint is stated in the
        // envelope, which names every clause rather than dropping the ones this service has a
        // sentence of its own for — and the clause about the amount names the bound the SERVICE
        // applies rather than the modelled block size.
        let stated = stated_refusal(&Refusal::Invalid(vec![
            violation(
                "targetDpus",
                ViolationKind::MinValue("4".to_string()),
                Some("0"),
            ),
            violation(
                "name",
                ViolationKind::Pattern("^[a-zA-Z0-9._-]+$".to_string()),
                Some("not a name!"),
            ),
        ]))
        .expect("expected the refusal to be stated");
        assert!(
            stated.message.starts_with("2 validation errors detected: "),
            "expected the envelope to count both clauses, got {:?}",
            stated.message
        );
        assert!(
            stated.message.contains(&format!(
                "Value at '{TARGET_DPUS_MEMBER}' failed to satisfy constraint: Member must \
                 have value greater than or equal to {}",
                crate::state::MIN_REQUESTED_CAPACITY_DPUS
            )),
            "expected the clause to name the bound the service applies, got {:?}",
            stated.message
        );

        // An amount that MEETS that bound is not stated in the envelope at all: the clause the
        // framework wrote is about the modelled block size, which is a requirement the amount
        // satisfies as the service states the bound, so only the constraint the request really
        // broke is named.
        let stated = stated_refusal(&Refusal::Invalid(vec![
            violation(
                "targetDpus",
                ViolationKind::MinValue("4".to_string()),
                Some("1"),
            ),
            violation(
                "name",
                ViolationKind::Pattern("^[a-zA-Z0-9._-]+$".to_string()),
                Some("not a name!"),
            ),
        ]))
        .expect("expected the refusal to be stated");
        assert_eq!(
            stated.message,
            "1 validation error detected: Value at 'name' failed to satisfy constraint: \
             Member must satisfy regular expression pattern: [a-zA-Z0-9._-]+",
            "expected only the constraint the request broke, got {:?}",
            stated.message
        );
    }

    /// A response at `status` carrying `body`.
    fn response(status: u16, body: &str) -> RtResponse {
        RtResponse::new(
            aws_smithy_runtime_api::http::StatusCode::try_from(status).expect("a valid status"),
            SdkBody::from(body),
        )
    }

    /// The body of `resp` as JSON.
    fn decoded(resp: &RtResponse) -> Value {
        serde_json::from_slice(resp.body().bytes().expect("an in-memory body"))
            .expect("the body is JSON")
    }

    /// RULE: a refusal body this rule owns reports its sentence in the modelled member and the
    /// class under both names, whether the framework rendered it bare or with a namespaced
    /// `__type`.
    #[test]
    fn a_framework_rendered_refusal_is_completed_into_this_services_body() {
        for (case, body) in [
            (
                "a bare shape name",
                r#"{"__type":"InvalidRequestException","message":"1 validation error detected: x"}"#,
            ),
            (
                "a namespaced shape id",
                r#"{"__type":"com.amazonaws.athena#InvalidRequestException","message":"1 validation error detected: x"}"#,
            ),
        ] {
            let answered = with_refusal_body(response(400, body));
            let document = decoded(&answered);
            assert_eq!(
                document.get(MODELLED_MESSAGE_MEMBER).and_then(Value::as_str),
                Some("1 validation error detected: x"),
                "case {case:?}: expected the sentence under {MODELLED_MESSAGE_MEMBER}, got {document}"
            );
            assert!(
                document.get(RENDERED_MESSAGE_MEMBER).is_none(),
                "case {case:?}: the sentence must travel under ONE member, got {document}"
            );
            for member in [ATHENA_ERROR_CODE_MEMBER, ERROR_CODE_MEMBER] {
                assert_eq!(
                    document.get(member).and_then(Value::as_str),
                    Some(ERROR_CODE_INVALID_INPUT),
                    "case {case:?}: expected the class under {member}, got {document}"
                );
            }
        }
    }

    /// RULE: a body this rule does not own is answered byte for byte as its renderer wrote it
    /// — a success, a refusal of another class, the scaffold's 501, and a refusal a handler
    /// serialized from the modelled shape, which already carries its own class and sentence.
    #[test]
    fn a_body_this_rule_does_not_own_is_answered_unchanged() {
        // (case, status, the body)
        let rows: &[(&str, u16, &str)] = &[
            (
                "a success whose members are not an error's",
                200,
                r#"{"TableMetadataList":[]}"#,
            ),
            (
                "a metastore's refusal, which declares no class member",
                400,
                r#"{"__type":"MetadataException","Message":"the metastore answered"}"#,
            ),
            (
                "the IAM layer's refusal",
                400,
                r#"{"__type":"AccessDeniedException","Message":"not authorized"}"#,
            ),
            (
                "the scaffold's 501, a class this model does not declare",
                501,
                r#"{"__type":"NotImplemented","message":"not implemented in this emulator"}"#,
            ),
            (
                "a refusal a handler serialized from the modelled shape",
                400,
                r#"{"AthenaErrorCode":"QUERY_EXECUTION_NOT_FOUND","Message":"QueryExecution x was not found","ErrorCode":"QUERY_EXECUTION_NOT_FOUND","__type":"InvalidRequestException"}"#,
            ),
        ];

        for (case, status, body) in rows {
            let answered = with_refusal_body(response(*status, body));
            assert_eq!(
                answered.body().bytes().expect("an in-memory body"),
                body.as_bytes(),
                "case {case:?}: expected the body unchanged"
            );
        }
    }
}
