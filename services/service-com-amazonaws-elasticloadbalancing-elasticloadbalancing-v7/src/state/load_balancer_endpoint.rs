// SPDX-License-Identifier: CC0-1.0

//! The NAME a Classic load balancer PUBLISHES as its `DNSName`, and where a client that
//! holds that name reaches it.
//!
//! `CreateAccessPointOutput$DNSName` is "the DNS name of the load balancer", and
//! `DescribeLoadBalancers` reports the same value: it is the one address a Classic client is
//! given, and what a client does with it is RESOLVE it and then connect to a listener port on
//! what it resolved. So the published value is a HOST NAME and nothing else — no port, no
//! scheme — in the shape Elastic Load Balancing's own names carry
//! ([`published_dns_name`]): a value with a port embedded in it is not a name a client can
//! resolve, put in a `Host` header or point a DNS record at.
//!
//! WHICH zone that name sits in is what makes it reachable. A name in a zone Amazon Web
//! Services serves resolves nowhere near this process or, worse, at genuine AWS
//! infrastructure, so this emulator never announces one; the zone it MAY announce a name of
//! its own in is read off the address the request reached it at
//! ([`::service_common::data_plane_addressing::announced_zone_of_authority`], the workspace's
//! one reading of that question) — the loopback zone, which a resolver answers with the very
//! interface the core binds, for a request that reached an address. The same reading the
//! database endpoints of this tree are minted under
//! (`service_com_amazonaws_rds_amazonrdsv19::state::served_endpoint_suffix`).
//!
//! The name is SERVED at the endpoint this emulator is reached on: a request whose host is a
//! load balancer's published name is claimed by this service and answered by that load
//! balancer's own plane ([`crate::service::data_plane`]), whichever port of this emulator it
//! arrived on. Nothing of a second address of this emulator's own choosing is published: an
//! extra loopback port is not an address a client that was handed a NAME can reach, and the
//! endpoint the caller is already talking to is.
//!
//! A load balancer is given a SOCKET of its own only where this emulator is reached on no
//! endpoint at all — an emulator embedded in a host's process, which is served on no port, so
//! there is no endpoint for a published name to be served at. That socket is then the one
//! address a client of such an emulator can dial, and it hands every request it accepts to
//! the same data plane ([`crate::service::listener_socket`]); it is opened on the same reading
//! a distribution's edge follows
//! (`service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::opened_edge_endpoint`,
//! "a create that reached the emulator's OWN endpoint needs no address of its own").
//!
//! The record lives for exactly as long as the load balancer: it is held on the store under
//! the load balancer's region and name, and dropping it closes any port it kept open.

use ::smithy_server_runtime::OperationContext;

/// One load balancer's published address: the name it hands a client, the address a client of
/// an UNSERVED emulator dials, and the socket — where one was opened at all — that keeps that
/// port open.
pub struct LoadBalancerEndpoint {
    /// The host name a client resolves, which is what the load balancer publishes as its
    /// `DNSName`.
    published_name: String,
    /// The `host:port` of the socket this load balancer was given, for an emulator that is
    /// served on no endpoint of its own — the one address a client of such an emulator can
    /// dial. `None` for a load balancer whose published name is served at this emulator's own
    /// endpoint.
    dialled_authority: Option<String>,
    /// The socket of its own this load balancer was given. Never read: it is held because
    /// dropping it closes the port.
    _listener: Option<::emulator_server::ResourceListener>,
}

impl ::std::fmt::Debug for LoadBalancerEndpoint {
    /// Everything about this record that is not a live socket: the name it publishes and the
    /// address its own socket answers at. `ResourceListener` is not `Debug`.
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("LoadBalancerEndpoint")
            .field("published_name", &self.published_name)
            .field("dialled_authority", &self.dialled_authority)
            .finish_non_exhaustive()
    }
}

impl LoadBalancerEndpoint {
    /// The host name a client reaches this load balancer at — its `DNSName`.
    pub fn published_name(&self) -> &str {
        &self.published_name
    }

    /// The `host:port` a client of an unserved emulator dials this load balancer at, and
    /// `None` for one whose published name is served at this emulator's own endpoint.
    pub fn dialled_authority(&self) -> Option<&str> {
        self.dialled_authority.as_deref()
    }
}

/// The label every Elastic Load Balancing name carries ahead of its zone, from the shape of
/// the names the service's own zone gives a load balancer
/// (`<name>-<id>.<region>.elb.amazonaws.com`).
pub const ELB_DNS_LABEL: &str = "elb";

/// The fixed prefix the name of an `internal` load balancer begins with — the one part of the
/// published name that says whether a load balancer's address resolves to private addresses.
pub const INTERNAL_NAME_PREFIX: &str = "internal-";

/// The name load balancer `load_balancer_name` of `region` publishes as its `DNSName`:
/// `[internal-]<name>-<id>.<region>.elb.<zone>`.
///
/// The fixed text is [`INTERNAL_NAME_PREFIX`] — carried only by the `internal` scheme, whose
/// name resolves to private addresses — [`ELB_DNS_LABEL`] and the separating dots; the
/// variable parts are the load balancer's own name, the id generated for it
/// ([`super::dns_suffix`], stable for one record so every read reports the same name), the
/// region it was created in, and `zone`, the zone this emulator may announce a name of its own
/// in ([`announced_zone`]).
///
/// The name is written as ONE DNS label per part
/// ([`::service_common::data_plane_addressing::dns_label`] for the caller's own name), so a
/// name carrying a host or path delimiter cannot make the published value name an authority
/// the caller chose.
#[must_use]
pub fn published_dns_name(
    region: &str,
    load_balancer_name: &str,
    scheme: &str,
    zone: &str,
) -> String {
    let label = ::service_common::data_plane_addressing::dns_label(load_balancer_name);
    let id = super::dns_suffix(region, load_balancer_name);
    let prefix = match scheme == super::SCHEME_INTERNAL {
        true => INTERNAL_NAME_PREFIX,
        false => "",
    };
    format!("{prefix}{label}-{id}.{region}.{ELB_DNS_LABEL}.{zone}")
}

