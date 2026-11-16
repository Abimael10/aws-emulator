// SPDX-License-Identifier: CC0-1.0

//! WHY a read of a function's code-signing association that finds none still answers a body.
//!
//! `GetFunctionCodeSigningConfig` "Returns the code signing configuration for the specified
//! function", so what it answers names the FUNCTION it reports on either way, with the
//! configuration's ARN beside it exactly when the function holds one (see
//! [`crate::ops::types::GetFunctionCodeSigningConfigOutput`], which is where that member set
//! is decided).
//!
//! A function holding none therefore still has a member to report — its own name, which the
//! request resolved — at the operation's own 200, because the function exists and this is a
//! report rather than a refusal. Nothing about that body is corrected here: the one status
//! with no room for a body is the no-content status
//! (`service_common::no_content_responses`), and a 200 of this service answers the JSON
//! object the protocol renders from the output structure, whichever members were written to
//! it.
//!
//! So this seam has no correction to make for such a read, and makes none: the member set is
//! the output type's, the status is the operation's, and the body is the protocol's. The rule
//! is recorded here because this is where a response-shaping correction WOULD live — beside
//! the class of a fault ([`super::fault_class`]) and the public-access flag of a policy read
//! ([`super::public_access`]) — so a later reader does not add one in the belief that none
//! was considered.
