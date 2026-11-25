// SPDX-License-Identifier: CC0-1.0

//! The addresses an environment PUBLISHES, and the reading of a host built from
//! one of them.
//!
//! `EnvironmentDescription$CNAME` is "the URL to the CNAME for this environment"
//! and `EndpointURL` is "for load-balanced, autoscaling environments, the URL to
//! the LoadBalancer. For single-instance environments, the IP address of the
//! instance" (`smithy_query(query="operation", operation="DescribeEnvironments")`).
//! Both are HOSTS: a client reads one and then points a DNS alias at it, writes it
//! into a health check, sets it as a request's `Host`, or splits the first label
//! off the CNAME to recover the prefix and hand it back to `CheckDNSAvailability`
//! ("the fully qualified CNAME to reserve when CreateEnvironment is called with
//! the provided prefix"). So neither may carry a port or a path: a name is a name,
//! and the port and the path a request needs are resolved at the listener that
//! serves it ([`super::environment_plane_endpoint`],
//! [`super::environment_plane_path`]).
//!
//! What each of the two names:
//!
//! * the CNAME is the name the environment's CNAME PREFIX forms in its Region
//!   under a sub-domain of that environment's own ([`environment_cname`]), in the
//!   zone this emulator answers in — the prefix is its first dot-label, the
//!   documented route back to it, and a `SwapEnvironmentCNAMEs` moves these whole
//!   names between two environments;
//! * the `EndpointURL` is the address of the thing answering BEHIND that name:
//!   the DNS name of the environment's own load balancer where its configuration
//!   provisions one ([`load_balancer_host`], built from the load balancer in the
//!   environment's stored inventory), and otherwise the address its single
//!   instance answers at. It stays with its environment when a swap moves the
//!   names, exactly as a load balancer does.
//!
//! Where each name sits. Every ADDRESS this service hands a client — the
//! environment's CNAME and the load balancer's host — sits under its own service's
//! label in the zone this emulator may announce a name of its own in
//! ([`announced_zone`], the workspace's one reading of that question): a name in a
//! zone Amazon Web Services serves resolves nowhere near this process or, worse,
//! at genuine infrastructure, so announcing one would send a client that dials the
//! one address the API publishes out of the emulated environment. The name Elastic
//! Beanstalk's own zone forms for a prefix is a different value, reported where
//! the service reports ON a name rather than handing out an address
//! ([`fully_qualified_cname`], `CheckDNSAvailability$FullyQualifiedCNAME`).
//!
//! Both addresses are read back by [`published_host`], which is how a request that
//! named one in its `Host` is served the environment that carries it — and a
//! client that resolves neither reaches the same environment at the emulator's own
//! endpoint, by the plane path ([`super::environment_plane_path`]).

use super::*;

use ::service_common::data_plane_addressing as addressing;

/// The DNS label naming the load-balancing service in the host a load balancer is
/// published under, as Amazon Web Services publishes one
/// (`<load balancer>.<region>.elb.<domain>`).
pub const LOAD_BALANCER_SEGMENT: &str = "elb";

/// WHERE this emulator publishes an environment's addresses: the DNS zone it may
/// announce a name of its OWN in — the zone every ADDRESS this service hands a
/// client sits in, the load balancer's host included — and the address an
/// environment's instances answer at.
///
/// Resolved once per launching call ([`published_addressing`]) and STORED on the
/// environment, so every later read of that environment reports the addresses its
/// launch published rather than ones re-derived from the reading caller's own
/// request.
#[derive(Debug, Clone)]
pub struct PublishedAddressing {
    /// The zone every ADDRESS this service announces sits in
    /// ([`announced_zone`]).
    pub zone: String,
    /// The address an environment's Amazon EC2 instances answer at: the interface
    /// the environment plane is served on ([`instance_address`]).
    pub instance_address: String,
}

/// The addressing the emulator serving `ctx` publishes an environment's addresses
/// from, opening the socket the environment plane is served on if this is the
/// first call that needed it ([`super::environment_plane_authority`]).
pub async fn published_addressing<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &smithy_server_runtime::OperationContext<'_>,
) -> PublishedAddressing {
    let plane_authority = environment_plane_authority(engine, ctx).await;
    PublishedAddressing {
        zone: announced_zone(ctx),
        instance_address: instance_address(&plane_authority),
    }
}

