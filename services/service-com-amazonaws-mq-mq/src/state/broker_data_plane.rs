// SPDX-License-Identifier: CC0-1.0

//! The DATA PLANE of an Amazon MQ broker: the socket its clients connect to, the message
//! broker behind that socket, and the address the control plane publishes for it.
//!
//! RULE: an endpoint a service advertises for a wire protocol is an address the service
//! ANSWERS that protocol on. A name under Amazon MQ's own DNS zone resolves to nothing on
//! the machine this emulator runs on, so a broker that reported one published an address
//! no client could reach — the one thing a published endpoint must never be. So every
//! broker whose engine speaks a wire this emulator serves is given a socket of the
//! EMULATOR's own, opened through the core (`engine.listeners().tcp`, which owns the
//! socket, the bind interface, the port range, the TLS and the connection caps), and the
//! address that socket answers on is what the broker's one `BrokerInstance` publishes for
//! that protocol.
//!
//! Which wire an engine is served is [`ServedWire`]: a RabbitMQ broker's clients speak
//! AMQP 0-9-1 ([`super::rabbitmq_plane::amqp_session`]) and an ActiveMQ broker's clients
//! speak STOMP ([`super::activemq_plane::stomp_session`]). Both wires are TLS from the
//! first byte — an `amqps://` and a `stomp+ssl://` endpoint both are — which is exactly
//! what the core's `tcp` listener serves, and both answer out of the one store of
//! destinations, queues and messages a broker holds
//! ([`super::rabbitmq_plane::message_broker`]).
//!
//! The socket's life is the broker's: the handle lives on the store under the broker's own
//! id, and dropping it (the delete) closes the port and severs every connection on it.
//!
//! An ActiveMQ broker's OTHER published protocols (OpenWire, AMQP 1.0, MQTT and the
//! WebSocket transport) are not answered here, so such a broker keeps publishing the ports
//! Amazon MQ publishes for them under the host this emulator answers on rather than an
//! address that would answer the wrong protocol.
//!
//! The broker's WEB CONSOLE is a socket of its own, opened here beside the wire and served
//! by [`super::broker_console`]: `consoleURL` is an address an operator opens, so it is the
//! address of a socket this emulator answers an HTTP request at.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::rabbitmq_plane::message_broker::MessageBroker;
use super::*;

/// The address a broker's own socket answers on, as the control plane publishes it.
///
/// The host and the port come from the CORE listener, never from a `format!` of a service
/// hostname: it is the one address a client on this machine can reach the broker at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedAddress {
    pub host: String,
    pub port: u16,
}

/// The address the CORE says `listener` is dialable at, read through the one shared reading
/// of it: a wildcard bind has no single address to dial and is published as loopback, and an
/// IPv6 bind keeps its brackets.
///
/// Every socket this service opens for a broker — its wire and its web console
/// ([`super::broker_console`]) — publishes its address through this one reading, so the two
/// cannot drift on what a client is told to dial.
pub fn served_address_of(listener: &::emulator_server::ResourceListener) -> ServedAddress {
    let authority = ::service_common::data_plane_addressing::listener_authority(listener);
    let host = ::service_common::data_plane_addressing::host_only(&authority).to_string();
    let port = ::service_common::data_plane_addressing::addressed_port(Some(&authority))
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or_else(|| listener.port());
    ServedAddress { host, port }
}

/// One broker's destinations and queues, shared between the store's record and every
/// session on the broker's socket.
///
/// A newtype rather than a bare `Arc<Mutex<_>>` so locking recovers from a poisoned lock
/// the way the engine's own state handle does: one session that panicked must not wedge
/// the broker for every later client.
#[derive(Debug, Clone, Default)]
pub struct SharedMessageBroker(Arc<Mutex<MessageBroker>>);

impl SharedMessageBroker {
    /// Lock the broker for exclusive access.
    pub fn lock(&self) -> MutexGuard<'_, MessageBroker> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What every session on one broker's socket is served from, whichever wire that socket
/// speaks.
#[derive(Clone)]
pub struct BrokerSocket {
    /// The control plane's store, so a connection authenticates against the broker's
    /// users as they are NOW — a user created or deleted while a session is open is in
    /// force for the next connection.
    pub state: ::engine::ServiceState<ComAmazonawsMqMqState>,
    /// Which broker this socket serves.
    pub broker_id: String,
    /// That broker's destinations and queues, shared with every other session on it.
    pub broker: SharedMessageBroker,
}

/// One broker's data plane, as the store holds it: where it answers, what it holds, and
/// the socket's handle.
///
/// Dropping it closes the port, so it is held for exactly as long as the broker it
/// belongs to.
pub struct BrokerDataPlane {
    endpoint: ServedAddress,
    broker: SharedMessageBroker,
    /// The socket itself. Never read: it is held because dropping it closes the port.
    _listener: ::emulator_server::ResourceListener,
}

impl std::fmt::Debug for BrokerDataPlane {
    /// Everything about this record that is not a live socket: where it answers and what
    /// its broker holds. `ResourceListener` is not `Debug`.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrokerDataPlane")
            .field("endpoint", &self.endpoint)
            .field("broker", &self.broker)
            .finish_non_exhaustive()
    }
}