/// The DNS zone this emulator may announce a name of its own in, for the request in `ctx`.
///
/// The workspace's one reading of that question
/// ([`::service_common::data_plane_addressing::announced_zone_of_authority`]), applied to the
/// address this request reached this emulator at: the loopback zone — which a resolver answers
/// with the interface the core binds its listeners on — for a request that reached an address,
/// the reached name itself for a request that reached one of this emulator's own names, and
/// never a zone AWS publishes its own endpoints in.
///
/// An emulator reached on no endpoint of its own has no address to read a zone off, and binds
/// every listener it opens on the loopback interface, so the zone a resolver answers with that
/// interface is the one a name of such a load balancer may sit in
/// ([`::service_common::data_plane_addressing::LOOPBACK_ZONE`]).
#[must_use]
pub fn announced_zone(ctx: &OperationContext<'_>) -> String {
    match endpoint_authority(ctx) {
        Some(authority) => {
            ::service_common::data_plane_addressing::announced_zone_of_authority(&authority)
        }
        None => ::service_common::data_plane_addressing::LOOPBACK_ZONE.to_string(),
    }
}

/// The `host[:port]` of THIS EMULATOR's own endpoint for the request in `ctx` — the address
/// the published name's zone is read off, and the endpoint that name is served at — or `None`
/// for a request this emulator has no endpoint of its own to name.
///
/// In the order the other data planes of this tree read it:
///
/// * a request that addressed the EMULATOR'S OWN endpoint
///   ([`::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts`]) comes
///   from a client that resolves nothing else, so that is the address it reached;
/// * a request on an emulator that IS served on a port
///   ([`::service_common::data_plane_addressing::served_endpoint_authority`]) — a caller that
///   addressed one of AWS's own hosts, including an in-process call of another service — is
///   read off the one address this process answers on.
///
/// `None` is the one case where there is no endpoint of this process to name: an emulator
/// embedded in a host's process, served on no port at all
/// ([`::smithy_server_runtime::UNSERVED_URL_BASE`], a host RFC 6761 reserves so that nothing
/// resolves it), reached by a caller that addressed one of AWS's own hosts.
fn endpoint_authority(ctx: &OperationContext<'_>) -> Option<String> {
    let base = ctx.endpoint();
    ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(ctx.parts())
        .or_else(|| ::service_common::data_plane_addressing::served_endpoint_authority(base.as_str()))
}

/// The published address of the load balancer `load_balancer_name` one create is minting in
/// `region`: the name it publishes, and the socket it is given where this emulator has no
/// endpoint of its own to serve that name at.
pub async fn open_load_balancer_endpoint<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    region: &str,
    load_balancer_name: &str,
    scheme: &str,
) -> LoadBalancerEndpoint {
    let published_name =
        published_dns_name(region, load_balancer_name, scheme, &announced_zone(ctx));
    // An emulator served on an endpoint of its own serves the whole surface there, this load
    // balancer's plane among it, so nothing of a second address is opened.
    let listener = match endpoint_authority(ctx) {
        Some(_) => None,
        None => open_listener(engine, region, load_balancer_name, ctx.callback().clone()).await,
    };
    LoadBalancerEndpoint {
        published_name,
        dialled_authority: listener
            .as_ref()
            .map(::service_common::data_plane_addressing::listener_authority),
        _listener: listener,
    }
}

/// Open the socket a load balancer of an UNSERVED emulator is dialled at, or `None` when this
/// engine vends no listeners at all.
///
/// `callback` is the handle into the emulator this request is running within
/// (`ctx.callback()`), which is how a request the socket accepts reaches the data plane: the
/// socket is not a second emulator, it is a second ADDRESS of this one.
async fn open_listener<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    load_balancer_name: &str,
    callback: ::smithy_server_runtime::EmbeddedEmulator,
) -> Option<::emulator_server::ResourceListener> {
    let shared = (engine as &dyn ::std::any::Any).downcast_ref::<::engine::InMemoryEngine>()?;
    let listeners = shared.listeners();
    let handler = crate::service::listener_socket::ListenerSocket::new(
        region.to_string(),
        load_balancer_name.to_string(),
        callback,
        // The core's own TLS server, for a client that dials this address with `https://` —
        // which is what a client of an HTTPS or SSL listener does. The service never touches a
        // key or a certificate of its own.
        listeners.tls_upgrader().ok(),
    );
    // Opened on a task of its OWN, so a runtime that cannot bind a socket at all — one built
    // with no IO driver, which is what a host that drives this emulator in process may hand a
    // request — leaves this create without a socket instead of taking the request down with
    // it: the join answers `Err` and this answers `None`.
    ::tokio::spawn(async move { listeners.tcp_plaintext(handler).await.ok() })
        .await
        .ok()
        .flatten()
}
