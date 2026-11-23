// SPDX-License-Identifier: CC0-1.0

//! The invoke a BROWSER sent from ANOTHER SITE: which ones a deployed stage answers, and how
//! this plane answers them.
//!
//! A deployed stage is a public HTTP endpoint, and a page a browser loaded from another origin
//! is one of its clients: the browser asks the endpoint first (the `OPTIONS` preflight it
//! sends before a cross-origin call), then sends the real request, and decides from the
//! `Access-Control-*` headers the answers carry whether the page may read them. A REST API
//! declares nothing about origins of its own, so the CORS contract of one is entirely the
//! methods the developer deployed and the header values their responses map.
//!
//! The emulator's front door refuses a request whose browser-set headers say a browser sent it
//! from another site, so a page the developer visits cannot drive this emulator, and it applies
//! that rule to every request it NAMES AN OPERATION for — after the operation is named and
//! before it is run, with no seam a service can admit one through. The data plane is named as
//! an operation ([`crate::service`]) because an integration is a call into another service and
//! only the router's context carries the transport for it, so EVERY cross-origin invoke was
//! refused `403`, the preflight included, which leaves a browser unable to make any
//! cross-origin call to an emulated API.
//!
//! API Gateway itself reads the `Origin` header of an invoke for NOTHING: a deployed stage
//! answers the preflight from its `OPTIONS` method and the real request from its own method,
//! integration and header mappings, and leaves to the browser what the `Access-Control-*`
//! headers mean. So what this module decides is not whether the stage would answer — it always
//! would — but HOW FAR the emulator's front door may be bypassed for this request, and that is
//! the one thing it reads the records for ([`admission`]). The reading is the resource's own
//! `Access-Control-Allow-Origin` mappings, which its developer configured through the CONTROL
//! plane that no browser reaches, and they are read per RESOURCE rather than per method because
//! that is where AWS's own "Enabling CORS for a REST API resource" flow puts them — on the
//! `OPTIONS` method it adds, while the method the real request reaches is commonly a proxy that
//! carries those headers in the backend's own answer instead. Three outcomes follow:
//!
//! * a declaration naming THE REQUEST'S OWN SITE is the api owner saying whose pages this
//!   resource's answers are for, so the request is served exactly as the same request from any
//!   other client is: the same resolution, the same authorization, the same method, integration,
//!   templates and header mappings, with the `Origin` forwarded to the integration like every
//!   other header and deciding nothing about the answer. It is the same statement a function
//!   URL's own address reads off its `Cors$AllowOrigins` — and that address runs the function
//!   for a page it names. The seam that answers it is handed no request context, so the
//!   transport its integration is performed over is the address this emulator answers ITSELF
//!   on, which the DEPLOYMENT the addressed stage serves carries
//!   ([`crate::state::DeploymentEntry::reached_emulator`]): a deployment is what makes a stage
//!   invocable at all, so the address its invokes are performed over is recorded with it;
//! * a declaration naming EVERY origin names no site, so it is no page in particular that the
//!   api's owner admitted. It admits only what this seam has always admitted: a method whose
//!   answer needs no call outside this service — a `MOCK`, the integration a CORS method is
//!   wired to, or no integration at all — served over a transport that REACHES NOTHING, so even
//!   a step that would reach for one is withheld. A page from such a site can therefore drive
//!   nothing of this emulator beyond the records of the api that admitted it, and the same is
//!   what a page from a site the declaration does not name gets: the endpoint answers every
//!   preflight of a CORS-configured resource the static mapping it declares, whoever asked, and
//!   the browser is what refuses a page the value does not name. A resource whose methods
//!   declare no origin at all is reached by no page;
//! * a request NO deployed method serves is admitted too, because what answers it is the stage's
//!   own refusal, which performs nothing — and the endpoint's recorded behaviour is that it
//!   answers such a preflight (`403` `Missing Authentication Token`), which is how a browser
//!   tells a resource that declines a preflight from one that answers it.
//!
//! And one outcome the records do not decide at all: a request whose own CREDENTIAL the stage
//! will AUTHENTICATE before it performs anything
//! ([`will_be_authenticated_before_anything_is_performed`]) is served with its integration
//! performed whatever the declaration says, because `Origin` is no authorization input of a
//! method — the recorded behaviour of this endpoint is that a SIGNED call is answered by the
//! integration whichever site the page that sent it was loaded from. Signing needs this
//! emulator's own secret, which a document a browser loaded from a site does not have, so what
//! this admits is a client holding those credentials and never a page: the front door's purpose
//! is kept by the thing that tells the two apart.
//!
//! Everything else — a page reaching a resource whose methods declare no origin at all, and a
//! page admitted only by a wildcard (or by nothing naming its own site) to a method whose
//! integration names a backend — is left to the front door,
//! which refuses it exactly as it did before this seam existed.
//!
//! The admission is the API's own, not the operator's, because the operator has no say to
//! consult: the front door's check is fixed at same-origin-only, the emulator's command line
//! parses no cross-origin option, and nothing in this tree resolves one. Where a browser-facing
//! data plane of this emulator does admit a page, it is the RESOURCE's own declaration that
//! admits it (a function URL's `Cors$AllowOrigins`, over the same core reading of a browser's
//! headers, invoking the function behind it over the handle the create that published the
//! address was serving under), and that is the rule this follows.

use std::sync::Arc;

use ::smithy_server_runtime::ServiceIdentity;

use super::*;

/// The response header whose mapping is a resource's statement that its answers are for pages:
/// `Access-Control-Allow-Origin`, the header a browser reads a cross-origin answer's admission
/// from.
const ALLOW_ORIGIN_HEADER: &str = "Access-Control-Allow-Origin";

/// The `Access-Control-Allow-Origin` value that names every origin.
const EVERY_ORIGIN: &str = "*";

