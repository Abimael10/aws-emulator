// SPDX-License-Identifier: CC0-1.0

//! What the ROUTE 53 RESOLVER data plane of a VPC answers a query asked inside that VPC, as the
//! components that CARRY such a query reach it.
//!
//! Two facts about a query sent from inside a VPC belong to the service that owns Resolver
//! endpoints and the one that owns hosted zones, and to no component that carries the query:
//!
//! * WHICH addresses of the VPC answer DNS, and whether the query is admitted to the one it was
//!   sent to. An inbound Resolver endpoint adds addresses of its own, in the subnets it was
//!   created in, and those addresses are addresses of ELASTIC NETWORK INTERFACES — so a query
//!   reaching one is delivered only where a security group of that endpoint admits the query's
//!   protocol and port from the querying address, and one no rule admits is DROPPED rather than
//!   refused. The VPC's own resolver is behind no such interface and admits every client of the
//!   VPC.
//! * WHAT the PRIVATE HOSTED ZONES associated with the VPC hold for the name asked for, which is
//!   what such a query is answered out of ahead of anything the VPC's own internal zone holds.
//!
//! This register is how a component that carries an in-VPC query asks for them: the owning
//! service records its data plane when it is mounted on the engine, and whatever carries a query
//! — the datagram a running function sends, the query a command run on an instance makes — reads
//! the answer out of the register instead of holding a copy of the endpoints, the groups or the
//! zones. Nothing is duplicated and nothing goes stale: the answer is read at the instant the
//! query is carried, off the very stores those services' own operations write.
//!
//! Engine-scoped, like every other store two services on either side of a hand-off share: the
//! record lives in the SAME shared engine as the services' own, so the plane one service
//! registered is the one another consults, and two engines in one process share nothing. An
//! engine with no plane registered — one this emulator did not build, and one no Resolver service
//! was mounted on — has no Resolver endpoint and no private zone in any VPC, which is the
//! position every query was in before one existed.
//!
//! LOCK ORDER: this record is taken LAST and released before the plane is asked. The plane itself
//! takes the owning services' stores, and nothing here holds any other lock while it asks, so no
//! in-VPC answer path holds a store guard across one of these readings.

use std::net::Ipv4Addr;
use std::sync::Arc;

/// The protocol keyword of a datagram query, as a security-group rule states one.
pub const PROTOCOL_UDP: &str = "udp";

/// The protocol keyword of a query carried on a connection, as a rule states one.
pub const PROTOCOL_TCP: &str = "tcp";

/// Where a query asked inside a VPC comes FROM: the three facts a security group on the
/// addressed interface is matched against.
#[derive(Debug, Clone, Copy)]
pub struct InVpcQuerySource<'a> {
    /// The VPC the client sits in, which decides both which addresses answer and which zones
    /// they answer out of.
    pub vpc_id: &'a str,
    /// The client's own private address, which is the SOURCE a rule admits or does not.
    pub source_ip: &'a str,
    /// The security groups the client is a member of, by id — what a rule whose source is a
    /// security group is matched against.
    pub security_group_ids: &'a [String],
}

/// Whether DNS is answered at one address of a VPC for one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryAdmission {
    /// Nothing of that VPC answers DNS at that address and port, so the query reaches no
    /// resolver at all.
    NoServer,
    /// The address IS one a resolver of the VPC answers at, but no security group on its
    /// interface admits such a query from this source: the packet is DROPPED, so the client is
    /// answered with nothing rather than with a refusal.
    Dropped,
    /// A resolver of the VPC answers there and the query is admitted to it.
    Admitted,
}

/// The response code a private hosted zone's answer carries (RFC 1035 header `RCODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateZoneCode {
    /// The name exists in the zone: the records are what was asked for, and an empty list is a
    /// name that carries no record of the type asked about.
    NoError,
    /// The zone holds no such name.
    NxDomain,
}

/// One record of a private hosted zone's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateZoneRecord {
    /// The owner name, fully qualified.
    pub name: String,
    /// The record set's TTL, in seconds.
    pub ttl: i64,
    /// The record type (`A`, `CNAME`, …).
    pub record_type: String,
    /// One record's value, as the record set holds it.
    pub value: String,
}

/// What the private hosted zones of a VPC hold for one question: the status, and the records of
/// the answer section in the order a resolver puts them (an alias before what it resolves to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateZoneAnswer {
    /// The response code the answer carries.
    pub code: PrivateZoneCode,
    /// The answer section.
    pub records: Vec<PrivateZoneRecord>,
}

