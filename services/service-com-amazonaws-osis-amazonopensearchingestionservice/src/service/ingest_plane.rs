// SPDX-License-Identifier: CC0-1.0

//! The pipeline INGEST data plane: how a request a client sends to the address a pipeline
//! publishes is recognised, and what the pipeline does with the records it carries.
//!
//! A pipeline publishes the address its records go to — `Pipeline$IngestEndpointUrls`, "the
//! ingestion endpoints for the pipeline, which you can send data to" — and a client READS
//! that address and POSTs to it. That address is the emulator's own endpoint with the
//! pipeline named in the path ([`crate::state::ingest_plane_base`]), which names no modelled
//! operation of this service: the front door could resolve no operation for such a request
//! and answered a routing failure, for the one address this service told the caller to send
//! data to. So this plane claims it ([`claims_parts`], read by `Service::can_handle`) and
//! serves it ([`IngestPlane`], returned by `Service::override_operation`).
//!
//! What it serves is what the pipeline's own CONFIGURATION says
//! ([`crate::state::ingest_routes`]): the paths its `http` sources accept records at, and
//! the sinks those sub-pipelines write to. A POST of records to a configured source path is
//! written into EVERY sink the configuration names for that sub-pipeline
//! ([`crate::state::delivered`]) and only then answered, so a 200 means the records are in
//! every destination the pipeline declared — readable there through that service's own API.
//! A route that declares a sink this emulator cannot write is answered for that BEFORE
//! anything is written, so such a POST delivers nothing at all; a destination that refuses
//! the object after an earlier one of the same route took it ends the request too, naming how
//! far it got ([`deliver`]), so a partly delivered batch is never answered as accepted.
//! Nothing is invented: a path no source declares, a method a source does not take, a body
//! that is not records, and a sink this emulator cannot write are each answered as such.
//!
//! Serving records MUTATES no pipeline: the records go to the sink, and the one write this
//! plane makes to this service's own store is the status transition the clock already owed
//! the pipeline it answers about — the same materialisation every operation of this service
//! makes ([`crate::state::StoredPipeline::settle`]).

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;
use ::serde_json::Value;
use ::smithy_server_runtime::{http, BoxFuture, ErasedOperation, OperationContext, Service};

use crate::state::{
    addressed_pipeline, delivered, ingest_routes, route_at, service_state, IngestRoute,
    PipelineSink, S3Sink, STATUS_ACTIVE,
};

/// The method an `http` source takes records by, which is the only method a source path
/// serves.
const INGEST_METHOD: &str = "POST";

/// The most request-body bytes this plane reads.
///
/// Source: the Data Prepper `http` source's `max_request_length`, which defaults to 10 MB. It
/// narrows what the framework's own listener already caps rather than being the only bound on
/// a buffered body, and it is read before the body is parsed or written anywhere.
const MAX_REQUEST_BODY_BYTES: usize = 10 * 1024 * 1024;

/// How many sinks of one sub-pipeline a single POST is written to.
///
/// EMULATOR RESOURCE GUARD, not an AWS limit: every sink of a route is written the whole
/// batch, so the work one request can ask for is its body ([`MAX_REQUEST_BODY_BYTES`]) times
/// the number of sinks the stored configuration declares — a count a caller chooses. This
/// caps that PRODUCT explicitly, before a record is serialised or an object written, rather
/// than leaving it to what the configuration body's own bounds happen to allow. It is far past
/// any Data Prepper sub-pipeline, which fans out to a handful of destinations.
pub const MAX_SINKS_PER_DELIVERY: usize = 8;

/// What the source answers a body past [`MAX_REQUEST_BODY_BYTES`] with.
const PAYLOAD_TOO_LARGE_STATUS: u16 = 413;

/// What it answers a body that is not a list of records with.
const BAD_REQUEST_STATUS: u16 = 400;

/// What this plane answers for an address no source serves: a path the pipeline's
/// configuration declares no `http` source at, and a pipeline the store no longer holds.
const NOT_FOUND_STATUS: u16 = 404;

/// What a source path answers a method it does not take with.
const METHOD_NOT_ALLOWED_STATUS: u16 = 405;