/// The `Origin` a document with no site of its own sends — a sandboxed frame, a `file://` page,
/// some redirects. It names no site, so no declaration names it.
const OPAQUE_ORIGIN: &str = "null";

/// Whether this plane answers `req` from the seam that runs ahead of the front door's
/// cross-site refusal: a request a browser sent from another site, addressed to a deployed
/// stage this plane serves, that the api's own deployed records ADMIT ([`admission`]).
///
/// Asked by BOTH seams in [`crate::service`] — the one that leaves such a request unnamed and
/// the one that answers it — so the request the front door never refuses is exactly the request
/// this plane serves, and neither seam can drift from the other.
pub fn claims_a_cross_origin_browser_request<Eng: Send + Sync + 'static>(
    engine: &Eng,
    req: &RtRequest,
) -> bool {
    refused_by_the_front_door(req)
        && claims_request(engine, req)
        && admission(engine, req).is_some()
}

/// Whether the FRONT DOOR refuses this request before a named operation of this service is
/// reached, because its browser-set headers say a browser sent it from another site.
///
/// The router applies that check to every request it NAMES AN OPERATION for, and to no request
/// it does not, with the same reading this calls
/// (`emulator_server::classify_browser_request` over same-origin-only). So the request this
/// answers `true` for is exactly the one that reaches no operation of this service at all.
fn refused_by_the_front_door(req: &RtRequest) -> bool {
    let header = |name: &str| req.headers().get(name);
    matches!(
        ::emulator_server::classify_browser_request(
            header("origin"),
            header("sec-fetch-site"),
            &::emulator_server::RemoteOrigins::SameOriginOnly,
            false,
        ),
        ::emulator_server::BrowserVerdict::Forbid(_)
    )
}

/// What the api's own DEPLOYED records admit for a request the front door would refuse as
/// another site's — WHICH of the two ways below it may be served, and `None` for a request
/// this plane does not answer at all, which the front door then refuses exactly as it did
/// before this seam existed.
#[derive(Debug, Clone)]
enum Admission {
    /// From the api's own records ALONE, over a transport that reaches nothing
    /// ([`super::reached_emulator::contained`]): the stage's own refusal of a request no
    /// deployed method serves, and a method whose answer needs no call outside this service.
    /// This is the whole of what this seam has ever done, and it is what a declaration naming
    /// EVERY origin admits — the confinement, not the fidelity, is what that distinction is
    /// about.
    FromTheRecordsAlone,
    /// With the matched method's own INTEGRATION PERFORMED, over the address this emulator
    /// answers ITSELF on, which the DEPLOYMENT the stage serves carries
    /// ([`crate::state::DeploymentEntry::reached_emulator`]) — the transport a named invoke of
    /// the same stage performs its integration over. Admitted only where a method of the
    /// resource declares the request's origin ITSELF: the api owner's statement, made through
    /// the control plane, that pages from THAT site are what this resource's answers are for.
    ///
    /// `None` where that deployment carries no such address, which leaves the request served
    /// over the contained transport, as [`Admission::FromTheRecordsAlone`] is.
    WithTheIntegrationPerformed(Option<::smithy_server_runtime::EmbeddedEmulator>),
}

/// What the api's own DEPLOYED records admit for `req`.
///
/// The records read are the ones that answer the request — the api this endpoint resolves to,
/// the deployment its stage serves, the resource path that matches most specifically and the
/// method that serves the verb — through this plane's own resolution, so the resource this asks
/// about is the resource that then answers. What decides it is the `Access-Control-Allow-Origin`
/// mapping its deployed methods SERVE ([`declared_for`]), which is a statement made through the
/// CONTROL plane that no browser reaches:
///
/// * a declaration naming the request's origin ITSELF is the api owner saying whose pages this
///   resource's answers are for — the same question a function URL's own address asks of its
///   `Cors$AllowOrigins`, and that address runs the function for a page it names — so the
///   method answers it as it answers anybody, with its integration performed over the address
///   the deployment carries;
/// * a declaration naming EVERY origin names no site, and so does one naming only OTHER sites
///   than this request's: it is nobody's page in particular, so it admits only what this seam
///   has always admitted, a method whose answer needs no call
///   outside this service ([`performs_no_onward_call`]), served over a transport that reaches
///   nothing — which is what the endpoint answers every preflight of a CORS-configured resource
///   with, the static mapping it declares whoever asked. A page reaching a resource whose
///   methods declare NO origin at all ([`declares_a_contract`]) is not admitted;
/// * a request whose own CREDENTIAL the stage will AUTHENTICATE before it performs anything
///   ([`will_be_authenticated_before_anything_is_performed`]) is admitted with its integration
///   performed whatever the declaration says, because what answers such a call is a client
///   holding this emulator's own credentials rather than a page, and `Origin` is no
///   authorization input of a method.
///
/// A request NO deployed method serves is admitted whatever the api declares, because what
/// answers it is the stage's own refusal — nothing is performed for it, and the endpoint's
/// recorded behaviour is that it answers such a preflight (`403` `Missing Authentication
/// Token`), which is how a browser tells a resource that declines a preflight from one that
/// answers it.
fn admission<Eng: Send + Sync + 'static>(engine: &Eng, req: &RtRequest) -> Option<Admission> {
    let state = crate::state::service_state(engine)?;
    let store = state.lock();
    let resolved = resolve(&store, &Addressed::of_request(req))?;
    let Some(served) = served_by(&resolved, req) else {
        // No deployed method serves it: what answers it is the stage's own refusal.
        return Some(Admission::FromTheRecordsAlone);
    };
    // The stage will AUTHENTICATE this request's own credential before it performs anything for
    // it, so what is answering is a client holding this emulator's own credentials rather than a
    // page: `Origin` is no authorization input of a method, and the recorded behaviour of this
    // endpoint is that such a call is answered by the integration whichever site it names.
    if will_be_authenticated_before_anything_is_performed(&served, req) {
        return Some(Admission::WithTheIntegrationPerformed(
            served.deployment.reached_emulator.clone(),
        ));
    }
    match declared_for(served.resource, req) {
        Some(Declaration::TheOriginItself) => Some(Admission::WithTheIntegrationPerformed(
            served.deployment.reached_emulator.clone(),
        )),
        // A declaration naming EVERY origin names no site, and so does one naming only OTHER
        // sites than this request's: either way the api's owner named no page in particular, so
        // all that is admitted is what the records answer by themselves — which is what the
        // endpoint answers every preflight of a CORS-configured resource with, the static
        // mapping whoever asked, read over a transport that reaches nothing. A resource whose
        // methods declare no origin at all ([`declares_a_contract`]) is reached by no page.
        Some(Declaration::EveryOrigin) | None => (declares_a_contract(served.resource)
            && performs_no_onward_call(served.method))
        .then_some(Admission::FromTheRecordsAlone),
    }
}

