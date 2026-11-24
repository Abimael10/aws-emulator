// SPDX-License-Identifier: CC0-1.0

//! The CBOR REPRESENTATION of this service's `awsJson1_1` protocol: how a request whose
//! body is CBOR rather than JSON is read, and how it is answered.
//!
//! `awsJson1_1` is a body-only RPC protocol: the operation is named by `X-Amz-Target` and
//! the whole input is the body. Which ENCODING that body is in is the caller's choice,
//! stated in `Content-Type`: `application/x-amz-json-1.1` carries it as JSON and
//! [`CONTENT_TYPE`] carries the same members as CBOR. A client that asks for one is
//! answered in it — the response encoding mirrors the request's, for a success and for an
//! error alike — and both encodings decode to the same modelled input, so the operation
//! semantics after the body is read are identical.
//!
//! The framework selects a codec per request from the protocols the MODEL declares
//! (`vendor/smithy-server-runtime/src/wire_protocol.rs`), and a model declares a protocol,
//! not an encoding of one: this service declares `awsJson1_1` alone, so every request —
//! whatever its `Content-Type` — was handed to the JSON codec, which refuses a CBOR body
//! with the codec's own parse complaint before any operation runs. The vendored tree is
//! read-only from a service crate, so the second encoding is served here, at the seam a
//! service has around dispatch ([`super::StatedPerOperation`]).
//!
//! Nothing of the protocol is re-implemented. The encoding is the framework's own CBOR
//! [`CborCodec`] driven through the framework's own RPC protocol
//! ([`HttpRpcServerProtocol`]) with this representation's media type and the SAME
//! `X-Amz-Target` routing and protocol id as the JSON representation ([`protocol`]), and
//! the request is then run by the framework's own per-request sequence
//! ([`smithy_server_runtime::run_operation_http`], [`answer`]) — deserialize, validate,
//! handle, serialize, telemetry — with [`ServedInCbor`] as the service it reads its codec
//! from. So an operation cannot behave differently in the two encodings.
//!
//! What this module owns beyond that is the LABEL on a framework-level failure: the
//! framework's CBOR error envelope is the one `rpcv2Cbor` publishes, which carries that
//! protocol's media type and its `smithy-protocol` header, neither of which belongs on an
//! `awsJson1_1` response ([`labelled`]).

use std::sync::{Arc, OnceLock};

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema::ShapeId;
use aws_smithy_schema_server::rpc::{routing_key_from_x_amz_target, HttpRpcServerProtocol};
use aws_smithy_schema_server::{CborCodec, ProtocolError};
use smithy_server_runtime::{
    http, ErasedServiceDefinition, Operation, OperationContext, PreDispatchProtocol, Refusal,
    Refused, ServerHttpProtocol, Service, Violation,
};

/// The media type a caller names this representation of `awsJson1_1` by, and the one every
/// response to such a request carries.
pub const CONTENT_TYPE: &str = "application/x-amz-cbor-1.1";

/// The media type the framework's own CBOR error envelope labels a body with: it is
/// `rpcv2Cbor`'s, the other protocol that encodes bodies as CBOR. The bytes are what this
/// representation needs; the label is not ([`labelled`]).
const FRAMEWORK_CBOR_CONTENT_TYPE: &str = "application/cbor";

/// The response header that same envelope stamps, which the `rpcv2Cbor` specification
/// requires and no `awsJson1_1` response carries ([`labelled`]).
const SMITHY_PROTOCOL_HEADER: &str = "smithy-protocol";

/// The bytes an EMPTY request body stands for in this representation: the empty CBOR map,
/// the CBOR spelling of the empty JSON object the JSON representation substitutes. An
/// operation whose every member is optional is callable with no body at all.
const EMPTY_CBOR_MAP: &[u8] = &[0xa0];

/// Whether `req` asks for this representation: its `Content-Type` names [`CONTENT_TYPE`].
///
/// The media type is read as a media type — the part before any parameter, case-insensitively
/// — because `Content-Type` is allowed to carry parameters and its case is not significant.
#[must_use]
pub fn is_requested(req: &RtRequest) -> bool {
    req.headers()
        .get("content-type")
        .is_some_and(|header| names(header, CONTENT_TYPE))
}

/// Whether one `Content-Type` header value names `media_type`: the part before any parameter,
/// trimmed, compared without case. ONE reading for both directions, so a label this module
/// recognises on a request it recognises on a response too ([`labelled`]).
fn names(header: &str, media_type: &str) -> bool {
    header
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case(media_type)
}

