// SPDX-License-Identifier: CC0-1.0

//! WHICH addresses an access point's DATA PLANE is reached at — the map
//! `GetAccessPoint$Endpoints` reports.
//!
//! RULE: a read of an access point reports one entry per endpoint variant S3
//! publishes for an access point — the plain Regional endpoint, the FIPS one,
//! FIPS over dual-stack, and dual-stack — in that one order, and every entry's
//! value is an address THIS emulator answers on.
//!
//! Two things that map has to get right, and both are about what the CLIENT does
//! with it:
//!
//! * The ENTRY ORDER is fixed. A map member of a restXml body is written entry
//!   by entry in the order the map holds, so the map is an order-preserving one
//!   ([`::indexmap::IndexMap`]) populated in the order above. A client that reads
//!   an entry by position, or that compares two reads of one unchanged access
//!   point, is then handed the same document every time instead of a fresh
//!   permutation per response.
//! * The ADDRESS is this emulator's own: the `host[:port]` the data plane is
//!   served at ([`::service_common::data_plane_addressing::emulator_endpoint_authority`],
//!   the tree's one reading of that question). A client reads this map to learn
//!   where to send its object requests, so every other value is a dead route: a
//!   host in a zone AWS serves its own endpoints in resolves off this machine —
//!   or at genuine AWS infrastructure, which sends the client out of the
//!   emulated environment carrying the credentials it holds — and a name of this
//!   emulator's own invention under the host it is reached at resolves nowhere,
//!   because nothing answers for a sub-name of that host.
//!
//! So the four variants are reported at the one address this process serves: it
//! runs a single data plane, over whichever stack its listener is bound on, and
//! there is no second listener for a FIPS or a dual-stack entry to name. The
//! KEYS still name the variants AWS publishes, which is what a client selects an
//! entry by. Reached through that address, an access point is addressed by its
//! alias or its ARN exactly as AWS documents
//! (`service_com_amazonaws_s3_amazons3::state::access_points`, which resolves
//! both), so the entry a client picks leads to the access point it read it from.

use ::service_common::data_plane_addressing as addressing;

/// The keys `GetAccessPoint$Endpoints` reports, IN THE ORDER they are reported:
/// the plain Regional endpoint first — the one a client that reads a single
/// entry takes — then FIPS, then FIPS over dual-stack, then dual-stack.
pub const REPORTED_VARIANTS: [&str; 4] = ["ipv4", "fips", "fips_dualstack", "dualstack"];

/// The endpoint map a read of an access point reports, for the request `ctx` is
/// serving: one entry per variant in [`REPORTED_VARIANTS`]' order, each naming
/// the address this emulator serves its data plane at (see the module docs).
pub fn announced_endpoints(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
) -> ::indexmap::IndexMap<String, String> {
    endpoints_at(&addressing::emulator_endpoint_authority(ctx))
}

/// [`announced_endpoints`] for an emulator served at `authority` — the
/// `host[:port]` of the one endpoint it answers on.
pub fn endpoints_at(authority: &str) -> ::indexmap::IndexMap<String, String> {
    REPORTED_VARIANTS
        .into_iter()
        .map(|variant| (variant.to_string(), authority.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the four entries are reported under their own keys, in their one
    /// order, each naming the address this emulator is served at — whatever
    /// address that is.
    #[test]
    fn every_variant_is_reported_in_order_at_the_served_address() {
        // The addresses this emulator can be served at: a loopback socket, an
        // IPv6 socket, a routable address, the loopback name, a name an operator
        // serves it under, and the base the core reports while it is served on no
        // port at all.
        let cases = [
            ("a loopback socket", "127.0.0.1:9297"),
            ("an IPv6 loopback socket", "[::1]:42899"),
            ("a routable address", "192.168.1.10:4566"),
            ("the loopback name with a port", "localhost:9297"),
            ("a name an operator serves it under", "emulator.internal:8443"),
            (
                "served on no port",
                addressing::url_authority(::smithy_server_runtime::UNSERVED_URL_BASE),
            ),
        ];
        for (case, authority) in cases {
            let reported = endpoints_at(authority);
            let keys: Vec<&str> = reported.keys().map(String::as_str).collect();
            assert_eq!(
                keys,
                REPORTED_VARIANTS.to_vec(),
                "case {case:?}: expected the entries in their reported order, got {keys:?}"
            );
            for (key, value) in &reported {
                assert_eq!(
                    value, authority,
                    "case {case:?}: expected entry {key} to name the served address \
                     {authority:?}, got {value:?}"
                );
                // A name in a zone AWS serves its own endpoints in leads a client
                // out of this emulator, so no reported value carries one.
                for aws_zone in addressing::AWS_ENDPOINT_DNS_SUFFIXES {
                    assert!(
                        !addressing::host_only(value).ends_with(aws_zone),
                        "case {case:?}: expected no zone of AWS's own ({aws_zone}) in {value:?}"
                    );
                }
            }
        }
    }
}