/// Whether the records of `resource` declare a CORS contract at all: one of its deployed methods
/// SERVES a static `Access-Control-Allow-Origin` literal, whichever origin that literal names.
///
/// That is the statement that makes the resource a browser-facing one — its developer configured
/// it for pages, through the control plane — and it is what the api's own records answering a
/// request from a site they do not name rests on: the value a preflight is answered with is the
/// declared one whoever asked, exactly as the mapping says, and the browser is what refuses a
/// page it does not name. A resource no record of which declares one is reached by no page.
fn declares_a_contract(resource: &ResourceEntry) -> bool {
    !declared_allow_origins(resource).is_empty()
}

/// Whether the stage will AUTHENTICATE the credential `req` carries before it performs anything
/// for it, which is what makes a request from ANY site one this seam serves in full.
///
/// It verifies nothing itself: it answers whether the SERVING will, which is the question the
/// admission has to put before the serving starts.
///
/// Three things have to hold. The method's `authorizationType` must be `AWS_IAM`, the one that
/// authenticates a caller at all; the request must CARRY a SigV4 credential
/// ([`super::carries_sigv4_credential`]), since a method that requires one refuses a request
/// with none before anything else; and this emulator must ACCEPT access keys, read off the very
/// handle the serving would be given ([`crate::state::accepts_access_keys`], the same reading
/// [`super::sigv4_authentication`] makes of the handle a named invoke's context carries). Where
/// all three hold, the serving verifies the signature against the keys this emulator published
/// and refuses a request not signed with them `403` with no integration performed.
///
/// So what this admits is a client holding this emulator's own credentials, whatever origin its
/// request names — which is the request AWS serves, the `Origin` being no authorization input of
/// a method — and what it does not admit is a page: a document a browser loaded from a site
/// cannot sign a request with those credentials, because it does not have them. Where this
/// emulator accepts no access key the serving verifies nothing, so it could not tell the two
/// apart, and this answers `false` for every request rather than admitting both.
fn will_be_authenticated_before_anything_is_performed(
    served: &ServedBy<'_>,
    req: &RtRequest,
) -> bool {
    served.method.authorization_type == AUTHORIZATION_AWS_IAM
        && carries_sigv4_credential(req)
        && served
            .deployment
            .reached_emulator
            .as_ref()
            .is_some_and(crate::state::accepts_access_keys)
}

/// What a deployed stage of the resolved api would serve `req` from.
struct ServedBy<'s> {
    /// The deployment the addressed stage points at: the snapshot the request is matched
    /// against, and what carries the address this emulator answers itself on.
    deployment: &'s DeploymentEntry,
    /// The resource of it whose path the request matched most specifically.
    resource: &'s ResourceEntry,
    /// The method of that resource that serves the request's verb.
    method: &'s MethodEntry,
}

/// What would serve `req` of the api `resolved` names — or `None` when the request names no
/// deployed stage, no matching resource path or no method for its verb, each of which the stage
/// answers with a refusal of its own rather than with a method's response.
fn served_by<'s>(resolved: &Resolved<'s, '_>, req: &RtRequest) -> Option<ServedBy<'s>> {
    let (stage_name, resource_path) = split_stage(resolved.invoke_path)?;
    let deployment = resolved
        .api
        .stages
        .get(stage_name)
        .and_then(|stage| resolved.api.deployments.get(&stage.deployment_id))?;
    let (matched_path, (verb, _)) = routed_method(deployment, &resource_path, req.method())?;
    let resource = deployment
        .resources
        .values()
        .find(|resource| resource.path == matched_path)?;
    resource.methods.get(verb).map(|method| ServedBy {
        deployment,
        resource,
        method,
    })
}

/// WHAT a resource's served `Access-Control-Allow-Origin` mappings say about the origin a
/// request was sent from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Declaration {
    /// One of them names that exact origin — by the whole scheme, host and port, as an origin
    /// compares.
    TheOriginItself,
    /// None names it, and one names EVERY origin, which is a site nobody in particular.
    EveryOrigin,
}

/// What `resource` declares about the origin `req` was sent from: one of its served
/// `Access-Control-Allow-Origin` mappings ([`declared_allow_origins`]) naming that exact
/// origin, else one naming every origin, else `None`.
///
/// A request carrying no `Origin` names no origin to declare, and the opaque origin a sandboxed
/// document sends ([`OPAQUE_ORIGIN`]) is no site's, so neither is admitted by a declaration:
/// what a wildcard admits is a page loaded from some site.
fn declared_for(resource: &ResourceEntry, req: &RtRequest) -> Option<Declaration> {
    let origin = req.headers().get(::http::header::ORIGIN.as_str())?;
    if origin == OPAQUE_ORIGIN {
        return None;
    }
    let declared = declared_allow_origins(resource);
    if declared.contains(&origin) {
        return Some(Declaration::TheOriginItself);
    }
    declared
        .contains(&EVERY_ORIGIN)
        .then_some(Declaration::EveryOrigin)
}

