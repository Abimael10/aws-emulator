// SPDX-License-Identifier: CC0-1.0

//! Tombstone. This module briefly held a hand-written copy of one rule — a Smithy `double`
//! rendered on the XML wire with the fractional part AWS always gives one. That rule has
//! one home for the whole fleet, `service_common::query_decimal_floats`, and this service
//! reaches it through [`crate::service::decimal_floats`], which serves every operation
//! whose response can carry a floating-point member behind it.
//!
//! Left undeclared and empty rather than carrying code: nothing should be added here.
