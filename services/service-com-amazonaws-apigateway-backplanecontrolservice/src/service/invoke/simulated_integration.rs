// SPDX-License-Identifier: CC0-1.0

//! How a SIMULATED invocation performs an integration that names an HTTP ENDPOINT.
//!
//! `TestInvokeMethod` "simulates the invocation of a Method in your RestApi", and what it
//! simulates is the REQUEST rather than the integration: the method's integration is
//! performed against the endpoint its `uri` names, so a method whose backend is an HTTP
//! endpoint answers a test invocation with that backend's own response. That is the whole of
//! why the operation exists before a deployment: a method can be exercised end to end with
//! no stage serving it.
//!
//! The integration request is the one the data plane builds ([`onward_request`]) — the same
//! `requestTemplates` rendering, the same `requestParameters` mappings, the same
//! `contentHandling` conversion — built from a request assembled out of the members the
//! caller simulated ([`simulated_request_of`]) rather than off a socket. So a configuration
//! performed for a deployed stage and the same configuration performed for a test invocation
//! address the same endpoint with the same payload, and the stage-variable references in it
//! are resolved from the `stageVariables` the caller supplied before anything reads the
//! configuration ([`with_stage_variables`]).
//!
//! Where that endpoint is a STAGE OF THIS PLANE the request re-enters this plane's own
//! serving, and where it is any other endpoint of this emulator it crosses the IN-PROCESS
//! transport the operation's context holds ([`onward_endpoint`], which dispatches into this
//! process's own router) — the two ways the data plane carries one. NEITHER opens a
//! connection, resolves a name or dials an address: the `uri`'s host selects which endpoint
//! OF THIS EMULATOR answers, and an address this emulator publishes nothing on reaches
//! nothing at all and is reported as the integration this api cannot perform. So a simulated
//! invocation reaches exactly what a request to a deployed stage of the same records reaches,
//! and the chain is bounded the same way: the hop count [`onward_request`] enforces, plus the
//! recursion depth the transport stamps on every request it carries.
//!
//! Nothing here reads or writes this service's records: it is handed the method it is to
//! perform, and the api's own records decide the answer where the request re-enters the
//! plane.

use super::*;

/// How many integrations of this emulator's own endpoints a SIMULATED call has been carried
/// through when its own integration is performed: none — a test invocation is where the chain
/// starts, so the endpoint it reaches is the first hop and the bound
/// [`onward_request`] enforces counts from here.
const SIMULATED_HOP: u8 = 0;

/// The request a simulated invocation is performed AS: the verb, path, query string, headers
/// and body a caller described the invocation with, assembled into the request the
/// integration's own mappings read.
///
/// The members are the ones `TestInvokeMethod` models — `httpMethod`, `pathWithQueryString`,
/// `headers`/`multiValueHeaders` and `body` — read by the one reader of a simulated request
/// ([`simulated_request`]), so what a mapping over `method.request.header.{name}` or
/// `method.request.querystring.{name}` resolves against is what the caller supplied.
///
/// `addressed` is what the caller simulated the request's target as and `resource_path` the
/// path of the resource the addressed method is on: a target the HTTP layer cannot represent
/// at all — one carrying a character no request line may — addresses the request at the
/// resource's own path instead, which is the path the method is reached at, rather than
/// leaving the simulation with no request to perform.
pub(crate) fn simulated_request_of(
    verb: &str,
    addressed: &str,
    resource_path: &str,
    headers: &BTreeMap<String, Vec<String>>,
    payload: Vec<u8>,
) -> RtRequest {
    let mut simulated = RtRequest::new(SdkBody::from(payload));
    if simulated.set_uri(addressed.to_string()).is_err() {
        tracing::warn!(
            addressed = %addressed,
            "the simulated request's path is not one a request can be addressed at, so the \
             integration is performed for the method's own resource path"
        );
        let _ = simulated.set_uri(resource_path.to_string());
    }
    if simulated.set_method(verb).is_err() {
        tracing::warn!(
            verb = %verb,
            "the simulated method's verb is not one a request can be made with, so the \
             integration is performed with the verb the request carries by default"
        );
    }
    for (name, values) in headers {
        for value in values {
            // `append`, not `insert`: a name the caller supplied several values for carries
            // every one of them, as it would off the wire. A value the HTTP layer cannot
            // represent leaves that header off rather than failing the simulation.
            if let Err(refused) = simulated
                .headers_mut()
                .try_append(name.clone(), value.clone())
            {
                tracing::warn!(
                    header = %name,
                    error = %refused,
                    "the simulated request carries a header value this emulator cannot \
                     represent, so the integration is performed without it"
                );
            }
        }
    }
    simulated
}

