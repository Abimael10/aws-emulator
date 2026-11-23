// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH resource a Resolver association call resolves,
//! and in which order.
//!
//! Two rules are pinned here.
//!
//! 1. A call that ASSOCIATES a Resolver resource with a VPC resolves, FIRST, the
//!    resource its own recorded refusal names: the rule associate and the DNS
//!    Firewall associate report the VPC the association would be made IN, so a
//!    request naming neither is refused for the VPC, while the query-logging
//!    associate reports the CONFIGURATION it would be made FOR, as an invalid
//!    parameter. The DNS Firewall slice reports a VPC the account holds nothing
//!    under with the one body it answers for every resource a caller cannot
//!    address — the coded can't-find sentence naming the id the request sent,
//!    closed with the trace clause of the request — rather than with a
//!    parameter-validation sentence of its own.
//! 2. A call that addresses an association by the (configuration, VPC) PAIR it
//!    was made from resolves the ASSOCIATION and nothing else, so every pair the
//!    service holds no association for — including one whose configuration it
//!    holds nothing under — is refused with that one fault's coded sentence.
//!
//! The rows differ along what the rules generalise over: which operation makes
//! the association, the form and length of the id that resolves to nothing
//! (Amazon EC2's two published forms, an id of no EC2 form, and both published
//! bounds of the modelled `ResourceId`'s length 1..64), whether the request
//! carried a tracing header, and which half of a pair the service holds nothing
//! for.
//!
//! [`super::association_vpc_references`] owns what a SERVED associate writes;
//! what is asserted here is which refusal an unserved one gets.
//!
//! Every case goes over the wire the service actually speaks (awsJson1_1,
//! `POST /` with `X-Amz-Target: Route53Resolver.<OperationName>`), through the
//! shared [`super::harness`].

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::harness::{
    call, call_tracing, emulator, error_type, ok, string_member, traced_message, tracing_chain,
    Emulator,
};
use super::resolver_query_log_config_associations::{association, association_count, config_id};

/// A VPC of the account, so a case about the resource the association is made
/// FOR is never about the VPC.
const HELD_VPC: &str = "vpc-00000000000000011";

/// The sentence an association the service holds nothing under is refused with,
/// whichever way the request addressed it: fixed text behind that fault's own
/// service error code, naming neither half of the pair.
const ASSOCIATION_GONE_SENTENCE: &str =
    "[RSLVR-01602] The specified query logging configuration association doesn't exist.";

/// The sentence the DNS Firewall slice answers for a resource the caller cannot
/// address, with the id the request named inside it.
fn cant_find_sentence(resource_id: &str) -> String {
    format!("[RSLVR-02025] Can’t find the resource with ID \"{resource_id}\".")
}

/// Ids that name no VPC of the account, spanning the forms and lengths one
/// takes: Amazon EC2's two published forms, an id of no EC2 form, and both
/// published bounds of the modelled `ResourceId`'s length 1..64
/// (`smithy_query(query="shape",shape="ResourceId")`).
fn unresolvable_vpc_ids() -> Vec<(&'static str, String)> {
    vec![
        ("an EC2 long-form id", "vpc-0123456789abcdef0".to_string()),
        ("an EC2 short-form id", "vpc-0a1b2c3d".to_string()),
        ("an id of no EC2 form", "not-in-this-account".to_string()),
        ("the shortest ResourceId (length 1)", "v".to_string()),
        ("the longest ResourceId (length 64)", "v".repeat(64)),
        (
            "an id differing from a held one only by case",
            HELD_VPC.to_uppercase(),
        ),
    ]
}

/// Create a DNS Firewall rule group and answer the id the service minted.
async fn rule_group_id(router: &Emulator, name: &str) -> String {
    let created = ok(
        router,
        "CreateFirewallRuleGroup",
        json!({ "CreatorRequestId": format!("group-{name}"), "Name": name }),
    )
    .await;
    string_member(
        created
            .get("FirewallRuleGroup")
            .unwrap_or_else(|| panic!("the create answers a FirewallRuleGroup, got {created}")),
        "Id",
    )
}

