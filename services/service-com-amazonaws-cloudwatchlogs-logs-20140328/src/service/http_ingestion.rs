// SPDX-License-Identifier: CC0-1.0

//! The seam that SERVES the HTTP ingestion endpoints: how a request to one is claimed, how
//! its bearer credential is authenticated, and how its records reach the one ingestion this
//! service performs.
//!
//! Addressing, body forms and answers are [`crate::state::http_ingestion`]'s; what is here
//! is the request-to-response step, because such a request names no modelled operation at
//! all: it carries a bearer credential instead of a SigV4 signature and addresses a path no
//! operation of this service's model binds, so the front door could resolve no target
//! service for it and answered `404 UnknownService` — for endpoints CloudWatch Logs serves
//! on its own endpoint. The service therefore claims that path itself
//! ([`claims_parts`]), exactly as every other emulated data plane claims its own traffic.
//!
//! WHO a request speaks for is the bearer credential it carries: a CloudWatch Logs API key,
//! which is a long-term service-specific credential IAM issued for
//! [`crate::state::http_ingestion::API_KEY_SERVICE_NAME`]. IAM holds the verifier of every
//! such credential's secret, so the account itself answers whether the presented token is
//! one it issued and still holds `Active` and unexpired, and WHICH IAM user it was created
//! for (`iam::state::…::service_specific_credential_for_secret`) — this service holds no
//! second copy of that credential and invents no token of its own. WHAT that user may then
//! do is its own identity-based policies' answer, taken through the same decision this
//! service takes in front of every modelled operation
//! ([`crate::state::identity_authorization::refusal_for_identity`]): an ingestion is
//! `logs:PutLogEvents` on the log stream the request addressed. A request carrying no bearer
//! credential, one presenting a token the account never issued or no longer holds, one
//! addressed to a log group that has not had bearer token authentication enabled on it
//! (`PutBearerTokenAuthentication`, "when enabled on a log group, bearer token
//! authentication is enabled on operations until it is explicitly disabled"), and one whose
//! user IAM does not allow that action are all forbidden: none of them is a request this
//! endpoint may ingest for, and the answer says only that, never which of the four it was.
//!
//! WHAT it ingests is the one ingestion this service already performs: the records the body
//! carried become one batch of log events handed to `crate::ops::put_log_events`, so the
//! clock-relative ingestion window, the per-event and per-batch size bounds, the group's
//! metric filters, data protection policies, subscription filters and Live Tail sessions all
//! act on a record shipped over HTTP exactly as they act on one uploaded with
//! `PutLogEvents`. The records are handed over in timestamp order: these endpoints state no
//! ordering requirement on a batch, while the modelled operation refuses one out of
//! chronological order, and a stream holds its events in timestamp order either way, so
//! ordering the batch changes nothing a reader can observe.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;
use ::service_common::jwt::BEARER_SCHEME;
use ::smithy_server_runtime::{
    http, BoxFuture, ErasedOperation, Operation, OperationContext, Service,
};
use ::std::sync::Arc;

use crate::state::http_ingestion::{self as ingestion, Endpoint, Record};

/// What a request this endpoint may not ingest for is refused with: `403`, the status a
/// credential that does not authorize a request is refused with, over the one-member
/// envelope these endpoints report a refusal in.
const FORBIDDEN_STATUS: u16 = 403;

/// AWS's own word for a request whose credential does not authorize it at an HTTP front
/// door, which is what a missing, unknown or disabled bearer credential is here.
const FORBIDDEN_MESSAGE: &str = "Forbidden";

/// The operation an ingestion over these endpoints performs, which is the action it is
/// decided against: `logs:PutLogEvents`, the one ingestion this service performs.
const PUT_LOG_EVENTS_OPERATION: &str = "PutLogEvents";

/// The status a batch no record of which could be ingested is refused with: a client error,
/// because the body carried nothing to ingest.
const BAD_REQUEST_STATUS: u16 = 400;