/// Perform `method`'s integration for the SIMULATED call `call`, including the two types that
/// name an HTTP endpoint — which the api's own records produce no answer for, so they are
/// performed by handing the integration request to the endpoint OF THIS EMULATOR the `uri`
/// names (see the module docs: no connection is opened and no address is dialled).
///
/// `simulated` is the request the caller described ([`simulated_request_of`]); `api_id` is the
/// api the simulated method belongs to, which API Gateway names to the endpoint it hands the
/// request to, and `region` is that api's own, the endpoint an integration ARN naming no
/// Region of its own is called at.
///
/// Every other type is performed exactly as the data plane performs it
/// ([`performed_integration`]): one entry point, so a stored configuration is performed one
/// way whichever side drives it.
pub(crate) async fn performed<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    region: &str,
    api_id: &str,
    method: &MethodEntry,
    call: &MethodRequest,
    simulated: &RtRequest,
) -> Result<Performed, UnservedIntegration> {
    let record = method.integration.as_ref();
    // WHICH of the two types that name an HTTP endpoint this is, which decides what becomes
    // of the answer: a standard integration's is the BACKEND answer its own responses map,
    // and a proxy integration's IS the method response.
    let endpoint_type = record.and_then(http_endpoint);
    let Some(passed_on) =
        record.and_then(|record| onward_request(record, simulated, SIMULATED_HOP, call, api_id))
    else {
        // The integration names no HTTP endpoint at all, so it is performed from the api's
        // own records (or by making the call an AWS integration names).
        return performed_integration(ctx, region, Some(method), call).await;
    };
    let (sent, reached) = match passed_on {
        PassedThrough::Unserved(unserved) => return Err(unserved),
        PassedThrough::Request(onward) => {
            // A stage of THIS plane: the integration request re-enters the plane's own
            // serving, under its own request ids, exactly as the data plane re-enters it for
            // an integration naming a stage. The record of what that request's own
            // authorization and integration did belongs to that request, not to this
            // simulation, so it is kept apart.
            let sent = request_payload(&onward);
            let ids = CallIds::of_request(&onward);
            let mut onward_recorded = stage_logs::Recorded::default();
            let reached = super::serve_resolved(
                engine,
                &onward,
                ctx,
                SIMULATED_HOP + 1,
                &ids,
                &mut onward_recorded,
                // A simulated call arrived over no connection, so it was delivered through
                // no interface VPC endpoint for the reached stage's policy to be decided on.
                None,
            )
            .await;
            (sent, reached)
        }
        PassedThrough::Onward(onward) => {
            // Another endpoint OF THIS EMULATOR: the request crosses the in-process
            // transport `ctx` holds — a dispatch into this process's own router, which opens
            // no socket — and what the service CLAIMING that endpoint answered is the
            // backend answer. An endpoint nothing in this emulator claims is answered by
            // nothing, which `onward_endpoint` reports as the integration this api cannot
            // perform.
            let sent = request_payload(&onward);
            let reached = match endpoint_type {
                Some(HttpEndpoint::Mapped) => {
                    onward_endpoint::backend_answered(ctx, *onward).await
                }
                _ => onward_endpoint::answered(ctx, *onward).await,
            };
            (sent, reached?)
        }
    };
    let received = reached.body().bytes().unwrap_or_default().to_vec();
    let answer = match endpoint_type {
        // A STANDARD `HTTP` integration: what came back is the backend's answer, which the
        // integration's own responses then map to the method response.
        Some(HttpEndpoint::Mapped) => integration_response::backend_answer(&reached),
        // A PROXY integration: what came back IS the method response, mapped by nothing.
        _ => integration_response::served_answer(&reached),
    }
    .ok_or_else(|| UnservedIntegration {
        refusal: API_CONFIGURATION_ERROR,
        reason: "the endpoint the integration's uri names answered a body this api cannot \
                 read, so there is no backend answer to map"
            .to_string(),
    })?;
    Ok(Performed {
        answer,
        configuration_error: None,
        // What the simulation EXCHANGED with that endpoint, which the execution log reports
        // the SIZE of — the integration request as it was carried and the answer as it
        // arrived.
        exchanged: Some(stage_logs::Exchanged { sent, received }),
    })
}