/// The service that owns the Resolver data plane of a VPC, as the components that carry an
/// in-VPC query reach it.
///
/// One implementation per engine: the service that holds the Resolver endpoints, read against
/// the hosted zones and the security groups of the same account.
pub trait VpcPrivateDns: Send + Sync + 'static {
    /// Whether a query from `source` over `protocol` to `server`:`port` reaches a resolver of
    /// the VPC, and whether it is admitted to it.
    fn admission(
        &self,
        source: &InVpcQuerySource<'_>,
        server: Ipv4Addr,
        port: u16,
        protocol: &str,
    ) -> QueryAdmission;

    /// What the private hosted zones associated with `vpc_id` hold for `name` of `record_type`,
    /// and `None` when no zone of that VPC holds the name at all — which is a question those
    /// zones do not answer, rather than one they answer with no record.
    fn zone_answer(
        &self,
        vpc_id: &str,
        name: &str,
        record_type: &str,
    ) -> Option<PrivateZoneAnswer>;
}

/// One engine's registered plane.
#[derive(Default)]
struct RegisteredPlane {
    /// The plane the owning service recorded, or `None` until one does.
    plane: Option<Arc<dyn VpcPrivateDns>>,
}

/// Written by hand rather than derived: a plane carries no `Debug`, and what is diagnosable
/// about this record is whether one is registered at all.
impl std::fmt::Debug for RegisteredPlane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredPlane")
            .field("registered", &self.plane.is_some())
            .finish()
    }
}

/// A handle on one engine's registered plane.
///
/// Clone-cheap (an `Arc` inside) and `'static`, so a seam that outlives the request that built
/// it — a running function's own — can hold one.
#[derive(Clone)]
pub struct VpcPrivateDnsPlane(::engine::ServiceState<RegisteredPlane>);

/// Written by hand rather than derived, for the reason [`RegisteredPlane`]'s own is.
impl std::fmt::Debug for VpcPrivateDnsPlane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VpcPrivateDnsPlane")
            .field("registered", &self.0.lock().plane.is_some())
            .finish()
    }
}

