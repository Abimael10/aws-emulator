// SPDX-License-Identifier: CC0-1.0

//! The wire an Amazon MQ for RABBITMQ broker's clients speak: AMQP 0-9-1 over the socket
//! the broker is served on, and the exchanges, queues and messages that wire answers out
//! of.
//!
//! The socket itself, the address the control plane publishes for it and which engine is
//! served which wire are [`super::broker_data_plane`]'s; this module is the RabbitMQ half
//! of it: the AMQP 0-9-1 server one connection is served by ([`amqp_session`]) and the
//! message broker behind it ([`message_broker`]).
//!
//! The message broker is not RabbitMQ-only — it is the one store of destinations, queues
//! and messages every broker of this service holds, which the ActiveMQ wire
//! ([`super::activemq_plane`]) answers out of too — and it lives here because AMQP
//! 0-9-1's own exchange, binding and dead-letter semantics are what it implements.

/// The AMQP 0-9-1 server one connection is served by.
pub mod amqp_session;
/// The exchanges, queues and messages that server answers out of.
pub mod message_broker;