/// What the endpoint answers while the pipeline behind it is not running: the status an
/// address whose server is not up answers, read off the pipeline's own stored status.
const NOT_RUNNING_STATUS: u16 = 503;

/// What it answers when the records could not be written to a sink the configuration names,
/// and when that configuration names no destination to write them to at all: nothing holds
/// them, so the request did not succeed.
const NOT_DELIVERED_STATUS: u16 = 500;

/// What it answers for a sink KIND, or a sink codec, this emulator does not write: the
/// records would be accepted by nothing, and the reason is this emulator's own gap rather
/// than anything the caller can fix.
const NOT_IMPLEMENTED_STATUS: u16 = 501;

/// The status a source answers an accepted batch with. The Data Prepper `http` source
/// answers the request once the records are in the pipeline, with no body of its own.
const ACCEPTED_STATUS: u16 = 200;

/// The media type an answer this plane words itself carries.
const PLAIN_TEXT: &str = "text/plain; charset=utf-8";

/// The auth schemes a credentialed ingest request is verified under.
///
/// SigV4 is the only scheme this service's model declares, and it is the one a client signs a
/// request to a pipeline's ingestion endpoint with, so naming it is what makes the router
/// verify the signature before any record is accepted — and what makes it refuse an ingest
/// request that presents no credential at all, exactly as this service refuses every other
/// unsigned call.
const INGEST_AUTH_SCHEMES: &[&str] = &[::emulator_server::aws_credentials::SIGV4_SCHEME];

/// Whether this plane serves the request `parts` addresses: a path-style ingest address
/// naming a pipeline this service HOLDS.
///
/// Read from the one shared store rather than from a path shape alone, so one request keeps
/// one claimant.
pub fn claims_parts<Eng: Send + Sync + 'static>(
    engine: &Eng,
    parts: &http::request::Parts,
) -> bool {
    claims_path(engine, parts.uri.path())
}

/// [`claims_parts`] for the runtime-API request the serving seam is handed, read by the same
/// rule, so the claim and the serving cannot disagree about which requests belong here.
pub fn claims_request<Eng: Send + Sync + 'static>(engine: &Eng, req: &RtRequest) -> bool {
    claims_path(engine, ::service_common::request_path(req.uri()))
}

/// The one rule both of those read.
fn claims_path<Eng: Send + Sync + 'static>(engine: &Eng, path: &str) -> bool {
    let Some((pipeline_name, _)) = addressed_pipeline(path) else {
        return false;
    };
    let Some(state) = service_state(engine) else {
        return false;
    };
    let store = state.lock();
    store.pipelines.contains_key(pipeline_name)
}

/// What this plane answers for a path-style ingest address naming a pipeline this service
/// does NOT hold: nothing is served there.
///
/// Read from `Service::override_routing_error`, which the router reaches for a request no
/// operation claimed — which is what such an address is, since the claim above is narrowed to
/// a pipeline the store holds. The alternative is this protocol's own routing failure, which
/// says the operation the path named is not implemented; the path named no operation, it
/// named an address, and the honest answer is that nothing answers at it.
pub fn serve_unclaimed(req: &RtRequest) -> Option<RtResponse> {
    addressed_pipeline(::service_common::request_path(req.uri()))
        .map(|_| worded(NOT_FOUND_STATUS, "No pipeline is served at this address."))
}

/// The ingest plane as the ROUTER serves it: the operation `Service::override_operation`
/// hands back for a request addressed to a pipeline this service holds.
pub struct IngestPlane<Eng> {
    /// The ONE shared engine this service's operations read and write, so a request served
    /// here sees exactly what the control plane persisted.
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> IngestPlane<Eng> {
    /// Mount the plane over the shared engine.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }

    /// Whether this plane serves `req`, by the one rule the claim uses.
    pub fn claims(&self, req: &RtRequest) -> bool {
        claims_request(&*self.engine, req)
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for IngestPlane<Eng> {
    fn run<'r>(
        &'r self,
        _svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move { serve(&*self.engine, req, ctx).await })
    }

    /// This is NOT a modelled operation, so there is no modelled operation for the router to
    /// read schemes off: without this the router would find no scheme for an ingest request
    /// and could not verify the credential it carries.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        Some(INGEST_AUTH_SCHEMES)
    }
}