/// The status the scaffold signal travels with, for the one branch that cannot be reached
/// while the ingestion answers a modelled error: "nobody has written it yet" is not a fault
/// of the request.
const NOT_IMPLEMENTED_STATUS: u16 = 501;

/// The status a request addressed to a destination the account does not hold is refused
/// with: there is no log group or log stream of that name to ingest into.
const NOT_FOUND_STATUS: u16 = 404;

/// The status a request the log store could not answer at all is refused with: the service
/// could not complete it, which is its own fault rather than the client's.
const UNAVAILABLE_STATUS: u16 = 503;

/// The status an accepted batch is answered with, whether every record of it was ingested
/// or only some were.
const OK_STATUS: u16 = 200;

/// The method these endpoints are served on: a client SHIPS records to them, so a request
/// of any other method addresses nothing they serve.
const INGESTION_METHOD: &str = "POST";

/// Whether the request these parts came from addresses an HTTP ingestion endpoint.
///
/// The claim is the METHOD and the PATH, which are this service's own: a request to one of
/// them that carries no credential is this service's to refuse, not the front door's to
/// answer `404` for, while a request of another method to the same path is not an ingestion
/// at all and is left to the framework's own routing refusal.
#[must_use]
pub fn claims_parts(parts: &http::request::Parts) -> bool {
    parts.method == INGESTION_METHOD && ingestion::endpoint_at(parts.uri.path()).is_some()
}

/// Whether a runtime-API request addresses an HTTP ingestion endpoint — the same claim as
/// [`claims_parts`], read off the request the other seams are handed.
#[must_use]
pub fn claims_request(req: &RtRequest) -> bool {
    req.method() == INGESTION_METHOD
        && ingestion::endpoint_at(::service_common::request_path(req.uri())).is_some()
}

/// The query string of a request's URI, or `None` when it carries none.
fn query_of(uri: &str) -> Option<&str> {
    uri.split_once('?').map(|(_, query)| query)
}

/// WHERE a request's records go: the destination its HEADERS name, or the one its query
/// string names.
///
/// One rule over every form these endpoints serve: a client that ships a body of its own
/// addresses its destination in the headers CloudWatch Logs reads it from, and one that
/// ships records addresses it in the query string, so both are read and neither form is tied
/// to one of them.
fn addressed_destination(req: &RtRequest) -> Option<ingestion::Destination> {
    ingestion::named_destination(
        req.headers().get(ingestion::LOG_GROUP_HEADER),
        req.headers().get(ingestion::LOG_STREAM_HEADER),
    )
    .or_else(|| ingestion::destination(query_of(req.uri())))
}

/// The bearer token a request presents, or `None` when it presents no bearer credential.
///
/// STRICT, which is why `service_common::jwt::without_bearer_scheme` is not what reads it:
/// that reader takes a value carrying no scheme as the token itself, while a credential
/// presented to these endpoints is a bearer credential or nothing — a request under any
/// other scheme presents no credential this endpoint can authenticate. The scheme itself is
/// compared case-insensitively, as RFC 7235 §2.1 defines it, through that module's own
/// spelling of it.
fn presented_token(req: &RtRequest) -> Option<&str> {
    let credential = req.headers().get("authorization")?.trim();
    let (scheme, token) = credential.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case(BEARER_SCHEME)
        .then(|| token.trim())
        .filter(|token| !token.is_empty())
}

/// The HTTP ingestion endpoints as one [`ErasedOperation`], so the router hands them the
/// whole request: they own the request-to-response step, since no modelled operation of
/// this service serves one.
pub struct HttpIngestion<Eng> {
    /// The SAME shared engine the generated entry threads into every operation, so the
    /// credential this endpoint authenticates and the group it ingests into are read from
    /// the one store the modelled operations write.
    engine: Arc<Eng>,
    /// The one ingestion this service performs, built from that same engine.
    put: crate::ops::put_log_events::PutLogEvents<Eng>,
}