impl VpcPrivateDnsPlane {
    /// The record of the SAME shared engine the services' own stores live in, or `None` for an
    /// engine that vends none.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        (engine as &dyn ::std::any::Any)
            .downcast_ref::<::engine::InMemoryEngine>()
            .map(|engine| Self(engine.state::<RegisteredPlane>()))
    }

    /// Record `plane` as this engine's Resolver data plane, replacing whatever was recorded: the
    /// service that owns Resolver endpoints calls this when it is mounted, and mounting it again
    /// records the same reading over the same stores.
    pub fn registering(&self, plane: Arc<dyn VpcPrivateDns>) {
        self.0.lock().plane = Some(plane);
    }

    /// [`VpcPrivateDns::admission`] of the registered plane, and [`QueryAdmission::NoServer`]
    /// where none is registered — then no Resolver endpoint answers anywhere.
    ///
    /// The plane is cloned out of the record first, so its own stores are taken with this
    /// record's guard already released.
    #[must_use]
    pub fn admission(
        &self,
        source: &InVpcQuerySource<'_>,
        server: Ipv4Addr,
        port: u16,
        protocol: &str,
    ) -> QueryAdmission {
        let Some(plane) = self.0.lock().plane.clone() else {
            return QueryAdmission::NoServer;
        };
        plane.admission(source, server, port, protocol)
    }

    /// [`VpcPrivateDns::zone_answer`] of the registered plane, and `None` where none is
    /// registered — then no private hosted zone answers anything.
    #[must_use]
    pub fn zone_answer(
        &self,
        vpc_id: &str,
        name: &str,
        record_type: &str,
    ) -> Option<PrivateZoneAnswer> {
        let plane = self.0.lock().plane.clone()?;
        plane.zone_answer(vpc_id, name, record_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plane holding one endpoint address and one name in one VPC, for the rows below.
    struct OneEndpoint {
        /// The VPC it answers in.
        vpc_id: &'static str,
        /// The address its endpoint answers at.
        address: Ipv4Addr,
        /// The source its security group admits.
        admitted_source: &'static str,
        /// The name its zone holds.
        name: &'static str,
    }

    impl VpcPrivateDns for OneEndpoint {
        fn admission(
            &self,
            source: &InVpcQuerySource<'_>,
            server: Ipv4Addr,
            port: u16,
            _protocol: &str,
        ) -> QueryAdmission {
            if source.vpc_id != self.vpc_id || server != self.address || port != 53 {
                return QueryAdmission::NoServer;
            }
            if source.source_ip == self.admitted_source {
                QueryAdmission::Admitted
            } else {
                QueryAdmission::Dropped
            }
        }

        fn zone_answer(
            &self,
            vpc_id: &str,
            name: &str,
            record_type: &str,
        ) -> Option<PrivateZoneAnswer> {
            if vpc_id != self.vpc_id || name != self.name {
                return None;
            }
            Some(PrivateZoneAnswer {
                code: PrivateZoneCode::NoError,
                records: vec![PrivateZoneRecord {
                    name: name.to_string(),
                    ttl: 300,
                    record_type: record_type.to_string(),
                    value: "192.0.2.10".to_string(),
                }],
            })
        }
    }

    /// The source the rows below ask as.
    fn source<'a>(vpc_id: &'a str, source_ip: &'a str) -> InVpcQuerySource<'a> {
        InVpcQuerySource {
            vpc_id,
            source_ip,
            security_group_ids: &[],
        }
    }

    /// RULE: an in-VPC query is decided by the plane the owning service registered on the SAME
    /// engine, and an engine with none registered has no Resolver endpoint and no private zone.
    #[test]
    fn the_registered_plane_answers_the_queries_of_its_own_engine() {
        let engine = ::engine::InMemoryEngine::new();
        let other = ::engine::InMemoryEngine::new();
        let plane = VpcPrivateDnsPlane::of(&engine).expect("the engine vends the record");
        let address: Ipv4Addr = "10.91.1.10".parse().expect("an address");
        assert_eq!(
            plane.admission(&source("vpc-1", "10.91.1.20"), address, 53, PROTOCOL_UDP),
            QueryAdmission::NoServer,
            "case \"nothing registered\": expected no server, got one"
        );
        assert_eq!(
            plane.zone_answer("vpc-1", "app.example.internal.", "A"),
            None,
            "case \"nothing registered\": expected no zone, got one"
        );
        plane.registering(Arc::new(OneEndpoint {
            vpc_id: "vpc-1",
            address,
            admitted_source: "10.91.1.20",
            name: "app.example.internal.",
        }));
        // (case, the VPC the query is asked in, the client's address, the address it is sent to,
        // what the plane answers)
        let rows: &[(&str, &str, &str, &str, QueryAdmission)] = &[
            (
                "the admitted source at the endpoint's address",
                "vpc-1",
                "10.91.1.20",
                "10.91.1.10",
                QueryAdmission::Admitted,
            ),
            (
                "another source at the same address",
                "vpc-1",
                "10.91.2.30",
                "10.91.1.10",
                QueryAdmission::Dropped,
            ),
            (
                "an address nothing answers at",
                "vpc-1",
                "10.91.1.20",
                "10.91.9.9",
                QueryAdmission::NoServer,
            ),
            (
                "the endpoint's address in another VPC",
                "vpc-2",
                "10.91.1.20",
                "10.91.1.10",
                QueryAdmission::NoServer,
            ),
        ];
        for (case, vpc_id, source_ip, server, expected) in rows {
            let server: Ipv4Addr = server.parse().expect("an address");
            let decided = plane.admission(&source(vpc_id, source_ip), server, 53, PROTOCOL_UDP);
            assert_eq!(
                decided, *expected,
                "case {case:?}: expected {expected:?}, got {decided:?}"
            );
        }
        let answered = plane.zone_answer("vpc-1", "app.example.internal.", "A");
        assert_eq!(
            answered.map(|answer| answer.records.len()),
            Some(1),
            "case \"the name the zone holds\": expected one record"
        );
        assert_eq!(
            plane.zone_answer("vpc-1", "absent.example.internal.", "A"),
            None,
            "case \"a name under no zone\": expected no answer, got one"
        );
        // Another engine shares nothing: its own record holds no plane.
        let elsewhere = VpcPrivateDnsPlane::of(&other).expect("the other engine vends the record");
        assert_eq!(
            elsewhere.admission(&source("vpc-1", "10.91.1.20"), address, 53, PROTOCOL_UDP),
            QueryAdmission::NoServer,
            "case \"another engine\": expected no server, got one"
        );
    }
}
