// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS one domain's own data plane is published at, and what keeps it open.
//!
//! `DomainStatus$Endpoint` is "the domain-specific endpoint used to submit index, search, and
//! data upload requests to the domain": not a label a caller reads back, but the address a
//! client is CONFIGURED with. AWS publishes a bare HOST there — no scheme, no port and no
//! path — so a client takes that value as a host: it signs it, it hands it to an OpenSearch
//! client as `hosts=[{'host': <endpoint>, …}]`, and it joins an absolute request path onto
//! it. A value carrying a path is therefore unusable to every client that does any of those
//! three, however reachable it is for one that concatenates.
//!
//! So a domain publishes a listener of ITS OWN, opened through the core
//! (`engine.listeners().http(..)`, which owns the socket, the bind interface and the TLS):
//! ONE listener per domain, so the ADDRESS identifies the domain and the whole path of a
//! request stays the client's. The host AWS names a domain's endpoint by
//! (`search-<domain>-<suffix>.<region>.es.amazonaws.com`) resolves nowhere on the machine
//! this emulator runs on, and the emulator's own front door can only serve a request that
//! carries the domain in its path ([`crate::service::domain_rest`]), which a client told a
//! bare host does not.
//!
//! The handle lives in
//! [`super::ComAmazonawsOpensearchAmazonopensearchserviceState::domain_planes`] under the
//! domain's name, beside the records that say which domains exist: dropping it closes the
//! port, so a deleted domain stops answering. How many ports one store can hold open is
//! bounded by the resources a caller may create: one per domain, and a store holds at most
//! [`super::DOMAIN_QUOTA`] of those.
//!
//! What the address answers is exactly what the front door's own path-style addressing
//! answers, out of the same records, because both go through
//! [`crate::service::domain_rest::serve_domain`]: the two addressings of one domain cannot
//! diverge in what they serve.
//!
//! An emulator that can open no listener at all (a host driving it on a runtime with no IO)
//! publishes the prefixed path-style address instead ([`super::published_domain_address`]),
//! so a domain is never left reporting an address nothing serves.
//!
//! # Who answers at that address
//!
//! The address is an ADDRESS, so nothing can be read off its host: a client inside this
//! emulator that is handed one (a running function that read `Endpoint` out of the control
//! plane and dials it) cannot tell which service answers there the way it can for an endpoint
//! host. So the authority is recorded with this service's signing name where every such
//! reader looks ([`::service_common::published_data_planes`]) — for as long as the port is
//! open, which is why the handle that keeps it open is what withdraws the record.

use std::future::Future;
use std::pin::Pin;

use super::ComAmazonawsOpensearchAmazonopensearchserviceState as ServiceState;

/// One domain's data-plane listener: the address it answers on, and the handle that keeps it
/// open.
pub struct DomainPlane {
    /// The `host:port` a client dials this domain at, as it was recorded where a reader
    /// resolves which service answers at an address, so the drop below can withdraw the very
    /// key the publish wrote.
    authority: String,
    /// Where that record lives, for as long as this handle does. `None` on an engine that
    /// vends no such records, where nothing was recorded either.
    published: Option<::service_common::published_data_planes::PublishedPlanes>,
    /// Held only to keep the port open; the core owns the socket and its TLS. Dropping it
    /// closes the port and severs what is in flight.
    _listener: ::emulator_server::ResourceListener,
}

impl DomainPlane {
    /// The `host:port` a client dials this domain's data plane at.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }
}

/// The record of WHO answers at this address lasts exactly as long as the port does: the
/// listener closes as this handle drops (the delete that removes the domain drops it), so the
/// address stops resolving to this service at the same instant rather than naming a service
/// for a port nothing listens on.
impl Drop for DomainPlane {
    fn drop(&mut self) {
        if let Some(published) = &self.published {
            published.withdraw(&self.authority);
        }
    }
}

/// Written by hand rather than derived: a [`DomainPlane`] holds a core `ResourceListener`,
/// which is a live socket and carries no `Debug`. What is diagnosable is the address it
/// announced.
impl std::fmt::Debug for DomainPlane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DomainPlane")
            .field("authority", &self.authority)
            .finish()
    }
}

/// Open the listener the domain `domain_name` is published on and record its address on the
/// stored domain, so every reader of that domain reports the address this emulator answers it
/// at — on a task of its OWN, so the create that asked for it does not hold its store across
/// the bind.
///
/// Called by the create that stored the domain, after the store's lock is released: a domain
/// publishes no address until its cluster is up ([`super::Domain::to_status`]), which is a
/// span later than the create, so the first read that can report one is a later call and this
/// has completed by then. An emulator with no runtime to spawn on opens nothing, and the
/// domain publishes the prefixed path-style address instead.
pub fn publish_in_background<Eng: Send + Sync + 'static>(
    engine: ::std::sync::Arc<Eng>,
    state: &::engine::ServiceState<ServiceState>,
    domain_name: &str,
    callback: ::smithy_server_runtime::EmbeddedEmulator,
) {
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let (state, domain_name) = (state.clone(), domain_name.to_string());
    runtime.spawn(async move { publish(engine, &state, &domain_name, callback).await });
}

