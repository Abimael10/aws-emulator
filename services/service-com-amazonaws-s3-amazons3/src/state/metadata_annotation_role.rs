// SPDX-License-Identifier: CC0-1.0

//! Whether the IAM role a metadata configuration names for its ANNOTATION table lets S3
//! Metadata read annotations through it.
//!
//! "If you include an `AnnotationTableConfiguration` with an IAM role, the role must have a
//! trust policy that allows the Amazon S3 metadata service to assume it, and a permissions
//! policy that grants the actions needed to read annotations from your bucket"
//! (`CreateBucketMetadataConfiguration`'s own documentation): the annotation table is filled
//! THROUGH that role, so the role is what decides whether the table can be created at all.
//!
//! The verdict is a PROVISIONING OUTCOME, not a refusal: the create and the update that name
//! the table are served, and the table itself is recorded
//! [`super::METADATA_TABLE_STATUS_FAILED`] carrying [`ANNOTATION_ROLE_PERMISSIONS_CODE`],
//! which is what `GetBucketMetadataConfiguration` reports as that table's `TableStatus` and
//! `Error` — exactly where the published provisioning failures of the journal and inventory
//! tables leave those two ([`super::metadata_inventory_table_failure`]).
//!
//! IAM is reached the way any emulated service reaches another, through the in-process
//! callback, by the reading every service resolving a caller-supplied role shares
//! (`service_common::iam_role`) — the same reading this service's replication role takes
//! ([`super::replication::role_access`]). Nothing is withheld that IAM did not answer: a
//! value that names no role of this account, an answer this emulator could not read and an
//! emulator with no IAM mounted all leave the table to be created.

/// The service principal S3 Metadata assumes an annotation role AS, and so the one such a
/// role's trust policy has to admit for the table to be filled through it: the
/// `ErrorDetails$ErrorCode` documentation names `metadata.s3.amazonaws.com` as the S3
/// service principal of a metadata configuration's tables
/// (`smithy_query(query="doc",shape="ErrorDetails",member="ErrorCode")`).
pub const METADATA_SERVICE_PRINCIPAL: &str = "metadata.s3.amazonaws.com";

/// The `ErrorDetails$ErrorCode` an annotation table carries when the role it would be filled
/// through does not grant S3 Metadata what reading annotations needs.
pub const ANNOTATION_ROLE_PERMISSIONS_CODE: &str = "InsufficientAnnotationRolePermissions";

/// See [`ANNOTATION_ROLE_PERMISSIONS_CODE`] — the sentence that code carries.
///
/// Recorded behaviour: the whole string is this error class's FIXED diagnostic, interpolating
/// nothing from the request or from state. It states what the role has to grant on the bucket,
/// which service principal its trust policy has to admit, what a KMS-encrypted bucket needs
/// beside that, and — as every published V2 table-provisioning failure closes
/// (`smithy_query(query="doc",shape="ErrorDetails",member="ErrorCode")`) — that a new table
/// takes deleting the configuration and creating it again.
pub const ANNOTATION_ROLE_PERMISSIONS_MESSAGE: &str =
    "The annotation role does not have sufficient permissions. Ensure the role grants \
     s3:GetObjectAnnotation, s3:GetObjectVersionAnnotation, s3:ListBucket, and \
     s3:ListBucketVersions on the bucket, and that the role's trust policy allows \
     metadata.s3.amazonaws.com to assume it. If the bucket or its objects/annotations are \
     encrypted with a KMS key, ensure the role also has kms:Decrypt permission. Additionally, \
     ensure that the KMS key used to encrypt the bucket still exists, is active and has a \
     resource policy granting access to the S3 service principals. To create a new S3 \
     Metadata table, you must delete the S3 Metadata configuration for this bucket, and then \
     create a new S3 Metadata configuration.";

/// Why an annotation table provisioned NOW under `role` could not be created, or `None` when
/// nothing IAM answered stops it.
///
/// ONE rule for every call that provisions an annotation table — the create that names a
/// whole configuration and the update that enables the table on one — so the two cannot
/// drift apart on it. `role` is the role that would be in effect for the table (the one the
/// call names, or failing that the one the configuration already carries); a call that leaves
/// the table with no role at all never reaches here, because an enabled annotation table
/// always has one.
///
/// Asked BEFORE this service's own guard is taken, so no state lock is held across the hop
/// into IAM.
pub async fn provisioning_failure(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    role: Option<&str>,
) -> Option<super::MetadataTableFailure> {
    let role = role?;
    ::service_common::iam_role::role_is_unassumable(
        ctx,
        &super::request_region(ctx.parts()),
        role,
        METADATA_SERVICE_PRINCIPAL,
    )
    .await
    .then_some(super::MetadataTableFailure {
        code: ANNOTATION_ROLE_PERMISSIONS_CODE,
        message: ANNOTATION_ROLE_PERMISSIONS_MESSAGE,
    })
}
