// SPDX-License-Identifier: CC0-1.0

//! The ADDRESSES a service's listener answers at, published so a client INSIDE this emulator
//! that reads the service's own DNS name out of the control plane reaches its data plane.
//!
//! `CreateService`'s `dnsEntry` is "The public DNS name of the service" and a listener "is a
//! process that checks for connection requests" on its port, so the pair — the announced name
//! and a listener's port — is the address a client was told to call the service on. A client of
//! this emulator that holds such an address has to be ROUTED to the service that answers
//! there: nothing about a name in this emulator's own zone says which service answers it, so
//! the service that opened the address records it, under the authority a client writes, in the
//! one register such a reader looks in ([`::service_common::published_data_planes`]). A
//! function this emulator runs then reaches a Lattice service by the very name the control
//! plane published, instead of being answered as a client of an address nothing serves.
//!
//! WHICH authorities are published is what a client WRITES, and a client writes the name it
//! was handed:
//!
//! * the announced name with the listener's port (`shop-0123….localhost:80`), which is what a
//!   client that states the port writes;
//! * the announced name ALONE, for a listener on the default port of its protocol — "for HTTP,
//!   the default is 80. For HTTPS, the default is 443" (`Target$port`, the same two defaults
//!   the `port` member of a listener documents) — because a URL on that port carries no port
//!   at all;
//! * the same two forms of the service's `customDomainName`, the other name a client reaches
//!   it by, where the service has one;
//! * the same two forms under the zone this emulator answers a client at an ADDRESS in
//!   ([`::service_common::data_plane_addressing::LOOPBACK_ZONE`]), so a client that read the
//!   name from a request that reached this emulator on a name still resolves it where names of
//!   that zone are the ones a resolver answers.
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

use ::service_common::data_plane_addressing::LOOPBACK_ZONE;
use ::service_common::published_data_planes::PublishedPlanes;

use super::{announced_dns_name, service_label, LatticeService, Listener};

/// The authorities ONE listener answers at, and the register they are published in.
///
/// Dropping it withdraws them, exactly as closing a port stops answering there, so the record
/// lives on the store under the listener's id and goes with the listener's own removal
/// ([`super::ComAmazonawsVpclatticeMercurycontrolplaneState::remove_listener`]).
#[derive(Debug)]
pub struct ListenerAddresses {
    /// The `host[:port]` authorities a client writes for this listener.
    authorities: Vec<String>,
    /// Where they are recorded, and `None` for an engine that vends no register — in which
    /// case nothing was published and there is nothing to withdraw.
    published: Option<PublishedPlanes>,
}

impl ListenerAddresses {
    /// The authorities this record holds, for a caller that reports on what a listener answers
    /// at.
    #[must_use]
    pub fn authorities(&self) -> &[String] {
        &self.authorities
    }
}

impl Drop for ListenerAddresses {
    fn drop(&mut self) {
        let Some(published) = self.published.as_ref() else {
            return;
        };
        for authority in &self.authorities {
            published.withdraw(authority);
        }
    }
}

/// Publish the addresses `listener` answers at for `service`, and answer the record that keeps
/// them published.
///
/// Called by the create that opened the listener, with the request's own context: the zone the
/// announced name is minted in is the one this emulator answers the CALLER in
/// ([`announced_dns_name`]), so the name published here is the name that create reported.
#[must_use]
pub fn published_listener_addresses<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    service: &LatticeService,
    listener: &Listener,
) -> ListenerAddresses {
    let published = PublishedPlanes::of(engine);
    let authorities = listener_authorities(ctx, service, listener);
    if let Some(published) = published.as_ref() {
        for authority in &authorities {
            published.publish(authority, super::ARN_SERVICE);
        }
    }
    ListenerAddresses {
        authorities,
        published,
    }
}

/// Every authority a client writes for `listener` of `service` — the module header's list, in
/// that order, with no authority repeated.
fn listener_authorities(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    service: &LatticeService,
    listener: &Listener,
) -> Vec<String> {
    let announced = announced_dns_name(ctx, service.announced());
    let loopback = format!(
        "{}.{LOOPBACK_ZONE}",
        service_label(&service.name, &service.id)
    );
    let mut authorities: Vec<String> = Vec::new();
    let names = [
        Some(announced),
        Some(loopback),
        service.custom_domain_name.clone(),
    ];
    for name in names.into_iter().flatten() {
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        for authority in authority_forms(&name, listener) {
            if !authorities.contains(&authority) {
                authorities.push(authority);
            }
        }
    }
    authorities
}

/// The forms of ONE name a client writes for `listener`: the name with the listener's port,
/// and — on the default port of the listener's protocol — the name alone, because a URL on
/// that port carries no port.
fn authority_forms(name: &str, listener: &Listener) -> Vec<String> {
    let mut forms = vec![format!("{name}:{}", listener.port)];
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

    /// RULE: a service's `customDomainName` is published beside its announced name, in the same
    /// two forms, and a name is published once however many ways it is reached.
    #[test]
    fn a_custom_domain_name_is_published_beside_the_announced_one() {
        let service = service("shop", Some("Shop.Example.Test"));
        let listener = listener("HTTP", 80);
        let loopback = format!("shop-0123456789abcdef0.{LOOPBACK_ZONE}");
        let forms = authority_forms(&loopback, &listener);
        assert_eq!(
            forms,
            vec![format!("{loopback}:80"), loopback.clone()],
            "expected the announced name in both forms, got {forms:?}"
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
}
