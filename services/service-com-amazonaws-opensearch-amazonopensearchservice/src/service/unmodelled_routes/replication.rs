// SPDX-License-Identifier: CC0-1.0

//! The five calls on ONE domain's cross-cluster replication arrangement:
//! `DescribeDomainReplication`, `AddReplicationPeer`, `RemoveReplicationPeer`,
//! `FailoverReplication` and `SwitchoverReplication`.
//!
//! None of them is in the on-disk model (see [`super`]), so their statuses, codes and
//! messages are RECORDED BEHAVIOUR ONLY, and each route's refusal classes are its own:
//!
//! | The call | A name outside the model's constraints | A domain this account does not hold | A domain that replicates with nobody |
//! |---|---|---|---|
//! | `DescribeDomainReplication` | `400 ValidationException` | `ResourceNotFoundException`, "Domain not found: &lt;arn&gt;" | served — `Peers` empty and no `Role` |
//! | `AddReplicationPeer` | `400 ValidationException` | `400 ValidationException`, "Domain not found: &lt;arn&gt;" | served — that is what it establishes |
//! | `RemoveReplicationPeer` | `400 ValidationException` | `ResourceNotFoundException`, "Domain is not replicating: &lt;arn&gt;" | the same refusal, whether the domain exists or not |
//! | `FailoverReplication` / `SwitchoverReplication` | `400 ValidationException` | `ResourceNotFoundException`, "Domain not found: &lt;arn&gt;" | `400 ValidationException`, "Domain is not replicating: &lt;arn&gt;" |
//!
//! The subject of a message is an ARN composed from the request's own Region and this
//! account ([`crate::state::domain_arn`]) — the domain's own ARN for every refusal about the
//! domain the path names, including the not-replicating one, and the ARN exactly as SENT for
//! a peer the request named.
//!
//! # What they write
//!
//! BOTH sides of an arrangement, in the one call that changes it: a domain asking to
//! replicate from a peer becomes that peer's FOLLOWER and the peer becomes its LEADER, so a
//! read of either domain reports the same arrangement
//! ([`crate::state::replication::DomainReplication`]). `DescribeDomainReplication` writes
//! nothing but the configuration settling every reader of a domain materialises.

use std::time::SystemTime;

use ::serde_json::{json, Value};

use super::{domain_name, required_text, Refusal};
use crate::state::replication::{
    domain_not_replicating_message, DomainReplication, ReplicationPeer, REPLICATION_ROLE_FOLLOWER,
    REPLICATION_ROLE_LEADER,
};
use crate::state::{domain_arn, domain_not_found_message, service_state};

/// The member a call names the peer domain in, as a request spells it.
const PEER_DOMAIN_ARN: &str = "PeerDomainArn";

/// The member `AddReplicationPeer` asks for validation WITHOUT the effect in.
///
/// "it refuses IDENTICALLY — same code, same message — whether DryRun is true or false, and
/// establishes no relationship in either case" is the recorded behaviour, so the flag is
/// read AFTER every refusal this call makes and decides only whether the arrangement is
/// written. Only a boolean sets it; anything else is no flag the request set.
const DRY_RUN: &str = "DryRun";


/// `DescribeDomainReplication`: the arrangement the domain the path names is part of.
///
/// A READ. The only thing it writes is the configuration change the clock already owed the
/// domain at `now` ([`crate::state::ComAmazonawsOpensearchAmazonopensearchserviceState::settled_mut`]),
/// which was due whether or not anybody called — the same materialisation every reader of a
/// domain performs.
pub(super) fn described<Eng: Send + Sync + 'static>(
    engine: &Eng,
    domain_name: &str,
    region: &str,
    now: SystemTime,
) -> Result<Value, Refusal> {
    let arn = addressed(domain_name, region)?;
    let state = service_state(engine).ok_or_else(Refusal::unavailable)?;
    let mut store = state.lock();
    let domain = store
        .settled_mut(domain_name, now)
        .ok_or_else(|| Refusal::not_found(domain_not_found_message(&arn)))?;
    Ok(reported(&domain.replication))
}

