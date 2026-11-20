// SPDX-License-Identifier: CC0-1.0

//! WHICH host an operation that REPORTS a Device Advisor endpoint reports: the one value
//! `GetEndpoint` vends and a suite run reports and persists for its primary device.
//!
//! `Endpoint` is "The response of an Device Advisor endpoint" and the model bounds it at
//! `len 45..75` (`smithy_query(query="operation",operation="GetEndpoint")`). What AWS
//! answers there is a DNS HOSTNAME: an endpoint-id label followed by
//! `deviceadvisor.iot.<region>` under the zone AWS serves its endpoints in. That is what
//! the bound describes, and what a client reads the value FOR — it resolves the name, it
//! takes the region out of it to know where the device under test connects, and it tells
//! the account-level host from a device-level one. A bare `host:port` of this process is
//! none of those things and sits far below the bound, so it is not a value this member can
//! carry.
//!
//! The host reported is therefore AWS's own FORM over the zone THIS emulator answers in:
//!
//! * the ENDPOINT ID label ([`endpoint_id`]) — derived from the account, the region and the
//!   device the request names, so one device asked about twice is answered one host, two
//!   devices are answered two, and the account-level host differs from a device-level one,
//!   which is the per-account, per-device hostname AWS vends;
//! * the two labels that name the plane ([`PLANE_LABELS`]) and the region the request is
//!   signed for, which is what makes the value a Device Advisor endpoint of that region
//!   rather than any other host;
//! * the DNS zone this emulator may announce a name of its own in and the port it is served
//!   on ([`::service_common::data_plane_addressing::announced_zone_of_authority`], the
//!   tree's one reading of that zone), so the name leads back to this process — `localhost`
//!   and every name under it being reserved for the loopback interface the core binds on.
//!   A zone of AWS's own resolves nowhere near here or, worse, at genuine AWS
//!   infrastructure, which sends a device out of the emulated environment carrying the
//!   credentials it holds.
//!
//! The modelled length bound is a requirement of the member, so it is what SIZES the name:
//! the id label is as wide as AWS's wherever the rest leaves room for it, and a reached zone
//! or a signed-for region too long to fit falls back to the loopback zone and to the region
//! this service's ARNs fall back to ([`endpoint_for`]), so every value reported is inside
//! the bound.
//!
//! Nothing of a request's own ADDRESSING moves the answer: the name is built on the endpoint
//! this PROCESS is served at, never on the host one request happened to carry, so the host a
//! suite run persists at its start and the host `GetEndpoint` vends for that run's device
//! cannot drift.

use ::service_common::data_plane_addressing as addressing;
use ::sha2::{Digest, Sha256};

use super::{DeviceIdentity, DEFAULT_REGION};

/// The two labels every Device Advisor endpoint host carries between its endpoint id and
/// its region (`<endpoint-id>.deviceadvisor.iot.<region>.<zone>`): the plane the host
/// serves, which is what tells such a host from any other the same zone holds.
pub const PLANE_LABELS: [&str; 2] = ["deviceadvisor", "iot"];

/// The shortest value the `endpoint` member may carry, as the model bounds the `Endpoint`
/// shape (`len 45..75`).
pub const MIN_ENDPOINT_LEN: usize = 45;

/// The longest value the `endpoint` member may carry (that same bound's maximum).
pub const MAX_ENDPOINT_LEN: usize = 75;

/// How many characters the endpoint-id label carries wherever the bound leaves room for it:
/// the width of the label AWS's own Device Advisor hostname leads with.
const ENDPOINT_ID_LEN: usize = 14;

/// The shortest endpoint-id label a host is still built around, for a name whose other
/// labels leave no room for a wider one.
const MIN_ENDPOINT_ID_LEN: usize = 1;

/// The host a reporting operation announces for `device` in `region`, for the request `ctx`
/// is serving: this emulator's Device Advisor hostname (see the module docs).
///
/// `GetEndpoint` vends it and `StartSuiteRun` reports and persists it for the run's primary
/// device, both through this one derivation, so a client told where a device connects by the
/// start call is told the same host when it asks for that device's endpoint.
pub fn announced_endpoint(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    device: &DeviceIdentity<'_>,
) -> String {
    endpoint_for(ctx.endpoint().as_str(), region, device)
}

