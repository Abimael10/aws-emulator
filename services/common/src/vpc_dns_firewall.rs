// SPDX-License-Identifier: CC0-1.0

//! What DNS FIREWALL decides about a query asked from inside a VPC, as the components that
//! ANSWER such a query reach it.
//!
//! A query a client inside a VPC sends to the Amazon-provided resolver is filtered before
//! anything resolves the name: the DNS Firewall rule groups that reach the VPC decide it, and
//! a name a matched `BLOCK` rule covers is answered that rule's way rather than out of any
//! zone. Those rule groups, their rules and their domain lists are the records of ONE service
//! — the one that owns DNS Firewall — and so is the Route 53 Profile a rule group can reach a
//! VPC through, so the decision is made there, over that service's own live records.
//!
//! This register is how a component that answers in-VPC queries asks for it: the owning
//! service records its filter when it is mounted on the engine, and whatever answers a query —
//! the resolver a running Lambda function's datagram reaches, the resolver a command run on an
//! instance queries — reads the decision out of the register instead of holding a copy of the
//! rules. Nothing is duplicated and nothing goes stale: a decision is read at the instant the
//! query is answered, off the very store the firewall's own operations write.
//!
//! Engine-scoped, like every other store two services on either side of a hand-off share: the
//! record lives in the SAME shared engine as the services' own, so the filter one service
//! registered is the one another consults, and two engines in one process share nothing. An
//! engine with no filter registered — one this emulator did not build, and one no DNS Firewall
//! service was mounted on — filters nothing, which is the position every query was in before a
//! rule group existed.
//!
//! LOCK ORDER: this record is taken LAST and released before the filter is called. The filter
//! itself takes the owning service's stores, and nothing here holds any other lock while it
//! asks, so no in-VPC answer path holds a store guard across a decision.

use std::sync::Arc;

/// The record type an `OVERRIDE` block answers with: the only type such a rule may name is a
/// `CNAME`, which is the type the custom record `BlockOverrideDomain` holds is.
pub const OVERRIDE_DNS_TYPE_CNAME: &str = "CNAME";

/// What the firewall decides about one query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirewallDecision {
    /// The query goes through: a rule that permits it matched.
    Allow,
    /// The query is answered as a name that does not exist, with no record.
    BlockNxDomain,
    /// The query is answered successfully, with no record.
    BlockNoData,
    /// The query is answered with the rule's OWN record.
    BlockOverride {
        /// The record's value: the custom domain the rule answers with.
        domain: String,
        /// The record's type ([`OVERRIDE_DNS_TYPE_CNAME`]).
        dns_type: String,
        /// How long the answer may be cached, in seconds.
        ttl: u32,
    },
}

/// The service that owns DNS Firewall, as the in-VPC answer paths reach it.
///
/// One implementation per engine: the service that holds the rule groups.
pub trait VpcQueryFilter: Send + Sync + 'static {
    /// What the firewall filtering `vpc_id` decides about EVERY query of one message, each
    /// given as the name asked for and the type as RFC 1035 numbers it — `None` for a type this
    /// emulator cannot number, which only a rule naming no type at all decides.
    ///
    /// One call per message rather than one per query: the rules are read ONCE for the whole
    /// message, so the work a message of many queries costs is one reading of the stores and
    /// not one per query. The answer holds one entry per query, in the order asked, `None`
    /// where no rule matched that query.
    fn decisions(
        &self,
        vpc_id: &str,
        asked: &[(String, Option<u16>)],
    ) -> Vec<Option<FirewallDecision>>;
}

/// One engine's registered filter.
#[derive(Default)]
struct RegisteredFilter {
    /// The filter the owning service recorded, or `None` until one does.
    filter: Option<Arc<dyn VpcQueryFilter>>,
}

/// Written by hand rather than derived: a filter carries no `Debug`, and what is diagnosable
/// about this record is whether one is registered at all.
impl std::fmt::Debug for RegisteredFilter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredFilter")
            .field("registered", &self.filter.is_some())
            .finish()
    }
}

/// A handle on one engine's registered filter.
///
/// Clone-cheap (an `Arc` inside) and `'static`, so a seam that outlives the request that built
/// it — a running function's own — can hold one.
#[derive(Clone)]
pub struct VpcQueryFilters(::engine::ServiceState<RegisteredFilter>);

