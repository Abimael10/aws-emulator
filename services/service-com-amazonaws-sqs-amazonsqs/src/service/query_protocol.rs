// SPDX-License-Identifier: CC0-1.0

//! The AWS Query API this endpoint serves beside its `awsJson1_0` surface.
//!
//! # Two wire protocols, one API
//!
//! Amazon SQS publishes ONE API over TWO wire protocols. A modern SDK sends
//! `awsJson1_0` (`X-Amz-Target: AmazonSQS.<Operation>`, a JSON body, a JSON answer); an
//! older SDK, a hand-rolled signer or a language runtime that was never migrated sends
//! the AWS Query protocol — a `POST` of an `application/x-www-form-urlencoded` body whose
//! `Action` field names the operation — and is answered with an XML document
//! (`<{Operation}Response><{Operation}Result>…`, `<ErrorResponse><Error><Code>…` for a
//! refusal, `content-type: text/xml`). Both reach the same queues, because they are two
//! spellings of one API and not two services.
//!
//! The model carries both spellings. Its protocol trait is `awsJson1_0`
//! (`smithy_query(query="service_info")`), so the generated entry declares that one codec
//! and every request — form-encoded or not — is deserialized and answered as JSON; and its
//! shapes carry the query protocol's own wire names beside it (`CreateQueueRequest$Attributes`
//! is `@xmlName("Attribute") @xmlFlattened`, so the map arrives as `Attribute.N.Name` /
//! `Attribute.N.Value`, and `ReceiveMessageResult$Messages` is `@xmlName("Message")
//! @xmlFlattened`, so the answer carries one `<Message>` element per message). The framework
//! holds the codec that reads and writes exactly those forms off the model
//! ([`QueryServerProtocol`]), so what this module adds is the SELECTION of that codec for a
//! request that arrives in the query protocol — not a second serializer.
//!
//! # What is selected, and where
//!
//! * [`QueryServedDefinition`] wraps this service's model-derived half and answers
//!   [`ErasedServiceDefinition::wire_protocol`] with the query codec and the query error
//!   envelope for a request that addresses the query protocol
//!   ([`addresses_the_query_api`]). Every step of the request then runs in that protocol,
//!   because the framework's own per-request sequence reads the codec from there: the
//!   `Action` field is the routing key, the form body is deserialized against the
//!   operation's own input schema, the output is serialized against its output schema, and
//!   a routing or framework failure renders in the XML envelope. Nothing else about the
//!   service changes — identity, the routing table and every JSON answer stay the wrapped
//!   definition's.
//! * [`QueryServedApi`] is what serves such a request: it resolves the `Action` in a
//!   routing table of its own — the same audited operations, built with the QUERY error
//!   envelope, so a modelled refusal is named by the legacy query code the model binds it
//!   (`QueueDoesNotExist` is `<Code>AWS.SimpleQueueService.NonExistentQueue</Code>`) — and
//!   publishes the queue metrics every served request owes, exactly as the JSON path does.
//!
//! A request whose `Action` names no operation of this API is left to the framework's
//! routing failure, which the envelope above renders as the query protocol's own
//! `InvalidAction`.

use std::sync::{Arc, OnceLock};

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema_server::{ProtocolError, QueryServerProtocol, ServerProtocol};
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, ErasedServiceDefinition, OperationContext, OperationRoutingTable,
    PreDispatchProtocol, Refused, ServerHttpProtocol, Service, ServiceIdentity, WireProtocol,
    WireProtocolKind,
};

/// The envelope every framework-level failure of a query-protocol request renders in: the
/// `awsQuery` one (`<ErrorResponse><Error><Type>…</Type><Code>…</Code>…`), which is the
/// value the generator emits for a model whose protocol is `awsQuery`.
pub const QUERY_ENVELOPE: PreDispatchProtocol<'static> = PreDispatchProtocol::Query { ec2: false };

/// The framework's `awsQuery` codec, built once per process.
///
/// It is the SAME codec a service whose model declares `awsQuery` is generated with
/// (`QueryServerProtocol::aws_query()`), reading and writing the forms this service's own
/// shapes declare. The model carries no `@xmlNamespace`, so — exactly as the generator
/// does for such a model — no `xmlns` attribute is stamped on the envelope.
pub fn query_codec() -> &'static QueryServerProtocol {
    static CODEC: OnceLock<QueryServerProtocol> = OnceLock::new();
    CODEC.get_or_init(QueryServerProtocol::aws_query)
}