/// This service's protocol in its CBOR representation, built ONCE for the process.
///
/// Everything but the codec and the media type is the JSON representation's: the protocol
/// ID is `awsJson1_1`, because that IS the protocol this service declares — an encoding is
/// not a protocol — and the routing key is read from `X-Amz-Target`, the only place an
/// `awsJson1_1` request names its operation.
///
/// It is a process-wide value rather than a per-request one because it holds no request
/// state: a protocol built per request would allocate a codec per call, and the framework's
/// own protocol seam ([`Service::override_protocol`]) hands back a borrow that must outlive
/// the request.
#[must_use]
pub fn protocol() -> &'static HttpRpcServerProtocol<CborCodec> {
    static PROTOCOL: OnceLock<HttpRpcServerProtocol<CborCodec>> = OnceLock::new();
    PROTOCOL.get_or_init(|| {
        HttpRpcServerProtocol::new(
            ShapeId::from_static("aws.protocols#awsJson1_1", "aws.protocols", "awsJson1_1"),
            CborCodec::default(),
            CONTENT_TYPE,
            Arc::new(routing_key_from_x_amz_target),
        )
        .with_empty_body(EMPTY_CBOR_MAP)
    })
}

/// This service as the framework's per-request sequence reads it for a request in the CBOR
/// representation: the SAME generated definition the mounted service holds — one identity,
/// one routing table, one set of model-derived facts — with the codec of this
/// representation.
///
/// It is not a second service and is never mounted: the router claims a request through the
/// one [`crate::service::EmulatedService`], which hands the request to the operation it
/// routed, and the operation runs it through this when the caller asked for CBOR. It exists
/// because the framework's codec seam is per SERVICE rather than per request
/// ([`Service::override_protocol`]), so the encoding a request asked for is expressed by
/// running the sequence against this view of the same service.
pub struct ServedInCbor<Eng: Send + Sync + 'static> {
    /// The one generated definition, shared with the mounted service.
    entry: Arc<crate::ServiceDefinition<Eng>>,
}

impl<Eng: Send + Sync + 'static> ServedInCbor<Eng> {
    /// This view of the service that `entry` is the generated definition of.
    pub fn new(entry: Arc<crate::ServiceDefinition<Eng>>) -> Self {
        Self { entry }
    }
}

impl<Eng: Send + Sync + 'static> Service for ServedInCbor<Eng> {
    fn definition(&self) -> &dyn ErasedServiceDefinition {
        self.entry.as_ref()
    }

    fn can_handle(&self, parts: &http::request::Parts) -> bool {
        // The same claim the mounted service makes — this view is of the same service — but
        // it is never asked: this is not mounted in the router.
        match smithy_server_runtime::resolve_route_key(parts) {
            Some(key) => key == self.entry.identity().signing_name(),
            None => false,
        }
    }

    fn override_protocol(&self) -> Option<&dyn ServerHttpProtocol> {
        Some(protocol())
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The same module the mounted service words a refusal through, so a refused request
        // carries one sentence whichever encoding it arrived in.
        match refusal {
            Refusal::Invalid(violations) => {
                super::validation::refusal(violations, ::std::collections::HashMap::new)
            }
            _ => None,
        }
    }
}

/// The envelope a framework-level failure of a request in this representation renders in:
/// the framework's CBOR one, which encodes `{"__type", "message"}` as CBOR. Its LABEL is
/// `rpcv2Cbor`'s and is corrected by [`labelled`].
const CBOR_ENVELOPE: PreDispatchProtocol<'static> = PreDispatchProtocol::RpcV2Cbor;

/// Serve `req` — already routed to `operation` — in this representation.
///
/// The framework's own per-request sequence, driven with `served_in_cbor` as the service it
/// reads its codec from, so the body is read as CBOR and the output, the modelled errors and
/// the telemetry are the operation's own.
pub async fn answer<'r, O: Operation>(
    operation: &'r O,
    served_in_cbor: &'r dyn Service,
    req: &'r RtRequest,
    ctx: &'r OperationContext<'r>,
) -> RtResponse {
    labelled(
        smithy_server_runtime::run_operation_http(
            operation,
            served_in_cbor,
            req,
            ctx,
            CBOR_ENVELOPE,
        )
        .await,
    )
}

/// This service's own statement of the constraints `violations` broke, in this
/// representation: the framework's CBOR envelope carrying the sentence
/// [`super::validation`] worded.
#[must_use]
pub fn refusal_response(violations: Vec<Violation>, stated: Refused) -> RtResponse {
    labelled(smithy_server_runtime::protocol_error_response_with(
        CBOR_ENVELOPE,
        ProtocolError::Invalid(violations),
        Some(stated),
    ))
}

