// SPDX-License-Identifier: CC0-1.0

//! How this front door READS a boolean form value: the lexical space the Query wire carries
//! one in.
//!
//! The rule is the PROTOCOL's and belongs to no one service — every boolean member of every
//! operation of an awsQuery front door is carried as the TEXT of a form field, and the values
//! that text may hold are the lexical space of an XML Schema boolean — so it lives in the
//! crate the services share (`service_common::query_boolean_values`), and this module is only
//! where THIS service joins it: the namespace its own envelope carries, and the protocol
//! `Service::override_protocol` hands back.
//!
//! Two things follow for this service. A request spelling a boolean the numeric way is read
//! as the boolean it spells, where the framework's own vendored reader takes the two words
//! only; and text in no spelling of the space is refused by the protocol layer that reads it,
//! which is the one unreadable class this service answers under its argument-refusal code
//! (see [`crate::service::malformed_input`]).
//!
//! Nothing else about the wire changes: routing, the response envelope, the error envelope
//! and every other member's reading stay the framework's.

use service_common::query_boolean_values::QueryBooleanValues;

/// This service's XML namespace, as the model declares it
/// (`smithy_query(query="service_info")` reports `xmlNamespace.uri`).
///
/// Stated here because the protocol below is CONSTRUCTED rather than taken from the generated
/// entry, and the namespace is part of the envelope that entry builds its own protocol with:
/// the replacement has to be the SAME protocol, differing only in how it reads a boolean's
/// text.
const XML_NAMESPACE: &str = "http://autoscaling.amazonaws.com/doc/2011-01-01/";

/// The protocol this service serves: the framework's `awsQuery`, reading a boolean member's
/// text as the Query wire carries one.
pub fn protocol() -> QueryBooleanValues {
    QueryBooleanValues::aws_query(XML_NAMESPACE)
}
