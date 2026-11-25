// SPDX-License-Identifier: CC0-1.0

//! The ADDRESSES a service's listener answers at, published so a client INSIDE this emulator
//! that holds one of the addresses the control plane handed it reaches its data plane.
//!
//! A listener "is a process that checks for connection requests" on its port, so the pair — a
//! name the service is reached by and a listener's port — is an address a client calls the
//! service on. A client of this emulator that holds such an address has to be ROUTED to the
//! service that answers there: nothing about a name in this emulator's own zone says which
//! service answers it, so the service that opened the address records it, under the authority
//! a client writes, in the one register such a reader looks in
//! ([`::service_common::published_data_planes`]). A function this emulator runs then reaches a
//! Lattice service by that address, instead of being answered as a client of an address
//! nothing serves.
//!
//! WHICH authorities a listener ANSWERS at is what a client WRITES, and a client writes one of
//! the two addresses it was handed:
//!
//! * a name of the SERVICE — the service's own label ([`service_label`], the first label every
//!   host-style request this plane claims is resolved from) under the zone a resolver answers
//!   with the loopback interface, the interface the core binds its listeners on unless the
//!   operator names another address
//!   ([`::service_common::data_plane_addressing::LOOPBACK_ZONE`]), and the service's
//!   `customDomainName`, the other name a client reaches it by, where the service has one;
//! * the host the control plane ANNOUNCES in its `DnsEntry` ([`super::announced_dns_hosts`]),
//!   which is this emulator's OWN address: a client handed nothing but `dnsEntry.domainName`
//!   dials that host on the LISTENER's port, because "the public DNS name of the service"
//!   carries no port of its own and the port a client connects on is the listener's.
//!
//! Each of them in the two forms a client writes an address in: with the listener's port
//! (`shop-0123….localhost:80`), which is what a client that states the port writes, and — for
//! a listener on the default port of its protocol, "for HTTP, the default is 80. For HTTPS,
//! the default is 443" (`Target$port`, the same two defaults the `port` member of a listener
//! documents) — the name ALONE, because a URL on that port carries no port at all.
//!
//! The SHARED REGISTER gets every one of those authorities but the bare announced host: that
//! form is the address of this emulator's own FRONT DOOR, where every service's control plane
//! answers, and only a PORT tells a listener of this service from it. A client inside this
//! process that dials a URL writing no port dialled the default port of its scheme, which is
//! an authority that reader also looks the register up by, so the port-carrying record is what
//! routes such a client ([`::service_common::published_data_planes`]).
//!
//! The record is held for exactly as long as the LISTENER: it is kept on the store under the
//! listener's id, and dropping it withdraws every authority it published, so a listener that
//! is deleted — with its service, or on its own — leaves no address recorded as answering.
//!
//! A service with listeners on BOTH documented default ports publishes the bare name twice,
//! and the delete of either listener withdraws it: the record says only that THIS service's
//! crate answers at the authority, and which lattice service a request is for is read off its
//! host where the request is served (`crate::service::data_plane`), so nothing is mis-routed
//! either way.
//!
//! LOCK ORDER: the register is taken LAST. The listener's own create holds this service's
//! store while it records the addresses, and the register takes no other lock.

use ::service_common::data_plane_addressing::{host_port_authority, LOOPBACK_ZONE};
use ::service_common::published_data_planes::PublishedPlanes;

use super::{service_label, LatticeService, Listener};

/// The authorities ONE listener answers at, and the register they are published in.
///
/// Dropping it withdraws them, exactly as closing a port stops answering there, so the record
/// lives on the store under the listener's id and goes with the listener's own removal
/// ([`super::ComAmazonawsVpclatticeMercurycontrolplaneState::remove_listener`]).
#[derive(Debug)]
pub struct ListenerAddresses {
    /// Every `host[:port]` authority a client writes for this listener.
    authorities: Vec<String>,
    /// The subset recorded in the shared register, for the reason the module header gives.
    published: Vec<String>,
    /// Where they are recorded, and `None` for an engine that vends no register — in which
    /// case nothing was published and there is nothing to withdraw.
    register: Option<PublishedPlanes>,
}

