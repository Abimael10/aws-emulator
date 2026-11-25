// SPDX-License-Identifier: CC0-1.0

//! How this service words a request its own modelled constraints refuse, before any
//! handler runs.
//!
//! The envelope is AWS's and the framework's — `N validation error(s) detected: Value
//! '<value>' at '<member path>' failed to satisfy constraint: <requirement>`, rendered by
//! [`::service_common::validation_message::aws_validation_refusal`] — and THREE renderings
//! inside it are this service's, each of them a property of the SHAPE a member targets or
//! of the RESOURCE a member addresses rather than of the operation or the request that
//! carried the value:
//!
//! * a `@pattern` requirement quotes the expression the service PUBLISHES for the shape,
//!   which is the model's text without the whole-value anchors Smithy wraps it in
//!   (`::service_common::validation::unanchored`) — so a caller lifting the expression out
//!   of the message and compiling it locally gets the one the service advertises. The
//!   CHECK still runs the model's pattern as written; only the quoting drops the pair, and
//!   an expression the model leaves unanchored is quoted exactly as declared.
//! * a member that ADDRESSES one of this service's resources by its id publishes a WIDER
//!   expression than the model's id expression: the service accepts either that id or a
//!   reference to the resource it names — the resource's own ARN, or an alias of it — so
//!   the expression such a clause quotes is the grouped alternation of the two
//!   ([`published_pattern`]). The model declares only the id half, because the reference
//!   half is not expressible as one `Uuid` shape; the CHECK is still the model's, so which
//!   values are admitted does not change, only what the refusal reports as acceptable.
//! * a value outside an enumerated type is refused naming the values that type admits in
//!   the order the front door's own TABLE of them is walked — ascending bucket, the bucket
//!   being the low bits of the value's string hash
//!   ([`::service_common::validation_message::in_hashed_enum_order`], this tree's one home
//!   for that derivation) — and not in the order the model declares them in. The order is
//!   COMPUTED from the values themselves, so it belongs to the enumerated TYPE: every
//!   member targeting one enum is listed alike, in any operation and at any depth of the
//!   request, and a type the model gains needs nothing added here.
//!
//! Everything else stays the framework's: the counted envelope, the member paths, the
//! value each clause echoes, every other requirement's wording and the error code, so this
//! service cannot word a constraint differently from the services stating the same
//! sentence. A refusal that is not about the request's values — an unauthenticated caller
//! — is not this service's to word at all.
//!
//! One half of a clause cannot be stated from here at all: the COLLECTION a clause about a
//! collection member is about, which the framework's walk summarises as a count because
//! this seam is handed the violations and never the request they were found in. That half
//! is [`super::submitted_collections`], a per-operation layer which has the request — and
//! which words the whole refusal through [`refused`] below, so the two paths cannot state
//! one constraint two ways.

use ::service_common::validation_message::{aws_validation_refusal, in_hashed_enum_order};
use ::smithy_server_runtime::{Refusal, Refused, Violation, ViolationKind};

/// The resources of this service a member can ADDRESS by id, each paired with the resource
/// segment its own ARNs carry.
///
/// Source: this service's ARN forms, which are the ones its handlers mint and report —
/// `crate::state::trust_anchor_arn`, `profile_arn`, `crl_arn` and `subject_arn`, over the
/// `TrustAnchorArn` / `ProfileArn` patterns the model declares
/// (`smithy_query(query="shape", shape="TrustAnchorArn,ProfileArn")`:
/// `^arn:aws(-[^:]+)?:rolesanywhere(:.*){2}(:trust-anchor.*)$` and `(:profile.*)`). Every
/// member named here targets the modelled `Uuid`
/// (`smithy_query(query="shape", shape="Uuid")`), wherever the request carries it — in the
/// URI as an `@httpLabel` or in the body — so one resource's id is refused alike whichever
/// operation addressed it.
const ADDRESSED_RESOURCES: &[(&str, &str)] = &[
    ("trustAnchorId", "trust-anchor"),
    ("profileId", "profile"),
    ("crlId", "crl"),
    ("subjectId", "subject"),
];

/// How this service states `refusal`, or `None` to keep the framework's own wording.
pub(crate) fn refused(refusal: &Refusal) -> Option<Refused> {
    let Refusal::Invalid(violations) = refusal else {
        return None;
    };
    // Each violation is restated BEFORE the envelope words it, so only the requirement
    // half of an enum clause and of a resource-id clause moves and every other violation
    // class reaches the envelope untouched.
    let stated = Refusal::Invalid(violations.iter().map(stated).collect());
    aws_validation_refusal(&stated)
}

/// `violation` with the REQUIREMENT half of its clause worded the way this service words
/// it, and every other half left exactly as the framework built it.
///
/// Both restatements are decided by the shape the member targets or the resource it
/// addresses, never by the operation or the request, so they are applied here — in the one
/// place every refusal of this service passes through — rather than per operation.
pub(crate) fn stated(violation: &Violation) -> Violation {
    let stated = in_hashed_enum_order(violation);
    let ViolationKind::Pattern(declared) = &stated.kind else {
        return stated;
    };
    let Some(published) = published_pattern(&stated.path, declared) else {
        return stated;
    };
    Violation {
        kind: ViolationKind::Pattern(published),
        ..stated
    }
}

/// The expression this service publishes for a member that addresses one of its resources
/// by id, or `None` for every other member — whose `@pattern` is quoted as the model
/// declares it, less the whole-value anchors.
///
/// The published expression is the grouped alternation of the id expression the model
/// declares for the member and the reference to the resource that id names: the resource's
/// own ARN, or an alias of it, in any partition and any Region and account the ARN form
/// admits. Grouped, because the alternation is one expression a value has to match as a
/// whole rather than two loose branches.
///
/// The member is read off the LAST segment of the path the clause is stated at, so a
/// resource id nested inside a request structure is published the same way as one bound to
/// the URI.
fn published_pattern(member_path: &str, declared: &str) -> Option<String> {
    let member = member_path.split('.').next_back()?;
    let (_, resource) = ADDRESSED_RESOURCES
        .iter()
        .find(|(addressed, _)| *addressed == member)?;
    // The id half is the model's own expression as this service quotes any other one: its
    // whole-value anchors belong to the model's spelling, not to the published expression.
    let id = ::service_common::validation::unanchored(declared);
    Some(format!(
        "({id}|arn:aws(-[^:]+)?:{namespace}(:.*){{2}}:({resource}|alias/{resource})/.*)",
        namespace = crate::state::ARN_NAMESPACE,
    ))
}
