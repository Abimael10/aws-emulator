// SPDX-License-Identifier: CC0-1.0

//! Resolving the customer managed key a write states, against KMS.
//!
//! `KmsKeyArn` is "the Amazon Resource Name (ARN) for the customer managed KMS key that
//! EventBridge Scheduler will use to encrypt and decrypt your data"
//! (`smithy_query(query="shape",shape="CreateScheduleInput",member="KmsKeyArn")`). That is a
//! REFERENCE to a resource of another service, and the member's `@length` and `@pattern` can
//! say only that a value is SHAPED like a key ARN — never that a key of that name is there.
//! EventBridge Scheduler has to reach the key to encrypt the schedule it is being asked to
//! store, so it resolves the reference as it takes it: a write stating a key this account and
//! Region hold none of is refused with the service's modelled `ValidationException` (HTTP
//! 400) and persists nothing, so a client never holds a schedule pointing at a key its
//! occurrences could never be encrypted under.
//!
//! The question — does this account and Region hold a key this identifier resolves to — is
//! KMS's, and it is asked through the ONE reading of a caller-supplied key identifier every
//! service in this tree shares (`service_common::kms_key`): the AWS SDK driven over the
//! emulator's own in-process transport, so a key is resolved by the very handlers a client's
//! own `DescribeKey` would reach. Nothing it answers OTHER than "there is no such key"
//! refuses a write — an emulator assembled without a KMS to ask, a fault, a key of another
//! account — because a refusal has to be grounded in what KMS said.
//!
//! The same reference is resolved by the UPDATE as by the create, because both state the
//! schedule's whole configuration: an update stating a key that is not there is refused with
//! the same sentence and leaves the stored schedule — key included — exactly as the last
//! accepted write left it.

/// How EventBridge Scheduler words a refusal of a write whose customer managed key it
/// cannot reach.
///
/// FIXED text around ONE variable part: the key the request named, interpolated as the
/// request stated it, because that is the reference the client has to correct.
#[must_use]
pub fn kms_key_refusal(kms_key_arn: &str) -> String {
    format!(
        "Unable to access the KMS key {kms_key_arn}. Check that the key exists, and that the \
         IAM principal you use to call EventBridge Scheduler has permission to access the key, \
         then try again."
    )
}

/// Refuse a write whose `KmsKeyArn` names a key KMS answers this account and Region hold
/// none of — and `None` for a write that states no key at all, which asks for no customer
/// managed key and so has nothing to resolve.
///
/// `region` is the Region the request was signed for, which is where a key lives whose
/// identifier carries no Region of its own; a key named by an ARN is resolved in the Region
/// that ARN names.
///
/// A READ: it asks KMS about the key and judges the answer, and changes nothing anywhere.
pub async fn kms_key_violation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kms_key_arn: Option<&str>,
) -> Option<String> {
    let key_arn = kms_key_arn.filter(|arn| !arn.is_empty())?;
    ::service_common::kms_key::key_is_absent(ctx, region, key_arn)
        .await
        .then(|| kms_key_refusal(key_arn))
}