/// The Device Advisor hostname this emulator reports for `device` in `region` while it is
/// served at `served_base` ([`::smithy_server_runtime::OperationContext::endpoint`], a base
/// URL — or an authority already, which is what a caller holding one passes).
///
/// Always a value inside the modelled bound: the id label is sized to what the rest of the
/// name leaves ([`ENDPOINT_ID_LEN`] wherever there is room for AWS's own width), and a zone
/// or a region that cannot fit at all is replaced — first by the loopback zone, which is an
/// address of this process either way, then by the default region — rather than reported
/// outside the bound.
pub fn endpoint_for(served_base: &str, region: &str, device: &DeviceIdentity<'_>) -> String {
    let authority = addressing::url_authority(served_base).trim_end_matches('/');
    let zone = addressing::announced_zone_of_authority(authority);
    let port =
        addressing::addressed_port(Some(authority)).and_then(|port| u16::try_from(port).ok());
    let suffix = fitting_suffix(region, &zone, port);
    // The room the rest of the name leaves the id label, and the width the bound's MINIMUM
    // asks of it. Both are read against the one dot that joins the label to the suffix.
    let room = MAX_ENDPOINT_LEN
        .saturating_sub(1 + suffix.len())
        .max(MIN_ENDPOINT_ID_LEN);
    let shortfall = MIN_ENDPOINT_LEN
        .saturating_sub(1 + suffix.len())
        .clamp(MIN_ENDPOINT_ID_LEN, room);
    let width = ENDPOINT_ID_LEN.clamp(shortfall, room);
    format!("{}.{suffix}", endpoint_id(region, device, width))
}

/// Everything of a reported host that follows its endpoint-id label, for an emulator
/// reached in `zone` on `port` by a request signed for `region`: the plane's labels, the
/// region, the zone, and the port that zone is served on.
///
/// The first form that leaves room for an id label inside the modelled bound is the one
/// reported. A zone too long for the bound to hold is replaced by the loopback zone, which
/// resolves to the interface the core binds on, so the name still leads back to this
/// process; a region too long for even that — no region AWS publishes is — is replaced by
/// the region this service's ARNs fall back to.
fn fitting_suffix(region: &str, zone: &str, port: Option<u16>) -> String {
    [
        host_suffix(region, zone, port),
        host_suffix(region, addressing::LOOPBACK_ZONE, port),
        host_suffix(region, addressing::LOOPBACK_ZONE, None),
    ]
    .into_iter()
    .find(|suffix| leaves_room_for_an_id(suffix))
    .unwrap_or_else(|| host_suffix(DEFAULT_REGION, addressing::LOOPBACK_ZONE, None))
}

/// Whether a host built on `suffix` is inside the modelled maximum with an id label of at
/// least [`MIN_ENDPOINT_ID_LEN`] characters and the dot that joins the two.
fn leaves_room_for_an_id(suffix: &str) -> bool {
    1 + MIN_ENDPOINT_ID_LEN + suffix.len() <= MAX_ENDPOINT_LEN
}

/// `deviceadvisor.iot.<region>.<zone>`, carrying `:<port>` when the emulator is served on
/// one — the port a client dials what it resolves this name to on.
fn host_suffix(region: &str, zone: &str, port: Option<u16>) -> String {
    let [plane, device_plane] = PLANE_LABELS;
    let host = format!("{plane}.{device_plane}.{region}.{zone}");
    match port {
        Some(port) => addressing::host_port_authority(&host, port),
        None => host,
    }
}

