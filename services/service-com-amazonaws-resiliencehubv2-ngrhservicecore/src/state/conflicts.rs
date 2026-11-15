// SPDX-License-Identifier: CC0-1.0

//! The shapes this service's `ConflictException` messages take.
//!
//! `ConflictException` is "Conflict — resource already exists" and carries one
//! `@required` member, `message`, so the wording is all a client has to tell one conflict
//! from another. This service words two of them, and both are worded ONCE here so two
//! operations refusing the same way cannot drift:
//!
//! * [`name_already_exists_message`] — a create whose `name` a live resource already
//!   holds, named by the resource type the model's shape for it carries.
//! * [`two_requests_one_token_message`] — an `@idempotencyToken` reused for creation
//!   parameters it was not first accepted with, which is not a retry of anything.

/// The refusal MESSAGE for a create whose `name` a live resource of `resource_type`
/// already holds.
///
/// `EntityName` is "Resource name (used in ARN — no spaces allowed)", so a name identifies
/// one resource of its type within the account and a second create under it is a conflict.
/// The message names the resource type and the name the REQUEST supplied, and nothing
/// else: the existing resource's ARN is not part of it.
pub fn name_already_exists_message(resource_type: &str, name: &str) -> String {
    format!("{resource_type} with name already exists: {name}")
}

/// The refusal MESSAGE for a `clientToken` presented with creation parameters other than
/// the ones it was first accepted with.
///
/// An `@idempotencyToken` makes a RETRY safe, so one token stands for one create: a token
/// offered for a second, different create is two requests wearing one token, and the
/// refusal says so and tells the caller what to do instead. The only variable part is the
/// token the request supplied.
pub fn two_requests_one_token_message(client_token: &str) -> String {
    format!(
        "Got two different request with the same token: {client_token}, please use a \
         different token"
    )
}
