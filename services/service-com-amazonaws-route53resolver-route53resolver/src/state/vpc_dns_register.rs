// SPDX-License-Identifier: CC0-1.0

//! This service's DNS data plane as the components of OTHER crates that carry an in-VPC query
//! reach it.
//!
//! A query sent from inside a VPC is carried by whatever runs the client — the datagram a
//! running function's socket hands over, the query a command run on a managed node makes — and
//! four of the facts that decide it are this service's and Route 53's rather than that
//! component's: WHICH addresses of the VPC answer DNS and whether the security groups on the
//! addressed interface admit the query, WHAT the private hosted zones associated with the
//! VPC hold for the name, whether the VPC's Resolver configuration still has the AUTODEFINED
//! rules for reverse DNS lookups, and WHERE a query that was answered is RECORDED. All four are
//! already one reading of those stores here ([`super::vpc_dns`], [`super::query_log_delivery`]),
//! so this module records that reading in the register the carrying components ask
//! ([`::service_common::vpc_private_dns`]) instead of either side holding a copy of the
//! endpoints, the groups, the zones or the configurations.
//!
//! The reading is taken PER QUESTION here, at the instant the query is carried, off the very
//! stores this service's and Route 53's own operations write — so revoking a security-group rule
//! or changing a record set is observed by the next query and not by the one after a cache.
//!
//! LOCK ORDER: every method below is called with NO store guard of the carrying component held,
//! and takes this service's, Amazon EC2's and Route 53's stores in turn, releasing each before
//! the next ([`super::vpc_dns::StoredVpcDns`]).

use ::std::net::Ipv4Addr;
use ::std::sync::Arc;

use ::service_common::vpc_private_dns::{
    InVpcQuerySource, LoggedQuery, PrivateZoneAnswer, PrivateZoneCode, PrivateZoneRecord,
    QueryAdmission, VpcPrivateDns, VpcPrivateDnsPlane,
};

use super::vpc_dns::{ResponseCode, StoredVpcDns, DNS_PORT};

impl VpcPrivateDns for StoredVpcDns {
    fn admission(
        &self,
        source: &InVpcQuerySource<'_>,
        server: Ipv4Addr,
        port: u16,
        protocol: &str,
    ) -> QueryAdmission {
        // DNS is served at ONE port (`domain`, 53): a datagram to any other port of an
        // endpoint's interface reaches no resolver, whatever the groups on it admit.
        if port != DNS_PORT {
            return QueryAdmission::NoServer;
        }
        let view =
            self.view_for_client(source.vpc_id, source.source_ip, source.security_group_ids);
        match view.admitted(server, protocol) {
            None => QueryAdmission::NoServer,
            Some(false) => QueryAdmission::Dropped,
            Some(true) => QueryAdmission::Admitted,
        }
    }

    fn zone_answer(
        &self,
        vpc_id: &str,
        name: &str,
        record_type: &str,
    ) -> Option<PrivateZoneAnswer> {
        // The zones alone: the firewall's decision about this query is read by the component
        // carrying it, through the register that owns that decision
        // ([`::service_common::vpc_dns_firewall`]), so it is not applied a second time here —
        // and the addresses that answer are read by `admission` above, so nothing of them is
        // read for a question.
        let answered = self.private_zone_answer(vpc_id, name, record_type)?;
        Some(PrivateZoneAnswer {
            code: match answered.code {
                ResponseCode::NxDomain => PrivateZoneCode::NxDomain,
                // A zone of the VPC holds the name, so the answer is its records or none of
                // that type; a zone answers no server failure — that is what a name under NO
                // zone of the VPC comes to, which is the `None` above.
                ResponseCode::NoError | ResponseCode::ServFail => PrivateZoneCode::NoError,
            },
            records: answered
                .records
                .into_iter()
                .map(|record| PrivateZoneRecord {
                    name: record.name,
                    ttl: record.ttl,
                    record_type: record.record_type,
                    value: record.value,
                })
                .collect(),
        })
    }

    fn answers_autodefined_reverse(&self, vpc_id: &str) -> bool {
        // The VPC's own Resolver behaviour configuration, read at the instant the query is
        // carried ([`StoredVpcDns::autodefined_reverse_answers`]).
        self.autodefined_reverse_answers(vpc_id)
    }

    fn log_query(&self, source: &InVpcQuerySource<'_>, query: &LoggedQuery<'_>) {
        // The configurations recording this VPC's queries, their destinations and the form of a
        // record are this service's own ([`super::query_log_delivery`]).
        self.record_query(source, query);
    }
}

/// Record this service as the Resolver data plane of `engine`'s VPCs, which the service does when
/// it is mounted on it ([`crate::service::EmulatedService::new`]).
///
/// A component of ANOTHER crate that carries a query asked inside a VPC then reads the addresses
/// that answer, what admits the query and what the private zones hold out of that register
/// instead of holding a copy of them. This crate's own answer paths take the view directly
/// ([`StoredVpcDns::view_from_instance`]), which is the same reading.
pub fn register_as_the_vpc_private_dns<Eng: Send + Sync + 'static>(engine: &Eng) {
    if let (Some(register), Some(plane)) = (
        VpcPrivateDnsPlane::of(engine),
        StoredVpcDns::of(engine),
    ) {
        register.registering(Arc::new(plane));
    }
}