impl<Eng: Send + Sync + 'static> HttpIngestion<Eng> {
    /// Build the endpoints from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            put: crate::ops::put_log_events::PutLogEvents::new(Arc::clone(&engine)),
            engine,
        }
    }

    /// The IAM USER a presented token speaks for: the ARN of the user holding the
    /// CloudWatch Logs API key whose secret is `token`, where the account issued one it
    /// still holds `Active` and unexpired. `None` for a token the account never issued, one
    /// it no longer holds, or one of another service's credentials — none of which
    /// authenticates anything here.
    ///
    /// Asked of IAM's own store, which holds the verifier of every service-specific
    /// credential's secret and the user that holds it: this service keeps no copy of a
    /// credential and cannot mint one. A request on an engine carrying no IAM store presents
    /// a credential nothing issued, so it authenticates nothing.
    fn authenticated_identity(&self, token: &str) -> Option<String> {
        let iam = iam_store::service_state(&*self.engine)?;
        let store = iam.lock();
        // One clock reading per crate: the store counts a credential's expiry in whole
        // seconds, which is this service's own millisecond clock divided down.
        let now_secs = crate::state::now_ms() / 1_000;
        let credential = store.service_specific_credential_for_secret(
            ingestion::API_KEY_SERVICE_NAME,
            token,
            now_secs,
        )?;
        // The principal is the USER the credential was created for, named by the ARN the
        // account holds that user under; a credential whose user the account no longer holds
        // speaks for nobody.
        store
            .users
            .get(&iam_store::name_key(&credential.user_name))
            .map(|user| user.arn.clone())
    }

    /// Whether the log group `log_group` has bearer token authentication enabled, which is
    /// what makes it a destination these endpoints ingest into. `false` for a group this
    /// account does not hold: there is no group to have enabled it.
    fn bearer_token_authentication_enabled(&self, log_group: &str) -> bool {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            return false;
        };
        let store = state.lock();
        store
            .log_groups
            .get(log_group)
            .is_some_and(|group| group.bearer_token_authentication_enabled)
    }

    /// What a request this endpoint REFUSES is answered with, and the audit record of that
    /// refusal: every refusal is recorded the way the served write is, naming what it
    /// addressed and the status it was answered with — and nothing of the credential it
    /// presented.
    fn refused(&self, status: u16, message: &str, log_group: Option<&str>) -> RtResponse {
        ::tracing::info!(
            audit = true,
            operation = PUT_LOG_EVENTS_OPERATION,
            outcome = "refused",
            log_group = log_group.unwrap_or_default(),
            status = status,
            "an ingestion over an HTTP endpoint was refused"
        );
        response(status, &ingestion::message_body(message))
    }

    /// What a request this endpoint may not ingest for is answered with: the answer says
    /// only that the request is forbidden — not which of its credential, its destination or
    /// its grants was the reason.
    fn forbidden(&self, log_group: Option<&str>) -> RtResponse {
        self.refused(FORBIDDEN_STATUS, FORBIDDEN_MESSAGE, log_group)
    }

    /// Serve one request to an HTTP ingestion endpoint.
    async fn serve(
        &self,
        endpoint: Endpoint,
        req: &RtRequest,
        ctx: &OperationContext<'_>,
    ) -> RtResponse {
        // WHO the request speaks for, before anything of its body is read: a request this
        // endpoint may not ingest for is refused whatever it carries, and the refusal says
        // nothing about whether the group it named exists.
        let Some(token) = presented_token(req) else {
            return self.forbidden(None);
        };
        let Some(identity) = self.authenticated_identity(token) else {
            return self.forbidden(None);
        };
        // WHERE the records go: a record is ingested into a log stream of a log group, so a
        // request that names neither in its headers nor in its query string addresses no
        // destination at all — which is a fault of the ADDRESSING rather than of the batch,
        // and is reported as one.
        let Some(destination) = addressed_destination(req) else {
            return self.refused(
                BAD_REQUEST_STATUS,
                &ingestion::no_destination_message(),
                None,
            );
        };
        if !self.bearer_token_authentication_enabled(&destination.log_group) {
            return self.forbidden(Some(&destination.log_group));
        }
        // WHAT that principal may do: the ingestion is `logs:PutLogEvents` on the log
        // stream the request addressed, decided against the IAM user the presented
        // credential speaks for — the same decision this service takes in front of every
        // modelled operation, asked here for a principal a bearer credential named rather
        // than one an access key id resolved.
        let region = crate::state::request_region(ctx.parts());
        let addressed = crate::state::log_stream_arn(
            &region,
            &destination.log_group,
            &destination.log_stream,
        );
        if crate::state::identity_authorization::refusal_for_identity(
            &*self.engine,
            &identity,
            PUT_LOG_EVENTS_OPERATION,
            &addressed,
        )
        .is_some()
        {
            return self.forbidden(Some(&destination.log_group));
        }
        let read = ingestion::read_records(endpoint, req.body().bytes().unwrap_or_default());
        let carried = read.records.len() + read.unreadable;
        // What one batch may hold bounds what one request may carry, so a request over that
        // maximum is refused before anything of it is ingested.
        if carried > ingestion::MAX_RECORDS_PER_REQUEST {
            return self.refused(
                BAD_REQUEST_STATUS,
                &ingestion::too_many_records_message(),
                Some(&destination.log_group),
            );
        }
        // A batch carrying no record at all is refused: there was nothing to ingest.
        if read.records.is_empty() {
            return self.refused(
                BAD_REQUEST_STATUS,
                ingestion::ALL_EVENTS_INVALID_MESSAGE,
                Some(&destination.log_group),
            );
        }
        let arrived_at = crate::state::now_ms();
        let mut events: Vec<crate::ops::types::InputLogEvent> = read
            .records
            .iter()
            .map(|Record { timestamp, message }| crate::ops::types::InputLogEvent {
                // A record that states no instant of its own is the event that arrived
                // now, which is the instant the endpoint received it.
                timestamp: timestamp.unwrap_or(arrived_at),
                message: message.clone(),
            })
            .collect();
        events.sort_by_key(|event| event.timestamp);
        let ingested = self
            .put
            .handle(
                crate::ops::types::PutLogEventsInput {
                    log_group_name: destination.log_group.clone(),
                    log_stream_name: destination.log_stream.clone(),
                    log_events: events,
                    sequence_token: None,
                    entity: None,
                },
                ctx,
            )
            .await;
        let answered = match ingested {
            Ok(output) => accepted(carried, read.records.len(), &output),
            // A refusal of the BATCH is the ingestion's own, reported under the status its
            // CLASS answers with and worded as the modelled operation words it, in the
            // envelope these endpoints report a refusal in.
            Err(refused) => {
                let (status, message) = refusal_answer(&refused);
                response(status, &ingestion::message_body(&message))
            }
        };
        // The audit record of the write this request performed: what it addressed, how many
        // records it carried and the status it was answered with — and nothing of the
        // credential it presented.
        ::tracing::info!(
            audit = true,
            operation = PUT_LOG_EVENTS_OPERATION,
            outcome = "ingested",
            log_group = %destination.log_group,
            log_stream = %destination.log_stream,
            records = carried,
            status = answered.status().as_u16(),
            "an ingestion over an HTTP endpoint was served"
        );
        answered
    }
}

