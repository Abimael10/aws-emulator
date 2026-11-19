// SPDX-License-Identifier: CC0-1.0

//! The FRAMING one request of this endpoint arrives in, and therefore the one it is read
//! and answered in.
//!
//! This endpoint serves its nine operations in two Smithy RPC framings. The one the model
//! names is `awsJson1_1`
//! (`smithy_query(query="service_info",model_path="services/service-com-amazonaws-resourcegroupstaggingapi-resourcegroupstaggingapi-20170126/smithy-model/com-amazonaws-resourcegroupstaggingapi-resourcegroupstaggingapi-20170126.json")`
//! -> protocols `["awsJson1_1"]`, `sigv4Name` `tagging`), which posts every operation to
//! `/` and names it in `X-Amz-Target`. The other is Smithy RPC v2 CBOR, which SDKs frame
//! JSON-protocol AWS services in as they migrate onto it: it names the operation in the
//! request PATH (`/service/{Service}/operation/{Operation}`), declares itself with the
//! `smithy-protocol` header, carries its body as CBOR, and deliberately sends no
//! `X-Amz-Target` at all.
//!
//! RULE: a request is read, dispatched and answered in the framing THE REQUEST names, so
//! the `X-Amz-Target` requirement belongs to the `awsJson` framing alone. A framing is
//! named by the request's headers and path and by nothing else, so which one it is is
//! settled before any byte of the body is read, and both framings decode to the same
//! modelled input and are served by the same operation over the same state: no operation
//! can behave differently in the two, and a read in either cannot perturb what a later
//! caller sees.
//!
//! Nothing of either framing is re-implemented here. The framework selects a codec per
//! request from the protocols the MODEL declares and refuses a request naming any other
//! (`vendor/smithy-server-runtime/src/wire_protocol.rs`), so a model naming one protocol
//! had every request — whatever framed it — handed to that protocol's codec, whose routing
//! key is the absent header. The vendored tree is read-only from a service crate, so the
//! second framing is served at the seam a service HAS around its model-derived half
//! ([`crate::service::EmulatedService::definition`]): [`ServedInTheFramingOfTheRequest`]
//! answers the framework's own per-request protocol accessor with the framework's own
//! `rpcv2Cbor` protocol — its CBOR codec, its path routing, its `smithy-protocol` and
//! `application/cbor` response headers — for a request that names that framing, and with
//! the wrapped definition's own answer for every other. Every later step of the request
//! reads that one accessor (operation lookup, the modelled auth schemes, deserialize,
//! serialize, and every error envelope), so one request cannot be routed in one framing
//! and answered in another.

use std::sync::OnceLock;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema_server::rpc::{HttpRpcServerProtocol, Rpcv2Cbor};
use aws_smithy_schema_server::CborCodec;
use smithy_server_runtime::{
    ErasedServiceDefinition, OperationRoutingTable, PreDispatchProtocol, ServerHttpProtocol,
    ServiceIdentity, WireProtocol, WireProtocolKind,
};

/// Whether `req` names the Smithy RPC v2 CBOR framing.
///
/// The question is asked of the framework's own detection — the one rule the whole tree
/// recognises a framing by, reading the `smithy-protocol` header and the
/// `/service/{Service}/operation/{Operation}` path and nothing else — so this service
/// cannot recognise a request the framework would not, or miss one it would.
fn names_rpc_v2_cbor(req: &RtRequest) -> bool {
    matches!(
        WireProtocolKind::detect(req),
        Some(WireProtocolKind::RpcV2Cbor)
    )
}

/// The framework's own Smithy RPC v2 CBOR protocol, built ONCE for the process.
///
/// Process-wide rather than per-request because it holds no request state, and because the
/// per-request accessor hands back a borrow that outlives the request it was asked for.
fn rpc_v2_cbor() -> &'static HttpRpcServerProtocol<CborCodec> {
    static PROTOCOL: OnceLock<HttpRpcServerProtocol<CborCodec>> = OnceLock::new();
    PROTOCOL.get_or_init(Rpcv2Cbor)
}

/// This service's model-derived half, serving each request in the framing the request
/// itself names ([module docs](self)).
///
/// Identity, the routing table and the model's own protocols come from the wrapped
/// definition untouched — nothing model-derived is restated here. Generic over that
/// definition so it wraps the generated entry, or another wrapper around it.
pub struct ServedInTheFramingOfTheRequest<D> {
    inner: D,
}