/// What this plane resolved a request to: where the pipeline's sink is addressed, and the
/// route the request's path reached.
struct Addressed {
    region: String,
    route: IngestRoute,
}

/// Serve one request a pipeline's ingestion endpoint received.
async fn serve<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
    ctx: &OperationContext<'_>,
) -> RtResponse {
    let now = ::std::time::SystemTime::now();
    let path = ::service_common::request_path(req.uri());
    let Some((pipeline_name, source_path)) = addressed_pipeline(path) else {
        return worded(NOT_FOUND_STATUS, "No pipeline is served at this address.");
    };

    // What the REQUEST itself is, read before any pipeline is looked at, so each of these
    // refusals is the same whatever state the pipeline it addresses is in.
    if !req.method().eq_ignore_ascii_case(INGEST_METHOD) {
        return worded(
            METHOD_NOT_ALLOWED_STATUS,
            &format!("This ingestion endpoint takes records by {INGEST_METHOD}."),
        );
    }
    // The router hands this seam a request whose body it has already buffered, so an absent
    // one is an empty body rather than a stream this plane declined to read.
    let body = req.body().bytes().unwrap_or_default();
    if body.len() > MAX_REQUEST_BODY_BYTES {
        return worded(
            PAYLOAD_TOO_LARGE_STATUS,
            &format!("A request carries at most {MAX_REQUEST_BODY_BYTES} bytes of records."),
        );
    }

    // The pipeline is read out of the store and the store RELEASED before anything is
    // parsed or written to a sink: the delivery awaits, and no guard may be held across it.
    let resolved = {
        let Some(state) = service_state(engine) else {
            return worded(
                NOT_DELIVERED_STATUS,
                "The OpenSearch Ingestion pipeline store is unavailable.",
            );
        };
        let mut store = state.lock();
        let Some(pipeline) = store.pipelines.get_mut(pipeline_name) else {
            return worded(NOT_FOUND_STATUS, "No pipeline is served at this address.");
        };
        // The one write a serving request makes, and only to the pipeline it is addressed
        // to: the transition the clock already owes this record, which was due whether or
        // not anybody posted.
        pipeline.settle(now);
        if pipeline.status != STATUS_ACTIVE {
            return worded(
                NOT_RUNNING_STATUS,
                &format!(
                    "The pipeline is not running: its status is {}.",
                    pipeline.status
                ),
            );
        }
        let routes = ingest_routes(&pipeline.pipeline_configuration_body);
        let Some(route) = route_at(&routes, source_path) else {
            return worded(
                NOT_FOUND_STATUS,
                "The pipeline's configuration declares no http source at this path.",
            );
        };
        Addressed {
            // Where the sink is addressed when it names no Region of its own: the Region
            // the pipeline itself lives in, read off its own ARN.
            region: ::service_common::arn::region_of(&pipeline.pipeline_arn, crate::state::ARN_SERVICE)
                .unwrap_or(crate::state::DEFAULT_REGION)
                .to_string(),
            route: route.clone(),
        }
    };

    let Some(records) = records_of(body) else {
        return worded(
            BAD_REQUEST_STATUS,
            "The request body must be a JSON array of records.",
        );
    };

    deliver(ctx, &resolved, &records, now).await
}

/// The records a request body carries, or `None` for a body that is not a list of them.
///
/// An `http` source takes a JSON ARRAY of records, so a body that is one array of JSON
/// values is the request's records and anything else is not a batch at all.
fn records_of(body: &[u8]) -> Option<Vec<Value>> {
    match ::serde_json::from_slice::<Value>(body) {
        Ok(Value::Array(records)) => Some(records),
        _ => None,
    }
}

