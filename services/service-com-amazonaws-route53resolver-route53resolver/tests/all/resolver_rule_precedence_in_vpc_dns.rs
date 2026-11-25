// SPDX-License-Identifier: CC0-1.0

//! What the Resolver rules in force in a VPC do to the answers that VPC's own DNS serves, read
//! PER QUERIED NAME.
//!
//! RULE under test: a query asked inside a VPC is decided by the Resolver rules whose association
//! with that VPC has SETTLED, before the VPC's resolver answers out of anything of its own — and
//! the rule that decides one name is the one whose `DomainName` is THAT name's longest whole-label
//! suffix: "if a query matches multiple Resolver rules (example.com and www.example.com), outbound
//! DNS queries are routed using the Resolver rule that contains the most specific domain name
//! (www.example.com)" (`ResolverRule$RuleType`). A `FORWARD` rule sends the name's query to the
//! resolvers its `TargetIps` name, so nothing of this VPC answers it and nothing comes back when
//! nothing answers there; every other type leaves the name to the VPC's own resolver, which is
//! what `SYSTEM` is for — "when you have a forwarding rule to forward DNS queries for a domain to
//! your network and you want Resolver to process queries for a subdomain of that domain, specify
//! SYSTEM". The choice is made for each name on its own, so a parent domain and a subdomain of it
//! asked in the same batch take different branches, and a rule decides nothing from the moment its
//! association with the VPC is removed.
//!
//! The rows differ along what the rule generalises over: how a name stands to the rules in force
//! (a forwarding rule's own domain, a subdomain of it carried by a more specific `SYSTEM` rule, a
//! `SYSTEM` rule's own domain, a subdomain of THAT carried by a more specific `FORWARD` rule, and
//! a name no rule's domain covers), how many rules are in force (none, one, and each one added and
//! then removed again), how the name is spelled (as asked, and fully qualified in upper case),
//! which record type is asked about (`A` and `AAAA`), and which address of the VPC's own resolver
//! the query is sent to (the link-local one and the one at the base of the VPC's range plus two).
//!
//! Both readings of the plane are asserted for every row: the view this crate's own answer paths
//! take, and the register a component of ANOTHER crate that carries an in-VPC query reads
//! (`service_common::vpc_private_dns`) — which is what a program running in the VPC observes.
//!
//! Every write goes over the wire the service actually speaks (awsJson1_1, `POST /` with
//! `X-Amz-Target: Route53Resolver.<OperationName>`), through the shared [`super::harness`].

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_route53resolver_route53resolver::state::{
    Answer, Resolution, ResponseCode, StoredVpcDns, VpcDnsView, ENDPOINT_STATUS_OPERATIONAL,
    IP_STATUS_ATTACHED, PROTOCOL_UDP, RULE_ASSOCIATION_STATUS_COMPLETE, TYPE_A, TYPE_AAAA,
    VPC_RESOLVER_LINK_LOCAL_OCTETS,
};
use service_common::vpc_private_dns::{ForwardedQuery, VpcPrivateDnsPlane};

use super::harness::{emulator_on, ok, string_member, Emulator};

/// The names this suite asks about, which are names a DNS STANDARD reserves for documentation —
/// the public names every resolver of this emulator answers out of its one reading of them
/// (`service_common::public_dns_standard::DOCUMENTATION_DOMAINS`, RFC 2606 §3, and the one host
/// label published beside each). They are what gives every row a non-empty answer to start from,
/// so the only thing that can empty one is a rule.
const PARENT: &str = "example.com";
/// A subdomain of [`PARENT`]: the name a `SYSTEM` rule carves out of the forwarded parent.
const CHILD: &str = "www.example.com";
/// A second domain, carried the other way round: the `SYSTEM` rule is on this one …
const OTHER_PARENT: &str = "example.org";
/// … and the more specific `FORWARD` rule is on this subdomain of it.
const OTHER_CHILD: &str = "www.example.org";
/// A name no rule of this suite covers at all.
const UNCOVERED: &str = "example.net";

/// Every name a stage asks about, in the order a stage's expectations are written in.
const NAMES: [&str; 5] = [PARENT, CHILD, OTHER_PARENT, OTHER_CHILD, UNCOVERED];

