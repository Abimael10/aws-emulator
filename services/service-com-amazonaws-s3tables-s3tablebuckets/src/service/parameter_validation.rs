// SPDX-License-Identifier: CC0-1.0

//! The refusal this service does NOT word itself: a value a modelled constraint
//! rules out on a member the service states no sentence of its own for.
//!
//! Two layers refuse a request before any handler runs, and a caller tells them
//! apart by the error code:
//!
//! * the SERVICE refuses what it validates itself — the names a create gives a
//!   resource, the ARNs an operation is addressed by, the encryption and format
//!   members it will not act on — with the one client-input error its model
//!   declares (`BadRequestException`) and one fixed sentence per class, which
//!   [`super::fixed_sentence`] is the list of;
//! * every OTHER modelled constraint is checked by the parameter validator in
//!   front of the service, which answers under its own code — a code that names
//!   no shape of this service's model, which is exactly what tells a client the
//!   request was refused for the SHAPE of a parameter rather than by a rule of
//!   the service.
//!
//! Nothing about that second refusal is worded here. The code, the envelope, the
//! member paths, the requirement per constraint class, the `@pattern` rendering
//! and the ORDER an enumerated type's value set is listed in are the whole
//! workspace's ([`service_common::validation_message`]), so no two crates can
//! word one constraint two ways. This module is only WHICH of the choices that
//! module leaves to a service this one makes.

use aws_smithy_schema_server::constraints::Violation;
use smithy_server_runtime::{Refusal, Refused};

/// How this service answers `refusal` when it words none of its violations
/// itself, or `None` when there is nothing to state (which leaves the
/// framework's own wording).
///
/// Two choices are this service's, and each is about one clause rather than
/// about the envelope:
///
/// * EVERY clause withholds the value half: its refusals are recorded stating
///   the member path and the requirement alone — `Value at '<member path>'
///   failed to satisfy constraint: <requirement>` — whatever constraint the
///   value broke, so the same refusal reads identically whatever was sent and
///   nothing of the request comes back;
/// * the value set an ENUMERATED type's requirement lists is stated
///   lexicographically ([`sorted_enum_sets`]), whatever order the model declares
///   that type's members in.
pub fn refusal(refusal: &Refusal) -> Option<Refused> {
    service_common::validation_message::aws_validation_refusal_withholding_violations(
        &sorted_enum_sets(refusal),
        |_| true,
    )
}

/// `refusal` with every enumerated type's value set stated in lexicographic
/// order, and anything else handed on unchanged.
///
/// A refusal of a value outside an enumerated type names the values that type
/// admits, and this service states that set sorted rather than in the order the
/// model happens to declare it in. The order belongs to the TYPE, so one enum
/// reads the same whichever member, operation or depth carried the value, and a
/// type the model gains or reorders needs nothing here.
///
/// The re-ordering happens BEFORE the envelope is rendered, so the clause — the
/// member path, the requirement's wording and the brackets around the set — is
/// still written in the one place the whole workspace writes it
/// (`service_common::validation_message::in_sorted_enum_order`). It is applied on
/// THIS path alone: the sentences the service words itself
/// ([`super::stated_sentence`]) read the class off the set the MODEL declares.
fn sorted_enum_sets(refusal: &Refusal) -> Refusal {
    match refusal {
        Refusal::Invalid(violations) => Refusal::Invalid(
            violations
                .iter()
                .map(service_common::validation_message::in_sorted_enum_order)
                .collect::<Vec<Violation>>(),
        ),
        other => other.clone(),
    }
}
