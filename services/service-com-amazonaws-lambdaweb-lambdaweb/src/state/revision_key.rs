// SPDX-License-Identifier: CC0-1.0

//! Whether the customer managed KMS key a revision would be encrypted under is a key this
//! service can still use — the pre-flight both create paths run BEFORE a revision is stored.
//!
//! `kmsKeyArn` is "the Amazon Resource Name (ARN) of the AWS Key Management Service (AWS KMS)
//! key used to encrypt the revision's code and environment variables"
//! (`smithy_query(query="doc",shape="CreateWebFunctionRevisionRequest",member="kmsKeyArn")`,
//! and the same member on `RevisionConfig`). Building a revision therefore ENCRYPTS under
//! that key, which is a KMS operation: the member's `@length` and `@pattern` can say only
//! that the value is shaped like a key ARN, never that the key is one KMS would perform that
//! operation with. A key that cannot be used could never have encrypted the revision, so the
//! request is refused on the call that can still be corrected rather than accepted into a
//! revision nothing could build or serve — both creates model `ValidationException` (HTTP 400)
//! for exactly that.
//!
//! The READING is not this service's: it is the tree's one reading of a caller-supplied key
//! identifier ([`::service_common::kms_key::key_use_refusal`]), which asks this emulator's own
//! KMS over the in-process callback and answers with KMS's own sentence about the key. Nothing
//! is decided here — this module only says WHEN this service asks and which member it asks
//! about, so the two creates cannot judge the same value differently.

/// The sentence a create is refused with when the key `kms_key_arn` names cannot be used, and
/// `None` when nothing KMS answered says it cannot — including a request that names no key at
/// all, which is a revision encrypted under no customer managed key and has nothing to ask
/// about.
///
/// `request_region` is the Region the call was signed for; a key named by an ARN is resolved in
/// the Region that ARN names.
pub async fn refusal(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    request_region: &str,
    kms_key_arn: Option<&str>,
) -> Option<String> {
    let identifier = kms_key_arn.map(str::trim).filter(|value| !value.is_empty())?;
    ::service_common::kms_key::key_use_refusal(ctx, request_region, identifier).await
}