/// The DNS ZONE this emulator may announce a name of its own in, for the emulator
/// serving `ctx`: read off the address it is reached at
/// ([`addressing::announced_zone_of_authority`]), which answers the loopback zone
/// for a process reached at an address literal, at no endpoint at all, or at a
/// host Amazon Web Services publishes its own endpoints at.
///
/// RFC 6761 reserves `localhost` and every name under it for the loopback
/// interface the core binds its listeners on, so a name announced under that zone
/// leads back to this process.
pub fn announced_zone(ctx: &smithy_server_runtime::OperationContext<'_>) -> String {
    let served =
        addressing::served_endpoint_authority(ctx.endpoint().as_str()).unwrap_or_default();
    addressing::announced_zone_of_authority(&served)
}

/// The address an environment's instances answer at, for an environment plane
/// served at `plane_authority`: that authority's HOST, without the port the
/// published member may not carry ([`addressing::host_only`]).
///
/// An emulator vending no listener at all names the loopback interface instead —
/// the interface the core binds on, and the one every address this process
/// answers at resolves to.
pub fn instance_address(plane_authority: &str) -> String {
    let host = addressing::host_only(plane_authority);
    match host.is_empty() {
        true => addressing::LOOPBACK_ZONE.to_string(),
        false => host.to_string(),
    }
}

/// The DNS zone an Elastic Beanstalk CNAME prefix forms its fully qualified NAME
/// in: `<prefix>.<region>.elasticbeanstalk.com`.
///
/// The zone the service's own published names sit in, which is what
/// `CheckDNSAvailability` REPORTS ON ([`fully_qualified_cname`]) — a question about
/// a name, answered whether or not anything of this emulator answers at it. It is
/// NOT where an address a client DIALS may sit: a name in a zone Amazon Web
/// Services serves resolves nowhere near this process, so every address this
/// service hands a client is announced in the zone this emulator may announce a
/// name of its own in instead ([`environment_cname`], [`load_balancer_host`]).
pub const CNAME_ZONE: &str = "elasticbeanstalk.com";

/// The fully qualified DNS NAME the CNAME prefix `cname_prefix` forms in `region` —
/// what `CheckDNSAvailability$FullyQualifiedCNAME` reports, "the fully qualified
/// CNAME to reserve when CreateEnvironment is called with the provided prefix".
///
/// A NAME the report is ABOUT, in the zone Elastic Beanstalk publishes its
/// environments' names in ([`CNAME_ZONE`]): what a caller does with it is read
/// `Available` beside it and then create an environment under that prefix. The
/// address that environment is then REACHED at is the other value, announced in
/// the zone this emulator answers in ([`environment_cname`]).
///
/// The PREFIX is the first label, exactly as the caller wrote it, because that is
/// the documented route back to it: a client splits the first dot-label off a
/// published name and hands it to `CheckDNSAvailability`. The REGION is the label
/// behind it, read through the Region grammar every model's ARN pattern states for
/// that field (`::service_common::arn::region_label`) so no value a request
/// carried can make the reported name name an authority the caller chose.
///
/// The same prefix in two Regions is two names, which is why the Region is part of
/// the name and `CheckDNSAvailability` reports on the two separately.
pub fn fully_qualified_cname(region: &str, cname_prefix: &str) -> String {
    format!("{cname_prefix}.{}.{CNAME_ZONE}", published_region(region))
}

/// The label an environment's own generated SUB-DOMAIN is minted under, as
/// Elastic Beanstalk mints one: `eba-<generated>`, the label that sits between an
/// environment's CNAME prefix and its Region.
const ENVIRONMENT_SUB_DOMAIN_LABEL: &str = "eba";

/// How many characters that sub-domain's generated body carries.
const ENVIRONMENT_SUB_DOMAIN_BODY_LENGTH: usize = 8;

