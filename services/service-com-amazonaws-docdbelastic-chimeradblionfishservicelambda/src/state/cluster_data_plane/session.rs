// SPDX-License-Identifier: CC0-1.0

//! What ONE connection on an elastic cluster's endpoint holds while it is open.
//!
//! An elastic cluster runs the SASL exchange itself, because it offers one mechanism and words
//! its refusals as its own engine does ([`super::commands`]), so the identity of a connection
//! lives here: who the session has authenticated as, and the exchange it is in the middle of.
//! Everything a document command needs of a connection — the cursors it holds — is the shared
//! command layer's own session, which is built only once this connection has authenticated, so
//! no command of that layer can run for a session that presented nothing.

use ::service_com_amazonaws_docdb_amazonrdsv19::state::cluster_data_plane as document_engine;

use document_engine::auth::ScramExchange;
use document_engine::commands::Session as EngineSession;

/// One connection on a cluster's socket.
#[derive(Debug)]
pub struct ClusterSession {
    /// The id this connection reports as its `connectionId`.
    pub(super) connection_id: i64,
    /// Who the session has authenticated as, or `None` for one that has not.
    pub(super) authenticated: Option<String>,
    /// The SASL exchange in progress, if any.
    pub(super) exchange: Option<ScramExchange>,
    /// Whether the client asked, when it began the exchange, for the closing empty message to
    /// be skipped (`options.skipEmptyExchange`), which is what a driver asks for.
    pub(super) skip_empty_exchange: bool,
    /// Whether this connection is ENDING once the reply being built is out: an exchange that
    /// failed to authenticate takes the connection with it, so a client does not get to keep
    /// presenting credentials on one connection
    /// ([`::service_com_amazonaws_docdb_amazonrdsv19::state::cluster_data_plane::wire::WireEndpoint::carries_on`]).
    pub(super) closing: bool,
    /// The command layer's session for this connection, built when the exchange holds: a
    /// session that has not authenticated reaches no document command at all.
    pub(super) served: Option<EngineSession>,
}

impl ClusterSession {
    /// A fresh session on a connection whose `connectionId` is `connection_id`.
    #[must_use]
    pub fn new(connection_id: i64) -> Self {
        Self {
            connection_id,
            authenticated: None,
            exchange: None,
            skip_empty_exchange: false,
            closing: false,
            served: None,
        }
    }

    /// Record that this connection authenticated as `username`, and open the command layer's
    /// session for it.
    pub(super) fn admit(&mut self, username: &str) {
        self.authenticated = Some(username.to_string());
        self.served = Some(EngineSession::authenticated_as(
            self.connection_id,
            username,
        ));
    }

    /// Forget who this connection is, which `logout` does: the cursors of the session it held
    /// go with it, and the next document command meets the same refusal an unauthenticated
    /// connection does.
    pub(super) fn forget(&mut self) {
        self.authenticated = None;
        self.exchange = None;
        self.served = None;
    }

    /// Whether this connection has authenticated.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        self.authenticated.is_some()
    }

    /// Whether the endpoint carries this connection on once the reply being built is out.
    #[must_use]
    pub fn carries_on(&self) -> bool {
        !self.closing
    }
}
