// SPDX-License-Identifier: CC0-1.0

//! The customer managed KMS key a data store is created under, resolved against the
//! service that owns keys.
//!
//! `CreateDatastore$kmsKeyArn` is "the Amazon Resource Name (ARN) assigned to the Key
//! Management Service (KMS) key for accessing encrypted data"
//! (`smithy_query(query="operation",operation="CreateDatastore")`): the data store's
//! content is encrypted under that key, so a data store is only a data store if the key
//! is there. The member's `@length` and `@pattern` can say only that a value is SHAPED
//! like a key ARN — the pattern admits both `key/<id>` and `alias/<name>` — never that
//! AWS KMS holds a key it resolves to, which is a fact only that service has.
//!
//! So the key is RESOLVED before anything is minted. A data store recorded under a key
//! that does not exist could never be read back under it, and the refusal belongs on the
//! call a caller can still correct rather than on every later read of its content.
//!
//! The resolution is the tree's one reading of a caller-supplied key identifier against
//! the service that owns keys ([`::service_common::kms_key::key_is_absent`]) — the AWS SDK
//! over this emulator's own in-process transport, the way `StartDICOMImportJob` resolves
//! its `dataAccessRoleArn` against IAM ([`super::unassumable_role_message`]). Only what
//! AWS KMS ANSWERED refuses the create: a key of another account, an answer about KMS
//! rather than about the key, an answer this emulator could not read, and an emulator with
//! no KMS mounted to ask establish nothing, and the data store is created.

/// What this service refuses a `kmsKeyArn` naming a key AWS KMS holds none of with.
///
/// AWS's own sentence for this condition, and the whole of it is FIXED text: it carries
/// neither the key the request named nor the data store it was for, stated by RULE like
/// every other refusal of this service ([`super::DATASTORE_NOT_ACTIVE_MESSAGE`]).
pub const UNLOCATABLE_KMS_KEY_MESSAGE: &str =
    "The provided KMS key for datastore cannot be located.";

/// The refusal a request earns for naming through `kms_key_arn` a key AWS KMS holds none
/// of, and `None` when it named a key KMS answered for — or named no key at all, which is
/// the optional member being absent and asks for the service owned key.
///
/// `region` is the Region the call was signed for, which is where an identifier carrying no
/// Region of its own is looked for; this member is modelled as an ARN, so the key is looked
/// for at the endpoint THAT ARN names.
///
/// Call it with NO store of this service held: it awaits.
pub async fn unlocatable_kms_key(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kms_key_arn: Option<&str>,
) -> Option<String> {
    let requested = kms_key_arn?;
    ::service_common::kms_key::key_is_absent(ctx, region, requested)
        .await
        .then(|| UNLOCATABLE_KMS_KEY_MESSAGE.to_string())
}
