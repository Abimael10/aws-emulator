// SPDX-License-Identifier: CC0-1.0

//! How this front door READS a boolean form value: the lexical space the Query wire carries
//! one in, and the one fixed sentence text outside it is refused with.
//!
//! The rule itself is the PROTOCOL's and belongs to no one service — the Amazon RDS Query
//! front door serves three APIs (Amazon RDS, Amazon DocumentDB and this one), and every
//! boolean member of every operation of each is carried the same way — so it lives in the
//! crate they share (`service_common::query_boolean_values`), and this module is only where
//! THIS service joins it: the namespace its own envelope carries, and the protocol
//! `Service::override_protocol` hands back.
//!
//! Nothing else about the wire changes: routing, the response envelope, the error envelope
//! and every other member's reading stay the framework's.

use service_common::query_boolean_values::QueryBooleanValues;

/// This service's XML namespace, as the model declares it
/// (`smithy_query(query="service_info")` reports `xmlNamespace.uri`).
///
/// Stated here because the protocol below is CONSTRUCTED rather than taken from the
/// generated entry, and the namespace is part of the envelope that entry builds its own
/// protocol with: the replacement has to be the SAME protocol, differing only in how it
/// reads a boolean's text.
const XML_NAMESPACE: &str = "http://rds.amazonaws.com/doc/2014-10-31/";

/// The protocol this service serves: the framework's `awsQuery`, reading a boolean member's
/// text as the Query wire carries one.
pub fn protocol() -> QueryBooleanValues {
    QueryBooleanValues::aws_query(XML_NAMESPACE)
}
