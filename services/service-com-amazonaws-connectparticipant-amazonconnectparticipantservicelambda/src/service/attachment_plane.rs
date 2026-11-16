// SPDX-License-Identifier: CC0-1.0

//! The attachment plane: the target `StartAttachmentUpload` publishes as
//! `UploadMetadata$Url`, and the one `GetAttachment` reports as `Url`.
//!
//! # Not a modelled operation
//!
//! Both members are documented as pre-signed URLs a caller follows directly with an HTTP
//! client — one to upload the file, one to download it. Such a request carries no
//! `X-Amz-Target`, names no modelled operation, and a pre-signed URL is followed with no
//! credential of its own. Nothing in the generated routing table can name it, so it is
//! reached through the two seams an emulated data plane uses:
//!
//! * [`crate::service::EmulatedService::can_handle`] already claims it, because the
//!   published path is inside this service's own modelled path space (see
//!   [`crate::state::ATTACHMENT_PATH_PREFIX`]);
//! * [`smithy_server_runtime::Service::override_routing_error`] serves it, and the router
//!   reaches that seam BEFORE authentication — which is what lets the published URL be
//!   followed unsigned, as a pre-signed URL is.
//!
//! # What is served
//!
//! `PUT /participant/attachments/<contact>/<attachment>` stores the request body as the
//! file of the attachment the path names, and `GET` (or `HEAD`) hands those very bytes
//! back, under the media type the upload declared. So the file a participant downloads is
//! the file that was uploaded, and an attachment nothing uploaded to names no object at
//! all: no bytes are invented for it.
//!
//! Each half is served only inside the window of the URL that was published for it — the
//! upload's own, and the longest one `GetAttachment` announced for the download — because
//! that is what the signature of a pre-signed URL decides for the service these targets
//! stand in for.
//!
//! The upload is held to what `StartAttachmentUpload` declared for the attachment — its
//! `AttachmentSizeInBytes`, which is the size `GetAttachment` reports — and to the window
//! the published URL was signed for. Both judgements are made on the DECLARED length first,
//! so an upload stating the wrong size is refused from its headers, and then on the length
//! that actually arrived, which is the authority.
//!
//! # The error envelope
//!
//! This plane is plain HTTPS rather than a modelled operation, so a refusal is answered in
//! its own small JSON envelope carrying the code and a reason — never in this service's
//! restJson1 shapes, which belong to the modelled operations. The codes and sentences are
//! the ones Amazon S3 answers the same conditions with, because that is the service whose
//! pre-signed URL the published target stands in for.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use aws_smithy_types::body::SdkBody;

/// The media type a refusal of this plane is stated in.
const ERROR_CONTENT_TYPE: &str = "application/json";

/// What this plane says to a method neither published URL describes.
const METHOD_NOT_ALLOWED_MESSAGE: &str =
    "The specified method is not allowed against this resource.";

/// What it says when the path names no attachment object that can be served: an attachment
/// this store does not hold, one of another chat, or one nothing has uploaded a file to.
/// One sentence carrying nothing from the request.
const NO_SUCH_KEY_MESSAGE: &str = "The specified key does not exist.";

/// What it says to an upload offered after the window the published URL was signed for has
/// closed, and to a download offered after the window `GetAttachment` announced for it — the
/// answer a pre-signed S3 URL gives once its signature is no longer valid.
const REQUEST_EXPIRED_MESSAGE: &str = "Request has expired";

/// What it says to a download of an attachment no pre-signed download URL was ever reported
/// for: such a request carries no window at all, which is what S3 answers a request it
/// cannot authorise.
const ACCESS_DENIED_MESSAGE: &str = "Access Denied";

/// What it says to an upload of an attachment whose upload is already confirmed: the
/// target described one upload, and `CompleteAttachmentUpload` has closed it.
const UPLOAD_COMPLETE_MESSAGE: &str = "The upload of this attachment is already complete.";