/// Whether `req` addresses the AWS Query protocol: the protocol a request NAMES, by the
/// framework's own reading of its headers ([`WireProtocolKind::detect`]), so a request is
/// read as a query request here exactly when the framework would read it as one.
pub fn addresses_the_query_api(req: &RtRequest) -> bool {
    WireProtocolKind::detect(req) == Some(WireProtocolKind::Query)
}

/// The same reading, over the PARTS a handler holds instead of over the whole request —
/// what a handler asks when the answer it is building is worded differently on this
/// protocol (`crate::state::queue_does_not_exist`).
///
/// The parts are lifted back into a request so the ONE reading above decides, rather than
/// the rule being written a second time over headers; the body is not part of that reading,
/// so an empty one stands in for it. The same lift `::service_common::query_wire_form`
/// makes in the other direction, for the same reason.
pub fn parts_address_the_query_api(parts: &::http::request::Parts) -> bool {
    let mut lifted = RtRequest::new(aws_smithy_types::body::SdkBody::empty());
    if lifted.set_uri(parts.uri.to_string()).is_err() {
        return false;
    }
    for (name, value) in &parts.headers {
        if let Ok(value) = value.to_str() {
            lifted
                .headers_mut()
                .append(name.as_str().to_string(), value.to_string());
        }
    }
    addresses_the_query_api(&lifted)
}

/// The operation the request's `Action` field names, through the query codec's own routing
/// key — so the field is read in one place and cannot be read two ways.
fn named_action(req: &RtRequest) -> Option<&str> {
    query_codec().routing_key(req).ok()
}

/// The code the AWS Query protocol answers a request whose `Action` names no operation of
/// the API it reached, and the one it answers a request that named no action at all.
///
/// Every AWS Query API answers these two, in the same two sentences: the action named and
/// the statement that it is not one of this web service's, and the statement that no action
/// was supplied. (The tree's other AWS Query surface,
/// `service-com-amazonaws-simpledbv2-simpledbv2`'s `sdb_query_api::envelope`, words them
/// identically, from the same API.)
const INVALID_ACTION: &str = "InvalidAction";
const MISSING_ACTION: &str = "MissingAction";

/// The status both carry: the caller named no operation of this API, which is a fault in
/// the request.
const NO_SUCH_ACTION_STATUS: u16 = 400;

/// The refusal of a query request whose `Action` this API does not publish, in that
/// protocol's own error document — the one the framework renders for every other
/// framework-level failure of such a request
/// ([`::aws_smithy_schema_server::query_error_body`]), with the class and the sentence this
/// API answers.
///
/// A query request names the API it is for by its `Action`, and every AWS Query API words
/// an action it does not have the same way: the action named, and the statement that it is
/// not one of this web service's. (The tree's other AWS Query surface,
/// `service-com-amazonaws-simpledbv2-simpledbv2`'s `sdb_query_api::envelope`, words it
/// identically.) A request that supplied no action at all has named nothing to look up and
/// is answered the other class.
pub fn no_such_action_response(req: &RtRequest) -> RtResponse {
    let (code, message) = match named_action(req).map(str::trim).filter(|action| !action.is_empty())
    {
        Some(action) => (
            INVALID_ACTION,
            format!("The action {action} is not valid for this web service."),
        ),
        None => (
            MISSING_ACTION,
            "No action was supplied with this request.".to_string(),
        ),
    };
    let body = ::aws_smithy_schema_server::query_error_body(false, code, &message);
    let mut resp = RtResponse::new(
        aws_smithy_runtime_api::http::StatusCode::try_from(NO_SUCH_ACTION_STATUS)
            .expect("400 is a status code"),
        aws_smithy_types::body::SdkBody::from(body),
    );
    let _ = resp.headers_mut().try_insert("content-type", "text/xml");
    service_common::query_error_envelope::with_request_id_header(resp)
}

/// This service's model-derived half, served in the protocol each request ARRIVES in.
///
/// Identity, the routing table and every answer to a request that is not a query request
/// come from the wrapped definition untouched. The one thing this wrapper decides is the
/// protocol: a request that addresses the query protocol is served by the query codec in
/// the query error envelope ([`QUERY_ENVELOPE`]), and every other request is served exactly
/// as the wrapped definition serves it.
///
/// Generic over the definition, so the audited definition this service already wraps nests
/// inside it and every query-protocol call is recorded like every other.
pub struct QueryServedDefinition<D> {
    inner: D,
}

