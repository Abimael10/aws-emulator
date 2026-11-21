// SPDX-License-Identifier: CC0-1.0

//! What this service's ERROR body carries on the wire, for the class whose answer is not
//! the operation's.
//!
//! RULE: ARC decides the caller's authorization on a call's target in the layer in FRONT of
//! the operation and answers the denial from there, in that layer's envelope rather than in
//! the operation's. The recorded 403s name the error class in the `x-amzn-errortype`
//! response header as a shape of that framework rather than as a shape of this service's own
//! model, and the JSON document they carry is that layer's: the denial text alone, under the
//! member name AWS puts it at (`Message`), with no shape DISCRIMINATOR member. A denial that
//! states no reason — `AccessDeniedException$message` is optional — therefore answers the
//! empty JSON document.
//!
//! WHY it matters: a client that reads the access-denied text out of the member the service
//! populates gets nothing when it is written elsewhere, so the 403 surfaces with an empty
//! reason; and a client that compares, logs or schema-validates the error body — a strict
//! JSON decoder, a recorded-response fixture, body diffing in a test — sees a member AWS
//! never sends.
//!
//! WHERE IT IS APPLIED. Both halves are taken rather than written here:
//!
//! * the MEMBER NAME is modelled, from the overlay model beside the snapshot
//!   (`smithy-model/access-denial.json` + `crate::ops::types::access_denied_types`, see
//!   `smithy-model/LOCAL-ADDITIONS.md`), so the codec writes it from a member schema like
//!   every other member of every other shape;
//! * the DOCUMENT — the error shape's own modelled members and nothing else — is
//!   [`::service_common::rest_json_error_body`]'s, the tree's one `restJson1` codec of this
//!   class, which this service HOLDS rather than copies (and which also documents why the
//!   framework's shared renderer cannot be changed). This service takes it at the one seam it
//!   owns over the answers it writes, [`smithy_server_runtime::Service::override_protocol`],
//!   which hands the dispatch walk [`ZonalShiftRestJson`] and so serializes every failure a
//!   HANDLER raised.
//!
//! NARROWED to [`ACCESS_DENIAL`], by SHAPE NAME: the rule above holds for every operation of
//! this service that declares that class, however the denial was reached, and is keyed on no
//! operation, route or request. That is how far this service's recordings reach — its denials
//! carry no discriminator, and no recorded answer of another class of this service has been
//! compared on one (the `ValidationException` divergences recorded for it name `$.message`,
//! which says nothing about a discriminator either way). The shared module states the
//! unnarrowed rule for the protocol as a whole; widening this service onto it is a change to
//! make when a recording of another class answers for it.
//!
//! One class is left as the framework writes it, and cannot be reached from a service seam:
//! a refusal rendered BEFORE a handler runs (an absent required member, a value outside a
//! modelled bound, an unauthenticated caller), which the dispatch walk renders from the
//! protocol's pre-dispatch ENVELOPE rather than from a response this crate is handed. The
//! status, every header including `x-amzn-errortype`, request deserialization and the whole
//! SUCCESS path stay with the framework renderer that owns them.

/// The error SHAPES this service answers from the layer in front of its operations, and
/// therefore the ones whose document is that layer's: the access denial every operation of
/// this service declares (`@error client`, `@httpError 403`).
pub(super) const ACCESS_DENIAL: &[&str] = &["AccessDeniedException"];

/// This service's wire protocol: `restJson1` exactly as the model declares it, with the
/// error body this service is recorded answering a denial with (see the module docs). The
/// shared codec, under the name this service's seam reads it by.
pub(super) use ::service_common::rest_json_error_body::ServedErrorBodyRestJson as ZonalShiftRestJson;
