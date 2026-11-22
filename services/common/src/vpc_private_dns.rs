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
//! Two more belong to the service that owns the VPC's RESOLVER CONFIGURATION and its QUERY
//! LOGGING, and to no component that carries a query either:
//!
//! * whether the AUTODEFINED rules for reverse DNS lookups are in force in the VPC, which is
//!   what answers a reverse question about an address of it;
//! * WHERE a query that was answered is RECORDED: the query logging configurations associated
//!   with the VPC, what each delivers into and in which form are that service's own records, so
//!   a component carrying a query hands over the query and what it came to, and renders and
//!   delivers nothing itself.
//!
//! Beside them it carries the ONE reading of what a name NO zone reaching the VPC holds comes to
//! ([`established_not_to_exist`]), because every resolver of such a query decides that the same
//! way and none of them recurses anywhere.
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

/// The top-level domains the root zone NEVER delegates, so a name under one of them is
/// established NOT to exist rather than left unanswered.
///
/// Source: RFC 6761 §6, "Domain Name Reservation Considerations" — `invalid`, whose names "are
/// guaranteed not to exist", `test`, "intended for testing of current or new DNS related code",
/// and `example`, reserved for documentation. For each of them "DNS Registries/Registrars MUST
/// NOT grant requests to register" such a name, so no zone under one is ever delegated and a
/// recursion for it ends at the root with a name that does not exist.
///
/// `localhost` is reserved by the same section and is deliberately NOT here: that is a name the
/// standard fixes as EXISTING — it names the loopback interface of the machine asking — rather
/// than one it fixes as absent.
pub const NEVER_DELEGATED_TLDS: &[&str] = &["invalid", "test", "example"];

/// Whether the public namespace ESTABLISHES that `name` does not exist, which is the one part of
/// it a resolver of this emulator can answer for without recursing anywhere: a name under one of
/// [`NEVER_DELEGATED_TLDS`], or one of those domains itself.
///
/// This is the ONE reading of that question, asked by every in-VPC resolver of this emulator:
/// what each of them answers for a name no zone reaching the VPC holds is what a recursion would
/// have come to, and this says when that is a name that does not exist. A name under a zone
/// somebody OWNS is not one of them — nothing here can establish either that it exists or that
/// it does not — so such a name stays a query the resolver could not answer.
///
/// `name` may be spelled with the trailing dot of a fully qualified name and in any case.
#[must_use]
pub fn established_not_to_exist(name: &str) -> bool {
    let named = name.trim();
    let named = named.strip_suffix('.').unwrap_or(named);
    let last = named.split('.').next_back().unwrap_or_default();
    NEVER_DELEGATED_TLDS
        .iter()
        .any(|reserved| last.eq_ignore_ascii_case(reserved))
}

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

/// ONE record of the answer section a query was answered with, as a query LOG states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedAnswer {
    /// The record's value, as the wire carried it.
    pub rdata: String,
    /// The record's type (`A`, `CNAME`, …).
    pub record_type: String,
    /// The record's class (`IN`).
    pub class: String,
}

/// One query asked inside a VPC and ANSWERED, as the component that carried it states it to the
/// service that records queries.
///
/// Everything here is a fact of the exchange that just happened — the question as the client
/// asked it, the response code it was answered with, and the records it was answered with — so
/// nothing of a query log's own form is decided by the carrier. The VPC the query was asked in
/// and the address it came from travel beside it, in the same [`InVpcQuerySource`] every other
/// reading of such a query is made against.
#[derive(Debug, Clone)]
pub struct LoggedQuery<'a> {
    /// The name asked for, as the question carried it.
    pub name: &'a str,
    /// The type asked about (`A`, `PTR`, …), and the empty string for a type the carrier cannot
    /// name.
    pub record_type: &'a str,
    /// The class asked about (`IN`).
    pub class: &'a str,
    /// The response code the client was answered with (`NOERROR`, `NXDOMAIN`, `SERVFAIL`).
    pub response_code: &'a str,
    /// The answer section, in the order the client read it.
    pub answers: &'a [LoggedAnswer],
    /// The transport the query was carried over ([`PROTOCOL_UDP`], [`PROTOCOL_TCP`]).
    pub transport: &'a str,
    /// The resources the query came FROM, each as `(kind, id)` — the kind being how a query log
    /// names such a source ([`SOURCE_ID_INSTANCE`]). Empty where the carrier is no resource of
    /// the account the query can be attributed to.
    pub source_ids: &'a [(&'a str, &'a str)],
}