impl<D: ErasedServiceDefinition> QueryServedDefinition<D> {
    /// Wrap `inner`, serving a query-protocol request in the query protocol.
    pub fn new(inner: D) -> Self {
        Self { inner }
    }

    /// The wrapped definition, for the seams that read this service's own model-derived
    /// values off it.
    pub fn inner(&self) -> &D {
        &self.inner
    }
}

impl<D: ErasedServiceDefinition> ErasedServiceDefinition for QueryServedDefinition<D> {
    fn identity(&self) -> &ServiceIdentity {
        self.inner.identity()
    }

    fn protocol(&self) -> &dyn ServerHttpProtocol {
        self.inner.protocol()
    }

    fn routing_table(&self) -> &OperationRoutingTable {
        self.inner.routing_table()
    }

    fn routing_error_response(&self, req: &RtRequest) -> RtResponse {
        if addresses_the_query_api(req) {
            return self.wire_protocol(req).routing_error_response(req);
        }
        self.inner.routing_error_response(req)
    }

    fn framework_error_response(&self, error: ProtocolError, req: &RtRequest) -> RtResponse {
        if addresses_the_query_api(req) {
            return self.wire_protocol(req).framework_error_response(error);
        }
        self.inner.framework_error_response(error, req)
    }

    fn refusal_response(
        &self,
        error: ProtocolError,
        chosen: Option<Refused>,
        req: &RtRequest,
    ) -> RtResponse {
        if addresses_the_query_api(req) {
            return self.wire_protocol(req).refusal_response(error, chosen);
        }
        self.inner.refusal_response(error, chosen, req)
    }

    fn wire_protocol(&self, req: &RtRequest) -> WireProtocol<'_> {
        if addresses_the_query_api(req) {
            return WireProtocol::new(query_codec(), QUERY_ENVELOPE);
        }
        self.inner.wire_protocol(req)
    }
}

/// The AWS Query API as the one [`ErasedOperation`] this service holds for it.
///
/// It serves the operation the request's `Action` names out of a table of its own: the same
/// audited operations the JSON path serves, built with the QUERY error envelope, so a
/// request that never reaches a handler (an absent required parameter, a value outside a
/// modelled bound) is refused in the XML envelope too and a modelled refusal is named by
/// the legacy query code the model binds to its class.
///
/// It makes no routing decision of its own: the key is the `Action` field the query codec
/// resolves, the operation is the one this service's own table names for it, and a key the
/// table does not name is handed back to the framework's routing failure — which the
/// definition above renders as this protocol's `InvalidAction`.
pub struct QueryServedApi<Eng: Send + Sync + 'static> {
    /// Every modelled operation of this service, audited, rendering a pre-dispatch failure
    /// in [`QUERY_ENVELOPE`].
    served: OperationRoutingTable,
    /// The SAME shared engine every other operation of this service is built from, so a
    /// queue a query-protocol call makes is the queue a JSON call reads.
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> QueryServedApi<Eng> {
    /// Build the surface from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            served: super::management_events::audited_table_in(&engine, QUERY_ENVELOPE),
            engine,
        }
    }

    /// Whether this API serves the operation `req` names — what
    /// `Service::override_operation` answers from, so a request naming no operation of
    /// this API falls through to the framework's routing failure instead of being served.
    pub fn names_the_operation(&self, req: &RtRequest) -> bool {
        named_action(req).is_some_and(|action| self.served.get(action).is_some())
    }
}

impl<Eng: Send + Sync + 'static> std::fmt::Debug for QueryServedApi<Eng> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryServedApi").finish_non_exhaustive()
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for QueryServedApi<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let served = named_action(req).and_then(|action| self.served.get(action));
            let answered = match served {
                Some(operation) => operation.run(svc, req, ctx).await,
                // Unreachable through `override_operation`, which hands a request here
                // only once this table has answered for its `Action`.
                None => svc.definition().routing_error_response(req),
            };
            // Every response of this service names the request it answered, exactly as the
            // JSON path's does.
            let answered = service_common::query_error_envelope::with_request_id_header(answered);
            // What the request left the queues holding, published off the answer with no
            // store lock held — the same reading every other request of this service owes.
            crate::state::publish_queue_metrics(&*self.engine);
            crate::state::drive_queue_metric_clock(&self.engine);
            answered
        })
    }

    /// `None` is "defer to the modelled operation the routing key names", which is the
    /// operation this surface delegates to: serving the API in its other protocol cannot
    /// change which credentials sign a call.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        None
    }
}
