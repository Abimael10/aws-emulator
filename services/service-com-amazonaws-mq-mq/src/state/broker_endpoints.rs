// SPDX-License-Identifier: CC0-1.0

//! Where a client REACHES a broker: the web console URL and the wire-protocol
//! endpoints a running broker publishes.
//!
//! RULE: a broker that is RUNNING reports the instance allocated to it, carrying its
//! console URL and one endpoint URI per wire protocol the broker's ENGINE serves, in
//! the order Amazon MQ publishes those protocols in; a broker still being provisioned
//! has no instance allocated yet and reports none.
//!
//! The protocols, their URI schemes and their ports are the ones Amazon MQ publishes
//! per engine (the Developer Guide's broker endpoint tables): an ActiveMQ broker serves
//! OpenWire, AMQP, STOMP, MQTT and WebSocket, each over TLS on its own port, with the
//! ActiveMQ Web Console on 8162; a RabbitMQ broker serves AMQP over TLS, with its
//! management console on the HTTPS port.
//!
//! The ORDER of the published list is that PROTOCOL order — never the alphabetical order
//! of the URI schemes — so a client that takes the first endpoint as the broker's primary
//! one (as the console's and the Developer Guide's connection snippets do) finds the
//! engine's primary wire there rather than whichever scheme happens to sort first.
//!
//! The ADDRESS they name is an address THIS EMULATOR ANSWERS ON. A console URL and a wire
//! endpoint are addresses a client RESOLVES and then dials — a library dials the authority
//! the control plane published and an operator opens the console URL verbatim — so an
//! authority composed out of the resource's id, this service's label and a DNS zone names a
//! host nothing on this machine resolves, and a broker publishing one would report a healthy
//! RUNNING data plane at an address pointing away from the emulator. So:
//!
//! * a protocol this emulator opens a SOCKET for — the broker's own wire ([`serve_broker`])
//!   and its web console ([`super::broker_console`]) — is published at that socket's own
//!   dialable `host:port`, which the CORE assigns, so what a client dials is where the
//!   broker really answers and no two brokers of one account publish that authority;
//! * every other protocol is published on the host those sockets are bound on
//!   ([`served_authority_host`]) and the port Amazon MQ publishes that protocol on — the
//!   host this emulator was reached at ([`published_broker_host`], read once by the create
//!   and kept on the record) where the broker has no socket of its own.
//!
//! The ADDRESS a broker reports for its own network interface (`BrokerInstance$ipAddress`)
//! is read the same way: the address of the interface this emulator serves that broker on,
//! never a private-subnet literal belonging to no interface of this machine.

use super::*;

/// One wire protocol a broker serves: the URI scheme a client dials it with, the port
/// Amazon MQ publishes that protocol on, and whether it is the protocol this emulator
/// opens a socket of its own for.
#[derive(Debug, Clone, Copy)]
struct WireProtocol {
    scheme: &'static str,
    port: u16,
    /// Whether a broker this emulator opened a socket for serves THIS protocol on it, in
    /// which case the endpoint published for it is that socket's address.
    served: bool,
}

impl WireProtocol {
    /// The endpoint URI this protocol is reached at on `host:port`, in the
    /// `<scheme>://<host>:<port>` form Amazon MQ publishes an endpoint as — with an IPv6
    /// host bracketed, as an authority carrying one has to be.
    fn endpoint(self, host: &str, port: u16) -> String {
        format!(
            "{}://{}",
            self.scheme,
            ::service_common::data_plane_addressing::host_port_authority(host, port)
        )
    }
}

/// The protocols an ActiveMQ broker serves, each over TLS, in the order Amazon MQ
/// publishes them in: OpenWire, AMQP, STOMP, MQTT and WebSocket — the order the
/// Developer Guide's ActiveMQ endpoint table names them in and the order a recorded
/// broker answer lists its endpoints in.
///
/// The order is the broker's own PROTOCOL order and not the spelling of the URI schemes:
/// the first endpoint is the broker's primary (OpenWire/JMS) one, which is what a client
/// that reads one endpoint rather than parsing every URI dials.
///
/// STOMP is the one of these this emulator opens the broker's own socket for
/// ([`served_wire`]), so its endpoint is that socket's address; the rest name the broker's
/// own host and the port Amazon MQ publishes that protocol on.
const ACTIVEMQ_WIRE_PROTOCOLS: &[WireProtocol] = &[
    // OpenWire — the broker's primary wire, which JMS clients connect over.
    WireProtocol {
        scheme: "ssl",
        port: 61_617,
        served: false,
    },
    // AMQP.
    WireProtocol {
        scheme: "amqp+ssl",
        port: 5_671,
        served: false,
    },
    // STOMP, which is the wire this emulator serves an ActiveMQ broker's own socket with.
    WireProtocol {
        scheme: "stomp+ssl",
        port: 61_614,
        served: true,
    },
    // MQTT.
    WireProtocol {
        scheme: "mqtt+ssl",
        port: 8_883,
        served: false,
    },
    // WebSocket.
    WireProtocol {
        scheme: "wss",
        port: 61_619,
        served: false,
    },
];