/// The `AssociateFirewallRuleGroup` request body naming `group` and `vpc`.
fn associate_firewall_request(tag: &str, group: &str, vpc: &str) -> Value {
    json!({
        "CreatorRequestId": format!("assoc-{tag}"),
        "FirewallRuleGroupId": group,
        "VpcId": vpc,
        // Inside the published band of `Priority` ("the allowed values for
        // Priority are between 100 and 9900").
        "Priority": 150,
        "Name": format!("assoc-{tag}"),
    })
}

/// A DNS Firewall associate naming a VPC the account holds nothing under is
/// refused with the slice's coded can't-find sentence naming THAT id, closed
/// with the trace clause of the request, and writes no association.
///
/// The rows span the forms and lengths an unresolvable id takes, and alternate
/// between a caller that names a trace chain and one that names none.
#[tokio::test]
async fn a_firewall_associate_reports_an_unresolvable_vpc_with_the_coded_sentence() {
    let mut sentences: Vec<String> = Vec::new();
    let mut minted: Vec<String> = Vec::new();

    for (index, (case, unknown)) in unresolvable_vpc_ids().into_iter().enumerate() {
        let router = emulator();
        // The account DOES hold a VPC, so every row is about the one id that
        // resolves to nothing rather than about an empty inventory.
        router.hold_vpcs([HELD_VPC]);
        let group = rule_group_id(&router, &format!("unresolvable-{index}")).await;

        let names_a_chain = index % 2 == 0;
        let (header, root) = tracing_chain(400 + u32::try_from(index).expect("a small index"));
        let (status, parsed) = call_tracing(
            &router,
            "AssociateFirewallRuleGroup",
            associate_firewall_request("unresolvable", &group, &unknown),
            names_a_chain.then_some(header.as_str()),
        )
        .await;

        assert_eq!(
            status, 400,
            "case {case:?}: the associate must answer HTTP 400, got {status}: {parsed}"
        );
        assert!(
            error_type(&parsed).contains("ResourceNotFoundException"),
            "case {case:?}: the refusal must name ResourceNotFoundException, got {parsed}"
        );
        let message = string_member(&parsed, "Message");
        let (stated, trace) = traced_message(&message).unwrap_or_else(|| {
            panic!(
                "case {case:?}: the refusal must close with the trace clause of the request, got \
                 {message:?}"
            )
        });
        assert_eq!(
            stated,
            cant_find_sentence(&unknown),
            "case {case:?}: the refusal must state the coded sentence naming the id the request \
             sent, got {message:?}"
        );
        if names_a_chain {
            assert_eq!(
                trace, root,
                "case {case:?}: the clause must carry the Root the request named, got {message:?}"
            );
        } else {
            minted.push(trace.to_string());
        }
        sentences.push(stated.to_string());

        // Nothing was written: the rule group holds no association.
        let listed = ok(
            &router,
            "ListFirewallRuleGroupAssociations",
            json!({ "FirewallRuleGroupId": group }),
        )
        .await;
        assert_eq!(
            listed
                .get("FirewallRuleGroupAssociations")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
            0,
            "case {case:?}: a refused associate must write no association, got {listed}"
        );
    }

    // The sentence is built from the request: distinct ids give distinct
    // sentences, and a request that named no trace is reported under its own
    // minted one.
    assert_eq!(
        sentences.iter().collect::<BTreeSet<&String>>().len(),
        sentences.len(),
        "every distinct VPC id must give a distinct sentence, got {sentences:?}"
    );
    assert_eq!(
        minted.iter().collect::<BTreeSet<&String>>().len(),
        minted.len(),
        "each request that named no trace must be reported under its own, got {minted:?}"
    );
}