/// The endpoint-id label a reported host leads with: `width` lower-case hexadecimal
/// characters of a digest over the ACCOUNT, the `region` and every ARN `device` names.
///
/// Derived rather than minted, so nothing has to be stored for it and every report agrees:
/// the same account, region and device are answered the same label by every call and in
/// every process, two devices are answered different labels, and a request that names no
/// device — the account-level question — is answered a label of its own. A separator no
/// account id, region or ARN carries joins the fields, so two identities cannot run
/// together into one preimage.
fn endpoint_id(region: &str, device: &DeviceIdentity<'_>, width: usize) -> String {
    let mut digest = Sha256::new();
    for field in [
        ::emulator_server::emulator_account_id(),
        region,
        device.thing_arn.unwrap_or_default(),
        device.certificate_arn.unwrap_or_default(),
        device.device_role_arn.unwrap_or_default(),
    ] {
        digest.update(field.as_bytes());
        digest.update([0u8]);
    }
    let mut label = ::hex::encode(digest.finalize());
    label.truncate(width);
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The addresses this emulator can be served at: a socket, an IPv6 socket, a routable
    /// address, the loopback name, a name an operator serves it under, a name with no port,
    /// and the base it reports while it is served on no port at all.
    const SERVED_BASES: [(&str, &str); 7] = [
        ("served on no port", ::smithy_server_runtime::UNSERVED_URL_BASE),
        ("a loopback socket", "https://127.0.0.1:42899"),
        ("an IPv6 loopback socket", "https://[::1]:9297"),
        ("a routable address", "https://192.168.1.10:4566"),
        ("the loopback name with a port", "https://localhost:9297"),
        ("a name an operator serves it under", "https://emulator.internal:8443"),
        ("a name with no port", "https://host.docker.internal"),
    ];

    /// RULE: a reported host is an id label over the plane's labels and the region, in a
    /// zone this emulator answers in and inside the modelled length bound — for every
    /// address this process can be served at and every region a request can be signed for.
    #[test]
    fn a_reported_host_is_a_device_advisor_name_inside_the_modelled_bound() {
        // Regions of several shapes, plus one no AWS region is: longer than the bound could
        // ever hold, which is what the fallbacks are for.
        let long_region = format!("{}-east-1", "a".repeat(80));
        let regions = [
            DEFAULT_REGION,
            "us-west-2",
            "eu-central-1",
            "ap-southeast-4",
            "us-gov-west-1",
            long_region.as_str(),
        ];
        let named_device = DeviceIdentity {
            thing_arn: Some("arn:aws:iot:us-west-2:000000000000:thing/probe"),
            ..DeviceIdentity::default()
        };
        let [plane, device_plane] = PLANE_LABELS;
        for (served, base) in SERVED_BASES {
            for region in regions {
                for (named, identity) in [
                    ("no device", DeviceIdentity::default()),
                    ("a device", named_device),
                ] {
                    let host = endpoint_for(base, region, &identity);
                    let case = format!("{served} in {region} for {named}");
                    assert!(
                        (MIN_ENDPOINT_LEN..=MAX_ENDPOINT_LEN).contains(&host.len()),
                        "case {case:?}: expected a host of \
                         {MIN_ENDPOINT_LEN}..={MAX_ENDPOINT_LEN} characters, got {} in \
                         {host:?}",
                        host.len()
                    );
                    assert!(
                        host.contains(&format!(".{plane}.{device_plane}.")),
                        "case {case:?}: expected the plane's labels in {host:?}"
                    );
                    assert!(
                        host.bytes().all(|byte| byte.is_ascii_alphanumeric()
                            || matches!(byte, b'.' | b'-' | b':')),
                        "case {case:?}: expected a hostname, got {host:?}"
                    );
                    // A name of AWS's own leads a device out of this emulator, so no
                    // reported host carries a zone AWS serves its own endpoints in.
                    for aws_zone in addressing::AWS_ENDPOINT_DNS_SUFFIXES {
                        assert!(
                            !addressing::host_only(&host).ends_with(aws_zone),
                            "case {case:?}: expected no zone of AWS's own ({aws_zone}) in \
                             {host:?}"
                        );
                    }
                }
            }
        }
    }

    /// RULE: the id label follows the account, the region and the device it is derived
    /// from — so one of them is answered one host and two of them two.
    #[test]
    fn the_id_label_follows_the_region_and_the_device_it_is_derived_from() {
        let base = "https://127.0.0.1:9297";
        let thing = |name: &str| format!("arn:aws:iot:us-west-2:000000000000:thing/{name}");
        let host_of = |region: &str, thing: Option<&str>| {
            endpoint_for(
                base,
                region,
                &DeviceIdentity {
                    thing_arn: thing,
                    ..DeviceIdentity::default()
                },
            )
        };
        let alpha = thing("alpha");
        let beta = thing("beta");

        let account_level = host_of("us-west-2", None);
        let for_alpha = host_of("us-west-2", Some(&alpha));
        let for_alpha_again = host_of("us-west-2", Some(&alpha));
        let for_beta = host_of("us-west-2", Some(&beta));
        let for_alpha_elsewhere = host_of("eu-central-1", Some(&alpha));

        assert_eq!(
            for_alpha, for_alpha_again,
            "case \"one device twice\": expected one host, got {for_alpha:?} and \
             {for_alpha_again:?}"
        );
        assert_ne!(
            for_alpha, for_beta,
            "case \"two devices\": expected two hosts, got {for_alpha:?} for both"
        );
        assert_ne!(
            for_alpha, account_level,
            "case \"a device and the account\": expected two hosts, got {for_alpha:?} for \
             both"
        );
        assert_ne!(
            for_alpha, for_alpha_elsewhere,
            "case \"one device in two regions\": expected two hosts, got {for_alpha:?} for \
             both"
        );
    }
}
