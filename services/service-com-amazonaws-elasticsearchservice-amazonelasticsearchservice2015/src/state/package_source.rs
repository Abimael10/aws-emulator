// SPDX-License-Identifier: CC0-1.0

//! Whether the Amazon S3 object a `PackageSource` names can be READ by this
//! service — the pre-condition of every call that imports package content.
//!
//! `PackageSource` is "the customer S3 location for importing the package",
//! given as `S3BucketName` and `S3Key` (the shape's own documentation), and the
//! content it names is the whole of what a package IS. The service copies that
//! object into the package when the call is made, so a call naming an object it
//! cannot read describes a package with nothing behind it: it is refused there
//! and then, rather than minting a record whose content never arrives and
//! deferring the failure to a later association or plugin load.
//!
//! The object is read through this emulator's own Amazon S3, reached the way any
//! emulated service reaches another: the AWS SDK over the in-process transport
//! the operation callback carries
//! ([`smithy_server_runtime::OperationContext::callback`]), which opens no
//! socket, resolves no name and reaches nothing outside this process — so an
//! object a caller staged with `PutObject` is an object this read serves. The
//! read is a HEAD: what the import needs to know is whether the object can be
//! FETCHED, not what is in it, so nothing of a dictionary of any size is
//! buffered.
//!
//! Only what the object store ANSWERED refuses a call: an answer that it holds
//! nothing at that address, or that the read is not permitted, or anything else
//! it said about that object. A failure carrying no answer at all — no object
//! store mounted to ask, a call that could not be dispatched — establishes
//! nothing, so the call is admitted; a refusal has to be grounded in what the
//! owning service said, which is the rule every cross-service pre-condition in
//! this tree follows (`services/common/src/network.rs`).

use super::*;

/// How many characters of an object key this service keeps: enough for Amazon
/// S3's own longest key (1024 UTF-8 bytes), so no key a caller can address an
/// object with is clipped before the store is asked about it.
const MAX_KEY_CHARS: usize = 1024;

/// What this service answers for a `PackageSource` whose object it cannot read.
///
/// The sentence is fixed text in its entirety — it names neither the bucket, nor
/// the key, nor the Region, so nothing of the request is interpolated into it —
/// and it states both halves of what the caller has to check: the access to the
/// object, and that the bucket is in the Region the call was signed for.
pub const PACKAGE_FILE_NOT_ACCESSIBLE: &str = "Package file not accessible. Please validate that \
                                               you have access to the S3 object and S3 bucket is \
                                               present in the same region.";

/// Whether the object `source` names is one this service could NOT read in
/// `region` — `true` only where the object store itself answered so.
///
/// The address is checked for being an address at all BEFORE anything is asked
/// of the store: a blank bucket or key, a key naming a prefix rather than an
/// object, a key longer than Amazon S3 holds one under, and a bucket name Amazon
/// S3 holds no bucket under ([`::service_common::s3_bucket_name::is_well_formed`])
/// name no object anywhere, so no read of them could ever serve one.
///
/// Call it with NO store of this service held — it awaits.
pub async fn is_unreadable(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    source: &PackageSource,
) -> bool {
    let bucket = source.s3_bucket_name.as_deref().unwrap_or_default().trim();
    let key = source.s3_key.as_deref().unwrap_or_default().trim();
    if bucket.is_empty()
        || !::service_common::s3_bucket_name::is_well_formed(bucket)
        || key.is_empty()
        || key.ends_with('/')
        || key.chars().count() > MAX_KEY_CHARS
    {
        ::tracing::debug!(
            bucket = ?bucket,
            key = ?key,
            "a package source names no Amazon S3 object"
        );
        return true;
    }
    // Built by the workspace's one constructor of this hop, so the package plane
    // reads an object exactly as every other emulated service does.
    let client = ::service_common::s3_client::s3_client(ctx.callback(), region);
    let Err(error) = client.head_object().bucket(bucket).key(key).send().await else {
        return false;
    };
    // What the answer ESTABLISHED, read through the tree's one reading of an
    // object store's answer, so this service and every other agree about what an
    // answer meant.
    ::service_common::s3_object_read::answered_refusal(&error)
        != ::service_common::s3_object_read::ObjectRead::Unanswered
}