/// What it says to an upload that declares no length this plane can read: the answer S3
/// gives a `PUT` carrying no `Content-Length`. Requiring the declaration is also what lets
/// the declared size be judged from the headers.
const MISSING_CONTENT_LENGTH_MESSAGE: &str = "You must provide the Content-Length HTTP header";

/// What it says to an upload longer than the attachment declared.
const ENTITY_TOO_LARGE_MESSAGE: &str = "Your proposed upload exceeds the maximum allowed size";

/// What it says to an upload shorter than the attachment declared.
const INCOMPLETE_BODY_MESSAGE: &str =
    "You did not provide the number of bytes specified by the Content-Length HTTP header";

/// What it says when the body could not be read at all. The router buffers every request
/// before dispatch, so a body it cannot hand back in memory is a failure of the server,
/// never a caller's malformed upload.
const UNREADABLE_BODY_MESSAGE: &str = "The server could not read the uploaded file";

/// What it says when this service's store is not reachable.
const STORE_UNAVAILABLE_MESSAGE: &str = "The connectparticipant store is unavailable";

/// Serve one request to a published attachment URL: `PUT` uploads the file, `GET` and
/// `HEAD` read it back.
///
/// WHICH attachment is named was already read off the path by
/// [`crate::state::parse_attachment_path`], so this function decides only the method, the
/// upload's size and window, and what the store holds.
pub fn serve<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    contact_id: &str,
    attachment_id: &str,
) -> RtResponse {
    match req.method() {
        "PUT" => upload(engine, req, contact_id, attachment_id),
        "GET" | "HEAD" => download(engine, contact_id, attachment_id, req.method() == "GET"),
        _ => refusal(405, "MethodNotAllowed", METHOD_NOT_ALLOWED_MESSAGE),
    }
}

/// Receive the file one attachment's published upload target was minted for.
fn upload<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    contact_id: &str,
    attachment_id: &str,
) -> RtResponse {
    // The DECLARED length is judged first, so an upload stating a size the attachment was
    // not minted for is refused from its headers, and one stating no size this plane can
    // read is refused rather than taken on trust.
    let Some(declared) = declared_length(req) else {
        return refusal(411, "MissingContentLength", MISSING_CONTENT_LENGTH_MESSAGE);
    };
    let Some(body) = req.body().bytes() else {
        return refusal(500, "InternalError", UNREADABLE_BODY_MESSAGE);
    };
    let Some(store) = crate::state::service_state(engine) else {
        return refusal(500, "InternalError", STORE_UNAVAILABLE_MESSAGE);
    };
    let mut state = store.lock();
    let Some(attachment) = state.attachment_of_contact_mut(attachment_id, contact_id) else {
        return refusal(404, "NoSuchKey", NO_SUCH_KEY_MESSAGE);
    };
    if attachment.is_approved() {
        return refusal(403, "AccessDenied", UPLOAD_COMPLETE_MESSAGE);
    }
    if std::time::SystemTime::now() >= attachment.upload_url_expiry {
        return refusal(403, "AccessDenied", REQUEST_EXPIRED_MESSAGE);
    }
    let expected = u64::try_from(attachment.size_in_bytes).unwrap_or(0);
    // The declared length, then the length that actually ARRIVED, which is the authority: a
    // declaration is a claim, and this is the size of the file the store would hold against
    // the size `GetAttachment` reports for it.
    let arrived = u64::try_from(body.len()).unwrap_or(u64::MAX);
    for length in [declared, arrived] {
        if length > expected {
            return refusal(400, "EntityTooLarge", ENTITY_TOO_LARGE_MESSAGE);
        }
        if length < expected {
            return refusal(400, "IncompleteBody", INCOMPLETE_BODY_MESSAGE);
        }
    }
    attachment.uploaded = Some(body.to_vec());
    stored()
}