/// The origins `resource` declares its answers are for: the static literal of every
/// `Access-Control-Allow-Origin` response-header mapping ANY of its deployed methods serves.
///
/// Every method of the resource, because that is where AWS's own flow for enabling CORS puts
/// the declaration: the `OPTIONS` method it adds is what carries the resource's
/// `Access-Control-Allow-Origin` mapping, while the method the real request reaches is
/// commonly a proxy whose own answer carries those headers and whose integration therefore
/// declares no mapping at all. So the contract a browser is admitted on is the resource's,
/// which is the unit AWS configures it on.
fn declared_allow_origins(resource: &ResourceEntry) -> Vec<&str> {
    resource
        .methods
        .values()
        .flat_map(method_allow_origins)
        .collect()
}

/// The origins ONE method declares: the static literal of every
/// `Access-Control-Allow-Origin` response-header mapping it SERVES.
///
/// A header is served only where the integration response maps it AND the method response of
/// the same status declares it, which is the pair [`super::mapped_response_headers`] sends a
/// mapped header on — so the declaration read here is the one a caller would be answered with,
/// never a mapping that reaches no response. The literal is read through this service's one
/// reading of a static mapping (`static_mapping_literal`); a mapping that names a value of the
/// call instead of a literal declares no origin, because what it resolves to is not knowable
/// before the call is served.
fn method_allow_origins(method: &MethodEntry) -> Vec<&str> {
    let header = ALLOW_ORIGIN_HEADER;
    let mut declared = Vec::new();
    let Some(integration) = method.integration.as_ref() else {
        return declared;
    };
    for (status, response) in integration.integration_responses.iter().flatten() {
        let Some(mapped) = response.response_parameters.as_ref() else {
            continue;
        };
        let by_the_method = method
            .method_responses
            .get(status)
            .and_then(|response| response.response_parameters.as_ref());
        for (key, value) in mapped {
            let Some(name) = key.strip_prefix(METHOD_RESPONSE_HEADER_PREFIX) else {
                continue;
            };
            if !name.eq_ignore_ascii_case(header) {
                continue;
            }
            if by_the_method.is_none_or(|declared| !declared.contains_key(key)) {
                continue;
            }
            if let Some(literal) = static_mapping_literal(value) {
                declared.push(literal);
            }
        }
    }
    declared
}

/// Whether serving `method` reaches nothing outside this service: a `MOCK` integration, whose
/// answer is the api's own records (the request template that selects a status, the integration
/// response that maps it and its template), or no integration at all, which the stage answers
/// with the configuration-error class without performing anything.
///
/// This is what [`Admission::FromTheRecordsAlone`] is confined to: a page admitted by a
/// declaration that names no site in particular can reach nothing of this emulator beyond the
/// records of the api whose own mapping admitted it.
fn performs_no_onward_call(method: &MethodEntry) -> bool {
    match method.integration.as_ref() {
        None => true,
        Some(integration) => integration.r#type.as_deref() == Some(MOCK_INTEGRATION_TYPE),
    }
}

/// [`super::serve`] from the seam that answers a cross-origin browser request, which carries NO
/// request context: every step is the same — the resolution, the authorization, the matched
/// method, its integration response, its template and its header mappings — and the one
/// difference is where the transport comes from, since no context handed one in
/// ([`serving_transport`]).
///
/// `identity` is this service's own vended identity, which the context the invoke is served
/// under names — the service's, not the request's, so it is the same identity a named invoke's
/// context carries.
///
/// An invoke this seam cannot serve at all — one the records admit nothing for, one on a runtime
/// that cannot drive it, or one arriving while it is already serving as many as it serves at
/// once — is answered as a method whose configuration could not serve the call, with the reason
/// logged.
pub fn serve_without_transport<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
    identity: &ServiceIdentity,
    req: &RtRequest,
) -> RtResponse {
    announce_once();
    // WHICH of the two ways the records admit this request, asked again here rather than
    // carried over from the seam that left it unnamed: the transport it is served over is the
    // one that admission allows and nothing wider. A request the records admit nothing for
    // reached this seam only through a store that changed in between, and is served nothing.
    let Some(admission) = admission(&**engine, req) else {
        tracing::warn!(
            "the records of this api admit nothing for this cross-origin invoke, so it was not \
             served"
        );
        return unservable();
    };
    let (Some(head), Some(served_request)) = (request_head(req), req.try_clone()) else {
        tracing::warn!("this cross-origin invoke could not be rendered for the seam serving it");
        return unservable();
    };
    // The runtime is resolved BEFORE anything is served, because the whole serving is one
    // future: there is no half of it this seam can drive on a runtime that refuses the wait.
    let Some(handle) = drivable_runtime() else {
        tracing::warn!(
            "this cross-origin invoke reached no runtime that can drive it while this seam \
             waits for it"
        );
        return unservable();
    };
    let Some(permit) = InvokePermit::claim() else {
        tracing::warn!(
            in_flight = MAX_INVOKES_IN_FLIGHT,
            "as many cross-origin invokes are in flight from this seam as it serves at once, \
             so this one was not served"
        );
        return unservable();
    };
    // Owned, so the work below is a future of its own that the runtime can drive: the engine
    // handle, the identity, the request and its head move into it, and the context is built
    // inside it from them.
    let owned_engine = Arc::clone(engine);
    let owned_identity = identity.clone();
    let transport = serving_transport(admission);
    let served = awaited(&handle, async move {
        let ctx = OperationContext::new(&head, &owned_identity, transport);
        super::serve(&*owned_engine, &served_request, &ctx).await
    });
    drop(permit);
    match served {
        Some(response) => *response,
        None => {
            // The task serving the invoke panicked or was cancelled, which is a request that
            // produced no response. The reason stays in this emulator's own log.
            tracing::warn!("the task serving this cross-origin invoke produced no answer");
            unservable()
        }
    }
}

