// SPDX-License-Identifier: CC0-1.0

//! What this service's ERROR body carries on the wire.
//!
//! RULE: this service speaks `restJson1` (`smithy_query(query="service_info")` reports
//! protocols `[restJson1]` for `AWSLakeFormation` 2017-03-31), and a `restJson1` failure
//! names its error SHAPE in the `x-amzn-errortype` response header, so the BODY of a
//! failure carries the error shape's own modelled members and nothing else — no shape
//! DISCRIMINATOR member. Every error shape of this model declares the single member
//! `Message` (`smithy_query(query="shape",shape="InvalidInputException")` and its
//! neighbours), so a refusal's body is exactly that one member.
//!
//! WHY it matters: a client that compares, logs or schema-validates the error body — a
//! strict JSON decoder, a recorded-response fixture, body diffing in a test — sees a member
//! AWS never sends, and tooling that keys off `__type` reads a discriminator that would be
//! absent against AWS, so the error class is derived from the wrong place.
//!
//! WHERE IT IS APPLIED. The rule, the codec that serves it and why the framework's shared
//! renderer cannot be changed are
//! [`::service_common::rest_json_error_body`]'s — the tree's one `restJson1` codec of this
//! class, which this service holds rather than copies. This service takes it at the two
//! seams it owns over the answers it writes:
//!
//! * [`smithy_server_runtime::Service::override_protocol`] hands the dispatch walk
//!   [`LakeFormationRestJson`], which serializes every failure a HANDLER raised;
//! * [`smithy_server_runtime::Service::override_routing_error`] answers a request this
//!   service claimed that names no modelled operation, through
//!   [`with_served_error_body`].
//!
//! One class is left as the framework writes it, and cannot be reached from a service seam:
//! a refusal rendered BEFORE a handler runs (an absent required member, a value outside a
//! modelled bound, an unauthenticated caller). The dispatch walk renders those from the
//! protocol's pre-dispatch ENVELOPE — a framework enum describing which protocol family
//! states the failure, not a response this crate is handed — and `override_refusal` chooses
//! only the code and the message inside it. The status, every header including
//! `x-amzn-errortype`, request deserialization and the whole SUCCESS path stay with the
//! framework renderer that owns them.

/// This service's wire protocol: `restJson1` exactly as the model declares it, with the
/// error body this service is recorded answering (see the module docs). The shared codec,
/// under the name this service's seams read it by.
pub use ::service_common::rest_json_error_body::ServedErrorBodyRestJson as LakeFormationRestJson;

/// One already-rendered failure as this service answers it, for the seam that is handed a
/// whole response rather than driving the codec.
pub use ::service_common::error_type_locator::without_error_shape_in_body as with_served_error_body;
