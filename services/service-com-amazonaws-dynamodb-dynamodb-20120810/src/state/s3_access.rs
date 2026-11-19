// SPDX-License-Identifier: CC0-1.0

//! Whether the CALLING PRINCIPAL may make the Amazon S3 calls a table export or a table
//! import performs for it.
//!
//! An export and an import move a table's data to and from Amazon S3 — "exports table data
//! to an S3 bucket" (`ExportTableToPointInTime`), "imports table data from an S3 bucket"
//! (`ImportTable`) — and the object calls that moves are made WITH THE CALLER's permissions
//! rather than with this service's own: the caller must hold them on the bucket it named,
//! which is why the documented policy for an export grants the object write to the principal
//! that starts it. So each of those calls is decided by exactly the documents Amazon S3
//! would decide it by — the identity-based policies in force on the caller and the bucket's
//! own policy — through this service's one decision ladder
//! ([`super::authorization::performed_for_caller_denial`]).
//!
//! A call the policies in force REFUSE is a call the task does not make, so its data does
//! not move: the task is accepted (both operations are asynchronous) and then ends FAILED,
//! reporting the refusal as its own failure ([`S3_ACCESS_DENIED_FAILURE_CODE`]) and leaving
//! the destination prefix empty — not a completion the task did not achieve.
//!
//! The actions and the ARNs are Amazon S3's own, named from that service's crate rather
//! than spelled again here, so a statement that covers one of its calls covers this one.

use super::*;

/// The `FailureCode` of a task whose Amazon S3 call the policies in force refused, in the
/// form this surface reports an S3-side failure in ([`EXPORT_FAILURE_NO_SUCH_BUCKET`]): the
/// S3 error code for a refused call, prefixed by the service that refused it.
pub const S3_ACCESS_DENIED_FAILURE_CODE: &str = "S3AccessDenied";

/// The IAM action one object of an export's snapshot is written under, which is Amazon S3's
/// own action for an object write.
pub const EXPORT_OBJECT_WRITE_ACTION: &str =
    ::service_com_amazonaws_s3_amazons3::service::request_authorization::PUT_OBJECT_ACTION;

/// The IAM action an import's SELECTION of the source objects is made under: it lists the
/// bucket's objects under the key prefix the source names, which is a listing of that
/// bucket.
pub const IMPORT_SOURCE_LISTING_ACTION: &str =
    ::service_com_amazonaws_s3_amazons3::state::LIST_BUCKET_ACTION;

/// The IAM action one source object of an import is read under.
pub const IMPORT_OBJECT_READ_ACTION: &str =
    ::service_com_amazonaws_s3_amazons3::state::GET_OBJECT_ACTION;

/// The policy `bucket` carries, as Amazon S3's own store holds it on this engine — `None`
/// for a bucket carrying none and for one that store holds nothing under.
///
/// The document is CLONED out from under the object store's guard, so the decision below is
/// taken with no lock of either service held, and it is read at the instant of the call: a
/// policy just put decides immediately and one just deleted stops deciding immediately.
pub fn bucket_policy<Eng: Send + Sync + 'static>(engine: &Eng, bucket: &str) -> Option<String> {
    let state = ::service_com_amazonaws_s3_amazons3::state::service_state(engine)?;
    let store = state.lock();
    store.bucket(bucket)?.policy.clone()
}

/// Why the policies in force refuse the caller the WRITE of `objects` into `bucket`, or
/// `None` when they permit it.
///
/// Every object the export would write is decided, before any of them is written: an export
/// writes its snapshot as one task, so a write the caller may not make fails the whole task
/// and leaves the destination prefix as it was, rather than landing the part that was
/// permitted.
pub fn export_write_denial<Eng: Send + Sync + 'static>(
    engine: &Eng,
    caller: &authorization::Caller,
    bucket: &str,
    objects: &[ExportedObject],
) -> Option<ExportFailure> {
    let policy = bucket_policy(engine, bucket);
    objects
        .iter()
        .find_map(|object| {
            authorization::performed_for_caller_denial(
                caller,
                policy.as_deref(),
                EXPORT_OBJECT_WRITE_ACTION,
                &::service_com_amazonaws_s3_amazons3::state::object_arn(bucket, &object.key),
            )
        })
        .map(|message| ExportFailure {
            code: S3_ACCESS_DENIED_FAILURE_CODE.to_string(),
            message,
        })
}

/// Why the policies in force refuse the caller the SELECTION of the source objects in
/// `bucket`, or `None` when they permit it — the listing an import makes to find the objects
/// under its key prefix, decided on the bucket's own ARN.
pub fn import_listing_denial<Eng: Send + Sync + 'static>(
    engine: &Eng,
    caller: &authorization::Caller,
    bucket: &str,
) -> Option<ImportFailure> {
    let policy = bucket_policy(engine, bucket);
    authorization::performed_for_caller_denial(
        caller,
        policy.as_deref(),
        IMPORT_SOURCE_LISTING_ACTION,
        &::service_com_amazonaws_s3_amazons3::state::bucket_arn(bucket),
    )
    .map(|message| ImportFailure {
        code: S3_ACCESS_DENIED_FAILURE_CODE.to_string(),
        message,
    })
}

/// Why the policies in force refuse the caller the READ of the source objects `objects` in
/// `bucket`, or `None` when they permit it.
///
/// Every object the import would read is decided before any of its data is used, for the
/// reason [`export_write_denial`] decides every object it would write: an import reads its
/// source as one task, so a read the caller may not make fails the task rather than
/// importing the part that was permitted.
pub fn import_read_denial<Eng: Send + Sync + 'static>(
    engine: &Eng,
    caller: &authorization::Caller,
    bucket: &str,
    objects: &[ImportObject],
) -> Option<ImportFailure> {
    let policy = bucket_policy(engine, bucket);
    objects
        .iter()
        .find_map(|object| {
            authorization::performed_for_caller_denial(
                caller,
                policy.as_deref(),
                IMPORT_OBJECT_READ_ACTION,
                &::service_com_amazonaws_s3_amazons3::state::object_arn(bucket, &object.key),
            )
        })
        .map(|message| ImportFailure {
            code: S3_ACCESS_DENIED_FAILURE_CODE.to_string(),
            message,
        })
}
