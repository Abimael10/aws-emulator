// SPDX-License-Identifier: CC0-1.0

//! Whether the customer managed key a `kmsEncryptionKey` names EXISTS — the question every
//! operation that accepts that member asks before it records an encryption configuration.
//!
//! `KmsEncryptionKey` is constrained only by length and pattern (`@length(1, 2048)`,
//! `^[a-zA-Z0-9:/_-]+$`, `smithy_query(query="shape", shape="KmsEncryptionKey")`), and the
//! member admits several forms of identifier ("You can specify the ARN, ID, or alias",
//! `smithy_query(query="doc", shape="UpdateClusterInput", member="kmsEncryptionKey")`) — so
//! whether the value names a key at all is not a boundary check. It is resolved against AWS
//! KMS, the component that OWNS keys, and a value KMS holds no key for is refused with the
//! modelled `ValidationException` BEFORE the cluster's `encryptionDetails` is written: a
//! cluster encrypted under a key that does not exist could never be read back, and the
//! refusal belongs on the call that can still be corrected rather than on every later read of
//! the cluster.
//!
//! The reserved value that selects the key owned by Amazon Web Services names no customer key
//! at all ([`crate::state::customer_managed_key`]), so there is nothing to resolve for it and
//! a request carrying it is never refused here.
//!
//! The resolution itself is [`::service_common::kms_key::key_is_absent`], the tree's one
//! reading of a key identifier against KMS, so what this service refuses on is grounded in
//! what KMS ANSWERED: a key of another account, an answer that is about KMS rather than about
//! the key, an answer this emulator could not read and an emulator with no KMS mounted to ask
//! establish nothing, and the write goes through. That is the bar the rest of this service's
//! cross-service resolutions are held to — a policy document's principals
//! ([`crate::state::unresolved_policy_principal`]), a stream's delivery target — and this one
//! is worded beside them.

use crate::ops::types::ValidationException;

/// The sentence a `kmsEncryptionKey` naming a key KMS holds none of is refused with.
///
/// Fixed text for the error class — "AWS KMS key " and " not found. Verify the key ID or ARN
/// and ensure the key exists in your AWS account and Region." — around the ONE variable part,
/// the key identifier exactly as the request supplied it, whichever of the forms the member
/// admits it was written in.
pub fn key_not_found_message(identifier: &str) -> String {
    format!(
        "AWS KMS key {identifier} not found. Verify the key ID or ARN and ensure the key \
         exists in your AWS account and Region."
    )
}

/// The refusal a request naming a key that does not exist earns, or `None` when the value
/// names a key KMS answered for (or no customer key at all).
///
/// `request_region` is the Region the call was signed for; which Region the key is looked for
/// in is [`::service_common::kms_key::key_is_absent`]'s reading of the identifier.
///
/// No `fieldList`: the value is well formed and names nothing, so this is not a member whose
/// value failed a declared constraint — the same reading
/// [`crate::state::unresolved_policy_principal`] takes of a document that grants to nobody,
/// and the `reason` is that same `other`.
pub async fn unresolved_encryption_key(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    kms_encryption_key: &str,
) -> Option<ValidationException> {
    let customer_key = super::customer_managed_key(Some(kms_encryption_key))?;
    if !::service_common::kms_key::key_is_absent(ctx, request_region, customer_key).await {
        return None;
    }
    Some(ValidationException {
        message: key_not_found_message(customer_key),
        reason: super::VALIDATION_REASON_OTHER.to_string(),
        field_list: None,
    })
}
