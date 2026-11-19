// SPDX-License-Identifier: CC0-1.0

//! Whether a collection's own endpoints are reachable at all: the `network` security
//! policies that admit traffic to them.
//!
//! A `network` security policy is what "specifies access to collections and their
//! endpoints" (`SecurityPolicyType$network`), and AWS's "Network access for Amazon
//! OpenSearch Serverless" states the rule a collection's endpoints are reached under: the
//! endpoints a collection publishes are reachable only through a network policy whose
//! rules name that collection — "you must have at least one network policy that applies
//! to a collection in order to access its endpoint" — and only FROM a source that policy
//! states. A collection no network policy covers publishes an address nothing is admitted
//! to, whichever principal dials it and whatever the data access policies say, which is why
//! this decision is taken before any other.
//!
//! The DOCUMENT is read the same way every other policy document of this service is: a
//! statement, or a list of them, each carrying `Rules` whose entries name a `ResourceType`
//! and the `Resource` patterns they apply to ([`super::policy_document_resources`] walks
//! the same three levels). So whether a stored policy's rules cover `collection/<name>` is
//! the same covering relation an encryption lookup
//! ([`super::ComAmazonawsOpensearchserverlessOpensearchserverlessState::policy_encryption_for`])
//! is answered by, and nothing is re-implemented for it. The SOURCES sit on the statement
//! that carries those rules, which is why the two are read together here: a collection
//! answers a request when ONE statement both covers it and states a source that request is
//! from.
//!
//! # Which sources a statement states
//!
//! A statement states where traffic may reach the collection from — `AllowFromPublic: true`
//! for anywhere, `SourceVPCEs` for the managed interface endpoints it names, and
//! `SourceServices` for the AWS services whose own traffic it admits. Narrowing those
//! sources therefore takes the endpoint out of service for a request from outside them, and
//! widening them again puts it back, because this decision is read off the policy state the
//! control plane holds at that instant.
//!
//! # Where a request is from
//!
//! Nothing a request carries says which network it crossed: every client of this emulator
//! dials the one address this process serves the collection at
//! ([`super::collection_plane`]), and neither that address nor any header of the request
//! names a VPC. Two things that ARE resolved place it:
//!
//! * the account's DEFAULT network. A managed interface endpoint is placed in a VPC, and EC2
//!   reports whether that VPC is the one AWS gives an account that created none of its own
//!   ([`super::ec2_network::Placement::in_default_vpc`], read when the endpoint is created
//!   and held on its record) — which is the network a client that built no network of its
//!   own is in. An endpoint carries traffic in the one status it settles into, so an endpoint
//!   that is not [`super::VPC_ENDPOINT_ACTIVE`] places nothing;
//! * the attachment the collection's endpoint was PUBLISHED behind
//!   ([`super::CollectionRecord::published_behind`]): the interface endpoints its own
//!   covering statements stated when that address was opened. An address that was reachable
//!   through a given interface endpoint when it opened goes on being reachable through it,
//!   so a client dialling it is one those statements already admitted.
//!
//! A refusal is accordingly GROUNDED in what the account holds and what was resolved about
//! it, never assumed from silence — the discipline the resolving gate that feeds it keeps too
//! ([`super::ec2_network`]). So nothing is refused on:
//!
//! * a statement that states no `SourceVPCEs` at all, which states nothing about where a
//!   request comes from;
//! * a `SourceVPCEs` entry the account holds no endpoint for, which places nobody — the
//!   policy was written against an endpoint of another account, or one since deleted;
//! * any statement at all for a collection NOTHING places a request to: the account holds no
//!   endpoint in its default network and the collection was published behind none either, so
//!   no stated source is evidence that a request is from somewhere else.
//!
//! What refuses is a statement whose every stated source is an endpoint of the account that
//! is neither in the network a request to this collection is placed in nor one the
//! collection's address was published behind.

use ::aws_smithy_types::Document;

use super::ComAmazonawsOpensearchserverlessOpensearchserverlessState as ServiceState;

/// The `SecurityPolicyType` whose policies say which traffic reaches a collection's
/// endpoints (`smithy_query(query="shape", shape="SecurityPolicyType")` -> `network`).
pub const SECURITY_POLICY_TYPE_NETWORK: &str = "network";

/// The statement member that admits traffic from ANYWHERE, as AWS's "Network access for
/// Amazon OpenSearch Serverless" spells it.
const ALLOW_FROM_PUBLIC: &str = "AllowFromPublic";

/// The statement member naming the managed interface endpoints traffic may reach the
/// collection through, spelled as that same documentation spells it.
const SOURCE_VPCES: &str = "SourceVPCEs";

/// The statement member naming the AWS services whose own traffic is admitted.
const SOURCE_SERVICES: &str = "SourceServices";

/// Where a request to one collection is placed: the interface endpoints of the account that
/// a request dialling that collection's address comes through (see the module docs).
#[derive(Debug, Default)]
struct Placed<'a> {
    /// The endpoints the account holds in its default network.
    in_default_network: Vec<&'a str>,
    /// The endpoints this collection's address was published behind.
    published_behind: &'a [String],
}

impl Placed<'_> {
    /// Whether nothing places a request to this collection, in which case no stated source
    /// is evidence that one comes from somewhere else.
    fn nowhere(&self) -> bool {
        self.in_default_network.is_empty() && self.published_behind.is_empty()
    }

    /// Whether the endpoint `id` names is one a request to this collection comes through.
    fn names(&self, id: &str) -> bool {
        self.in_default_network.contains(&id)
            || self.published_behind.iter().any(|behind| behind == id)
    }
}