impl BrokerDataPlane {
    /// The address a client reaches this broker at.
    pub fn endpoint(&self) -> &ServedAddress {
        &self.endpoint
    }

    /// The broker's destinations and queues.
    pub fn broker(&self) -> &SharedMessageBroker {
        &self.broker
    }
}

/// The wire protocol this emulator opens a broker's own socket for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServedWire {
    /// AMQP 0-9-1, which is what a RabbitMQ broker's clients speak.
    Amqp091,
    /// STOMP, one of the protocols an ActiveMQ broker's clients speak.
    Stomp,
}

/// The wire this emulator serves a broker running `engine_type`, or `None` for an engine
/// whose protocols this emulator answers none of — such a broker gets no socket of its
/// own and keeps publishing the ports Amazon MQ publishes for its protocols.
pub fn served_wire(engine_type: &str) -> Option<ServedWire> {
    match engine_type {
        ENGINE_TYPE_RABBITMQ => Some(ServedWire::Amqp091),
        ENGINE_TYPE_ACTIVEMQ => Some(ServedWire::Stomp),
        _ => None,
    }
}

/// Open the sockets a broker is served on — its web console, and the wire its engine's
/// clients speak — and publish their addresses on the broker's record, so the console URL
/// and the endpoint its `BrokerInstance` reports for that wire are addresses a client can
/// reach.
///
/// Called by the create once the record is stored. Does nothing for a broker that already
/// has its sockets or on an engine that vends no listeners — in which case the broker
/// publishes the ports Amazon MQ publishes for its console and its protocols on the host
/// this emulator answered the create on.
pub async fn serve_broker<Eng: Send + Sync + 'static>(
    engine: &Eng,
    state: &::engine::ServiceState<ComAmazonawsMqMqState>,
    broker_id: &str,
) {
    let wire = {
        let store = state.lock();
        let Some(broker) = store.brokers.get(broker_id) else {
            return;
        };
        if store.broker_data_planes.contains_key(broker_id)
            || store.broker_consoles.contains_key(broker_id)
        {
            return;
        }
        served_wire(&broker.engine_type)
    };
    let Some(shared) = (engine as &dyn ::std::any::Any).downcast_ref::<::engine::InMemoryEngine>()
    else {
        ::tracing::warn!(
            broker_id,
            "this engine vends no listeners, so the broker publishes the ports Amazon MQ \
             publishes for its console and its protocols rather than sockets of this \
             emulator's own"
        );
        return;
    };
    let listeners = shared.listeners();
    let message_broker = SharedMessageBroker::default();

    // The console first: it is served for every broker, whichever wire its engine speaks.
    match super::broker_console::open_broker_console(&listeners, state, broker_id).await {
        Some(console) => {
            let mut store = state.lock();
            // A broker DELETED while the port was being bound is a broker whose console
            // nothing will ever close again, so the handle is not stored at all: dropping
            // it as this arm ends closes the port.
            let published = match store.brokers.get_mut(broker_id) {
                Some(record) => {
                    record.console_endpoint = Some(console.endpoint().clone());
                    true
                }
                None => false,
            };
            if published {
                store.broker_consoles.insert(broker_id.to_string(), console);
            }
        }
        None => ::tracing::warn!(
            broker_id,
            "no socket could be opened for this broker's web console, so it publishes the \
             port Amazon MQ publishes that console on rather than a socket of this \
             emulator's own"
        ),
    }

    let Some(wire) = wire else {
        return;
    };
    let socket = BrokerSocket {
        state: state.clone(),
        broker_id: broker_id.to_string(),
        broker: message_broker.clone(),
    };
    // Opened on a task of its OWN, so a runtime that cannot bind a socket at all — one
    // built with no IO driver, which is what a host driving this emulator in process may
    // hand a request — leaves this create without an address of its own instead of taking
    // the request down with it.
    //
    // Which server answers is the broker's ENGINE's wire; the socket, its TLS and its caps
    // are the core's either way.
    let opened = match wire {
        ServedWire::Amqp091 => {
            let handler = super::rabbitmq_plane::amqp_session::AmqpWire::new(socket);
            ::tokio::spawn(async move { listeners.tcp(handler).await.ok() }).await
        }
        ServedWire::Stomp => {
            let handler = super::activemq_plane::stomp_session::StompWire::new(socket);
            ::tokio::spawn(async move { listeners.tcp(handler).await.ok() }).await
        }
    };
    let Some(listener) = opened.ok().flatten() else {
        ::tracing::warn!(
            broker_id,
            "no socket could be opened for this broker, so it publishes the port Amazon MQ \
             publishes for its protocol rather than a socket of this emulator's own"
        );
        return;
    };
    let endpoint = served_address_of(&listener);
    let mut store = state.lock();
    // As for the console above: a broker deleted while the port was being bound takes no
    // handle, so no socket outlives the record it belongs to.
    let published = match store.brokers.get_mut(broker_id) {
        Some(record) => {
            record.served_endpoint = Some(endpoint.clone());
            true
        }
        None => false,
    };
    if !published {
        return;
    }
    store.broker_data_planes.insert(
        broker_id.to_string(),
        BrokerDataPlane {
            endpoint,
            broker: message_broker,
            _listener: listener,
        },
    );
}
