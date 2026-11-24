// SPDX-License-Identifier: CC0-1.0

//! Reading the AMAZON S3 OBJECT a declaration addresses, as the text of the property whose
//! document it is.
//!
//! ## The rule this module implements
//!
//! Several resource types publish a property PAIR: a document, and a reference to an Amazon S3
//! object holding it — an api's `Body` beside its `BodyS3Location`, a resolver's `Code` beside
//! its `CodeS3Location`. The reference says WHERE the text is, not what it means, so a
//! declaration that states it is created from the bytes that object holds, and every provider
//! that reads one reads it the same way: through the in-process callback into this same
//! emulator ([`crate::state::provisioner::s3_client`]), bounded, and as the TEXT of a property.
//!
//! One reader for every PROVIDER that resolves such a reference, because the read is the same
//! step wherever it appears: the document a client staged with its own `PutObject` is what the
//! resource is created from, nothing here opens a socket or leaves the host, and nothing is
//! read from the object but the bytes it holds. (A read made AS THE CALLER rather than by a
//! provider — the template a `TemplateURL` addresses — is that operation's own and lives in
//! [`crate::state::template_url`], which authorizes it against the caller's access to the
//! object.)
//!
//! ## What a failed read answers
//!
//! What a provider REPORTS for a read it could not make is that provider's own ([`ReadFailure`]
//! carries the parts each of them words it from): the api providers raise the exception their
//! handler words for a reference it could not materialise
//! ([`super::apigateway_definitions`]), and AWS AppSync's nest the read inside the document
//! they were materialising ([`super::appsync_s3_documents`]). So this module answers WHAT the
//! read reported and leaves the wording to the provider that asked for it.

use crate::state::template_url;

/// Why a read of a staged object served no document.
///
/// One variant per thing a provider words differently: a refusal AMAZON S3 answered, which
/// carries that service's own code, message and status; an object past the bound the read
/// allows, which is not a refusal of any call; and a read that was served and carried no
/// document at all.
#[derive(Debug)]
pub(super) enum ReadFailure {
    /// Amazon S3 refused the read: that service's own error code, the message it worded, and
    /// the HTTP status it answered — `None` for an answer that carried no response at all.
    Refused {
        code: String,
        message: String,
        status: Option<u16>,
    },
    /// The object is longer than the read allows, which is reported before any of it is held.
    TooLarge,
    /// The read was served and no document came of it: a body that stopped, or bytes that are
    /// no text.
    NoDocument,
}

/// The TEXT the object `addressed` holds, read from this emulator's own Amazon S3, or what the
/// read reported instead.
///
/// `max_bytes` is the caller's bound on the document: the length the object REPORTS is checked
/// before a single byte of it is held, and the bytes themselves are checked again, so nothing
/// past the bound is ever read into memory.
pub(super) async fn text(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    addressed: &template_url::Object,
    max_bytes: usize,
) -> Result<String, ReadFailure> {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    let answered = crate::state::provisioner::s3_client(ctx, region)
        .get_object()
        .bucket(&addressed.bucket)
        .key(&addressed.key)
        .set_version_id(addressed.version.clone())
        .send()
        .await
        .map_err(|error| {
            let code = error.code().unwrap_or(UNIDENTIFIED_FAILURE).to_string();
            let message = error.message().unwrap_or(&code).to_string();
            ReadFailure::Refused {
                code,
                message,
                status: error.raw_response().map(|answer| answer.status().as_u16()),
            }
        })?;
    // The length the object REPORTS, before a single byte of it is held.
    if answered
        .content_length()
        .is_some_and(|length| length > max_bytes as i64)
    {
        return Err(ReadFailure::TooLarge);
    }
    let body = answered
        .body
        .collect()
        .await
        // The object was handed over and the body then stopped: nothing identified why.
        .map_err(|_| ReadFailure::NoDocument)?
        .into_bytes();
    if body.len() > max_bytes {
        return Err(ReadFailure::TooLarge);
    }
    // What the reference addressed is the TEXT of a property; an object that is not valid
    // UTF-8 carries none, so the read served no document.
    String::from_utf8(body.into()).map_err(|_| ReadFailure::NoDocument)
}

/// What stands in as the error code where Amazon S3 identified nothing: the read was not served
/// — or was served and carried no text — and nothing on the other end said why.
///
/// Read by the providers above too, which report it as what the read answered for a failure no
/// code of that service's came back for.
pub(super) const UNIDENTIFIED_FAILURE: &str = "InternalFailure";

/// The refusal a provider reports for a read it could not make, in the shape the refused call
/// of any other service reaches a failed resource in: that service's own message with its SDK's
/// diagnostic of the call ([`super::sdk_message`]), for a refusal Amazon S3 answered.
///
/// `None` for a failure Amazon S3 answered nothing for — the two that are not a refusal of any
/// call — which the asking provider words itself.
pub(super) fn s3_exception(failure: &ReadFailure) -> Option<String> {
    let ReadFailure::Refused {
        code,
        message,
        status,
    } = failure
    else {
        return None;
    };
    // An answer that carried no response at all is reported as it was worded, with no
    // diagnostic invented for it.
    Some(match status {
        Some(status) => super::sdk_message(S3_SIGNING_NAME, *status, code, message),
        None => message.clone(),
    })
}

/// Amazon S3's own signing name, which routes the read in process and is what the diagnostic
/// looks the SDK's name for that service up under ([`super::sdk_message`]).
pub(super) const S3_SIGNING_NAME: &str = "s3";
