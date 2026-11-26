// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a Classic load balancer PUBLISHES as its `DNSName`, and where a client that
//! holds that address reaches it.
//!
//! `CreateAccessPointOutput$DNSName` is "the DNS name of the load balancer", and
//! `DescribeLoadBalancers` reports the same value: it is the ONE address a Classic client is
//! given, and what a client does with it is OPEN A CONNECTION to it and send the load
//! balancer its traffic. So the only thing the value may carry is an address the client that
//! was handed it can reach.
//!
//! RULE: that address is a SOCKET OF THIS LOAD BALANCER'S OWN, opened through the core when
//! the load balancer is created and published as the `host:port` a client dials
//! ([`open_load_balancer_endpoint`]) — the same reading the ELBv2 half of this endpoint
//! publishes a load balancer's address under
//! (`service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::ops::create_load_balancer`).
//! Two addresses a load balancer could be published at are NOT reachable, and neither is
//! published:
//!
//! * a NAME of this emulator's own, in Elastic Load Balancing's zone or in the zone a
//!   resolver answers with the interface the core binds ([`published_dns_name`]): the first
//!   resolves nowhere near this process or, worse, at genuine AWS infrastructure, and nothing
//!   on the machine this emulator runs on resolves the second either, so a client handed one
//!   reaches nothing at all;
//! * ONE host of this emulator's own — the address its API is served at — which would be the
//!   published address of EVERY load balancer at once, leaving nothing in a request addressed
//!   there to say which load balancer a client was handed it for.
//!
//! A port therefore travels in the value, because the address has to be dialable as it stands:
//! this emulator's sockets are opened on ports the core assigns, so an address of one of them
//! without its port names nothing.
//!
//! The socket is opened through the core (`engine.listeners()`), which owns the bind
//! interface, the port range and the connection caps, and every request it accepts is handed
//! to this load balancer's own data plane ([`crate::service::listener_socket`], then
//! [`crate::service::data_plane`]). That plane also claims a request whose host IS the
//! published address on whatever endpoint of this emulator it arrived, so an in-process client
//! reaches the load balancer through the address it read off the control plane too.
//!
//! A load balancer of an emulator whose engine vends no listener at all has no socket to
//! publish, and then the name this service would have minted is published instead
//! ([`published_dns_name`]) — a value of the modelled shape, naming the load balancer and
//! nothing of AWS's own zone, for an emulator that can hand out no dialable address of any
//! kind.
//!
//! The record lives for exactly as long as the load balancer: it is held on the store under
//! the load balancer's region and name, and dropping it closes the port it kept open.

use ::smithy_server_runtime::OperationContext;

/// One load balancer's published address: the address it hands a client, the socket that
/// address answers at, and the handle that keeps that port open.
pub struct LoadBalancerEndpoint {
    /// The address a client dials, which is what the load balancer publishes as its
    /// `DNSName`: the `host:port` of its own socket, or the minted name for an emulator whose
    /// engine vends no listener.
    published_name: String,
    /// The `host:port` of the socket this load balancer was given — the address a client
    /// opens a connection to — and `None` for a load balancer of an emulator whose engine
    /// vends no listener at all.
    dialled_authority: Option<String>,
    /// The socket of its own this load balancer was given. Never read: it is held because
    /// dropping it closes the port.
    _listener: Option<::emulator_server::ResourceListener>,
}

impl ::std::fmt::Debug for LoadBalancerEndpoint {
    /// Everything about this record that is not a live socket: the address it publishes and
    /// the address its own socket answers at. `ResourceListener` is not `Debug`.
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("LoadBalancerEndpoint")
            .field("published_name", &self.published_name)
            .field("dialled_authority", &self.dialled_authority)
            .finish_non_exhaustive()
    }
}

impl LoadBalancerEndpoint {
    /// The address a client reaches this load balancer at — its `DNSName`.
    pub fn published_name(&self) -> &str {
        &self.published_name
    }

    /// The `host:port` a client dials this load balancer at, and `None` for a load balancer
    /// of an emulator whose engine vends no listener at all.
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

/// The name load balancer `load_balancer_name` of `region` would be published under where
/// this emulator can hand out no dialable address at all:
/// `[internal-]<name>-<id>.<region>.elb.<zone>`.
///
/// What a load balancer publishes is the address of its own socket
/// ([`open_load_balancer_endpoint`]); this is the value for an emulator whose engine vends no
/// listener, where there is no socket to name and a value of the modelled shape says at least
/// WHICH load balancer it belongs to.
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
/// the minted name's zone is read off ([`announced_zone`]) — or `None` for a request this
/// emulator has no endpoint of its own to name.
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
/// `region`: the socket it is given, and the `host:port` of that socket, which is what it
/// publishes as its `DNSName`.
pub async fn open_load_balancer_endpoint<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    region: &str,
    load_balancer_name: &str,
    scheme: &str,
) -> LoadBalancerEndpoint {
    // A socket of this load balancer's OWN, whatever address this emulator's API is served
    // at: the published value has to be dialable by the client that was handed it, and this
    // is the one address of this process that names THIS load balancer (see the module docs).
    let listener = open_listener(engine, region, load_balancer_name, ctx.callback().clone()).await;
    let dialled_authority = listener
        .as_ref()
        .map(::service_common::data_plane_addressing::listener_authority);
    LoadBalancerEndpoint {
        published_name: dialled_authority.clone().unwrap_or_else(|| {
            published_dns_name(region, load_balancer_name, scheme, &announced_zone(ctx))
        }),
        dialled_authority,
        _listener: listener,
    }
}

/// Open the socket a load balancer is dialled at, or `None` when this engine vends no
/// listeners at all.
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
