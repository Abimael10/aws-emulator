// SPDX-License-Identifier: CC0-1.0

//! How a read of one bot names the error shape of a bot it cannot report: it does not.
//!
//! # What the wire shows
//!
//! A `GetBot` whose `botId` names no bot of the `networkId` it was asked under is refused
//! with the status and a `message`, and with NO error-shape discriminator — neither the
//! `x-amzn-errortype` header `restJson1` resolves an error shape from, nor a `__type` /
//! `code` / `Code` member in the body:
//!
//! ```json
//! HTTP/1.1 404 Not Found
//! content-type: application/json; charset=utf-8
//!
//! {"message":"Bot not found"}
//! ```
//!
//! A client therefore classifies that one refusal by STATUS: finding no shape in either
//! place, an SDK reports the error code as the bare status string and none of its
//! modelled-exception classes match. The framework's `restJson1` renderer writes the
//! discriminator in both places for every modelled error
//! (`vendor/smithy-schema-server/src/rest_json.rs`'s `serialize_error`), and `vendor/` is
//! read-only, so this correction lives at the one seam this service owns over the answers
//! its operations write.
//!
//! # What it reaches, and what it deliberately does not
//!
//! Only the gone-bot refusal: the [`GONE_STATUS`] answer of the one operation this wrapper
//! fronts. `GetBot`'s handler answers that status for exactly one reason — the bot the
//! request named is not there ([`crate::ops::get_bot`]) — and every other answer of this
//! service, read or write, success or refusal, keeps the shape its renderer named it with,
//! because this API does name the shape elsewhere: a network read of a deleted network is
//! answered as a typed `ForbiddenError`, and a refused member as a typed `ValidationError`.
//! Nothing here decides WHICH refusals a service names: that is a fact of its wire, stated
//! by the one operation this value is put in front of ([`crate::service`]).
//!
//! The status, the sentence, every other member and every other header stay exactly as the
//! framework rendered them, and the whole success path is untouched. The service's response
//! envelope is applied LAST, after the discriminator is gone, because its entity tag is a
//! function of the bytes the client receives.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_types::body::SdkBody;
use serde_json::Value;
use smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The status the gone-bot refusal carries: the one the model declares for
/// `ResourceNotFoundError` (`@httpError(404)`), which is the only reason a bot read answers
/// it.
const GONE_STATUS: u16 = 404;

/// The response header a `restJson1` client resolves a failure's error shape from, named by
/// the tree's one home for it.
const ERROR_TYPE_HEADER: &str = service_common::error_type_locator::ERROR_TYPE_HEADER;

/// The body members a JSON failure can name its error shape in, in the order a client
/// consults them: `__type` is what this emulator's `restJson1` renderer writes, and
/// `code` / `Code` are the other spellings the same parsers accept.
const DISCRIMINATOR_MEMBERS: [&str; 3] = [
    service_common::error_type_locator::SHAPE_DISCRIMINATOR,
    "code",
    "Code",
];

/// The header a renderer announces a body's length in, which has to be corrected when the
/// body is rewritten under it.
const CONTENT_LENGTH: &str = "content-length";

/// `body` with every error-shape discriminator removed, or `None` when there is nothing to
/// remove: the body is not a JSON object, or it names no shape at all.
///
/// Every other member is written back as it was, so only the discriminator goes.
fn without_discriminator(body: &[u8]) -> Option<Vec<u8>> {
    let mut document: Value = serde_json::from_slice(body).ok()?;
    let object = document.as_object_mut()?;
    if !DISCRIMINATOR_MEMBERS
        .iter()
        .any(|member| object.contains_key(*member))
    {
        return None;
    }
    for member in DISCRIMINATOR_MEMBERS {
        object.remove(member);
    }
    serde_json::to_vec(&document).ok()
}