/// The answer to a batch the ingestion accepted: the records it refused, counted from what
/// the ingestion reported back and from the records the body carried no event in.
fn accepted(
    carried: usize,
    handed_over: usize,
    output: &crate::ops::types::PutLogEventsOutput,
) -> RtResponse {
    let refusals =
        ingestion::WindowRefusals::of(handed_over, output.rejected_log_events_info.as_ref());
    // A record the body carried no log event in was never handed over, so it is refused
    // too: the count a partial success reports is of the records the REQUEST carried.
    let rejected = carried.saturating_sub(handed_over) + refusals.total();
    if rejected == 0 {
        return response(OK_STATUS, ingestion::ACCEPTED_BODY);
    }
    if rejected >= carried {
        // Nothing of the batch was ingested after all, so there is no partial success.
        return response(
            BAD_REQUEST_STATUS,
            &ingestion::message_body(ingestion::ALL_EVENTS_INVALID_MESSAGE),
        );
    }
    response(
        OK_STATUS,
        &ingestion::partial_success_body(rejected, &ingestion::window_refusal_document(&refusals)),
    )
}

/// The status and the sentence a modelled refusal of the ingestion is reported with, so the
/// endpoint answers the ingestion's own wording under the status that refusal's CLASS
/// carries rather than one status for every class: a destination the account does not hold
/// is NOT FOUND, a store that cannot answer is the service's own fault, and everything else
/// is a fault of the batch the client sent.
fn refusal_answer(refused: &crate::ops::put_log_events::PutLogEventsError) -> (u16, String) {
    use crate::ops::put_log_events::PutLogEventsErrorModeled as Modeled;
    let modeled = match refused {
        crate::scaffolded::Scaffolded::Modeled(modeled) => modeled,
        // The ingestion answers a modelled error for every refusal it makes today, so
        // these branches are unreachable; each is answered as what it SAYS rather than
        // folded into a fault of the batch, so a signal one of them carries later is not
        // dressed up as one.
        crate::scaffolded::Scaffolded::NotImplemented(signal) => {
            return (NOT_IMPLEMENTED_STATUS, signal.message.clone())
        }
        crate::scaffolded::Scaffolded::ValidationException(refusal) => {
            return (BAD_REQUEST_STATUS, refusal.message.clone())
        }
        crate::scaffolded::Scaffolded::AccessDenied(refusal) => {
            return (FORBIDDEN_STATUS, refusal.message.clone())
        }
    };
    let (status, message) = match modeled {
        Modeled::ResourceNotFoundException(error) => (NOT_FOUND_STATUS, error.message.clone()),
        Modeled::ServiceUnavailableException(error) => (UNAVAILABLE_STATUS, error.message.clone()),
        Modeled::DataAlreadyAcceptedException(error) => (BAD_REQUEST_STATUS, error.message.clone()),
        Modeled::InvalidParameterException(error) => (BAD_REQUEST_STATUS, error.message.clone()),
        Modeled::InvalidSequenceTokenException(error) => {
            (BAD_REQUEST_STATUS, error.message.clone())
        }
        Modeled::UnrecognizedClientException(error) => (FORBIDDEN_STATUS, error.message.clone()),
    };
    (
        status,
        message.unwrap_or_else(|| ingestion::ALL_EVENTS_INVALID_MESSAGE.to_string()),
    )
}

/// One response of these endpoints: a JSON body under the status it is reported with.
fn response(status: u16, body: &str) -> RtResponse {
    let mut response = RtResponse::new(
        ::aws_smithy_runtime_api::http::StatusCode::try_from(status)
            .expect("the status is a constant of this module"),
        ::aws_smithy_types::body::SdkBody::from(body.to_string()),
    );
    response
        .headers_mut()
        .insert("content-type", "application/json");
    response
}

impl<Eng: Send + Sync + 'static> ErasedOperation for HttpIngestion<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            match ingestion::endpoint_at(::service_common::request_path(req.uri())) {
                Some(endpoint) => self.serve(endpoint, req, ctx).await,
                // Unreachable: this operation is named only for a request the endpoints
                // claim, which is a request to one of their paths.
                None => svc.definition().routing_error_response(req),
            }
        })
    }

    /// NO auth scheme, which is what the router reads as "served unsigned": a request to
    /// these endpoints authenticates with a bearer credential rather than with a SigV4
    /// signature, and this endpoint is what verifies it
    /// ([`HttpIngestion::authenticated_identity`]).
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        Some(&[])
    }
}