/// The one protocol a RabbitMQ broker serves: AMQP over TLS, which is the wire this
/// emulator opens the broker's own socket for.
const RABBITMQ_WIRE_PROTOCOLS: &[WireProtocol] = &[WireProtocol {
    scheme: "amqps",
    port: 5_671,
    served: true,
}];

/// The port the ActiveMQ Web Console is published on.
const ACTIVEMQ_CONSOLE_PORT: u16 = 8_162;

/// The port the RabbitMQ management console is published on: the HTTPS one.
const RABBITMQ_CONSOLE_PORT: u16 = 443;

/// The scheme every broker console URL carries: both consoles are served over TLS.
const CONSOLE_SCHEME: &str = "https";

/// The host THIS EMULATOR was reached at, which a broker publishes an address under when
/// this emulator opened no socket of its own for that protocol.
///
/// A published address is an address a client RESOLVES and then dials, so the only host a
/// broker of this emulator can name is one this process answers on: the `host[:port]` the
/// request reached it at, which is either an address of this machine or the name an operator
/// points clients at, read through the workspace's one reading of that question
/// (`::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts`, which
/// hands back no host that names some service's endpoint). A caller that addressed nothing
/// host-shaped leaves the broker published on the interface the core binds its listeners on
/// ([`DEFAULT_SERVED_INTERFACE`]).
///
/// It is read ONCE, by the create, and kept on the record ([`Broker::published_host`]), so
/// every later read of the broker publishes the one host whichever host a later caller
/// reached the emulator on.
pub fn published_broker_host(reached_authority: &str) -> String {
    let host = ::service_common::data_plane_addressing::host_only(reached_authority);
    let names_an_interface = ::service_common::data_plane_addressing::is_host_shaped(host)
        // The UNSPECIFIED address (`0.0.0.0`, `::`) is what an operator binds to name EVERY
        // interface, and names none to dial: a client handed it reaches nothing.
        && address_literal(host).is_none_or(|address| !address.is_unspecified());
    match names_an_interface {
        true => host.to_string(),
        false => DEFAULT_SERVED_INTERFACE.to_string(),
    }
}

/// The interface this emulator serves a broker on when it opened no socket of its own for it
/// and was reached at no host either: the one the core binds every listener on unless the
/// operator chooses an address (`::emulator_server::DEFAULT_BIND_ADDRESS`).
pub const DEFAULT_SERVED_INTERFACE: ::std::net::IpAddr = ::emulator_server::DEFAULT_BIND_ADDRESS;

/// The host every address a broker publishes names: the one the SOCKETS this emulator opened
/// for that broker are bound on.
///
/// What a published address has to be is an address the broker ANSWERS at, so the host it
/// names is the host of the sockets it answers on — the core's own reading of where it bound
/// them ([`serve_broker`], [`super::broker_console`]), the wire's and the console's being
/// the same interface. A broker this emulator opened no socket at all for names the host the
/// emulator was reached at instead ([`published_broker_host`]), which is the one host left
/// that this process answers on.
pub fn served_authority_host<'a>(
    served: Option<&'a ServedAddress>,
    console: Option<&'a ServedAddress>,
    published_host: &'a str,
) -> &'a str {
    served
        .or(console)
        .map_or(published_host, |address| address.host.as_str())
}

/// The address of the interface this emulator serves a broker on, as
/// `BrokerInstance$ipAddress` reports it ("The IP address of the Elastic Network Interface
/// (ENI) attached to the broker").
///
/// Amazon MQ reports the address of the interface its clients reach the broker THROUGH, so
/// what answers it here is the first of `hosts` that IS an address of an interface, in the
/// order they answer that question: the host the broker's own sockets are bound on
/// ([`served_authority_host`]) — the interface its published addresses name, so the two
/// agree — and then the host a client reached this emulator at. A host that is a NAME names
/// no interface by itself and neither does the unspecified address, which names every one of
/// them; a broker left with no address at all reports the interface the core binds on
/// ([`DEFAULT_SERVED_INTERFACE`]) — never a private-subnet literal, which belongs to no
/// interface this emulator serves.
pub fn served_interface_address(hosts: &[&str]) -> String {
    hosts
        .iter()
        .copied()
        .filter_map(address_literal)
        .find(|address| !address.is_unspecified())
        .map_or_else(|| DEFAULT_SERVED_INTERFACE.to_string(), |a| a.to_string())
}

