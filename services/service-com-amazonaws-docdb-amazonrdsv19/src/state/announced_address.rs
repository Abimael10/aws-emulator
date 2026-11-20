// SPDX-License-Identifier: CC0-1.0

//! The HOST an instance ANNOUNCES for a client to connect to.
//!
//! `DBInstance$Endpoint` "specifies the connection endpoint", whose `Address` is the DNS
//! address of the instance. An instance is a second address onto the storage its CLUSTER
//! manages, so what it announces is a name under the same DNS suffix its cluster minted;
//! only its own label differs, and the `Port` beside it is its cluster's.
//!
//! The host announced is the one this engine's control plane publishes rather than the
//! address this emulator is reached at, because this emulator serves no Amazon DocumentDB
//! data plane: a resource whose plane it serves — as a native wire on a listener of its own,
//! or as an AWS API at this emulator's own endpoint — publishes the address that plane
//! answers on, and a resource whose plane it serves neither way keeps the name its own
//! control plane publishes. That is the one reading the shared front door this engine is
//! also served through takes
//! (`services/service-com-amazonaws-rds-amazonrdsv19/src/state/api_plane_address.rs`), so
//! the two views of this engine announce the same host for one instance.

/// The `Endpoint$Address` one instance announces, resolved once when the instance is
/// created and stored on its record, so every later read of it reports the same host.
///
/// `endpoint_suffix` is the DNS suffix the instance's cluster minted, which its endpoints
/// and its members' endpoints share.
pub fn announced_instance_host(identifier: &str, endpoint_suffix: &str) -> String {
    format!("{identifier}.{endpoint_suffix}")
}