/// `resp` as this API answers a gone bot: no `x-amzn-errortype` header, and a body naming
/// no error shape either.
///
/// Any other response — a success, a refusal of another class — is handed back exactly as
/// its renderer wrote it: status, headers and every body byte.
#[must_use]
pub fn untyped(mut resp: RtResponse) -> RtResponse {
    if resp.status().as_u16() != GONE_STATUS {
        return resp;
    }
    // The header and the body are two independent places a client reads the shape from, so
    // each is dropped on its own: a body that cannot be rewritten still loses the header.
    resp.headers_mut().remove(ERROR_TYPE_HEADER);
    let rewritten = {
        // An error body is built in memory by its renderer, so these bytes are readable
        // here; a streamed body is not an error body and is left alone.
        let Some(bytes) = resp.body().bytes() else {
            return resp;
        };
        match without_discriminator(bytes) {
            Some(rewritten) => rewritten,
            None => return resp,
        }
    };
    // Only a renderer that announced a length needs it corrected; the framework's error
    // renderers set `content-type` alone and leave framing to the transport.
    if resp.headers().get(CONTENT_LENGTH).is_some() {
        let length = rewritten.len().to_string();
        let _ = resp.headers_mut().try_insert(CONTENT_LENGTH, length);
    }
    *resp.body_mut() = SdkBody::from(rewritten);
    resp
}

/// Serves the modelled operation this service's routing table names for the request exactly
/// as the framework would, reports a gone bot untyped, and answers the result in this
/// service's response envelope.
///
/// It owns no operation and no routing of its own: the one it delegates to is the one the
/// generated routing table names for the request, resolved from the `svc` the framework
/// passes into [`ErasedOperation::run`], so the whole walk — deserialize, validate, handle,
/// serialize — and every status, code, message and body byte it answers with stay the
/// framework's. This wrapper owns only whether that one refusal names its shape on the
/// wire.
#[derive(Debug, Clone, Copy, Default)]
pub struct UntypedBotRefusalOperation;

impl UntypedBotRefusalOperation {
    /// The one value a service holds. A service builds it in its constructor, because
    /// `Service::override_operation` hands back a borrow of it.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ErasedOperation for UntypedBotRefusalOperation {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let definition = svc.definition();
            let modeled = definition
                .protocol()
                .routing_key(req)
                .ok()
                .and_then(|key| definition.routing_table().get(key));
            let answered = match modeled {
                Some(operation) => untyped(operation.run(svc, req, ctx).await),
                // Unreachable through a `Service::override_operation` that claims a request
                // only when the routing table holds its key; answered as the framework
                // answers an unroutable request rather than guessed at.
                None => definition.routing_error_response(req),
            };
            // LAST, over the bytes the client receives: the envelope's entity tag is a
            // function of the body, so it cannot be computed over one a later step
            // rewrites.
            crate::service::response_envelope::enveloped(answered)
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation the routing key names" — which is the
        // operation this wrapper delegates to, so its model-declared schemes decide and
        // this wrapper cannot weaken them.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A response at `status` carrying `body`, named by `error_type` when a renderer named
    /// the shape in the header.
    fn response(status: u16, body: &str, error_type: Option<&str>) -> RtResponse {
        let mut resp = RtResponse::new(
            aws_smithy_runtime_api::http::StatusCode::try_from(status).expect("a valid status"),
            SdkBody::from(body),
        );
        if let Some(error_type) = error_type {
            resp.headers_mut().insert(ERROR_TYPE_HEADER, error_type.to_string());
        }
        resp
    }

    /// The body of `resp` as JSON.
    fn decoded(resp: &RtResponse) -> Value {
        serde_json::from_slice(resp.body().bytes().expect("an in-memory body"))
            .expect("the body is JSON")
    }

    /// RULE: the gone-bot refusal names its error shape nowhere. The rows differ along what
    /// the rule generalises over: which spelling the renderer named the shape in, whether
    /// the header named it too, and whether the shape name carries a namespace.
    #[test]
    fn the_gone_bot_refusal_names_its_shape_in_neither_place() {
        // (case, the body as rendered, the header as rendered)
        let rows: &[(&str, &str, Option<&str>)] = &[
            (
                "the discriminator the restJson1 renderer writes",
                r#"{"__type":"ResourceNotFoundError","message":"Bot not found"}"#,
                Some("ResourceNotFoundError"),
            ),
            (
                "a namespaced discriminator",
                r#"{"__type":"com.amazonaws.wickr#ResourceNotFoundError","message":"Bot not found"}"#,
                Some("com.amazonaws.wickr#ResourceNotFoundError"),
            ),
            (
                "the shape in the header alone",
                r#"{"message":"Bot not found"}"#,
                Some("ResourceNotFoundError"),
            ),
            (
                "the lowercase code spelling",
                r#"{"code":"ResourceNotFoundError","message":"Bot not found"}"#,
                None,
            ),
            (
                "the capitalised Code spelling",
                r#"{"Code":"ResourceNotFoundError","message":"Bot not found"}"#,
                None,
            ),
        ];

        for (case, body, error_type) in rows {
            let answered = untyped(response(GONE_STATUS, body, *error_type));
            assert_eq!(
                answered.status().as_u16(),
                GONE_STATUS,
                "case {case:?}: the status stays the renderer's, got {}",
                answered.status().as_u16()
            );
            assert_eq!(
                answered.headers().get(ERROR_TYPE_HEADER),
                None,
                "case {case:?}: the header must name no shape, got {:?}",
                answered.headers().get(ERROR_TYPE_HEADER)
            );
            let document = decoded(&answered);
            assert!(
                DISCRIMINATOR_MEMBERS
                    .iter()
                    .all(|member| document.get(*member).is_none()),
                "case {case:?}: the body must name no shape, got {document}"
            );
            assert_eq!(
                document.get("message").and_then(Value::as_str),
                Some("Bot not found"),
                "case {case:?}: the sentence must survive, got {document}"
            );
        }
    }

