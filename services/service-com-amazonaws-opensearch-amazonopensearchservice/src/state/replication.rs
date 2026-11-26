// SPDX-License-Identifier: CC0-1.0

//! A domain's CROSS-CLUSTER REPLICATION arrangement, as this emulator persists it: which
//! peer domains it replicates with, which role it plays with them, and the one wording this
//! service refuses a call on a domain that replicates with nobody.
//!
//! RECORDED BEHAVIOUR ONLY: the on-disk model describes none of the calls that read or write
//! this record (`smithy_query(query="operation",
//! operation="AddReplicationPeer,RemoveReplicationPeer,FailoverReplication,SwitchoverReplication,DescribeDomainReplication")`
//! answers "not found" for every one of them), so what a peer and a role ARE here is the
//! vocabulary those calls answer with — a domain's `Peers` and its `Role` — and nothing
//! beyond it is invented.
//!
//! The record lives ON the [`Domain`] it belongs to, for the reason its VPC endpoint
//! authorizations do: an arrangement is a property of the domain, so releasing the domain
//! releases it. BOTH sides of one arrangement are written by the one call that establishes
//! it ([`crate::service::unmodelled_routes::replication`]), so a read of either domain
//! reports the same arrangement.

use super::*;

/// The role of the domain an arrangement replicates FROM.
pub const REPLICATION_ROLE_LEADER: &str = "LEADER";

/// The role of the domain an arrangement replicates TO — the side that asked for the
/// arrangement, since replication is started on the domain that follows a peer.
pub const REPLICATION_ROLE_FOLLOWER: &str = "FOLLOWER";

/// The other side of `role`: the role the peer of a domain playing `role` plays.
///
/// An arrangement has exactly two sides, so this is a total function over the two roles
/// above; anything else is a role this service never writes and is paired with the leader,
/// the side a stored peer of a following domain is on.
#[must_use]
pub fn counterpart_role(role: &str) -> &'static str {
    if role == REPLICATION_ROLE_LEADER {
        REPLICATION_ROLE_FOLLOWER
    } else {
        REPLICATION_ROLE_LEADER
    }
}

/// One peer of a domain's replication arrangement.
#[derive(Debug, Clone)]
pub struct ReplicationPeer {
    /// The ARN of the peer domain — a domain of this store, which is what the call that
    /// establishes the arrangement resolves before writing either side of it.
    pub peer_domain_arn: String,
    /// The role the PEER plays, which is the other side of the role the holding domain
    /// plays ([`counterpart_role`]).
    pub peer_role: String,
}

/// A domain's whole replication arrangement: the peers it replicates with, and the role it
/// plays with them.
#[derive(Debug, Clone, Default)]
pub struct DomainReplication {
    /// The role this domain plays, and `None` while it replicates with nobody — which is
    /// why a read of such a domain reports no role at all.
    pub role: Option<String>,
    /// The peers, keyed by the peer domain's ARN: a domain holds at most one arrangement
    /// per peer, so the ARN IS the key. Every peer is a domain of this store, so
    /// [`DOMAIN_QUOTA`] bounds the set. Iterating a `BTreeMap` is order-stable, which is
    /// what lets two reads report one domain's peers in the same order.
    pub peers: BTreeMap<String, ReplicationPeer>,
}

impl DomainReplication {
    /// Whether this domain replicates with anybody at all — the condition the replication
    /// calls that act on an existing arrangement refuse on ([`domain_not_replicating`]).
    #[must_use]
    pub fn replicating(&self) -> bool {
        !self.peers.is_empty()
    }

    /// Whether this domain replicates with the peer `peer_domain_arn` names.
    #[must_use]
    pub fn replicates_with(&self, peer_domain_arn: &str) -> bool {
        self.peers.contains_key(peer_domain_arn)
    }

    /// Record an arrangement with the peer `peer_domain_arn` names, this domain playing
    /// `role` and the peer the other side of it.
    ///
    /// Idempotent in the peer: a repeat for one peer overwrites that peer's entry rather
    /// than holding two arrangements with one domain.
    pub fn establish(&mut self, peer_domain_arn: &str, role: &str) {
        self.role = Some(role.to_string());
        self.peers.insert(
            peer_domain_arn.to_string(),
            ReplicationPeer {
                peer_domain_arn: peer_domain_arn.to_string(),
                peer_role: counterpart_role(role).to_string(),
            },
        );
    }

    /// Release the arrangement with the peer `peer_domain_arn` names, and answer whether
    /// one was held.
    ///
    /// A domain left with no peer plays no role, so the role goes with the last peer: that
    /// is what makes a domain whose arrangement was torn down read exactly like one that
    /// never had one.
    pub fn release(&mut self, peer_domain_arn: &str) -> bool {
        let released = self.peers.remove(peer_domain_arn).is_some();
        if self.peers.is_empty() {
            self.role = None;
        }
        released
    }

    /// Give this domain `role`, and every peer it holds the other side of it — the role
    /// move a failover or a switchover performs on the domain it is called on.
    pub fn take_role(&mut self, role: &str) {
        if !self.replicating() {
            return;
        }
        self.role = Some(role.to_string());
        let peer_role = counterpart_role(role);
        for peer in self.peers.values_mut() {
            peer.peer_role = peer_role.to_string();
        }
    }
}

/// What this service says of a domain that replicates with nobody, in whichever envelope
/// the call refusing it answers with: the ONE wording, so two of those calls cannot word
/// the same miss differently.
///
/// The subject is the CALLING domain's own ARN rather than the peer's — the domain the
/// request is addressed to is the one that is not replicating.
#[must_use]
pub fn domain_not_replicating_message(domain_arn: &str) -> String {
    format!("Domain is not replicating: {domain_arn}")
}