/// The transport a cross-origin browser request is served over, which is exactly what its
/// [`Admission`] allows — see [`super::reached_emulator`] for what each handle is:
///
/// * [`Admission::FromTheRecordsAlone`] — the handle that reaches nothing at all, so the
///   answer is the api's own records and nothing beyond them;
/// * [`Admission::WithTheIntegrationPerformed`] — the address this emulator answers ITSELF on,
///   which the DEPLOYMENT the addressed stage serves carries.
fn serving_transport(admission: Admission) -> ::smithy_server_runtime::EmbeddedEmulator {
    match admission {
        Admission::FromTheRecordsAlone => super::reached_emulator::contained(),
        Admission::WithTheIntegrationPerformed(reached) => {
            super::reached_emulator::reached_or_contained(reached)
        }
    }
}

/// Say ONCE, the first time this seam serves a request, that a browser from another site
/// reached a deployed stage of this emulator and what such a request is served on: a developer
/// whose api declares a CORS contract learns that pages are being answered from it, and learns
/// it at a level they see rather than from a missing refusal.
///
/// Once per process, because the sentence is about the standing arrangement rather than about
/// one request, and a per-request line on a data plane a browser drives would bury it.
fn announce_once() {
    static ANNOUNCED: ::std::sync::OnceLock<()> = ::std::sync::OnceLock::new();
    ANNOUNCED.get_or_init(|| {
        tracing::warn!(
            "a browser is calling a deployed stage of this emulator from another site; such a \
             request is served only where a deployed method of the resource it addresses \
             declares that origin or the stage authenticates the credential it carries, it \
             reaches this api's own integration only where a method of that resource declares \
             THAT SITE itself or that credential authenticates, and a declaration naming every \
             origin reaches nothing outside this service"
        );
    });
}

/// What this seam answers an invoke it could not serve at all: the class API Gateway reports for
/// a method whose configuration cannot serve the call, `500`
/// `{"message":"Internal server error"}` — rendered through the one funnel every refusal of this
/// plane is rendered through, with no api's own gateway responses read, because the resolution
/// that would name the api never ran.
fn unservable() -> RtResponse {
    refusal_response(None, API_CONFIGURATION_ERROR)
}

/// The head of the request being served, as the [`OperationContext`] it is served under reads
/// it: the method, the URI and every header the client sent.
///
/// `None` for a head the `http` crate will not build, which no request that reached this point
/// has — it arrived over that same crate's types.
fn request_head(req: &RtRequest) -> Option<http::request::Parts> {
    let mut builder = http::Request::builder()
        .method(req.method())
        .uri(req.uri());
    for (name, value) in req.headers().iter() {
        builder = builder.header(name, value);
    }
    Some(builder.body(()).ok()?.into_parts().0)
}

/// The runtime an invoke can be served on from a SYNCHRONOUS seam, or `None` when this process
/// is not on one.
///
/// It is the MULTI-THREAD runtime the emulator boots. A current-thread runtime is driven by the
/// one thread now inside the seam, which is therefore driving nothing while it waits: a task
/// spawned on it could not be polled and its timers would not advance, so the invoke is not
/// served from there at all rather than waited on. This is also what keeps [`awaited`]'s
/// `block_in_place` from the one runtime that refuses it.
fn drivable_runtime() -> Option<::tokio::runtime::Handle> {
    let handle = ::tokio::runtime::Handle::try_current().ok()?;
    (handle.runtime_flavor() == ::tokio::runtime::RuntimeFlavor::MultiThread).then_some(handle)
}

/// Run `work` to completion on `handle` from a SYNCHRONOUS seam, or `None` for work that
/// produced no answer because the task performing it panicked or was cancelled.
///
/// Serving an invoke is asynchronous and the seam that asks for it is not, so the work is handed
/// to the runtime as a TASK rather than driven on the thread that is waiting for it. That is
/// what lets the deadlines inside it fire while this seam waits: the integration's own
/// `timeoutInMillis` is on the runtime's own timer, and the task stays cancellable.
/// `block_in_place` hands this worker's queue to a replacement thread while it waits, so the
/// runtime keeps serving every other request.
fn awaited(
    handle: &::tokio::runtime::Handle,
    work: impl ::std::future::Future<Output = RtResponse> + Send + 'static,
) -> Option<Box<RtResponse>> {
    let task = handle.spawn(work);
    ::tokio::task::block_in_place(|| handle.block_on(task))
        .ok()
        .map(Box::new)
}

/// How many cross-origin invokes this seam serves AT ONCE, across the whole process.
///
/// Each one holds a worker of the runtime for as long as it takes to serve (the wait in
/// [`awaited`]), so this is what keeps a page that sends many of them from taking the runtime's
/// workers: an invoke that finds no permit is answered rather than queued. The count is the
/// router's own bound on a chain of in-process calls, so this seam holds no more of the runtime
/// at once than the framework already allows a request to.
const MAX_INVOKES_IN_FLIGHT: usize = ::smithy_server_runtime::CALLBACK_RECURSION_LIMIT as usize;

/// How many cross-origin invokes this seam currently has in flight.
static INVOKES_IN_FLIGHT: ::std::sync::atomic::AtomicUsize =
    ::std::sync::atomic::AtomicUsize::new(0);

/// One of the [`MAX_INVOKES_IN_FLIGHT`] invokes this seam serves at once, given back when it is
/// dropped — on every path, including one a served invoke panicked on.
struct InvokePermit;