impl<D: ErasedServiceDefinition> ServedInTheFramingOfTheRequest<D> {
    /// Wrap a definition — the generated entry, or another wrapper around it.
    pub fn new(inner: D) -> Self {
        Self { inner }
    }
}

impl<D: ErasedServiceDefinition> ErasedServiceDefinition for ServedInTheFramingOfTheRequest<D> {
    fn identity(&self) -> &ServiceIdentity {
        self.inner.identity()
    }

    fn protocol(&self) -> &dyn ServerHttpProtocol {
        self.inner.protocol()
    }

    fn routing_table(&self) -> &OperationRoutingTable {
        self.inner.routing_table()
    }

    /// Every framework-level failure renders in the envelope of the framing the request is
    /// served in — the three renderers below read [`Self::wire_protocol`] for that reason,
    /// so a routing failure, a transport failure and a refusal cannot answer a CBOR call in
    /// the JSON envelope.
    fn routing_error_response(&self, req: &RtRequest) -> RtResponse {
        self.wire_protocol(req).routing_error_response(req)
    }

    fn framework_error_response(
        &self,
        error: aws_smithy_schema_server::ProtocolError,
        req: &RtRequest,
    ) -> RtResponse {
        self.wire_protocol(req).framework_error_response(error)
    }

    fn refusal_response(
        &self,
        error: aws_smithy_schema_server::ProtocolError,
        chosen: Option<smithy_server_runtime::Refused>,
        req: &RtRequest,
    ) -> RtResponse {
        self.wire_protocol(req).refusal_response(error, chosen)
    }

    fn wire_protocol(&self, req: &RtRequest) -> WireProtocol<'_> {
        if names_rpc_v2_cbor(req) {
            return WireProtocol::new(rpc_v2_cbor(), PreDispatchProtocol::RpcV2Cbor);
        }
        self.inner.wire_protocol(req)
    }
}

#[cfg(test)]
mod tests {
    use super::names_rpc_v2_cbor;
    use aws_smithy_runtime_api::http::Request as RtRequest;
    use aws_smithy_types::body::SdkBody;

    /// A request to `uri` carrying `headers`.
    fn asked(uri: &str, headers: &[(&str, &str)]) -> RtRequest {
        let mut req = RtRequest::new(SdkBody::empty());
        req.set_uri(uri.to_string()).expect("a valid uri");
        for (name, value) in headers {
            req.headers_mut().insert(name.to_string(), value.to_string());
        }
        req
    }

    /// One row of the table below: the case, the request's uri, the headers it
    /// carries, and whether it names the CBOR framing.
    type FramingCase = (
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
        bool,
    );

    /// RULE: the CBOR framing is named by the `smithy-protocol` header or by the path that
    /// framing routes on; the `awsJson` framing of the same endpoint is named by neither,
    /// whatever else it carries.
    #[test]
    fn a_request_names_the_cbor_framing_by_that_framings_own_markers() {
        // (case, request uri, headers, whether the request names the CBOR framing)
        let rows: &[FramingCase] = &[
            (
                "the framing's own header and path",
                "/service/ResourceGroupsTaggingAPI_20170126/operation/GetResources",
                &[
                    ("smithy-protocol", "rpc-v2-cbor"),
                    ("content-type", "application/cbor"),
                ],
                true,
            ),
            (
                "that header spelled in upper case",
                "/",
                &[("smithy-protocol", "RPC-V2-CBOR")],
                true,
            ),
            (
                "the framing's path with no header",
                "/service/ResourceGroupsTaggingAPI_20170126/operation/GetTagKeys",
                &[],
                true,
            ),
            (
                "a protocol header naming something else",
                "/",
                &[("smithy-protocol", "rpc-v2-json")],
                false,
            ),
            (
                "the awsJson framing of the same operation",
                "/",
                &[
                    ("content-type", "application/x-amz-json-1.1"),
                    (
                        "x-amz-target",
                        "ResourceGroupsTaggingAPI_20170126.GetResources",
                    ),
                ],
                false,
            ),
            (
                "a CBOR body with neither marker",
                "/",
                &[("content-type", "application/cbor")],
                false,
            ),
            ("no header at all", "/", &[], false),
        ];
        for (case, uri, headers, expected) in rows.iter().copied() {
            let actual = names_rpc_v2_cbor(&asked(uri, headers));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the request to name the CBOR framing: {expected}, \
                 got {actual}"
            );
        }
    }
}
