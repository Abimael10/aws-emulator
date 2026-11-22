// SPDX-License-Identifier: CC0-1.0

//! The ONE rewrite of a rendered SUCCESS body, for every service whose wire carries a
//! member its checked-in Smithy model does not declare.
//!
//! # Why a service needs this
//!
//! The model a service crate is generated from is a SNAPSHOT of the API, and a live wire
//! has often moved past it: an answer carries a member the model declares nowhere, or
//! carries one modelled member under a second name as well. Such a member cannot be a
//! field of a generated shape, so it cannot be written from a handler or through the
//! schema serializer — the only layer that can write it is the one holding the body the
//! framework's own serializer just rendered.
//!
//! WHICH members a service's wire carries, and what they hold, is that service's own
//! fact, read off its recordings and its model: this module owns the PLUMBING of that
//! rewrite and none of the decisions. It is the success-body sibling of
//! [`crate::error_detail_member::with_error_body_members`], which owns the same rewrite
//! over an ERROR body.
//!
//! # What it owns
//!
//! * [`RawObject`] and [`raw_value`] — reading and writing a JSON body value by value
//!   through `serde_json`'s RAW values, so every value the renderer wrote is written back
//!   byte for byte. That matters most for a NUMBER: a timestamp's or a score's own digits
//!   are the renderer's, and re-rendering them through a float would change the bytes a
//!   client reads. What does change is the order of the members of each object that is
//!   rewritten, which carries no meaning on a JSON protocol.
//! * [`with_rewritten_success_body`] — the response-level half: a failure and a streamed
//!   body are left alone (a failure's own wording belongs to the service's refusal path),
//!   and a rewritten body has the `content-length` its renderer advertised corrected to
//!   describe it.
//!
//! # Services still carrying their own copy
//!
//! This plumbing predates the module and is inlined in the service crates that needed it
//! before there was anywhere shared to put it —
//! `grep -rln 'fn served_success_body' services/*/src` lists them (today the Amazon
//! Systems Manager crate, `src/service/recorded_response_members.rs`). Those copies are
//! to be MIGRATED onto this module and are left in place only because this change does
//! not own those services' issues, each being covered by its own suite. Nothing new
//! should inline the plumbing again — call this module.

use ::std::collections::BTreeMap;

use ::aws_smithy_runtime_api::http::Response as RtResponse;
use ::aws_smithy_types::body::SdkBody;
use ::serde_json::value::RawValue;

/// One JSON object, each of its members still the bytes it was read as.
///
/// Read with `serde_json::from_slice`/`from_str` and written with `serde_json::to_vec`:
/// a member neither the reader nor the writer looks inside is copied, not re-rendered.
pub type RawObject<'b> = BTreeMap<String, &'b RawValue>;

/// `bytes` as one raw JSON value, or `None` when they are not one.
///
/// This is how a member REWRITTEN as its own bytes is put back into the object that
/// carries it: the enclosing object holds borrowed raw values, so the rewritten bytes
/// have to outlive it.
#[must_use]
pub fn raw_value(bytes: &[u8]) -> Option<&RawValue> {
    let text = ::std::str::from_utf8(bytes).ok()?;
    ::serde_json::from_str(text).ok()
}