/// The subnets the OUTBOUND endpoint's addresses are placed in: "you must specify at least two IP
/// addresses, in two different Availability Zones" (`CreateResolverEndpointRequest$IpAddresses`).
const SUBNETS: [&str; 2] = ["subnet-000000000000000a1", "subnet-000000000000000a2"];

/// How long the polls below wait before they look again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long they keep looking before they give up: generous, so how fast the host runs never
/// decides the outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// What the rules in force must make of one queried name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// No rule sends the name away, so the VPC's own resolver answers it.
    AnsweredHere,
    /// A forwarding rule sends the name's query to its targets, where nothing of this emulator
    /// answers DNS — so the client is left with an empty answer section.
    SentAway,
}

use Expected::{AnsweredHere, SentAway};

/// What one stage expects of each name of [`NAMES`], in that order.
type StageExpectation = [Expected; NAMES.len()];

/// Nothing is associated yet, so every name is answered where it was before any rule existed.
const NO_RULE_IN_FORCE: StageExpectation =
    [AnsweredHere, AnsweredHere, AnsweredHere, AnsweredHere, AnsweredHere];

/// The parent's `FORWARD` rule is in force: it covers its own domain AND the subdomain under it.
const PARENT_FORWARDED: StageExpectation =
    [SentAway, SentAway, AnsweredHere, AnsweredHere, AnsweredHere];

/// The subdomain's `SYSTEM` rule is in force too: the more specific rule carries that name, and
/// the parent stays forwarded.
const CHILD_CARVED_OUT: StageExpectation =
    [SentAway, AnsweredHere, AnsweredHere, AnsweredHere, AnsweredHere];

/// A `SYSTEM` rule on the second domain changes nothing: a type that leaves the query to the VPC's
/// own resolver sends nothing away.
const OTHER_PARENT_PROCESSED: StageExpectation = CHILD_CARVED_OUT;

/// A `FORWARD` rule on a subdomain of that `SYSTEM` domain: the more specific rule carries that
/// subdomain away, and its parent — covered only by the less specific `SYSTEM` rule — stays here.
const OTHER_CHILD_FORWARDED: StageExpectation =
    [SentAway, AnsweredHere, AnsweredHere, SentAway, AnsweredHere];

/// The subdomain's `SYSTEM` association is removed: that rule decides nothing any more, so the
/// name falls back under its parent's `FORWARD` rule.
const CHILD_CARVE_OUT_REMOVED: StageExpectation =
    [SentAway, SentAway, AnsweredHere, SentAway, AnsweredHere];

/// The parent's `FORWARD` association is removed as well: both names it covered are answered here
/// again, and the rules still in force keep deciding their own names.
const PARENT_FORWARD_REMOVED: StageExpectation =
    [AnsweredHere, AnsweredHere, AnsweredHere, SentAway, AnsweredHere];

/// The client whose queries a stage asks about: where it sits, and the addresses of its VPC's own
/// resolver it addresses them to.
struct Client {
    vpc_id: String,
    source_ip: String,
    /// The addresses the VPC's own resolver answers at — the link-local one and the one at the
    /// base of the VPC's range plus two — so no row depends on which of them was used.
    servers: Vec<String>,
}

/// One IPv4 address of the block `cidr_block`, with `host` as its last octet.
fn address_in(cidr_block: &str, host: u8) -> String {
    let base = cidr_block.split('/').next().unwrap_or_default();
    let mut octets: Vec<String> = base.split('.').map(str::to_string).collect();
    assert_eq!(
        octets.len(),
        4,
        "a subnet's block carries four octets, got {cidr_block:?}"
    );
    octets[3] = host.to_string();
    octets.join(".")
}

/// The addresses a documentation name is answered with for `record_type`, as the ONE reading of
/// those names this tree holds states them (`service_common::public_dns_standard`).
fn standard_answers(name: &str, record_type: &str) -> Vec<String> {
    service_common::public_dns_standard::fixed_addresses_of_type(name, record_type)
        .into_iter()
        .map(|address| address.to_string())
        .collect()
}