/// The IP ADDRESS `host` is, for a host written as an address literal — one bracketed IPv6
/// pair included, whose brackets belong to the authority it was read out of rather than to
/// the address — and `None` for a host that is a NAME, which is no address.
fn address_literal(host: &str) -> Option<::std::net::IpAddr> {
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
        .parse::<::std::net::IpAddr>()
        .ok()
}

/// One instance allocated to a broker, as a client addresses it: the console to open,
/// the endpoints to dial, and the broker's own IP address when it has one.
///
/// Held as plain values rather than as the modelled `BrokerInstance` so the record and
/// the response shape stay separate — the handler that answers builds the modelled
/// structure from this.
#[derive(Debug, Clone)]
pub struct BrokerInstanceAddress {
    pub console_url: String,
    pub endpoints: Vec<String>,
    pub ip_address: Option<String>,
}

impl Broker {
    /// The wire protocols this broker's engine serves. A value outside the model's
    /// `EngineType` set reaches no stored broker (a create refuses one), and ActiveMQ
    /// is the engine whose protocol set the model's own members describe.
    fn wire_protocols(&self) -> &'static [WireProtocol] {
        if self.engine_type == ENGINE_TYPE_RABBITMQ {
            RABBITMQ_WIRE_PROTOCOLS
        } else {
            ACTIVEMQ_WIRE_PROTOCOLS
        }
    }

    /// The port this broker's web console is published on.
    fn console_port(&self) -> u16 {
        if self.engine_type == ENGINE_TYPE_RABBITMQ {
            RABBITMQ_CONSOLE_PORT
        } else {
            ACTIVEMQ_CONSOLE_PORT
        }
    }

    /// The host every address this broker publishes names: the host of the sockets this
    /// emulator opened for it ([`served_authority_host`]).
    fn authority_host(&self) -> &str {
        served_authority_host(
            self.served_endpoint.as_ref(),
            self.console_endpoint.as_ref(),
            &self.published_host,
        )
    }

    /// The broker's own IP address, as `BrokerInstance$ipAddress` reports it ("The IP
    /// address of the Elastic Network Interface (ENI) attached to the broker. Does not
    /// apply to RabbitMQ brokers").
    ///
    /// It is the address of the interface this emulator serves THIS broker on
    /// ([`served_interface_address`]), so the address reported and the addresses published
    /// name the one interface a client reaches the broker through; a RabbitMQ broker
    /// reports none, the member not applying to that engine.
    fn ip_address(&self) -> Option<String> {
        (self.engine_type != ENGINE_TYPE_RABBITMQ).then(|| {
            served_interface_address(&[self.authority_host(), &self.published_host])
        })
    }

    /// The URL of this broker's web console, as `BrokerInstance$consoleURL` reports it.
    ///
    /// A console this emulator opened a socket for ([`super::broker_console`]) is published
    /// at that socket's own address, which is where the console really answers; a broker
    /// whose console socket could not be opened names the host it is served on and the port
    /// Amazon MQ publishes that engine's console on.
    fn console_url(&self) -> String {
        let authority = match &self.console_endpoint {
            Some(console) => ::service_common::data_plane_addressing::host_port_authority(
                &console.host,
                console.port,
            ),
            None => ::service_common::data_plane_addressing::host_port_authority(
                self.authority_host(),
                self.console_port(),
            ),
        };
        format!("{CONSOLE_SCHEME}://{authority}")
    }

    /// The instances allocated to this broker as of `now`: one once the clock has made
    /// the broker's move to `RUNNING` due, and none while it is still being
    /// provisioned — an instance that is not allocated yet publishes nothing.
    ///
    /// Also none for a broker whose record carries no published host, which is the one
    /// case there is no address to report rather than a made-up one.
    pub fn allocated_instances(&self, now: SystemTime) -> Vec<BrokerInstanceAddress> {
        if self.broker_state(now) != BROKER_STATE_RUNNING || self.published_host.is_empty() {
            return Vec::new();
        }
        vec![BrokerInstanceAddress {
            console_url: self.console_url(),
            endpoints: self
                .wire_protocols()
                .iter()
                .map(|protocol| self.endpoint_of(*protocol))
                .collect(),
            ip_address: self.ip_address(),
        }]
    }

    /// The endpoint URI this broker publishes for `protocol`.
    ///
    /// A broker this emulator opened a SOCKET for publishes that socket's own address for
    /// the protocol the socket serves, because an endpoint is an address the broker
    /// answers on; every other endpoint names the host the broker's sockets are bound on
    /// ([`served_authority_host`]) and the port Amazon MQ publishes that protocol on.
    fn endpoint_of(&self, protocol: WireProtocol) -> String {
        match &self.served_endpoint {
            Some(served) if protocol.served => protocol.endpoint(&served.host, served.port),
            _ => protocol.endpoint(self.authority_host(), protocol.port),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the host a broker publishes an address under is a host THIS EMULATOR answers
    /// on — the one the request that created it reached the emulator at, and the interface
    /// the core binds on where the request named no host at all — never a name composed from
    /// the broker's id and a DNS zone.
    #[test]
    fn a_brokers_published_host_is_one_this_emulator_answers_on() {
        // The rows span the authority forms a request reaches this emulator in (an IPv4
        // address with and without a port, a bracketed IPv6 literal, a name an operator
        // points clients at, a name with userinfo in front of it) and the values that name
        // no host at all: the empty authority, and one carrying characters no host:port
        // carries.
        let rows: &[(&str, &str, &str)] = &[
            ("an emulator reached at an address", "127.0.0.1:9297", "127.0.0.1"),
            ("an address with no port", "127.0.0.1", "127.0.0.1"),
            ("a bracketed IPv6 literal", "[::1]:9297", "[::1]"),
            ("an emulator reached at a name", "emulator:9297", "emulator"),
            ("a name carrying userinfo", "user@emulator:9297", "emulator"),
            ("an authority naming no host", "", "127.0.0.1"),
            ("an authority no host:port could be", "not a host", "127.0.0.1"),
        ];
        for (case, authority, expected) in rows {
            let host = published_broker_host(authority);
            assert_eq!(
                &host, expected,
                "case {case:?}: expected the host {expected:?}, got {host:?}"
            );
        }
    }

    /// RULE: the address a broker reports for its own interface is the address of the
    /// interface this emulator serves that broker on — the host its own sockets are bound on
    /// where that host is an address, then the host a client reached this emulator at, and
    /// the interface the core binds on where neither is an address — and is always an IP
    /// address of one interface, never a name, a bracketed literal or the unspecified
    /// address.
    #[test]
    fn a_brokers_reported_address_is_the_interface_it_is_served_on() {
        // The rows are `[the host the broker's sockets are bound on, the host this emulator
        // was reached at]`, and span what the reading has to work from: a socket on an IPv4
        // and on an IPv6 interface, a broker with no socket reached at an address, at a
        // bracketed literal and at a name, and the unspecified address, which names every
        // interface and so names none.
        let rows: &[(&str, &[&str], &str)] = &[
            ("a socket on the loopback interface", &["127.0.0.1", "emulator"], "127.0.0.1"),
            ("a socket on an IPv6 interface", &["[::1]", "emulator"], "::1"),
            ("a socket on another interface", &["10.0.1.5", "127.0.0.1"], "10.0.1.5"),
            ("no socket, reached at an address", &["10.0.1.5", "10.0.1.5"], "10.0.1.5"),
            ("no socket, reached at a bracketed literal", &["[::1]", "[::1]"], "::1"),
            ("no socket, reached at a name", &["emulator", "emulator"], "127.0.0.1"),
            ("a socket on the unspecified address", &["0.0.0.0", "10.0.1.5"], "10.0.1.5"),
            ("nothing but the unspecified address", &["0.0.0.0", "0.0.0.0"], "127.0.0.1"),
            ("no host at all", &[], "127.0.0.1"),
        ];
        for (case, hosts, expected) in rows {
            let reported = served_interface_address(hosts);
            assert_eq!(
                &reported, expected,
                "case {case:?}: expected the address {expected:?}, got {reported:?}"
            );
            let address = reported
                .parse::<::std::net::IpAddr>()
                .unwrap_or_else(|_| panic!("case {case:?}: expected an IP address, got {reported:?}"));
            assert!(
                !address.is_unspecified(),
                "case {case:?}: expected the address of one interface, got {reported:?}"
            );
        }
    }

    /// RULE: every address a broker publishes names the host its own sockets are bound on,
    /// and the host this emulator was reached at only where it has no socket of its own.
    #[test]
    fn a_broker_publishes_the_host_its_sockets_are_bound_on() {
        let socket = |host: &str| ServedAddress {
            host: host.to_string(),
            port: 41_234,
        };
        // The rows span which sockets a broker has: its wire and its console, the console
        // alone (an engine whose wire this emulator serves none of), and neither.
        let rows: &[(&str, Option<ServedAddress>, Option<ServedAddress>, &str)] = &[
            (
                "a wire socket and a console",
                Some(socket("127.0.0.1")),
                Some(socket("127.0.0.1")),
                "127.0.0.1",
            ),
            ("a console socket only", None, Some(socket("127.0.0.2")), "127.0.0.2"),
            ("no socket of its own", None, None, "emulator"),
        ];
        for (case, served, console, expected) in rows {
            let host = served_authority_host(served.as_ref(), console.as_ref(), "emulator");
            assert_eq!(
                host, *expected,
                "case {case:?}: expected the host {expected:?}, got {host:?}"
            );
        }
    }
}