/// `AddReplicationPeer`: establish an arrangement between the domain the path names and the
/// peer the body names.
///
/// The order of the checks is the recorded one: the LOCAL domain is resolved first, then the
/// peer, so a call naming neither is told about the domain it was addressed to. Both
/// resolutions refuse with `ValidationException` — this call's own class for a domain that
/// does not exist — and neither depends on the dry-run flag.
pub(super) fn peer_added<Eng: Send + Sync + 'static>(
    engine: &Eng,
    domain_name: &str,
    region: &str,
    asked: &Value,
    now: SystemTime,
) -> Result<Value, Refusal> {
    let arn = addressed(domain_name, region)?;
    let state = service_state(engine).ok_or_else(Refusal::unavailable)?;
    let mut store = state.lock();
    if store.settled_mut(domain_name, now).is_none() {
        return Err(Refusal::validation(domain_not_found_message(&arn)));
    }
    // The peer is named by ARN, and the subject of the refusal is that ARN exactly as the
    // request sent it: the value a caller must correct is the one it wrote.
    let peer_arn = required_text(asked, PEER_DOMAIN_ARN)?;
    let peer_name = match store.domain_of_arn(&peer_arn) {
        Some(peer) => peer.domain_name.clone(),
        None => return Err(Refusal::validation(domain_not_found_message(&peer_arn))),
    };
    if peer_name == domain_name {
        // No recording covers it, and no arrangement can be written for it: an arrangement
        // has two sides, and both of these would be one domain. Refused as the invalid
        // input it is rather than stored as a domain replicating with itself.
        return Err(Refusal::validation(format!(
            "Domain cannot replicate with itself: {arn}"
        )));
    }
    if asked.get(DRY_RUN).and_then(Value::as_bool) == Some(true) {
        // Validation without the effect: the arrangement is reported exactly as the store
        // still holds it, so the answer states what a dry run did — nothing.
        let domain = store
            .settled_mut(domain_name, now)
            .ok_or_else(|| Refusal::validation(domain_not_found_message(&arn)))?;
        return Ok(reported(&domain.replication));
    }
    // Both sides, in one call. The domain that ASKED to replicate from a peer is the
    // follower of that peer, which is the side replication is started on.
    let established = match store.settled_mut(domain_name, now) {
        Some(domain) => {
            domain
                .replication
                .establish(&peer_arn, REPLICATION_ROLE_FOLLOWER);
            reported(&domain.replication)
        }
        None => return Err(Refusal::validation(domain_not_found_message(&arn))),
    };
    if let Some(peer) = store.settled_mut(&peer_name, now) {
        peer.replication.establish(&arn, REPLICATION_ROLE_LEADER);
    }
    Ok(established)
}

/// `RemoveReplicationPeer`: release the arrangement between the domain the path names and
/// the peer the body names.
///
/// The recorded refusal for a domain with no such arrangement is the missing-resource one
/// naming the CALLING domain's own ARN, and it is answered whether or not that domain
/// exists — so this gate is taken before the domain is resolved as a resource at all, which
/// is why a name nobody created earns the not-replicating refusal rather than a not-found
/// one.
pub(super) fn peer_removed<Eng: Send + Sync + 'static>(
    engine: &Eng,
    domain_name: &str,
    region: &str,
    asked: &Value,
    now: SystemTime,
) -> Result<Value, Refusal> {
    let arn = addressed(domain_name, region)?;
    let state = service_state(engine).ok_or_else(Refusal::unavailable)?;
    let mut store = state.lock();
    let not_replicating = || Refusal::not_found(domain_not_replicating_message(&arn));
    if store
        .settled_mut(domain_name, now)
        .is_none_or(|domain| !domain.replication.replicating())
    {
        return Err(not_replicating());
    }
    let peer_arn = required_text(asked, PEER_DOMAIN_ARN)?;
    let released = match store.settled_mut(domain_name, now) {
        // A peer this domain holds no arrangement with is the same class of miss as holding
        // none at all, and the subject stays the calling domain's own ARN: what the refusal
        // states is that THIS domain is not replicating with what the request named.
        Some(domain) if !domain.replication.replicates_with(&peer_arn) => {
            return Err(not_replicating())
        }
        Some(domain) => {
            domain.replication.release(&peer_arn);
            reported(&domain.replication)
        }
        None => return Err(not_replicating()),
    };
    // The other side of the arrangement goes with it, in the same call that released this
    // one — so neither domain is left holding a peer the other no longer knows.
    let peer_name = store
        .domain_of_arn(&peer_arn)
        .map(|peer| peer.domain_name.clone());
    if let Some(peer) = peer_name.and_then(|named| store.settled_mut(&named, now)) {
        peer.replication.release(&arn);
    }
    Ok(released)
}