/// How a query log names an INTERNET-class question and record, which is the one class a query
/// asked inside a VPC is asked in: what [`LoggedQuery::class`] and [`LoggedAnswer::class`] carry.
pub const CLASS_INTERNET: &str = "IN";

/// How a query log names the RESOURCE a query came from: its `instance` member, which carries
/// the Amazon EC2 instance a query made on one came from — and, for a client that is no instance,
/// the elastic network interface of the VPC the query left through, which is the resource the
/// account's inventory holds the query's source address on and what a record of such a query
/// names there.
pub const SOURCE_ID_INSTANCE: &str = "instance";

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

    /// Whether the AUTODEFINED rules for reverse DNS lookups are in force in `vpc_id`, which is
    /// what answers a reverse question about an address of that VPC.
    fn answers_autodefined_reverse(&self, vpc_id: &str) -> bool;

    /// Record `query`, asked inside the VPC `source` names and already answered, wherever the
    /// account asked for its queries to be recorded.
    ///
    /// Called with the client ALREADY answered, so it decides nothing about the answer: a
    /// configuration that cannot be delivered into loses the record rather than failing a query.
    fn log_query(&self, source: &InVpcQuerySource<'_>, query: &LoggedQuery<'_>);
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

    /// [`VpcPrivateDns::answers_autodefined_reverse`] of the registered plane, and `true` where
    /// none is registered — nothing then says the rules a VPC has by default are off.
    ///
    /// The plane is cloned out of the record first, for the reason [`Self::admission`]'s is.
    #[must_use]
    pub fn answers_autodefined_reverse(&self, vpc_id: &str) -> bool {
        let Some(plane) = self.0.lock().plane.clone() else {
            return true;
        };
        plane.answers_autodefined_reverse(vpc_id)
    }

    /// [`VpcPrivateDns::log_query`] of the registered plane; an engine with none registered
    /// records no query, because no query logging configuration exists on it to record one in.
    ///
    /// The plane is cloned out of the record first, for the reason [`Self::admission`]'s is.
    pub fn log_query(&self, source: &InVpcQuerySource<'_>, query: &LoggedQuery<'_>) {
        let Some(plane) = self.0.lock().plane.clone() else {
            return;
        };
        plane.log_query(source, query);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the public namespace establishes that a name under a top-level domain the root zone
    /// never delegates does not exist, and establishes nothing about a name under a domain
    /// somebody owns.
    ///
    /// The rows span the catalog ([`NEVER_DELEGATED_TLDS`], RFC 6761 §6) and the forms a name is
    /// written in: each reserved domain itself and a name under it, the spelling with a trailing
    /// dot, mixed case, and the `localhost` the same section reserves as a name that DOES exist.
    #[test]
    fn only_a_name_the_root_zone_never_delegates_is_established_not_to_exist() {
        // (case, the name as it is asked, whether the namespace establishes it does not exist)
        let rows: &[(&str, &str, bool)] = &[
            ("a name under the reserved invalid domain", "db.nothing.invalid", true),
            ("the reserved invalid domain itself", "invalid", true),
            ("a name under the reserved test domain", "db.probe.example.test", true),
            ("the same name fully qualified", "db.probe.example.test.", true),
            ("a name under the reserved example domain", "host.example", true),
            ("one spelled in mixed case", "Host.INVALID", true),
            ("a name under a domain somebody owns", "broker.example.com", false),
            ("one under another such domain", "www.example.net", false),
            ("the loopback domain the same section reserves", "db.localhost", false),
            ("a name of a private zone under no reserved domain", "app.probe.internal", false),
            ("no name at all", "", false),
        ];
        for (case, name, expected) in rows {
            let decided = established_not_to_exist(name);
            assert_eq!(
                decided, *expected,
                "case {case:?}: expected established_not_to_exist({name:?}) to be {expected}, \
                 got {decided}"
            );
        }
    }

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
        /// The VPC it has the autodefined reverse rules turned OFF in.
        reverse_disabled_in: &'static str,
        /// Every query it was handed to record, as `(vpc, name, response code)`.
        recorded: ::std::sync::Mutex<Vec<(String, String, String)>>,
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

        fn answers_autodefined_reverse(&self, vpc_id: &str) -> bool {
            vpc_id != self.reverse_disabled_in
        }

        fn log_query(&self, source: &InVpcQuerySource<'_>, query: &LoggedQuery<'_>) {
            self.recorded.lock().expect("the record").push((
                source.vpc_id.to_string(),
                query.name.to_string(),
                query.response_code.to_string(),
            ));
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
            reverse_disabled_in: "vpc-2",
            recorded: ::std::sync::Mutex::new(Vec::new()),
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

    /// RULE: the two facts of the VPC's own Resolver behaviour the register carries — whether the
    /// autodefined reverse rules answer, and where an answered query is recorded — come from the
    /// plane the owning service registered, and an engine with none registered has the rules a
    /// VPC has by default and records nothing.
    #[test]
    fn the_registered_plane_states_the_reverse_rules_and_records_the_queries() {
        let engine = ::engine::InMemoryEngine::new();
        let plane = VpcPrivateDnsPlane::of(&engine).expect("the engine vends the record");
        // Nothing registered: the default is the documented one, and no query is recorded.
        assert!(
            plane.answers_autodefined_reverse("vpc-2"),
            "case \"nothing registered\": the autodefined reverse rules are on by default"
        );
        plane.log_query(&source("vpc-2", "10.91.1.20"), &query("app.example.internal."));
        let registered = Arc::new(OneEndpoint {
            vpc_id: "vpc-1",
            address: "10.91.1.10".parse().expect("an address"),
            admitted_source: "10.91.1.20",
            name: "app.example.internal.",
            reverse_disabled_in: "vpc-2",
            recorded: ::std::sync::Mutex::new(Vec::new()),
        });
        plane.registering(registered.clone());
        // (case, the VPC, whether the autodefined reverse rules answer in it)
        let rows: &[(&str, &str, bool)] = &[
            ("the VPC the rules are disabled in", "vpc-2", false),
            ("a VPC they are not disabled in", "vpc-1", true),
        ];
        for (case, vpc_id, expected) in rows {
            let answered = plane.answers_autodefined_reverse(vpc_id);
            assert_eq!(
                answered, *expected,
                "case {case:?}: expected {expected}, got {answered}"
            );
        }
        plane.log_query(&source("vpc-1", "10.91.1.20"), &query("app.example.internal."));
        plane.log_query(&source("vpc-1", "10.91.1.20"), &query("absent.example.internal."));
        let recorded = registered.recorded.lock().expect("the record").clone();
        assert_eq!(
            recorded,
            vec![
                (
                    "vpc-1".to_string(),
                    "app.example.internal.".to_string(),
                    "NOERROR".to_string()
                ),
                (
                    "vpc-1".to_string(),
                    "absent.example.internal.".to_string(),
                    "NOERROR".to_string()
                ),
            ],
            "case \"the queries handed over\": expected both, in the order asked, got {recorded:?}"
        );
    }

    /// One answered query of the rows above: the name, asked of the one class and type a row
    /// needs.
    fn query(name: &str) -> LoggedQuery<'_> {
        LoggedQuery {
            name,
            record_type: "A",
            class: "IN",
            response_code: "NOERROR",
            answers: &[],
            transport: PROTOCOL_UDP,
            source_ids: &[],
        }
    }
}
