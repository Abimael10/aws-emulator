// SPDX-License-Identifier: CC0-1.0

//! The wire an Amazon MQ for ACTIVEMQ broker's clients speak: STOMP over the socket the
//! broker is served on.
//!
//! The socket itself, the address the control plane publishes for it and which engine is
//! served which wire are [`super::broker_data_plane`]'s; this module is the ActiveMQ half
//! of it: the STOMP frame format ([`stomp_frames`]) and the STOMP server one connection is
//! served by ([`stomp_session`]), which answers out of the same store of destinations,
//! queues and messages every broker of this service holds
//! ([`super::rabbitmq_plane::message_broker`]).
//!
//! ActiveMQ's other published protocols (OpenWire, AMQP 1.0, MQTT and the WebSocket
//! transport) are not answered here: a broker publishes the ports Amazon MQ publishes for
//! them under its own host, and only the protocol a socket is opened for publishes that
//! socket's address.

/// The STOMP frame format: the bytes on the wire, in both directions.
pub mod stomp_frames;
/// The STOMP server one connection is served by.
pub mod stomp_session;