/// Each associate resolves, FIRST, the resource its own recorded refusal names:
/// a request naming a VPC the account does not hold AND a resource this service
/// holds nothing under is refused for the VPC by the rule and DNS Firewall
/// associates, and for the CONFIGURATION by the query-logging associate.
///
/// The three writes are `AssociateResolverRule`'s `VPCId`,
/// `AssociateResolverQueryLogConfig`'s `ResourceId` and
/// `AssociateFirewallRuleGroup`'s `VpcId`. The first two model
/// `InvalidParameterException`: the rule associate reports the sentence Amazon
/// EC2 words for an id its inventory does not hold, the query-logging associate
/// the coded sentence of the configuration that cannot be addressed. The third
/// models no `InvalidParameterException` at all and reports the DNS Firewall
/// slice's coded can't-find sentence.
#[tokio::test]
async fn each_associate_is_refused_for_the_resource_it_resolves_first() {
    // An id of each kind that this service holds no resource under, in the form
    // it mints one, the member of that request which names the VPC, the refusal
    // it is answered with and which resource that refusal is about.
    let writes: [(&str, &str, Value, &str, &str, Subject); 3] = [
        (
            "a Resolver rule",
            "AssociateResolverRule",
            json!({ "ResolverRuleId": "rslvr-rr-0000000000000000" }),
            "VPCId",
            "InvalidParameterException",
            Subject::VpcOfTheInventory,
        ),
        (
            "a query logging configuration",
            "AssociateResolverQueryLogConfig",
            json!({ "ResolverQueryLogConfigId": "rqlc-0000000000000000" }),
            "ResourceId",
            "InvalidParameterException",
            Subject::QueryLogConfig,
        ),
        (
            "a DNS Firewall rule group",
            "AssociateFirewallRuleGroup",
            associate_firewall_request("ordering", "rslvr-frg-0000000000000000", ""),
            "VpcId",
            "ResourceNotFoundException",
            Subject::VpcThatCantBeFound,
        ),
    ];

    for (case, operation, template, vpc_member, refusal, subject) in writes {
        for (id_case, unknown) in unresolvable_vpc_ids() {
            let router = emulator();
            router.hold_vpcs([HELD_VPC]);
            let mut body = template.clone();
            body[vpc_member] = json!(unknown);

            let (status, parsed) = call(&router, operation, body).await;
            assert_eq!(
                status, 400,
                "case {case:?}/{id_case:?}: {operation} must answer HTTP 400, got {status}: \
                 {parsed}"
            );
            assert!(
                error_type(&parsed).contains(refusal),
                "case {case:?}/{id_case:?}: {operation} must answer {refusal} for the resource it \
                 resolves first, got {parsed}"
            );
            let message = string_member(&parsed, "Message");
            let expected = match subject {
                Subject::VpcOfTheInventory => format!("The vpc ID '{unknown}' does not exist"),
                Subject::VpcThatCantBeFound => cant_find_sentence(&unknown),
                Subject::QueryLogConfig => CONFIG_GONE_SENTENCE.to_string(),
            };
            let stated =
                traced_message(&message).map_or(message.clone(), |(stated, _)| stated.to_string());
            assert_eq!(
                stated, expected,
                "case {case:?}/{id_case:?}: {operation} must name the resource it resolves first, \
                 got {message:?}"
            );
        }
    }
}

/// Which resource an associate's refusal is ABOUT, and in whose words.
#[derive(Debug, Clone, Copy)]
enum Subject {
    /// The VPC, in the sentence Amazon EC2 words for an id its inventory does
    /// not hold.
    VpcOfTheInventory,
    /// The VPC, in the DNS Firewall slice's coded can't-find sentence.
    VpcThatCantBeFound,
    /// The query logging configuration, in its own coded sentence.
    QueryLogConfig,
}

/// The sentence a query logging configuration the service holds nothing under is
/// refused with, whichever class the path that could not resolve it reports.
const CONFIG_GONE_SENTENCE: &str =
    "[RSLVR-01601] The specified query logging configuration doesn't exist.";