/// A fresh SUB-DOMAIN for one environment: `eba-<generated>`
/// ([`ENVIRONMENT_SUB_DOMAIN_LABEL`]).
///
/// Server-generated and opaque, like the `EnvironmentId` beside it (and drawn the
/// same way, so this crate reaches one random source only): each environment gets
/// its own, it is minted once when the environment is launched and travels inside
/// the stored name, so two reads of one environment report one `CNAME` and no two
/// environments are published under one name.
pub fn new_environment_sub_domain() -> String {
    format!(
        "{ENVIRONMENT_SUB_DOMAIN_LABEL}-{}",
        random_id_body(ENVIRONMENT_SUB_DOMAIN_BODY_LENGTH)
    )
}

/// The DNS NAME an ENVIRONMENT publishes as its `CNAME`, which a client DIALS: the
/// CNAME prefix it carries, that environment's own sub-domain
/// ([`new_environment_sub_domain`]), its Region and this service's own label under
/// `zone` — the zone this emulator may announce a name of its own in.
///
/// The prefix is the FIRST label, so the documented route back to it is the same
/// one the reported name offers ([`fully_qualified_cname`]), and the Region is the
/// label ahead of this service's own, so the name reads back as that prefix in
/// that Region ([`published_host`]) and the plane serves a request stating it. The
/// zone is NOT the one Elastic Beanstalk publishes in: this value is an address,
/// and an address a client dials has to lead back to this process.
///
/// EVERY label a request's values reach is folded into one DNS label
/// ([`addressing::dns_label`]) — the prefix as much as the sub-domain — because
/// this value is the AUTHORITY of an address a client dials: a `:` in it would
/// move the port and a `/` a path, so a caller-supplied prefix (or an
/// `EnvironmentName` the generated prefix is derived from) could otherwise make
/// the published address name a host of the caller's choosing. DNS compares labels
/// case-insensitively and this store does too
/// ([`super::environment_serving_cname_prefix`]), so the folded label still names
/// the prefix the environment holds and `CheckDNSAvailability` still reports it
/// taken.
///
/// A `SwapEnvironmentCNAMEs` exchanges these whole names between two
/// environments, sub-domain included: what moves is the name, and an environment
/// answers at the name it now carries.
pub fn environment_cname(
    zone: &str,
    region: &str,
    sub_domain: &str,
    cname_prefix: &str,
) -> String {
    format!(
        "{}.{}.{}.{DATA_PLANE_SEGMENT}.{zone}",
        addressing::dns_label(cname_prefix),
        addressing::dns_label(sub_domain),
        published_region(region)
    )
}

/// The DNS name of the load balancer `load_balancer_name` of an environment in
/// `region` — the host `EnvironmentDescription$EndpointURL` carries for a
/// load-balanced environment, "the URL to the LoadBalancer".
///
/// The load balancer's own name is the first label, folded into one DNS label
/// ([`addressing::dns_label`]), then the Region and the load-balancing service's
/// label under this emulator's zone: the form Amazon Web Services publishes a load
/// balancer's name in, in a zone that leads back to this process.
pub fn load_balancer_host(zone: &str, region: &str, load_balancer_name: &str) -> String {
    format!(
        "{}.{}.{LOAD_BALANCER_SEGMENT}.{zone}",
        addressing::dns_label(load_balancer_name),
        published_region(region)
    )
}

/// The Region as a published name carries it: the label the Region grammar admits,
/// and this service's default Region for a value that is no Region label at all —
/// the same reading the stored names are compared by
/// ([`super::environment_serving_cname_prefix`]), so a request's Region and a
/// stored name cannot disagree about which Region the name sits in.
fn published_region(region: &str) -> &str {
    ::service_common::arn::region_label(region).unwrap_or(DEFAULT_REGION)
}

/// What a published HOST names: one of the two addresses an environment publishes,
/// taken apart into the parts that identify it.
///
/// The two are different questions. A NAME is answered by whichever environment
/// CARRIES it now — the point of `SwapEnvironmentCNAMEs`, where the names move and
/// the environments do not — and a load balancer's host by the environment that
/// load balancer fronts, which no swap moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishedHost<'r> {
    /// The name the CNAME prefix `cname_prefix` forms in `region`
    /// ([`fully_qualified_cname`]).
    Name {
        region: &'r str,
        cname_prefix: &'r str,
    },
    /// The host of the load balancer `load_balancer` of an environment in `region`
    /// ([`load_balancer_host`]).
    LoadBalancer {
        region: &'r str,
        load_balancer: &'r str,
    },
}

