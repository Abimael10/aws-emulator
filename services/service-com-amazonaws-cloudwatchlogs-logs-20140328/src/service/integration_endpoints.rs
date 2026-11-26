// SPDX-License-Identifier: CC0-1.0

//! The seam that SERVES the two addresses a read of an integration announces: the OpenSearch
//! Service application that hosts the vended logs dashboards, and the OpenSearch Serverless
//! collection those dashboards query.
//!
//! `GetIntegration` reports `application.applicationEndpoint` and `collection.collectionEndpoint`
//! — addresses a client DIALS, not labels it reads back — so each one is announced on the
//! `host[:port]` this emulator answers on, with the resource named in the path
//! ([`crate::state::integration_resource_path`]). Nothing in this service's model binds an
//! operation to those paths, so the front door could resolve no target service for a request to
//! one and answered `404 UnknownService` — for an address this service told the caller to use.
//! The service therefore claims those paths itself ([`claims_parts`]), exactly as every other
//! emulated data plane claims its own traffic, and this module owns the request-to-response step.
//!
//! WHAT it answers is what this service KNOWS about the resource the address names, read off the
//! integration record the create stored:
//!
//! | request | answer |
//! |---|---|
//! | `GET <endpoint>` | the resource this address belongs to: its own identifiers, the integration that owns it, and the status of its setup |
//! | any other route under it | `501`, naming the method and the route asked for |
//!
//! A resource no integration of the account holds — one whose integration was deleted, or a host
//! naming none — is `404`, and one whose integration has not finished provisioning is `404` as
//! well: until the setup completes, the OpenSearch Service resources are not there
//! (`OpenSearchResourceStatus` reports `NOT_FOUND`), and a reader of this plane sees the same
//! thing a reader of `GetIntegration` does.
//!
//! The OpenSearch query and dashboards protocols are NOT served here: no route answers an index,
//! a search or a dashboard, and every such request is the `501` above rather than an empty `200`,
//! so a client cannot mistake an unimplemented route for an implemented one with nothing to say.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::smithy_server_runtime::{http, BoxFuture, ErasedOperation, OperationContext, Service};
use ::std::sync::Arc;

use crate::state::{AddressedIntegrationResource, COLLECTION_RESOURCE};

/// The status the endpoint itself is answered with.
const OK_STATUS: u16 = 200;

/// The status a route under an endpoint that this plane does not serve is answered with: "nobody
/// has written it yet", never an empty success.
const NOT_IMPLEMENTED_STATUS: u16 = 501;

/// The status an address naming a resource the account does not hold is answered with.
const NOT_FOUND_STATUS: u16 = 404;

/// The status a request the log store could not answer at all is answered with: the service
/// could not complete it, which is its own fault rather than the client's.
const UNAVAILABLE_STATUS: u16 = 503;

/// The method the endpoint itself is read with: a client FETCHES the resource at that address.
const READ_METHOD: &str = "GET";

/// The route that names the endpoint itself rather than anything beneath it.
const ENDPOINT_ROUTE: &str = "/";

/// The media type every answer below carries.
const JSON_MEDIA_TYPE: &str = "application/json";

/// Whether the request these parts came from addresses one of the announced endpoints.
///
/// The claim is the PATH, which is this service's own ([`crate::state::INTEGRATION_PLANE_SEGMENT`]):
/// a request to one of these addresses is this service's to answer whatever method it carries,
/// because this service is what published the address.
#[must_use]
pub fn claims_parts(parts: &http::request::Parts) -> bool {
    crate::state::addressed_integration_resource(parts.uri.path()).is_some()
}

/// Whether a runtime-API request addresses one of them — the same claim as [`claims_parts`],
/// read off the request the other seams are handed.
#[must_use]
pub fn claims_request(req: &RtRequest) -> bool {
    crate::state::addressed_integration_resource(::service_common::request_path(req.uri()))
        .is_some()
}

