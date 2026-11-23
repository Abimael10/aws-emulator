// SPDX-License-Identifier: CC0-1.0

//! How this endpoint STATES the boundary validator's refusal of a request whose value one
//! of the model's constraints does not admit.
//!
//! The envelope is the shared AWS one — `N validation error(s) detected: …` over one clause
//! per constraint the request broke, the member paths spelled as the wire spells them —
//! and nothing about it is written here
//! ([`::service_common::validation_message::aws_validation_refusal_withholding_with`]).
//! Two renderings of a clause are this endpoint's, and they are all this module chooses:
//!
//! * THE VALUE HALF. A clause of this family states the REQUIREMENT a value had to satisfy
//!   and never the value that missed it, whatever constraint class it broke: a request
//!   refused at the boundary is answered a sentence that carries none of its own bytes, so
//!   a media identifier, a cursor or a fragment number a caller sent is not read back out
//!   of this error channel. The Kinesis Video Streams control plane in front of this data
//!   plane states its own clauses the same way (`states_no_value` in
//!   `service-com-amazonaws-kinesisvideo-kinesisvideo-20170930`), so the two halves of one
//!   API do not word one refusal two ways.
//! * THE EXPRESSION A `@pattern` CLAUSE QUOTES. A clause quotes the expression this API
//!   PUBLISHES for the member's shape — the `Pattern` its reference documents — which is
//!   the expression the service itself constrains the value by and is not always the
//!   model's trait text: a shape whose published expression is the inner one carries the
//!   whole-value anchors only in the model, and a shape whose published expression names
//!   no single ARN partition is written in the model with one partition fixed. Which of
//!   the two a shape is published with cannot be read off the trait text, so it is
//!   reference data about the shape ([`PUBLISHED_AS_DECLARED`], [`PUBLISHED_INSTEAD`]),
//!   keyed by the EXPRESSION, since what is published belongs to the shape and so holds
//!   for every member that targets it.
//!
//! REPORTED, not ENFORCED, as for every `@pattern` rendering in
//! `services/common/src/validation.rs`: the constraint walk keeps applying the model's
//! trait exactly as the model writes it, so which values this endpoint admits is the
//! model's and only the quoting is the API's.

use ::smithy_server_runtime::{Refusal, Refused};

/// The expressions this API publishes EXACTLY as the model declares them, anchors
/// included.
///
/// Source: the `Pattern` each shape's entry in this API's reference carries, for the
/// shapes whose published expression is the model's own text —
/// `StartSelector$AfterFragmentNumber` -> `FragmentNumberString` and
/// `StartSelector$ContinuationToken` -> `ContinuationToken`, the latter shared with
/// `GetMediaOutput$ContentType`
/// (`smithy_query(query="closure", operation="GetMedia")` lists every constrained member
/// of this service's one operation).
const PUBLISHED_AS_DECLARED: &[&str] = &["^[0-9]+$", r"^[a-zA-Z0-9_\.\-]+$"];

/// The expressions this API publishes as something OTHER than the model's trait text,
/// each paired with the text it publishes.
///
/// One entry: `ResourceARN`, the shape `GetMedia$StreamARN` targets. The model pins the
/// partition (`arn:aws:`) while the expression published for the shape admits any
/// partition an ARN names, so dropping the whole-value anchors alone would not recover it
/// — the published text is held here in full rather than derived from the trait, because
/// no rule reads one from the other.
const PUBLISHED_INSTEAD: &[(&str, &str)] = &[(
    "^arn:aws:kinesisvideo:[a-z0-9-]+:[0-9]+:[a-z]+/[a-zA-Z0-9_.-]+/[0-9]+$",
    r"arn:[a-z\d-]+:kinesisvideo:[a-z0-9-]+:[0-9]+:[a-z]+/[a-zA-Z0-9_.-]+/[0-9]+",
)];

/// The expression a `@pattern` clause of this API quotes for a member whose model trait is
/// `regex`: the text this API publishes for that member's shape.
///
/// An expression [`PUBLISHED_INSTEAD`] names is published as the text beside it; every
/// other one is published as the inner expression the model wrapped in whole-value anchors
/// unless [`PUBLISHED_AS_DECLARED`] names it, which is the shared reading of that wrapping
/// (`service_common::validation::unanchored_unless_published_as_declared`) rather than a
/// second one written here.
pub(crate) fn published_pattern(regex: &str) -> String {
    if let Some((_, published)) = PUBLISHED_INSTEAD
        .iter()
        .find(|(declared, _)| *declared == regex)
    {
        return (*published).to_string();
    }
    ::service_common::validation::unanchored_unless_published_as_declared(
        regex,
        PUBLISHED_AS_DECLARED,
    )
}

/// How this service words a request the boundary validator refused before any handler ran,
/// or `None` to keep the framework's own wording for a refusal that is not about a value
/// at all (an unauthenticated caller keeps the framework's status and sentence).
///
/// The code travels with the message, as it does wherever that envelope is stated: the
/// generic constraint error of this protocol, which is also the one the framework's own
/// renderer picks here, so a client branching on the code sees no change.
pub(crate) fn stated_refusal(refusal: &Refusal) -> Option<Refused> {
    ::service_common::validation_message::aws_validation_refusal_withholding_with(
        refusal,
        published_pattern,
        // Every clause, whatever member it is about and whatever class it broke: the
        // value half is absent from this family's envelope rather than from some of its
        // members.
        |_member| true,
    )
}