/// Write `records` to EVERY sink of the route they were posted to, and answer for what the
/// sinks did.
///
/// A Data Prepper sub-pipeline writes its records to each of its sinks, so the request is
/// answered only once every one of them holds them — a client that reads any of those
/// destinations straight after its POST finds the records there.
///
/// Which means a route whose sinks are not ALL writable here is answered BEFORE anything is
/// written: a sink kind or codec this emulator does not write, a sink that names no
/// destination, a route that declares no sink at all, and a fan-out past
/// [`MAX_SINKS_PER_DELIVERY`] each end the request with nothing delivered, rather than
/// leaving some destinations holding the records and some not while the caller is told the
/// batch was accepted. What is left is the narrower guarantee a sequence of writes can give:
/// a destination that REFUSES the object after an earlier one has taken it ends the request
/// too, with the 500 naming which destinations already hold the records — so a caller is
/// never told a partly delivered batch was accepted, and is told how far it got.
async fn deliver(
    ctx: &OperationContext<'_>,
    resolved: &Addressed,
    records: &[Value],
    at: ::std::time::SystemTime,
) -> RtResponse {
    let route = &resolved.route;
    if route.sinks.len() > MAX_SINKS_PER_DELIVERY {
        return worded(
            NOT_IMPLEMENTED_STATUS,
            &format!(
                "This emulator writes at most {MAX_SINKS_PER_DELIVERY} sinks per \
                 sub-pipeline, and {} declares {}.",
                route.sub_pipeline,
                route.sinks.len()
            ),
        );
    }
    let mut writable: Vec<&S3Sink> = Vec::new();
    for sink in &route.sinks {
        match sink {
            PipelineSink::S3(s3) => match &s3.codec {
                crate::state::SinkCodec::Unsupported(named) => {
                    return worded(
                        NOT_IMPLEMENTED_STATUS,
                        &format!(
                            "This emulator does not write the {named} codec the {} sink of \
                             the sub-pipeline {} names.",
                            crate::state::S3_SINK, route.sub_pipeline
                        ),
                    )
                }
                _ => writable.push(s3),
            },
            PipelineSink::UnsupportedKind(named) => {
                return worded(
                    NOT_IMPLEMENTED_STATUS,
                    &format!(
                        "This emulator does not write the {named} sink the sub-pipeline {} \
                         declares.",
                        route.sub_pipeline
                    ),
                )
            }
            PipelineSink::NoDestination(named) => {
                return worded(
                    NOT_DELIVERED_STATUS,
                    &format!(
                        "The {} sink the sub-pipeline {} declares names no destination to \
                         write records to.",
                        named.as_deref().unwrap_or("sink"),
                        route.sub_pipeline
                    ),
                )
            }
        }
    }
    if writable.is_empty() {
        return worded(
            NOT_DELIVERED_STATUS,
            &format!(
                "The sub-pipeline {} declares no sink to write records to.",
                route.sub_pipeline
            ),
        );
    }
    // Which destinations have taken the batch so far, for the answer a later sink's refusal
    // gets: a POST that reached some of a route's sinks and not others is not a success, and
    // the caller is told how far it got.
    let mut held_by: Vec<String> = Vec::new();
    for sink in writable {
        match delivered(ctx, &resolved.region, sink, records, at).await {
            Ok(object) => held_by.push(object.bucket),
            Err(undelivered) => {
                // Nothing holds THIS sink's records, so the POST did not succeed — and the
                // answer says which destinations of the route already took them, so a caller
                // can tell a wholly undelivered batch from a partly delivered one.
                let already = match held_by.is_empty() {
                    true => "no destination of this sub-pipeline holds them".to_string(),
                    false => format!("{} already holds them", held_by.join(", ")),
                };
                return worded(
                    NOT_DELIVERED_STATUS,
                    &format!("{} ({already}).", undelivered.reason()),
                );
            }
        }
    }
    // Accepted: every sink of the route holds the records.
    worded(ACCEPTED_STATUS, "")
}

/// One answer this plane words itself: a status, and the sentence that says why (empty for
/// the accepted batch, which the source answers with no body of its own).
fn worded(code: u16, reason: &str) -> RtResponse {
    let mut answer = RtResponse::new(status(code), SdkBody::from(reason.as_bytes().to_vec()));
    if !reason.is_empty() {
        answer
            .headers_mut()
            .insert("content-type", PLAIN_TEXT.to_string());
    }
    answer
        .headers_mut()
        .insert("content-length", reason.len().to_string());
    answer
}

/// A status this plane answers with. Every call site passes one of the constants above, so
/// the fallback is unreachable.
fn status(code: u16) -> StatusCode {
    StatusCode::try_from(code).unwrap_or_else(|_| {
        StatusCode::try_from(NOT_DELIVERED_STATUS).expect("500 is a status code")
    })
}