/// Open the listener the domain `domain_name` is published on and record its address on the
/// stored domain, so every reader of that domain reports the address this emulator answers it
/// at.
///
/// IDEMPOTENT, and checked BEFORE a socket is bound: a domain already published at an address
/// keeps that address, because a client may be dialling it. Nothing is published either when
/// no listener can be opened, or when the domain is already gone: the record keeps the
/// prefixed path-style address, which is what [`super::published_domain_address`] answers.
pub async fn publish<Eng: Send + Sync + 'static>(
    engine: ::std::sync::Arc<Eng>,
    state: &::engine::ServiceState<ServiceState>,
    domain_name: &str,
    callback: ::smithy_server_runtime::EmbeddedEmulator,
) {
    if state.lock().domain_planes.contains_key(domain_name) {
        return;
    }
    let Some(plane) = open(engine, domain_name, callback).await else {
        return;
    };
    let authority = plane.authority().to_string();
    let mut store = state.lock();
    // Checked AGAIN under this guard, because the check above released it for the bind: a
    // second call that got there first has published an address a client may already be
    // dialling, and REPLACING its record would close that port. The address stands, and the
    // plane this call opened closes as it returns.
    if store.domain_planes.contains_key(domain_name) {
        return;
    }
    // The domain may have been deleted while the port was being bound; its address is then
    // nothing to publish, and the listener closes as this returns.
    let Some(record) = store.domains.get_mut(domain_name) else {
        return;
    };
    record.plane_authority = Some(authority);
    store.domain_planes.insert(domain_name.to_string(), plane);
}

/// Open one domain's listener, or `None` when this engine vends none and when the socket
/// cannot be bound at all. Either leaves the domain publishing the prefixed path-style address
/// rather than taking the request down with it.
///
/// It runs on the task [`publish_in_background`] spawned, which is what keeps the bind off the
/// call that asked for it: a host driving this emulator in process may poll a request on
/// another executor, or on a runtime built with no IO driver, and neither can open a socket —
/// which is why that function asks the runtime for a handle rather than assuming one.
async fn open<Eng: Send + Sync + 'static>(
    engine: ::std::sync::Arc<Eng>,
    domain_name: &str,
    callback: ::smithy_server_runtime::EmbeddedEmulator,
) -> Option<DomainPlane> {
    let listeners = shared_engine(&*engine)?.listeners();
    // Where the address this listener is opened at is recorded, read before the engine is
    // handed to the handler below. `None` on an engine that vends no such records, which
    // records nothing and withdraws nothing.
    let published = ::service_common::published_data_planes::PublishedPlanes::of(&*engine);
    let handler = DomainRestServer {
        engine,
        domain_name: domain_name.to_string(),
        callback,
    };
    let listener = match listeners
        .http(handler, ::emulator_server::HttpPolicy::default())
        .await
    {
        Ok(listener) => listener,
        // Reported rather than discarded, so a port range or a descriptor table that is
        // exhausted is told apart from a runtime that opens no sockets at all.
        Err(error) => {
            ::tracing::warn!(
                %error,
                domain_name,
                "a domain's data-plane address could not be opened"
            );
            return None;
        }
    };
    // The core's OWN reading of the address a client dials, which names the loopback interface
    // for a wildcard bind — where the bound address would be `0.0.0.0`, an address no client
    // dials — and brackets an IPv6 one, so the address parses wherever the operator bound it.
    let authority = ::service_common::data_plane_addressing::listener_authority(&listener);
    // Recorded as the port opens, under this service's own signing name — which is the
    // credential scope a client signs a request to this address with — so a client inside this
    // emulator that was handed the address can be routed to this service (see the module
    // docs). Withdrawn when the handle above is dropped.
    if let Some(published) = &published {
        published.publish(&authority, super::ARN_NAMESPACE);
    }
    Some(DomainPlane {
        authority,
        published,
        _listener: listener,
    })
}

/// The shared engine behind a handler's `Eng`, which is what vends the core's listeners.
fn shared_engine<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<&::engine::InMemoryEngine> {
    (engine as &dyn ::std::any::Any).downcast_ref::<::engine::InMemoryEngine>()
}

/// Serves ONE domain's published address. The core runs the browser checks and owns CORS;
/// this answers the OpenSearch REST request out of the domain's own records.
///
/// The shared engine is held because the decisions this plane takes are not this service's
/// alone: WHO a request speaks for is a question about the account's identity records, which
/// live in that engine beside this service's own store
/// (`crate::service::domain_rest::serve_published`).
///
/// The CALLBACK is held for the same reason: what a request accepted here does is not this
/// service's alone either — a monitor run whose trigger fires publishes its action's
/// notification to another service's topic — and reaching that service in process is what this
/// handle is. It is the handle of the call that opened this listener, and it stays usable for
/// as long as the emulator does, so every request served at this address can hand over what it
/// owes just as one served at the front door can.
struct DomainRestServer<Eng> {
    engine: ::std::sync::Arc<Eng>,
    domain_name: String,
    callback: ::smithy_server_runtime::EmbeddedEmulator,
}

impl<Eng: Send + Sync + 'static> ::emulator_server::HttpHandler for DomainRestServer<Eng> {
    fn handle(
        &self,
        req: ::http::Request<Vec<u8>>,
    ) -> Pin<Box<dyn Future<Output = ::http::Response<Vec<u8>>> + Send>> {
        let engine = ::std::sync::Arc::clone(&self.engine);
        let domain_name = self.domain_name.clone();
        let callback = self.callback.clone();
        Box::pin(async move {
            crate::service::domain_rest::serve_published(
                &*engine,
                &domain_name,
                &req,
                Some(&callback),
            )
            .await
        })
    }
}