/// The announced endpoints as one [`ErasedOperation`], so the router hands them the whole
/// request: they own the request-to-response step, since no modelled operation of this service
/// serves one.
pub struct IntegrationEndpoints<Eng> {
    /// The SAME shared engine the generated entry threads into every operation, so the resource
    /// this plane reports is the one `PutIntegration` stored and `GetIntegration` reads.
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> IntegrationEndpoints<Eng> {
    /// Build the plane from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }

    /// Answer one request addressed to `addressed`.
    fn serve(&self, method: &str, addressed: &AddressedIntegrationResource) -> RtResponse {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return json(UNAVAILABLE_STATUS, message("the log store is unavailable"));
        };
        let mut store = state.lock();
        // An integration provisions on its own, so the status this read reports is the one the
        // clock has ALREADY made due — materialised here rather than moved on because something
        // looked, exactly as the control-plane read of the same record does.
        store.materialize_due_integration_transitions(crate::state::now_ms());
        let Some(integration) = store.integrations.values().find(|integration| {
            integration.published_resource_host(&addressed.kind) == addressed.published_host
        }) else {
            return json(NOT_FOUND_STATUS, message(&not_found(addressed)));
        };
        let status = integration.resource_status();
        if status.status.as_deref() != Some(crate::state::OPENSEARCH_RESOURCE_STATUS_ACTIVE) {
            return json(NOT_FOUND_STATUS, message(&not_found(addressed)));
        }
        if addressed.route != ENDPOINT_ROUTE || !method.eq_ignore_ascii_case(READ_METHOD) {
            return json(
                NOT_IMPLEMENTED_STATUS,
                message(&format!(
                    "{method} {} is not served by this {} endpoint",
                    addressed.route, addressed.kind
                )),
            );
        }
        // The resource this address belongs to, read off the stored integration: its own
        // identifiers, the integration that owns it, and the status of its setup.
        let mut reported = ::serde_json::Map::new();
        reported.insert(
            "integrationName".to_string(),
            ::serde_json::Value::String(integration.name.clone()),
        );
        reported.insert(
            "integrationStatus".to_string(),
            ::serde_json::Value::String(integration.status.clone()),
        );
        if addressed.kind == COLLECTION_RESOURCE {
            reported.insert(
                "collectionArn".to_string(),
                ::serde_json::Value::String(integration.collection_arn()),
            );
            reported.insert(
                "collectionEndpoint".to_string(),
                ::serde_json::Value::String(integration.collection_endpoint()),
            );
        } else {
            reported.insert(
                "applicationArn".to_string(),
                ::serde_json::Value::String(integration.application_arn()),
            );
            reported.insert(
                "applicationId".to_string(),
                ::serde_json::Value::String(integration.application_id()),
            );
            reported.insert(
                "applicationEndpoint".to_string(),
                ::serde_json::Value::String(integration.application_endpoint()),
            );
        }
        if let Some(reported_status) = status.status {
            reported.insert(
                "status".to_string(),
                ::serde_json::Value::String(reported_status),
            );
        }
        json(
            OK_STATUS,
            ::serde_json::Value::Object(reported).to_string(),
        )
    }
}

/// The sentence an address naming a resource the account does not hold is answered with: the
/// kind of resource and the host that named it, both taken from the request.
fn not_found(addressed: &AddressedIntegrationResource) -> String {
    format!(
        "No {} of an integration of this account is published at {}.",
        addressed.kind, addressed.published_host
    )
}

/// One message body of this plane: the one-member JSON envelope this service reports a sentence
/// in.
fn message(sentence: &str) -> String {
    ::serde_json::Value::Object(
        [(
            "message".to_string(),
            ::serde_json::Value::String(sentence.to_string()),
        )]
        .into_iter()
        .collect(),
    )
    .to_string()
}

/// One answer of this plane: a JSON body under the status it is reported with.
fn json(status: u16, body: String) -> RtResponse {
    let mut response = RtResponse::new(
        ::aws_smithy_runtime_api::http::StatusCode::try_from(status)
            .expect("the status is a constant of this module"),
        ::aws_smithy_types::body::SdkBody::from(body),
    );
    response.headers_mut().insert("content-type", JSON_MEDIA_TYPE);
    response
}

impl<Eng: Send + Sync + 'static> ErasedOperation for IntegrationEndpoints<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        // `ctx` carries request metadata only; which resource is addressed is the PATH's
        // answer, and what is reported about it is the stored integration's.
        let _ = ctx;
        Box::pin(async move {
            match crate::state::addressed_integration_resource(::service_common::request_path(
                req.uri(),
            )) {
                Some(addressed) => self.serve(req.method(), &addressed),
                // Unreachable: this operation is named only for a request these endpoints
                // claim, which is a request to one of their paths.
                None => svc.definition().routing_error_response(req),
            }
        })
    }
}