/// Every pair a disassociate names that the service holds no association for is
/// refused with the one coded sentence for a missing association — a
/// configuration the service holds nothing under, a VPC that configuration was
/// never associated with, the two members swapped, and a second disassociate of
/// a pair already taken out — and the association of another pair is left in
/// place.
#[tokio::test]
async fn a_disassociate_reports_every_pair_it_holds_no_association_for() {
    let router = emulator();
    let config = config_id(&router, "pair-resolution").await;
    let other_config = config_id(&router, "pair-resolution-other").await;
    let associated_vpc = "vpc-0123456789abcdef0";
    let unassociated_vpc = "vpc-0a1b2c3d";
    let kept_vpc = "vpc-00000000000000022";
    router.hold_vpcs([associated_vpc, unassociated_vpc, kept_vpc]);

    // One association that is taken out, and one of another pair that every row
    // must leave alone.
    let made = ok(
        &router,
        "AssociateResolverQueryLogConfig",
        json!({ "ResolverQueryLogConfigId": config.clone(), "ResourceId": associated_vpc }),
    )
    .await;
    let taken_out = string_member(association(&made), "Id");
    let kept = ok(
        &router,
        "AssociateResolverQueryLogConfig",
        json!({ "ResolverQueryLogConfigId": other_config.clone(), "ResourceId": kept_vpc }),
    )
    .await;
    let kept_id = string_member(association(&kept), "Id");
    ok(
        &router,
        "DisassociateResolverQueryLogConfig",
        json!({ "ResolverQueryLogConfigId": config.clone(), "ResourceId": associated_vpc }),
    )
    .await;

    let rows: Vec<(&str, String, String)> = vec![
        (
            "a configuration the service holds nothing under",
            "rqlc-0000000000000000".to_string(),
            associated_vpc.to_string(),
        ),
        (
            "a configuration id of no Resolver form",
            "not-a-config-id".to_string(),
            associated_vpc.to_string(),
        ),
        (
            "a stored configuration and a VPC it was never associated with",
            config.clone(),
            unassociated_vpc.to_string(),
        ),
        (
            "a stored configuration and a VPC associated with another",
            config.clone(),
            kept_vpc.to_string(),
        ),
        (
            "the two members swapped",
            associated_vpc.to_string(),
            config.clone(),
        ),
        (
            "a second disassociate of the pair already taken out",
            config.clone(),
            associated_vpc.to_string(),
        ),
        (
            "the association's own id in place of the configuration",
            taken_out.clone(),
            associated_vpc.to_string(),
        ),
    ];

    let mut sentences: Vec<String> = Vec::new();
    for (index, (case, named_config, named_resource)) in rows.into_iter().enumerate() {
        let (header, root) = tracing_chain(500 + u32::try_from(index).expect("a small index"));
        let (status, parsed) = call_tracing(
            &router,
            "DisassociateResolverQueryLogConfig",
            json!({
                "ResolverQueryLogConfigId": named_config,
                "ResourceId": named_resource,
            }),
            Some(header.as_str()),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: the disassociate must answer HTTP 400, got {status}: {parsed}"
        );
        assert!(
            error_type(&parsed).contains("ResourceNotFoundException"),
            "case {case:?}: the refusal must name ResourceNotFoundException, got {parsed}"
        );
        let message = string_member(&parsed, "Message");
        let (stated, trace) = traced_message(&message).unwrap_or_else(|| {
            panic!(
                "case {case:?}: the refusal must close with the trace clause of the request, got \
                 {message:?}"
            )
        });
        assert_eq!(
            stated, ASSOCIATION_GONE_SENTENCE,
            "case {case:?}: the refusal must state the association's own coded sentence, got \
             {message:?}"
        );
        assert_eq!(
            trace, root,
            "case {case:?}: the clause must carry the Root the request named, got {message:?}"
        );
        assert_eq!(
            string_member(&parsed, "ResourceType"),
            "ResolverQueryLogConfigAssociation",
            "case {case:?}: the refusal must name the resource that is missing, got {parsed}"
        );
        sentences.push(stated.to_string());
    }

    // The sentence names neither half of the pair, so every row states exactly
    // the same one.
    assert_eq!(
        sentences.iter().collect::<BTreeSet<&String>>().len(),
        1,
        "every pair the service holds no association for must state the one fixed sentence, got \
         {sentences:?}"
    );

    // Nothing of the other pair was touched by any of the refusals.
    let read = ok(
        &router,
        "GetResolverQueryLogConfigAssociation",
        json!({ "ResolverQueryLogConfigAssociationId": kept_id.clone() }),
    )
    .await;
    assert_eq!(
        string_member(association(&read), "Id"),
        kept_id,
        "the association of another pair must still read back, got {read}"
    );
    assert_eq!(
        association_count(&router, &other_config).await,
        1,
        "the other configuration must still count the VPC it logs for"
    );
    assert_eq!(
        association_count(&router, &config).await,
        0,
        "the configuration whose only association was taken out must count none"
    );
}