/// `FailoverReplication` and `SwitchoverReplication`: promote the domain the path names to
/// the leader of its arrangement — the unplanned promotion and the planned one.
///
/// ONE function because the recordings show the two taking the SAME two gates in the same
/// order and leaving the domain in the same place: the domain must EXIST — the
/// missing-resource refusal — and it must then be replicating — this call's
/// `ValidationException`. The role move is the whole effect: the domain becomes the leader
/// and every peer it holds becomes a follower, on both records, so a read of either side
/// reports one arrangement.
///
/// `AllowDataLoss`, the member a failover may carry, admits a promotion that discards writes
/// the follower has not received. It changes nothing here, and that is a property of this
/// emulator's state rather than a member ignored: an arrangement's SIDES are all this
/// service holds — there is no replication backlog — so a promotion has no unreplicated
/// write to discard whichever way the member is set.
pub(super) fn promoted<Eng: Send + Sync + 'static>(
    engine: &Eng,
    domain_name: &str,
    region: &str,
    now: SystemTime,
) -> Result<Value, Refusal> {
    let arn = addressed(domain_name, region)?;
    let state = service_state(engine).ok_or_else(Refusal::unavailable)?;
    let mut store = state.lock();
    let domain = store
        .settled_mut(domain_name, now)
        .ok_or_else(|| Refusal::not_found(domain_not_found_message(&arn)))?;
    if !domain.replication.replicating() {
        return Err(Refusal::validation(domain_not_replicating_message(&arn)));
    }
    domain.replication.take_role(REPLICATION_ROLE_LEADER);
    let promoted = reported(&domain.replication);
    let peers: Vec<String> = peer_names(&store, domain_name);
    for named in peers {
        if let Some(peer) = store.settled_mut(&named, now) {
            peer.replication.take_role(REPLICATION_ROLE_FOLLOWER);
        }
    }
    Ok(promoted)
}

/// The names of the domains this store holds for the peers of `domain_name`'s arrangement.
///
/// Resolved before the other side is written, because each side is borrowed from the store
/// in turn. A peer ARN that resolves to no domain of this store contributes no name: every
/// peer was a domain when the arrangement was established, and one released since has
/// nothing left to write.
fn peer_names(
    store: &crate::state::ComAmazonawsOpensearchAmazonopensearchserviceState,
    domain_name: &str,
) -> Vec<String> {
    let Some(domain) = store.domains.get(domain_name) else {
        return Vec::new();
    };
    domain
        .replication
        .peers
        .keys()
        .filter_map(|arn| store.domain_of_arn(arn))
        .map(|peer| peer.domain_name.clone())
        .collect()
}

/// The ARN of the domain the path names, once that name is one the model admits.
///
/// Every one of these calls takes this step first: the name is held to the constraints the
/// model declares for a `DomainName` ([`domain_name`]), and the ARN it is refused under is
/// composed from the request's own Region and this account rather than from anything the
/// request carried.
fn addressed(named: &str, region: &str) -> Result<String, Refusal> {
    match domain_name::refusal(named) {
        Some(refusal) => Err(refusal),
        None => Ok(domain_arn(named, region)),
    }
}

/// The arrangement as these calls report it: the peers, and the role the domain plays in it.
///
/// `Peers` is present and EMPTY for a domain that replicates with nobody, and such a domain
/// reports NO role at all — the recorded shape of the read, and the reason the role is
/// `Option` on the record.
fn reported(replication: &DomainReplication) -> Value {
    let peers: Vec<Value> = replication.peers.values().map(peer).collect();
    let mut document = ::serde_json::Map::new();
    document.insert("Peers".to_string(), Value::Array(peers));
    if let Some(role) = &replication.role {
        document.insert("Role".to_string(), json!(role));
    }
    Value::Object(document)
}

/// One peer of that arrangement: which domain it is, and which side of the arrangement it is
/// on.
fn peer(peer: &ReplicationPeer) -> Value {
    json!({ "PeerDomainArn": peer.peer_domain_arn, "Role": peer.peer_role })
}