impl ServiceState {
    /// Whether a stored `network` security policy admits this request to the endpoints the
    /// collection `name` publishes: one whose rules cover the collection and whose statement
    /// states a source the request is from (see the module docs).
    ///
    /// `false` for a collection no such policy names, and for one whose policies state only
    /// sources the request is not from: every request to its endpoints is refused, which is
    /// what this service's data plane answers for one.
    pub fn network_policy_admits(&self, name: &str) -> bool {
        let resource = format!("{}/{name}", super::POLICY_COLLECTION_RESOURCE_TYPE);
        // Where a request to this collection is placed, which is the same for every
        // statement read below.
        let placed = self.placed(name);
        self.security_policies
            .iter()
            .filter(|((kind, _), _)| kind == SECURITY_POLICY_TYPE_NETWORK)
            .any(|(_, record)| self.policy_admits(&record.policy, &resource, &placed))
    }

    /// The managed interface endpoints the `network` statements covering the collection
    /// `name` state as their sources, in the order the policies are stored and without
    /// repeats.
    ///
    /// What the create of a collection records as the attachment its address is published
    /// behind ([`super::CollectionRecord::published_behind`]).
    ///
    /// At most [`super::MAX_POLICY_RESOURCES`] ids are collected and each is at most
    /// [`super::MAX_RESOURCE_PATTERN_LENGTH`] bytes, exactly as
    /// [`super::policy_document_resources`] holds the resources of a document, so neither
    /// the length of this list nor the cost of one comparison against it is chosen by
    /// whoever wrote the document.
    pub fn network_statement_source_endpoints(&self, name: &str) -> Vec<String> {
        let resource = format!("{}/{name}", super::POLICY_COLLECTION_RESOURCE_TYPE);
        let mut stated: Vec<String> = Vec::new();
        for record in self
            .security_policies
            .iter()
            .filter(|((kind, _), _)| kind == SECURITY_POLICY_TYPE_NETWORK)
            .map(|(_, record)| record)
        {
            for statement in super::policy_document_entries(Some(&record.policy)) {
                if !super::statement_covers(statement, &resource) {
                    continue;
                }
                for id in source_endpoints(statement) {
                    if stated.len() >= super::MAX_POLICY_RESOURCES {
                        return stated;
                    }
                    if id.len() <= super::MAX_RESOURCE_PATTERN_LENGTH
                        && !stated.iter().any(|already| already == id)
                    {
                        stated.push(id.clone());
                    }
                }
            }
        }
        stated
    }

    /// Where a request to the collection `name` is placed (see the module docs).
    fn placed(&self, name: &str) -> Placed<'_> {
        Placed {
            in_default_network: self
                .vpc_endpoints
                .values()
                .filter(|record| {
                    // An endpoint routes traffic in the one status it settles into: one
                    // still being created, one that failed and one being deleted carries
                    // none, so none of them places a request either.
                    record.in_default_vpc == Some(true)
                        && record.status == super::VPC_ENDPOINT_ACTIVE
                })
                .map(|record| record.id.as_str())
                .collect(),
            published_behind: self
                .collection_by_name(name)
                .map_or(&[], |record| record.published_behind.as_slice()),
        }
    }

    /// Whether one policy document admits a request to `resource`: some statement of it both
    /// covers that resource and states a source the request is from.
    ///
    /// Read over every statement rather than the first covering one, because a document is a
    /// list of them and a collection two statements cover is admitted by either.
    fn policy_admits(&self, policy: &Document, resource: &str, placed: &Placed<'_>) -> bool {
        super::policy_document_entries(Some(policy))
            .into_iter()
            .any(|statement| {
                super::statement_covers(statement, resource)
                    && self.sources_admit(statement, placed)
            })
    }

    /// Whether one statement's own sources state a source this request is from.
    fn sources_admit(&self, statement: &Document, placed: &Placed<'_>) -> bool {
        let Document::Object(members) = statement else {
            return false;
        };
        if matches!(members.get(ALLOW_FROM_PUBLIC), Some(Document::Bool(true))) {
            return true;
        }
        // A statement admitting an AWS service's own traffic states a source nothing places a
        // request outside of: the services it names call in process.
        if !super::policy_document_entries(members.get(SOURCE_SERVICES)).is_empty() {
            return true;
        }
        let named = source_endpoints(statement);
        // Nothing stated, or nothing placing a request to this collection: no evidence that
        // it comes from somewhere else, and nothing is refused on silence.
        if named.is_empty() || placed.nowhere() {
            return true;
        }
        named
            .into_iter()
            .any(|id| placed.names(id) || !self.vpc_endpoints.contains_key(id))
    }
}

/// The `SourceVPCEs` entries one statement states, as the ids they are — an entry that is no
/// text at all names no endpoint and is left out, and so is one past
/// [`super::MAX_RESOURCE_PATTERN_LENGTH`], whose length is whoever wrote the document's to
/// choose and a factor in every comparison's cost.
fn source_endpoints(statement: &Document) -> Vec<&String> {
    let Document::Object(members) = statement else {
        return Vec::new();
    };
    super::policy_document_entries(members.get(SOURCE_VPCES))
        .into_iter()
        .take(super::MAX_POLICY_RESOURCES)
        .filter_map(|entry| match entry {
            Document::String(id) if id.len() <= super::MAX_RESOURCE_PATTERN_LENGTH => Some(id),
            _ => None,
        })
        .collect()
}