/// Written by hand rather than derived, for the reason [`RegisteredFilter`]'s own is.
impl std::fmt::Debug for VpcQueryFilters {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VpcQueryFilters")
            .field("registered", &self.0.lock().filter.is_some())
            .finish()
    }
}

impl VpcQueryFilters {
    /// The record of the SAME shared engine the services' own stores live in, or `None` for an
    /// engine that vends none.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        (engine as &dyn ::std::any::Any)
            .downcast_ref::<::engine::InMemoryEngine>()
            .map(|engine| Self(engine.state::<RegisteredFilter>()))
    }

    /// Record `filter` as what decides this engine's in-VPC queries, replacing whatever was
    /// recorded: the service that owns DNS Firewall calls this when it is mounted, and mounting
    /// it again records the same decision over the same stores.
    pub fn registering(&self, filter: Arc<dyn VpcQueryFilter>) {
        self.0.lock().filter = Some(filter);
    }

    /// What the registered filter decides about every query of one message asked inside
    /// `vpc_id`, and an empty answer where nothing filters it.
    ///
    /// The filter is cloned out of the record first, so its own stores are taken with this
    /// record's guard already released, and it reads them ONCE for the whole message.
    #[must_use]
    pub fn decisions(
        &self,
        vpc_id: &str,
        asked: &[(String, Option<u16>)],
    ) -> Vec<Option<FirewallDecision>> {
        let Some(filter) = self.0.lock().filter.clone() else {
            return Vec::new();
        };
        filter.decisions(vpc_id, asked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A filter that blocks one name of one VPC, for the rows below.
    struct OneName {
        /// The VPC it filters.
        vpc_id: &'static str,
        /// The name it blocks there.
        name: &'static str,
    }

    impl VpcQueryFilter for OneName {
        fn decisions(
            &self,
            vpc_id: &str,
            asked: &[(String, Option<u16>)],
        ) -> Vec<Option<FirewallDecision>> {
            asked
                .iter()
                .map(|(name, _qtype)| {
                    (vpc_id == self.vpc_id && name == self.name)
                        .then_some(FirewallDecision::BlockNxDomain)
                })
                .collect()
        }
    }

    /// What the register decides about ONE query, for the rows below.
    fn decision(
        filters: &VpcQueryFilters,
        vpc_id: &str,
        name: &str,
    ) -> Option<FirewallDecision> {
        filters
            .decisions(vpc_id, &[(name.to_string(), Some(1))])
            .into_iter()
            .next()
            .flatten()
    }

    /// RULE: a query is decided by the filter the owning service registered on the SAME engine,
    /// and an engine with none registered filters nothing.
    #[test]
    fn the_registered_filter_decides_the_queries_of_its_own_engine() {
        let engine = ::engine::InMemoryEngine::new();
        let other = ::engine::InMemoryEngine::new();
        let filters = VpcQueryFilters::of(&engine).expect("the engine vends the record");
        assert_eq!(
            decision(&filters, "vpc-1", "blocked.example.com"),
            None,
            "case \"nothing registered\": expected no decision, got one"
        );
        filters.registering(Arc::new(OneName {
            vpc_id: "vpc-1",
            name: "blocked.example.com",
        }));
        let rows: &[(&str, &str, &str, Option<FirewallDecision>)] = &[
            (
                "the name the filter blocks in the VPC it filters",
                "vpc-1",
                "blocked.example.com",
                Some(FirewallDecision::BlockNxDomain),
            ),
            (
                "another name of that VPC",
                "vpc-1",
                "control.example.net",
                None,
            ),
            (
                "the blocked name in another VPC",
                "vpc-2",
                "blocked.example.com",
                None,
            ),
        ];
        for (case, vpc_id, name, expected) in rows {
            let decided = decision(&filters, vpc_id, name);
            assert_eq!(
                &decided, expected,
                "case {case:?}: expected {expected:?}, got {decided:?}"
            );
        }
        // Another engine shares nothing: its own record holds no filter.
        let elsewhere = VpcQueryFilters::of(&other).expect("the other engine vends the record");
        assert_eq!(
            decision(&elsewhere, "vpc-1", "blocked.example.com"),
            None,
            "case \"another engine\": expected no decision, got one"
        );
    }
}