/// `resp` with its body replaced by `rewrite`'s, and the `content-length` its renderer
/// advertised corrected to describe it.
///
/// A response this rewrite does not own is returned untouched — status, headers and every
/// byte of the body exactly as the renderer wrote them: a failure (whose body is the
/// service's own refusal, reworded on its own path), a streamed body, and a body
/// `rewrite` answers `None` for.
#[must_use]
pub fn with_rewritten_success_body(
    mut resp: RtResponse,
    rewrite: impl FnOnce(&[u8]) -> Option<Vec<u8>>,
) -> RtResponse {
    if !resp.status().is_success() {
        return resp;
    }
    let rewritten = {
        // A success body of a JSON protocol is built in memory by its renderer, so the
        // bytes are readable here; a streamed body is not one and is left alone.
        let Some(bytes) = resp.body().bytes() else {
            return resp;
        };
        match rewrite(bytes) {
            Some(rewritten) => rewritten,
            None => return resp,
        }
    };
    // Only a renderer that advertised a length needs it corrected; the framework's
    // renderers set `content-type` alone and leave framing to the transport.
    if resp.headers().get("content-length").is_some() {
        let length = rewritten.len().to_string();
        // A decimal length is always a valid header value, so this cannot fail today; if
        // it ever did, the header is REMOVED rather than left describing the body before
        // the rewrite, so the two can never disagree.
        if resp
            .headers_mut()
            .try_insert("content-length", length)
            .is_err()
        {
            resp.headers_mut().remove("content-length");
        }
    }
    *resp.body_mut() = SdkBody::from(rewritten);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A response of `status` carrying `body`, with the length its renderer advertised.
    fn response(status: u16, body: &str) -> RtResponse {
        let mut resp = RtResponse::new(
            ::aws_smithy_runtime_api::http::StatusCode::try_from(status).expect("a status"),
            SdkBody::from(body),
        );
        resp.headers_mut()
            .insert("content-length", body.len().to_string());
        resp
    }

    /// One member added to a JSON object, the way a caller of this module adds the member
    /// its wire carries: every other member copied as the bytes it was read as.
    fn with_member(body: &[u8]) -> Option<Vec<u8>> {
        let mut object: RawObject = ::serde_json::from_slice(body).ok()?;
        let added = raw_value(b"true")?;
        object.insert("added".to_string(), added);
        ::serde_json::to_vec(&object).ok()
    }

    /// RULE: a rewritten success body is the rewrite's bytes, described by the length the
    /// renderer advertised, with every value it already carried written back unchanged.
    /// The rows differ along the kind of value sitting beside the added member.
    #[test]
    fn a_rewritten_success_body_is_described_by_its_advertised_length() {
        for (case, body, kept) in [
            ("a timestamp of many digits", r#"{"at":1794894495.9506013}"#, "1794894495.9506013"),
            ("a trailing zero", r#"{"at":1794894495.4231250}"#, "1794894495.4231250"),
            ("escaped text", r#"{"text":"a \"quoted\" answer"}"#, r#"a \"quoted\" answer"#),
        ] {
            let answered = with_rewritten_success_body(response(200, body), with_member);
            let bytes = answered.body().bytes().expect("bytes").to_vec();
            let rendered = String::from_utf8(bytes.clone()).expect("the body is text");
            assert!(
                rendered.contains(kept) && rendered.contains(r#""added":true"#),
                "case {case:?}: expected {kept:?} kept beside the added member, got {rendered}"
            );
            let advertised = answered.headers().get("content-length");
            assert_eq!(
                advertised,
                Some(bytes.len().to_string().as_str()),
                "case {case:?}: the advertised length must describe the rewritten body, got \
                 {advertised:?}"
            );
        }
    }

    /// A response this rewrite does not own is returned byte for byte: a failure, whose
    /// body belongs to the service's refusal path, and a body the rewrite declines.
    #[test]
    fn a_response_this_rewrite_does_not_own_is_left_alone() {
        // (case, the status, whether the rewrite would answer a body of its own)
        for (case, status, rewrites) in [
            ("a failure, which the rewrite must never be asked about", 400, true),
            ("a body the rewrite declines", 200, false),
        ] {
            let body = r#"{"at":1794894495.9506013}"#;
            let answered = with_rewritten_success_body(response(status, body), |_| {
                rewrites.then(|| b"{\"rewritten\":true}".to_vec())
            });
            assert_eq!(
                answered.body().bytes().expect("bytes"),
                body.as_bytes(),
                "case {case:?}: the body must be byte-identical"
            );
            assert_eq!(
                answered.headers().get("content-length"),
                Some(body.len().to_string().as_str()),
                "case {case:?}: the advertised length must be untouched"
            );
        }
    }
}
