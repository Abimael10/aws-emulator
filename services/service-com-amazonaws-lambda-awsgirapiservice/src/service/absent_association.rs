// SPDX-License-Identifier: CC0-1.0

//! WHY a read of a function's code-signing association that finds none still answers a 200,
//! and why that answer carries NO member at all.
//!
//! `GetFunctionCodeSigningConfig` "Returns the code signing configuration for the specified
//! function", and the two members it answers with —
//! [`crate::ops::types::GetFunctionCodeSigningConfigOutput`], which is where that member set
//! is decided — describe ONE attachment between a configuration and the function it covers.
//! A function holding none therefore has no attachment to describe, so neither member is
//! written and what tells a caller the function is covered by no code signing is the absence
//! of the pair.
//!
//! Nothing about that body is corrected here: the one status with no room for a body is the
//! no-content status (`service_common::no_content_responses`), and a 200 of this service
//! answers the JSON object the protocol renders from the output structure, whichever members
//! were written to it — an object with no members at all when the structure carries none.
//!
//! So this seam has no correction to make for such a read, and makes none: the member set is
//! the output type's, the status is the operation's, and the body is the protocol's. The rule
//! is recorded here because this is where a response-shaping correction WOULD live — beside
//! the class of a fault ([`super::fault_class`]) and the public-access flag of a policy read
//! ([`super::public_access`]) — so a later reader does not add one in the belief that none
//! was considered.