/// What the host `host` names, or `None` for a host that is no published address
/// of this service.
///
/// The reading of the names above, anchored at the END: the label of the SERVICE
/// the name belongs to — this service's own for a CNAME, the load-balancing
/// service's for a load balancer — with the domain the name sits in behind it
/// (which carries at least one label of its own, since a name is a name only with
/// a domain under it), the Region in front of it, and the resource the name is
/// ABOUT as the leading label: the CNAME prefix, or the load balancer.
///
/// Anchored that way because an environment's own `CNAME` carries one label the
/// name its prefix reserves does not — that environment's own sub-domain
/// ([`environment_cname`]) — and both names name the same prefix in the same
/// Region, so both read back the same and the plane serves either. Nothing else
/// may sit between the two: a host carrying more labels than that names no
/// published address of this service.
///
/// Any userinfo, the port and a trailing root dot are not part of the name and are
/// dropped first, and the service label is compared ignoring case, the way DNS
/// compares names. The DOMAIN behind that label is not compared at all: what
/// identifies the environment is the leading labels, so a client that pointed a
/// name of its own at this emulator is served the same request.
pub fn published_host(host: &str) -> Option<PublishedHost<'_>> {
    let named = addressing::host_only(host).trim_end_matches('.');
    let labels: Vec<&str> = named.split('.').collect();
    if labels.iter().any(|label| label.is_empty()) {
        return None;
    }
    // The service's label sits at least two labels in (the resource and its
    // Region) and at least one label out (the domain), and the environment's own
    // sub-domain is the ONE optional label between the resource and the Region.
    let (kind, region) = [3usize, 2]
        .into_iter()
        .filter(|at| at + 1 < labels.len())
        .find_map(|at| {
            let kind = labels.get(at).copied().and_then(kind_of)?;
            let region = labels.get(at - 1).copied()?;
            Some((kind, region))
        })?;
    let resource = labels.first().copied()?;
    Some(match kind {
        PublishedService::Environment => PublishedHost::Name {
            region,
            cname_prefix: resource,
        },
        PublishedService::LoadBalancer => PublishedHost::LoadBalancer {
            region,
            load_balancer: resource,
        },
    })
}

/// WHICH service a label in a published host names, and `None` for a label that
/// names neither.
fn kind_of(label: &str) -> Option<PublishedService> {
    if label.eq_ignore_ascii_case(DATA_PLANE_SEGMENT) {
        return Some(PublishedService::Environment);
    }
    label
        .eq_ignore_ascii_case(LOAD_BALANCER_SEGMENT)
        .then_some(PublishedService::LoadBalancer)
}

/// The service a published host's own label names ([`kind_of`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishedService {
    /// This service, which assigns an environment its CNAME.
    Environment,
    /// Elastic Load Balancing, which names the load balancer in front of one.
    LoadBalancer,
}

