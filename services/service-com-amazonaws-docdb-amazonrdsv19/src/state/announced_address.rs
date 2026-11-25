// SPDX-License-Identifier: CC0-1.0

//! The HOST an instance announces when this emulator serves no data plane for it.
//!
//! `DBInstance$Endpoint` "specifies the connection endpoint", whose `Address` is the DNS
//! address of the instance. Where this emulator SERVES that instance's data plane it
//! publishes the address its own socket answers on, because an endpoint is an address the
//! service answers on ([`super::cluster_data_plane`]). Where it serves none — an instance of
//! a cluster whose engine speaks no wire this emulator runs, or a process whose engine vends
//! no listeners at all — there is no address of this emulator's to publish, and the instance
//! keeps the name its own control plane would give it: an instance is a second address onto
//! the storage its CLUSTER manages, so that name sits under the same DNS suffix its cluster
//! minted, with only its own label differing.

/// The `Endpoint$Address` an instance announces where this emulator serves no data plane for
/// it, resolved once when the instance is created and stored on its record, so every later
/// read of it reports the same host.
///
/// `endpoint_suffix` is the DNS suffix the instance's cluster minted, which its endpoints
/// and its members' endpoints share.
pub fn announced_instance_host(identifier: &str, endpoint_suffix: &str) -> String {
    format!("{identifier}.{endpoint_suffix}")
}
