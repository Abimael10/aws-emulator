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
//! The ADDRESS they name is the BROKER's OWN: Amazon MQ publishes a broker's console and
//! its wire endpoints under a host that carries that broker's id and the Region it was
//! created in ([`published_broker_host`]), so no two brokers of one account advertise the
//! same authority and the broker an address belongs to is recoverable from the address.
//! The host is composed ONCE, by the create, and kept on the record
//! ([`Broker::published_host`]), so every later read of the broker publishes the one host.
//!
//! The ZONE that host sits in is the one this emulator may announce a name of its own in
//! ([`::service_common::data_plane_addressing::announced_zone_of_authority`]), never the
//! zone AWS serves its own names in: a name a service hands a client is a name the client
//! RESOLVES and then dials, so it has to lead back to this process, and the zone of an
//! emulator reached at an ADDRESS is the one a resolver answers with the loopback
//! interface — the interface the core binds its listeners on unless the operator names
//! another.
//!
//! A broker whose engine this emulator serves a WIRE for is given a socket of the
//! emulator's own ([`serve_broker`]), and the endpoint published for the protocol that
//! socket serves is that socket's own dialable address: an endpoint is an address the
//! broker ANSWERS on, and which interface and port it answered on is the core's to assign.
//! Every other endpoint names the broker's own host and the port Amazon MQ publishes that
//! protocol on.

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
    /// The endpoint URI this protocol is reached at on `host`, in the
    /// `<scheme>://<host>:<port>` form Amazon MQ publishes an endpoint as.
    fn endpoint(self, host: &str) -> String {
        format!("{}://{host}:{}", self.scheme, self.port)
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
    // STOMP.
    WireProtocol {
        scheme: "stomp+ssl",
        port: 61_614,
        served: false,
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

/// The label every Amazon MQ broker host carries between the broker's own label and the
/// Region: this service's `endpointPrefix` (`smithy_query(query="service_info")`), which is
/// what names Amazon MQ in the host of every broker it publishes.
const BROKER_HOST_SERVICE_LABEL: &str = "mq";

/// The ordinal that follows a broker's id in the host of its FIRST instance — the only one
/// a `SINGLE_INSTANCE` broker has — which is how Amazon MQ numbers the instances of one
/// broker in their host names.
const FIRST_INSTANCE_ORDINAL: u8 = 1;

/// The host a broker PUBLISHES its console and its wire endpoints under:
/// `<brokerId>-<ordinal>.<service>.<region>.<zone>`.
///
/// Every label of it but the zone comes from the BROKER: its own minted id and instance
/// ordinal, the label that names this service and the Region the create was signed for. So
/// the published authority identifies the resource — two brokers never share one, and a
/// client correlating a connection with its control-plane broker, or building a
/// Region-aware client from the endpoint, reads both back out of the host — which is what
/// `BrokerInstance$endpoints` ("The broker's wire-level protocol endpoints") and
/// `consoleURL` ("The brokers web console URL") carry in Amazon MQ.
///
/// The ZONE is the one this emulator may announce a name of its own in, read off the
/// `host[:port]` a request reached it at through the workspace's one reading of that
/// question: the loopback zone where that address is an IP address rather than a name (see
/// the module docs). The Region is CALLER-SUPPLIED text that ends up in a host here, so it
/// is taken only as a Region LABEL (`service_common::arn::region_label`, this tree's one
/// statement of that grammar) and anything else falls back to [`DEFAULT_REGION`]: a value
/// carrying a host or path delimiter would name an authority the caller chose.
pub fn published_broker_host(reached_authority: &str, broker_id: &str, region: &str) -> String {
    let region = ::service_common::arn::region_label(region).unwrap_or(DEFAULT_REGION);
    let zone =
        ::service_common::data_plane_addressing::announced_zone_of_authority(reached_authority);
    format!("{broker_id}-{FIRST_INSTANCE_ORDINAL}.{BROKER_HOST_SERVICE_LABEL}.{region}.{zone}")
}

/// The first octet of the block a broker's interface address is drawn from: `10.0.0.0/8`,
/// the largest range RFC 1918 reserves for private networks and the space a VPC subnet's
/// addresses are allocated out of.
const BROKER_INTERFACE_BLOCK: u8 = 10;

/// How many of a `/24`'s addresses an interface can hold: its 256 less the network and
/// broadcast addresses, which name no interface.
const ASSIGNABLE_HOSTS: u8 = 254;

/// The address of the elastic network interface attached to the broker `broker_id` names, as
/// `BrokerInstance$ipAddress` reports it.
///
/// Amazon MQ attaches that interface in one of the BROKER's own subnets, so the address is
/// the one that VPC assigned to it — never an address of the host the control plane is
/// answering on, which belongs to no broker. This emulator provisions no interface, so the
/// address is DERIVED from the broker's own minted id, out of the private block above:
/// distinct per broker, stable across every read of it, and never a network or broadcast
/// host byte. It is derived the same way every other value this service assigns a broker
/// from its id is ([`assigned_maintenance_window`]).
pub fn broker_interface_ipv4(broker_id: &str) -> String {
    let digest = <::sha2::Sha256 as ::sha2::Digest>::digest(broker_id.as_bytes());
    ::std::net::Ipv4Addr::new(
        BROKER_INTERFACE_BLOCK,
        digest[0],
        digest[1],
        digest[2] % ASSIGNABLE_HOSTS + 1,
    )
    .to_string()
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

    /// The broker's own IP address, as `BrokerInstance$ipAddress` reports it ("The IP
    /// address of the Elastic Network Interface (ENI) attached to the broker. Does not
    /// apply to RabbitMQ brokers").
    ///
    /// It is the address of the interface attached to THIS broker
    /// ([`broker_interface_ipv4`]), so it identifies the broker rather than the host the
    /// control plane answered on; a RabbitMQ broker reports none, the member not applying
    /// to that engine.
    fn ip_address(&self) -> Option<String> {
        (self.engine_type != ENGINE_TYPE_RABBITMQ)
            .then(|| broker_interface_ipv4(&self.broker_id))
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
        let host = self.published_host.as_str();
        vec![BrokerInstanceAddress {
            console_url: format!("{CONSOLE_SCHEME}://{host}:{}", self.console_port()),
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
    /// answers on; every other endpoint names the host the broker is published at and the
    /// port Amazon MQ publishes that protocol on.
    fn endpoint_of(&self, protocol: WireProtocol) -> String {
        match &self.served_amqps {
            Some(served) if protocol.served => {
                format!("{}://{}:{}", protocol.scheme, served.host, served.port)
            }
            _ => protocol.endpoint(&self.published_host),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a broker's published host names the BROKER — its own id and instance ordinal,
    /// this service's label and the Region it was created in — in the zone this emulator
    /// may announce a name of its own in, which is the loopback zone wherever the emulator
    /// is reached at an address rather than a name.
    #[test]
    fn a_brokers_published_host_names_the_broker_in_the_announced_zone() {
        // The rows span what the composition reads: the authority forms a client reaches
        // this emulator on (an IPv4 address with and without a port, a bracketed IPv6
        // literal, a name, a host naming one of AWS's own endpoints, which is not this
        // emulator's zone to announce in) and the Region values a request can sign for,
        // including one that is no Region label at all.
        let rows: &[(&str, &str, &str, &str, &str)] = &[
            (
                "an emulator reached at an address",
                "127.0.0.1:9297",
                "b-0123456789abcdef0123456789abcdef",
                "us-west-2",
                "b-0123456789abcdef0123456789abcdef-1.mq.us-west-2.localhost",
            ),
            (
                "an address with no port",
                "127.0.0.1",
                "b-1",
                "eu-central-1",
                "b-1-1.mq.eu-central-1.localhost",
            ),
            (
                "a bracketed IPv6 literal",
                "[::1]:9297",
                "b-1",
                "ap-northeast-1",
                "b-1-1.mq.ap-northeast-1.localhost",
            ),
            (
                "an emulator reached at a name",
                "mq.emulator.test:9297",
                "b-2",
                "us-east-2",
                "b-2-1.mq.us-east-2.mq.emulator.test",
            ),
            (
                "a host naming one of AWS's own endpoints",
                "mq.us-west-2.amazonaws.com",
                "b-3",
                "us-west-2",
                "b-3-1.mq.us-west-2.localhost",
            ),
            (
                "a region that is no Region label",
                "127.0.0.1:9297",
                "b-4",
                "us-west-2/../somewhere",
                "b-4-1.mq.us-east-1.localhost",
            ),
        ];
        for (case, authority, broker_id, region, expected) in rows {
            let host = published_broker_host(authority, broker_id, region);
            assert_eq!(
                &host, expected,
                "case {case:?}: expected the host {expected:?}, got {host:?}"
            );
        }
    }

    /// RULE: the address a broker reports for its own interface is a private address
    /// derived from that broker's id — one address per broker, the same on every read, no
    /// network or broadcast host byte, and never the loopback address.
    #[test]
    fn a_brokers_interface_address_is_private_stable_and_its_own() {
        // The rows span the ids a broker record can carry: ids this service mints, and the
        // shortest and longest values a record could hold.
        let ids: &[&str] = &[
            "b-0123456789abcdef0123456789abcdef",
            "b-fedcba9876543210fedcba9876543210",
            "b-d2783693-0791-2384-7a71-ae0ae15403f9",
            "b-1",
            "b",
        ];
        let mut seen: Vec<String> = Vec::new();
        for id in ids {
            let reported = broker_interface_ipv4(id);
            let address: ::std::net::Ipv4Addr = reported.parse().unwrap_or_else(|_| {
                panic!("case {id:?}: expected an IPv4 address, got {reported:?}")
            });
            assert!(
                address.is_private() && !address.is_loopback(),
                "case {id:?}: expected a private address that is not the loopback one, got \
                 {reported:?}"
            );
            let host_byte = address.octets()[3];
            assert!(
                (1..=ASSIGNABLE_HOSTS).contains(&host_byte),
                "case {id:?}: expected an assignable host byte, got {reported:?}"
            );
            assert_eq!(
                broker_interface_ipv4(id),
                reported,
                "case {id:?}: the same broker must always report the same address"
            );
            assert!(
                !seen.contains(&reported),
                "case {id:?}: two brokers must not report the same address, got {reported:?} \
                 again among {seen:?}"
            );
            seen.push(reported);
        }
    }
}