impl ListenerAddresses {
    /// The authorities this record holds, for a caller that reports on what a listener answers
    /// at.
    #[must_use]
    pub fn authorities(&self) -> &[String] {
        &self.authorities
    }

    /// Whether this listener ANSWERS at `authority` — the `host[:port]` a request was sent to,
    /// compared as an address is: trimmed and without regard to case.
    ///
    /// The reading the data plane resolves a request addressed to the announced host by
    /// (`crate::service::data_plane`), so what a listener answers at is stated once, where it
    /// is recorded.
    #[must_use]
    pub fn answers_at(&self, authority: &str) -> bool {
        let addressed = authority.trim();
        self.authorities
            .iter()
            .any(|held| held.eq_ignore_ascii_case(addressed))
    }
}

impl Drop for ListenerAddresses {
    fn drop(&mut self) {
        let Some(register) = self.register.as_ref() else {
            return;
        };
        for authority in &self.published {
            register.withdraw(authority);
        }
    }
}

/// Publish the addresses `listener` answers at for `service`, and answer the record that keeps
/// them published.
///
/// Called by the create that opened the listener, with the request's own context, so a
/// listener publishes its addresses exactly once — when the port is opened — and withdraws
/// them when it is closed. `ctx` is what the ANNOUNCED hosts are read from
/// ([`super::announced_dns_hosts`]): the addresses this emulator hands clients in a
/// `DnsEntry`.
#[must_use]
pub fn published_listener_addresses<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    service: &LatticeService,
    listener: &Listener,
) -> ListenerAddresses {
    let register = PublishedPlanes::of(engine);
    let (authorities, published) =
        listener_authorities(service, listener, &super::announced_dns_hosts(ctx));
    if let Some(register) = register.as_ref() {
        for authority in &published {
            register.publish(authority, super::ARN_SERVICE);
        }
    }
    ListenerAddresses {
        authorities,
        published,
        register,
    }
}

/// Every authority a client writes for `listener` of `service` — the module header's list, in
/// that order, with no authority repeated — and the subset recorded in the shared register.
fn listener_authorities(
    service: &LatticeService,
    listener: &Listener,
    announced: &[String],
) -> (Vec<String>, Vec<String>) {
    let loopback = format!(
        "{}.{LOOPBACK_ZONE}",
        service_label(&service.name, &service.id)
    );
    let mut authorities: Vec<String> = Vec::new();
    let mut published: Vec<String> = Vec::new();
    let names = [Some(loopback), service.custom_domain_name.clone()];
    for name in names.into_iter().flatten() {
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        for authority in authority_forms(&name, listener) {
            if !authorities.contains(&authority) {
                authorities.push(authority.clone());
            }
            if !published.contains(&authority) {
                published.push(authority);
            }
        }
    }
    for host in announced {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            continue;
        }
        for form in authority_forms(&host, listener) {
            // The BARE form of this emulator's own host is its front door's address, which
            // only a port tells this listener from: it is answered here and kept out of the
            // shared register (see the module header).
            let bare = form == host;
            if !authorities.contains(&form) {
                authorities.push(form.clone());
            }
            if !bare && !published.contains(&form) {
                published.push(form);
            }
        }
    }
    (authorities, published)
}