/// `resp` labelled as this representation, where it carries the framework's own CBOR label.
///
/// The CBOR error envelope is shared with `rpcv2Cbor`, which labels its bodies
/// `application/cbor` and stamps the `smithy-protocol` header its specification requires.
/// The bytes are the ones this representation needs; those two markers name the other
/// protocol, and an `awsJson1_1` client dispatches on the media type it asked for. A
/// response already labelled by this representation's own codec is left exactly as that
/// codec wrote it.
fn labelled(mut resp: RtResponse) -> RtResponse {
    let framework_labelled = resp
        .headers()
        .get("content-type")
        .is_some_and(|header| names(header, FRAMEWORK_CBOR_CONTENT_TYPE));
    if framework_labelled {
        resp.headers_mut().insert("content-type", CONTENT_TYPE);
        resp.headers_mut().remove(SMITHY_PROTOCOL_HEADER);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_smithy_types::body::SdkBody;
    use smithy_server_runtime::ServerProtocol;

    /// A request carrying `content_type`, or none at all.
    fn request(content_type: Option<&str>) -> RtRequest {
        let mut req = RtRequest::new(SdkBody::empty());
        if let Some(content_type) = content_type {
            req.headers_mut().insert("content-type", content_type.to_string());
        }
        req
    }

    /// RULE: the representation is the one the request's `Content-Type` MEDIA TYPE names,
    /// read case-insensitively and without its parameters; every other media type, and a
    /// request that states none, is the JSON representation.
    #[test]
    fn the_media_type_names_the_representation() {
        // (case, the header value, whether it asks for this representation)
        let rows: &[(&str, Option<&str>, bool)] = &[
            ("this representation", Some(CONTENT_TYPE), true),
            ("upper case", Some("APPLICATION/X-AMZ-CBOR-1.1"), true),
            ("with a charset parameter", Some("application/x-amz-cbor-1.1; charset=utf-8"), true),
            ("padded", Some("  application/x-amz-cbor-1.1  "), true),
            ("the JSON representation", Some("application/x-amz-json-1.1"), false),
            ("the other awsJson version", Some("application/x-amz-json-1.0"), false),
            ("rpcv2Cbor's media type", Some("application/cbor"), false),
            ("plain JSON", Some("application/json"), false),
            ("no content type at all", None, false),
        ];
        for (case, header, expected) in rows.iter().copied() {
            let actual = is_requested(&request(header));
            assert_eq!(
                actual, expected,
                "case {case:?} ({header:?}): expected is_requested == {expected}, got {actual}"
            );
        }
    }

    /// The protocol served is this service's own, in the CBOR encoding: one process-wide
    /// value, identified as `awsJson1_1`.
    #[test]
    fn the_representation_serves_this_services_protocol() {
        let protocol = protocol();
        assert_eq!(
            protocol.protocol_id().to_string(),
            "aws.protocols#awsJson1_1",
            "case the protocol id: an encoding is not a protocol, so it stays this service's"
        );
        assert!(
            std::ptr::eq(protocol, super::protocol()),
            "case built once: every request reads the one process-wide protocol"
        );
    }

    /// RULE: a response is labelled with the media type the caller asked for. The
    /// framework's CBOR envelope — shared with `rpcv2Cbor` — is relabelled and loses that
    /// protocol's header; anything else is left as its codec wrote it.
    #[test]
    fn a_framework_envelope_is_relabelled_as_this_representation() {
        // (case, the label the response arrived with, the label it leaves with)
        let rows: &[(&str, &str, &str)] = &[
            ("the framework's CBOR envelope", FRAMEWORK_CBOR_CONTENT_TYPE, CONTENT_TYPE),
            (
                "the framework's envelope with a parameter",
                "application/cbor; charset=utf-8",
                CONTENT_TYPE,
            ),
            ("already this representation", CONTENT_TYPE, CONTENT_TYPE),
            ("a JSON body", "application/x-amz-json-1.1", "application/x-amz-json-1.1"),
        ];
        for &(case, arrived, expected) in rows {
            let mut resp = RtResponse::new(
                ::aws_smithy_runtime_api::http::StatusCode::try_from(400u16).expect("400"),
                SdkBody::from("body"),
            );
            resp.headers_mut().insert("content-type", arrived.to_string());
            resp.headers_mut().insert(SMITHY_PROTOCOL_HEADER, "rpc-v2-cbor");
            let resp = labelled(resp);
            let actual = resp.headers().get("content-type").unwrap_or_default();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the label {expected:?}, got {actual:?}"
            );
        }
        // The `rpcv2Cbor` header never survives on a response of this protocol.
        let mut resp = RtResponse::new(
            ::aws_smithy_runtime_api::http::StatusCode::try_from(400u16).expect("400"),
            SdkBody::from("body"),
        );
        resp.headers_mut()
            .insert("content-type", FRAMEWORK_CBOR_CONTENT_TYPE);
        resp.headers_mut()
            .insert(SMITHY_PROTOCOL_HEADER, "rpc-v2-cbor");
        let resp = labelled(resp);
        assert_eq!(
            resp.headers().get(SMITHY_PROTOCOL_HEADER),
            None,
            "case the protocol header: an awsJson1_1 response carries none"
        );
    }
}
