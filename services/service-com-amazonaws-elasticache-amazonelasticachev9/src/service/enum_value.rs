// SPDX-License-Identifier: CC0-1.0

//! How this service words a value outside a modelled enum.
//!
//! A member that targets one of this model's enums is walked by the framework while the
//! request is validated, so a value outside the enum is refused BEFORE the handler runs —
//! structurally, as a [`Refusal::Invalid`] carrying a [`ViolationKind::EnumValue`] at the
//! member's path. The framework's own wording for that violation names the constraint
//! (`Member must satisfy enum value set: [...]`) under the generic validation code, which is
//! a code NO operation of this service models: a client catching the modelled
//! `InvalidParameterValueException` never catches it. ElastiCache states such a value as the
//! bad parameter value it is — HTTP 400, `InvalidParameterValue` — with the sentence it
//! words for THAT member.
//!
//! The sentences are per member rather than one for the whole service, because the service
//! words them that way: one lists the vocabulary it serves and interpolates nothing, and the
//! status filters name the value the request sent. So this module is a TABLE
//! ([`WORDED_MEMBERS`]) keyed by the member the violation is at.
//!
//! A member the table does not hold keeps the framework's whole answer — its code as well as
//! its diagnosis, which at least names the value and the set it missed. Half of it is not
//! reworded, for two reasons. The code and the sentence are ONE choice: a client told
//! `InvalidParameterValue` reads the sentence beside it, and the code alone says no more than
//! the status already does. And this seam is service-wide: it is handed the member the
//! violation is at and not the operation that was refused, so it cannot check that the code it
//! would name is one that operation models — this model declares no service-common errors
//! (`smithy_query(query="service_info")`), every error is per operation, and naming an
//! unmodelled code is the very defect a reworded refusal is for. A member joins the table when
//! the sentence AWS words for it is known, which is one row.

use smithy_server_runtime::{Refusal, Refused, Violation, ViolationKind};

/// The awsQuery error code ElastiCache names a bad parameter value with, which is what the
/// `InvalidParameterValueException` the operations taking these members model renders as.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// One member whose enum refusal this service words itself.
///
/// A named record rather than a tuple, so each part says what it is where it is read.
struct WordedMember {
    /// The member, as the framework spells the LEADING segment of a violation's path:
    /// lowerCamel, with the position inside a collection following it.
    member: &'static str,
    /// The sentence the refusal carries, built from the value the request sent — which a
    /// wholly fixed sentence ignores.
    sentence: fn(&str) -> String,
}

/// Every member of this API whose enum refusal ElastiCache words itself.
///
/// `sourceType` is `DescribeEvents$SourceType`, the one member targeting the `SourceType`
/// enum; the two status filters are `ServiceUpdateStatus` and `UpdateActionStatus`, which
/// `DescribeServiceUpdates` and `DescribeUpdateActions` take as lists of enum members, so a
/// violation at either arrives at `<member>.<n>.member`.
const WORDED_MEMBERS: &[WordedMember] = &[
    WordedMember {
        member: "sourceType",
        sentence: invalid_event_source_type,
    },
    WordedMember {
        member: "serviceUpdateStatus",
        sentence: invalid_service_update_status,
    },
    WordedMember {
        member: "updateActionStatus",
        sentence: invalid_update_action_status,
    },
];

/// `DescribeEvents$SourceType`: the whole sentence is fixed text
/// ([`super::event_source_type`]), so the rejected value is not named at all.
fn invalid_event_source_type(_value: &str) -> String {
    super::event_source_type::INVALID_EVENT_SOURCE_TYPE.to_string()
}

/// `ServiceUpdateStatus`: fixed text around the status the request sent.
fn invalid_service_update_status(value: &str) -> String {
    format!("The input service update status {value} is not valid.")
}

/// `UpdateActionStatus`: the same shape of sentence as its sibling status filter
/// ([`invalid_service_update_status`]), naming the status the request sent.
///
/// INFERRED from that sibling rather than observed: what is recorded for this member is the
/// parameter-combination refusal it gets without a `ServiceUpdateName`
/// ([`super::parameter_combination`]), which is answered first and is what a request naming no
/// update reads. A later recording of this member's own refusal corrects the wording here.
fn invalid_update_action_status(value: &str) -> String {
    format!("The input update action status {value} is not valid.")
}

/// How this service words `refusal` when it is a value outside a modelled enum at a member
/// [`WORDED_MEMBERS`] holds, or `None` to leave it to the renderer that would otherwise
/// answer.
///
/// Only an ENUM violation is reworded, and only at those members: a request refused for
/// anything else keeps the diagnosis that names what it failed.
pub fn enum_value_refusal(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    // The FIRST such violation, which is the value a caller fixing the request changes next.
    let (worded, violation) = violations.iter().find_map(|violation| {
        if !matches!(violation.kind, ViolationKind::EnumValue(_)) {
            return None;
        }
        Some((worded_member(violation)?, violation))
    })?;
    Some(Refused {
        code: INVALID_PARAMETER_VALUE.to_string(),
        // A `@sensitive` member carries no value in the violation; none of the members here
        // is sensitive, and a sentence that interpolates nothing does not read one anyway.
        message: (worded.sentence)(violation.value.as_deref().unwrap_or_default()),
    })
}

/// The table row for the member `violation` is at, or `None` when this service words no
/// sentence of its own for it.
fn worded_member(violation: &Violation) -> Option<&'static WordedMember> {
    let member = violation
        .path
        .split('.')
        .next()
        .unwrap_or(violation.path.as_str());
    WORDED_MEMBERS
        .iter()
        .find(|worded| worded.member == member)
}