    /// RULE: every other answer is handed back exactly as its renderer wrote it — a
    /// success, and a refusal of another class, whose shape a client still resolves.
    #[test]
    fn an_answer_this_rule_does_not_own_is_returned_unchanged() {
        // (case, the status, the body, the header)
        let rows: &[(&str, u16, &str, Option<&str>)] = &[
            (
                "a bot a read reports",
                200,
                r#"{"botId":"1","username":"a-bot"}"#,
                None,
            ),
            (
                "a refused member",
                422,
                r#"{"__type":"ValidationError","reasons":[{"field":"username"}]}"#,
                Some("ValidationError"),
            ),
            (
                "a network read of a deleted network",
                403,
                r#"{"__type":"ForbiddenError","message":"Network not found"}"#,
                Some("ForbiddenError"),
            ),
            (
                "an operation nobody has written yet",
                501,
                r#"{"__type":"NotImplemented","message":"scaffolded"}"#,
                Some("NotImplemented"),
            ),
        ];

        for (case, status, body, error_type) in rows {
            let answered = untyped(response(*status, body, *error_type));
            assert_eq!(
                answered.headers().get(ERROR_TYPE_HEADER),
                *error_type,
                "case {case:?}: the header stays as rendered, got {:?}",
                answered.headers().get(ERROR_TYPE_HEADER)
            );
            assert_eq!(
                answered.body().bytes().expect("an in-memory body"),
                body.as_bytes(),
                "case {case:?}: the body must be byte-identical"
            );
        }
    }

    /// RULE: a 404 body that names no shape at all, or that is not a JSON object, is left
    /// to its renderer byte for byte — and still loses the header, which is read on its
    /// own.
    #[test]
    fn a_body_that_cannot_be_rewritten_still_loses_the_header() {
        // (case, the body)
        let rows: &[(&str, &str)] = &[
            ("a body naming no shape", r#"{"message":"Bot not found"}"#),
            ("an empty body", ""),
            ("a body that is not JSON", "<Error/>"),
            ("a body that is not an object", r#"["ResourceNotFoundError"]"#),
        ];

        for (case, body) in rows {
            let answered = untyped(response(GONE_STATUS, body, Some("ResourceNotFoundError")));
            assert_eq!(
                answered.headers().get(ERROR_TYPE_HEADER),
                None,
                "case {case:?}: the header must name no shape, got {:?}",
                answered.headers().get(ERROR_TYPE_HEADER)
            );
            assert_eq!(
                answered.body().bytes().expect("an in-memory body"),
                body.as_bytes(),
                "case {case:?}: a body with nothing to remove must be byte-identical"
            );
        }
    }

    /// RULE: a renderer that announced a body length has it corrected to the bytes the
    /// client receives, so the framing cannot disagree with the body.
    #[test]
    fn an_announced_body_length_follows_the_rewritten_body() {
        let body = r#"{"__type":"ResourceNotFoundError","message":"Bot not found"}"#;
        let mut given = response(GONE_STATUS, body, Some("ResourceNotFoundError"));
        given
            .headers_mut()
            .insert(CONTENT_LENGTH, body.len().to_string());
        let answered = untyped(given);
        let served = answered.body().bytes().expect("an in-memory body").len();
        assert_eq!(
            answered.headers().get(CONTENT_LENGTH),
            Some(served.to_string().as_str()),
            "expected the announced length {served}, got {:?}",
            answered.headers().get(CONTENT_LENGTH)
        );
    }
}
