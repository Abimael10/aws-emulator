// SPDX-License-Identifier: CC0-1.0

//! WHICH server names an edge address presents a certificate for, read from this service's own
//! records.
//!
//! The reading of the connection and the refusal itself are shared
//! ([`::service_common::in_band_tls::negotiated_scheme_for_covered_names`]): a TLS server
//! presents a certificate for the name the client asked for, and a name it holds none for is
//! refused in the handshake rather than answered over HTTP. What is CloudFront's own is WHICH
//! names those are, and that differs by address:
//!
//! * At a CONNECTION GROUP's routing endpoint the certificates are the TENANTS' own: each
//!   tenant brings the domain its viewers reach it at and the certificate that domain is served
//!   under, and the routing endpoint is only the address those domains are pointed at. So the
//!   names covered are the domains of the tenants that group routes ([`routed_tenant`]) — and
//!   the routing endpoint itself, a domain the group routes nowhere, and no name at all are
//!   each a name no certificate there covers.
//! * At a DISTRIBUTION's own address the names covered are the ones that distribution is served
//!   on — the name CloudFront assigned it, under CloudFront's own zone, and the alternate domain
//!   names its `ViewerCertificate` names ([`crate::service::edge::serves_host`]) — plus the
//!   addresses of the interface the listener is bound on, which this emulator's own certificate
//!   covers. A client that asks for NO name is served there too: it is answered under the
//!   certificate of the address it dialled.
//!
//! The records are read, never written: deciding a handshake changes nothing a later caller
//! sees.

use super::{ComAmazonawsCloudfrontCloudfront20200531State, StoredDistributionTenant};

/// The tenant `group` routes `name` to, or `None` when that group routes the name to none.
///
/// Read through `distribution_tenant_by_domain`, the one host-to-tenant map a viewer request is
/// resolved through — which answers only for a domain CloudFront can already serve, one whose
/// certificate has been validated — and then narrowed to the tenants routed through THIS group,
/// so one group's address never answers for a name another group's tenant holds.
///
/// The name is read AS ADDRESSED first and then without its port, the same order the data plane
/// reads a host in: a domain this emulator published for a resource carries a port, which is
/// part of the name a viewer was handed, while a customer's own domain does not and is addressed
/// with whatever port the viewer dialled.
///
/// ONE lookup, so WHICH tenant a name reaches cannot drift between the handshake that admits the
/// name and the request served afterwards — the two differ only in what each then asks of the
/// tenant it found (whether it is `Enabled`, which decides what a request is ANSWERED, while the
/// certificate covers the domain either way).
pub(crate) fn routed_tenant<'store>(
    store: &'store ComAmazonawsCloudfrontCloudfront20200531State,
    group: &str,
    name: &str,
) -> Option<&'store StoredDistributionTenant> {
    store
        .distribution_tenant_by_domain(name)
        .or_else(|| {
            store.distribution_tenant_by_domain(
                ::service_common::data_plane_addressing::host_only(name),
            )
        })
        .filter(|tenant| tenant.connection_group_id == group)
}