impl InvokePermit {
    /// One permit, or `None` when this seam already has as many invokes in flight as it serves
    /// at once.
    fn claim() -> Option<Self> {
        let mut held = INVOKES_IN_FLIGHT.load(::std::sync::atomic::Ordering::Relaxed);
        loop {
            if held >= MAX_INVOKES_IN_FLIGHT {
                return None;
            }
            match INVOKES_IN_FLIGHT.compare_exchange_weak(
                held,
                held.saturating_add(1),
                ::std::sync::atomic::Ordering::AcqRel,
                ::std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Self),
                Err(current) => held = current,
            }
        }
    }
}

impl Drop for InvokePermit {
    fn drop(&mut self) {
        INVOKES_IN_FLIGHT.fetch_sub(1, ::std::sync::atomic::Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the table below: the case, the browser-set headers the request carries, and
    /// whether the front door refuses it.
    type Row = (
        &'static str,
        &'static [(&'static str, &'static str)],
        bool,
    );

    /// RULE: the front door's reading of a request's browser-set headers is what decides
    /// whether this plane can answer the invoke from the routing seam at all, and this module
    /// asks it the same question the router does.
    ///
    /// The rows span what that reading generalises over: a client that sets neither header (an
    /// SDK, a CLI, this emulator calling itself), a browser that classified its own request as
    /// same-origin or as no site's, and the three ways a request reads as another site's — a
    /// cross-site classification, a same-site one, and an `Origin` with no classification
    /// beside it, which is what an older browser's preflight carries.
    #[test]
    fn an_invoke_a_browser_sent_from_another_site_is_the_one_left_unnamed() {
        let cases: [Row; 7] = [
            ("a client that sets neither header", &[], false),
            (
                "a browser's own same-origin classification",
                &[
                    ("origin", "https://app.example"),
                    ("sec-fetch-site", "same-origin"),
                ],
                false,
            ),
            (
                "a browser's own no-site classification",
                &[("sec-fetch-site", "none")],
                false,
            ),
            (
                "a browser's own cross-site classification",
                &[
                    ("origin", "https://app.example"),
                    ("sec-fetch-site", "cross-site"),
                ],
                true,
            ),
            (
                "a browser's own same-site classification",
                &[
                    ("origin", "https://app.example"),
                    ("sec-fetch-site", "same-site"),
                ],
                true,
            ),
            (
                "an origin with no classification beside it",
                &[("origin", "https://app.example")],
                true,
            ),
            ("an opaque origin", &[("origin", "null")], true),
        ];
        for (case, headers, expected) in cases {
            let actual = refused_by_the_front_door(&request(headers));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the front door's refusal to be {expected}, got {actual}"
            );
        }
    }

    /// One row of the declaration table below: the case, the mapping the method's one
    /// integration response carries (its key, its value, and whether the method response of the
    /// same status declares that key) or none at all, the headers the request carries, and WHAT
    /// the resource then declares about that request's origin.
    type DeclarationRow = (
        &'static str,
        Option<(&'static str, &'static str, bool)>,
        &'static [(&'static str, &'static str)],
        Option<Declaration>,
    );

    /// RULE: what a resource declares about the origin a request was sent from is read off the
    /// `Access-Control-Allow-Origin` mappings its deployed methods SERVE — that exact origin
    /// named is the api owner's statement about THAT site, the wildcard names no site in
    /// particular, and anything else declares nothing.
    ///
    /// The rows span what that reading generalises over: one site named, the wildcard, a mapping
    /// the method response does not declare (so nothing is served and nothing is declared), a
    /// mapping that names a value of the call rather than a literal, another header's mapping, a
    /// method carrying no integration, and the header name spelled in another case (one header,
    /// by HTTP's own comparison).
    #[test]
    fn a_resource_declares_what_its_served_mappings_name() {
        const ALLOW_ORIGIN: &str = "method.response.header.Access-Control-Allow-Origin";
        let cases: [DeclarationRow; 9] = [
            (
                "one site named",
                Some((ALLOW_ORIGIN, "'https://app.example'", true)),
                &[("origin", "https://app.example")],
                Some(Declaration::TheOriginItself),
            ),
            (
                "the wildcard",
                Some((ALLOW_ORIGIN, "'*'", true)),
                &[("origin", "https://app.example")],
                Some(Declaration::EveryOrigin),
            ),
            (
                "another origin declared",
                Some((ALLOW_ORIGIN, "'https://other.example'", true)),
                &[("origin", "https://app.example")],
                None,
            ),
            (
                "a mapping the method response does not declare",
                Some((ALLOW_ORIGIN, "'https://app.example'", false)),
                &[("origin", "https://app.example")],
                None,
            ),
            (
                "a mapping that names a value of the call",
                Some((ALLOW_ORIGIN, "context.requestId", true)),
                &[("origin", "https://app.example")],
                None,
            ),
            (
                "another header's mapping",
                Some((
                    "method.response.header.Access-Control-Allow-Methods",
                    "'GET,OPTIONS'",
                    true,
                )),
                &[("origin", "https://app.example")],
                None,
            ),
            (
                "a method carrying no integration",
                None,
                &[("origin", "https://app.example")],
                None,
            ),
            (
                "the header name in another case",
                Some((
                    "method.response.header.access-control-allow-origin",
                    "'https://app.example'",
                    true,
                )),
                &[("origin", "https://app.example")],
                Some(Declaration::TheOriginItself),
            ),
            (
                "the opaque origin against the wildcard",
                Some((ALLOW_ORIGIN, "'*'", true)),
                &[("origin", "null")],
                None,
            ),
        ];
        for (case, mapping, headers, expected) in cases {
            let resource = resource_with(&[("GET", method_with(mapping))]);
            let actual = declared_for(&resource, &request(headers));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the resource to declare {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a method's answer needs no call outside this service exactly where its integration
    /// is a `MOCK` or it carries no integration at all — which is what a page admitted only by a
    /// declaration naming every origin is confined to.
    ///
    /// The rows span every integration type this service's control plane accepts (`MOCK`, the
    /// two `crate::ops::put_integration::AWS_INTEGRATION_TYPES` and the two
    /// `HTTP_INTEGRATION_TYPES`), beside a method carrying no integration and a type this
    /// emulator knows nothing about.
    #[test]
    fn a_mock_method_and_a_method_with_no_integration_reach_nothing_outside_this_service() {
        let cases: [(&str, Option<&str>, bool); 7] = [
            ("a method carrying no integration", None, true),
            ("a MOCK integration", Some("MOCK"), true),
            ("an AWS integration", Some("AWS"), false),
            ("an AWS_PROXY integration", Some("AWS_PROXY"), false),
            ("an HTTP integration", Some("HTTP"), false),
            ("an HTTP_PROXY integration", Some("HTTP_PROXY"), false),
            ("an integration type this emulator does not know", Some("OTHER"), false),
        ];
        for (case, integration, expected) in cases {
            let method = MethodEntry {
                integration: integration.map(|kind| GetIntegrationOutput {
                    r#type: Some(kind.to_string()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let actual = performs_no_onward_call(&method);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the method to reach nothing outside this service to be \
                 {expected}, got {actual}"
            );
        }
    }

    /// One row of the resource table below: the case, the mapping the resource's `OPTIONS`
    /// method carries, the mapping the method the real request reaches carries, and WHAT the
    /// resource then declares about the request's origin. A mapping is the triple
    /// [`DeclarationRow`] carries.
    type ResourceRow = (
        &'static str,
        Option<(&'static str, &'static str, bool)>,
        Option<(&'static str, &'static str, bool)>,
        Option<Declaration>,
    );

    /// RULE: the declaration is the RESOURCE's, so a request to any of its deployed methods
    /// reads what ANY of them declares — which is what AWS's own flow for enabling CORS
    /// produces, where the `OPTIONS` method carries the mapping and the method the real request
    /// reaches commonly carries none of its own.
    ///
    /// The rows span what that generalises over: which method of the resource carries the
    /// declaration (its `OPTIONS` preflight method, the method the real request reaches, both),
    /// whether the other method declares anything at all, what the declaration names (that site
    /// itself, every origin), and a resource whose declaration names another origin.
    #[test]
    fn a_resource_declares_what_any_of_its_methods_declares() {
        const ALLOW_ORIGIN: &str = "method.response.header.Access-Control-Allow-Origin";
        const ORIGIN: &str = "https://app.example";
        let declared = Some((ALLOW_ORIGIN, "'https://app.example'", true));
        let wildcard = Some((ALLOW_ORIGIN, "'*'", true));
        let another = Some((ALLOW_ORIGIN, "'https://other.example'", true));
        let itself = Some(Declaration::TheOriginItself);
        let every = Some(Declaration::EveryOrigin);
        let cases: [ResourceRow; 7] = [
            ("only the OPTIONS method declares the origin", declared, None, itself),
            ("only the OPTIONS method declares the wildcard", wildcard, None, every),
            ("only the method the real request reaches declares it", None, declared, itself),
            ("both methods declare it", declared, declared, itself),
            ("the origin itself beside the wildcard", wildcard, declared, itself),
            ("neither method declares anything", None, None, None),
            ("the only declaration names another origin", another, None, None),
        ];
        for (case, on_options, on_get, expected) in cases {
            let resource = resource_with(&[
                ("OPTIONS", method_with(on_options)),
                ("GET", method_with(on_get)),
            ]);
            for verb in ["OPTIONS", "GET"] {
                let sent = verbed_request(verb, &[("origin", ORIGIN)]);
                let actual = declared_for(&resource, &sent);
                assert_eq!(
                    actual, expected,
                    "case {case:?} on {verb}: expected the resource to declare {expected:?}, got \
                     {actual:?}"
                );
            }
        }
    }

    /// One deployed resource carrying `methods`, keyed by verb as a stored resource is.
    fn resource_with(methods: &[(&str, MethodEntry)]) -> ResourceEntry {
        ResourceEntry {
            methods: methods
                .iter()
                .map(|(verb, method)| ((*verb).to_string(), method.clone()))
                .collect(),
            ..Default::default()
        }
    }

    /// One method carrying `mapping` as the only response-parameter mapping of its one
    /// integration response, declared by the method response of the same status or not.
    fn method_with(mapping: Option<(&str, &str, bool)>) -> MethodEntry {
        let Some((key, value, declared_by_the_method)) = mapping else {
            return MethodEntry::default();
        };
        let response = GetIntegrationResponseOutput {
            status_code: Some("200".to_string()),
            response_parameters: Some(
                [(key.to_string(), value.to_string())].into_iter().collect(),
            ),
            ..Default::default()
        };
        let integration = GetIntegrationOutput {
            r#type: Some(MOCK_INTEGRATION_TYPE.to_string()),
            integration_responses: Some([("200".to_string(), response)].into_iter().collect()),
            ..Default::default()
        };
        let method_responses = if declared_by_the_method {
            [(
                "200".to_string(),
                crate::ops::types::GetMethodResponseOutput {
                    status_code: Some("200".to_string()),
                    response_parameters: Some([(key.to_string(), false)].into_iter().collect()),
                    response_models: None,
                },
            )]
            .into_iter()
            .collect()
        } else {
            Default::default()
        };
        MethodEntry {
            integration: Some(integration),
            method_responses,
            ..Default::default()
        }
    }

    /// One preflight carrying `headers`.
    fn request(headers: &[(&str, &str)]) -> RtRequest {
        verbed_request("OPTIONS", headers)
    }

    /// One request of `verb` carrying `headers`.
    fn verbed_request(verb: &str, headers: &[(&str, &str)]) -> RtRequest {
        let mut builder = ::http::Request::builder()
            .method(verb)
            .uri("https://127.0.0.1:1/_aws/execute-api/abc1230000/live/cors");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(SdkBody::empty())
            .expect("the test request is well formed");
        RtRequest::try_from(request).expect("the test request is a runtime request")
    }

    /// One row of the credential table below: the case, the `authorizationType` the method
    /// declares, the headers the request carries, and whether it presents a SigV4 credential.
    type CredentialRow = (
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
        bool,
    );

    /// RULE: the stage will authenticate a cross-origin request's own credential before
    /// performing anything for it exactly where the method's `authorizationType` is `AWS_IAM`,
    /// the request CARRIES a SigV4 credential, and the handle the serving would be given vends
    /// access keys — and for no other request, so one nothing could authenticate is never
    /// admitted on that strength.
    ///
    /// The rows span what the rule generalises over: each `authorizationType` this data plane
    /// admits a request under, each form a credential travels in (the `Authorization` header a
    /// signed SDK request carries and the query a presigned URL carries), and a request carrying
    /// none — each asked of all three states the handle can be in: an emulator that ACCEPTS
    /// access keys, one that accepts none, and a deployment carrying no handle at all.
    #[test]
    fn a_credential_the_stage_will_authenticate_is_what_admits_a_signed_call() {
        const SIGNED_HEADER: &str = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/\
                                     execute-api/aws4_request, SignedHeaders=host, Signature=00";
        let cases: [CredentialRow; 6] = [
            (
                "an AWS_IAM method, signed in the Authorization header",
                AUTHORIZATION_AWS_IAM,
                &[("authorization", SIGNED_HEADER)],
                true,
            ),
            (
                "an AWS_IAM method reached with no credential at all",
                AUTHORIZATION_AWS_IAM,
                &[],
                false,
            ),
            (
                "a method that authenticates nobody, signed all the same",
                AUTHORIZATION_NONE,
                &[("authorization", SIGNED_HEADER)],
                true,
            ),
            (
                "a method behind a custom authorizer, signed all the same",
                "CUSTOM",
                &[("authorization", SIGNED_HEADER)],
                true,
            ),
            (
                "a method behind a user pool, signed all the same",
                AUTHORIZATION_COGNITO_USER_POOLS,
                &[("authorization", SIGNED_HEADER)],
                true,
            ),
            // The presigned form carries the credential in the query, which the request below
            // writes into the URI rather than into a header.
            ("an AWS_IAM method reached as a presigned URL", AUTHORIZATION_AWS_IAM, &[], true),
        ];
        // The three states the handle the serving would be given can be in: an emulator that
        // accepts access keys and therefore verifies the signature, one that accepts none and
        // so verifies nothing, and a deployment that carries no handle at all.
        let accepting_keys = authenticating_emulator();
        let handles: [(&str, Option<::smithy_server_runtime::EmbeddedEmulator>); 3] = [
            ("an emulator that accepts access keys", Some(accepting_keys)),
            (
                "an emulator that accepts none",
                Some(super::reached_emulator::contained()),
            ),
            ("a deployment carrying no handle", None),
        ];
        for (case, authorization_type, headers, carries_credential) in cases {
            let presigned = case.contains("presigned");
            let req = if presigned {
                presigned_request()
            } else {
                verbed_request("GET", headers)
            };
            // What the request itself carries, read through the one reader this plane reads a
            // presented credential with, which is the half of the rule that does not depend on
            // the emulator.
            assert_eq!(
                carries_sigv4_credential(&req),
                carries_credential,
                "case {case:?}: expected the request to carry a credential to be \
                 {carries_credential}"
            );
            for (handle_case, reached) in &handles {
                let deployment = DeploymentEntry {
                    reached_emulator: reached.clone(),
                    ..Default::default()
                };
                let method = MethodEntry {
                    authorization_type: authorization_type.to_string(),
                    ..Default::default()
                };
                let resource = resource_with(&[("GET", method.clone())]);
                let served = ServedBy {
                    deployment: &deployment,
                    resource: &resource,
                    method: &method,
                };
                // Authenticated only where the method requires a credential, the request
                // carries one, AND the handle vends access keys — which is the first handle
                // alone.
                let expected = carries_credential
                    && authorization_type == AUTHORIZATION_AWS_IAM
                    && reached
                        .as_ref()
                        .is_some_and(crate::state::accepts_access_keys);
                let actual = will_be_authenticated_before_anything_is_performed(&served, &req);
                assert_eq!(
                    actual, expected,
                    "case {case:?} with {handle_case}: expected the serving to authenticate it \
                     to be {expected}, got {actual}"
                );
            }
        }
    }

    /// A handle onto an emulator that ACCEPTS access keys: a router with this process's own
    /// authenticator registered, which is what vends the credentials a signed request is
    /// verified against.
    fn authenticating_emulator() -> ::smithy_server_runtime::EmbeddedEmulator {
        let mut router = ::smithy_server_runtime::MultiServiceRouter::new();
        router.set_request_authentication(true);
        ::smithy_server_runtime::EmbeddedEmulator::from_router(router)
    }

    /// One presigned invoke: the credential a presigned URL carries in its query string, which
    /// is the second form a request presents a SigV4 credential in.
    fn presigned_request() -> RtRequest {
        let request = ::http::Request::builder()
            .method("GET")
            .uri(
                "https://127.0.0.1:1/_aws/execute-api/abc1230000/live/cors\
                 ?X-Amz-Algorithm=AWS4-HMAC-SHA256\
                 &X-Amz-Credential=AKIDEXAMPLE%2F20260101%2Fus-east-1%2Fexecute-api%2Faws4_request\
                 &X-Amz-Date=20260101T000000Z&X-Amz-SignedHeaders=host&X-Amz-Signature=00",
            )
            .header("origin", "https://app.example")
            .header("sec-fetch-site", "cross-site")
            .body(SdkBody::empty())
            .expect("the test request is well formed");
        RtRequest::try_from(request).expect("the test request is a runtime request")
    }
}