/// The forms of ONE name a client writes for `listener`: the name with the listener's port,
/// and — on the default port of the listener's protocol — the name alone, because a URL on
/// that port carries no port.
fn authority_forms(name: &str, listener: &Listener) -> Vec<String> {
    let port = u16::try_from(listener.port).unwrap_or_default();
    let mut forms = vec![host_port_authority(name, port)];
    if super::default_port_for(&listener.protocol) == Some(listener.port) {
        forms.push(name.to_string());
    }
    forms
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A service for the rows below.
    fn service(name: &str, custom_domain_name: Option<&str>) -> LatticeService {
        LatticeService {
            id: "svc-0123456789abcdef0".to_string(),
            arn: "arn:aws:vpc-lattice:us-west-2:000000000000:service/svc-0123456789abcdef0"
                .to_string(),
            name: name.to_string(),
            custom_domain_name: custom_domain_name.map(ToString::to_string),
            certificate_arn: None,
            auth_type: "NONE".to_string(),
            idle_timeout_seconds: 60,
            dns_hosted_zone_id: "Z0123456789ABCDEFGHIJ".to_string(),
            tags: std::collections::HashMap::new(),
            created_at: std::time::SystemTime::UNIX_EPOCH,
            last_updated_at: std::time::SystemTime::UNIX_EPOCH,
            active_at: std::time::SystemTime::UNIX_EPOCH,
            sequence: 1,
        }
    }

    /// A listener for the rows below.
    fn listener(protocol: &str, port: i32) -> Listener {
        Listener {
            id: "listener-0123456789abcdef0".to_string(),
            arn: "arn:aws:vpc-lattice:us-west-2:000000000000:service/svc-0123456789abcdef0/\
                  listener/listener-0123456789abcdef0"
                .to_string(),
            name: "http".to_string(),
            protocol: protocol.to_string(),
            port,
            service_id: "svc-0123456789abcdef0".to_string(),
            service_arn:
                "arn:aws:vpc-lattice:us-west-2:000000000000:service/svc-0123456789abcdef0"
                    .to_string(),
            default_action: crate::ops::types::RuleAction::fixedResponse(
                crate::ops::types::FixedResponseAction { status_code: 404 },
            ),
            tags: std::collections::HashMap::new(),
            created_at: std::time::SystemTime::UNIX_EPOCH,
            last_updated_at: std::time::SystemTime::UNIX_EPOCH,
            sequence: 1,
        }
    }

    /// RULE: a name is published with the listener's port, and alone as well only on the
    /// default port of the listener's protocol — the port a URL carries no port for.
    ///
    /// The rows span what the rule generalises over: both protocols that carry HTTP requests,
    /// each on its documented default port and on a port of its own.
    #[test]
    fn a_name_is_published_with_its_port_and_alone_on_the_protocols_default() {
        let rows: &[(&str, &str, i32, &[&str])] = &[
            ("HTTP on its default port", "HTTP", 80, &["shop.localhost:80", "shop.localhost"]),
            ("HTTP on another port", "HTTP", 8080, &["shop.localhost:8080"]),
            (
                "HTTPS on its default port",
                "HTTPS",
                443,
                &["shop.localhost:443", "shop.localhost"],
            ),
            ("HTTPS on another port", "HTTPS", 8443, &["shop.localhost:8443"]),
        ];
        for (case, protocol, port, expected) in rows {
            let forms = authority_forms("shop.localhost", &listener(protocol, *port));
            assert_eq!(
                forms, *expected,
                "case {case:?}: expected {expected:?}, got {forms:?}"
            );
        }
    }

    /// RULE: a service's `customDomainName` is published beside its own label name, in the
    /// same two forms, and a name is published once however many ways it is reached.
    #[test]
    fn a_custom_domain_name_is_published_beside_the_label_name() {
        let service = service("shop", Some("Shop.Example.Test"));
        let listener = listener("HTTP", 80);
        let loopback = format!("shop-0123456789abcdef0.{LOOPBACK_ZONE}");
        let forms = authority_forms(&loopback, &listener);
        assert_eq!(
            forms,
            vec![format!("{loopback}:80"), loopback.clone()],
            "expected the service's own label name in both forms, got {forms:?}"
        );
        let custom = authority_forms("shop.example.test", &listener);
        assert_eq!(
            custom,
            vec!["shop.example.test:80".to_string(), "shop.example.test".to_string()],
            "expected the custom domain name in both forms, got {custom:?}"
        );
        assert_eq!(
            service.custom_domain_name.as_deref(),
            Some("Shop.Example.Test"),
            "the record keeps the custom domain name as the create sent it"
        );
    }

    /// RULE: a listener ANSWERS at every address the control plane handed a client — a name of
    /// the service and the host announced in its `dnsEntry`, each with the listener's port and,
    /// on the default port of its protocol, alone — while the SHARED REGISTER gets every one of
    /// them but the bare announced host, which is this emulator's own front door's address.
    ///
    /// The rows span what the rule generalises over: the announced-host forms a client is
    /// handed (an address, the loopback name, the unserved base, several of them at once, none
    /// at all), and the listener's port (a documented default and one of its own).
    #[test]
    fn a_listener_answers_at_every_address_the_control_plane_handed_a_client() {
        /// One row: a listener's protocol and port, the hosts the control plane announced,
        /// and the authorities those hosts add to what the listener answers at and to what
        /// the shared register holds — beside the service's own name, which every row has.
        struct Row {
            case: &'static str,
            protocol: &'static str,
            port: i32,
            announced: &'static [&'static str],
            answered: &'static [&'static str],
            published: &'static [&'static str],
        }
        let service = service("shop", None);
        let label = format!("shop-0123456789abcdef0.{LOOPBACK_ZONE}");
        let rows: &[Row] = &[
            Row {
                case: "one announced address, on the protocol's default port",
                protocol: "HTTP",
                port: 80,
                announced: &["127.0.0.1"],
                answered: &["127.0.0.1:80", "127.0.0.1"],
                published: &["127.0.0.1:80"],
            },
            Row {
                case: "the same, on a port of its own",
                protocol: "HTTP",
                port: 8080,
                announced: &["127.0.0.1"],
                answered: &["127.0.0.1:8080"],
                published: &["127.0.0.1:8080"],
            },
            Row {
                case: "the loopback name and the unserved base, both announced",
                protocol: "HTTPS",
                port: 443,
                announced: &["localhost", "emulator.invalid"],
                answered: &[
                    "localhost:443",
                    "localhost",
                    "emulator.invalid:443",
                    "emulator.invalid",
                ],
                published: &["localhost:443", "emulator.invalid:443"],
            },
            Row {
                case: "an announced host carrying the root dot and upper case",
                protocol: "HTTP",
                port: 80,
                announced: &["Emulator.Test."],
                answered: &["emulator.test:80", "emulator.test"],
                published: &["emulator.test:80"],
            },
            Row {
                case: "no announced host at all",
                protocol: "HTTP",
                port: 80,
                announced: &[],
                answered: &[],
                published: &[],
            },
        ];
        for row in rows {
            let case = row.case;
            let listener = listener(row.protocol, row.port);
            let announced: Vec<String> = row.announced.iter().map(ToString::to_string).collect();
            let (answered, published) = listener_authorities(&service, &listener, &announced);
            let own = authority_forms(&label, &listener);
            let expected_answered: Vec<String> = own
                .iter()
                .cloned()
                .chain(row.answered.iter().map(ToString::to_string))
                .collect();
            let expected_published: Vec<String> = own
                .iter()
                .cloned()
                .chain(row.published.iter().map(ToString::to_string))
                .collect();
            assert_eq!(
                answered, expected_answered,
                "case {case:?}: expected the addresses {expected_answered:?} to be answered, \
                 got {answered:?}"
            );
            assert_eq!(
                published, expected_published,
                "case {case:?}: expected {expected_published:?} in the shared register, got \
                 {published:?}"
            );
        }
    }

    /// RULE: a listener answers at an address compared as an address is — trimmed and without
    /// regard to case — and at no other.
    #[test]
    fn a_listener_answers_at_an_address_compared_as_an_address_is() {
        let listener = listener("HTTP", 80);
        let (authorities, published) = listener_authorities(
            &service("shop", None),
            &listener,
            &["127.0.0.1".to_string()],
        );
        let addresses = ListenerAddresses {
            authorities,
            published,
            register: None,
        };
        let rows: &[(&str, &str, bool)] = &[
            ("the announced host with the listener's port", "127.0.0.1:80", true),
            ("the announced host alone", "127.0.0.1", true),
            ("the same, padded as a header value may be", "  127.0.0.1  ", true),
            (
                "the service's own label name in another case",
                "SHOP-0123456789ABCDEF0.LOCALHOST",
                true,
            ),
            ("the announced host on another port", "127.0.0.1:9297", false),
            ("another host entirely", "10.1.2.3", false),
            ("no address at all", "", false),
        ];
        for (case, authority, expected) in rows {
            let answers = addresses.answers_at(authority);
            assert_eq!(
                answers, *expected,
                "case {case:?}: expected answers_at({authority:?}) to be {expected}, got \
                 {answers}"
            );
        }
    }
}
