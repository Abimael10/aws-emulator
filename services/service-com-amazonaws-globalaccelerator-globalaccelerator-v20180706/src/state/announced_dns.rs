// SPDX-License-Identifier: CC0-1.0

//! WHICH host an accelerator ANNOUNCES to a client — `Accelerator$DnsName`,
//! `Accelerator$DualStackDnsName` and `CustomRoutingAccelerator$DnsName`.
//!
//! `DnsName` is "The Domain Name System (DNS) name that Global Accelerator creates that
//! points to an accelerator's static IPv4 addresses", and `DualStackDnsName` the name that
//! "points to a dual-stack accelerator's four static IP addresses: two IPv4 addresses and
//! two IPv6 addresses" (`smithy_query(query="doc",shape="Accelerator",member="DnsName")`).
//! Neither is a label a client reads back and puts away: it is the host the client RESOLVES
//! and then sends its accelerated traffic to. So the only thing either member may carry is a
//! host THIS emulator answers on — the host of the one endpoint it serves its whole surface
//! at, read through the tree's one reading of where that is
//! ([`::service_common::data_plane_addressing`]).
//!
//! A host under the zone the model's naming convention describes
//! (`a1234567890abcdef.awsglobalaccelerator.com`) is a host AWS serves: it resolves nowhere
//! near this process or, worse, at genuine Global Accelerator infrastructure, so a client
//! handed one leaves the emulated environment carrying the credentials it holds and reaches
//! none of the emulated endpoint groups. A host of this emulator's own invention is no
//! better, whatever zone it is minted in: nothing answers for a sub-name of the host this
//! process is reached at, so a client that resolves one reaches nothing either.
//!
//! ONE host therefore answers for every accelerator of this emulator, where AWS mints a name
//! per accelerator: this process serves its whole surface at a single endpoint, so the host a
//! caller can reach an accelerator at is the host it is already talking to. An accelerator's
//! own identity travels in its ARN, its name and its static addresses, which are per-record
//! and unique; what these members carry is an address, and there is one. Both DNS members of
//! one accelerator carry it: one process answers for both address families, so a dual-stack
//! accelerator is reached where every other one is.
//!
//! # Why not a name of this emulator's own in the zone it answers in
//!
//! The tree also carries the reading for a name a service MINTS of its own —
//! [`addressing::announced_zone_of_authority`], "the DNS zone this emulator may ANNOUNCE a
//! name of its own in", which a service with a per-resource name to publish builds its first
//! label over (the VPC Lattice `DnsEntry` does). This member does not use it, for the reason
//! the reading above gives and the same one the Device Advisor endpoint and the DynamoDB
//! endpoint-discovery address state: nothing in this process answers for a SUB-NAME of the
//! host it is reached at, so `<label>.<zone>` identifies the accelerator but resolves nowhere,
//! and a client that resolves it reaches no accelerator at all. A per-resource name is
//! publishable where the resource's identity can travel in the PATH of a URL the emulator
//! serves (an api's `ApiEndpoint`, say); `DnsName` carries a bare host, which leaves the
//! address as the only thing it can carry. The accelerator-labelled form was also put to the
//! recorded traces, which report a host under it as one the responder does not own.
//!
//! The host is read per response rather than stored, so nothing persists an address this
//! emulator may no longer be reached at: a create, a read, a listing and an update all
//! announce the address of the request being served.
//!
//! An emulator served on NO port has no address of its own to announce, and
//! [`::smithy_server_runtime::OperationContext::endpoint`] then names its unserved base — a
//! host RFC 6761 reserves so that nothing resolves it. That is the honest answer for an
//! in-process caller with no socket behind it: it names no host of AWS's and leads no client
//! out of this process, which is what the member has to guarantee.

use ::service_common::data_plane_addressing as addressing;

/// The host an accelerator's DNS members announce for the request being served: the host of
/// the endpoint the request reached, or of the address this process is served at for a caller
/// that addressed one of AWS's own hosts — and the unserved base's reserved host for an
/// emulator served on no port at all (see the module docs).
///
/// A HOST, with no port: a client feeds this value to a resolver, writes it in a `Host`
/// header and uses it as a TLS server name, and the port it then connects on is the
/// accelerator's listener's, never this member's.
pub fn announced_dns_host(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    let authority = addressing::reached_emulator_endpoint_of_parts(ctx.parts())
        .unwrap_or_else(|| addressing::emulator_endpoint_authority(ctx));
    addressing::host_only(&authority).to_string()
}