impl Environment {
    /// `EnvironmentDescription$EndpointURL` — "for load-balanced, autoscaling
    /// environments, the URL to the LoadBalancer. For single-instance
    /// environments, the IP address of the instance".
    ///
    /// Derived from what this environment IS: the load balancer its stored
    /// inventory holds ([`EnvironmentResources::load_balancer`], which follows the
    /// tier and the `EnvironmentType` option it was launched or reconfigured with)
    /// is published by its own host, and an environment with no load balancer in
    /// front of it publishes the address its instances answer at. So a
    /// reconfiguration that takes the load balancer away, or a rebuild that
    /// recreates it under a new name, moves this member with it rather than
    /// leaving a stale address behind — and two reads of one environment report one
    /// address.
    pub fn endpoint_url(&self) -> Option<String> {
        Some(
            match self
                .resources
                .as_ref()
                .and_then(|resources| resources.load_balancer.as_deref())
            {
                Some(load_balancer) => {
                    load_balancer_host(&self.published_zone, &self.region, load_balancer)
                }
                None => self.instance_address.clone(),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a published address is a dot-separated host and nothing else — no
    /// port, no path — whose first label names the resource (the CNAME prefix, the
    /// load balancer), whose second is the Region, and which sits under the domain
    /// of the service that assigns it.
    #[test]
    fn a_published_address_is_a_host_naming_its_resource_and_its_region() {
        // The rows span what the minting generalises over: the Region, the length
        // and case of the leading label, the zone a load balancer is announced in,
        // and a Region no Region label admits.
        let rows: &[(&str, &str, &str, &str)] = &[
            ("a name", "localhost", "us-west-2", "published-name"),
            ("another region", "localhost", "eu-central-1", "published-name"),
            (
                "a region whose name has three parts",
                "localhost",
                "ap-southeast-2",
                "published-name",
            ),
            ("a named zone", "emulator.test", "us-west-2", "published-name"),
            ("the shortest prefix", "localhost", "us-west-2", "abcd"),
            ("a mixed-case prefix", "localhost", "us-west-2", "Published-NAME"),
            (
                "a region that is no region label",
                "localhost",
                "us-west-2/../another",
                "published-name",
            ),
            // A prefix carrying the delimiters that would move the authority of
            // the address this name IS.
            (
                "a prefix carrying an authority delimiter",
                "localhost",
                "us-west-2",
                "evil.example:8080/x",
            ),
        ];
        for (case, zone, region, prefix) in rows {
            // The name the prefix forms in Elastic Beanstalk's own zone, which the
            // service REPORTS ON rather than hands out as an address.
            let reported = fully_qualified_cname(region, prefix);
            assert!(
                reported.ends_with(&format!(".{CNAME_ZONE}")),
                "case {case:?}: expected the reported name in the service's own zone, got \
                 {reported:?}"
            );
            // And the ADDRESSES, which sit in the zone this emulator announces in
            // and are the AUTHORITY of an address a client dials: no delimiter a
            // request's value carried may reach them.
            let cname = environment_cname(zone, region, &new_environment_sub_domain(), prefix);
            let load_balancer_name = "awseb-e-abcdef-stack-AWSEBLoadBalancer-XYZ";
            for address in [
                cname.clone(),
                load_balancer_host(zone, region, load_balancer_name),
            ] {
                assert!(
                    !address.contains(':') && !address.contains('/'),
                    "case {case:?}: an address carries neither ':' nor '/', got {address:?}"
                );
                assert!(
                    address.split('.').all(|label| !label.is_empty()),
                    "case {case:?}: an address carries no empty label, got {address:?}"
                );
                assert!(
                    address.ends_with(&format!(".{zone}")),
                    "case {case:?}: expected an address this emulator announces in {zone:?}, got \
                     {address:?}"
                );
            }
            assert!(
                cname.ends_with(&format!(".{DATA_PLANE_SEGMENT}.{zone}")),
                "case {case:?}: expected an address under this service's own label, got {cname:?}"
            );
            assert!(
                load_balancer_host(zone, region, load_balancer_name)
                    .ends_with(&format!(".{LOAD_BALANCER_SEGMENT}.{zone}")),
                "case {case:?}: expected a load balancer under the load-balancing service's \
                 label, got {zone:?}"
            );
            // The first label of the ADDRESS is the prefix folded into one DNS
            // label, which is what keeps a delimiter in it from moving the
            // authority; the reported NAME carries the prefix as the caller wrote
            // it, since nothing dials it.
            assert_eq!(
                cname.split('.').next(),
                Some(addressing::dns_label(prefix).as_str()),
                "case {case:?}: expected the prefix as one DNS label, got {cname:?}"
            );
            assert!(
                reported.starts_with(&format!("{prefix}.")),
                "case {case:?}: expected the reported name to carry the prefix as written, got \
                 {reported:?}"
            );
            assert_eq!(
                reported,
                format!("{prefix}.{}.{CNAME_ZONE}", published_region(region)),
                "case {case:?}: expected the prefix and the Region ahead of the service's own \
                 zone"
            );
            // The ADDRESS reads back as that prefix in that Region, which is how a
            // request that named it is served.
            assert_eq!(
                published_host(&cname),
                Some(PublishedHost::Name {
                    region: published_region(region),
                    cname_prefix: addressing::dns_label(prefix).as_str(),
                }),
                "case {case:?}: expected {cname:?} to read back as a published name"
            );
            let load_balancer = load_balancer_host(zone, region, "AWSEBLoadBalancer-Xyz");
            assert_eq!(
                published_host(&load_balancer),
                Some(PublishedHost::LoadBalancer {
                    region: published_region(region),
                    load_balancer: "awsebloadbalancer-xyz",
                }),
                "case {case:?}: expected {load_balancer:?} to read back as a load balancer's host"
            );
            // The environment's address carries one label more than the reported
            // name — its own sub-domain — with the Region behind it.
            let published_labels: Vec<&str> = cname.split('.').collect();
            assert_eq!(
                published_labels.get(2).copied(),
                Some(published_region(region)),
                "case {case:?}: expected the Region behind the sub-domain, got {cname:?}"
            );
            assert_eq!(
                published_labels.len(),
                4 + zone.split('.').count(),
                "case {case:?}: expected the prefix, one sub-domain, the Region, this service's \
                 label and the zone, got {cname:?}"
            );
        }
    }

    /// RULE: every environment gets a sub-domain of its OWN, so no two
    /// environments are published under one name.
    #[test]
    fn each_environment_is_published_under_a_sub_domain_of_its_own() {
        let minted: std::collections::BTreeSet<String> =
            (0..16).map(|_| new_environment_sub_domain()).collect();
        assert_eq!(
            minted.len(),
            16,
            "expected a sub-domain of its own per environment, got {minted:?}"
        );
        for sub_domain in &minted {
            assert_eq!(
                addressing::dns_label(sub_domain),
                *sub_domain,
                "expected a sub-domain that is already one DNS label, got {sub_domain:?}"
            );
            assert!(
                sub_domain.starts_with(&format!("{ENVIRONMENT_SUB_DOMAIN_LABEL}-")),
                "expected the label Elastic Beanstalk mints a sub-domain under, got \
                 {sub_domain:?}"
            );
        }
    }

    /// RULE: a host that is no published address of this service reads back as
    /// none — whatever it is shaped like.
    #[test]
    fn a_host_that_names_no_published_address_reads_back_as_none() {
        // The rows span the ways a host misses the form: another service's label,
        // too few labels, an empty label, an address literal, and the emulator's
        // own endpoint.
        let rows: &[(&str, &str)] = &[
            ("another service's name", "queue.us-west-2.sqs.localhost"),
            ("no domain under the name", "published-name.us-west-2.elasticbeanstalk"),
            ("no service label", "published-name.us-west-2"),
            ("an empty leading label", ".us-west-2.elasticbeanstalk.localhost"),
            ("an address literal", "127.0.0.1"),
            ("an address literal with a port", "127.0.0.1:9297"),
            ("the emulator's own endpoint", "localhost"),
            ("nothing at all", ""),
            // This service's own API endpoint, which is no environment's address:
            // its leading label is the service's own.
            (
                "the service's regional endpoint",
                "elasticbeanstalk.us-west-2.amazonaws.com",
            ),
            // More labels than a published name carries between its prefix and
            // its Region.
            (
                "a name carrying more than one sub-domain",
                "published-name.eba-abcdef12.extra.us-west-2.elasticbeanstalk.com",
            ),
        ];
        for (case, host) in rows {
            let read = published_host(host);
            assert!(
                read.is_none(),
                "case {case:?}: expected {host:?} to name no published address, got {read:?}"
            );
        }
    }

    /// RULE: the port a client dialled is not part of the name, and neither is a
    /// trailing root dot or the case a client wrote the service label in.
    #[test]
    fn a_published_host_is_read_the_way_dns_compares_names() {
        let expected = PublishedHost::Name {
            region: "us-west-2",
            cname_prefix: "published-name",
        };
        for host in [
            "published-name.us-west-2.elasticbeanstalk.com",
            "published-name.us-west-2.elasticbeanstalk.com:8080",
            "published-name.us-west-2.elasticbeanstalk.com.",
            "published-name.us-west-2.ElasticBeanstalk.com",
            "published-name.us-west-2.elasticbeanstalk.localhost",
            "published-name.eba-abcdef12.us-west-2.elasticbeanstalk.com",
            "published-name.eba-abcdef12.us-west-2.elasticbeanstalk.com:8080",
        ] {
            assert_eq!(
                published_host(host),
                Some(expected),
                "case {host:?}: expected the same published name"
            );
        }
    }
}
