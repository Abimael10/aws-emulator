// SPDX-License-Identifier: CC0-1.0

//! Where a connector's certificate-enrollment policy server is ANNOUNCED.
//!
//! `CertificateEnrollmentPolicyServerEndpoint` is the "certificate enrollment endpoint
//! for Active Directory domain-joined objects reach out to when requesting certificates"
//! (the member's own doc): an address the CALLER then dials, not an identifier. So it has
//! to lead back to the one endpoint this emulator answers on — the base URL a later
//! request addresses ([`::smithy_server_runtime::OperationContext::endpoint`]), with the
//! connector's identity in the PATH, since a client on the machine this emulator runs on
//! resolves no name AWS publishes. An endpoint built from a
//! `<service>.<region>.amazonaws.com` host sends every enrollment request off this
//! process to a host nothing here serves.
//!
//! The reading of which address a request reached is the tree's one reading of it
//! (`service_common::data_plane_addressing`), so this service writes no second rule for
//! which host is the emulator's own.

/// The segment naming this plane, behind the reserved path segment every address this
/// emulator announces on its own endpoint sits under
/// (`service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`): the certificate
/// enrollment policy a domain-joined object asks this connector for.
const ENROLLMENT_PLANE_SEGMENT: &str = "cep";

/// The tail of the path AWS publishes a connector's enrollment policy server under — the
/// Active Directory policy-provider endpoint a domain-joined object requests policies at.
/// Kept as the published spelling, so a client configured by hand against either address
/// asks for the same thing.
const POLICY_PROVIDER_CEP_PATH: &str = "ADPolicyProvider_CEP_Kerberos/service.svc/CEP";

/// One connector's enrollment policy server endpoint on the address `authority`:
/// `https://<authority>/_aws/cep/<region>/<connector-id>/<policy provider path>`.
///
/// The Region and the connector's id are carried in the PATH rather than in the host:
/// one emulator serves every Region on one endpoint, and the only host a client here can
/// resolve is the one it already reached. The value is distinct for two connectors and
/// carries no clock and no request id, so it is stable for the resource.
#[must_use]
pub fn enrollment_policy_server_endpoint(
    authority: &str,
    region: &str,
    connector_id: &str,
) -> String {
    format!(
        "https://{authority}/{namespace}/{ENROLLMENT_PLANE_SEGMENT}/{region}/{connector_id}/\
         {POLICY_PROVIDER_CEP_PATH}",
        namespace = ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE,
    )
}

/// The endpoint a connector created by THIS request announces, resolved once on the
/// create path and stored, so every later read reports the same value.
///
/// The address is the one the caller can reach:
///
/// * the EMULATOR'S OWN endpoint this request arrived on, for a client that resolves
///   nothing else (which is what a client configured with one local endpoint URL sends);
/// * else the address this process is SERVED at, so a connector minted through an
///   in-process call of another service is announced an address that service's own caller
///   can reach too;
/// * else the base this emulator reports while it is served on no port at all
///   ([`::smithy_server_runtime::UNSERVED_URL_BASE`]) — an emulator embedded in a host
///   process has no other address to name, and naming AWS's own host instead would hand
///   out an address this process never answers.
#[must_use]
pub fn announced_enrollment_policy_server_endpoint(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    connector_id: &str,
) -> String {
    let base = ctx.endpoint();
    let served = base.as_str();
    let authority = ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(
        ctx.parts(),
    )
    .or_else(|| ::service_common::data_plane_addressing::served_endpoint_authority(served))
    .unwrap_or_else(|| {
        ::service_common::data_plane_addressing::url_authority(served)
            .trim_end_matches('/')
            .to_string()
    });
    enrollment_policy_server_endpoint(&authority, region, connector_id)
}
