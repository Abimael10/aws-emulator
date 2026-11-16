// SPDX-License-Identifier: CC0-1.0

//! Whether the software bill of materials an association names is one the service can
//! READ at all.
//!
//! `AssociateSbomWithPackageVersion` attaches "a specific software bill of materials" to a
//! package version and starts the validation of it "against the Software Package Data
//! Exchange (SPDX) and CycloneDX industry standard formats"
//! (`smithy_query(query="operation",operation="AssociateSbomWithPackageVersion")`). That
//! document is not carried in the request: a `Sbom` is one `s3Location`, so the bill of
//! materials IS the S3 object it names, and an object the service cannot read is one no
//! validation could ever run over. Such an association is refused — the modelled
//! `ValidationException` (HTTP 400, "The request is not valid") — so nothing is recorded
//! and no reader of the version reports a bill of materials it does not have.
//!
//! The read is made the way any emulated service reaches another: the AWS SDK over this
//! emulator's own in-process transport ([`::service_common::s3_client`], which hands the
//! SDK `callback.http_client()`), so NOTHING LEAVES THIS PROCESS — no socket is opened, no
//! name is resolved, and the object a caller staged with `PutObject` is the object this
//! read serves. It is a HEAD — what has to be established is that the object is THERE and
//! may be fetched, and nothing here reads the document's bytes.

use crate::ops::types::Sbom;

/// The sentence a refused association states.
///
/// It is entirely fixed text — it names neither the bucket, the key nor the version — and
/// it covers both halves of what the read establishes: the object has to exist, and the
/// service has to be allowed to fetch it.
pub const INACCESSIBLE_SBOM_MESSAGE: &str = "SBOM not accessible by the service. Please make \
                                             sure the bucket/key/version exists and S3 \
                                             permission is granted.";

/// The sentence an association whose S3 location names no object VERSION is refused with.
///
/// A bill of materials is pinned to the version of the object it was read from, so the
/// location has to name one: a location without it names whatever the key holds at the
/// moment of a read, which is no fixed document. The sentence is fixed text naming the
/// member that was null.
pub const SBOM_VERSION_REQUIRED_MESSAGE: &str = "S3 object version for SBOM cannot be null";

/// Whether the S3 location `sbom` names is missing the object VERSION it has to pin.
///
/// Asked BEFORE anything is read from the object store, because it is a fact about the
/// request alone. An `Sbom` carrying no `s3Location` at all names no location whose
/// members this could be about, so nothing is established about it here.
#[must_use]
pub fn sbom_names_no_object_version(sbom: &Sbom) -> bool {
    sbom.s3_location.as_ref().is_some_and(|location| {
        location
            .version
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
    })
}

/// Whether this emulator's own Amazon S3 ANSWERED that the object `sbom` names is one the
/// service cannot read: the store holds nothing at that address, it answered that the read
/// is not permitted, or it answered something else about that object — a `version` that
/// names no version of anything, say — through which no object can be read.
///
/// Those three are the answers AWS's own refusal is about: the bucket, the key and the
/// version have to exist, and the read of them has to be permitted. Only what the store
/// ANSWERED grounds the refusal
/// ([`::service_common::s3_object_read::answered_refusal`]): with no object store mounted
/// to ask, or with a hop that could not be made at all, nothing is established about the
/// object and the caller's request is not what is at fault.
///
/// An `Sbom` that names no object address at all — no `s3Location`, or one carrying no
/// bucket or no key — asks the store nothing, so nothing is established about it either:
/// which members of `S3Location` a caller must supply is the model's question, not this
/// read's.
///
/// NOTHING of the request reaches an ADDRESS, because this read makes no outbound
/// connection at all: the SDK client is handed the emulator's own in-process transport
/// (`ctx.callback()`'s `http_client`, through [`::service_common::s3_client`]), which opens
/// no socket and resolves no name, so the request it carries is served by this same process
/// and nothing else is reachable from it. The endpoint it is built with is addressing for
/// that transport, derived from the Region alone, and that Region is held to the
/// Region-label grammar ([`::service_common::arn::region_label`]) before it gets there, so a
/// value carrying a host, a port or a path separator addresses nothing and the association
/// is refused for nothing. The bucket, the key and the object version travel as the PATH of
/// that in-process request — the hop is path-style — so no value a caller supplies is a
/// destination, and no answer about anything outside this process can be obtained through
/// it.
pub async fn sbom_is_unreadable(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    sbom: &Sbom,
) -> bool {
    // A Region that is not a Region label is no address at all: nothing is asked, and so
    // nothing about the object is established.
    let Some(region) = ::service_common::arn::region_label(region) else {
        return false;
    };
    let Some(location) = sbom.s3_location.as_ref() else {
        return false;
    };
    let (Some(bucket), Some(key)) = (location.bucket.as_deref(), location.key.as_deref()) else {
        return false;
    };
    // The one client construction this tree uses for the object store, handed the
    // emulator's own in-process transport: the request below is dispatched back into this
    // same process, never onto a socket.
    let client = ::service_common::s3_client::s3_client(ctx.callback(), region);
    let mut head = client.head_object().bucket(bucket).key(key);
    // "The S3 bucket version": a bill of materials pinned to one version IS that version of
    // the object, so that is the object the store is asked about.
    if let Some(version) = location
        .version
        .as_deref()
        .map(str::trim)
        .filter(|version| !version.is_empty())
    {
        head = head.version_id(version);
    }
    let Err(answer) = head.send().await else {
        return false;
    };
    // Every answer the store gave ABOUT the object refuses the association; only a read
    // nothing answered at all establishes nothing.
    ::service_common::s3_object_read::answered_refusal(&answer)
        != ::service_common::s3_object_read::ObjectRead::Unanswered
}