/// Hand back the file one attachment holds. `with_body` is false for a `HEAD`, which
/// reports the object's metadata without its bytes.
fn download<Eng: Send + Sync + 'static>(
    engine: &Eng,
    contact_id: &str,
    attachment_id: &str,
    with_body: bool,
) -> RtResponse {
    let Some(store) = crate::state::service_state(engine) else {
        return refusal(500, "InternalError", STORE_UNAVAILABLE_MESSAGE);
    };
    let state = store.lock();
    // An attachment this store no longer holds, one of another chat, and one nothing has
    // uploaded a file to all name no object: the last is the honest answer for an upload
    // that was started, or even confirmed, with no bytes ever sent to its target.
    let Some(attachment) = state.attachment_of_contact(attachment_id, contact_id) else {
        return refusal(404, "NoSuchKey", NO_SUCH_KEY_MESSAGE);
    };
    // A download is served inside the window of a URL that was REPORTED for this attachment,
    // and nowhere else: that is the judgement the signature of a pre-signed URL carries, and
    // `GetAttachment` is the only call that announces one.
    if !attachment.is_downloadable_at(std::time::SystemTime::now()) {
        let message = if attachment.download_url_expiry.is_some() {
            REQUEST_EXPIRED_MESSAGE
        } else {
            ACCESS_DENIED_MESSAGE
        };
        return refusal(403, "AccessDenied", message);
    }
    let Some(bytes) = attachment.uploaded.as_deref() else {
        return refusal(404, "NoSuchKey", NO_SUCH_KEY_MESSAGE);
    };
    let body = if with_body {
        SdkBody::from(bytes.to_vec())
    } else {
        SdkBody::empty()
    };
    let mut response = RtResponse::new(status(200), body);
    // The media type the upload declared for this file, which is what the attachment is
    // recorded with. Inserted through the FALLIBLE `try_insert`, never the panicking
    // `insert`: the value came from a caller, and a stored value that is not valid HTTP must
    // leave this plane serving the file under the generic media type rather than panicking
    // inside a request. `StartAttachmentUpload` refuses such a `ContentType` up front, so
    // this is the second of two guards rather than the only one.
    if response
        .headers_mut()
        .try_insert("content-type", attachment.content_type.clone())
        .is_err()
    {
        let _ = response
            .headers_mut()
            .try_insert("content-type", FALLBACK_CONTENT_TYPE);
    }
    let _ = response
        .headers_mut()
        .try_insert("content-length", bytes.len().to_string());
    response
}

/// The media type a stored file is served under when the one recorded for it is not a valid
/// HTTP header value: the generic "these are bytes" type, which is what a store with no
/// usable type for an object reports.
const FALLBACK_CONTENT_TYPE: &str = "application/octet-stream";

/// The length the request declares for the file, when it declares one this plane can read
/// as a byte count. A header is caller-controlled text, so a value that is not a plain byte
/// count is not a declaration at all.
fn declared_length(req: &RtRequest) -> Option<u64> {
    req.headers()
        .get(::http::header::CONTENT_LENGTH.as_str())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// The answer to an upload this plane stored: the bytes are held, and there is nothing to
/// report back about them.
fn stored() -> RtResponse {
    let mut response = RtResponse::new(status(200), SdkBody::empty());
    let _ = response.headers_mut().try_insert("content-length", "0");
    response
}

/// One attachment-plane refusal: the status, the code in `x-amzn-errortype`, and a JSON
/// body carrying the reason.
fn refusal(code: u16, error_code: &str, message: &str) -> RtResponse {
    let body = ::serde_json::json!({ "code": error_code, "message": message }).to_string();
    let mut response = RtResponse::new(status(code), SdkBody::from(body));
    let _ = response
        .headers_mut()
        .try_insert("content-type", ERROR_CONTENT_TYPE);
    let _ = response
        .headers_mut()
        .try_insert("x-amzn-errortype", error_code.to_string());
    response
}

/// A status this plane answers with. Every call site passes a constant, so the conversion
/// cannot fail.
fn status(code: u16) -> StatusCode {
    StatusCode::try_from(code).expect("the attachment plane's statuses are valid constants")
}
