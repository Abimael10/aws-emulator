// SPDX-License-Identifier: CC0-1.0

//! The TRANSPORT of a portal's website: the core-owned socket each `Portal` of this account
//! is reached at.
//!
//! `EndpointConfigurationResponse$PortalDefaultDomainName` is the address a CONSUMER of a
//! developer portal fetches — "this domain name is generated and managed by API Gateway" —
//! so a client that reads it off `CreatePortal` or `GetPortal` and asks for its root expects
//! the portal's site. A name under API Gateway's own zone resolves to nothing on the machine
//! this emulator runs on, so a portal that published one published an address no client could
//! reach, and everything publishing a portal exists for — the site being served on the
//! address the service itself reported, and stopping on `DisablePortal` — was unobservable.
//!
//! So each portal gets a socket of the EMULATOR's own, opened through the core
//! (`engine.listeners()`, which owns the socket, the bind interface, the port range and the
//! connection caps), and the record publishes the `host:port` that socket answers on — the
//! same practice a custom domain name follows for its own published address
//! ([`super::domain_name_transport`]). A client dials exactly what it was told.
//!
//! The socket is the ADDRESS, not the data plane: what a request it accepts is answered BY —
//! the build of the portal's last settled publish, its preview, or the refusal a portal
//! serving nothing earns — stays in [`crate::service::portal`], which serves the same request
//! however it was addressed ([`crate::service::portal_socket`]).
//!
//! The socket's life is the portal's: the handle is held on this service's own store under
//! the record's own key ([`super::listener_registry`], the one registry every kind of
//! core-owned socket of this service is kept in), and dropping it (a delete) closes the port
//! and severs every connection in flight on it.

/// The `host:port` a core listener is addressed by — re-exported from the one registry every
/// kind of socket of this service shares, so a caller of this module reads an address the
/// same way the custom domain name's transport does.
pub use super::listener_registry::authority_of;

/// The listener each portal of this account is reached at, keyed by the `PortalId` the record
/// is stored under.
///
/// A field of [`crate::state::ComAmazonawsApigatewayv2Apigatewayv2State`] rather than a store
/// of its own, for the reason the custom domain name's transport is one
/// ([`super::domain_name_transport`]): the records that say which portals exist and the
/// sockets those portals are reached at are read under ONE lock and cannot drift apart.
pub type PortalTransport = super::listener_registry::ListenerRegistry;