/// The answer the view served for one query, with the resolution it came in.
fn answered(view: &VpcDnsView, case: &str, server: &str, name: &str, record_type: &str) -> Answer {
    match view.answer(server, name, record_type, PROTOCOL_UDP) {
        Resolution::Answered(answer) => answer,
        other => panic!(
            "case {case:?}: the VPC's own resolver at {server} must answer {name} {record_type}, \
             got {other:?}"
        ),
    }
}

/// Create the OUTBOUND endpoint a forwarding rule routes its queries through, and answer its `Id`.
async fn outbound_endpoint(router: &Emulator, creator: &str) -> String {
    let created = ok(
        router,
        "CreateResolverEndpoint",
        json!({
            "CreatorRequestId": creator,
            "Direction": "OUTBOUND",
            "SecurityGroupIds": ["sg-000000000000000a1"],
            "IpAddresses": [
                { "SubnetId": SUBNETS[0] },
                { "SubnetId": SUBNETS[1] },
            ],
        }),
    )
    .await;
    string_member(
        created
            .get("ResolverEndpoint")
            .expect("the create answers a ResolverEndpoint"),
        "Id",
    )
}

/// Look at the endpoint `endpoint_id` again until it reports the settled status AND every address
/// it placed reports attached, so a rule created through it really has an interface to leave the
/// VPC by.
///
/// The CONDITION is what is waited for — what the endpoint REPORTS — and never a span of the
/// clock, so a loaded or a faster host changes nothing about the outcome.
async fn operational_endpoint(router: &Emulator, endpoint_id: &str) {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = ok(
            router,
            "GetResolverEndpoint",
            json!({ "ResolverEndpointId": endpoint_id }),
        )
        .await;
        let status = string_member(
            read.get("ResolverEndpoint").unwrap_or(&Value::Null),
            "Status",
        );
        let listed = ok(
            router,
            "ListResolverEndpointIpAddresses",
            json!({ "ResolverEndpointId": endpoint_id }),
        )
        .await;
        let addresses = listed
            .get("IpAddresses")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let attached = !addresses.is_empty()
            && addresses
                .iter()
                .all(|address| string_member(address, "Status") == IP_STATUS_ATTACHED);
        if status == ENDPOINT_STATUS_OPERATIONAL && attached {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the OUTBOUND endpoint must reach {ENDPOINT_STATUS_OPERATIONAL} with every address \
             {IP_STATUS_ATTACHED}, it still reports {status:?}: {listed}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create a `FORWARD` rule for `domain` that sends its queries to `target`, through `endpoint`.
async fn forward_rule(
    router: &Emulator,
    creator: &str,
    domain: &str,
    endpoint: &str,
    target: &str,
) -> String {
    let created = ok(
        router,
        "CreateResolverRule",
        json!({
            "CreatorRequestId": creator,
            "Name": creator,
            "RuleType": "FORWARD",
            "DomainName": domain,
            "ResolverEndpointId": endpoint,
            "TargetIps": [{ "Ip": target, "Port": 53 }],
        }),
    )
    .await;
    string_member(
        created
            .get("ResolverRule")
            .expect("the create answers a ResolverRule"),
        "Id",
    )
}

/// Create a `SYSTEM` rule for `domain`, which names neither a target nor an endpoint: it is the
/// rule that has Resolver process the queries itself.
async fn system_rule(router: &Emulator, creator: &str, domain: &str) -> String {
    let created = ok(
        router,
        "CreateResolverRule",
        json!({
            "CreatorRequestId": creator,
            "Name": creator,
            "RuleType": "SYSTEM",
            "DomainName": domain,
        }),
    )
    .await;
    string_member(
        created
            .get("ResolverRule")
            .expect("the create answers a ResolverRule"),
        "Id",
    )
}

/// Associate `rule` with `vpc` and wait until that association REPORTS the settled status — the
/// condition the rule's being in force is stated by — then answer the association's `Id`.
async fn associated_rule(router: &Emulator, rule: &str, vpc: &str, case: &str) -> String {
    let created = ok(
        router,
        "AssociateResolverRule",
        json!({ "ResolverRuleId": rule, "VPCId": vpc, "Name": format!("{rule}-in-{vpc}") }),
    )
    .await;
    let association_id = string_member(
        created
            .get("ResolverRuleAssociation")
            .expect("the associate answers a ResolverRuleAssociation"),
        "Id",
    );
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = ok(
            router,
            "GetResolverRuleAssociation",
            json!({ "ResolverRuleAssociationId": association_id.clone() }),
        )
        .await;
        let status = string_member(
            read.get("ResolverRuleAssociation").unwrap_or(&Value::Null),
            "Status",
        );
        if status == RULE_ASSOCIATION_STATUS_COMPLETE {
            return association_id;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the association of {rule} with {vpc} must reach \
             {RULE_ASSOCIATION_STATUS_COMPLETE}, it still reports {status:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Remove the association between `rule` and `vpc`.
async fn disassociate_rule(router: &Emulator, rule: &str, vpc: &str) {
    ok(
        router,
        "DisassociateResolverRule",
        json!({ "ResolverRuleId": rule, "VPCId": vpc }),
    )
    .await;
}

/// Assert every name of [`NAMES`] is decided the way `expected` says at this stage — over both
/// spellings, both record types and both addresses of the VPC's own resolver — through the view
/// this crate's own answer paths take AND through the register another crate's carrier reads.
fn assert_stage(
    plane: &StoredVpcDns,
    register: &VpcPrivateDnsPlane,
    client: &Client,
    stage: &str,
    expected: &StageExpectation,
) {
    for (name, expect) in NAMES.iter().zip(expected.iter()) {
        // The same name as asked for, and fully qualified in upper case: a name is matched
        // without regard to case or the trailing dot that spells the root.
        let spellings = [(*name).to_string(), format!("{}.", name.to_uppercase())];
        for spelling in &spellings {
            for record_type in [TYPE_A, TYPE_AAAA] {
                let fixed = standard_answers(spelling, record_type);
                for server in &client.servers {
                    let case =
                        format!("{stage}: {spelling} {record_type} asked at {server}");
                    // Each query takes a FRESH reading of the stores, which is what a client
                    // asking one at that instant gets.
                    let view = plane.view_for_client(&client.vpc_id, &client.source_ip, &[]);
                    let answer = answered(&view, &case, server, spelling, record_type);
                    let values: Vec<String> = answer
                        .records
                        .iter()
                        .map(|record| record.value.clone())
                        .collect();
                    match expect {
                        AnsweredHere => {
                            assert_eq!(
                                answer.code,
                                ResponseCode::NoError,
                                "case {case:?}: a name the VPC's own resolver answers must come \
                                 back without error, got {answer:?}"
                            );
                            assert_eq!(
                                values, fixed,
                                "case {case:?}: expected the addresses the standard fixes for \
                                 the name, {fixed:?}, got {values:?}"
                            );
                            assert!(
                                !values.is_empty(),
                                "case {case:?}: the answer section must not be empty, got \
                                 {answer:?}"
                            );
                        }
                        SentAway => assert!(
                            values.is_empty(),
                            "case {case:?}: a name a forwarding rule sent to targets nothing \
                             answers at must come back with an empty answer section, got \
                             {answer:?}"
                        ),
                    }
                }

                // The register another crate's carrier reads must decide the same name the same
                // way: a name sent away came back unanswered, and a name left here is no
                // forwarded query of that register at all.
                let carried = register.forwarded(&client.vpc_id, spelling, record_type);
                let case = format!("{stage}: {spelling} {record_type} carried by another crate");
                match expect {
                    AnsweredHere => assert!(
                        carried.is_none(),
                        "case {case:?}: a name no forwarding rule covers is left to the VPC's \
                         own resolver, got {carried:?}"
                    ),
                    SentAway => assert_eq!(
                        carried,
                        Some(ForwardedQuery::Unanswered),
                        "case {case:?}: a name a forwarding rule sent to targets nothing answers \
                         at is carried back unanswered, got {carried:?}"
                    ),
                }
            }
        }
    }
}

/// Each queried name is decided by the most specific Resolver rule in force in the VPC — so a
/// `SYSTEM` rule carves its own subdomain out of a forwarded parent, the parent stays forwarded,
/// and removing the carve-out's association puts the subdomain back under the parent.
#[tokio::test]
async fn each_queried_name_takes_the_most_specific_rule_in_force() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));

    // The VPC the queries are asked inside, and the subnets the endpoint's addresses sit in.
    let vpc_id = router.hold_vpc();
    router.hold_subnets_in(&vpc_id, SUBNETS);
    let block = router
        .cidr_block_of_subnet(SUBNETS[0])
        .expect("the account's Amazon EC2 store holds the subnet's block");

    let plane = StoredVpcDns::of(&*engine).expect("the engine carries this service's DNS plane");
    let register =
        VpcPrivateDnsPlane::of(&*engine).expect("the engine carries the in-VPC DNS register");
    let client = Client {
        vpc_id: vpc_id.clone(),
        source_ip: address_in(&block, 5),
        servers: vec![
            Ipv4Addr::from(VPC_RESOLVER_LINK_LOCAL_OCTETS).to_string(),
            plane
                .view_for_client(&vpc_id, &address_in(&block, 5), &[])
                .nameserver(),
        ],
    };

    // Where the forwarding rules send their queries: an address of the VPC nothing answers DNS
    // at, so a query a rule sends there comes back with nothing.
    let dead_target = address_in(&block, 250);

    assert_stage(
        &plane,
        &register,
        &client,
        "no rule is in force",
        &NO_RULE_IN_FORCE,
    );

    let endpoint = outbound_endpoint(&router, "precedence-outbound").await;
    operational_endpoint(&router, &endpoint).await;

    let parent_rule = forward_rule(
        &router,
        "precedence-parent-forward",
        PARENT,
        &endpoint,
        &dead_target,
    )
    .await;
    associated_rule(&router, &parent_rule, &vpc_id, "the parent's FORWARD rule").await;
    assert_stage(
        &plane,
        &register,
        &client,
        "a FORWARD rule on the parent domain",
        &PARENT_FORWARDED,
    );

    let child_rule = system_rule(&router, "precedence-child-system", CHILD).await;
    let child_association =
        associated_rule(&router, &child_rule, &vpc_id, "the subdomain's SYSTEM rule").await;
    assert_stage(
        &plane,
        &register,
        &client,
        "a SYSTEM rule on one subdomain of it",
        &CHILD_CARVED_OUT,
    );

    let other_parent_rule =
        system_rule(&router, "precedence-other-parent-system", OTHER_PARENT).await;
    associated_rule(
        &router,
        &other_parent_rule,
        &vpc_id,
        "the second domain's SYSTEM rule",
    )
    .await;
    assert_stage(
        &plane,
        &register,
        &client,
        "a SYSTEM rule on a second domain",
        &OTHER_PARENT_PROCESSED,
    );

    let other_child_rule = forward_rule(
        &router,
        "precedence-other-child-forward",
        OTHER_CHILD,
        &endpoint,
        &dead_target,
    )
    .await;
    associated_rule(
        &router,
        &other_child_rule,
        &vpc_id,
        "the FORWARD rule under that SYSTEM domain",
    )
    .await;
    assert_stage(
        &plane,
        &register,
        &client,
        "a FORWARD rule on a subdomain of the SYSTEM domain",
        &OTHER_CHILD_FORWARDED,
    );

    disassociate_rule(&router, &child_rule, &vpc_id).await;
    // The association the carve-out was in force by is gone, so nothing of it is left to decide a
    // query.
    let (status, _body) = super::harness::call(
        &router,
        "GetResolverRuleAssociation",
        json!({ "ResolverRuleAssociationId": child_association }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a disassociated association is no record of the store any more"
    );
    assert_stage(
        &plane,
        &register,
        &client,
        "the subdomain's SYSTEM association removed",
        &CHILD_CARVE_OUT_REMOVED,
    );

    disassociate_rule(&router, &parent_rule, &vpc_id).await;
    assert_stage(
        &plane,
        &register,
        &client,
        "the parent's FORWARD association removed",
        &PARENT_FORWARD_REMOVED,
    );
}
